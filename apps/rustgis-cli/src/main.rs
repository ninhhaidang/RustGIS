//! RustGIS on the command line. Every command the ribbon runs is available here by id.
//!
//! ```text
//! rustgis-cli commands                                  # list command ids and parameters
//! rustgis-cli tools                                     # list geoprocessing tools
//! rustgis-cli info data.geojson                         # layer summary as JSON
//! rustgis-cli render data.geojson out.png [--basemap ocean] [--tiles URL] [--width 1600 --height 900]
//! rustgis-cli run [--sample | --project p.rgis | --data f.geojson …] \
//!     --cmd 'gp.run={"tool":"buffer","params":{"input":"Cities","distance":100}}' \
//!     --cmd 'share.exportMap={"path":"out.png"}' [--save out.rgis]
//! rustgis-cli run … --use-saved-secrets            # use what the desktop app saved in the system keychain
//! rustgis-cli ask [--sample | --project p.rgis | --data f.geojson …] "Which cities have over 10 million people?" \
//!     [--provider anthropic|openai|chatgpt|gemini|openrouter|ollama|custom|requesty|trustedrouter] [--model M] [--base-url URL] [--yes] [--save out.rgis] [--trace log.txt]
//! rustgis-cli ask --models [--provider P]                # list the provider's models
//! rustgis-cli ask --accounts | --account CLIENT_ID       # list or select a saved registration
//! rustgis-cli ask --remove-account CLIENT_ID             # sign out and forget a saved registration
//! rustgis-cli ask --login [--another-account]            # sign in with ChatGPT to use your ChatGPT plan
//! rustgis-cli ask --logout
//! rustgis-cli mcp [--sample | --project p.rgis | --data f.geojson …] [--yes]   # an MCP server on stdio
//! rustgis-cli mcp --attach [PORT] [--yes]   # the same, on the desktop app already open (its MCP Relay)
//! rustgis-cli mcp-register [--client claude] [--name N] [--port P]   # register the open app's MCP over HTTP
//! rustgis-cli plugins [install <file|folder> | uninstall <id> | enable <id> | disable <id>
//!                      | browse | get <id> | prepare <id>] [--registry URL]
//! ```
//!
//! Installed plugins are loaded first (`--no-plugins` skips them); `--plugin <file|folder>`
//! loads one more for this run only, so its commands (`plugin:<id>/<command>`) and tools can
//! be run with `--cmd`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// `println!` that ignores a closed pipe (`rustgis-cli commands | head`).
macro_rules! out {
    ($($t:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

use rustgis_assistant::{Config, provider};
use rustgis_engine::data::secrets::SecretStore;
use rustgis_engine::{Project, Session};
use serde_json::{Map, Value, json};

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  rustgis-cli commands | tools\n  rustgis-cli plugins [install <file|folder> | uninstall <id> | enable <id> | disable <id> | browse | get <id> | prepare <id>] [--registry URL]\n  rustgis-cli skills [add <SKILL.md|folder|.zip> | remove <name> | enable <name> | disable <name>]\n  rustgis-cli info <data>\n  rustgis-cli logo <out.png|out.ico|out.bmp> [--size N] [--canvas WxH --at X,Y (.bmp only)]\n  rustgis-cli render <data|project> <out.png> [--basemap NAME] [--tiles URL] [--width N] [--height N] [--dpi N] [--worldview auto|default|china]\n  rustgis-cli run [--sample] [--project P] [--data F]… --cmd 'id={{json}}'… [--save P] [--worldview auto|default|china]\n  rustgis-cli ask [--sample] [--project P] [--data F]… \"request\" [--provider P] [--model M] [--base-url URL] [--yes] [--save P]\n  rustgis-cli ask --models [--provider P]\n  rustgis-cli ask --login [--another-account] [--account CLIENT_ID] | --logout\n  rustgis-cli ask --accounts | --account CLIENT_ID | --remove-account CLIENT_ID\n  common options: --use-saved-secrets (use the connections, passwords, keys, ChatGPT sign-in and Earthdata Login token the RustGIS app saved; reads the system keychain)\n  rustgis-cli mcp [--sample] [--project P] [--data F]… [--yes] [--attach [PORT] [--token-file FILE] [--timeout SECONDS (5-600)]]\n  rustgis-cli mcp-register [--client claude] [--name NAME] [--port PORT]"
    );
    ExitCode::from(2)
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn print(v: &Value) {
    out!("{}", serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()));
}

// The progress line's state, for the one command `run` runs at a time (and resets after it).

/// Whether a progress line is left without its end (a run that failed partway).
static PROGRESS_OPEN: AtomicBool = AtomicBool::new(false);
/// The tenth of a run last told where standard error isn't a terminal (a log, CI).
static TOLD_TENTH: AtomicUsize = AtomicUsize::new(usize::MAX);

/// A long run's progress on standard error, one line rewritten (`cube.trend: 42%`); in a log, a
/// line every tenth and at the end.
fn progress(what: &str, share: f64) {
    let percent = (share * 100.0).clamp(0.0, 100.0);
    let ended = share >= 1.0;
    if !std::io::stderr().is_terminal() {
        let tenth = (percent / 10.0) as usize;
        if TOLD_TENTH.swap(if ended { usize::MAX } else { tenth }, Ordering::Relaxed) != tenth || ended {
            eprintln!("{what}: {percent:.0}%");
        }
        return;
    }
    eprint!("\r{what}: {percent:.0}%");
    if ended {
        eprintln!();
    }
    PROGRESS_OPEN.store(!ended, Ordering::Relaxed);
}

/// Runs a command; what it said beside its result (the app's status bar) goes to standard error.
fn run(s: &mut Session, id: &str, p: Value) -> Result<Value, String> {
    let mark = s.notice_mark();
    let r = s.run(id, &p);
    // A progress line a failure cut short ends here, so what follows starts a line of its own.
    if PROGRESS_OPEN.swap(false, Ordering::Relaxed) {
        eprintln!();
    }
    TOLD_TENTH.store(usize::MAX, Ordering::Relaxed);
    // A failure is noted too, redacted; it's printed once, by the caller.
    let told = r.as_ref().err().map(|e| rustgis_engine::data::diagnostics::redact(e));
    for n in s.notices_since(mark).iter().filter(|n| r.as_ref().err() != Some(&n.text) && told.as_ref() != Some(&n.text)) {
        eprintln!("rustgis-cli: {}{}", if n.error { "error: " } else { "" }, n.text);
    }
    r.map_err(|e| format!("{id}: {e}"))
}

/// Makes a Python plugin's environment, saying what it's doing as it goes.
fn prepare_env(id: &str) -> Result<String, String> {
    eprintln!("rustgis-cli: preparing {id}'s Python environment (the first time downloads Python and its packages)");
    let python = rustgis_engine::plugins::pyenv::prepare(id, &mut |line| eprintln!("rustgis-cli: {line}"))?;
    Ok(python.display().to_string())
}

fn is_project(path: &str) -> bool {
    rustgis_engine::project::opens_as_project(path)
}

fn num(args: &[String], name: &str) -> Option<f64> {
    opt(args, name).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite())
}

