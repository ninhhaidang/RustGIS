"""Checks for mark-preview.py, which marks a pull request preview's web app before it is published."""

import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("mark_preview", Path(__file__).with_name("mark-preview.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

APP = Path(__file__).resolve().parents[2] / "apps" / "rustgis-web" / "index.html"
AWARE_SCRIPT = '<script>if (document.querySelector(\'meta[name="rustgis-preview"]\')) {}</script>'


class MarkPreview(unittest.TestCase):
    def run_on(self, html, worker=False):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "index.html").write_text(html, encoding="utf-8")
            if worker:
                (root / "sw.js").write_text("// worker", encoding="utf-8")
            status = MODULE.mark(root)
            return status, (root / "index.html").read_text(encoding="utf-8"), (root / "sw.js").exists()

    def test_the_app_page_is_marked_once_and_loses_its_worker(self):
        html = APP.read_text(encoding="utf-8")
        status, marked, worker = self.run_on(html, worker=True)
        self.assertEqual(status, 0)
        self.assertFalse(worker)
        self.assertEqual(marked.count(MODULE.MARK), 1)
        self.assertTrue(MODULE.Page(marked).marked)
        with tempfile.TemporaryDirectory() as folder:
            (Path(folder) / "index.html").write_text(marked, encoding="utf-8")
            self.assertEqual(MODULE.mark(folder), 0)
            self.assertEqual((Path(folder) / "index.html").read_text(encoding="utf-8"), marked, "a second run changes nothing")

    def test_a_head_in_a_comment_script_or_title_is_not_the_page_head(self):
        for decoy in ["<!--\n<head>\n-->", '<script>let s = "\n<head>\n";</script>', "<title><head></title>"]:
            html = f"<!DOCTYPE html>\n<html>\n{decoy}\n<head>\n{AWARE_SCRIPT}\n</head>\n<body></body>\n</html>\n"
            status, marked, _ = self.run_on(html)
            self.assertEqual(status, 0, decoy)
            self.assertIn("<head>" + MODULE.MARK, marked.split(decoy, 1)[1], decoy)
            self.assertNotIn(MODULE.MARK, marked.split(decoy, 1)[0] + decoy, decoy)

    def test_a_mark_in_a_comment_does_not_count(self):
        html = f"<html><head><!-- {MODULE.MARK} -->{AWARE_SCRIPT}</head><body></body></html>"
        status, marked, _ = self.run_on(html)
        self.assertEqual(status, 0)
        self.assertEqual(marked.count(MODULE.MARK), 2)
        self.assertTrue(MODULE.Page(marked).marked)

    def test_an_app_built_before_the_mark_is_refused_unchanged(self):
        html = "<html><head><title>RustGIS</title></head><body></body></html>"
        status, kept, worker = self.run_on(html, worker=True)
        self.assertEqual(status, 2)
        self.assertEqual(kept, html)
        self.assertFalse(worker)

    def test_a_page_without_a_head_is_refused(self):
        html = f"<html><!-- <head> -->{AWARE_SCRIPT}</html>"
        status, kept, _ = self.run_on(html)
        self.assertEqual(status, 1)
        self.assertEqual(kept, html)

    def test_a_folder_without_a_page_has_nothing_to_mark(self):
        with tempfile.TemporaryDirectory() as folder:
            (Path(folder) / "sw.js").write_text("// worker", encoding="utf-8")
            self.assertEqual(MODULE.mark(folder), 0)
            self.assertFalse((Path(folder) / "sw.js").exists())


if __name__ == "__main__":
    unittest.main()
