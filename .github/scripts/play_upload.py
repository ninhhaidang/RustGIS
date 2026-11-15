#!/usr/bin/env python3
"""Uploads an Android App Bundle to a Google Play track as a draft release (#1902).

Used by `.github/workflows/mobile.yml`. It talks to the Google Play Developer API directly, with
the standard library and the `openssl` program (which signs the service account's token), so no
third-party code handles the service account's key:

1. a token for the service account in `PLAY_SERVICE_ACCOUNT_JSON` (its JSON key);
2. an edit of the app; the bundle uploaded to it;
3. the track given a release of that version code, as a draft (or the status asked for);
4. the edit committed.

A draft release appears in Play Console for someone to review and roll out. `--status completed`
rolls the release out to everyone on the track (after Google's review, on production); an app
never published yet takes only drafts, so there the release is made a draft and that is said.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Callable

SCOPE = "https://www.googleapis.com/auth/androidpublisher"
API = "https://androidpublisher.googleapis.com/androidpublisher/v3/applications"
UPLOAD_API = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications"
# Release notes Play takes, per language.
NOTES_LIMIT = 500

# (method, url, headers, body) -> parsed JSON response.
Request = Callable[[str, str, dict, bytes | None], dict]


def b64url(data: bytes) -> str:
    """Returns `data` in unpadded base64url, as a JWT wants it.

    Args:
        data: The bytes to encode.

    Returns:
        The encoded text.
    """
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def jwt_assertion(account: dict, now: int, sign: Callable[[bytes, str], bytes]) -> str:
    """Builds the signed JWT a service account exchanges for an access token.

    Args:
        account: The service account's JSON key (`client_email`, `private_key`, `token_uri`).
        now: The current time, in seconds since the epoch.
        sign: Signs bytes with the PEM private key given, RS256.

    Returns:
        The JWT, `header.claims.signature`.
    """
    header = b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode())
    claims = {
        "iss": account["client_email"],
        "scope": SCOPE,
        "aud": account.get("token_uri", "https://oauth2.googleapis.com/token"),
        "iat": now,
        "exp": now + 3600,
    }
    body = b64url(json.dumps(claims, separators=(",", ":")).encode())
    signing_input = f"{header}.{body}".encode()
    return f"{header}.{body}.{b64url(sign(signing_input, account['private_key']))}"


def openssl_sign(data: bytes, private_key_pem: str) -> bytes:
    """Signs `data` with SHA-256 and RSA by the `openssl` program.

    The key is written to a file only this user can read, in a directory removed afterwards.

    Args:
        data: The bytes to sign.
        private_key_pem: The RSA private key, PEM.

    Returns:
        The signature.
    """
    with tempfile.TemporaryDirectory() as directory:
        key = Path(directory) / "key.pem"
        key.touch(mode=0o600)
        key.write_text(private_key_pem)
        return subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", str(key)], input=data, capture_output=True, check=True
        ).stdout


def http_request(method: str, url: str, headers: dict, body: bytes | None) -> dict:
    """Sends one HTTPS request and returns its JSON response.

    Args:
        method: The HTTP method.
        url: The address.
        headers: The request's headers.
        body: The request's body, if any.

    Returns:
        The parsed response (an empty dict for an empty one).

    Raises:
        RuntimeError: The server answered with an error; its message is kept.
    """
    request = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=600) as response:
            text = response.read().decode()
    except urllib.error.HTTPError as error:
        detail = error.read().decode(errors="replace")
        raise RuntimeError(f"{method} {url.split('?')[0]}: HTTP {error.code}: {detail}") from None
    return json.loads(text) if text.strip() else {}


def release_notes(text: str) -> str:
    """Fits release notes to Play's limit, cutting at a word and marking the cut.

    Args:
        text: The notes.

    Returns:
        The notes, at most `NOTES_LIMIT` characters.
    """
    text = text.strip()
    if len(text) <= NOTES_LIMIT:
        return text
    cut = text[: NOTES_LIMIT - 1].rsplit(" ", 1)[0]
    return cut + "…"


def upload(
    package: str,
    bundle: Path,
    track: str,
    status: str,
    name: str,
    notes: str,
    token: str,
    request: Request = http_request,
) -> int:
    """Uploads `bundle` to `track` as a release with `status`, in one committed edit.

    Args:
        package: The app's package name.
        bundle: The signed App Bundle.
        track: The track (internal, alpha, beta, production).
        status: The release's status (draft, or completed to roll it out).
        name: The release's name in Play Console.
        notes: The release notes, in English (US).
        token: An access token for the Play Developer API.
        request: Sends a request (replaced in tests).

    Returns:
        The version code Play read from the bundle.
    """
    auth = {"Authorization": f"Bearer {token}"}
    base = f"{API}/{urllib.parse.quote(package)}/edits"
    edit = request("POST", base, {**auth, "Content-Type": "application/json"}, b"{}")["id"]
    uploaded = request(
        "POST",
        f"{UPLOAD_API}/{urllib.parse.quote(package)}/edits/{edit}/bundles?uploadType=media",
        {**auth, "Content-Type": "application/octet-stream"},
        bundle.read_bytes(),
    )
    version_code = int(uploaded["versionCode"])
    release = {"name": name, "versionCodes": [str(version_code)], "status": status}
    if notes:
        release["releaseNotes"] = [{"language": "en-US", "text": release_notes(notes)}]

    def put_track() -> None:
        request(
            "PUT",
            f"{base}/{edit}/tracks/{urllib.parse.quote(track)}",
            {**auth, "Content-Type": "application/json"},
            json.dumps({"track": track, "releases": [release]}).encode(),
        )

    try:
        put_track()
    except RuntimeError as error:
        # An app that has never been published takes only draft releases.
        if status == "draft" or "draft app" not in str(error).lower():
            raise
        print(
            f"play_upload: the app isn't published yet, so Play takes only drafts: the {track} release "
            "is a draft, to roll out in Play Console",
            file=sys.stderr,
        )
        release["status"] = "draft"
        put_track()
    request("POST", f"{base}/{edit}:commit", auth, None)
    return version_code


def access_token(account: dict, request: Request = http_request) -> str:
    """Exchanges the service account's signed JWT for an access token.

    Args:
        account: The service account's JSON key.
        request: Sends a request (replaced in tests).

    Returns:
        The access token.
    """
    assertion = jwt_assertion(account, int(time.time()), openssl_sign)
    form = urllib.parse.urlencode(
        {"grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer", "assertion": assertion}
    ).encode()
    url = account.get("token_uri", "https://oauth2.googleapis.com/token")
    return request("POST", url, {"Content-Type": "application/x-www-form-urlencoded"}, form)["access_token"]


def main(argv: list[str]) -> int:
    """Runs the upload from the command line.

    Args:
        argv: The arguments, without the program's name.

    Returns:
        The exit code.
    """
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("bundle", type=Path, help="the signed .aab")
    parser.add_argument("--package", required=True)
    parser.add_argument("--track", default="internal")
    parser.add_argument("--status", default="draft", choices=["draft", "completed"])
    parser.add_argument("--name", required=True, help="the release's name in Play Console")
    parser.add_argument("--notes", default="", help="the release notes, in English (US)")
    args = parser.parse_args(argv)
    raw = os.environ.get("PLAY_SERVICE_ACCOUNT_JSON", "")
    if not raw.strip():
        print("play_upload: PLAY_SERVICE_ACCOUNT_JSON isn't set", file=sys.stderr)
        return 1
    try:
        account = json.loads(raw)
    except json.JSONDecodeError as error:
        print(f"play_upload: PLAY_SERVICE_ACCOUNT_JSON isn't a JSON key: {error}", file=sys.stderr)
        return 1
    code = upload(args.package, args.bundle, args.track, args.status, args.name, args.notes, access_token(account))
    print(f"play_upload: version code {code} is on the {args.track} track")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