fn load(s: &mut Session, path: &str) -> Result<(), String> {
    if is_project(path) {
        run(s, "project.open", json!({"path": path}))?;
    } else {
        run(s, "layer.addData", json!({"path": path}))?;
    }
    Ok(())
}

/// Opens `--sample`, `--project` and every `--data`.
fn open_inputs(s: &mut Session, args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--sample") {
        run(s, "project.sample", Value::Null)?;
    }
    if let Some(p) = opt(args, "--project") {
        load(s, p)?;
    }
    for (i, a) in args.iter().enumerate() {
        if a == "--data"
            && let Some(p) = args.get(i + 1)
        {
            load(s, p)?;
        }
    }
    Ok(())
}

/// `skills`: the assistant's skills, and adding, removing and switching them (see
/// `rustgis_assistant::skills`).
fn skills(args: &[String]) -> Result<(), String> {
    use rustgis_assistant::skills::{self, Places, Source};
    let places = Places::here();
    let arg = |what: &str| args.get(2).filter(|a| !a.starts_with("--")).cloned().ok_or(format!("skills {what} needs an argument"));
    match args.get(1).map(String::as_str) {
        None | Some("list") => {
            let (listed, problems) = skills::list(&places);
            for p in problems {
                eprintln!("rustgis-cli: {p}");
            }
            let rows: Vec<Value> = listed
                .iter()
                .map(|l| {
                    let (source, folder) = match &l.skill.source {
                        Source::BuiltIn => ("built-in", None),
                        Source::Plugin(_) => ("plugin", None),
                        Source::User(d) => ("user", Some(d.display().to_string())),
                        Source::Shared(d) => ("shared", Some(d.display().to_string())),
                    };
                    json!({"name": l.skill.name, "description": l.skill.description, "source": source, "folder": folder, "on": l.on})
                })
                .collect();
            print(&json!(rows));
        }
        Some("add") => print(&json!({"added": skills::add(&places, std::path::Path::new(&arg("add")?))?})),
        Some("remove") => {
            let name = arg("remove")?;
            skills::remove(&places, &name)?;
            print(&json!({"removed": name}));
        }
        Some(on @ ("enable" | "disable")) => {
            let name = arg(on)?;
            skills::set_enabled(&places, &name, on == "enable")?;
            print(&json!({"name": name, "on": on == "enable"}));
        }
        Some(other) => return Err(format!("unknown skills subcommand '{other}'")),
    }
    Ok(())
}

/// What `--use-saved-secrets` gave this run: the desktop app's preferences (read once, when it
/// opted in) and its system keychain, which is asked only when a request needs a secret.
struct Saved {
    prefs: Map<String, Value>,
    store: std::sync::Arc<dyn SecretStore>,
}

/// What `ask` takes from the process environment; tests supply their own.
struct Environment {
    /// `RUSTGIS_ASSISTANT_PROVIDER`.
    provider: Option<String>,
    /// `RUSTGIS_ASSISTANT_MODEL`.
    model: Option<String>,
    /// A provider's defaults with the environment's key and address (`Config::from_env`).
    config: fn(&str) -> Config,
}

impl Environment {
    fn process() -> Self {
        Environment {
            provider: std::env::var("RUSTGIS_ASSISTANT_PROVIDER").ok(),
            model: std::env::var("RUSTGIS_ASSISTANT_MODEL").ok(),
            config: Config::from_env,
        }
    }
}

/// The provider to use: `--provider`, `RUSTGIS_ASSISTANT_PROVIDER`, the first provider with an
/// environment key and, with `--use-saved-secrets`, the one the desktop app uses, else `ollama`.
fn choose_provider(args: &[String], env: &Environment, saved: Option<&Saved>) -> String {
    provider::choose(opt(args, "--provider"), env.provider.as_deref(), provider::first_keyed(env.config), saved.map(|s| &s.prefs))
}

/// Builds a request's configuration without reading unrelated credentials during management.
/// A prompt may use the desktop's API key for its provider and address; model listing uses
/// only environment API keys, so `--models` never prompts for a saved API key.
fn configuration(provider: &str, args: &[String], env: &Environment, saved: Option<&Saved>) -> Config {
    let mut cfg = (env.config)(provider);
    if cfg.login()
        && let Some(c) = rustgis_assistant::chatgpt::load_any()
    {
        cfg.sign_in(&c);
    }
    if let Some(m) = opt(args, "--model").map(str::to_string).or_else(|| env.model.clone()) {
        cfg.model = m;
    }
    if let Some(u) = opt(args, "--base-url") {
        cfg.base_url = u.to_string();
    }
    // An `Err` is ignored: the keychain's wrapper has already said once that it can't be used.
    if !args.iter().any(|arg| arg == "--models")
        && let Some(saved) = saved
        && let Ok(Some(key)) = provider::saved_key(&cfg, &saved.prefs, saved.store.as_ref())
    {
        cfg.key = key.expose().to_string();
    }
    cfg
}

