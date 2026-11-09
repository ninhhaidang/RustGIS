//! A plugin for tests, written in the WebAssembly text format (feature `testing`).
//!
//! It answers by the first letter of the command or tool id. Requests are serialized with
//! sorted keys, so the id starts at byte 15 (`{"abi":1,"id":"…`) and a one-letter command's
//! params start at byte 56 (`{"abi":1,"id":"f","kind":"command","lang":"en","params":…}`; every
//! interface language code is two letters).
//!
//! - `h` (hello): logs "hi" and returns `{"hello": "world"}`.
//! - `f` (forward): sends its params to RustGIS as a host request and returns the reply.
//! - `s` (spin): loops forever (stopped by the fuel limit).
//! - `g` (garbage): returns a reply outside its memory.
//! - `o` (out of memory): asks for 4 GB, then traps.
//! - `e` (error): fails with "nope".
//! - `m` (make, a tool): returns a point layer "Made" and a message.
//! - `x` (select, a tool): selects rows 0 and 2 of the layer "Cities".
//! - `r` (read, a tool with a `file` parameter, declared with the `files` permission): returns
//!   what `m` does. A request carrying files has `files` before `id` (byte 15 is then the `"`
//!   closing `"files"`), so it's answered the same way.
//! - `p` (panel state): returns a fixed panel state (see [`panel_state`]), as the Historical
//!   Imagery plugin's would look.
//! - `q` (echo): returns the request itself, so a test sees what the plugin was sent; its
//!   `panel` (the panel's values) is then the answer's, and is taken in again.
//!
//! [`panel_manifest`] declares a panel over `p` and `q`. With a panel, requests carry `panel`
//! before `params`, so `f` reads them only from plugins without one.

use std::io::Write;

