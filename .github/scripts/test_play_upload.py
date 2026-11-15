"""Tests for play_upload.py: the signed token and the order of the Play Developer API calls."""

import base64
import contextlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import play_upload  # noqa: E402


def unb64url(text: str) -> bytes:
    """Decodes unpadded base64url.

    Args:
        text: The encoded text.

    Returns:
        The bytes.
    """
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


class JwtTest(unittest.TestCase):
    def test_the_token_is_signed_by_the_key_and_names_the_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            key = Path(directory) / "key.pem"
            subprocess.run(["openssl", "genrsa", "-out", str(key), "2048"], check=True, capture_output=True)
            public = Path(directory) / "public.pem"
            subprocess.run(["openssl", "rsa", "-in", str(key), "-pubout", "-out", str(public)], check=True, capture_output=True)
            account = {"client_email": "upload@example.iam.gserviceaccount.com", "private_key": key.read_text(),
                       "token_uri": "https://oauth2.googleapis.com/token"}
            jwt = play_upload.jwt_assertion(account, 1_800_000_000, play_upload.openssl_sign)
            header, claims, signature = jwt.split(".")
            self.assertEqual(json.loads(unb64url(header)), {"alg": "RS256", "typ": "JWT"})
            body = json.loads(unb64url(claims))
            self.assertEqual(body["iss"], account["client_email"])
            self.assertEqual(body["scope"], play_upload.SCOPE)
            self.assertEqual(body["exp"] - body["iat"], 3600)
            sig = Path(directory) / "sig"
            sig.write_bytes(unb64url(signature))
            checked = subprocess.run(
                ["openssl", "dgst", "-sha256", "-verify", str(public), "-signature", str(sig)],
                input=f"{header}.{claims}".encode(), capture_output=True,
            )
            self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)


class UploadTest(unittest.TestCase):
    def test_an_edit_gets_the_bundle_a_draft_release_and_is_committed(self):
        calls = []

        def request(method, url, headers, body):
            calls.append((method, url.split("?")[0].rsplit("/", 3)[-3:], body))
            if url.endswith("/edits"):
                return {"id": "E1"}
            if "/bundles" in url:
                return {"versionCode": 1506}
            return {}

        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "app.aab"
            bundle.write_bytes(b"bundle")
            code = play_upload.upload("org.rustgis.mobile", bundle, "internal", "draft", "0.6.0", "Notes",
                                      "TOKEN", request)
        self.assertEqual(code, 1506)
        self.assertEqual([c[0] for c in calls], ["POST", "POST", "PUT", "POST"])
        self.assertEqual(calls[1][2], b"bundle")
        track = json.loads(calls[2][2])
        self.assertEqual(track["track"], "internal")
        self.assertEqual(track["releases"][0]["versionCodes"], ["1506"])
        self.assertEqual(track["releases"][0]["status"], "draft")
        self.assertEqual(track["releases"][0]["releaseNotes"][0]["language"], "en-US")
        self.assertTrue(calls[3][1][-1].endswith("E1:commit"))

    def test_an_unpublished_app_gets_a_draft_and_says_so(self):
        statuses = []

        def request(method, url, headers, body):
            if url.endswith("/edits"):
                return {"id": "E1"}
            if "/bundles" in url:
                return {"versionCode": 7}
            if method == "PUT":
                status = json.loads(body)["releases"][0]["status"]
                statuses.append(status)
                if status != "draft":
                    raise RuntimeError("PUT: HTTP 400: Only releases with status draft may be created on draft app.")
            return {}

        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "app.aab"
            bundle.write_bytes(b"bundle")
            with contextlib.redirect_stderr(io.StringIO()) as said:
                play_upload.upload("org.rustgis.mobile", bundle, "production", "completed", "1", "", "T", request)
        self.assertEqual(statuses, ["completed", "draft"])
        self.assertIn("draft", said.getvalue())

    def test_other_track_errors_are_not_retried(self):
        def request(method, url, headers, body):
            if url.endswith("/edits"):
                return {"id": "E1"}
            if "/bundles" in url:
                return {"versionCode": 7}
            if method == "PUT":
                raise RuntimeError("PUT: HTTP 403: The caller does not have permission")
            return {}

        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "app.aab"
            bundle.write_bytes(b"bundle")
            with self.assertRaises(RuntimeError):
                play_upload.upload("org.rustgis.mobile", bundle, "production", "completed", "1", "", "T", request)

    def test_long_notes_are_cut_at_a_word_within_the_limit(self):
        notes = play_upload.release_notes("word " * 200)
        self.assertLessEqual(len(notes), play_upload.NOTES_LIMIT)
        self.assertTrue(notes.endswith("…"))
        self.assertEqual(play_upload.release_notes(" short "), "short")


if __name__ == "__main__":
    unittest.main()