const NEEDS_REQUEST: &str = "ask needs a request, e.g. rustgis-cli ask --sample \"Which countries have the largest GDP?\"";

/// `ask "<request>"`: one conversation turn with the AI assistant, which acts on the project.
fn ask(s: &mut Session, args: &[String], saved: Option<&Saved>, env: &Environment) -> Result<(), String> {
    use rustgis_assistant::{Agent, Entry, agent, http};
    // The first value that isn't an option's.
    let takes_value = [
        "--provider",
        "--model",
        "--base-url",
        "--project",
        "--data",
        "--save",
        "--trace",
        "--plugin",
        "--registry",
        "--worldview",
        "--account",
        "--remove-account",
    ];
    let prompt = args
        .iter()
        .enumerate()
        .skip(1)
        .find(|(i, a)| !a.starts_with("--") && !args.get(i - 1).is_some_and(|p| takes_value.contains(&p.as_str())))
        .map(|(_, a)| a.clone());
    let provider = choose_provider(args, env, saved);
    if provider::preset(&provider).is_none() {
        let ids: Vec<&str> = provider::PRESETS.iter().map(|p| p.id).collect();
        return Err(format!("unknown provider '{provider}' (one of {})", ids.join(", ")));
    }
    if args.iter().any(|a| a == "--accounts") {
        for c in rustgis_assistant::chatgpt::accounts()? {
            out!("{}\t{}", c.client_id, c.label());
        }
        return Ok(());
    }
    if let Some(id) = opt(args, "--account") {
        let accounts = rustgis_assistant::chatgpt::accounts()?;
        let matches: Vec<_> = accounts.iter().filter(|c| c.client_id == id).collect();
        if matches.len() != 1 {
            return Err("--account needs an unambiguous client id from --accounts".into());
        }
        if let Some(c) = matches.first() {
            rustgis_assistant::chatgpt::select(&c.client_id, &c.subject)?;
        }
        if prompt.is_none() && !args.iter().any(|a| a == "--login" || a == "--models") {
            return Ok(());
        }
    }
    if let Some(id) = opt(args, "--remove-account") {
        use rustgis_assistant::chatgpt;
        let accounts = chatgpt::accounts()?;
        let matches: Vec<_> = accounts.iter().filter(|c| c.client_id == id).collect();
        let [c] = matches.as_slice() else {
            return Err("--remove-account needs an unambiguous client id from --accounts".into());
        };
        if let Some(r) = chatgpt::remove(&c.client_id, &c.subject)?
            && let Err(e) = chatgpt::revoke(&r)
        {
            eprintln!(
                "removed on this computer, but OpenAI didn't confirm the sign-out ({e}); disconnect RustGIS in ChatGPT's settings if needed"
            );
            return Ok(());
        }
        eprintln!("removed {}", c.label());
        return Ok(());
    }
    let models = args.iter().any(|a| a == "--models");
    if args.iter().any(|a| a == "--logout") {
        use rustgis_assistant::chatgpt;
        let out = chatgpt::sign_out()?;
        if let Some(e) = &out.unrevoked {
            eprintln!(
                "signed out on this computer, but OpenAI wasn't asked to end the session ({e}); disconnect RustGIS in ChatGPT's settings if needed"
            );
            return Ok(());
        }
        let Some(r) = out.revocation else {
            // No token here to end a session with (none, or a command-line sign-in that has ended).
            eprintln!("signed out on this computer; no session could be ended at OpenAI from here");
            return Ok(());
        };
        if let Err(e) = chatgpt::revoke(&r) {
            eprintln!(
                "signed out on this computer, but OpenAI didn't confirm it ({e}); disconnect RustGIS in ChatGPT's settings if needed"
            );
            return Ok(());
        }
        eprintln!("signed out of ChatGPT");
        return Ok(());
    }
    if args.iter().any(|a| a == "--login") {
        use rustgis_assistant::chatgpt;
        let login = chatgpt::login(args.iter().any(|a| a == "--another-account"))?;
        eprintln!("Open this page in your browser to continue with ChatGPT:\n{}", login.url);
        let c = loop {
            if let Some(r) = login.poll() {
                break r?;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        if !c.plan_allowed() {
            return Err(rustgis_assistant::Error::new(rustgis_assistant::Recovery::Consent, "").to_string());
        }
        let who = if c.email.is_empty() { "your ChatGPT account" } else { c.email.as_str() };
        eprintln!("signed in as {who}; RustGIS now uses your ChatGPT plan (manage usage at {})", chatgpt::USAGE_URL);
        if c.kept == chatgpt::Kept::Session {
            if saved.is_none() {
                eprintln!(
                    "this sign-in isn't remembered: it lasts for this command only (pass --use-saved-secrets to keep it in the system keychain)"
                );
            } else {
                eprintln!("the system keychain didn't keep this sign-in, so it lasts for this command only");
            }
        }
        return Ok(());
    }
    // Account management is done. Read a saved API key only for a prompt, not model listing.
    if !models && prompt.is_none() {
        return Err(NEEDS_REQUEST.into());
    }
    let mut cfg = configuration(&provider, args, env, saved);
    // A signed-in account's requests renew its tokens as needed.
    // Without a usable sign-in, say how to sign in before asking for anything.
    if cfg.login()
        && cfg.key.is_empty()
        && (prompt.is_some() || args.iter().any(|a| a == "--models"))
        && let Some(why) = cfg.missing()
    {
        return Err(format!("{provider}: {why} (run rustgis-cli ask --login)"));
    }
    let login = cfg.login().then(|| cfg.binding());
    let signed_in = login.is_some();
    let transport = move |req: &rustgis_assistant::HttpRequest| match &login {
        Some(account) => rustgis_assistant::chatgpt::send(req, account),
        None => http::send(req),
    };
    if signed_in && !args.iter().any(|a| a == "--models") && prompt.is_some() {
        let r = transport(&provider::models_request(&cfg))?;
        let models = provider::parse_catalog(&r)?;
        if models.is_empty() {
            return Err("no models are available for this account".into());
        }
        let explicit = opt(args, "--model").is_some() || env.model.is_some();
        if !explicit {
            cfg.model.clear();
        }
        provider::use_catalog(&mut cfg, &models, explicit);
    }
    if args.iter().any(|a| a == "--models") {
        let r = transport(&provider::models_request(&cfg))?;
        if signed_in {
            for m in provider::parse_catalog(&r)? {
                out!("{}\t{}", m.id, m.name);
            }
        } else {
            for m in provider::parse_models(cfg.api(), &r)? {
                out!("{m}");
            }
        }
        return Ok(());
    }
    let prompt = prompt.ok_or(NEEDS_REQUEST)?;
    if let Some(why) = cfg.missing() {
        let hint = match provider::preset(&provider) {
            Some(p) if p.login => "run rustgis-cli ask --login".to_string(),
            Some(p) => format!(
                "set {}{}",
                p.key_env.join(" or "),
                if saved.is_none() { ", or pass --use-saved-secrets to use the key saved in the RustGIS app" } else { "" }
            ),
            None => String::new(),
        };
        return Err(format!("{provider}: {why} ({hint})"));
    }
    eprintln!("[{} · {}]", cfg.provider, cfg.model);
    let yes = args.iter().any(|a| a == "--yes");
    let mut a = Agent::new(cfg, s);
    a.set_skills(rustgis_assistant::skills::installed());
    let mut shown = 0;
    let mut show = |a: &Agent| {
        for e in a.transcript.iter().skip(shown) {
            match e {
                Entry::User(_) => {}
                Entry::Assistant(t) => out!("{t}"),
                Entry::Tool { summary, result, error, .. } => {
                    let r: String = result.chars().take(200).collect();
                    eprintln!("  {} {summary} → {r}", if *error { "✗" } else { "✓" });
                }
                Entry::Error(e) => eprintln!("rustgis-cli: {e}"),
                Entry::Note(n) => eprintln!("({n})"),
            }
        }
        shown = a.transcript.len();
    };
    // `--trace FILE`: each request and response body (never the headers, which carry the key).
    let trace = opt(args, "--trace").map(std::path::PathBuf::from);
    let send = |req: &rustgis_assistant::HttpRequest| {
        let r = transport(req);
        if let Some(path) = &trace {
            let resp = match &r {
                Ok(r) => format!("HTTP {}\n{}", r.status, String::from_utf8_lossy(&r.body)),
                Err(e) => e.to_string(),
            };
            let entry = format!("> {} {}\n{}\n< {resp}\n\n", req.method, req.url, req.body.as_deref().unwrap_or(""));
            let written = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(entry.as_bytes()));
            if let Err(e) = written {
                eprintln!("rustgis-cli: {}: {e}", path.display());
            }
        }
        r
    };
    // `--yes` allows file and network access; actions reserved to the user (plugins, scripts,
    // tasks, models, replacing the project) are never run from here.
    agent::run_blocking(&mut a, &prompt, s, send, |what, why, access| {
        let allow = yes && access;
        if !allow {
            let hint = if access { "; pass --yes to allow" } else { "" };
            eprintln!("  declined: {what} ({why}{hint})");
        }
        allow
    });
    show(&a);
    if a.transcript.iter().any(|e| matches!(e, Entry::Error(_))) {
        return Err("the assistant couldn't finish".into());
    }
    Ok(())
}

/// `--attach [PORT]` as `mcp` takes it: the port that follows when it isn't another flag, else
/// the app's default.
fn attach_port(args: &[String]) -> Result<Option<u16>, String> {
    let Some(i) = args.iter().position(|a| a == "--attach") else { return Ok(None) };
    match args.get(i + 1).filter(|a| !a.starts_with("--")) {
        Some(p) => p.parse::<u16>().map(Some).map_err(|_| format!("mcp --attach {p}: expected a port number")),
        None => rustgis_assistant::relay::default_port().map(Some),
    }
}

/// `mcp --attach [PORT]`: the same MCP server, on the desktop app that is already open. The
/// app's session runs the calls (its MCP Relay command started the listener on 127.0.0.1); this
/// process relays the protocol only. `--yes` goes along with the connection, so the app applies
/// the same file and network refusals as the private server does here.
fn attach(port: u16, allow_access: bool, token_file: Option<&str>, timeout: Option<f64>) -> Result<(), String> {
    // The relay wrote the run's token where only this user can read it; carrying it in the
    // attach message is what proves this is the user's own client, so nothing is pasted. An
    // agent the app starts itself gets a token file of its own (`--token-file`).
    let token = match token_file {
        Some(f) => rustgis_assistant::relay::read_token_file(std::path::Path::new(f))?,
        None => rustgis_assistant::relay::read_token(port)?,
    };
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("nothing is listening on 127.0.0.1:{port} ({e}); start RustGIS's MCP Relay command first"))?;
    let mut hello = json!({"jsonrpc": "2.0", "method": "rustgis/attach", "params": {"access": allow_access, "token": token}});
    // How long each call may wait for the app (5 to 600 seconds; 60 unless asked).
    if let (Some(secs), Some(params)) = (timeout, hello.get_mut("params").and_then(Value::as_object_mut)) {
        params.insert("timeout_s".into(), json!(secs));
    }
    writeln!(stream, "{hello}").map_err(|e| format!("telling the app what --yes allows: {e}"))?;
    eprintln!(
        "rustgis-cli: MCP server ready (the open RustGIS app at 127.0.0.1:{port}; file and network access {})",
        if allow_access { "allowed" } else { "off, --yes allows it" }
    );
    let mut input = std::io::BufReader::new(stream.try_clone().map_err(|e| format!("opening the app's replies: {e}"))?);
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    loop {
        let line = match rustgis_assistant::framing::read_message(&mut stdin, rustgis_assistant::framing::Oversize::Skip) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(why) => {
                let e = json!({"jsonrpc": "2.0", "id": Value::Null, "error": {"code": -32700, "message": read_error(why, "reading standard input")}});
                let _ = writeln!(stdout, "{e}").and_then(|()| stdout.flush());
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        writeln!(stream, "{line}").and_then(|()| stream.flush()).map_err(|e| format!("sending to the app: {e}"))?;
        if !rustgis_assistant::mcp::expects_reply(&line) {
            continue;
        }
        let reply = match rustgis_assistant::framing::read_message(&mut input, rustgis_assistant::framing::Oversize::Stop) {
            Ok(Some(reply)) => reply,
            Ok(None) => return Err("the RustGIS app closed the relay".into()),
            Err(why) => return Err(format!("the app's reply: {why}")),
        };
        writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).map_err(|e| format!("writing to standard output: {e}"))?;
    }
    Ok(())
}

