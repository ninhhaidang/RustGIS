# RustGIS downloads

This branch is generated automatically from the latest
[`opengeos/RustGIS`](https://github.com/opengeos/RustGIS/releases/latest) release and served at
<https://downloads.rustgis.app>. Files up to 100 MB are mirrored for GitHub Pages; larger files
link to their GitHub Release assets. The store-only packages (the Microsoft Store MSIX, the
Android App Bundle for Google Play and the iOS app for the App Store) are omitted, as are the
release's own checksum files: `SHA256SUMS.txt` here covers every mirrored file.

Do not edit this repository by hand. The page templates and the workflow that publishes it live
in the RustGIS repository (`packaging/downloads`, `.github/workflows/downloads-mirror.yml`).