const WAT: &str = r#"
(module
  (import "rustgis" "call" (func $call (param i32 i32) (result i32)))
  (import "rustgis" "result" (func $result (param i32)))
  (import "rustgis" "log" (func $log (param i32 i32)))
  (memory (export "memory") 2)
  (global $heap (mut i32) (i32.const 8192))
  (data (i32.const 0) "{\22ok\22:{\22hello\22:\22world\22}}")
  (data (i32.const 64) "hi")
  (data (i32.const 128) "{\22error\22:\22nope\22}")
  (data (i32.const 192) "{\22ok\22:")
  (data (i32.const 2048) "PANEL_DATA")
  (data (i32.const 256) "{\22ok\22:{\22layers\22:[{\22name\22:\22Made\22,\22geojson\22:{\22type\22:\22FeatureCollection\22,\22features\22:[{\22type\22:\22Feature\22,\22properties\22:{\22n\22:1},\22geometry\22:{\22type\22:\22Point\22,\22coordinates\22:[1,2]}}]}}],\22messages\22:[\22made\22]}}")
  (data (i32.const 1024) "{\22ok\22:{\22selection\22:{\22layer\22:\22Cities\22,\22rows\22:[0,2]}}}")
  (func $alloc (export "rustgis_alloc") (param $n i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $n)))
    (if (i32.gt_u (global.get $heap) (i32.mul (memory.size) (i32.const 65536)))
      (then (drop (memory.grow (i32.add (i32.div_u (local.get $n) (i32.const 65536)) (i32.const 1))))))
    (local.get $p))
  (func $reply (param $p i32) (param $n i32) (result i64)
    (i64.or (i64.shl (i64.extend_i32_u (local.get $p)) (i64.const 32)) (i64.extend_i32_u (local.get $n))))
  (func $copy (param $d i32) (param $s i32) (param $n i32)
    (block $done (loop $l
      (br_if $done (i32.eqz (local.get $n)))
      (i32.store8 (local.get $d) (i32.load8_u (local.get $s)))
      (local.set $d (i32.add (local.get $d) (i32.const 1)))
      (local.set $s (i32.add (local.get $s) (i32.const 1)))
      (local.set $n (i32.sub (local.get $n) (i32.const 1)))
      (br $l))))
  (func (export "rustgis_call") (param $p i32) (param $n i32) (result i64)
    (local $c i32) (local $len i32) (local $buf i32)
    (local.set $c (i32.load8_u (i32.add (local.get $p) (i32.const 15))))
    (if (i32.eq (local.get $c) (i32.const 104)) (then
      (call $log (i32.const 64) (i32.const 2))
      (return (call $reply (i32.const 0) (i32.const 24)))))
    (if (i32.eq (local.get $c) (i32.const 102)) (then
      (local.set $len (call $call (i32.add (local.get $p) (i32.const 56)) (i32.sub (local.get $n) (i32.const 57))))
      (local.set $buf (call $alloc (local.get $len)))
      (call $result (local.get $buf))
      (return (call $reply (local.get $buf) (local.get $len)))))
    (if (i32.eq (local.get $c) (i32.const 115)) (then (loop $l (br $l))))
    (if (i32.eq (local.get $c) (i32.const 103)) (then (return (i64.const 0x7fff000000000010))))
    (if (i32.eq (local.get $c) (i32.const 111)) (then (drop (memory.grow (i32.const 65000))) unreachable))
    (if (i32.eq (local.get $c) (i32.const 109)) (then (return (call $reply (i32.const 256) (i32.const 195)))))
    (if (i32.or (i32.eq (local.get $c) (i32.const 114)) (i32.eq (local.get $c) (i32.const 34)))
      (then (return (call $reply (i32.const 256) (i32.const 195)))))
    (if (i32.eq (local.get $c) (i32.const 120)) (then (return (call $reply (i32.const 1024) (i32.const 52)))))
    (if (i32.eq (local.get $c) (i32.const 112)) (then (return (call $reply (i32.const 2048) (i32.const PANEL_LEN)))))
    (if (i32.eq (local.get $c) (i32.const 113)) (then
      (local.set $buf (call $alloc (i32.add (local.get $n) (i32.const 7))))
      (call $copy (local.get $buf) (i32.const 192) (i32.const 6))
      (call $copy (i32.add (local.get $buf) (i32.const 6)) (local.get $p) (local.get $n))
      (i32.store8 (i32.add (i32.add (local.get $buf) (i32.const 6)) (local.get $n)) (i32.const 125))
      (return (call $reply (local.get $buf) (i32.add (local.get $n) (i32.const 7))))))
    (call $reply (i32.const 128) (i32.const 16)))
)
"#;

/// The panel state command `p` answers: a release timeline like the Historical Imagery
/// plugin's, at its last release.
pub fn panel_state() -> serde_json::Value {
    let dates = [
        "2014-02-20",
        "2015-01-21",
        "2016-01-13",
        "2017-01-11",
        "2018-01-08",
        "2019-01-09",
        "2020-01-08",
        "2021-01-13",
        "2022-01-12",
        "2023-01-11",
        "2024-01-18",
        "2025-01-30",
        "2025-12-18",
    ];
    serde_json::json!({"panel": {
        "values": {"release": 12},
        "items": {
            "selected": {"text": "2025-12-18\nRelease 13192"},
            "release": {"min": 0, "max": 12, "labels": dates},
            "newer": {"enabled": false},
            "metadata": {"text": "Source: Maxar (Vivid)\nResolution: 0.3 m\nCaptured: 2025-06-02\nAccuracy: 5 m"}
        }
    }})
}

/// The test module.
pub fn wasm() -> Vec<u8> {
    let answer = serde_json::json!({"ok": panel_state()}).to_string();
    let data = answer.replace('\\', "\\5c").replace('"', "\\22");
    wat::parse_str(WAT.replace("PANEL_DATA", &data).replace("PANEL_LEN", &answer.len().to_string())).unwrap_or_default()
}

