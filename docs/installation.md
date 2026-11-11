# Installation

## Install with an agent

Point your agent at the [RustGIS skill](https://github.com/opengeos/RustGIS/blob/main/skills/rustgis/SKILL.md)
and ask it to install RustGIS. The skill picks the platform, installs the app and `rustgis-cli`,
checks the release checksum, and registers the MCP server. The same skill is how an agent manages
the desktop app that is already open.

## Microsoft Store

On Windows 10 and 11, the easiest way to install RustGIS is from the Microsoft Store, which
signs the app and keeps it up to date:

[![Get it from Microsoft](https://get.microsoft.com/images/en-us%20dark.svg){ width="200" }](https://apps.microsoft.com/detail/9ppjw8h561lq?mode=direct)

## Download

Each [GitHub release](https://github.com/opengeos/RustGIS/releases/latest) carries ready-to-run
builds of the desktop app and `rustgis-cli`. The same files, grouped by platform, are mirrored at
**[downloads.rustgis.app](https://downloads.rustgis.app)**:

| Platform | File |
| --- | --- |
| Windows x64 / arm64 (installer, no admin rights) | `RustGIS-<version>-windows-x86_64-setup.exe` / `RustGIS-<version>-windows-aarch64-setup.exe` |
| Windows x64 / arm64 (installer for all users) | `RustGIS-<version>-windows-x86_64.msi` / `RustGIS-<version>-windows-aarch64.msi` |
| Windows x64 / arm64 (no install) | `RustGIS-<version>-windows-x86_64-portable.zip` / `RustGIS-<version>-windows-aarch64-portable.zip` (unzip and run `rustgis.exe`) |
| Windows x64 / arm64 (Microsoft Store package) | `RustGIS-<version>-windows-x86_64.msix` / `RustGIS-<version>-windows-aarch64.msix` |
| macOS Apple Silicon | `RustGIS-<version>-macos-aarch64.dmg` |
| macOS Intel | `RustGIS-<version>-macos-x86_64.dmg` |
| Linux x64 / arm64 (Debian, Ubuntu) | `rustgis_<version>_amd64.deb` / `rustgis_<version>_arm64.deb` |
| Linux x64 / arm64 (Fedora, RHEL, openSUSE) | `rustgis-<version>-1.x86_64.rpm` / `rustgis-<version>-1.aarch64.rpm` |
| Linux x64 / arm64 (any distribution, no install) | `RustGIS-<version>-linux-x86_64.AppImage` / `RustGIS-<version>-linux-aarch64.AppImage` |
| Linux x64 / arm64 (other) | `RustGIS-<version>-linux-x86_64.tar.gz` / `RustGIS-<version>-linux-aarch64.tar.gz` |

On macOS the command-line tool is a separate `rustgis-cli-<version>-macos-<arch>.tar.gz`;
the other packages include it (the Windows installers and the MSIX add `rustgis-cli` to every
new terminal). `SHA256SUMS` lists the checksum of every file.

The AppImages carry update information, so tools such as Gear Lever can update them to the
latest release. AppImageUpdate, and AppImageLauncher through the AppImageUpdate library, download
only the parts that changed, with the `.zsync` file published beside each AppImage.

On Windows, `setup.exe` installs for your account in `%LOCALAPPDATA%\Programs\RustGIS` and needs
no admin rights; the `.msi` installs for every user in `Program Files` and suits managed
deployment (`msiexec /i RustGIS-<version>-windows-<arch>.msi /qn`, where `<arch>` is `x86_64` or
`aarch64`). Both add a Start menu
entry, open `.rgis` projects, list RustGIS under **Open with** for `.geojson`, and
uninstall from **Settings › Apps**.

RustGIS installed with `setup.exe` (by hand or by winget, which runs it) updates itself: **Help ›
Check for Updates** shows a newer release's version and notes, and **Download and Install**
downloads that release's `setup.exe` for your computer and checks it against the release's
`SHA256SUMS` before it may run. That catches a damaged or incomplete download; it can't tell a
replaced release, whose checksums would be replaced too, until Windows builds are signed.
**Install and Restart** then closes RustGIS (asking to save unsaved changes first), installs the
update over it and opens it again; settings, plugins and data stay where they are. Chosen and
then cancelled at the save question, the update installs the next time RustGIS closes. The next
start says whether the new version arrived. The `.msi`, the Store and the portable zip update
their own way.

The `.msix` is the package submitted to the Microsoft Store, which signs it. It is unsigned
on the release page, so Windows only installs it from the
[Store](https://apps.microsoft.com/detail/9ppjw8h561lq); outside the Store, download an installer
or the portable zip instead.

The macOS app needs macOS 11 or later. It is signed and notarized by Apple, so it opens without a
warning; the command-line tool is too, but macOS checks it online the first time it runs. The
`setup.exe`, `.msi` and portable zip are not signed: if SmartScreen warns about an unknown
publisher, choose **More info › Run anyway**.

## Package managers

RustGIS is also published to these package managers, which keep it up to date:

| Platform | Command |
| --- | --- |
| Windows x64 ([winget](https://learn.microsoft.com/windows/package-manager/)) | `winget install OpenGeos.RustGIS` (x64 only for now: on ARM64, use the ARM64 setup.exe or .msi above) |
| macOS and Linux ([Homebrew](https://github.com/Homebrew/homebrew-cask/blob/main/Casks/r/rustgis.rb)) | `brew install --cask rustgis` (the DMG on macOS, the AppImage on Linux) |
| macOS and Linux, command-line tool only (Homebrew) | `brew install opengeos/rustgis/rustgis-cli` |
| Arch Linux ([AUR](https://aur.archlinux.org/packages/rustgis-bin)) | `yay -S rustgis-bin` (or any AUR helper) |
| Fedora ([COPR](https://copr.fedorainfracloud.org/coprs/giswqs/rustgis/)) | `sudo dnf copr enable giswqs/rustgis && sudo dnf install rustgis` |

winget installs for your account with the setup.exe; add `--scope machine` for the `.msi`. Close
RustGIS and `rustgis-cli` before `winget upgrade`: the setup.exe, run silently, waits 30 seconds
for each to close and otherwise stops without installing (exit code 2), rather than replace only
part of it. The same goes for Help › Check for Updates › Install and Restart: close any other
RustGIS window first, or the update waits for it and then isn't installed (RustGIS says so).
The `rustgis` cask is in Homebrew's own repository, so it needs no tap. The `rustgis-cli`
formula comes from the opengeos/rustgis tap; installing it by that full name trusts just that
formula, with no separate `brew trust` step. If you installed the app from that tap
before (`opengeos/rustgis/rustgis`), it keeps updating from there; to move to Homebrew's cask,
run `brew uninstall --cask opengeos/rustgis/rustgis && brew install --cask rustgis`.
Each package is the same build as the release page, and a new release reaches the package
managers shortly after it is published (winget after Microsoft's review).

## From source

RustGIS builds from source with a recent stable Rust toolchain (1.90 or later), a C compiler (for
the C code some dependencies bundle; see [Architecture](architecture.md#pure-rust)) and `curl`
(to download the sample data).

```sh
git clone https://github.com/opengeos/RustGIS.git
cd RustGIS
cargo run --release -p rustgis            # the desktop app
cargo build --release -p rustgis-cli      # the command-line tool
```

The build downloads the Natural Earth sample data from
[rustgis-assets](https://github.com/opengeos/rustgis-assets). To build offline, download
`countries.geojson`, `cities.geojson`, `rivers.geojson` and `lakes.geojson` from `https://assets.rustgis.app/data/` once and point the build at them:

```sh
RUSTGIS_DATA_DIR=/path/to/data cargo build --release
```

## Platforms

| Platform | Status |
| --- | --- |
| Linux (X11 and Wayland) | Supported |
| macOS | Supported |
| Windows | Supported |
| Web browser | Supported: [web.rustgis.app](https://web.rustgis.app) (WebGPU or WebGL2), installable as an app on iOS, Android and desktops ([how](#install-the-web-app)) |

## Web app

RustGIS also runs in the browser, compiled to WebAssembly: open
**[web.rustgis.app](https://web.rustgis.app)**. It is the same app, with these differences:

- **Open** and **Add Data** use the browser's file picker, and you can drop files on the page.
  GeoJSON, CSV, GeoTIFF, ASCII grids, `.rgis` projects and the other formats in
  [Data formats](user-guide/data-formats.md) work; pick or drop a Shapefile's `.shp` and `.dbf`
  together (with its `.shx` and `.prj` if it has them), or its zip.
- **Save** and **Export** download the file. **Save As**, and the first **Save** of a project,
  ask for the file name first; the browser decides which folder downloads go to (Firefox, for
  one, saves to its Downloads folder unless told to ask). A project opened from a file is named
  after it, and **Save** downloads it under that name again. The browser doesn't tell RustGIS
  whether it saved a download (its save prompt can be cancelled), so **Save** keeps any unsaved
  changes, and **Save As** to a different name makes the project unsaved. Closing or leaving
  the tab asks first while there are unsaved changes; an unchanged project stays clean.
- The interface uses the built-in font, because a web page can't read installed fonts. Choosing
  Chinese, Japanese, Korean or Persian downloads a font for that script.
- Process plugins (including Python plugins), and the formats read through GDAL or DuckDB,
  need the desktop app or `rustgis-cli`.

The web app opens the way **Settings › Start-up** says (remembered in the browser). The URL can
override that for one visit: add `?blank` to start with an empty project,
`?projection=EPSG:3010` (a supported projected EPSG or ESRI code, a preset id such as `robinson`, or `globe`) to
start in another projection, `?extent=10.5,55,24.5,69.5` (west, south, east, north in degrees) to
open on an area, `?lang=de` (or any language code) to pick the interface language, `?gpu` (or
`?gpu=0`) to turn GPU Rendering on (off) for the visit, or `?webgl` to force
WebGL2. Combine them with `&`, so a self-hosted build can link to
`?blank&projection=EPSG:3010&extent=10.5,55,24.5,69.5`.

`?registry=<address>` opens the Plugin Manager's Browse tab on another plugin registry for the
visit; installing still asks first.

Links can carry data too: `?data=<address>` adds a file by its web address (any format Add Data
reads, downloaded in the background; repeat it for up to 16, and the map zooms to the first that
lands with something to show), `?style=<address>` adds a MapLibre style (opening at the style's
own place, zoom, bearing and pitch unless the link places the map), `?tiles=<template>`
a tile layer, and `?zarr=<address>` opens Add Zarr Variable on a Zarr store. Percent-encode an address holding `&`, `#` or spaces (`encodeURIComponent`); a `+`
stays a `+`. The server must allow the browser to fetch the file (CORS); a file that can't be
added is said in a notice. For example,
`?blank&data=https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/2.5_week.geojson` opens
the past week's earthquakes.

A MapLibre-style position after a `#` places the map: `#zoom/latitude/longitude/bearing/pitch`,
the last two optional, so `?style=<address>#15.5/40.7484/-73.9857/30/60` opens a style tilted over
Midtown Manhattan.

`?project=<address>` opens a whole project by its web address: a `.rgis` file (or a
`.rgispkg` package), or any address that answers with a project's JSON, such as an API that
builds the project on the fly. Its maps, layers, symbology and view open as saved, so one link
shares a styled map. The project replaces the start-up one, so `?data=`, `?stac=`, `?style=`,
`?tiles=` and `?zarr=` are left out beside it (a notice says so). The data the project points to
is read as it would be from a saved file, and its servers must allow the browser to fetch it
(CORS), as must the project's.

`?stac=<address>` opens STAC Search on a [STAC](https://stacspec.org) item or collection: an
item's footprint is drawn and its default asset (its true-colour `visual` image, else its first
GeoTIFF) added, signed first when it is a Microsoft Planetary Computer one; a collection is chosen
in the dialog, with its API, ready to search. For example,
`?stac=https://planetarycomputer.microsoft.com/api/stac/v1/collections/sentinel-2-l2a/items/S2B_MSIL2A_20240727T102559_R108_T32TLS_20240727T131405`
opens a Sentinel-2 scene of Lake Geneva.

`?embed` runs the app inside another page, as the [Python package's notebook widget](python.md)
does: it starts blank, keeps its settings in memory rather than in the browser, and exchanges
the project with the page that frames it through `postMessage` (the messages are described in
`apps/rustgis-web/src/embed.rs`).

### Install the web app

The web app installs like an app on phones, tablets and computers, with its own icon and a window
without the browser's toolbars, and starts without a network connection once it has been opened
online. Nothing is downloaded from an app store, and it updates itself from web.rustgis.app.

| Platform | How |
| --- | --- |
| iPhone and iPad | Open web.rustgis.app in Safari, tap **Share**, then **Add to Home Screen** |
| Android | Open it in Chrome, tap the **⋮** menu, then **Install app** (or **Add to Home screen**) |
| Windows, macOS, Linux, ChromeOS | In Chrome or Edge, click the install icon at the right of the address bar (or **⋮ › Cast, save and share › Install page as app**) |
| Mac (Safari 17 or later) | **File › Add to Dock** |

To remove it, delete it like any other app: long-press the icon on a phone or tablet, or use
**Uninstall** in the app window's menu on a computer.

Offline, the installed app opens the World Atlas sample and the files you open, but not basemap
tiles, web data, plugins or the AI Assistant, which need the network. When a new version has been
published while the app is open, a notice offers to reload into it; otherwise the next launch
picks it up. Two things to know on iPhone and iPad: iOS gives a web page less memory than a
desktop browser, so very large rasters may not open, and it can clear the offline copy when the
device runs short of storage, so the next launch then needs a connection.

To build it yourself, install [Trunk](https://trunkrs.dev), the `wasm32-unknown-unknown` target
and clang (it compiles the Zstandard decoder used for GeoTIFFs to WebAssembly). Linux
distributions' clang can; on macOS, Apple's can't, so install `llvm` with Homebrew and put its
`bin` first on your `PATH`. Then:

```sh
cd apps/rustgis-web
trunk serve          # http://127.0.0.1:8772
trunk build --release   # output in dist/web
cargo xtask web-pwa ../../dist/web   # add the service worker (offline start), as the deploy does
```

The service worker is left out of `trunk serve`, so a development build is never served from a
stale cache.

### Self-host the static build

Download `RustGIS-<version>-web.zip` from the
[GitHub release](https://github.com/opengeos/RustGIS/releases) and unzip it into a directory of
its own (or the document root, if the server serves nothing else). Serve it over HTTP, rather
than opening `index.html` as a local file. For a local preview, run `python3 -m http.server 8080`
from the extracted directory and open `http://localhost:8080/`. Browsers do not let a page opened
as a local file start the app, so it shows these steps instead.

The archive uses relative URLs, so it can be hosted at the root or under a path such as
`https://example.com/rustgis/` (with the trailing slash). Use HTTPS, or `localhost` for local
use, to install the app and start it offline. The ZIP includes the service worker and contains
no analytics tag.

### Self-host the web app with Docker

A ready-made image of the web app, served by nginx, is published to the GitHub Container
Registry for x86-64 and arm64, so you need only [Docker](https://docs.docker.com/get-docker/):

```sh
docker run --rm -p 8080:8080 ghcr.io/opengeos/rustgis   # http://localhost:8080
```

`latest` (also tagged `edge`) is the newest build of `main`; `0.5.0` or `0.5` is a given
release (releases have images from 0.5.0 on).

To build the image yourself, the `Dockerfile` at the root of the repository builds the web app
the way the deploy does:

```sh
docker build -t rustgis .
docker run --rm -p 8080:8080 rustgis
```

The image holds only the built files and runs nginx as a non-root user on port 8080. Its pages
check for a newer build on every load, so the app's new-version notice offers the next image you
deploy. Unlike web.rustgis.app, it carries no analytics. Put it behind a reverse proxy that
serves HTTPS: browsers install the app and start its service worker only over HTTPS (or on
`localhost`). The app's addresses are relative, so the proxy can also serve it under a path,
such as `https://example.com/rustgis/`, passing requests on without the path. Signing in to
ArcGIS from your copy needs an OAuth app of your own whose redirect URLs list the copy's
`oauth-callback.html`, as RustGIS's own app accepts only web.rustgis.app's (see
[Secured ArcGIS services](user-guide/data-formats.md#secured-arcgis-services)).

The image is for the platform of the machine that builds it. To build it for another, such as
on an Apple silicon Mac for an x86-64 server, add `--platform linux/amd64` to `docker build`.
With a builder that can build several platforms (one made by `docker buildx create --use`, or
Docker Desktop's containerd image store),
`docker buildx build --platform linux/amd64,linux/arm64 -t <registry>/rustgis --push .`
publishes one image for both. The app itself is WebAssembly, so it is compiled once, on the
machine that builds it, whichever platforms the image is for.

### The command line in Docker

`ghcr.io/opengeos/rustgis-cli` holds the [command line](cli.md), `rustgis-cli`, on a slim Debian,
for x86-64 and arm64, with the same tags. It runs in `/data`, so mount the folder to work on
there:

```sh
docker run --rm -v "$PWD:/data" ghcr.io/opengeos/rustgis-cli info cities.geojson
docker run --rm -v "$PWD:/data" ghcr.io/opengeos/rustgis-cli render cities.geojson map.png --basemap ocean
```

It runs as a user with ID 1000, the first user on most Linux systems, so the files it writes are
yours; elsewhere, add `--user "$(id -u):$(id -g)" -e HOME=/tmp`. The optional GDAL, DuckDB and
GeoLibre programs aren't in the image. To build it yourself:
`docker build -f packaging/docker/cli.Dockerfile -t rustgis-cli .`

## Fonts

RustGIS uses an installed sans-serif when it finds one (Segoe UI, San Francisco, Arial, Inter,
Noto Sans, Open Sans or DejaVu Sans) and falls back to a built-in face. Set
`RUSTGIS_BUILTIN_FONTS=1` to always use the built-in face.

## Settings, data and cache folders

The desktop app and the command line keep their files in three folders:

| Folder | Holds | Windows | macOS | Linux |
| --- | --- | --- | --- | --- |
| Settings | preferences (`prefs.json`), the crash log, AI Assistant sign-ins and skills | `%APPDATA%\RustGIS` | `~/Library/Application Support/RustGIS` | `$XDG_CONFIG_HOME/rustgis` (`~/.config/rustgis`) |
| Data | installed plugins, the GeoLibre runner and recovery copies of unsaved work | `%LOCALAPPDATA%\RustGIS` | `~/Library/Application Support/RustGIS` | `$XDG_DATA_HOME/rustgis` (`~/.local/share/rustgis`) |
| Cache | GDAL and DuckDB conversions | `%LOCALAPPDATA%\RustGIS\cache` | `~/Library/Caches/RustGIS` | `$XDG_CACHE_HOME/rustgis` (`~/.cache/rustgis`) |

Set `RUSTGIS_CONFIG_DIR`, `RUSTGIS_APP_DATA_DIR` or `RUSTGIS_CACHE_DIR` to use another folder
instead, for a portable install on a USB drive, for example. The folder is used as is, and RustGIS
keeps the same files in it (`plugins/`, `geolibre/` and `recovery/` in the data folder, `gdal/` and `duckdb/`
in the cache folder). `RUSTGIS_PLUGINS_DIR`, `RUSTGIS_GDAL_CACHE` and `RUSTGIS_DUCKDB_CACHE` name
the plugins folder and the GDAL and DuckDB caches themselves, ahead of these.

## If RustGIS closes unexpectedly

The desktop app writes its internal errors to `crash.log` in its
[settings folder](#settings-data-and-cache-folders): one that closed the app, and also one it
recovered from (an error in the AI Assistant, for example, stops the request and says so in the
chat instead of closing the app). The next launch moves the log to `crash-last.log` and names the
file in a notice.

Attach it to a [bug report](https://github.com/opengeos/RustGIS/issues/new?template=bug_report.yml).
It holds the version, the operating system, the error message and where in RustGIS it happened.
An error message can quote what RustGIS was working on, such as a file path or a layer name, so
read the log first and remove anything you don't want to share.

Unsaved work isn't lost with it: the next launch offers the copy autosave kept (see
[Autosave and recovery](user-guide/workspace.md#autosave-and-recovery)).

## If the app doesn't start

The desktop app draws its window with the graphics card through Direct3D 12 on Windows, Metal on
macOS and Vulkan on Linux, with OpenGL as a fallback. A graphics driver that crashes there closes
the app before it can open a window or write `crash.log`. Choose another graphics interface with
`--backend` (`dx12`, `vulkan`, `gl` or `metal`, or several separated by commas), from a terminal
or in the target of a shortcut to `rustgis.exe`:

```sh
rustgis --backend gl       # OpenGL
rustgis --backend dx12     # Direct3D 12 (Windows)
rustgis --backend vulkan   # Vulkan
rustgis --backend metal    # Metal (macOS)
```

The `WGPU_BACKEND` environment variable takes the same names, for every launch. On Windows,
RustGIS uses Vulkan only when asked like this: some Vulkan drivers, such as Intel's for UHD 600
series graphics, crash as the app starts. Updating the graphics driver may also help.
