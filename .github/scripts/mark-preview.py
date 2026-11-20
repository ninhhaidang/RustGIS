"""Marks a pull request preview's web app before docs-preview-publish.yml publishes it.

Every preview shares one origin (preview.rustgis.app), where any preview's scripts can read and
change what the others keep in the browser. A marked app (`<meta name="rustgis-preview">`) keeps
its settings in memory, installs no service worker and clears what earlier previews left there.

    mark-preview.py <app folder>

Removes the folder's service worker (sw.js) and adds the mark to its index.html, as the first
element of its <head>, found by parsing the HTML: a `<head>` inside a comment, a script or text
is not the page's. Running it again changes nothing.

Exit status: 0 marked (or no index.html to mark); 2 the app predates the mark (its page never
looks for it, so marking would change nothing: don't publish it); 1 it can't be marked.
"""

from html.parser import HTMLParser
from pathlib import Path
import sys

MARK = '<meta name="rustgis-preview" content="shared-origin">'
# What a page that reads the mark contains (apps/rustgis-web/index.html).
AWARE = 'meta[name="rustgis-preview"]'
# Elements whose content a browser doesn't parse as markup (or, for template, doesn't put in
# the document), which Python's parser may still read tags in.
OPAQUE = {"title", "textarea", "noscript", "template", "xmp", "iframe", "noembed", "noframes", "plaintext"}


class Page(HTMLParser):
    """Where the page's <head> start tag ends, and whether a mark is inside that head."""

    def __init__(self, text):
        super().__init__(convert_charrefs=True)
        self.starts = [0]
        for i, c in enumerate(text):
            if c == "\n":
                self.starts.append(i + 1)
        self.head_end = None
        self.in_head = False
        self.marked = False
        self.opaque = []
        self.feed(text)
        self.close()

    def at(self):
        line, column = self.getpos()
        return self.starts[line - 1] + column

    def handle_starttag(self, tag, attrs):
        if self.opaque:
            if tag == self.opaque[-1]:
                self.opaque.append(tag)
            return
        if tag in OPAQUE:
            self.opaque.append(tag)
            return
        if tag == "head" and self.head_end is None:
            self.head_end = self.at() + len(self.get_starttag_text() or "")
            self.in_head = True
        elif tag == "body":
            self.in_head = False
        elif tag == "meta" and self.in_head and ("name", "rustgis-preview") in attrs:
            self.marked = True

    def handle_endtag(self, tag):
        if self.opaque:
            if tag == self.opaque[-1]:
                self.opaque.pop()
            return
        if tag == "head":
            self.in_head = False


def mark(folder):
    """Marks the app in `folder`; returns the exit status."""
    folder = Path(folder)
    (folder / "sw.js").unlink(missing_ok=True)
    page = folder / "index.html"
    if not page.is_file():
        return 0
    text = page.read_text(encoding="utf-8", errors="replace")
    if AWARE not in text:
        return 2
    parsed = Page(text)
    if parsed.marked:
        return 0
    if parsed.head_end is None:
        return 1
    marked = text[: parsed.head_end] + MARK + text[parsed.head_end :]
    if not Page(marked).marked:
        return 1
    page.write_text(marked, encoding="utf-8")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: mark-preview.py <app folder>", file=sys.stderr)
        sys.exit(1)
    sys.exit(mark(sys.argv[1]))