/// A manifest for the test module, with the given id and permissions.
pub fn manifest(id: &str, permissions: &[&str]) -> String {
    let mut m = serde_json::json!({
        "id": id,
        "name": format!("Test {id}"),
        "version": "1.0.0",
        "description": "A plugin for tests",
        "permissions": permissions,
        "commands": [
            {"id": "h", "label": "Hello", "icon": "star"},
            {"id": "f", "label": "Forward", "params": "a host request"},
            {"id": "s", "label": "Spin"},
            {"id": "g", "label": "Garbage"},
            {"id": "o", "label": "Out of memory"},
            {"id": "e", "label": "Error"}
        ],
        "tools": [
            {"id": "m", "name": "Make Points", "summary": "Makes a point", "params": [
                {"name": "input", "label": "Input", "kind": "vectorLayer", "required": true},
                {"name": "size", "label": "Size", "kind": "number", "default": 3},
                {"name": "mode", "label": "Mode", "kind": "choice", "choices": ["a", "b"], "default": "a"}
            ]},
            {"id": "x", "name": "Select Two", "params": [{"name": "input", "label": "Input", "kind": "layer"}]}
        ],
        "i18n": {"de": {"Hello": "Hallo"}}
    });
    // A file parameter needs `files`.
    if permissions.contains(&"files")
        && let Some(tools) = m["tools"].as_array_mut()
    {
        tools.push(serde_json::json!({"id": "r", "name": "Read File", "params": [
            {"name": "input", "label": "Input", "kind": "layer"},
            {"name": "model", "label": "Model", "kind": "file", "required": true, "extensions": ["nc", "nc4"]}
        ]}));
    }
    m.to_string()
}

/// A manifest for the test module with a panel like the Historical Imagery plugin's: texts, a
/// checkbox and a stepped slider running the echo command `q`, buttons, and `p` as its state
/// command.
pub fn panel_manifest(id: &str) -> String {
    let mut m: serde_json::Value = serde_json::from_str(&manifest(id, &[])).unwrap_or_default();
    if let Some(c) = m["commands"].as_array_mut() {
        c.push(serde_json::json!({"id": "p", "label": "Panel State"}));
        c.push(serde_json::json!({"id": "q", "label": "Echo"}));
        // Answered by their first letters, as `p` and `q` are.
        c.push(serde_json::json!({"id": "p-older", "label": "Older", "icon": "prev_extent"}));
        c.push(serde_json::json!({"id": "q-newer", "label": "Newer", "icon": "next_extent"}));
    }
    m["panel"] = serde_json::json!({
        "title": "Historical Imagery",
        "icon": "history",
        "help": "Step through the archived releases.",
        "state": "p",
        "items": [
            {"kind": "text", "id": "selected", "label": "Selected release", "text": "None yet"},
            {"kind": "checkbox", "id": "local", "label": "Only versions with local changes", "command": "q"},
            {"kind": "slider", "id": "release", "label": "Release timeline", "min": 0, "max": 0, "steps": true, "command": "q"},
            {"kind": "button", "command": "p-older", "id": "older"},
            {"kind": "button", "command": "q-newer", "id": "newer"},
            {"kind": "button", "command": "h", "id": "hello", "label": "Say Hello"},
            {"kind": "text", "id": "metadata", "label": "Location metadata"}
        ]
    });
    m["i18n"] =
        serde_json::json!({"de": {"Hello": "Hallo", "Historical Imagery": "Historische Luftbilder", "Release timeline": "Zeitleiste"}});
    m.to_string()
}

/// A `.rgplugin` package of the test module with [`panel_manifest`].
pub fn panel_package(id: &str) -> Vec<u8> {
    zip_of(&[("plugin.json", panel_manifest(id).as_bytes()), ("plugin.wasm", &wasm())])
}

/// A `.rgplugin` package of the test module.
pub fn package(id: &str, permissions: &[&str]) -> Vec<u8> {
    zip_of(&[("plugin.json", manifest(id, permissions).as_bytes()), ("plugin.wasm", &wasm())])
}

