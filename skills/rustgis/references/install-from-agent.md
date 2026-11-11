# Install RustGIS for the user and connect an agent

Run these steps on the **user's desktop machine**, not an unrelated agent host.
Install the desktop app and `rustgis-cli`; registering MCP alone installs neither.
This procedure follows <https://rustgis.app/installation/> and the repository's
`docs/installation.md` and `docs/mcp.md`.

## 1. Detect the platform and existing installation

On Linux or macOS:

```sh
uname -s
uname -m
command -v rustgis-cli || true
command -v brew || true
# Linux only:
cat /etc/os-release
command -v apt || command -v dnf || command -v zypper || command -v yay || true
```

Map `x86_64` to release architecture `x86_64` (Debian `amd64`), and
`arm64`/`aarch64` to `aarch64` (Debian `arm64`). On Apple Silicon, use a native
terminal so `uname -m` is not reporting a Rosetta-emulated Intel process.

On Windows, use PowerShell:

```powershell
[System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
Get-Command rustgis-cli, winget -ErrorAction SilentlyContinue
```

The documented Windows release assets are x64 (`windows-x86_64`) and, where a
release has them, ARM64 (`windows-aarch64`). On ARM64, use the ARM64 asset when
the release has one (winget installs x64 only). If an existing CLI works, keep
it; also check that the desktop app is installed. Prefer a package manager
already present. Do not install a package manager just to avoid a release
download, and do not silently elevate privileges.

## 2. Install the desktop app and CLI

### Package-manager route

Run the appropriate platform row, not every command:

| Platform | Agent-runnable commands |
| --- | --- |
| Windows x64 | `winget install OpenGeos.RustGIS` |
| macOS | `brew install --cask rustgis` then `brew install opengeos/rustgis/rustgis-cli` |
| Linux with Homebrew | `brew install --cask rustgis` (the AppImage) then `brew install opengeos/rustgis/rustgis-cli` |
| Arch Linux with an AUR helper | `yay -S rustgis-bin` |
| Fedora | `sudo dnf copr enable giswqs/rustgis && sudo dnf install rustgis` |

The `rustgis` cask needs no tap. The `rustgis-cli` formula comes from the
opengeos/rustgis tap; installing it by that full name trusts just that formula,
so don't run `brew trust` on the whole tap.
winget's default is the per-user installer; use
`--scope machine` only for an authorized all-user MSI installation.
If elevation needs the user's password, leave that interaction to the user;
the portable route below does not require system-wide installation.

Package managers handle their package integrity checks. For direct downloads,
verify each downloaded asset against that same release's `SHA256SUMS` before
running or extracting it. A checksum verifies the asset against the published
manifest; it is not an independent publisher signature.

### Checked release downloads: Linux and macOS

Use the current release at <https://github.com/opengeos/RustGIS/releases/latest>,
or its mirror <https://downloads.rustgis.app>. Discover the version instead of
pinning an old release. The following Bash setup requires `curl` and `jq`; if
those are unavailable, use the agent's HTTP/JSON tools to obtain the same
release metadata and downloads rather than guessing asset URLs.
Run in a fresh download directory so unrelated files are not overwritten:

```bash
set -euo pipefail
work=$(mktemp -d)
cd "$work"
curl -fsSL https://api.github.com/repos/opengeos/RustGIS/releases/latest -o release.json
version=$(jq -er '.tag_name | ltrimstr("v")' release.json)
case "$(uname -m)" in
  x86_64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) echo 'No documented release asset for this architecture' >&2; exit 1 ;;
esac
manifest_url=$(jq -er '.assets[] | select(.name == "SHA256SUMS") | .browser_download_url' release.json)
curl -fsSL "$manifest_url" -o SHA256SUMS
fetch_checked() {
  local asset="$1" url expected actual
  url=$(jq -er --arg name "$asset" '.assets[] | select(.name == $name) | .browser_download_url' release.json)
  curl -fsSL "$url" -o "$asset"
  expected=$(while read -r hash name; do
    if [ "${name#\*}" = "$asset" ]; then printf '%s' "$hash"; fi
  done < SHA256SUMS)
  [ -n "$expected" ] || { echo "Missing checksum: $asset" >&2; return 1; }
  if command -v sha256sum >/dev/null; then
    actual=$(sha256sum "$asset")
  else
    actual=$(shasum -a 256 "$asset")
  fi
  [ "${actual%% *}" = "$expected" ] || { echo "Checksum mismatch: $asset" >&2; return 1; }
}
```

