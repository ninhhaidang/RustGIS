"""Tests for play_upload.py: the signed token and the order of the Play Developer API calls."""

import base64
import contextlib
import hashlib
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


def bundle_file(directory: str) -> Path:
    """Writes a stand-in App Bundle.

    Args:
        directory: Where to write it.

    Returns:
        Its path.
    """
    bundle = Path(directory) / "app.aab"
    bundle.write_bytes(b"bundle")
    return bundle


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
            result = play_upload.upload("org.rustgis.mobile", bundle_file(directory), "internal", "draft", "0.6.0",
                                        "Notes", "TOKEN", request=request)
        self.assertEqual(result, (1506, "draft"))
        self.assertEqual([c[0] for c in calls], ["POST", "POST", "PUT", "POST"])
        self.assertEqual(calls[1][2], b"bundle")
        track = json.loads(calls[2][2])
        self.assertEqual(track["track"], "internal")
        self.assertEqual(track["releases"][0]["versionCodes"], ["1506"])
        self.assertEqual(track["releases"][0]["status"], "draft")
        self.assertEqual(track["releases"][0]["releaseNotes"][0]["language"], "en-US")
        self.assertTrue(calls[3][1][-1].endswith("E1:commit"))
        self.assertEqual(calls[3][2], b"")

    def check_draft_fallback(self, refuse_at):
        """Checks a draft-app refusal at `refuse_at` (PUT or commit) gives a new edit with a draft.

        Args:
            refuse_at: "PUT" for the track's update, "commit" for the edit's commit.
        """
        edits, statuses, pending, dropped = [], [], {}, []

        def request(method, url, headers, body):
            if method == "DELETE":
                dropped.append(url.rsplit("/", 1)[-1])
                return {}
            if url.endswith("/edits"):
                edits.append(f"E{len(edits) + 1}")
                return {"id": edits[-1]}
            if "/bundles" in url:
                return {"versionCode": 7}
            if method == "PUT":
                pending["status"] = json.loads(body)["releases"][0]["status"]
                statuses.append(pending["status"])
                if refuse_at == "PUT" and pending["status"] != "draft":
                    raise RuntimeError("PUT: HTTP 400: Only releases with status draft may be created on draft app.")
            if url.endswith(":commit") and refuse_at == "commit" and pending["status"] != "draft":
                raise RuntimeError("POST: HTTP 400: Only releases with status draft may be created on draft app.")
            return {}

        with tempfile.TemporaryDirectory() as directory:
            with contextlib.redirect_stderr(io.StringIO()) as said:
                result = play_upload.upload("org.rustgis.mobile", bundle_file(directory), "production", "completed",
                                            "1", "", "T", request=request)
        self.assertEqual(result, (7, "draft"))
        self.assertEqual(statuses, ["completed", "draft"])
        self.assertEqual(edits, ["E1", "E2"])
        self.assertEqual(dropped, ["E1"])
        self.assertIn("draft", said.getvalue())

    def test_an_unpublished_app_gets_a_draft_when_the_track_refuses(self):
        self.check_draft_fallback("PUT")

    def test_an_unpublished_app_gets_a_draft_when_the_commit_refuses(self):
        self.check_draft_fallback("commit")

    def test_an_edit_is_dropped_after_any_failure_and_the_first_error_stands(self):
        dropped = []

        def request(method, url, headers, body):
            if method == "DELETE":
                dropped.append(url.rsplit("/", 1)[-1])
                raise OSError("network is unreachable")
            if url.endswith("/edits"):
                return {"id": "E1"}
            if "/bundles" in url:
                raise TimeoutError("the upload timed out")
            return {}

        with tempfile.TemporaryDirectory() as directory:
            with contextlib.redirect_stderr(io.StringIO()) as said:
                with self.assertRaisesRegex(TimeoutError, "timed out"):
                    play_upload.upload("org.rustgis.mobile", bundle_file(directory), "internal", "draft", "1", "",
                                       "T", request=request)
        self.assertEqual(dropped, ["E1"])
        self.assertIn("could not be dropped", said.getvalue())

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
            with self.assertRaises(RuntimeError):
                play_upload.upload("org.rustgis.mobile", bundle_file(directory), "production", "completed", "1", "",
                                   "T", request=request)

    def test_a_bundle_play_already_has_is_released_by_its_version_code(self):
        tracks = []
        held = {"sha256": hashlib.sha256(b"bundle").hexdigest()}

        def request(method, url, headers, body):
            if url.endswith("/edits"):
                return {"id": "E1"}
            if method == "GET" and url.endswith("/bundles"):
                return {"bundles": [{"versionCode": 1505, "sha256": "0"}, {"versionCode": 1506, **held}]}
            if "/bundles" in url:
                raise RuntimeError("POST: HTTP 403: APK specified version code 1506 has already been used.")
            if method == "PUT":
                tracks.append(json.loads(body))
            return {}

        with tempfile.TemporaryDirectory() as directory:
            with contextlib.redirect_stderr(io.StringIO()) as said:
                result = play_upload.upload("org.rustgis.mobile", bundle_file(directory), "production", "completed",
                                            "1", "", "T", 1506, request=request)
            self.assertEqual(result, (1506, "completed"))
            self.assertEqual(tracks[0]["releases"][0]["versionCodes"], ["1506"])
            self.assertIn("1506", said.getvalue())
            # Without the version code, the refusal stands.
            with self.assertRaises(RuntimeError):
                play_upload.upload("org.rustgis.mobile", bundle_file(directory), "production", "completed", "1", "",
                                   "T", request=request)
            # Play's bundle with that code is another build (another commit with the same count).
            held["sha256"] = hashlib.sha256(b"another build").hexdigest()
            with self.assertRaisesRegex(RuntimeError, "another build"):
                play_upload.upload("org.rustgis.mobile", bundle_file(directory), "production", "completed", "1", "",
                                   "T", 1506, request=request)
            self.assertEqual(len(tracks), 1)

    def test_a_key_openssl_cannot_read_is_named(self):
        with self.assertRaisesRegex(RuntimeError, "openssl couldn't sign"):
            play_upload.openssl_sign(b"data", "not a key")

    def test_long_notes_are_cut_at_a_word_within_the_limit(self):
        notes = play_upload.release_notes("word " * 200)
        self.assertLessEqual(len(notes), play_upload.NOTES_LIMIT)
        self.assertTrue(notes.endswith("…"))
        self.assertEqual(play_upload.release_notes(" short "), "short")


if __name__ == "__main__":
    unittest.main()