/// A skills package (no code) with the given skills, each a `SKILL.md` naming and describing it.
pub fn skills_package(id: &str, skills: &[&str]) -> Vec<u8> {
    let manifest = serde_json::json!({"id": id, "name": format!("Skills {id}"), "version": "1.0.0", "runtime": "skills", "skills": skills})
        .to_string();
    let texts: Vec<(String, String)> = skills
        .iter()
        .map(|s| (format!("skills/{s}/SKILL.md"), format!("---\nname: {s}\ndescription: The {s} skill.\n---\n\n# {s}\nDo {s}.\n")))
        .collect();
    let mut files: Vec<(&str, &[u8])> = vec![("plugin.json", manifest.as_bytes())];
    files.extend(texts.iter().map(|(p, t)| (p.as_str(), t.as_bytes())));
    zip_of(&files)
}

/// A zip archive of `files` (name, bytes), stored uncompressed.
pub fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in files {
        if w.start_file(*name, opts).is_err() || w.write_all(bytes).is_err() {
            return Vec::new();
        }
    }
    w.finish().map(|c| c.into_inner()).unwrap_or_default()
}

/// The argument that tells a test binary, started as a process plugin, to act as one (see
/// [`serve_if_child`]). It doubles as a test filter that matches no test.
const CHILD_MARK: &str = "rustgis-plugin-child-mode";

/// `run` for a process plugin played by this test binary: it starts the test named `test` (in
/// the crate's tests, it calls [`serve_if_child`]) and nothing else.
#[cfg(not(target_arch = "wasm32"))]
pub fn child_run(test: &str) -> Vec<String> {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    [exe.as_str(), test, CHILD_MARK, "--exact", "--nocapture", "--test-threads=1"].iter().map(|s| s.to_string()).collect()
}

/// A manifest for the process test plugin: the test module's commands and tools, run by `run`.
/// With `files`, its tool `r` takes a file.
pub fn process_manifest(id: &str, permissions: &[&str], run: &[String]) -> String {
    let mut m: serde_json::Value = serde_json::from_str(&manifest(id, permissions)).unwrap_or_default();
    m["runtime"] = serde_json::json!("process");
    m["run"] = serde_json::json!(run);
    if let Some(c) = m["commands"].as_array_mut() {
        c.push(serde_json::json!({"id": "n", "label": "Noisy"}));
        c.push(serde_json::json!({"id": "c", "label": "Crash"}));
        c.push(serde_json::json!({"id": "d", "label": "Say, then forward"}));
    }
    if let Some(t) = m["tools"].as_array_mut() {
        t.push(serde_json::json!({"id": "w", "name": "Work Forever", "params": []}));
    }
    m.to_string()
}

/// A manifest for the session test plugin (`"session": true`): the process test plugin's
/// commands, and `k`, `v`, `i` and `z` (see [`serve_if_child`]).
pub fn session_manifest(id: &str, permissions: &[&str], run: &[String]) -> String {
    let mut m: serde_json::Value = serde_json::from_str(&process_manifest(id, permissions, run)).unwrap_or_default();
    m["session"] = serde_json::json!(true);
    if let Some(c) = m["commands"].as_array_mut() {
        c.push(serde_json::json!({"id": "k", "label": "Ask Later"}));
        c.push(serde_json::json!({"id": "v", "label": "Heard"}));
        c.push(serde_json::json!({"id": "i", "label": "Process"}));
        c.push(serde_json::json!({"id": "z", "label": "Quit"}));
        c.push(serde_json::json!({"id": "l", "label": "Launch"}));
        c.push(serde_json::json!({"id": "p", "label": "Pause"}));
        c.push(serde_json::json!({"id": "w", "label": "Last Words"}));
        c.push(serde_json::json!({"id": "b", "label": "Bad Exit"}));
        c.push(serde_json::json!({"id": "y", "label": "Long Goodbye"}));
        c.push(serde_json::json!({"id": "r", "label": "Leave a Writer"}));
    }
    m.to_string()
}