/// `mcp-register`: registers the desktop app's MCP over HTTP with an agent (and makes the token
/// file when there is none yet, as the app would; the app needn't be running). The address is the
/// app's HTTP port (`--port`, else `RUSTGIS_MCP_HTTP_PORT` or 8768), the token the stable one in
/// the settings folder, the same file the app reads. `--client claude` runs `claude mcp add`;
/// without it the configuration is printed for any client.
fn mcp_register(args: &[String]) -> Result<(), String> {
    use rustgis_assistant::relay::{HTTP_PATH, default_http_port, http_token, http_token_path};
    // A flag given without its value is an error, not the default (a bare --client would print
    // the token instead of registering it).
    for flag in ["--client", "--name", "--port"] {
        if args.iter().position(|a| a == flag).is_some_and(|i| args.get(i + 1).is_none_or(|v| v.starts_with("--"))) {
            return Err(format!("mcp-register {flag}: expected a value"));
        }
    }
    let port = match opt(args, "--port") {
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| format!("mcp-register --port {p}: expected a port number (1 to 65535)"))?,
        None => default_http_port(),
    };
    let name = opt(args, "--name").unwrap_or("rustgis-live");
    // Passed to claude as an argument: a name that looks like an option is refused.
    if name.starts_with('-') || name.trim().is_empty() {
        return Err(format!("mcp-register --name {name}: expected a name, not an option"));
    }
    let url = format!("http://127.0.0.1:{port}{HTTP_PATH}");
    let token = http_token()?;
    let header = format!("Authorization: Bearer {token}");
    match opt(args, "--client") {
        Some("claude") => {
            eprintln!(
                "rustgis-cli: passing the MCP token to `claude mcp add` (other users of this computer can see a running program's arguments for that moment)"
            );
            let add = ["mcp", "add", "--transport", "http", name, &url, "--header", &header];
            // An npm install on Windows provides only claude.cmd.
            let run = |program: &str| std::process::Command::new(program).args(add).status();
            let status = match run("claude") {
                Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::NotFound => run("claude.cmd"),
                other => other,
            }
            .map_err(|e| format!("running claude: {e} (is Claude Code installed?)"))?;
            if !status.success() {
                return Err(format!("claude mcp add failed ({status})"));
            }
            eprintln!("rustgis-cli: registered {name} ({url}) with Claude Code; RustGIS serves it while agents are turned on in the app");
            Ok(())
        }
        Some(other) => {
            Err(format!("mcp-register --client {other}: only claude is run for you; leave --client out to print the configuration"))
        }
        None => {
            print(&json!({"mcpServers": {name: {"type": "http", "url": url, "headers": {"Authorization": format!("Bearer {token}")}}}}));
            eprintln!(
                "rustgis-cli: the token is kept in {}; deleting it makes a new one (re-register then)",
                http_token_path().map(|p| p.display().to_string()).unwrap_or_default()
            );
            Ok(())
        }
    }
}