If the release has no matching asset or checksum, stop that installation route;
do not execute the unverified download. Use one of the following install paths
in the same shell so `version`, `arch` and `fetch_checked` remain available.

**Debian/Ubuntu:**

```bash
case "$arch" in x86_64) debarch=amd64 ;; aarch64) debarch=arm64 ;; esac
asset="rustgis_${version}_${debarch}.deb"
fetch_checked "$asset"
sudo apt install "./$asset"
```

**RPM distributions:**

```bash
asset="rustgis-${version}-1.${arch}.rpm"
fetch_checked "$asset"
# Fedora/RHEL:
sudo dnf install "./$asset"
# On openSUSE use this instead:
# sudo zypper install "./$asset"
```

**Linux portable installation without administrator rights:** the tarball
includes both executables, unlike macOS's separate CLI archive.

```bash
asset="RustGIS-${version}-linux-${arch}.tar.gz"
fetch_checked "$asset"
mkdir -p "$HOME/.local/opt/rustgis/$version" "$HOME/.local/bin"
tar -xzf "$asset" -C "$HOME/.local/opt/rustgis/$version" --strip-components=1
ln -sfn "$HOME/.local/opt/rustgis/$version/rustgis" "$HOME/.local/bin/rustgis"
ln -sfn "$HOME/.local/opt/rustgis/$version/rustgis-cli" "$HOME/.local/bin/rustgis-cli"
export PATH="$HOME/.local/bin:$PATH"
```

Check existing destinations before replacing symlinks; do not overwrite another
installation without the user's intent to update it. Make the PATH addition
persistent in the user's shell configuration only if needed, without duplicating
an existing entry. GUI-launched MCP clients may not inherit shell configuration;
a full executable path in MCP configuration avoids that problem.

The alternative Linux AppImage is
`RustGIS-${version}-linux-${arch}.AppImage`. Verify it with `fetch_checked`,
then `chmod +x` and run it. For an agent needing a conventional `rustgis-cli`
on PATH, prefer the package or tarball route above.

**macOS:** download **both** the app DMG and CLI tarball. This example installs
for the current user without writing to system-wide `/Applications`:

```bash
dmg="RustGIS-${version}-macos-${arch}.dmg"
cli="rustgis-cli-${version}-macos-${arch}.tar.gz"
fetch_checked "$dmg"
fetch_checked "$cli"
mkdir -p "$work/mount" "$HOME/Applications" "$HOME/.local/bin" "$HOME/.local/opt/rustgis-cli/$version"
hdiutil attach "$dmg" -nobrowse -mountpoint "$work/mount"
ditto "$work/mount/RustGIS.app" "$HOME/Applications/RustGIS.app"
hdiutil detach "$work/mount"
tar -xzf "$cli" -C "$HOME/.local/opt/rustgis-cli/$version" --strip-components=1
ln -sfn "$HOME/.local/opt/rustgis-cli/$version/rustgis-cli" "$HOME/.local/bin/rustgis-cli"
export PATH="$HOME/.local/bin:$PATH"
open "$HOME/Applications/RustGIS.app"
```

Check for an existing app before copying over it. The macOS releases are
Developer ID signed and notarized. Prefer connecting to the internet and
retrying if Apple cannot verify an old release; do not routinely strip quarantine.

### Checked release downloads: Windows x64 and ARM64

On x64, prefer winget when present. On ARM64, winget installs the x64 build (run
under emulation), so prefer the ARM64 zip below when the release has one. The
portable zip needs no administrator rights and includes both the app and CLI.
On ARM64 Windows the script takes the ARM64 zip when the release has one, else
the x64 one. In PowerShell:

```powershell
$ErrorActionPreference = 'Stop'
$release = Invoke-RestMethod 'https://api.github.com/repos/opengeos/RustGIS/releases/latest'
$version = $release.tag_name -replace '^v', ''
$arch = 'x86_64'
$arm = "RustGIS-$version-windows-aarch64-portable.zip"
$os = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($os -eq 'Arm64' -and ($release.assets.name -contains $arm)) {
  $arch = 'aarch64'
}
$name = "RustGIS-$version-windows-$arch-portable.zip"
$asset = $release.assets | Where-Object name -eq $name
$manifest = $release.assets | Where-Object name -eq 'SHA256SUMS'
if (!$asset -or !$manifest) { throw 'Release asset or SHA256SUMS is missing' }
$work = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $work | Out-Null
$zip = Join-Path $work $name
$sums = Join-Path $work 'SHA256SUMS'
Invoke-WebRequest $asset.browser_download_url -OutFile $zip
Invoke-WebRequest $manifest.browser_download_url -OutFile $sums
$pattern = '^([0-9a-fA-F]{64})\s+\*?' + [regex]::Escape($name) + '$'
$line = Get-Content $sums | Where-Object { $_ -match $pattern }
if (@($line).Count -ne 1) { throw 'Expected exactly one checksum for the asset' }
$expected = [regex]::Match($line, $pattern).Groups[1].Value
if ((Get-FileHash $zip -Algorithm SHA256).Hash -ine $expected) { throw 'Checksum mismatch' }
$destination = Join-Path $env:LOCALAPPDATA "Programs\RustGIS-portable\$version"
if (Test-Path $destination) { throw 'Destination exists; inspect it before updating' }
Expand-Archive $zip -DestinationPath $destination
$bin = Join-Path $destination "RustGIS-$version-windows-$arch"
$env:Path = "$bin;$env:Path"
& (Join-Path $bin 'rustgis-cli.exe') commands
if ($LASTEXITCODE -ne 0) { throw 'RustGIS CLI failed' }
Start-Process (Join-Path $bin 'rustgis.exe')
```

Use `$bin\rustgis-cli.exe` as the MCP command if the client does not inherit the
updated PATH. For a permanent per-user PATH entry, preserve the existing value
and avoid duplicates, or let the per-user installer manage it. The alternative
`RustGIS-<version>-windows-<arch>-setup.exe` installs into
`%LOCALAPPDATA%\Programs\RustGIS`. Verify its checksum before launching its
installer. For authorized all-user deployment, the verified MSI uses
`msiexec /i RustGIS-<version>-windows-<arch>.msi /qn` (`<arch>` is `x86_64` or
`aarch64`).
Do not sideload the unsigned release-page MSIX; use the Microsoft Store for
that package. Windows installers may show a SmartScreen unknown-publisher
warning; do not disable security controls to make installation unattended.

### Source installation when needed

