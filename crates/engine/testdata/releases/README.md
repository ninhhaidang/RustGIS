# Files written by RustGIS releases

These are the original bytes written by the published Linux x86-64 binaries
downloaded from
[opengeos/RustGIS](https://github.com/opengeos/RustGIS/releases).
Never regenerate, reformat or resave them. All releases below support both file
types. Record a skip here when a release has no such format, and add it to `SKIPS` in
`crates/engine/src/tests/released_files.rs`. The test checks this table against
the files, so a release's row and its files are added or kept together.

| Writer | Release archive | Archive SHA-256 |
|---|---|---|
| v0.2.0 | `RustGIS-0.2.0-linux-x86_64.tar.gz` | `1cd13d8c5d84777537d211314adc745ccf9341707b546555e24a294f990b384a` |
| v0.3.0 | `RustGIS-0.3.0-linux-x86_64.tar.gz` | `98c7c2a3c2e646fb0affb6c812db6735f83b316d35c4ef89fd3bb658cebb08f2` |
| v0.4.0 | `RustGIS-0.4.0-linux-x86_64.tar.gz` | `6ccd39f1c35ff824e2ce9d875922394a00ab2468057ad183080205d17f1e07a1` |
| v0.5.0 | `RustGIS-0.5.0-linux-x86_64.tar.gz` | `a14e6bed30059ab8fb1c026c71bdb46891b2b4195e0d0ea39cc1f903b64fa9bf` |
| v0.6.0 | `RustGIS-0.6.0-linux-x86_64.tar.gz` | `633bea1712c3369ce43aa0d5ffcebe9928ff2dd1068280e9a6a8c31a2c94a972` |
| v0.7.0 | `RustGIS-0.7.0-linux-x86_64.tar.gz` | `8330b3fa8f1d817818d03d41d4d5653e7c2367954d0ab2262c8d88d36f6b7e98` |

The [generator](../../../../.github/scripts/release-fixtures.py) verifies the
download against the release's `SHA256SUMS`. It saves the project with that
release's CLI using two inline GeoJSON layers, Places and Routes. It starts the
released desktop app with an empty `XDG_CONFIG_HOME` under Xvfb and sends Ctrl+F1.
It copies the resulting preferences verbatim.

On Linux x86-64 with `gh`, Python 3.11 or newer, Xvfb, xauth, xdotool and the
desktop runtime libraries, run from the repository root with the new release tag:

```sh
xvfb-run -a python3 .github/scripts/release-fixtures.py vMAJOR.MINOR.PATCH
```

The [Release fixtures workflow](../../../../.github/workflows/release-fixtures.yml)
runs this for new releases as soon as the assets are available. It can also be run
manually with a published tag. If Actions cannot open a pull request, the workflow
retains the two files and the updated README as a run artifact. For a generated pull
request, approve the workflow runs from the merge box. If GitHub shows neither an
approval banner nor checks, close and reopen the pull request to start CI.
