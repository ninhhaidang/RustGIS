# MCP Server

`rustgis-cli mcp` runs RustGIS as a [Model Context Protocol](https://modelcontextprotocol.io)
server, so an AI agent such as Claude Code, Claude Desktop, Cursor, VS Code or Codex can make
maps with it. The agent opens data, runs RustGIS commands and geoprocessing tools within the
access policy below, reads attribute tables, looks at the map as an image and searches this
documentation. RustGIS needs no model API key in this mode: the MCP client supplies the model.

```sh
rustgis-cli mcp --sample                # the World Atlas sample
rustgis-cli mcp --data parcels.geojson  # your own data (--data more than once, or --project p.rgis)
rustgis-cli mcp --yes                   # also allow file and network access
rustgis-cli mcp --attach [--yes]        # the desktop app that is already open (its MCP Relay)
```

The server talks over standard input and output, so you don't start it yourself: you tell your
agent how to start it (below), and the agent runs it. Each agent gets its own project, held in
memory until the agent saves it.
`--sample`, `--project` and repeated `--data` options initialize a private project.
They cannot be combined with `--attach`, which uses the desktop app's existing project.
Use absolute paths for data, projects and executables in client configurations: clients may
start the server in a different working directory.


## Adding it to an agent

Claude Code:

```sh
claude mcp add rustgis -- rustgis-cli mcp --sample
```

Codex:

```sh
codex mcp add rustgis -- rustgis-cli mcp --sample
```

Claude Desktop (`claude_desktop_config.json`), Cursor (`~/.cursor/mcp.json`) and other clients
that use an `mcpServers` object:

```json
{
  "mcpServers": {
    "rustgis": {
      "command": "rustgis-cli",
      "args": ["mcp", "--sample"]
    }
  }
}
```

VS Code (`.vscode/mcp.json`):

```json
{
  "servers": {
    "rustgis": {
      "type": "stdio",
      "command": "rustgis-cli",
      "args": ["mcp", "--sample"]
    }
  }
}
```

If the agent can't find `rustgis-cli`, give its full path (see [Installation](installation.md)).
Then ask, for example:

- "Which 5 countries have the largest GDP? Show them on the map."
- "Buffer the cities by 200 km, colour the result by population and show me the map."
- "Load ~/data/parcels.geojson, label the parcels with their owner and export the map to
  parcels.png." (this reads and writes files, so it needs `--yes`)

## Managing the open desktop app

By default each agent gets its own project in memory. To drive the project that is open in the
desktop app instead, open **Analyze › AI Assistant**, click the **Assistant settings** (gear)
icon in the pane's header, and tick **Let your Agents Control RustGIS** below **Skills**.
These are the AI Assistant's settings, separate from the main **Settings** ribbon tab. Ticking it
starts the relay and shows what an agent needs to connect: the MCP server configuration, and the
`claude mcp add` / `codex mcp add` commands. While it's ticked, attached agents may also read and
write files and use the network, without `--yes`. RustGIS remembers the choice and starts the
relay with the app. The **MCP Relay** command (Help menu, or Command Search with Ctrl+K) starts the
relay too, without that: attached agents then have the access their `--yes` asks for.
The relay is off until the box is ticked or that command runs. While it runs, the status bar shows `MCP relay · n attached`,
where n is the number of authenticated clients. For ten seconds after an attached agent runs a
tool, the label also names it (`MCP relay · 1 attached · layer.addSample`, in red when it failed);
hovering shows `Stop MCP relay` and the last call with its parameters, and clicking it stops the
relay. Agents attached through the relay are told, when they connect, that this is the project you
have open and that you watch the map change as they work.

Under it, **Full access** (off unless you tick it, for this session only) lifts every refusal for
agents attached with the relay's own token. They may then also run scripts, tasks and models, and
install, remove and run plugins: what you could do yourself in RustGIS. It isn't remembered: it's
off again at the next launch, and when agents are turned off. While it's on, the status bar reads
`MCP relay · full access · n attached` in red. Full access also lifts the project-replacement
and undo-ownership refusals described below; it is not just file and network access.
Agents the app starts itself keep their own grants' rules.

```sh
rustgis-cli mcp --attach                 # the open app on 127.0.0.1:8767
rustgis-cli mcp --attach 9000 --yes      # another port, with file and network access
rustgis-cli mcp --attach --timeout 300   # wait up to 5 minutes for each call (5 to 600 seconds)
```

The port is 8767 unless the `RUSTGIS_MCP_PORT` environment variable names another, for the app
and for `--attach` alike (a second copy of RustGIS, or a port another program holds). When the
port is taken, the relay's error says so and names that variable; a value that isn't a port number
is an error too, not a quiet fall back to 8767. While the relay runs, the app
publishes `endpoint.json` in the relay's private folder: `schema`, `host`, `port`, `url`, `pid`,
`started_at`, the app `version`, the `token_file` path (not the token), the `attach` command, and
once MCP over HTTP runs, its `http_url` and `http_token_file`.
Setup scripts can read it instead of hard-coding the port. It's removed when the relay stops. With
two copies of RustGIS on different ports it describes the one started last, and a crash can leave
it behind: check that its `pid` is still running before trusting it.

```json
{
  "mcpServers": {
    "rustgis-app": {
      "command": "rustgis-cli",
      "args": ["mcp", "--attach", "--yes"]
    }
  }
}
```

Attached calls run on the open app's session through the same tools and refusal policy as the
private server. `snapshot` additionally supports a real window capture with `mode: "screen"`.
If the map is not on screen, it falls back to an offscreen render and explains why in `note`.
The relay holds a screen request until the app has taken the picture, and runs nothing else
meanwhile. Direct `run_command` calls to `map.reloadTiles` and `map.repairTiles` validate local
PMTiles/MBTiles archives on the desktop's existing background-read worker after admission and,
when required, approval. Their replies and audit entries wait for the command to actually land;
later entries in the same JSON-RPC batch and later relay requests wait too. Cancelling, revoking
the caller or changing the original archive target prevents installation. The original agent
still owns the resulting undo step. Arbitrary `run` scripts and file-bridge scripts retain their
existing synchronous execution contract; this does not move their child commands to the worker.
Notebook cells, task steps and Script pane scripts validate reload and repair inline on the window
thread, without reload status or cancellation during validation.
File and network commands need `--yes`, unless **Let your Agents Control RustGIS** is ticked.

Without **Full access**, an attached agent cannot replace the open project:
`project.new`, `project.open`, `project.openText`, `project.sample` and `project.fromTemplate`
are refused, with or without `--yes`. Commands are recorded in your undo history as
`agent 1`, `agent 2`… with the relay run's identity. An agent's `edit.undo` and `edit.redo`
work only on its own steps: when the top step is yours or another agent's, they are refused.
They run on their own through `run_command`, or as a one-line `run` script. You can still undo
anything. Full access lifts both these restrictions for the relay's own token.

Undo ownership and the active layer are scoped to a token, not a client connection.
Ordinary `--attach` clients share the relay's token, so they share one agent identity and
active layer; local agents with separate grants are kept apart. Selecting a layer through
an agent (`layer.select`, or a command that makes a layer active) does not change your Layers
pane selection. Once that token has chosen a layer, commands that omit `layer` use it while
you select others; until then they use your current selection.

Every tool call an attached agent makes is written to an audit log, one JSON line per call in a
file per day (`2026-10-18.jsonl`) in the `mcp-audit` folder of the app-data folder
(`~/.local/share/rustgis/mcp-audit` on Linux, `~/Library/Application Support/RustGIS/mcp-audit` on
macOS, `%LOCALAPPDATA%\RustGIS\mcp-audit` on Windows). Each line holds `ts`, `source` (`mcp` for the relay and MCP over HTTP, `file` for the file
bridge),
`tool`, `command` or `script` when there is one, `args`, `success`, `duration_s` and `error`.
Passwords, keys and tokens in the arguments, a `run` script's lines included, are replaced before
they are written. On Linux and macOS the folder and files are readable by you only; on Windows they're protected by
your user profile's permissions. A batch's calls share the batch's
`duration_s` and are marked `"batch": true`. An error whose text mentions a credential (a token, a
password, a bearer…) is logged as withheld rather than risk keeping the credential. Log days older
than 30 days are pruned when a logged call creates a new day's file, not by a timer; older files
can remain while logging is idle. You can remove unwanted day files from this folder yourself.
Updating does not rewrite old logs. If an older log exposed a credential, rotate it with its
service before sharing the log.

### MCP over HTTP

While **Let your Agents Control RustGIS** is on, the app also serves MCP over HTTP ("Streamable
HTTP", stateless, JSON replies) on `http://127.0.0.1:8768/mcp`, beside the relay. The
`RUSTGIS_MCP_HTTP_PORT` environment variable names another port. Clients that speak HTTP connect
without `rustgis-cli`. Every request needs `Authorization: Bearer <token>`. The token is made once
and kept in `mcp-http.token` in the settings folder (readable by you only), so a client registered
with it keeps working across launches; delete the file to make a new one. Requests that name
another host, or that come from a web page (an `Origin` header), are refused before the token is
read. Calls go through the same queue, refusals and one-at-a-time rule as the relay's. HTTP clients count
as agents attached with the relay's own token: they share that agent's access, active layer and
name in the undo history, and the token lasts across launches where the relay's lasts one run.

`rustgis-cli mcp-register --client claude` registers it with Claude Code (`claude mcp add
--transport http rustgis-live http://127.0.0.1:8768/mcp --header "Authorization: Bearer …"`), and
the settings section shows that command. Without `--client` it prints the configuration for any
HTTP client. `--name` and `--port` change the registration's name and port. Either way the token
leaves its file: `claude mcp add` takes it as an argument (other users of the computer can see a
running program's arguments for the moment it runs), and the printed configuration shows it, so
keep it out of shared logs. Whoever holds the token can do, while agents are on, what an attached agent can, files and the
network included. If it leaks, stop the relay, delete `mcp-http.token`, and turn agents on again
to create and load a new token; each registered client then needs registering again. Deleting
the file alone does not revoke the token already loaded by a running listener. On Windows the
token file is protected by your user profile's permissions, not by a mode of its own.

An agent the app starts itself gets a run-scoped token instead of the relay's own:
`rustgis-cli mcp --attach PORT --token-file FILE`. Each such grant carries its own file and network
permission, and the app revokes it when it's done with the agent; that closes its connections and
removes its file.

### The file bridge

For an agent that can't use MCP, or when the relay's port is taken, the app also runs scripts
written to a file. It's on while **Let your Agents Control RustGIS** is ticked, even if the relay
couldn't start: agents then stay on, and the status bar says only the file bridge is running.

1. Write a script to `bridge-command.txt` in the relay's private folder
   (`$XDG_RUNTIME_DIR/rustgis` on Linux, a `rustgis-mcp-<user>` folder in the temporary folder
   elsewhere). The settings section shows the full path. The script is in the `script.run` format:
   one `command-id {json}` per line, `#` for comments.
2. Once the file has stopped changing for half a second, RustGIS runs it on the open project. (To be
   sure a half-written script never runs, write it to another file in the folder and rename it to
   `bridge-command.txt`.) It runs as one undo step, with
   the same rules as the `run` tool: a refused or failing line stops it, and nothing it did to the
   project is kept. Files written by an earlier line stay written.
3. The outcome is written to `bridge-result.json` in the same folder: `{success, result, lines,
   layersAdded, notices, error, duration_s, timestamp}`. The status bar says whether it worked.

Only one RustGIS at a time runs the bridge: a second copy's is refused while the first's runs
(it holds `bridge.lock`). The bridge runs a script each time the file changes. Whatever the file held when the bridge started
isn't run. While the bridge is on, `bridge-ready.json` there holds its `pid`, the two files' paths,
`started_at`, the app `version` and a `schema` number. The folder is readable by this user only,
like the relay's token. Scripts run through the bridge are written to the audit log (`source:
"file"`), and **Full access** applies to them as to agents attached with the relay's own token. The bridge is
named `file bridge` in your undo history. Without Full access, a script's `edit.undo`
(alone on its line) takes back only the bridge's own steps. It also keeps its own active layer.

### Relay requirements

- **Off until asked.** Only `ui.mcpRelay` starts a listener: run directly, or by ticking **Let your
  Agents Control RustGIS** (also at launch, when it was left ticked). `ui.mcpRelay` with no parameters
  or with the same port reports the address it already listens on. A different explicit `port`
  is refused until the relay is stopped. `ui.mcpRelay {"stop": true}` stops it and is
  idempotent. The default port is 8767.
- **Loopback only.** `Relay::bind` refuses any non-loopback address before a socket is opened; the
  app's `ui.mcpRelay` binds 127.0.0.1.
- **Private token.** Each run draws 16 bytes from the operating system's random source,
  written as 32 hex characters. The token file is `{port}.token` in `$XDG_RUNTIME_DIR/rustgis`
  when that is an absolute folder, otherwise in a per-user folder under the system temporary
  folder. The folder must be owned by the current user and not accessible to others. The file
  must be a regular file owned by the current user, without group or other access. A file
  that is a symlink is refused. `rustgis-cli mcp --attach` reads it with `read_token(port)`.
  The token never appears on a command line. The file is removed when the relay stops.
- **App-granted access.** `Relay::set_allow_access(true)` lets connections attached with the relay's
  own token use files and the network whether or not they asked; the desktop app sets it while
  **Let your Agents Control RustGIS** is ticked. This includes any process running as the same
  user that can read that token, not just agents launched by RustGIS. It is checked for every call.
  Turning the setting off stops the relay and disconnects its clients, including a relay started
  manually. If the relay fails but the file bridge starts, the saved setting stays on and the
  app reports the relay error; if both fail, the setting is cleared.
- **Grants.** `Relay::grant(access)` writes a run-scoped token to `{port}-{run}-{n}.token` in the
  same private folder (`run` is random per relay run). A grant asked for after the relay stopped is
  refused. A connection that attaches with it gets the grant's permission. Dropping the grant
  removes the file, forgets the token and closes its connections. Stopping the relay removes every
  grant's file.
- **Attach first.** A connection's first non-empty line must be a `rustgis/attach` message
  carrying the token. It must arrive within 5 seconds, measured from connection, and be at most
  4096 bytes including its newline. The token is compared without early exit. A connection that fails
  this is closed. The `access` flag sets what that connection may do; the per-call refusal
  policy still applies. A repeat attach may change `access`.
- **Handshake and idle limits.** Attached clients that send nothing for 300 seconds are closed.
  A reply write that takes longer than 10 seconds closes the connection.
- **Connection caps.** At most 16 open connections, and at most 4 that have not attached. A
  connection beyond either cap is dropped without a reply.
- **Queue cap.** Up to 64 ordinary messages wait for the app's frame; messages held for
  local-agent approval have a separate cap of 64. A request beyond either cap gets error
  `-32001` (`the app is busy; try again`) if it expects a reply.
- **Bounded request waits.** A request waits up to 60 seconds for the app to run it, or as long
  as the client's attach asks (`timeout_s`, 5 to 600 seconds; `--timeout` on the command line).
  If it is still pending then, it is cancelled and never runs; the client gets error `-32002`. If
  it is already running, the client waits one more interval of the same length. If the reply
  still has not come, the client gets error `-32003` (`its outcome is unknown`), and the command
  may still complete later.
- **Busy app.** When the app has drawn no frame for 10 seconds since a request was queued (a file
  dialog is open, a long tool is running, or the window is minimized or hidden and the system has
  stopped drawing it), the request is cancelled at once and the client gets
  error `-32004` saying RustGIS is busy (apart from the full queue's `-32001`). It never runs, and the connection stays open. An app that
  was only idle before the request isn't busy: the request wakes it. This comes before a long
  `timeout_s`: a stalled app answers after 10 seconds whatever the client's wait, and a client
  waiting less than 10 seconds gets `-32002` instead. A `timeout_s` outside 5 to 600 is clamped to
  that range, and one that isn't a number is ignored (the 60-second default).
- **Stop.** Stopping the relay, or dropping its last owner, closes the listener and every
  client, drops queued messages unrun, and removes the token file. A waiting request ends at
  once, checked every 25 milliseconds.
- **Wake.** A queued message or a change in the attached-client count wakes the UI thread so
  it draws a frame. A relay started before the first frame installs its wake on the first
  logic call after that frame exists.
- **Shared framing.** Each message is one JSON-RPC message per line. After attach, messages and
  replies are read with `read_message_max` at 64 MiB (`MAX_LINE`), the limit counting the newline.
  The attach line is read at most 4096 bytes including its newline (see Attach first). After an
  oversized line on a socket the relay sends a -32700 error and closes that connection. On standard
  input the reader skips the rest of the line and continues. IO errors name neither standard input
  nor a socket.

### Tests

These behavioral tests in `crates/assistant/src/relay.rs` check the requirements above:

- Loopback bind and command run: `relay_runs_a_command_on_the_target_session`
- Token required: `relay_drops_connections_without_the_token`
- Parse error after attach: `relay_returns_parse_error_after_attach_and_stays_usable`
- File commands need `--yes`: `relay_refuses_file_commands_without_yes`
- Separate active layers: `relay_keeps_the_agents_active_layer_apart_from_the_users`
- Agents undo only their own steps: `relay_agents_undo_only_their_own_steps`
- Agents of an earlier relay run aren't this run's: `relay_agents_of_another_run_cant_undo_earlier_steps`
- Busy app: `relay_says_the_app_is_busy_early`
- Client-chosen waits: `relay_waits_as_long_as_the_client_asks`
- Endpoint file and port variable: `relay_publishes_its_endpoint_and_names_the_port_variable`
- Full access, for the relay's own token only: `relay_full_access_lifts_the_refusals_for_its_own_token_only`
- Screen snapshots go to the app: `relay_hands_screen_snapshots_to_the_app`
- Snapshots without the screen: `relay_draws_snapshots_offscreen_without_the_screen`
- Saved snapshots need access: `relay_refuses_a_saved_snapshot_without_access`
- The agent's last call, for the status bar: `relay_records_the_agents_last_call`
- MCP over HTTP with the token: `relay::http::tests::http_clients_with_the_token_drive_the_session`
- HTTP refusals (no token, other host, web page, method, size): `relay::http::tests::http_refuses_requests_without_the_token_or_from_elsewhere`
- Stable, private HTTP token: `relay::http::tests::http_token_is_stable_and_private`
- Audit log: `relay_writes_each_call_to_the_audit_log`
- The open project stays the user's: `relay_keeps_project_commands_for_the_user`
- Timed-out command never runs: `relay_cancels_a_timed_out_command_so_it_never_runs`
- Stop cleanup: `relay_stop_closes_clients_and_removes_the_token`
- Last owner drop: `relay_last_owner_drop_closes_the_listener`
- Unauthenticated cap: `relay_caps_unauthenticated_connections`
- Oversized pre-attach line: `relay_refuses_an_oversized_unauthenticated_line`
- Token random and private: `relay_token_is_random_and_private`
- Client count and wake: `relay_counts_authenticated_clients_and_wakes_on_change`
- Stop ends a reply wait: `relay_stop_ends_a_reply_wait_and_the_command_never_runs`
- Running command reported as unknown: `relay_reports_a_running_command_past_its_wait_as_unknown`
- Handshake deadline: `relay_closes_a_trickling_handshake_at_its_deadline`
- App-granted access: `relay_app_lets_agents_use_files_without_yes`
- Grants are scoped and revocable: `relay_grants_are_scoped_and_revocable`


## Tools

| Tool | What it does |
| --- | --- |
| `list_layers` | The whole project in one call: path and unsaved changes, the view (centre, longitude/latitude extent, scale, projection), the active layer, layers in table-of-contents order (group, type, source, extent, feature count, selection, definition query, renderer, labels, fields, visibility, transparency), web tile layers, basemap, tables and layouts with their page size in millimetres and their map frames. `name_filter` keeps the layers and tables whose name holds it; `include_fields: false` leaves fields out |
| `view_map` | The map as a PNG image at the current view; `width` and `height` default to 1024 × 640, each clamped to 64–2048 pixels |
| `snapshot` | The map as a PNG with JSON metadata: `mode`, `width`, `height`, `extent` (longitude/latitude), `center`, `scale`, `projection`, `render_settled`, `saved` and `note`. `mode: "screen"` captures the RustGIS window through the relay, once the map has finished drawing or after `wait_s` seconds (default 5, clamped to 0–30); `overlays: false` leaves out the map's controls, scale bar, north arrow and sketches. `mode: "render"` draws offscreen at `max_width` (default 1024, clamped to 64–2048 pixels); height follows the view's shape, with each side capped at 2048. It works while the window is hidden. `mode: "auto"` (default) takes the screen when the map is on it. Unavailable screen captures fall back to render. `save_path` also writes the PNG to a file, which needs file access |
| `search_commands` | Finds RustGIS commands by words: their ids, ribbon locations and parameters |
| `run_command` | Runs a command by id with JSON parameters; each run is one undo step. The result names the layers it added (`layersAdded`) and what the command said beside it (`notices`), including any parameters it ignored (`layer.moveDown ignores 'steps' (it reads layer)`) |
| `run` | Runs several commands in one call, as one undo step: a script in the `script.run` format (one `command-id {json}` per line, `#` comments). A failing line stops it, nothing it did to the project is kept, and the error names the line; files an earlier line wrote (an export) stay written. Answers `{success, result, lines, layersAdded, notices, error, duration_s}`, where `result` is the last command's. Each line follows `run_command`'s rules, checked before any line runs |
| `search_tools` | Finds geoprocessing tools by words |
| `describe_tool` | A geoprocessing tool's parameters |
| `run_tool` | Runs a geoprocessing tool; its output is added to the map as a new layer (with `layersAdded` and `notices` as for `run_command`), or to the project as a standalone table, which the result names (`table`) |
| `read_features` | A vector layer's or a standalone table's attribute rows: `layer` (name or id) or `table` (a standalone table's name or id; a `layer` name that no layer has reads the table of that name), optional `where`, `fields`, `order_by`, `descending` and `selected` (layers only); `limit` defaults to 20, capped at 200. Returns the matching count and rows with `_row`, and a layer's `_lon` and `_lat` |
| `search_docs` | Searches this documentation and returns the best matching sections |
| `read_doc` | Reads a documentation page or section as Markdown; `offset` (1-based line) and `limit` (default 2000 lines) page through long results using `next_offset` |

MCP exposes these twelve tools: the nine shared [AI Assistant](user-guide/assistant.md) tools,
plus `view_map`, `snapshot` and `run`. The assistant-only `delegate` and `load_skill` tools are
not exposed through MCP. Instead, enabled [skills](user-guide/assistant.md#skills) are offered
as MCP prompts (`prompts/list` and `prompts/get`). Every built-in documentation page is also a
resource (`rustgis://docs/cli.md`, `rustgis://docs/user-guide/maps.md`…) that a client can attach
to a conversation. Successful JSON tool results use the default
[tool-output budget](user-guide/assistant.md#tool-output-limits): an estimated 10,000 tokens
per result. Read large documentation results in sections or continue with `next_offset`.

## What the agent may do

Your MCP client's approval settings determine when it asks you. The server adds its own limits:

- Commands that read or write files, connect to an address on the internet or run a plugin's
  command run only when the server was started with `--yes`, as does opening a project from
  text (it could bring in web layers). Without it, the agent can still work with the data you
  gave the server and with the built-in sample, basemaps and geoprocessing. `view_map` draws
  the web tile layers the project you opened already has, fetching their tiles.
- Plugin management, scripts, tasks, models and the GeoLibre runner are never run from here,
  unless you turned on **Full access** for agents in the desktop app. That applies only to agents
  attached to it with the relay's own token.
- In private-server mode, replacing and undoing in the project are allowed: it is the agent's
  own. With `--attach`, project replacement and undo ownership follow the desktop restrictions
  above, unless Full access is enabled. `ui.*` commands that only open panes and dialogs are
  unavailable to the tools.

Installed [plugins](plugins.md) load first, as with the other subcommands (`--no-plugins` skips
them). Notes such as "MCP server ready" go to standard error; standard output carries only the
protocol.


### Local-agent approvals

These rules govern requests authenticated with the run's token. Local agent programs run with
your operating-system account and their own tool permissions; a program allowed to read the main
relay token can use its native-client access described above.

Local agents launched by the assistant use `Relay::grant_reviewed(auto)`, not an unrestricted
run grant. With automatic approval off, gated tool calls wait in a RustGIS-side queue scoped to
that grant and exact message, including all tool calls in a batch. `--yes` and the general relay
checkbox do not bypass it. An approval cannot revive an expired request; revoking the grant or
stopping the relay cancels pending work. The approval wait has a separate 15-minute bound;
other clients' reads can still run while it waits. Explicit automatic approval bypasses this
extra queue for that run, but not the reserved-command refusals. Ordinary MCP clients retain
the access policy described above.

The displayed batch is frozen with its effective parameters, targets and runtime loads. If
those change while it waits, RustGIS refuses it and the agent must request it again. Delayed
downloads, tool results and screenshots retain the original grant, check it before starting
another stage and before landing, and can be completed only once by their owner.

Stopping the relay through its command or status-bar control also turns off its remembered
agent-control choice. Copyable setup commands are labelled for POSIX shells or PowerShell and
quote the executable path literally; use the matching shell.