/// The complaint for a message that could not be read: the protocol's own words about the
/// message, or the failing reader named by its caller (the framing helper's I/O text is
/// neutral and names no source).
fn read_error(why: rustgis_assistant::framing::Error, source: &str) -> String {
    match why {
        rustgis_assistant::framing::Error::Io(e) => format!("{source}: {e}"),
        other => other.to_string(),
    }
}

/// `mcp`: a Model Context Protocol server on standard input and output, one JSON-RPC message per
/// line, until the client closes its end. Only messages go to standard output; notes go to
/// standard error. `--attach [PORT]` serves the same protocol on the desktop app's session
/// instead of a private one.
fn mcp(s: &mut Session, args: &[String]) -> Result<(), String> {
    let allow_access = args.iter().any(|a| a == "--yes");
    if let Some(port) = attach_port(args)? {
        if args.iter().any(|a| matches!(a.as_str(), "--sample" | "--project" | "--data")) {
            return Err("mcp --attach manages the project open in the app; --sample, --project and --data make a private one".into());
        }
        let timeout = match opt(args, "--timeout") {
            Some(t) => Some(
                t.parse::<f64>()
                    .ok()
                    .filter(|s| (5.0..=600.0).contains(s))
                    .ok_or_else(|| format!("mcp --timeout {t}: expected seconds from 5 to 600"))?,
            ),
            None => None,
        };
        return attach(port, allow_access, opt(args, "--token-file"), timeout);
    }
    if args.iter().any(|a| a == "--timeout") {
        return Err("mcp --timeout sets how long calls wait for the open app; use it with --attach".into());
    }
    let mut server = rustgis_assistant::mcp::Server::new(allow_access);
    server.skills = rustgis_assistant::skills::installed();
    eprintln!(
        "rustgis-cli: MCP server ready ({} layers; file and network access {})",
        s.map().layers.len(),
        if server.allow_access { "allowed" } else { "off, --yes allows it" }
    );
    // The longest message read (a project's JSON can be large); longer lines are refused.
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    loop {
        let refuse = |stdout: &mut std::io::StdoutLock, why: &str| {
            let e = json!({"jsonrpc": "2.0", "id": Value::Null, "error": {"code": -32700, "message": why}});
            writeln!(stdout, "{e}").and_then(|()| stdout.flush()).is_ok()
        };
        let line = match rustgis_assistant::framing::read_message(&mut stdin, rustgis_assistant::framing::Oversize::Skip) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(why) => {
                if !refuse(&mut stdout, &read_error(why, "reading standard input")) {
                    break;
                }
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // A last resort: a panic answers that request with an error instead of ending the server.
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.handle(s, &line))).unwrap_or_else(|_| {
            // The panic message is already on standard error; the id lets the client match the error.
            eprintln!("rustgis-cli: a request failed unexpectedly; the project may be partly changed");
            let id = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|v| v.get("id").cloned())
                .filter(|id| id.is_string() || id.is_number())
                .unwrap_or(Value::Null);
            Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": "internal error"}}).to_string())
        });
        if let Some(reply) = reply {
            // The client hung up.
            if writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).is_err() {
                break;
            }
        }
    }
    Ok(())
}