/// In a test binary started by a process test plugin ([`child_run`]): answers one request as the
/// test module does, then exits. Elsewhere it returns at once.
///
/// - `h` logs "hi" and returns `{"hello": "world"}`; `f` sends its params as a host request
///   and returns the reply; `d` says an error first, then does what `f` does; `e` fails with "nope"; `n` prints a line that isn't JSON and a JSON object that isn't a message first;
///   `c` writes "boom" to its error output and exits without answering; `s` never answers;
///   `g` starts another copy of itself that never answers, logs its process id and never
///   answers either.
/// - Tools: `m` returns the layer "Made"; `x` selects rows 0 and 2 of "Cities"; `r` returns the
///   names of the files it was given as its messages; `w` logs "working", reports
///   progress "step 1 of 3" and never answers; `m` reports progress "making" first.
///
/// Started as a session (its first line `{"session": …}`), it keeps answering commands until
/// its input closes: `h`, `e`, `c` and `s` as above; `f` sends its params as a host request and
/// returns the reply, skipping events; `k` sends that request and answers at once, leaving the
/// reply for later; `v` returns what it has heard since (`events`, `answers`, and the latest
/// `view` event's view as `last_view`); `i` returns its
/// process id; `z` answers and exits; `l` starts a copy of itself that never answers, answers
/// with its process id and exits, leaving it running; `p` answers, then reads nothing for two
/// seconds; `w` answers, then sends its params as a host request, logs "last words" and exits at
/// once; `b` answers, writes "boom" to its error output and exits with status 3; `y` answers,
/// logs 70 lines, sends its params as a host request and exits at once; `r` starts a copy of
/// itself that writes into its output forever, answers with that copy's process id and exits.
#[cfg(not(target_arch = "wasm32"))]
pub fn serve_if_child() {
    use serde_json::{Value, json};
    use std::io::{BufRead, Write};
    if !std::env::args().any(|a| a == CHILD_MARK) {
        return;
    }
    let mut out = std::io::stdout();
    // The test harness has printed "test … " without a line end: start a line of our own.
    let _ = writeln!(out);
    let mut input = std::io::stdin().lock();
    let mut line = String::new();
    let _ = input.read_line(&mut line);
    let req: Value = serde_json::from_str(&line).unwrap_or_default();
    if req.get("session").is_some() {
        serve_session(input, out);
    }
    let params = req.get("params").cloned().unwrap_or_default();
    let reply = match req.get("id").and_then(Value::as_str).and_then(|s| s.chars().next()) {
        Some('h') => {
            let _ = writeln!(out, "{}", json!({"log": "hi"}));
            json!({"ok": {"hello": "world"}})
        }
        Some('f') => {
            let _ = writeln!(out, "{}", json!({"call": {"op": params["op"], "params": params["params"]}}));
            let _ = out.flush();
            let mut answer = String::new();
            let _ = input.read_line(&mut answer);
            serde_json::from_str(&answer).unwrap_or_else(|_| json!({"error": "no answer"}))
        }
        // An error notice first, then what `f` does.
        Some('d') => {
            let said = json!({"call": {"op": "host.notify", "params": {"text": "said before the call", "error": true}}});
            let _ = writeln!(out, "{said}");
            let _ = out.flush();
            let mut answer = String::new();
            let _ = input.read_line(&mut answer);
            let _ = writeln!(out, "{}", json!({"call": {"op": params["op"], "params": params["params"]}}));
            let _ = out.flush();
            answer.clear();
            let _ = input.read_line(&mut answer);
            let reply: Value = serde_json::from_str(&answer).unwrap_or_else(|_| json!({"error": "no answer"}));
            if params.get("fail").and_then(Value::as_bool) == Some(true)
                && reply.get("ok").and_then(|v| v.get("pending")).and_then(Value::as_bool) == Some(true)
            {
                json!({"error":"nope"})
            } else {
                reply
            }
        }
        Some('n') => {
            let _ = writeln!(out, "a library printing");
            let _ = writeln!(out, "{}", json!({"library": "a JSON object of its own"}));
            json!({"ok": {"n": 1}})
        }
        Some('c') => {
            eprintln!("boom");
            std::process::exit(3);
        }
        Some('w') => {
            let _ = writeln!(out, "{}", json!({"log": "working"}));
            let _ = writeln!(out, "{}", json!({"progress": "step 1 of 3"}));
            let _ = out.flush();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        Some('s') => loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        },
        // Logs forever, as fast as it can, saying on its error output once it has written more
        // than a pipe holds: from then on its output is waiting to be read, whenever it runs.
        Some('u') => {
            for _ in 0..8_192 {
                let _ = writeln!(out, "{}", json!({"log": "more"}));
            }
            eprintln!("writing");
            loop {
                let _ = writeln!(out, "{}", json!({"log": "more"}));
            }
        }
        Some('g') => {
            let argv = child_run(&std::env::args().nth(1).unwrap_or_default());
            let spawned = argv.split_first().and_then(|(exe, args)| {
                std::process::Command::new(exe)
                    .args(args)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .ok()
            });
            if let Some(mut grandchild) = spawned {
                if let Some(mut stdin) = grandchild.stdin.take() {
                    let _ = writeln!(stdin, "{}", json!({"id": "s", "params": {}}));
                }
                let _ = writeln!(out, "{}", json!({"log": grandchild.id().to_string()}));
                let _ = out.flush();
            }
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        Some('m') => {
            // Hold an actual tool request for cancellation tests; ordinary calls do not wait.
            if let (Some(ready), Some(release)) =
                (params.get("control_ready").and_then(Value::as_str), params.get("control_release").and_then(Value::as_str))
            {
                let _ = std::fs::write(ready, b"started");
                let start = std::time::Instant::now();
                while !std::path::Path::new(release).is_file() && start.elapsed() < std::time::Duration::from_secs(20) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            let _ = writeln!(out, "{}", json!({"progress": "making"}));
            json!({"ok": {"layers": [{"name": "Made", "geojson": {"type": "FeatureCollection", "features": [
            {"type": "Feature", "properties": {"n": 1}, "geometry": {"type": "Point", "coordinates": [1, 2]}}]}}], "messages": ["made"]}})
        }
        Some('x') => json!({"ok": {"selection": {"layer": "Cities", "rows": [0, 2]}}}),
        Some('r') => {
            let names: Vec<Value> = req["files"].as_object().map(|f| f.values().map(|v| v["name"].clone()).collect()).unwrap_or_default();
            json!({"ok": {"layers": [], "messages": names}})
        }
        _ => json!({"error": "nope"}),
    };
    let _ = writeln!(out, "{reply}");
    let _ = out.flush();
    std::process::exit(0);
}

/// The session test plugin: see [`serve_if_child`].
#[cfg(not(target_arch = "wasm32"))]
fn serve_session(mut input: impl std::io::BufRead, mut out: impl std::io::Write) -> ! {
    use serde_json::{Value, json};
    let (mut events, mut answers) = (Vec::<Value>::new(), Vec::<Value>::new());
    let mut last_view = Value::Null;
    let mut asked = 0u64;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).map_or(true, |n| n == 0) {
            std::process::exit(0);
        }
        let msg: Value = serde_json::from_str(&line).unwrap_or_default();
        if let Some(e) = msg.get("event") {
            if e == "view" {
                last_view = msg["view"].clone();
            }
            events.push(e.clone());
            continue;
        }
        let Some(req) = msg.get("command") else {
            answers.push(msg);
            continue;
        };
        let id = msg["id"].clone();
        let params = req.get("params").cloned().unwrap_or_default();
        let mut ask = |out: &mut dyn std::io::Write| {
            asked += 1;
            let _ = writeln!(out, "{}", json!({"call": {"op": params["op"], "params": params["params"]}, "id": format!("a{asked}")}));
            let _ = out.flush();
            format!("a{asked}")
        };
        let reply = match req.get("id").and_then(Value::as_str).and_then(|s| s.chars().next()) {
            Some('h') => json!({"ok": {"hello": "world"}}),
            Some('f') => {
                let want = ask(&mut out);
                loop {
                    let mut answer = String::new();
                    if input.read_line(&mut answer).map_or(true, |n| n == 0) {
                        std::process::exit(0);
                    }
                    let a: Value = serde_json::from_str(&answer).unwrap_or_default();
                    if let Some(e) = a.get("event") {
                        if e == "view" {
                            last_view = a["view"].clone();
                        }
                        events.push(e.clone());
                    } else if a["id"] == json!(want) {
                        break a;
                    }
                }
            }
            Some('r') => {
                let argv = child_run(&std::env::args().nth(1).unwrap_or_default());
                // Its output is this program's: it keeps writing there after this one exits.
                let spawned = argv.split_first().and_then(|(exe, args)| {
                    std::process::Command::new(exe)
                        .args(args)
                        .stdin(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                        .ok()
                });
                let pid = spawned.map(|mut g| {
                    if let Some(mut stdin) = g.stdin.take() {
                        let _ = writeln!(stdin, "{}", json!({"id": "u", "params": {}}));
                    }
                    // Exits only once the copy is writing: its output is flowing when it's gone.
                    if let Some(err) = g.stderr.take() {
                        let mut started = String::new();
                        let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(err), &mut started);
                    }
                    g.id()
                });
                let _ = writeln!(out, "{}", json!({"ok": pid, "id": id}));
                let _ = out.flush();
                std::process::exit(0);
            }
            Some('y') => {
                let _ = writeln!(out, "{}", json!({"ok": "bye", "id": id}));
                for i in 0..70 {
                    let _ = writeln!(out, "{}", json!({"log": format!("line {i}")}));
                }
                ask(&mut out);
                std::process::exit(0);
            }
            Some('b') => {
                let _ = writeln!(out, "{}", json!({"ok": "bye", "id": id}));
                let _ = out.flush();
                eprintln!("boom");
                std::process::exit(3);
            }
            Some('w') => {
                let _ = writeln!(out, "{}", json!({"ok": "bye", "id": id}));
                ask(&mut out);
                let _ = writeln!(out, "{}", json!({"log": "last words"}));
                let _ = out.flush();
                std::process::exit(0);
            }
            Some('p') => {
                let _ = writeln!(out, "{}", json!({"ok": "paused", "id": id}));
                let _ = out.flush();
                std::thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }
            // Answered first, so the request comes after the command.
            Some('k') => {
                let _ = writeln!(out, "{}", json!({"ok": "asked", "id": id}));
                ask(&mut out);
                continue;
            }
            Some('v') => {
                json!({"ok": {"events": std::mem::take(&mut events), "answers": std::mem::take(&mut answers), "last_view": last_view}})
            }
            Some('i') => json!({"ok": std::process::id()}),
            Some('c') => {
                eprintln!("boom");
                std::process::exit(3);
            }
            Some('s') => loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            },
            Some('z') => {
                let _ = writeln!(out, "{}", json!({"ok": "bye", "id": id}));
                let _ = out.flush();
                std::process::exit(0);
            }
            Some('l') => {
                let argv = child_run(&std::env::args().nth(1).unwrap_or_default());
                let spawned = argv.split_first().and_then(|(exe, args)| {
                    std::process::Command::new(exe)
                        .args(args)
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::null())
                        .spawn()
                        .ok()
                });
                let pid = spawned.map(|mut g| {
                    if let Some(mut stdin) = g.stdin.take() {
                        let _ = writeln!(stdin, "{}", json!({"id": "s", "params": {}}));
                    }
                    g.id()
                });
                let _ = writeln!(out, "{}", json!({"ok": pid, "id": id}));
                let _ = out.flush();
                std::process::exit(0);
            }
            _ => json!({"error": "nope"}),
        };
        let mut reply = reply;
        reply["id"] = id;
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
    }
}
