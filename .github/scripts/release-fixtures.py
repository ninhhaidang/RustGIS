#!/usr/bin/env python3
"""Capture fixtures from the published RustGIS Linux x86-64 release."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time


REPO = "opengeos/RustGIS"
FIXTURES = Path(__file__).resolve().parents[2] / "crates/engine/testdata/releases"


def capture_project(cli, version, destination, env):
    """Save the same inline layers, styles and camera across released writers."""
    places = {
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": coordinates},
                "properties": {"name": name, "pop": pop},
            }
            for coordinates, name, pop in [([1, 2], "Alpha", 10), ([3, 4], "Béta", 20)]
        ],
    }
    routes = {
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": [[1, 2], [3, 4]]},
                "properties": {"name": "Route"},
            },
        ],
    }
    commands = [
        ("project.new", {"name": version}),
        ("layer.addDataText", {"fileName": "Places.geojson", "text": json.dumps(places)}),
        ("layer.symbology", {"layer": "Places", "type": "unique", "field": "name"}),
        ("labels.enable", {"layer": "Places", "on": True}),
        ("labels.field", {"layer": "Places", "field": "name"}),
        ("labels.style", {"layer": "Places", "bold": True, "halo": True, "size": 14, "color": "#123456"}),
        ("layer.definitionQuery", {"layer": "Places", "where": "pop >= 20"}),
        ("layer.addDataText", {"fileName": "Routes.geojson", "text": json.dumps(routes)}),
        ("layer.color", {"layer": "Routes", "color": "#336699"}),
        ("layer.outline", {"layer": "Routes", "color": "#112233", "width": 2.5}),
        ("layer.transparency", {"layer": "Routes", "value": 25}),
        ("map.camera", {"lon": 2, "lat": 3, "scale": 250_000}),
    ]
    args = [cli, "run", "--no-plugins"]
    for command, params in commands:
        args.extend(["--cmd", f"{command}={json.dumps(params)}"])
    subprocess.run([*args, "--save", destination], env=env, cwd=destination.parent, check=True, timeout=60)


def capture_prefs(desktop, env):
    """Send the ribbon shortcut to the release window and return the bytes it saved."""
    prefs = Path(env["XDG_CONFIG_HOME"]) / "rustgis/prefs.json"
    # The desktop and xdotool share a private display created by xvfb-run.
    with subprocess.Popen([desktop, "--sample"], env=env, cwd=prefs.parent.parent) as app:
        try:
            window = subprocess.check_output(
                ["xdotool", "search", "--sync", "--onlyvisible", "--pid", str(app.pid)],
                text=True,
                timeout=60,
            ).splitlines()[0]
            # The window can be mapped before the app reads input (slow software GL on a runner), so
            # a key sent too early can be lost or held back. Press again every few seconds until
            # the saved preference reads "true": the app saves it on every toggle, so a held-back
            # press that toggles it back to "false" is seen and answered with another press.
            press = ["xdotool", "windowfocus", "--sync", window, "key", "ctrl+F1"]
            subprocess.run(press, check=True, timeout=10)
            deadline = time.monotonic() + 60
            resend = time.monotonic() + 5
            while time.monotonic() < deadline and app.poll() is None:
                saved = {}
                if prefs.exists():
                    data = prefs.read_bytes()
                    try:
                        saved = json.loads(data)
                    except ValueError:  # JSON or UTF-8 caught mid-write: read again on the next pass
                        saved = None
                    if saved and saved.get("ribbon_minimized") == "true":
                        return data
                if saved is not None and time.monotonic() >= resend:
                    subprocess.run(press, check=True, timeout=10)
                    resend = time.monotonic() + 5
                time.sleep(0.1)
            raise RuntimeError("The release did not persist the collapsed ribbon after Ctrl+F1")
        finally:
            if app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    app.kill()


def isolated_env(work):
    """Keep each writer's configuration and caches separate from the other writer and the host."""
    env = {
        key: value for key, value in os.environ.items()
        if not key.startswith(("RUSTGIS_", "ACTIONS_"))
        and key not in ("GH_TOKEN", "GITHUB_TOKEN", "WAYLAND_DISPLAY")
    }
    for kind in ("CONFIG", "DATA", "CACHE", "STATE"):
        directory = work / kind.lower()
        directory.mkdir(parents=True)
        env[f"XDG_{kind}_HOME"] = str(directory)
    env["LIBGL_ALWAYS_SOFTWARE"] = "1"
    return env


def generate(version):
    """Verify the archive and finish both captures before adding either fixture."""
    project = FIXTURES / f"{version}.rgis"
    prefs = FIXTURES / f"{version}.prefs.json"
    readme = FIXTURES / "README.md"
    notes = readme.read_text(encoding="utf-8")
    row_start = f"| {version} |"
    if project.exists() or prefs.exists() or any(line.startswith(row_start) for line in notes.splitlines()):
        raise RuntimeError(f"A fixture or README row already exists for {version}")
    table_end = notes.index("\n\n", notes.index("| Writer |"))

    is_draft = subprocess.check_output(
        ["gh", "release", "view", version, "-R", REPO, "--json", "isDraft", "--jq", ".isDraft"],
        text=True,
    )
    if is_draft.strip() != "false":
        raise RuntimeError(f"{version} is not a published release")

    archive_root = f"RustGIS-{version[1:]}-linux-x86_64"
    archive = f"{archive_root}.tar.gz"
    with tempfile.TemporaryDirectory(prefix="rustgis-release-fixtures-") as temp:
        work = Path(temp)
        subprocess.run(
            ["gh", "release", "download", version, "-R", REPO, "-p", archive, "-p", "SHA256SUMS"],
            cwd=work, check=True,
        )
        checksums = (work / "SHA256SUMS").read_text(encoding="utf-8").splitlines()
        expected = [line[:64] for line in checksums if line[64:] in (f"  {archive}", f" *{archive}")]
        with (work / archive).open("rb") as download:
            digest = hashlib.file_digest(download, "sha256").hexdigest()
        if expected != [digest]:
            raise RuntimeError(f"SHA-256 mismatch or missing checksum for {archive}")

        # Extract just the two files we execute, without unpacking the rest of the release.
        subprocess.run(
            ["tar", "-xzf", archive, f"{archive_root}/rustgis", f"{archive_root}/rustgis-cli"],
            cwd=work, check=True,
        )
        binaries = work / archive_root
        saved_project = work / project.name
        capture_project(binaries / "rustgis-cli", version, saved_project, isolated_env(work / "cli"))
        saved_prefs = capture_prefs(binaries / "rustgis", isolated_env(work / "desktop"))

        # Exclusive writes protect historical files even if a second capture begins concurrently.
        with project.open("xb") as output:
            output.write(saved_project.read_bytes())
        with prefs.open("xb") as output:
            output.write(saved_prefs)

    row = f"{row_start} `{archive}` | `{digest}` |"
    readme.write_text(notes[:table_end] + "\n" + row + notes[table_end:], encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="published release tag, e.g. v0.7.0")
    args = parser.parse_args()
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", args.version):
        parser.error("expected a release tag in the form vMAJOR.MINOR.PATCH")
    if not os.environ.get("DISPLAY"):
        parser.error("run under xvfb-run -a to give the desktop a private display")
    generate(args.version)
