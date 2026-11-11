# Command Line

`rustgis-cli` runs RustGIS engine commands headlessly, using the same ids as the app.
Window and pane commands (`ui.*`) belong to the app and are not available in a private CLI session.

```sh
rustgis-cli commands                  # every command id with its parameters
rustgis-cli tools                     # geoprocessing tools
rustgis-cli info data.geojson         # project and layer summary as JSON
rustgis-cli render data.geojson map.png --basemap ocean --width 1600 --height 900
rustgis-cli render data.geojson map.png --tiles 'https://tile.openstreetmap.org/{z}/{x}/{y}.png'
rustgis-cli logo icon.png --size 256  # the app icon as a PNG
rustgis-cli plugins                   # installed plugins (also: browse, get, install, uninstall…)
rustgis-cli skills                    # the AI Assistant's skills (also: add, remove, enable, disable)
```

`commands` lists the engine commands and their JSON parameters; `tools` lists geoprocessing
tools and their parameters. See [Installation](installation.md) for the CLI packages, and
[The command line in Docker](installation.md#the-command-line-in-docker) for its image.

## Rendering

`render <data|project> <out.png>` opens a data file or saved project and exports its map.
It defaults to 1600 × 900 pixels at 96 DPI; `--width`, `--height` and `--dpi` change those.
A data file is zoomed to its full extent; a project keeps its saved view.
`--tiles` accepts an XYZ template, vector tiles or a MapLibre-style address.

`--worldview auto|default|CODE` chooses the disputed borders in the built-in countries and
world outline, for `render`, `run`, `ask` and `mcp`: the default borders, or a
[worldview](user-guide/maps.md#worldview)'s code, such as `chn`, `ind` or `usa`. A worldview
other than the default is downloaded once and kept in the cache folder; if the download fails,
a named one is an error. `auto` follows `RUSTGIS_WORLDVIEW`, else the system time zone and
locale (China's view in mainland China, falling back to the default borders with a warning when
it can't be downloaded). `--worldview-detail simplified|original` chooses how detailed: the
simplified borders (the default, about 250 KB) or Natural Earth's 1:10m ones (a 3 MB download),
which `--worldview default` also downloads. Given explicitly, it makes a failed download an
error even with `auto`. Nothing is downloaded by `ask`'s model and sign-in options, by `ask`
with no request unless it reads or saves a project (with `--account` and no request, not even
then), nor by `mcp --attach`.

`--threads N` sets the most threads one piece of work uses (1 to 1024, never more than the
cores), as [`gp.threads`](user-guide/analysis.md#analysis-workflows) sets it. With `mcp --attach` the
open app's own setting holds, so `--threads` is refused there. Without it, a run takes
the computer's cores, up to 16.

## Chaining commands

`run` starts with an empty project. `--sample` loads the World Atlas sample, `--project <path>`
opens a project, and repeatable `--data <path>` adds files, in that order. Repeat `--cmd` to run
commands in order; `--save <path>` saves the resulting project:

```sh
rustgis-cli run --sample \
  --cmd 'gp.run={"tool":"countPoints","params":{"polygons":"Countries","points":"Cities"}}' \
  --cmd 'layer.symbology={"layer":"Countries_Summarize","type":"graduated","field":"point_count"}' \
  --cmd 'bookmark.go={"name":"Europe"}' \
  --cmd 'share.exportMap={"path":"europe.png","dpi":192}' \
  --save atlas.rgis
```

Each non-null command result goes to standard output as JSON. `--cmd 'id'` runs a command with
no parameters; otherwise use `--cmd 'id={json params}'`. What a command reports beside its result,
which the app shows in its status bar, goes to standard error.

Pass `--use-saved-secrets` to load the desktop app's saved connections and let
`connection.login` and project sources use their remembered database passwords and S3 keys.
It also lets `ask` use the API key the app remembers, only for the same provider and address
when the environment supplies no key, and the ChatGPT sign-in kept in the system keychain, and
lets files on NASA's Earthdata data servers use the Earthdata Login token the app remembers
(`earthdata.login` with `"remember": true` remembers one from here, too).
Command parameters and environment variables take precedence over saved secrets. Connection
changes aren't saved back to the desktop preferences. The flag applies to `run`, `ask`, `mcp`,
`render` and `info`; for example, `rustgis-cli run --use-saved-secrets --cmd connection.list`.
Without the flag, nothing reads the keychain or desktop preferences, including in CI. Reads are
lazy: a command that doesn't need a secret neither touches the keychain nor emits a keychain
warning. If the keychain is unavailable (a headless server or locked keyring, for example), the
run continues without that saved secret and warns once on standard error. An unreadable or
malformed preferences file produces a separate warning when opting in; missing preferences are
normal. On macOS, Keychain may ask once per item whether `rustgis-cli` may read it; **Always
Allow** stops that question for the executable and item.

Nothing waits on `run` the way a window does, so a long run takes the bounds the app's panes give
it on a thread: a trend over a data cube (`cube.trend`) may compare up to 20 billion pairs, ten
times what the Script pane allows, and a trend's or a summary's STAC search up to 10,000 items,
where the app takes 2,000. A trend or a summary over time (`cube.reduce`) prints its progress to
standard error (`cube.trend: 42%`; a line every tenth when standard error isn't a terminal).
`ask` and `mcp` keep the Script pane's bounds, as an assistant or an MCP client waits on them, and
so does Python. See [Trend analysis](user-guide/analysis.md).

Installed [plugins](plugins.md) load first (`--no-plugins` skips them), and `--plugin <file or
folder>` (repeatable) loads one more for this run only. Their commands and tools have ids like
`plugin:vector-toolkit/hexagon-grid`:

```sh
rustgis-cli run --sample --plugin vector-toolkit-0.1.0.rgplugin \
  --cmd 'gp.run={"tool":"plugin:vector-toolkit/hexagon-grid","params":{"input":"Countries","size":300}}' \
  --cmd 'share.exportMap={"path":"hexagons.png"}'
```

`plugins install` and `plugins get` also prepare a Python plugin's environment;
`plugins prepare <id>` prepares it on demand, reusing it if it is already ready.
`--registry <URL>` points `plugins browse` and `plugins get` at another registry.

Rasters work the same way:

```sh
rustgis-cli render dem.tif dem.png
rustgis-cli run --data dem.tif \
  --cmd 'raster.symbology={"type":"shadedRelief","ramp":"Terrain","zFactor":1.5}' \
  --cmd 'raster.info={}' \
  --cmd 'share.exportMap={"path":"relief.png","dpi":192}'
```

<figure markdown="span">
  ![A map of Europe exported by rustgis-cli](https://assets.rustgis.app/images/cli-export.webp)
  <figcaption>A map exported by <code>rustgis-cli</code>, with no window open.</figcaption>
</figure>

## Asking the AI assistant

`ask` gives the [AI Assistant](user-guide/assistant.md) one request. It loads data like `run`,
then the model answers and acts on the project through commands; `--save` keeps the result:

```sh
export ANTHROPIC_API_KEY=…            # or OPENAI_API_KEY, GEMINI_API_KEY, OPENROUTER_API_KEY, …
rustgis-cli ask --sample "Which 5 countries have the largest GDP?"
rustgis-cli ask --sample "Buffer the cities by 200 km and label them" --save buffered.rgis
rustgis-cli ask --data parcels.geojson --provider ollama --model qwen3:8b "Summarise the fields"
rustgis-cli ask --models --provider openai          # the provider's models
rustgis-cli ask --login                             # sign in with ChatGPT to use your ChatGPT plan
rustgis-cli ask --sample --provider chatgpt "Zoom to Tokyo"
```

The provider is `--provider` (`anthropic`, `openai`, `chatgpt`, `gemini`, `openrouter`, `ollama`, `custom`, `requesty`, `trustedrouter`),
else `RUSTGIS_ASSISTANT_PROVIDER`, else the first with a key in the environment, else the app's
saved provider when `--use-saved-secrets` is set, else Ollama. ChatGPT sign-in is used only when
that provider is selected. Account management and `--models` do not read saved API keys:
model listing uses environment API keys (or the selected ChatGPT sign-in), whereas a prompt
can use a remembered API key.
`--model` (or `RUSTGIS_ASSISTANT_MODEL`) and `--base-url` override the defaults. The answer goes to
standard output and each command the assistant ran to standard error. Commands that read or write
files or use the network are declined unless you pass `--yes`; plugin commands, scripts, tasks,
models and replacing the project are always declined here. `--trace log.txt` appends each request
and response body (never the headers, which carry the key) to a file; the bodies can contain your
data and prompts. `--login` prints a browser sign-in URL so `--provider chatgpt` uses your ChatGPT
plan. `--another-account` starts a new registration, `--accounts` lists saved registrations,
`--account <client-id>` selects one, `--remove-account <client-id>` signs it out and forgets it,
and `--logout` signs out. Without `--use-saved-secrets`, the command line has no system keychain:
it uses a sign-in an earlier version saved in the settings folder, but not one the desktop app
keeps in the keychain, and its own sign-ins last for the command only. A later `--logout` or
`--remove-account` of such a sign-in clears it on this computer but can't end its session at
OpenAI (its token is gone); disconnect RustGIS in ChatGPT's settings for that. Without the flag,
those commands refuse an account whose token is in the keychain, and `--login` never replaces
such a sign-in. With `--use-saved-secrets`, the CLI uses, remembers, signs out and removes
keychain sign-ins as the app does. See
[Signing in with ChatGPT](user-guide/assistant.md#signing-in-with-chatgpt).

## Serving AI agents

`mcp` runs RustGIS as a Model Context Protocol server for agents such as Claude Code, Cursor and
VS Code, which then run commands, look at the map and search the documentation themselves:

```sh
claude mcp add rustgis -- rustgis-cli mcp --sample
```

To use the already-open desktop project instead, enable its MCP Relay and register
`rustgis-cli mcp --attach`. Do not combine `--attach` with `--sample`, `--project` or `--data`.
File and network commands require `--yes` unless the app's agent-control setting grants access.

`mcp-register --client claude` registers the open desktop app's MCP over HTTP with Claude Code,
for when agents are turned on in the app. Without `--client`, it prints the configuration;
`--name` (default `rustgis-live`) and `--port` (default `RUSTGIS_MCP_HTTP_PORT`, else 8768)
change the entry:

```sh
rustgis-cli mcp-register --client claude
```

See [MCP Server](mcp.md).