Use source only when a matching release is unsuitable or the required MCP Relay
is not present in the installed app/CLI. Use a checkout that includes the
[documented open-app support](https://rustgis.app/mcp/), with stable Rust 1.90 or
later. In a user-chosen source directory:

```sh
git clone https://github.com/opengeos/RustGIS.git
cd RustGIS
cargo build --release -p rustgis-cli
cargo run --release -p rustgis
```

The last command launches the desktop app and stays running; use another
terminal for the agent. Register the full path to `target/release/rustgis-cli`
(`rustgis-cli.exe` on Windows), or add that directory to PATH. The build fetches
Natural Earth sample data. For offline builds, follow the `RUSTGIS_DATA_DIR`
procedure in the installation documentation. If neither the release nor the
available source exposes MCP Relay and `--attach`, report the missing capability
instead of substituting a private MCP session.

## 3. Confirm the CLI and register MCP

In the terminal/environment the MCP client will use:

```sh
command -v rustgis-cli
rustgis-cli commands
```

On Windows:

```powershell
Get-Command rustgis-cli
rustgis-cli commands
```

Require successful command discovery and a successful command listing. A GUI
app alone is not enough, especially on macOS. Restart terminals after installer
PATH changes; use the resolved full executable path if the agent still cannot
find it.

MCP uses **stdio**, not an HTTP endpoint. The client launches the following
command; do not run a second detached server process:

```sh
rustgis-cli mcp --sample              # a private World Atlas project
rustgis-cli mcp --data parcels.geojson # private input; repeat --data for more files
rustgis-cli mcp --project map.rgis    # a private project from disk
rustgis-cli mcp --attach              # the open desktop app; start its relay first
```

Choose private or attached mode deliberately. The private project is held in
memory until saved. Add `--yes` only for authorized file/network work. For the
open app, follow section 4 before connecting, and register these commands:

```sh
claude mcp add rustgis-app -- rustgis-cli mcp --attach
codex mcp add rustgis-app -- rustgis-cli mcp --attach
```

For a **private** session instead, the canonical registration is:

```sh
claude mcp add rustgis -- rustgis-cli mcp --sample
codex mcp add rustgis -- rustgis-cli mcp --sample
```

Claude Desktop's `claude_desktop_config.json`, Cursor's `~/.cursor/mcp.json`,
and other `mcpServers` clients can attach with this entry:

```json
{
  "mcpServers": {
    "rustgis-app": {
      "command": "rustgis-cli",
      "args": ["mcp", "--attach"]
    }
  }
}
```

VS Code uses `.vscode/mcp.json` with a different outer key:

```json
{
  "servers": {
    "rustgis-app": {
      "type": "stdio",
      "command": "rustgis-cli",
      "args": ["mcp", "--attach"]
    }
  }
}
```

Merge the entry with the user's existing configuration; do not replace other
servers. Replace `command` with the full CLI path when needed (escape Windows
backslashes in JSON). For file/network access, append `"--yes"` to `args`, or
append `--yes` to the registration command. For a private sample, replace
`"--attach"` with `"--sample"` rather than adding both. Reload the client's
MCP configuration or start a new session, as that client requires.

## 4. Manage the already-open desktop app

1. Launch the installed RustGIS desktop app in the user's graphical session,
   or keep the existing window and project. On Linux use `rustgis`; on macOS
   open RustGIS.app; on Windows use its Start menu entry or `rustgis.exe`.
2. In RustGIS, open **Command Search (Ctrl+K)** and run **MCP Relay**
   (`ui.mcpRelay`). If the agent cannot operate the UI, ask the user to perform
   this one action; an unattached MCP server cannot start the app's listener.
   Confirm the status bar says `MCP relay listening on 127.0.0.1:8767`.
3. Have the MCP client launch the attached command:

   ```sh
   rustgis-cli mcp --attach                 # the open app on 127.0.0.1:8767
   rustgis-cli mcp --attach --yes           # also allow file and network access
   ```

   The listener binds `127.0.0.1` only, on port **8767**, and remains active
   until the desktop app closes. This is local external-agent management,
   not an internet-facing service or an HTTP bridge. The agent must run on
   the desktop machine. To use another port, start the app command with
   `ui.mcpRelay {"port": 9000}` and connect with
   `rustgis-cli mcp --attach 9000 --yes`.
4. Call `list_layers` and `view_map` and compare them with the user's open
   project. Discover commands with `search_commands` and geoprocessing with
   `search_tools`/`describe_tool`; execute with `run_command`/`run_tool`.
   Inspect `layersAdded`, `notices` and the resulting map before claiming
   success. Each run is one undo step on the actual open project.
5. Save/export only to the user's intended destination. Opening, replacing,
   undoing and saving now act on **the user's open project**, not a private
   agent project. File and network commands **still require `--yes`**;
   attaching does not bypass the MCP access policy. Plugin management,
   scripts, tasks, models and the GeoLibre runner remain unavailable.

If connection fails, check the window is open, the status bar confirms the
relay, the port matches, and both installed executables support this feature.
Do not combine `--attach` with `--sample`, `--project` or `--data`: the CLI
rejects mixing an attached session with private-project startup inputs. Do not
report a successful private sample session as proof of controlling the app.