/// Runs the command line, then stops the session plugins' programs its commands started.
fn main() -> ExitCode {
    let code = cli();
    // Session plugins' programs a command started end with the command line.
    rustgis_engine::plugins::session::stop_all();
    code
}

/// The command line: loads the plugins, then runs the subcommand the arguments name.
fn cli() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(|s| s.as_str()) else { return usage() };
    if !args.iter().any(|a| a == "--no-plugins") {
        for e in rustgis_engine::cmd::plugins::load_installed() {
            eprintln!("rustgis-cli: warning: {e}");
        }
    }
    for (i, a) in args.iter().enumerate() {
        if a == "--plugin"
            && let Some(p) = args.get(i + 1)
        {
            let loaded =
                rustgis_engine::plugins::Package::from_path(std::path::Path::new(p)).and_then(|pkg| rustgis_engine::plugins::load(pkg, p));
            if let Err(e) = loaded {
                eprintln!("rustgis-cli: {p}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    // Whose borders the built-in countries and world outline show: the system's by default.
    let worldview = opt(&args, "--worldview").unwrap_or("auto");
    if worldview != "auto" && rustgis_engine::sample::Worldview::from_code(worldview).is_none() {
        eprintln!("rustgis-cli: --worldview {worldview}: expected auto, default or china");
        return ExitCode::from(2);
    }
    rustgis_engine::sample::apply_worldview(worldview);
    let mut s = Session::new(Project::default());
    // `--use-saved-secrets`: the desktop app's saved connections, and the passwords, keys and
    // ChatGPT sign-in it keeps in the system keychain. Without it, nothing reads the keychain.
    // Opting in reads the preferences once and touches no secret; the keychain is asked only when
    // a command needs one, and a keychain that can't be used is warned about once.
    let saved: Option<Saved> = if args.iter().any(|a| a == "--use-saved-secrets") {
        let prefs =
            rustgis_engine::prefs::opt_in(&mut s, rustgis_keychain::Keychain::default(), |w| eprintln!("rustgis-cli: warning: {w}"));
        // The ChatGPT sign-in shares the session's store.
        s.database_logins.secret_store().cloned().map(|store| {
            rustgis_assistant::chatgpt::set_secret_store(Some(store.clone()));
            Saved { prefs, store }
        })
    } else {
        None
    };
    let res: Result<(), String> = (|| match cmd {
        "plugins" => {
            let registry = opt(&args, "--registry").map(|u| json!(u)).unwrap_or(Value::Null);
            let arg = |what: &str| args.get(2).filter(|a| !a.starts_with("--")).cloned().ok_or(format!("plugins {what} needs an argument"));
            let out = match args.get(1).map(String::as_str) {
                None | Some("list") => run(&mut s, "plugin.list", Value::Null)?,
                Some("install") => run(&mut s, "plugin.install", json!({"path": arg("install")?}))?,
                Some("uninstall") => run(&mut s, "plugin.uninstall", json!({"id": arg("uninstall")?}))?,
                Some("enable") => run(&mut s, "plugin.enable", json!({"id": arg("enable")?, "enabled": true}))?,
                Some("disable") => run(&mut s, "plugin.enable", json!({"id": arg("disable")?, "enabled": false}))?,
                Some("browse") => run(&mut s, "plugin.registry", json!({"url": registry}))?,
                Some("get") => run(&mut s, "plugin.installFromRegistry", json!({"id": arg("get")?, "url": registry}))?,
                Some("prepare") => json!({"python": prepare_env(&arg("prepare")?)?}),
                Some(other) => return Err(format!("unknown plugins subcommand '{other}'")),
            };
            // A Python plugin just installed gets its environment now, so it runs.
            if matches!(args.get(1).map(String::as_str), Some("install" | "get"))
                && let Some(id) = out.get("id").and_then(Value::as_str)
                && rustgis_engine::plugins::pyenv::status(id) == Some(false)
            {
                prepare_env(id)?;
            }
            print(&out);
            Ok(())
        }
        "skills" => skills(&args),
        "mcp-register" => mcp_register(&args),
        "commands" => {
            print(&run(&mut s, "info.commands", Value::Null)?);
            Ok(())
        }
        "tools" => {
            print(&run(&mut s, "gp.tools", Value::Null)?);
            Ok(())
        }
        "info" => {
            let path = args.get(1).ok_or("info needs a file")?;
            load(&mut s, path)?;
            print(&run(&mut s, "info.project", Value::Null)?);
            Ok(())
        }
        "logo" => {
            // The app icon as a PNG (or an .ico for the Windows installers), for installers and
            // desktop entries.
            let out = args.get(1).filter(|a| !a.starts_with("--")).ok_or("logo needs <out.png> first")?;
            let lower = out.to_ascii_lowercase();
            let size = match opt(&args, "--size") {
                Some(v) => v.parse::<u32>().map_err(|_| format!("--size {v}: expected a whole number"))?,
                None => 512,
            }
            .clamp(16, 1024);
            if !lower.ends_with(".bmp") && (opt(&args, "--canvas").is_some() || opt(&args, "--at").is_some()) {
                return Err("--canvas and --at apply to a .bmp only".into());
            }
            if lower.ends_with(".ico") {
                // Every icon size from 16 to 256 px: --size does not apply.
                let ico = rustgis_engine::render::logo::ico().ok_or("could not draw the logo")?;
                std::fs::write(out, ico).map_err(|e| format!("{out}: {e}"))?;
                return Ok(());
            }
            if lower.ends_with(".bmp") {
                // An installer wizard image: the logo on a white canvas (--canvas WxH, default
                // the logo's size), at --at X,Y (default centred).
                let pair = |name: &str, sep: char| -> Result<Option<(i64, i64)>, String> {
                    let Some(v) = opt(&args, name) else { return Ok(None) };
                    let bad = || format!("{name} {v}: expected two whole numbers like 10{sep}20");
                    let (a, b) = v.split_once(sep).ok_or_else(bad)?;
                    Ok(Some((a.trim().parse().map_err(|_| bad())?, b.trim().parse().map_err(|_| bad())?)))
                };
                let (w, h) = pair("--canvas", 'x')?.unwrap_or((i64::from(size), i64::from(size)));
                let (w, h) = (w.clamp(1, 2048) as u32, h.clamp(1, 2048) as u32);
                let (x, y) = pair("--at", ',')?.unwrap_or(((i64::from(w) - i64::from(size)) / 2, (i64::from(h) - i64::from(size)) / 2));
                let (x, y) = (x.clamp(-4096, 4096) as i32, y.clamp(-4096, 4096) as i32);
                let bmp = rustgis_engine::render::logo::bmp(size, w, h, x, y).ok_or("could not draw the logo")?;
                std::fs::write(out, bmp).map_err(|e| format!("{out}: {e}"))?;
                return Ok(());
            }
            let png =
                rustgis_engine::render::logo::pixmap(size).ok_or("could not draw the logo")?.encode_png().map_err(|e| e.to_string())?;
            std::fs::write(out, png).map_err(|e| format!("{out}: {e}"))?;
            Ok(())
        }
        "render" => {
            let (Some(input), Some(out)) = (args.get(1), args.get(2)) else { return Err("render needs <input> <out.png>".into()) };
            load(&mut s, input)?;
            if let Some(b) = opt(&args, "--basemap") {
                run(&mut s, "map.basemap", json!({"name": b}))?;
            }
            // A web tile basemap: raster tiles, vector tiles or a MapLibre style by address.
            if let Some(u) = opt(&args, "--tiles") {
                run(&mut s, "map.addTiles", json!({"url": u}))?;
            }
            let w = num(&args, "--width").unwrap_or(1600.0).clamp(16.0, 8000.0);
            let h = num(&args, "--height").unwrap_or(900.0).clamp(16.0, 8000.0);
            s.view_size = (w, h);
            if !is_project(input) {
                run(&mut s, "map.zoomFull", Value::Null)?;
            }
            let dpi = num(&args, "--dpi").unwrap_or(96.0);
            print(&run(&mut s, "share.exportMap", json!({"path": out, "dpi": dpi}))?);
            Ok(())
        }
        "ask" => {
            open_inputs(&mut s, &args)?;
            // What the assistant did is saved even when it couldn't finish.
            let asked = ask(&mut s, &args, saved.as_ref(), &Environment::process());
            if let Some(p) = opt(&args, "--save")
                && !args.iter().any(|a| a == "--models")
            {
                run(&mut s, "project.saveAs", json!({"path": p}))?;
            }
            asked
        }
        "mcp" => {
            if !args.iter().any(|a| a == "--attach") {
                open_inputs(&mut s, &args)?;
            }
            mcp(&mut s, &args)
        }
        "run" => {
            // Nothing waits on `run` but the shell (an assistant or an MCP client does, and has a
            // timeout): long runs take the app's bounds, their progress on stderr. A person at the
            // terminal can stop a run with Ctrl-C, so there tools take no work cap; a program
            // calling it may have a timeout, and keeps the caps.
            s.long_runs = Some(progress);
            s.stoppable = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            open_inputs(&mut s, &args)?;
            for (i, a) in args.iter().enumerate() {
                if a == "--cmd"
                    && let Some(c) = args.get(i + 1)
                {
                    let (id, p) = match c.split_once('=') {
                        Some((id, json)) => (id, serde_json::from_str(json).map_err(|e| format!("{id}: bad JSON: {e}"))?),
                        None => (c.as_str(), Value::Null),
                    };
                    let out = run(&mut s, id, p)?;
                    if !out.is_null() {
                        print(&out);
                    }
                }
            }
            if let Some(p) = opt(&args, "--save") {
                run(&mut s, "project.saveAs", json!({"path": p}))?;
            }
            Ok(())
        }
        _ => Err(format!("unknown subcommand '{cmd}'")),
    })();
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rustgis-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretError};
    use std::sync::{Arc, Mutex};

    /// A store that records the accounts it is asked for.
    struct Asked {
        inner: MemorySecrets,
        accounts: Mutex<Vec<String>>,
    }

    impl Asked {
        fn with(inner: MemorySecrets) -> Arc<Asked> {
            Arc::new(Asked { inner, accounts: Mutex::new(Vec::new()) })
        }

        fn accounts(&self) -> Vec<String> {
            self.accounts.lock().unwrap().clone()
        }
    }

    impl SecretStore for Asked {
        fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
            self.accounts.lock().unwrap().push(account.to_string());
            self.inner.load(account)
        }

        fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
            self.inner.save(account, value)
        }

        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.inner.delete(account)
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    /// An environment with no keys.
    fn bare() -> Environment {
        fn config(provider: &str) -> Config {
            let mut c = Config::from_env(provider);
            c.key.clear();
            c
        }
        Environment { provider: None, model: None, config }
    }

    /// An environment with an OpenAI key.
    fn with_openai_key() -> Environment {
        fn config(provider: &str) -> Config {
            let mut c = Config::from_env(provider);
            c.key = if provider == "openai" { "env-key".into() } else { String::new() };
            c
        }
        Environment { provider: None, model: None, config }
    }

    fn desktop(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), json!(v))).collect()
    }

    fn saved(prefs: Map<String, Value>, store: &Arc<Asked>) -> Saved {
        Saved { prefs, store: store.clone() }
    }

    fn remembered() -> Map<String, Value> {
        desktop(&[(provider::PROVIDER_PREF, "anthropic"), (provider::REMEMBER_KEY_PREF, "on")])
    }

    fn anthropic_key() -> Arc<Asked> {
        let memory = MemorySecrets::new();
        memory.save("assistant/anthropic", &Secret::new("stored-sentinel")).unwrap();
        Asked::with(memory)
    }

    #[test]
    fn a_saved_provider_is_used_only_with_the_opt_in_and_after_explicit_and_environment_choices() {
        let store = anthropic_key();
        let opted_in = saved(remembered(), &store);
        let none: [&str; 1] = ["ask"];
        // Without --use-saved-secrets the desktop's choice is never consulted.
        assert_eq!(choose_provider(&args(&none), &bare(), None), "ollama");
        assert_eq!(choose_provider(&args(&none), &bare(), Some(&opted_in)), "anthropic");
        assert_eq!(choose_provider(&args(&["ask", "--provider", "gemini"]), &bare(), Some(&opted_in)), "gemini");
        let environment = Environment { provider: Some("openrouter".into()), ..bare() };
        assert_eq!(choose_provider(&args(&none), &environment, Some(&opted_in)), "openrouter");
        // A provider with a key in the environment is a more specific choice than a saved one.
        assert_eq!(choose_provider(&args(&none), &with_openai_key(), Some(&opted_in)), "openai");
        // Choosing a provider reads no secret.
        assert!(store.accounts().is_empty());
    }

    #[test]
    fn a_prompt_uses_the_saved_key_but_model_listing_does_not_read_it() {
        let store = anthropic_key();
        let opted_in = saved(remembered(), &store);
        let list = configuration("anthropic", &args(&["ask", "--models"]), &bare(), Some(&opted_in));
        let request = provider::models_request(&list);
        assert!(request.headers.iter().all(|(_, value)| !value.contains("stored-sentinel")));
        assert!(store.accounts().is_empty());
        let environment_list = configuration("openai", &args(&["ask", "--models"]), &with_openai_key(), Some(&opted_in));
        assert_eq!(environment_list.key, "env-key");
        assert!(store.accounts().is_empty());
        let prompt = configuration("anthropic", &args(&["ask", "--provider", "anthropic", "hi"]), &bare(), Some(&opted_in));
        assert_eq!(prompt.key, "stored-sentinel");
        assert_eq!(store.accounts(), ["assistant/anthropic"]);
    }

    #[test]
    fn without_the_opt_in_or_consent_no_key_is_read_and_the_environment_key_wins() {
        let store = anthropic_key();
        // No --use-saved-secrets: the model list goes out without the saved key.
        let request = provider::models_request(&configuration("anthropic", &args(&["ask", "--models"]), &bare(), None));
        assert!(request.headers.iter().all(|(_, value)| !value.contains("stored-sentinel")));
        // Opted in, but the desktop does not remember keys.
        let off = saved(desktop(&[(provider::REMEMBER_KEY_PREF, "off")]), &store);
        assert_eq!(configuration("anthropic", &args(&["ask", "hi"]), &bare(), Some(&off)).key, "");
        // A key from the environment is the one used, and nothing is read.
        let opted_in = saved(remembered(), &store);
        assert_eq!(configuration("openai", &args(&["ask", "hi"]), &with_openai_key(), Some(&opted_in)).key, "env-key");
        // The desktop sends its key to its own address only: another one gets no key.
        let elsewhere = args(&["ask", "--base-url", "https://elsewhere.example/v1", "hi"]);
        assert_eq!(configuration("anthropic", &elsewhere, &bare(), Some(&opted_in)).key, "");
        assert!(store.accounts().is_empty());
    }

    /// Runs the test `name` again in a child process whose settings folder is a fresh temporary
    /// one, so the ChatGPT sign-in operations never touch the developer's own. Gives whether
    /// this is the child, which runs the test's body.
    fn in_isolated_settings(name: &str) -> bool {
        if std::env::var_os("RUSTGIS_TEST_ISOLATED").is_some() {
            return true;
        }
        let dir = std::env::temp_dir().join(format!("rustgis-cli-{}-{}", name.rsplit("::").next().unwrap_or(name), std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name])
            .env("RUSTGIS_TEST_ISOLATED", "1")
            .env(rustgis_engine::data::dirs::CONFIG_ENV, &dir)
            .output()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("1 passed"), "the isolated run didn't run {name}:\n{stdout}");
        false
    }

    #[test]
    fn managing_the_chatgpt_sign_in_never_reads_an_api_key() {
        if !in_isolated_settings("tests::managing_the_chatgpt_sign_in_never_reads_an_api_key") {
            return;
        }
        let store = anthropic_key();
        let opted_in = saved(remembered(), &store);
        let mut s = Session::new(Project::default());
        let env = bare();
        let management: [&[&str]; 4] =
            [&["ask", "--accounts"], &["ask", "--logout"], &["ask", "--remove-account", "nobody"], &["ask", "--account", "nobody"]];
        for management in management {
            let _ = ask(&mut s, &args(management), Some(&opted_in), &env);
        }
        assert!(store.accounts().is_empty(), "asked for {:?}", store.accounts());
    }
}
