//! The AI Assistant pane: a chat with a model of the user's choosing, which acts on the map by
//! running commands (see the `rustgis-assistant` crate). Requests go out off the frame: on a
//! thread on the desktop, through `Services::http` (the browser's fetch) on the web.

use egui::{Align, Layout, RichText, Ui, vec2};
use rustgis_assistant::{Agent, Config, Entry, HttpRequest, HttpResponse, PRESETS, Step, chatgpt, provider};

use crate::theme::{Tokens, regular, semibold};
use crate::{RustGisApp, Slot, icons};

type Reply = Slot<Result<HttpResponse, rustgis_assistant::Error>>;
/// A streamed reply's pieces, as they arrive.
type Chunks = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

/// Example requests offered in an empty conversation.
const SUGGESTIONS: &[&str] = &[
    "Which 5 countries have the largest GDP?",
    "Zoom to Tokyo",
    "Label the cities with their names",
    "Buffer the cities by 200 km",
    "Switch to a dark basemap",
];

#[derive(Default)]
pub struct AssistantState {
    pub(crate) agent: Option<Agent>,
    pub input: String,
    /// The input box's height, set by dragging the grip above it (0 until dragged: two lines).
    input_height: f32,
    /// A message to send on the next frame (`ui.pane.assistant {"ask": …}`).
    pub queued: Option<String>,
    /// The request out: its generation and where its response lands.
    inflight: Option<(u64, Reply)>,
    /// The pieces of the request out's reply, as they stream in (desktop).
    chunks: Option<(u64, Chunks)>,
    /// The settings form is open.
    pub settings: bool,
    /// Startup layout waits for the initial key read unless the user chooses it first.
    #[cfg(not(target_arch = "wasm32"))]
    settings_pending: Option<String>,
    show_key: bool,
    /// Keys are remembered only when the user says so.
    remember_key: bool,
    /// Desktop keychain operations and session keys, separate from preference storage.
    #[cfg(not(target_arch = "wasm32"))]
    keys: crate::assistantkeys::Keys,
    /// Legacy migration is scheduled once after the platform preferences are available.
    #[cfg(not(target_arch = "wasm32"))]
    keys_migrated: bool,
    /// The AI provider section of the settings is folded away (kept in the preferences).
    provider_closed: bool,
    models: Vec<String>,
    catalog: Vec<provider::Model>,
    /// The catalog is fetched once per provider/account selection.
    catalog_pending: bool,
    models_slot: Option<Reply>,
    models_error: Option<rustgis_assistant::Error>,
    /// Earlier messages, for Up/Down in the composer.
    history: Vec<String>,
    history_pos: Option<usize>,
    /// "Allow for this conversation" in the approval card.
    always: bool,
    loaded: bool,
    /// Settings changed and not yet saved: they are saved once no settings field is being typed in.
    unsaved: bool,
    /// A ChatGPT sign-in under way in the browser (desktop only).
    #[cfg(not(target_arch = "wasm32"))]
    login: Option<chatgpt::Login>,
    /// The ChatGPT account signed in, for the settings.
    account: Option<chatgpt::Account>,
    login_error: Option<rustgis_assistant::Error>,
    /// A sign-out's revocation at OpenAI, under way (desktop only).
    #[cfg(not(target_arch = "wasm32"))]
    signout: Option<Slot<Result<(), rustgis_assistant::Error>>>,
    /// The ChatGPT sign-in, being read on a worker: its tokens are in the system keychain, which
    /// may block or ask (desktop only).
    #[cfg(not(target_arch = "wasm32"))]
    sign_in_read: Option<SignInRead>,
    /// The first sign-in's confirmation that the ChatGPT plan is in use, until dismissed.
    welcome: bool,
    /// A failed request to send again: its generation and when.
    retry: Option<(u64, web_time::Instant)>,
    /// The user lets the assistant hand tasks to local agents (Claude Code, Codex).
    local_agents: bool,
    /// Agent threads on this computer: their processes, windows and requests (desktop).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) threads: crate::agentthreads::Threads,
    /// The user lets agents' requests through without asking (off unless chosen).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) agents_auto: bool,
    /// The saved ChatGPT registrations, read once (the file is locked across processes) and
    /// again after a sign-in, sign-out or selection.
    #[cfg(not(target_arch = "wasm32"))]
    accounts: Option<Vec<chatgpt::Credentials>>,
    /// The last sign-in added a second registration for an account already saved.
    #[cfg(not(target_arch = "wasm32"))]
    duplicate: bool,
    /// The plugins' revision when the agent's skills were read: plugins carry skills, so they're
    /// read again when plugins change.
    skills_rev: u64,
    /// The settings' Skills list, read when shown (desktop).
    #[cfg(not(target_arch = "wasm32"))]
    skills_list: Option<(Vec<rustgis_assistant::skills::Listed>, Vec<String>)>,
    /// What the last change to the skills did: whether it worked, and what to say.
    #[cfg(not(target_arch = "wasm32"))]
    skills_message: Option<(bool, String)>,
    /// The agents on this computer, looked for when the settings open (a search of `PATH`, not
    /// every frame) and again when the switch changes.
    #[cfg(not(target_arch = "wasm32"))]
    agents_found: Option<rustgis_assistant::acp::Installed>,
}

/// The settings fields typed into (saved when none has focus).
const FIELDS: [&str; 3] = ["assistant_model", "assistant_url", "assistant_key"];

/// Whether a provider settings field still owns the keyboard focus.
fn typing(ctx: &egui::Context) -> bool {
    ctx.memory(|m| FIELDS.iter().any(|f| m.has_focus(egui::Id::unique(f))))
}

/// Reads a nonempty preference, including legacy keys awaiting migration.
fn pref(app: &RustGisApp, key: &str) -> Option<String> {
    app.services.load_pref.as_ref().and_then(|l| l(key)).filter(|v| !v.is_empty())
}

/// Saves a preference through the platform hook.
fn save_pref(app: &RustGisApp, key: &str, value: &str) {
    if let Some(s) = &app.services.save_pref {
        s(key, value);
    }
}

/// A provider's settings: its defaults and the environment, then what the user saved.
fn load_config(app: &mut RustGisApp, provider: &str, ctx: &egui::Context) -> Config {
    let mut c = Config::from_env(provider);
    if let Some(m) = pref(app, &format!("assistant_model_{provider}")) {
        c.model = m;
    }
    if let Some(u) = pref(app, &provider::url_pref(provider)).filter(|_| !c.login()) {
        c.base_url = u;
    }
    if !c.login() {
        let legacy = pref(app, &format!("assistant_key_{provider}"));
        #[cfg(not(target_arch = "wasm32"))]
        {
            c.key = app.assistant.keys.load(provider, &c.key, legacy, app.assistant.remember_key, app.services.secrets.clone(), ctx);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = ctx;
            if let Some(key) = legacy {
                c.key = key;
            }
        }
    }
    c
}

/// Initializes the selected provider once, without blocking on its keychain.
fn ensure_loaded(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.assistant.loaded {
        return;
    }
    app.assistant.loaded = true;
    app.assistant.remember_key = provider::remembers_keys(pref(app, provider::REMEMBER_KEY_PREF).as_deref());
    let provider = pref(app, provider::PROVIDER_PREF)
        .or_else(|| std::env::var("RUSTGIS_ASSISTANT_PROVIDER").ok())
        .filter(|p| provider::preset(p).is_some())
        // The first provider with a key in the environment (a ChatGPT sign-in is chosen, not assumed).
        .or_else(|| PRESETS.iter().find(|p| !p.login && !Config::from_env(p.id).key.is_empty()).map(|p| p.id.to_string()))
        .unwrap_or_else(|| "anthropic".into());
    let mut cfg = load_config(app, &provider, ctx);
    #[cfg(not(target_arch = "wasm32"))]
    migrate_keys(app, ctx);
    if let Ok(m) = std::env::var("RUSTGIS_ASSISTANT_MODEL")
        && !m.trim().is_empty()
        && pref(app, &format!("assistant_model_{provider}")).is_none()
    {
        cfg.model = m.trim().to_string();
    }
    #[cfg(not(target_arch = "wasm32"))]
    if cfg.login() {
        read_sign_in(app, None, ctx);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let loading = reading(app, &provider);
    #[cfg(target_arch = "wasm32")]
    let loading = false;
    if !loading {
        init_settings(app, &cfg);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if loading {
        app.assistant.settings_pending = Some(provider);
    }
    let mut agent = Agent::new(cfg, &app.session);
    app.assistant.local_agents = pref(app, "assistant_local_agents").as_deref() == Some("on");
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.assistant.agents_auto = pref(app, "assistant_agents_auto").as_deref() == Some("on");
    }
    // The skills switched on (the plugins' too), and the local agents the user allows (desktop).
    agent.set_skills(rustgis_assistant::skills::installed());
    #[cfg(not(target_arch = "wasm32"))]
    agent.set_agents_auto(app.assistant.agents_auto);
    app.assistant.skills_rev = rustgis_engine::plugins::revision();
    #[cfg(not(target_arch = "wasm32"))]
    if app.assistant.local_agents {
        agent.set_harnesses(rustgis_assistant::acp::installed().0.into_iter().map(|(spec, _)| spec).collect());
    }
    app.assistant.agent = Some(agent);
}

/// An unconfigured provider opens its fields; a configured one restores the saved fold state.
fn init_settings(app: &mut RustGisApp, cfg: &Config) {
    let missing = cfg.missing().is_some();
    app.assistant.settings = missing;
    app.assistant.provider_closed = !missing && pref(app, "assistant_provider_closed").as_deref() == Some("on");
}

/// An explicit settings choice takes precedence over startup's pending key read.
fn toggle_settings(app: &mut RustGisApp) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.assistant.settings_pending = None;
    }
    app.assistant.settings = !app.assistant.settings;
}

/// Schedules all remembered legacy providers once, without opening the Assistant or making requests.
#[cfg(not(target_arch = "wasm32"))]
fn migrate_keys(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.assistant.keys_migrated {
        return;
    }
    app.assistant.keys_migrated = true;
    if !provider::remembers_keys(pref(app, provider::REMEMBER_KEY_PREF).as_deref()) {
        return;
    }
    for p in PRESETS.iter().filter(|p| !p.login) {
        if let Some(legacy) = pref(app, &format!("assistant_key_{}", p.id)) {
            app.assistant.keys.load(p.id, "", Some(legacy), true, app.services.secrets.clone(), ctx);
        }
    }
}

/// Reads the skills again when they changed (`force`) or the plugins did.
fn sync_skills(app: &mut RustGisApp, force: bool) {
    let rev = rustgis_engine::plugins::revision();
    if !force && rev == app.assistant.skills_rev {
        return;
    }
    app.assistant.skills_rev = rev;
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.assistant.skills_list = None;
    }
    if let Some(a) = app.assistant.agent.as_mut() {
        a.set_skills(rustgis_assistant::skills::installed());
    }
}

/// A sign-in read under way: the account it checks (`None`: take whichever is selected) and
/// where the worker leaves what it read.
#[cfg(not(target_arch = "wasm32"))]
type SignInRead = (Option<chatgpt::Binding>, Slot<Option<chatgpt::Credentials>>);

/// Starts reading the selected ChatGPT sign-in on a worker; [`poll_sign_in`] applies it. With
/// `checking`, it only says whether that sign-in is still there (after a failed request).
#[cfg(not(target_arch = "wasm32"))]
fn read_sign_in(app: &mut RustGisApp, checking: Option<chatgpt::Binding>, ctx: &egui::Context) {
    let slot: Slot<Option<chatgpt::Credentials>> = Slot::default();
    let (out, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || {
        let c = chatgpt::load_any();
        if let Ok(mut s) = out.lock() {
            *s = Some(c);
        }
        ctx.request_repaint();
    });
    app.assistant.sign_in_read = Some((checking, slot));
}

/// Applies a finished sign-in read, if the sign-in provider is still the one chosen (account
/// actions drop a read begun before them). A check after a failed request never takes another
/// account; when its sign-in is gone (expired, revoked, signed out elsewhere) the key is cleared
/// and the settings open to sign in again. A read taking the selected account goes through
/// [`apply_config`], which stops work bound to another account, and loads the new catalog.
#[cfg(not(target_arch = "wasm32"))]
fn poll_sign_in(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some((checking, slot)) = app.assistant.sign_in_read.as_ref() else { return };
    let Some(read) = slot.lock().ok().and_then(|mut s| s.take()) else { return };
    let checking = checking.clone();
    app.assistant.sign_in_read = None;
    let Some(cfg) = app.assistant.agent.as_ref().map(|a| a.config.clone()).filter(Config::login) else { return };
    let usable = read.filter(|c| c.signed_in() && c.plan_allowed());
    if let Some(binding) = checking {
        // Still signed in, or another account selected and signed in (requests for this one
        // then fail with "the account changed", and the user chooses): nothing to clear.
        let signed_in_somewhere = usable.is_some();
        if signed_in_somewhere || cfg.binding() != binding || cfg.key.is_empty() {
            return;
        }
        // The same as signing out here: no key, account or catalog, and a model list still on
        // its way is dropped.
        let mut next = cfg;
        forget_sign_in(app, &mut next);
        apply_config(app, next);
        app.assistant.settings = true;
        return;
    }
    let mut next = cfg.clone();
    match &usable {
        Some(c) => next.sign_in(c),
        None => next.key.clear(),
    }
    if next == cfg {
        return;
    }
    apply_config(app, next.clone());
    app.assistant.account = usable.map(|c| c.account());
    app.assistant.models.clear();
    app.assistant.catalog.clear();
    app.assistant.models_error = None;
    app.assistant.models_slot = None;
    if !next.key.is_empty() {
        list_models(app, &next, ctx);
    }
}

/// Whether the provider's key or sign-in is still being read (off the frame).
#[cfg(not(target_arch = "wasm32"))]
fn reading(app: &RustGisApp, provider: &str) -> bool {
    app.assistant.keys.loading(provider) || (provider::preset(provider).is_some_and(|p| p.login) && app.assistant.sign_in_read.is_some())
}

/// Persists ordinary settings; desktop secrets are queued for the keychain worker.
fn save_config(app: &mut RustGisApp, c: &Config, ctx: &egui::Context) {
    save_pref(app, provider::PROVIDER_PREF, &c.provider);
    save_pref(app, &format!("assistant_model_{}", c.provider), c.model.trim());
    save_pref(app, &provider::url_pref(&c.provider), c.base_url.trim());
    // A key from the environment stays there; a sign-in's tokens live in their own file.
    let from_env = Config::from_env(&c.provider).key;
    if c.login() {
        return;
    }
    let pref_key = format!("assistant_key_{}", c.provider);
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !app.assistant.keys.changed(&c.provider) {
            return;
        }
        let key = crate::assistantkeys::remembered(&c.key, app.assistant.remember_key, &from_env);
        let legacy = pref(app, &pref_key);
        // Forgetting plaintext is immediate, even if the OS refuses to delete its copy.
        if key.is_none() {
            save_pref(app, &pref_key, "");
        }
        app.assistant.keys.save(&c.provider, key, legacy, app.services.secrets.clone(), ctx);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = ctx;
        let keep = app.assistant.remember_key && c.key.trim() != from_env;
        save_pref(app, &pref_key, if keep { c.key.trim() } else { "" });
    }
}

/// Applies background replies without overwriting a later edit or another provider.
#[cfg(not(target_arch = "wasm32"))]
fn poll_keys(app: &mut RustGisApp, ctx: &egui::Context) {
    migrate_keys(app, ctx);
    let previous = app
        .assistant
        .agent
        .as_ref()
        .and_then(|a| app.assistant.keys.key(&a.config.provider).map(|key| (a.config.provider.clone(), key.to_string())));
    for (provider, migrated) in app.assistant.keys.poll() {
        let name = format!("assistant_key_{provider}");
        if pref(app, &name).as_deref() == Some(migrated.expose()) {
            save_pref(app, &name, "");
        }
    }
    if let Some(agent) = app.assistant.agent.as_mut()
        && !agent.config.login()
        && previous.as_ref().is_some_and(|(provider, key)| *provider == agent.config.provider && *key == agent.config.key)
        && let Some(key) = app.assistant.keys.key(&agent.config.provider)
    {
        agent.config.key = key.to_string();
    }
    if let Some(provider) = app.assistant.settings_pending.as_deref()
        && !reading(app, provider)
    {
        let cfg = app.assistant.agent.as_ref().filter(|a| a.config.provider == provider).map(|a| a.config.clone());
        app.assistant.settings_pending = None;
        if let Some(cfg) = cfg {
            init_settings(app, &cfg);
        }
    }
}

/// Sends a request off the frame. A signed-in account's requests renew its tokens as needed.
fn dispatch(app: &mut RustGisApp, req: HttpRequest, login: Option<chatgpt::Binding>, chunks: Option<Chunks>, ctx: &egui::Context) -> Reply {
    let slot: Reply = Slot::default();
    let fail = |slot: &Reply, e: String| {
        if let Ok(mut s) = slot.lock() {
            *s = Some(Err(e.into()));
        }
    };
    if login.is_some() && !chatgpt::api_url(&req.url) {
        fail(&slot, tl!("A ChatGPT sign-in can only be used with the OpenAI API.").into());
        return slot;
    }
    if !provider::allowed_url(&req.url) {
        fail(
            &slot,
            crate::i18n::fmt("{url}: only HTTPS addresses, or plain HTTP on this computer or network, are allowed", &[("url", &req.url)]),
        );
        return slot;
    }
    if let Some(http) = &app.services.http
        && login.is_none()
    {
        http(req, slot.clone());
        return slot;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (out, ctx) = (slot.clone(), ctx.clone());
        std::thread::spawn(move || {
            let repaint = ctx.clone();
            let mut on_chunk = |piece: &[u8]| {
                if let Some(Ok(mut buf)) = chunks.as_ref().map(|c| c.lock()) {
                    buf.extend_from_slice(piece);
                }
                repaint.request_repaint();
            };
            let r = match (&login, chunks.is_some()) {
                (Some(account), true) => chatgpt::send_streaming(&req, account, &mut on_chunk),
                (Some(account), false) => chatgpt::send(&req, account),
                (None, true) => rustgis_assistant::http::send_streaming(&req, &mut on_chunk),
                (None, false) => rustgis_assistant::http::send(&req),
            };
            if let Ok(mut s) = out.lock() {
                *s = Some(r);
            }
            ctx.request_repaint();
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (req, ctx, login, chunks);
        fail(&slot, "this build can't reach AI providers".into());
    }
    slot
}

/// Asks the provider for its models.
fn list_models(app: &mut RustGisApp, cfg: &Config, ctx: &egui::Context) {
    if key_loading(app) {
        app.assistant.catalog_pending = true;
        return;
    }
    app.assistant.catalog_pending = false;
    let req = provider::models_request(cfg);
    app.assistant.models_slot = Some(dispatch(app, req, cfg.login().then(|| cfg.binding()), None, ctx));
}

/// Replaces provider settings and stops work bound to a different provider or sign-in.
fn apply_config(app: &mut RustGisApp, cfg: Config) {
    if let Some(agent) = app.assistant.agent.as_mut() {
        if agent.config.provider != cfg.provider
            || (agent.config.login() && (agent.config.binding() != cfg.binding() || agent.config.key != cfg.key))
        {
            agent.stop();
            app.assistant.inflight = None;
            app.assistant.retry = None;
            app.assistant.chunks = None;
        }
        agent.config = cfg;
    }
}

/// A ChatGPT account signed in: the assistant uses its plan from now on.
#[cfg(not(target_arch = "wasm32"))]
fn signed_in(app: &mut RustGisApp, c: &chatgpt::Credentials, ctx: &egui::Context) {
    app.assistant.sign_in_read = None;
    app.assistant.account = Some(c.account());
    app.assistant.login_error = None;
    let saved = chatgpt::accounts().unwrap_or_default();
    // "Use another account" with an account already saved registers RustGIS for it again.
    app.assistant.duplicate = saved.iter().any(|a| a.subject == c.subject && a.client_id != c.client_id);
    app.assistant.accounts = Some(saved);
    // The first sign-in says that the plan is now used (once).
    app.assistant.welcome = c.plan_allowed() && pref(app, "assistant_chatgpt_welcomed").as_deref() != Some("on");
    let Some(mut cfg) = app.assistant.agent.as_ref().map(|a| a.config.clone()) else { return };
    if !cfg.login() {
        return;
    }
    cfg.sign_in(c);
    app.assistant.models.clear();
    app.assistant.catalog.clear();
    app.assistant.models_error = None;
    if c.plan_allowed() {
        list_models(app, &cfg, ctx);
    } else {
        app.assistant.models_slot = None;
        app.assistant.catalog_pending = false;
        app.assistant.login_error = Some(rustgis_assistant::Error::new(rustgis_assistant::Recovery::Consent, ""));
    }
    apply_config(app, cfg);
    app.assistant.unsaved = true;
}

/// After a failed request: when the sign-in on this computer no longer has tokens (they expired
/// or were revoked), the settings say so and offer to sign in again (read off the frame).
fn sync_sign_in(app: &mut RustGisApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(binding) = app.assistant.agent.as_ref().filter(|a| a.config.login() && !a.config.key.is_empty()).map(|a| a.config.binding())
    {
        read_sign_in(app, Some(binding), ctx);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (app, ctx);
}

fn take(slot: &Reply) -> Option<Result<HttpResponse, rustgis_assistant::Error>> {
    slot.lock().ok().and_then(|mut s| s.take())
}

/// Ends the agent threads on this computer (with every process they started), and the relay if
/// they started it: nothing keeps acting on the map after the conversation.
fn end_threads(app: &mut RustGisApp) {
    #[cfg(not(target_arch = "wasm32"))]
    crate::agentthreads::end_all(app);
    #[cfg(target_arch = "wasm32")]
    let _ = app;
}

/// Carries out what the agent asked for.
pub(crate) fn follow(app: &mut RustGisApp, step: Step, ctx: &egui::Context) {
    app.map.invalidate();
    if let Step::Retry(after) = step {
        let generation = app.assistant.agent.as_ref().map_or(0, Agent::generation);
        app.assistant.retry = Some((generation, web_time::Instant::now() + after));
        ctx.request_repaint_after(after);
        return;
    }
    if let Step::Send(req) = step {
        let generation = app.assistant.agent.as_ref().map_or(0, Agent::generation);
        let login = app.assistant.agent.as_ref().filter(|a| a.config.login()).map(|a| a.config.binding());
        let chunks = Chunks::default();
        let slot = dispatch(app, req, login, Some(chunks.clone()), ctx);
        app.assistant.inflight = Some((generation, slot));
        app.assistant.chunks = Some((generation, chunks));
    }
}

/// Runs `f` on the agent and the session; errors of the commands it ran show in the transcript,
/// not as notices too.
fn with_agent(app: &mut RustGisApp, f: impl FnOnce(&mut Agent, &mut rustgis_engine::Session) -> Step) -> Option<Step> {
    let mut agent = app.assistant.agent.take()?;
    let before = app.session.notices.len();
    let step = f(&mut agent, &mut app.session);
    let mut i = 0;
    app.session.notices.retain(|n| {
        i += 1;
        i <= before || !n.error
    });
    app.assistant.agent = Some(agent);
    Some(step)
}

/// Per-frame: responses that arrived, a message queued by a command, and settings to save.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    poll_keys(app, ctx);
    #[cfg(not(target_arch = "wasm32"))]
    poll_sign_in(app, ctx);
    if app.assistant.catalog_pending
        && let Some(cfg) = app.assistant.agent.as_ref().map(|a| a.config.clone())
    {
        list_models(app, &cfg, ctx);
    }
    if app.assistant.unsaved && !typing(ctx) {
        app.assistant.unsaved = false;
        save_pref(app, provider::REMEMBER_KEY_PREF, provider::remember_key_value(app.assistant.remember_key));
        if let Some(cfg) = app.assistant.agent.as_ref().map(|a| a.config.clone()) {
            save_config(app, &cfg, ctx);
        }
    }
    if let Some(text) = app.assistant.queued.take() {
        ensure_loaded(app, ctx);
        if app.assistant.agent.as_ref().is_some_and(Agent::busy) {
            app.session.notify(tl!("The assistant is busy; stop it or wait for it to finish."), true);
        } else {
            send(app, text, ctx);
        }
    }
    // The reply's text so far, as it streams in.
    if let Some((generation, chunks)) = &app.assistant.chunks {
        let generation = *generation;
        let piece = chunks.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
        if !piece.is_empty()
            && let Some(agent) = app.assistant.agent.as_mut()
        {
            agent.stream(generation, &piece);
        }
    }
    if let Some((generation, slot)) = &app.assistant.inflight
        && let Some(r) = take(slot)
    {
        let generation = *generation;
        app.assistant.inflight = None;
        app.assistant.chunks = None;
        let failed = r.is_err();
        if let Some(step) = with_agent(app, |a, s| a.receive_result(generation, r, s)) {
            follow(app, step, ctx);
        }
        if failed {
            sync_sign_in(app, ctx);
        }
    }
    // Agent threads on this computer: what they did, asked and reported.
    #[cfg(not(target_arch = "wasm32"))]
    crate::agentthreads::poll(app, ctx);
    if let Some((generation, at)) = app.assistant.retry
        && web_time::Instant::now() >= at
    {
        app.assistant.retry = None;
        if let Some(step) = with_agent(app, |a, _| a.retry(generation)) {
            follow(app, step, ctx);
        }
    }
    if let Some(slot) = &app.assistant.models_slot
        && let Some(r) = take(slot)
    {
        app.assistant.models_slot = None;
        let api = app.assistant.agent.as_ref().map_or(rustgis_assistant::Api::OpenAi, |a| a.config.api());
        let models = r.and_then(|r| {
            if api == rustgis_assistant::Api::Responses {
                let catalog = provider::parse_catalog(&r)?;
                if catalog.is_empty() {
                    return Err("No models are available for this account.".into());
                }
                if let Some(a) = app.assistant.agent.as_mut() {
                    provider::use_catalog(&mut a.config, &catalog, false);
                    app.assistant.unsaved = true;
                }
                let ids = catalog.iter().map(|m| m.id.clone()).collect();
                app.assistant.catalog = catalog;
                Ok(ids)
            } else {
                provider::parse_models(api, &r).map_err(Into::into)
            }
        });
        match models {
            Ok(m) => {
                // A signed-in account's catalog says which models it may use: the first is
                // the one to suggest when the chosen one isn't offered.
                if let Some(a) = app.assistant.agent.as_mut()
                    && a.config.login()
                    && let Some(first) = m.first()
                    && !m.contains(&a.config.model)
                {
                    a.config.model = first.clone();
                    app.assistant.unsaved = true;
                }
                app.assistant.models = m;
                app.assistant.models_error = None;
            }
            Err(e) => {
                app.assistant.models_error = Some(e);
                sync_sign_in(app, ctx);
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = app.assistant.login.as_ref().and_then(chatgpt::Login::poll) {
        app.assistant.login = None;
        match r {
            Ok(c) => signed_in(app, &c, ctx),
            Err(e) => app.assistant.login_error = Some(e),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = app.assistant.signout.as_ref().and_then(|s| s.lock().ok().and_then(|mut s| s.take())) {
        app.assistant.signout = None;
        if r.is_err() {
            app.assistant.login_error = Some(
                tl!("Signed out on this computer, but OpenAI didn't confirm it. You can disconnect RustGIS in ChatGPT's settings.").into(),
            );
        }
    }
}

/// Starts a user request with the selected provider's current session configuration.
fn send(app: &mut RustGisApp, text: String, ctx: &egui::Context) {
    if key_loading(app) {
        app.assistant.queued = Some(text);
        return;
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    app.assistant.history.retain(|h| *h != text);
    app.assistant.history.push(text.clone());
    app.assistant.history_pos = None;
    app.assistant.always = false;
    let session = &app.session;
    let Some(step) = app.assistant.agent.as_mut().map(|a| a.ask(&text, session)) else { return };
    app.assistant.input.clear();
    follow(app, step, ctx);
}

/// Only an initial keychain read delays requests; a known legacy key works during migration.
fn key_loading(app: &RustGisApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.assistant.agent.as_ref().is_some_and(|a| reading(app, &a.config.provider))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        false
    }
}

/// Draws the Assistant pane, loading its provider on first use.
pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    ensure_loaded(app, ui.ctx());
    sync_skills(app, false);
    let ctx = ui.ctx().clone();
    if app.assistant.catalog_pending
        && let Some(cfg) = app.assistant.agent.as_ref().map(|a| a.config.clone())
    {
        list_models(app, &cfg, &ctx);
    }
    let busy = app.assistant.agent.as_ref().is_some_and(Agent::busy);
    // The model and settings, at the top.
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let (label, model) = app
                .assistant
                .agent
                .as_ref()
                .map(|a| (provider::preset(&a.config.provider).map_or("", |p| p.label), a.config.model.clone()))
                .unwrap_or_default();
            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::hover());
            icons::paint(ui.painter(), r, "assistant", t.text_dim, t.accent);
            let model_label = egui::Label::new(RichText::new(format!("{} · {}", tl!(label), model)).small().color(t.text_dim))
                .truncate()
                .sense(egui::Sense::click());
            // Room for the two buttons; a long model name is cut short.
            let w = (ui.available_width() - 2.0 * (26.0 + ui.spacing().item_spacing.x)).max(0.0);
            if ui
                .allocate_ui(vec2(w, 16.0), |ui| ui.add(model_label))
                .inner
                .on_hover_text(tl!("The model answering; click to change it"))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                toggle_settings(app);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::widgets::icon_button(ui, "settings", tl!("Assistant settings")).clicked() {
                    toggle_settings(app);
                }
                let has_chat = app.assistant.agent.as_ref().is_some_and(|a| !a.transcript.is_empty());
                if has_chat && crate::widgets::icon_button(ui, "delete", tl!("New conversation")).clicked() {
                    if let Some(a) = app.assistant.agent.as_mut() {
                        a.clear();
                    }
                    app.assistant.inflight = None;
                    end_threads(app);
                    app.assistant.chunks = None;
                    app.assistant.retry = None;
                }
                let idle = app.assistant.agent.as_ref().is_some_and(|a| !a.busy());
                if has_chat
                    && idle
                    && ui
                        .small_button(tl!("Compact"))
                        .on_hover_text(tl!("Summarise the conversation so far, so a long chat stays within the model's context"))
                        .clicked()
                    && let Some(step) = with_agent(app, |a, _| a.compact())
                {
                    follow(app, step, &ctx);
                }
            });
        });
    });
    // The composer first, so tall settings scroll above it instead of running under it.
    composer_panel(app, ui, t, busy, &ctx);
    // Closed settings look for the agents again when they next open.
    #[cfg(not(target_arch = "wasm32"))]
    if !app.assistant.settings {
        app.assistant.agents_found = None;
    }
    if app.assistant.settings {
        egui::ScrollArea::vertical()
            .id_salt("assistant_settings_scroll")
            .auto_shrink([false, true])
            .min_scrolled_height(0.0)
            .max_height((ui.available_height() - 8.0).max(0.0))
            .show(ui, |ui| {
                egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 4)).show(ui, |ui| settings(app, ui, t));
            });
        ui.separator();
    }
    // The conversation in between.
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).min_scrolled_height(0.0).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            // Text in a row (a list item, a quote, the user's message) wraps too, instead of widening the pane.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            transcript(app, ui, t, &ctx);
        });
    });
}

fn composer_panel(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, busy: bool, ctx: &egui::Context) {
    egui::Panel::bottom("assistant_composer")
        .frame(egui::Frame::NONE.fill(t.pane).inner_margin(egui::Margin::symmetric(10, 8)))
        .show(ui, |ui| composer(app, ui, t, busy, ctx));
}

/// Edits provider settings and invalidates pending key reads as soon as a key changes.
fn settings(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    #[cfg(not(target_arch = "wasm32"))]
    poll_sign_in(app, ui.ctx());
    let Some(mut cfg) = app.assistant.agent.as_ref().map(|a| a.config.clone()) else { return };
    let before = (cfg.clone(), app.assistant.remember_key);
    let open = !app.assistant.provider_closed;
    let section = egui::CollapsingHeader::new(RichText::new(tl!("AI provider")).font(semibold(12.5)))
        .id_salt("assistant_provider_section")
        .open(Some(open))
        .show(ui, |ui| provider_settings(app, ui, t, &mut cfg));
    if section.header_response.clicked() {
        app.assistant.provider_closed = open;
        #[cfg(not(target_arch = "wasm32"))]
        {
            app.assistant.settings_pending = None;
        }
        save_pref(app, "assistant_provider_closed", if open { "on" } else { "" });
    }
    if !key_loading(app)
        && let Some(why) = cfg.missing()
    {
        ui.label(RichText::new(tl!(why)).small().color(t.amber));
    }
    #[cfg(not(target_arch = "wasm32"))]
    local_agents_setting(app, ui, t);
    #[cfg(not(target_arch = "wasm32"))]
    skills_setting(app, ui, t);
    // Agents on this computer working on this map through the MCP relay (desktop).
    #[cfg(not(target_arch = "wasm32"))]
    crate::connectui::section(app, ui, t);
    if cfg.login() {
        provider::use_catalog(&mut cfg, &app.assistant.catalog, true);
    }
    if (cfg.clone(), app.assistant.remember_key) != before {
        if app.assistant.remember_key != before.1 {
            save_pref(app, provider::REMEMBER_KEY_PREF, provider::remember_key_value(app.assistant.remember_key));
        }
        #[cfg(not(target_arch = "wasm32"))]
        if !cfg.login() {
            let key_changed = cfg.provider == before.0.provider && cfg.key != before.0.key;
            let enabling = app.assistant.remember_key && !before.1;
            let legacy = pref(app, &format!("assistant_key_{}", cfg.provider));
            let reading =
                enabling && !key_changed && app.assistant.keys.enable(&cfg.provider, legacy, app.services.secrets.clone(), ui.ctx());
            if key_changed || (app.assistant.remember_key != before.1 && !reading) {
                app.assistant.keys.edit(&cfg.provider, &cfg.key);
                // Forget immediately on an explicit clear/opt-out, even while the key field has focus.
                let opted_out = !app.assistant.remember_key && before.1;
                if opted_out || (key_changed && cfg.key.trim().is_empty()) {
                    save_config(app, &cfg, ui.ctx());
                }
            }
        }
        app.assistant.unsaved = true;
        apply_config(app, cfg);
    }
}

/// The settings' AI provider section: the provider, account, model, address and key, with
/// notes on where they're kept and what is sent.
fn provider_settings(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, cfg: &mut Config) {
    egui::Grid::new("assistant_settings").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
        let provider_label = ui.label(tl!("Provider"));
        let current = provider::preset(&cfg.provider).map_or("", |p| p.label);
        egui::ComboBox::from_id_salt("assistant_provider")
            .selected_text(tl!(current))
            .width(180.0)
            .show_ui(ui, |ui| {
                for p in PRESETS {
                    // Signing in needs the browser and a server on this computer: the desktop app.
                    if p.login && cfg!(target_arch = "wasm32") {
                        continue;
                    }
                    if ui.selectable_label(cfg.provider == p.id, tl!(p.label)).clicked() && cfg.provider != p.id {
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            app.assistant.login = None;
                        }
                        // Switching providers is also a focus change: save the previous key first.
                        if app.assistant.unsaved {
                            save_config(app, cfg, ui.ctx());
                        }
                        *cfg = load_config(app, p.id, ui.ctx());
                        app.assistant.models_slot = None;
                        app.assistant.models.clear();
                        app.assistant.catalog.clear();
                        app.assistant.models_error = None;
                        app.assistant.login_error = None;
                        #[cfg(not(target_arch = "wasm32"))]
                        if cfg.login() {
                            read_sign_in(app, None, ui.ctx());
                        }
                    }
                }
            })
            .response
            .labelled_by(provider_label.id);
        ui.end_row();
        if cfg.login() {
            ui.label(tl!("Account"));
            ui.vertical(|ui| account_row(app, ui, t, cfg));
            ui.end_row();
        }
        let model_label = ui.label(tl!("Model"));
        let signed_out = cfg.login() && cfg.key.is_empty();
        ui.add_enabled_ui(!signed_out, |ui| {
            ui.horizontal(|ui| {
                if signed_out {
                    ui.label(tl!("Sign in to choose a model."));
                    return;
                }
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.model)
                        .id(egui::Id::unique(FIELDS[0]))
                        .desired_width(150.0)
                        .hint_text(tl!("model id")),
                )
                .labelled_by(model_label.id);
                if !app.assistant.models.is_empty() {
                    egui::ComboBox::from_id_salt("assistant_models")
                        .selected_text("")
                        .width(24.0)
                        .height(320.0)
                        .show_ui(ui, |ui| {
                            for m in &app.assistant.models {
                                if ui
                                    .selectable_label(
                                        cfg.model == *m,
                                        app.assistant.catalog.iter().find(|c| c.id == *m).map_or(m.as_str(), |c| c.name.as_str()),
                                    )
                                    .clicked()
                                {
                                    cfg.model = m.clone();
                                }
                            }
                        })
                        .response
                        .on_hover_text(tl!("Model"));
                }
                let loading = app.assistant.models_slot.is_some();
                if ui
                    .add_enabled(!loading, egui::Button::new(if loading { "…" } else { "↻" }))
                    .on_hover_text(tl!("List the provider's models"))
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    list_models(app, cfg, &ctx);
                }
            });
        });
        ui.end_row();
        let preset = provider::preset(&cfg.provider);
        if !cfg.login() {
            let address_label = ui.label(tl!("Address"));
            ui.add(egui::TextEdit::singleline(&mut cfg.base_url).id(egui::Id::unique(FIELDS[1])).desired_width(200.0))
                .labelled_by(address_label.id);
            ui.end_row();
            let key_label = ui.label(tl!("API key"));
            ui.horizontal(|ui| {
                let hint = if preset.is_some_and(|p| p.keyless) { tl!("not needed") } else { tl!("paste your key") };
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.key)
                        .id(egui::Id::unique(FIELDS[2]))
                        .password(!app.assistant.show_key)
                        .desired_width(150.0)
                        .hint_text(hint),
                )
                .labelled_by(key_label.id);
                let eye = if app.assistant.show_key { tl!("Hide") } else { tl!("Show") };
                if ui.small_button(eye).clicked() {
                    app.assistant.show_key = !app.assistant.show_key;
                }
            });
        }
        ui.end_row();
    });
    let preset = provider::preset(&cfg.provider);
    let mut notes = Vec::new();
    if cfg.login() {
        notes.push(sign_in_storage_note(app.assistant.account.as_ref().filter(|_| !cfg.key.is_empty())));
        notes.push(tl!("Requests go to the OpenAI API and use your ChatGPT plan.").to_string());
    } else {
        ui.checkbox(&mut app.assistant.remember_key, tl!("Remember the key on this computer"));
        if let Some(p) = preset.filter(|p| !p.key_env.is_empty()) {
            notes.push(crate::i18n::fmt("On the desktop, the key can also come from {vars}.", &[("vars", &p.key_env.join(", "))]));
        }
        notes.push(key_storage_note(app, &cfg.provider));
    }
    notes.push(
        tl!("Requests send your message and the map's layer names, fields and the rows the assistant reads to the provider.").to_string(),
    );
    for n in notes {
        ui.label(RichText::new(n).small().color(t.text_dim));
    }
    if let Some(p) = preset.filter(|p| !p.key_url.is_empty() && !p.login) {
        ui.hyperlink_to(RichText::new(if p.keyless { tl!("Get it") } else { tl!("Get a key") }).small(), p.key_url);
    }
    if let Some(e) = &app.assistant.models_error {
        show_error(ui, e, t);
    }
    if cfg.login() && !cfg.key.is_empty() {
        plan_notice(app, ui, t);
    }
    if let Some(e) = &app.assistant.login_error {
        show_error(ui, e, t);
    }
}

/// Where the ChatGPT sign-in is kept, as truthfully as is known.
fn sign_in_storage_note(account: Option<&chatgpt::Account>) -> String {
    match account.map(|a| a.kept) {
        Some(chatgpt::Kept::SettingsFile) => tl!(
            "The sign-in is still kept in the app's settings folder, readable by your user account: the system keychain is unavailable, so it hasn't been moved there yet."
        ),
        Some(chatgpt::Kept::Session) => tl!(
            "The system keychain is unavailable, so this sign-in won't be remembered: it lasts until RustGIS closes."
        ),
        _ => tl!("The sign-in is kept in the system keychain, never in the app's settings or a project file."),
    }
    .to_string()
}

/// Describes actual storage, including a legacy key retained after a failed migration.
fn key_storage_note(app: &RustGisApp, provider: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = provider;
        if app.assistant.remember_key {
            tl!("Saved keys are kept unencrypted in the browser's storage.").to_string()
        } else {
            tl!("The key is kept for this session only.").to_string()
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::assistantkeys::Status;
        use rustgis_engine::data::secrets::SecretError;
        let mut notes = Vec::new();
        let plaintext = pref(app, &format!("assistant_key_{provider}")).is_some();
        match app.assistant.keys.status(provider) {
            Some(Status::Stored) if app.assistant.remember_key => notes.push(tl!("The key is saved in the system keychain.")),
            Some(Status::Pending { .. }) => notes.push(tl!("Accessing the system keychain…")),
            Some(Status::Failed { error, legacy, deleting, reading }) => {
                notes.push(match error {
                    SecretError::Denied => tl!("The system keychain is locked or access was refused."),
                    SecretError::Unavailable(_) => tl!("The system keychain is unavailable."),
                    _ => tl!("The system keychain could not complete the request."),
                });
                if *reading {
                    notes.push(tl!("A saved key could not be read. A key entered now will be kept for this session only."));
                } else if *deleting {
                    notes.push(tl!("RustGIS could not confirm that any previously saved key was deleted from the system keychain."));
                } else if !legacy || !plaintext {
                    notes.push(tl!("The key is kept for this session only; it has not been remembered."));
                }
            }
            _ => notes.push(tl!("The key is kept for this session only.")),
        }
        if plaintext {
            notes.push(tl!(
                "A previously saved key is still kept unencrypted in the app's preferences until it can be moved to the system keychain."
            ));
        }
        notes.join("\n")
    }
}

/// The settings' account row of a sign-in provider: who is signed in and Sign out, or
/// Continue with ChatGPT (the browser).
#[cfg(not(target_arch = "wasm32"))]
fn account_row(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, cfg: &mut Config) {
    if let Some(login) = &app.assistant.login {
        let (reopen, cancel) = waiting_for_browser(ui, t, &login.url);
        if reopen {
            app.ui.open_url = Some(login.browser_url.clone());
        }
        if cancel {
            app.assistant.login = None;
        }
        return;
    }
    let saved = app.assistant.accounts.get_or_insert_with(|| chatgpt::accounts().unwrap_or_default()).clone();
    let active = saved.iter().find(|c| c.client_id == cfg.client_id && c.subject == cfg.account);
    let label = active.map(chatgpt::Credentials::label).unwrap_or_else(|| tl!("Choose an account").into());
    let picker = egui::ComboBox::from_id_salt("assistant_account").selected_text(label).width(180.0).truncate().show_ui(ui, |ui| {
        for account in &saved {
            let selected = active.as_ref().is_some_and(|c| c.client_id == account.client_id && c.subject == account.subject);
            // Selecting the current row also recovers a window with an outdated sign-in.
            if ui.selectable_label(selected, account.label()).clicked() {
                app.assistant.accounts = None;
                app.assistant.duplicate = false;
                // A read begun before this choice must not undo it.
                app.assistant.sign_in_read = None;
                match chatgpt::select(&account.client_id, &account.subject) {
                    Ok(c) => {
                        cfg.sign_in(&c);
                        if !c.plan_allowed() {
                            cfg.key.clear();
                        }
                        app.assistant.account = Some(c.account());
                        app.assistant.models.clear();
                        app.assistant.catalog.clear();
                        app.assistant.models_slot = None;
                        app.assistant.models_error = None;
                        if !cfg.key.is_empty() {
                            list_models(app, cfg, ui.ctx());
                        }
                    }
                    Err(e) => app.assistant.login_error = Some(e.auth_context()),
                }
            }
        }
    });
    picker.response.clone().on_hover_text(tl!("Choose an account"));
    // Opening the list reads it again: another window or the command line may have added one.
    if picker.response.clicked() {
        app.assistant.accounts = None;
    }
    let another = ui.small_button(tl!("Use another account")).clicked();
    if another {
        start_login(app, cfg, true);
        return;
    }
    let selected = active.map(|c| (c.client_id.clone(), c.subject.clone()));
    if let Some((client_id, subject)) = selected
        && ui
            .small_button(tl!("Remove account"))
            .on_hover_text(tl!("Signs this account out and forgets it on this computer, with its saved sign-in."))
            .clicked()
    {
        match chatgpt::remove(&client_id, &subject) {
            Ok(revocation) => {
                revoke_later(app, revocation, ui.ctx());
                forget_sign_in(app, cfg);
                cfg.client_id.clear();
                cfg.account.clear();
                cfg.session.clear();
            }
            Err(e) => app.assistant.login_error = Some(e.auth_context()),
        }
        return;
    }
    if !cfg.key.is_empty() {
        let who = app.assistant.account.as_ref().map(|a| a.email.clone()).filter(|e| !e.is_empty());
        ui.label(RichText::new(who.unwrap_or_else(|| tl!("signed in").to_string())).small());
        if ui.small_button(tl!("Sign out")).clicked() {
            match chatgpt::sign_out_selected(&cfg.binding()) {
                Ok(out) => {
                    revoke_later(app, out.revocation, ui.ctx());
                    forget_sign_in(app, cfg);
                    if out.unrevoked.is_some() {
                        app.assistant.login_error = Some(
                            tl!("Signed out on this computer, but the system keychain couldn't be read, so OpenAI wasn't asked to end the session. You can disconnect RustGIS in ChatGPT's settings.")
                                .into(),
                        );
                    }
                }
                Err(e) => {
                    app.assistant.login_error = Some(e.auth_context());
                    return;
                }
            }
        }
        if app.assistant.duplicate {
            ui.label(
                RichText::new(tl!("This ChatGPT account was already saved. Next time, choose it in the list instead of adding it again."))
                    .small()
                    .color(t.text_dim),
            );
        }
        return;
    }
    if ui.button(tl!("Continue with ChatGPT")).clicked() {
        start_login(app, cfg, false);
    }
}

/// Revokes a signed-out session at OpenAI: network I/O, off the frame.
#[cfg(not(target_arch = "wasm32"))]
fn revoke_later(app: &mut RustGisApp, revocation: Option<chatgpt::Revocation>, ctx: &egui::Context) {
    let Some(r) = revocation else { return };
    let slot: Slot<Result<(), rustgis_assistant::Error>> = Slot::default();
    let (out, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || {
        let result = chatgpt::revoke(&r);
        if let Ok(mut s) = out.lock() {
            *s = Some(result);
        }
        ctx.request_repaint();
    });
    app.assistant.signout = Some(slot);
}

/// After signing out or removing the account: no tokens, no catalog, and nothing waiting to be
/// sent with them (a queued message goes back to the input box, not to the provider).
#[cfg(not(target_arch = "wasm32"))]
fn forget_sign_in(app: &mut RustGisApp, cfg: &mut Config) {
    app.assistant.sign_in_read = None;
    app.assistant.models_slot = None;
    app.assistant.catalog_pending = false;
    if let Some(text) = app.assistant.queued.take() {
        app.assistant.input = text;
    }
    cfg.key.clear();
    app.assistant.account = None;
    app.assistant.models.clear();
    app.assistant.catalog.clear();
    app.assistant.welcome = false;
    app.assistant.accounts = None;
}

#[cfg(not(target_arch = "wasm32"))]
fn waiting_for_browser(ui: &mut Ui, t: &Tokens, url: &str) -> (bool, bool) {
    ui.spinner();
    ui.label(RichText::new(tl!("Waiting for the browser…")).small().color(t.text_dim));
    let reopen = ui.small_button(tl!("Open browser again")).clicked();
    if ui.small_button(tl!("Copy sign-in link")).clicked() {
        ui.ctx().copy_text(url.to_string());
    }
    (reopen, ui.small_button(tl!("Cancel")).clicked())
}

#[cfg(not(target_arch = "wasm32"))]
fn start_login(app: &mut RustGisApp, cfg: &mut Config, another: bool) {
    app.assistant.sign_in_read = None;
    if !another && !cfg.client_id.is_empty() {
        match chatgpt::select(&cfg.client_id, &cfg.account) {
            Ok(c) => cfg.sign_in(&c),
            Err(e) => {
                app.assistant.login_error = Some(e.auth_context());
                return;
            }
        }
    }
    match chatgpt::login(another) {
        Ok(login) => {
            app.ui.open_url = Some(login.browser_url.clone());
            app.assistant.login = Some(login);
            app.assistant.login_error = None;
        }
        Err(e) => app.assistant.login_error = Some(e),
    }
}

/// Whether the assistant may hand tasks to the agents on this computer (Claude Code, Codex,
/// Gemini CLI…) through ACP. Off unless the user turns it on; each agent's actions then wait for
/// the user's approval, unless they choose otherwise.
#[cfg(not(target_arch = "wasm32"))]
fn local_agents_setting(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if let Some(a) = app.assistant.agent.as_mut() {
        a.set_agents_auto(app.assistant.agents_auto);
    }
    if ui.checkbox(&mut app.assistant.local_agents, tl!("Let the assistant hand tasks to agents on this computer")).changed() {
        app.assistant.agents_found = None;
        let (found, _) = app.assistant.agents_found.get_or_insert_with(rustgis_assistant::acp::installed);
        let found: Vec<_> = found.iter().map(|(spec, _)| *spec).collect();
        save_pref(app, "assistant_local_agents", if app.assistant.local_agents { "on" } else { "off" });
        let agents = if app.assistant.local_agents { found } else { Vec::new() };
        if let Some(a) = app.assistant.agent.as_mut() {
            a.set_harnesses(agents);
        }
    }
    let (found, need_node) = app.assistant.agents_found.get_or_insert_with(rustgis_assistant::acp::installed).clone();
    let note = if found.is_empty() {
        tl!("No agents that speak ACP (Claude Code, Codex, Gemini CLI, GitHub Copilot CLI, Cursor, OpenCode, goose) were found on this computer.").to_string()
    } else {
        let names: Vec<&str> = found.iter().map(|(spec, _)| spec.label).collect();
        crate::i18n::fmt(
            "Found: {names}. An agent works on this map through RustGIS's tools and with its own (web, scripts, files) in a folder of its own, kept in Documents › RustGIS › Agent work. It asks you before each action.",
            &[("names", &names.join(", "))],
        )
    };
    ui.label(RichText::new(note).small().color(t.text_dim));
    if found.iter().any(|(spec, command)| {
        spec.adapter.is_some() && command.first().is_some_and(|c| std::path::Path::new(c).file_stem().is_some_and(|s| s == "npx"))
    }) {
        ui.label(
            RichText::new(tl!(
                "Claude Code and Codex connect through an ACP adapter. On first use, npx downloads and runs its pinned version from npm."
            ))
            .small()
            .color(t.text_dim),
        );
    }
    if !need_node.is_empty() {
        let names: Vec<&str> = need_node.iter().map(|spec| spec.label).collect();
        ui.label(
            RichText::new(crate::i18n::fmt("{names}: needs Node.js (npx) for its ACP adapter.", &[("names", &names.join(", "))]))
                .small()
                .color(t.amber),
        );
    }
    if app.assistant.local_agents && ui.checkbox(&mut app.assistant.agents_auto, tl!("Approve agents' actions without asking")).changed() {
        // Publish the change before returning from the widget: a held ACP reply can arrive
        // before the next frame polls threads.
        app.assistant.threads.set_auto(app.assistant.agents_auto);
        save_pref(app, "assistant_agents_auto", if app.assistant.agents_auto { "on" } else { "off" });
        if let Some(a) = app.assistant.agent.as_mut() {
            a.set_agents_auto(app.assistant.agents_auto);
        }
    }
    if app.assistant.local_agents && app.assistant.agents_auto {
        ui.label(
            RichText::new(tl!("Agents then run commands, change files and use the web on this computer without asking you."))
                .small()
                .color(t.red),
        );
    }
}

/// The Skills list: every skill and where it comes from, a switch for each, and adding and
/// removing the user's (desktop).
#[cfg(not(target_arch = "wasm32"))]
fn skills_setting(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    use rustgis_assistant::skills::{self, Places, Source};
    ui.collapsing(tl!("Skills"), |ui| {
        // Read once and kept; each frame copies only what the rows show (not the skills' bodies),
        // and the folders are looked up only when something is done.
        let (listed, problems) = app.assistant.skills_list.get_or_insert_with(|| skills::list(&Places::here()));
        let rows: Vec<(String, String, Source, bool)> =
            listed.iter().map(|l| (l.skill.name.clone(), l.skill.description.clone(), l.skill.source.clone(), l.on)).collect();
        let problems = problems.clone();
        let mut done: Option<Result<String, String>> = None;
        for (name, description, source, on) in &rows {
            ui.push_id((name, format!("{source:?}")), |ui| { ui.horizontal(|ui| {
                let mut on = *on;
                if ui.checkbox(&mut on, name).on_hover_text(description).changed() {
                    done = Some(skills::set_source_enabled(&Places::here(), name, source, on).map(|()| String::new()));
                }
                let from = match source {
                    Source::BuiltIn => tl!("Built in"),
                    Source::Plugin(_) => tl!("Plugin"),
                    Source::User(_) => tl!("Yours"),
                    Source::Shared(_) => tl!("Shared"),
                };
                ui.label(RichText::new(from).small().color(t.text_dim));
                if matches!(source, Source::User(_)) && ui.small_button(tl!("Remove")).clicked() {
                    done = Some(skills::remove(&Places::here(), name).map(|()| String::new()));
                }
            }); });
        }
        for p in &problems {
            ui.label(RichText::new(p).small().color(t.red));
        }
        // Wrapped, so the buttons never widen the settings past the pane.
        ui.horizontal_wrapped(|ui| {
            let mut picked = None;
            if ui.button(tl!("Add skill…")).clicked() {
                picked = rfd::FileDialog::new()
                    .set_title(tl!("Add skill…"))
                    .add_filter(tl!("Skill (SKILL.md or .zip)"), &["md", "zip"])
                    .pick_file();
            }
            if ui.button(tl!("Add folder…")).clicked() {
                picked = rfd::FileDialog::new().set_title(tl!("Add folder…")).pick_folder();
            }
            if let Some(path) = picked {
                done = Some(
                    skills::add(&Places::here(), &path).map(|names| crate::i18n::fmt("Added: {names}", &[("names", &names.join(", "))])),
                );
            }
            if ui.button(tl!("Open folder")).clicked()
                && let Some(dir) = Places::here().user
            {
                let opened = std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display())).and_then(|()| crate::open_with_system(&dir));
                if let Err(e) = opened {
                    done = Some(Err(e));
                }
            }
            // Skills added to the folder by hand.
            if ui.button(tl!("Refresh")).clicked() {
                done = Some(Ok(String::new()));
            }
        });
        ui.label(
            RichText::new(tl!(
                "Your skills are kept in the skills folder of the app's settings folder. Shared skills (~/.agents/skills) are off until you turn them on."
            ))
            .small()
            .color(t.text_dim),
        );
        if let Some(r) = done {
            app.assistant.skills_message = match r {
                Ok(m) if m.is_empty() => None,
                Ok(m) => Some((true, m)),
                Err(e) => Some((false, e)),
            };
            sync_skills(app, true);
        }
        if let Some((ok, m)) = &app.assistant.skills_message {
            ui.label(RichText::new(m).small().color(if *ok { t.text_dim } else { t.red }));
        }
    });
}

/// Recovery text is translated; server diagnostics retain their original wording.
fn show_error(ui: &mut Ui, error: &rustgis_assistant::Error, t: &Tokens) {
    ui.label(RichText::new(tl!(error.message())).small().color(t.red));
    if error.recovery == rustgis_assistant::Recovery::Usage
        && crate::widgets::primary_button(ui, tl!("Weekly usage"), true, vec2(160.0, 26.0)).clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(chatgpt::USAGE_URL));
    }
    if error.status.is_some() || error.recovery != rustgis_assistant::Recovery::Other {
        ui.collapsing(tl!("Diagnostics"), |ui| {
            ui.label(&error.detail);
            if let Some(status) = error.status {
                ui.monospace(format!("HTTP {status}"));
            }
            for diagnostic in [&error.code, &error.param, &error.request_id].into_iter().flatten() {
                ui.monospace(diagnostic);
            }
        });
    }
}

/// While a ChatGPT plan is in use: saying so, with a link to its usage settings, and after the
/// first sign-in, a confirmation to dismiss.
fn plan_notice(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if app.assistant.welcome {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new(tl!("You're using your ChatGPT plan")).font(semibold(12.5)));
            ui.label(
                RichText::new(tl!("Eligible requests in RustGIS use your ChatGPT plan. You can manage usage in ChatGPT settings."))
                    .small()
                    .color(t.text_dim),
            );
            if ui.button(tl!("Got it")).clicked() {
                app.assistant.welcome = false;
                save_pref(app, "assistant_chatgpt_welcomed", "on");
            }
        });
    }
    plan_line(ui, t);
}

/// "Using ChatGPT plan · Weekly usage", near the composer and in the settings.
fn plan_line(ui: &mut Ui, t: &Tokens) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Using ChatGPT plan")).small().color(t.text_dim));
        ui.hyperlink_to(RichText::new(tl!("Weekly usage")).small(), chatgpt::USAGE_URL);
    });
}

#[cfg(target_arch = "wasm32")]
fn account_row(_app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, _cfg: &mut Config) {
    ui.label(RichText::new(tl!("Signing in with ChatGPT works in the desktop app.")).small().color(t.text_dim));
}

/// The input box's least height (two lines), and how tall it grows with text before it scrolls.
const INPUT_HEIGHT: f32 = 44.0;
const INPUT_GROWS: f32 = 160.0;

fn composer(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, busy: bool, ctx: &egui::Context) {
    // Agent threads on this computer, working or waiting for a follow-up.
    #[cfg(not(target_arch = "wasm32"))]
    crate::agentthreads::list(app, ui, t);
    // An action waiting for approval comes first.
    let pending = app
        .assistant
        .agent
        .as_ref()
        .and_then(|a| a.pending(&app.session).map(|(what, why, always)| (what, why, always, a.pending_review().cloned())));
    if let Some((what, why, can_always, review)) = pending {
        egui::Frame::NONE.fill(t.amber.gamma_multiply(0.15)).corner_radius(6).inner_margin(8).stroke(egui::Stroke::new(1.0, t.amber)).show(
            ui,
            |ui| {
                ui.label(RichText::new(tl!("Allow this action?")).font(semibold(12.5)));
                egui::ScrollArea::vertical().max_height(280.0).id_salt(("assistant_approval", &what, &why)).show(ui, |ui| {
                    if let Some(review) = &review {
                        crate::approvalcard::show(ui, &app.session.registry, review);
                    } else {
                        // Legacy requests have no structured review: keep all their context visible.
                        ui.label(
                            RichText::new(crate::i18n::fmt("The assistant wants to run this, and {why}:", &[("why", tl_raw!(&why))]))
                                .small(),
                        );
                        ui.add(egui::Label::new(RichText::new(&what).monospace().small()).wrap());
                    }
                });
                // Plugin commands, scripts and replacing the project are asked about every time.
                if can_always {
                    ui.checkbox(&mut app.assistant.always, tl!("Allow such actions for this conversation"));
                } else {
                    app.assistant.always = false;
                }
                ui.horizontal(|ui| {
                    let allow = crate::widgets::primary_button(ui, tl!("Allow"), true, vec2(70.0, 26.0)).clicked();
                    let deny = ui.button(tl!("Decline")).clicked();
                    if allow || deny {
                        let always = app.assistant.always;
                        if let Some(step) = with_agent(app, |a, s| a.decide(allow, always, s)) {
                            follow(app, step, ctx);
                        }
                    }
                });
            },
        );
        ui.add_space(6.0);
    }
    if app.assistant.agent.as_ref().is_some_and(|a| a.config.login() && !a.config.key.is_empty()) {
        plan_line(ui, t);
    }
    let id = egui::Id::unique("assistant_input");
    // Enter sends; Shift+Enter starts a new line.
    let focused = ui.memory(|m| m.has_focus(id));
    let mut submit = false;
    if focused && !busy {
        submit = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        // Up and Down step through earlier messages while the box holds one of them (or nothing).
        let shown = app.assistant.history_pos.and_then(|p| app.assistant.history.get(p)).is_some_and(|h| *h == app.assistant.input);
        if app.assistant.input.is_empty() || shown {
            let n = app.assistant.history.len();
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) && n > 0 {
                let p = app.assistant.history_pos.map_or(n - 1, |p| p.saturating_sub(1));
                app.assistant.history_pos = Some(p);
                app.assistant.input = app.assistant.history.get(p).cloned().unwrap_or_default();
            } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
                && let Some(p) = app.assistant.history_pos
            {
                app.assistant.history_pos = (p + 1 < n).then_some(p + 1);
                app.assistant.input = app.assistant.history_pos.and_then(|p| app.assistant.history.get(p)).cloned().unwrap_or_default();
            }
        }
    }
    // A grip above the box: dragging it up makes the box taller, up to half the window.
    let most = (ui.ctx().content_rect().height() * 0.5).max(INPUT_HEIGHT);
    let (grip, drag) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), egui::Sense::drag());
    let drag = drag.on_hover_cursor(egui::CursorIcon::ResizeVertical).on_hover_text(tl!("Drag to resize the message box"));
    if drag.dragged() {
        app.assistant.input_height = (app.assistant.input_height.max(INPUT_HEIGHT) - drag.drag_delta().y).clamp(INPUT_HEIGHT, most);
    }
    let color = if drag.hovered() || drag.dragged() { t.text } else { t.text_dim.gamma_multiply(0.6) };
    ui.painter().line_segment([grip.center() - vec2(14.0, 0.0), grip.center() + vec2(14.0, 0.0)], egui::Stroke::new(2.0, color));
    let height = app.assistant.input_height.clamp(INPUT_HEIGHT, most);
    // The box grows with what is typed, past the height set, then scrolls.
    egui::ScrollArea::vertical().id_salt("assistant_input_scroll").max_height(height.max(INPUT_GROWS).min(most)).show(ui, |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut app.assistant.input)
                .id(id)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .min_size(vec2(0.0, height))
                .font(regular(13.0))
                .hint_text(tl!("Ask about the map, or tell it what to do…")),
        );
    });
    ui.horizontal(|ui| {
        // Room for the button; the hint gives way in long languages.
        let w = (ui.available_width() - 84.0).max(0.0);
        ui.allocate_ui(vec2(w, 20.0), |ui| {
            ui.add(egui::Label::new(RichText::new(tl!("Enter to send, Shift+Enter for a new line")).small().color(t.text_dim)).truncate());
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if busy {
                if ui.button(tl!("Stop")).clicked() {
                    if let Some(a) = app.assistant.agent.as_mut() {
                        a.stop();
                    }
                    app.assistant.inflight = None;
                    app.assistant.retry = None;
                    app.assistant.chunks = None;
                }
            } else {
                let can = !app.assistant.input.trim().is_empty();
                if crate::widgets::primary_button(ui, tl!("Send"), can, vec2(64.0, 26.0)).clicked() {
                    submit = true;
                }
            }
        });
    });
    if submit && !busy {
        let text = app.assistant.input.clone();
        send(app, text, ctx);
        ui.memory_mut(|m| m.request_focus(id));
    }
}

fn transcript(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, ctx: &egui::Context) {
    let Some(agent) = app.assistant.agent.as_ref() else { return };
    if agent.transcript.is_empty() {
        ui.label(RichText::new(tl!("Ask a question about your data, or describe what to do with the map. The assistant runs RustGIS commands for you; every change can be undone.")).color(t.text_dim));
        ui.add_space(8.0);
        let mut chosen = None;
        for s in SUGGESTIONS {
            let b = egui::Button::new(RichText::new(tl!(s)).small()).corner_radius(12);
            if ui.add(b).clicked() {
                chosen = Some(tl!(s).to_string());
            }
        }
        if let Some(text) = chosen {
            send(app, text, ctx);
        }
        return;
    }
    for e in &agent.transcript {
        match e {
            Entry::User(text) => {
                ui.add_space(6.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    egui::Frame::NONE.fill(t.selection).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.set_max_width(ui.available_width() * 0.85);
                        ui.add(egui::Label::new(RichText::new(text).color(t.text)).halign(Align::LEFT));
                    });
                });
                ui.add_space(4.0);
            }
            Entry::Assistant(text) => {
                markdown(ui, text, t);
                ui.add_space(4.0);
            }
            Entry::Tool { name, summary, result, error } => {
                let (mark, col) = if *error { ("✗", t.red) } else { ("✓", t.green) };
                let head = format!("{mark} {}", summary.chars().take(90).collect::<String>());
                // A header never wraps: it is cut to the room there is. Opened, it shows the whole call.
                let room = (ui.available_width() - ui.spacing().indent - ui.spacing().button_padding.x).max(0.0);
                let head = egui::WidgetText::from(RichText::new(head).monospace().small().color(col)).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Truncate),
                    room,
                    egui::TextStyle::Button,
                );
                egui::CollapsingHeader::new(head).id_salt(("assistant_tool", name, summary.as_str(), result.len())).show(ui, |ui| {
                    ui.label(RichText::new(summary).monospace().small());
                    ui.label(RichText::new(result.chars().take(2000).collect::<String>()).monospace().small().color(t.text_dim));
                });
            }
            Entry::Error(e) => {
                show_error(ui, e, t);
            }
            Entry::Note(n) => {
                let args: Vec<(&str, &str)> = n.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
                ui.label(RichText::new(crate::i18n::fmt(n.template, &args)).small().italics().color(t.text_dim));
            }
        }
    }
    if agent.waiting() {
        // The reply so far, as it streams in.
        if let Some(text) = agent.preview() {
            markdown(ui, text, t);
            ui.spinner();
            return;
        }
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(tl!("Thinking…")).small().color(t.text_dim));
        });
    }
}

// ---------------------------------------------------------------------------------------------
// A small Markdown renderer: headings, lists, quotes, code blocks, tables (as text), **bold**,
// *italic*, `code` and links (shown as text). Model output is text, never markup to run.

fn markdown(ui: &mut Ui, text: &str, t: &Tokens) {
    let mut code: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match code.take() {
                Some(lines) => code_block(ui, &lines.join("\n"), t),
                None => code = Some(Vec::new()),
            }
            continue;
        }
        if let Some(lines) = code.as_mut() {
            lines.push(line);
            continue;
        }
        let trimmed = line.trim_start();
        // A few levels at most: a reply indented further would push its text out of the pane.
        let indent = ((line.len() - trimmed.len()) as f32 * 4.0).min(48.0);
        if trimmed.is_empty() {
            ui.add_space(4.0);
        } else if let Some(h) = ["### ", "## ", "# "].iter().find_map(|p| trimmed.strip_prefix(p)) {
            ui.add_space(2.0);
            ui.label(inline(ui, h, t, 14.0, true));
        } else if let Some(item) = ["- ", "* ", "+ "].iter().find_map(|p| trimmed.strip_prefix(p)) {
            bullet(ui, indent, "•", item, t);
        } else if let Some((num, item)) = numbered(trimmed) {
            bullet(ui, indent, &format!("{num}."), item, t);
        } else if let Some(q) = trimmed.strip_prefix("> ") {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("▎").color(t.border_strong));
                ui.label(inline(ui, q, t, 13.0, false));
            });
        } else if trimmed.starts_with('|') {
            // Table rows as monospace text; the divider row is dropped.
            if !trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                let cells: Vec<&str> = trimmed.trim_matches('|').split('|').map(str::trim).collect();
                ui.label(RichText::new(cells.join("  │  ")).monospace().small());
            }
        } else {
            ui.label(inline(ui, trimmed, t, 13.0, false));
        }
    }
    if let Some(lines) = code {
        code_block(ui, &lines.join("\n"), t);
    }
}

fn numbered(s: &str) -> Option<(&str, &str)> {
    let (num, rest) = s.split_once(". ")?;
    (!num.is_empty() && num.len() <= 3 && num.chars().all(|c| c.is_ascii_digit())).then_some((num, rest))
}

fn bullet(ui: &mut Ui, indent: f32, mark: &str, item: &str, t: &Tokens) {
    ui.horizontal_top(|ui| {
        ui.add_space(4.0 + indent);
        ui.label(RichText::new(mark).color(t.text_dim));
        ui.label(inline(ui, item, t, 13.0, false));
    });
}

fn code_block(ui: &mut Ui, code: &str, t: &Tokens) {
    egui::Frame::NONE.fill(t.input).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(4).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(code).monospace().small());
    });
}

/// A line with **bold**, *italic*, `code` and [links](url) (as their text) laid out.
fn inline(ui: &Ui, text: &str, t: &Tokens, size: f32, heading: bool) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let base = |bold: bool, italic: bool, code: bool| {
        let font = if code {
            egui::FontId::monospace(size - 1.0)
        } else if bold || heading {
            semibold(size)
        } else {
            regular(size)
        };
        TextFormat {
            font_id: font,
            color: if code { t.accent_text } else { t.text },
            italics: italic,
            background: if code { t.input } else { egui::Color32::TRANSPARENT },
            ..Default::default()
        }
    };
    let (mut bold, mut italic) = (false, false);
    let mut buf = String::new();
    let flush = |job: &mut LayoutJob, buf: &mut String, bold: bool, italic: bool| {
        if !buf.is_empty() {
            job.append(buf, 0.0, base(bold, italic, false));
            buf.clear();
        }
    };
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")) {
            flush(&mut job, &mut buf, bold, italic);
            bold = !bold;
            rest = r;
        } else if c == '`'
            && let Some(end) = rest.get(1..).and_then(|r| r.find('`'))
        {
            flush(&mut job, &mut buf, bold, italic);
            job.append(rest.get(1..1 + end).unwrap_or(""), 0.0, base(bold, italic, true));
            rest = rest.get(end + 2..).unwrap_or("");
        } else if c == '*' && rest.get(1..).is_some_and(|r| !r.starts_with(' ')) {
            flush(&mut job, &mut buf, bold, italic);
            italic = !italic;
            rest = rest.get(1..).unwrap_or("");
        } else if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest.get(close..).and_then(|r| r.find(')'))
        {
            buf.push_str(rest.get(1..close).unwrap_or(""));
            rest = rest.get(close + end + 1..).unwrap_or("");
        } else {
            buf.push(c);
            rest = rest.get(c.len_utf8()..).unwrap_or("");
        }
    }
    flush(&mut job, &mut buf, bold, italic);
    job
}

/// A peer's project cannot inherit a pending approval, provider reply or queued user action.
pub(crate) fn peer_replaced(app: &mut RustGisApp) {
    if let Some(agent) = &mut app.assistant.agent {
        agent.project_replaced();
    }
    app.assistant.inflight = None;
    app.assistant.chunks = None;
    app.assistant.queued = None;
    app.assistant.always = false;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    type KeyPrefs = std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>;

    /// An Assistant with isolated preferences and a fake keychain, never the OS store.
    #[cfg(not(target_arch = "wasm32"))]
    fn keys_app(
        legacy: Option<&str>,
        store: Option<std::sync::Arc<dyn rustgis_engine::data::secrets::SecretStore>>,
    ) -> (RustGisApp, KeyPrefs) {
        keys_app_remember(legacy, store, true)
    }

    /// Builds either the opt-in or opt-out startup path with isolated preference storage.
    #[cfg(not(target_arch = "wasm32"))]
    fn keys_app_remember(
        legacy: Option<&str>,
        store: Option<std::sync::Arc<dyn rustgis_engine::data::secrets::SecretStore>>,
        remember: bool,
    ) -> (RustGisApp, KeyPrefs) {
        use std::{
            collections::BTreeMap,
            sync::{Arc, Mutex},
        };
        let prefs = Arc::new(Mutex::new(BTreeMap::from([
            (provider::PROVIDER_PREF.into(), "custom".into()),
            (provider::REMEMBER_KEY_PREF.into(), provider::remember_key_value(remember).into()),
        ])));
        if let Some(key) = legacy {
            prefs.lock().unwrap().insert("assistant_key_custom".into(), key.into());
        }
        let mut app = RustGisApp::with_sample();
        let read = prefs.clone();
        app.services.load_pref = Some(Box::new(move |key| read.lock().unwrap().get(key).cloned()));
        let write = prefs.clone();
        app.services.save_pref = Some(Box::new(move |key, value| {
            write.lock().unwrap().insert(key.into(), value.into());
        }));
        app.services.secrets = store;
        ensure_loaded(&mut app, &egui::Context::default());
        (app, prefs)
    }

    /// Waits for the fake keychain and applies its preference cleanup on the UI side.
    #[cfg(not(target_arch = "wasm32"))]
    fn settle_keys(app: &mut RustGisApp) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            poll_keys(app, &egui::Context::default());
            let provider = app.assistant.agent.as_ref().map_or("custom", |a| a.config.provider.as_str());
            if !matches!(app.assistant.keys.status(provider), Some(crate::assistantkeys::Status::Pending { .. })) {
                return;
            }
            assert!(std::time::Instant::now() < end, "fake keychain timed out");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Only successful migrations clear legacy preferences; failures keep them usable and visible.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_migrate_without_losing_a_legacy_key() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretError, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        let (mut app, prefs) = keys_app(Some("synthetic-legacy"), Some(store.clone()));
        settle_keys(&mut app);
        assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("synthetic-legacy")));
        assert_eq!(prefs.lock().unwrap().get("assistant_key_custom").map(String::as_str), Some(""));
        assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, "synthetic-legacy");
        assert!(key_storage_note(&app, "custom").contains("system keychain"));
        let (mut failed, prefs) =
            keys_app(Some("synthetic-legacy"), Some(std::sync::Arc::new(MemorySecrets::failing(SecretError::Denied))));
        settle_keys(&mut failed);
        assert_eq!(prefs.lock().unwrap().get("assistant_key_custom").map(String::as_str), Some("synthetic-legacy"));
        assert_eq!(failed.assistant.agent.as_ref().unwrap().config.key, "synthetic-legacy");
        let note = key_storage_note(&failed, "custom");
        assert!(note.contains("unencrypted") && note.contains("refused"));
        assert!(!note.contains("synthetic-legacy"));
    }

    /// Desktop without a store never writes a new plaintext key; opting out also clears legacy plaintext.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_have_no_new_plaintext_fallback() {
        let (mut app, prefs) = keys_app(None, None);
        let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
        cfg.key = "synthetic-new".into();
        app.assistant.keys.edit("custom", &cfg.key);
        apply_config(&mut app, cfg.clone());
        save_config(&mut app, &cfg, &egui::Context::default());
        assert!(!prefs.lock().unwrap().contains_key("assistant_key_custom"));
        let note = key_storage_note(&app, "custom");
        assert!(note.contains("unavailable") && note.contains("session only"));
        assert!(!note.contains("synthetic-new"));
        let (mut legacy, prefs) = keys_app(Some("synthetic-legacy"), None);
        legacy.assistant.remember_key = false;
        let cfg = legacy.assistant.agent.as_ref().unwrap().config.clone();
        legacy.assistant.keys.edit("custom", &cfg.key);
        save_config(&mut legacy, &cfg, &egui::Context::default());
        assert_eq!(prefs.lock().unwrap().get("assistant_key_custom").map(String::as_str), Some(""));
    }

    /// A failed new-key write leaves the key in the session and never echoes backend payloads.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_report_failed_writes_without_leaking_the_key() {
        use rustgis_engine::data::secrets::{MemorySecrets, SecretError};
        for error in [SecretError::Denied, SecretError::Failed("synthetic-new".into())] {
            let (mut app, prefs) = keys_app(None, Some(std::sync::Arc::new(MemorySecrets::failing(error))));
            settle_keys(&mut app);
            assert!(key_storage_note(&app, "custom").contains("could not be read"));
            let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
            cfg.key = "synthetic-new".into();
            app.assistant.keys.edit("custom", &cfg.key);
            apply_config(&mut app, cfg.clone());
            save_config(&mut app, &cfg, &egui::Context::default());
            settle_keys(&mut app);
            assert!(!prefs.lock().unwrap().contains_key("assistant_key_custom"));
            assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, "synthetic-new");
            let note = key_storage_note(&app, "custom");
            assert!(note.contains("session only") && !note.contains("synthetic-new"));
        }
    }

    /// A failed replacement is session-only even when a different legacy key still needs migration.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_distinguish_a_failed_replacement_from_legacy_migration() {
        use rustgis_engine::data::secrets::{MemorySecrets, SecretError, SecretStore};
        let failing: std::sync::Arc<dyn SecretStore> =
            std::sync::Arc::new(MemorySecrets::failing(SecretError::Failed("synthetic-backend-payload".into())));
        for store in [None, Some(failing)] {
            let (mut app, prefs) = keys_app(Some("synthetic-legacy"), store);
            settle_keys(&mut app);
            let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
            cfg.key = "synthetic-replacement".into();
            app.assistant.keys.edit("custom", &cfg.key);
            apply_config(&mut app, cfg.clone());
            save_config(&mut app, &cfg, &egui::Context::default());
            settle_keys(&mut app);
            assert_eq!(prefs.lock().unwrap().get("assistant_key_custom").map(String::as_str), Some("synthetic-legacy"));
            assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, "synthetic-replacement");
            let note = key_storage_note(&app, "custom");
            assert!(note.contains("session only; it has not been remembered") && note.contains("unencrypted"));
            for secret in ["synthetic-legacy", "synthetic-replacement", "synthetic-backend-payload"] {
                assert!(!note.contains(secret));
            }
        }
    }

    /// Requests wait for the saved key instead of sending with a temporary empty/environment value.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_wait_before_contacting_a_provider() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        store.save("assistant/custom", &Secret::new("synthetic-stored")).unwrap();
        let (mut app, _) = keys_app(None, Some(store));
        assert!(key_loading(&app));
        let cfg = app.assistant.agent.as_ref().unwrap().config.clone();
        let ctx = egui::Context::default();
        list_models(&mut app, &cfg, &ctx);
        send(&mut app, "hello".into(), &ctx);
        assert!(app.assistant.catalog_pending && app.assistant.models_slot.is_none());
        assert!(app.assistant.inflight.is_none());
        assert_eq!(app.assistant.queued.as_deref(), Some("hello"));
        settle_keys(&mut app);
        assert!(!key_loading(&app));
        assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, "synthetic-stored");
    }

    /// Startup waits for the saved key before opening settings or restoring a folded provider.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_settings_follow_the_initial_key_read() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretError, SecretStore};
        for saved in [false, true] {
            for collapsed in [false, true] {
                let store = std::sync::Arc::new(MemorySecrets::new());
                if saved {
                    store.save("assistant/openai", &Secret::new("synthetic-stored")).unwrap();
                }
                let (mut app, prefs) = keys_app(None, None);
                prefs.lock().unwrap().insert(provider::PROVIDER_PREF.into(), "openai".into());
                prefs.lock().unwrap().insert("assistant_provider_closed".into(), if collapsed { "on" } else { "" }.into());
                app.assistant = AssistantState::default();
                app.services.secrets = Some(store);
                let ctx = egui::Context::default();
                app.assistant.keys.load("openai", "", None, true, app.services.secrets.clone(), &ctx);
                ensure_loaded(&mut app, &ctx);
                assert!(key_loading(&app));
                assert!(!app.assistant.settings);
                settle_keys(&mut app);
                assert_eq!(app.assistant.settings, !saved);
                assert_eq!(app.assistant.provider_closed, saved && collapsed);
            }
        }
        let (mut failed, _) = keys_app(None, Some(std::sync::Arc::new(MemorySecrets::failing(SecretError::Denied))));
        failed.assistant.agent.as_mut().unwrap().config.model.clear();
        settle_keys(&mut failed);
        assert!(failed.assistant.settings && !failed.assistant.provider_closed);
    }

    /// A user's open or close choice wins even when it returns to the pending default.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_settings_keep_user_choices_during_the_initial_key_read() {
        use rustgis_engine::data::secrets::MemorySecrets;
        for open in [false, true] {
            let (mut app, _) = keys_app(None, Some(std::sync::Arc::new(MemorySecrets::new())));
            assert!(key_loading(&app));
            app.assistant.agent.as_mut().unwrap().config.model = if open { "synthetic-model" } else { "" }.into();
            toggle_settings(&mut app);
            if !open {
                toggle_settings(&mut app);
            }
            settle_keys(&mut app);
            assert_eq!(app.assistant.settings, open);
        }
    }

    /// Clearing or unticking Remember deletes the key while keeping an unticked key usable this session.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_are_deleted_when_cleared_or_not_remembered() {
        use rustgis_engine::data::secrets::{MemorySecrets, SecretStore};
        for clear in [false, true] {
            let store = std::sync::Arc::new(MemorySecrets::new());
            let (mut app, _) = keys_app(Some("synthetic-legacy"), Some(store.clone()));
            settle_keys(&mut app);
            let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
            if clear {
                cfg.key.clear();
            } else {
                app.assistant.remember_key = false;
            }
            app.assistant.keys.edit("custom", &cfg.key);
            apply_config(&mut app, cfg.clone());
            save_config(&mut app, &cfg, &egui::Context::default());
            settle_keys(&mut app);
            assert_eq!(store.load("assistant/custom").unwrap(), None);
            assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, if clear { "" } else { "synthetic-legacy" });
        }
    }

    /// The shared settings harness installs the real fonts before drawing the form.
    #[cfg(not(target_arch = "wasm32"))]
    fn keys_harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(vec2(700.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    ui.ctx().request_repaint();
                    return;
                }
                let ctx = ui.ctx().clone();
                poll(app, &ctx);
                settings(app, ui, &Tokens::light());
            },
            app,
        )
    }

    /// Render the real local-agent settings without polling threads, preserving the interval
    /// between a checkbox callback and the next poll while the fake ACP agent replies.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn local_agents_harness(mut app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        app.assistant.local_agents = true;
        app.assistant.agents_auto = true;
        app.assistant.agents_found = Some((Vec::new(), Vec::new()));
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(vec2(700.0, 350.0)).build_ui_state(
            move |ui, app| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    ui.ctx().request_repaint();
                    return;
                }
                local_agents_setting(app, ui, &Tokens::light());
            },
            app,
        )
    }

    /// The actual Remember checkbox goes through the deferred save path and deletes the stored key.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_remember_checkbox_forgets_the_key() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::secrets::{MemorySecrets, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        let (mut app, prefs) = keys_app(Some("synthetic-legacy"), Some(store.clone()));
        settle_keys(&mut app);
        let mut h = keys_harness(app);
        h.run_steps(3);
        h.get_by_label("Remember the key on this computer").click();
        h.run_steps(4);
        settle_keys(h.state_mut());
        assert!(!h.state().assistant.remember_key);
        assert_eq!(store.load("assistant/custom").unwrap(), None);
        assert_eq!(prefs.lock().unwrap().get(provider::REMEMBER_KEY_PREF).map(String::as_str), Some("off"));
    }

    /// Ticking Remember after an off startup loads the existing key instead of deleting it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_remember_checkbox_loads_an_unread_key() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        store.save("assistant/custom", &Secret::new("synthetic-stored")).unwrap();
        let (app, prefs) = keys_app_remember(None, Some(store.clone()), false);
        let mut h = keys_harness(app);
        h.run_steps(3);
        h.get_by_label("Remember the key on this computer").click();
        h.run_steps(4);
        settle_keys(h.state_mut());
        assert!(h.state().assistant.remember_key);
        assert_eq!(h.state().assistant.agent.as_ref().unwrap().config.key, "synthetic-stored");
        assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("synthetic-stored")));
        assert_eq!(prefs.lock().unwrap().get(provider::REMEMBER_KEY_PREF).map(String::as_str), Some("on"));
    }

    /// Clearing the focused password field deletes its saved key before the field loses focus.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_clear_without_waiting_for_focus_loss() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::secrets::{MemorySecrets, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        let (mut app, _) = keys_app(Some("synthetic-legacy"), Some(store.clone()));
        settle_keys(&mut app);
        let mut h = keys_harness(app);
        h.run_steps(3);
        h.get_by_label("API key").focus();
        h.run_steps(2);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.key_press(egui::Key::Backspace);
        h.run_steps(3);
        assert!(h.get_by_label("API key").is_focused());
        assert!(h.state().assistant.agent.as_ref().unwrap().config.key.is_empty());
        settle_keys(h.state_mut());
        assert_eq!(store.load("assistant/custom").unwrap(), None);
    }

    /// Session-only typing waits for focus loss instead of deleting a key on each keystroke.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_session_key_typing_defers_storage() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::secrets::{MemorySecrets, SecretError};
        let (app, prefs) = keys_app_remember(None, Some(std::sync::Arc::new(MemorySecrets::failing(SecretError::Denied))), false);
        let mut h = keys_harness(app);
        h.run_steps(3);
        h.get_by_label("API key").focus();
        h.run_steps(2);
        prefs.lock().unwrap().remove("assistant_model_custom");
        for text in ["synthetic", "-session"] {
            h.get_by_label("API key").type_text(text);
            h.run_steps(3);
            assert!(h.get_by_label("API key").is_focused());
            assert!(matches!(h.state().assistant.keys.status("custom"), Some(crate::assistantkeys::Status::Session)));
            assert!(!prefs.lock().unwrap().contains_key("assistant_model_custom"));
        }
        assert_eq!(h.state().assistant.agent.as_ref().unwrap().config.key, "synthetic-session");
        h.get_by_label("Remember the key on this computer").focus();
        h.run_steps(3);
        settle_keys(h.state_mut());
        assert!(prefs.lock().unwrap().contains_key("assistant_model_custom"));
        assert!(matches!(h.state().assistant.keys.status("custom"), Some(crate::assistantkeys::Status::Failed { deleting: true, .. })));
    }

    /// Startup migrates keys for unselected providers without opening the Assistant or contacting models.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_migrate_all_legacy_providers_at_startup() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretStore};
        use std::{
            collections::BTreeMap,
            sync::{Arc, Mutex},
        };
        let prefs = Arc::new(Mutex::new(BTreeMap::from([
            (provider::REMEMBER_KEY_PREF.to_string(), "on".to_string()),
            ("assistant_key_custom".into(), "first".into()),
            ("assistant_key_anthropic".into(), "second".into()),
        ])));
        let store = Arc::new(MemorySecrets::new());
        let mut app = RustGisApp::with_sample();
        let read = prefs.clone();
        app.services.load_pref = Some(Box::new(move |key| read.lock().unwrap().get(key).cloned()));
        let write = prefs.clone();
        app.services.save_pref = Some(Box::new(move |key, value| {
            write.lock().unwrap().insert(key.into(), value.into());
        }));
        app.services.secrets = Some(store.clone());
        let ctx = egui::Context::default();
        poll_keys(&mut app, &ctx);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while ["custom", "anthropic"]
            .iter()
            .any(|p| matches!(app.assistant.keys.status(p), Some(crate::assistantkeys::Status::Pending { .. })))
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(std::time::Duration::from_millis(1));
            poll_keys(&mut app, &ctx);
        }
        assert!(app.assistant.agent.is_none());
        assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("first")));
        assert_eq!(store.load("assistant/anthropic").unwrap(), Some(Secret::new("second")));
        for p in ["custom", "anthropic"] {
            assert_eq!(prefs.lock().unwrap().get(&format!("assistant_key_{p}")).map(String::as_str), Some(""));
        }
    }

    /// A signed-out ChatGPT settings pane for tests that never contact a provider.
    fn plan_app() -> RustGisApp {
        let mut app = RustGisApp::with_sample();
        let mut cfg = Config::from_env("chatgpt");
        cfg.key.clear();
        cfg.account.clear();
        cfg.client_id.clear();
        app.assistant.agent = Some(Agent::new(cfg, &app.session));
        app.assistant.loaded = true;
        app.run("ui.pane.assistant", serde_json::Value::Null).unwrap();
        app.assistant.settings = true;
        app
    }

    /// Clicking Stop drops the old UI backoff deadline and streamed chunks before another run.
    #[test]
    fn stop_button_clears_retry_deadline_and_chunks() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let mut config = Config::from_env("custom");
        config.model = "test".into();
        let mut agent = Agent::new(config, &app.session);
        assert!(matches!(agent.ask("hello", &app.session), Step::Send(_)));
        let response = Ok(HttpResponse { status: 503, body: b"{}".to_vec(), headers: vec![("retry-after".into(), "0".into())] });
        assert!(matches!(agent.receive_result(agent.generation(), response, &mut app.session), Step::Retry(_)));
        let generation = agent.generation();
        app.assistant.agent = Some(agent);
        app.assistant.retry = Some((generation, web_time::Instant::now()));
        app.assistant.chunks = Some((generation, Chunks::default()));
        app.assistant.inflight = Some((generation, Slot::default()));
        // Render just the composer: no polling or network dispatch is needed to press Stop.
        let mut h = egui_kittest::Harness::builder().with_size(vec2(600.0, 160.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                let busy = app.assistant.agent.as_ref().is_some_and(Agent::busy);
                composer(app, ui, &Tokens::light(), busy, &ctx);
            },
            app,
        );
        h.run();
        h.get_by_label("Stop").click();
        h.step();
        assert!(h.state().assistant.retry.is_none());
        assert!(h.state().assistant.chunks.is_none());
        assert!(h.state().assistant.inflight.is_none());
        assert!(!h.state().assistant.agent.as_ref().unwrap().busy());
    }

    /// Real pending reviews keep their reach visible while exact requests wait under Details.
    #[test]
    fn compact_approval_details_preserve_frozen_request_and_defaults() {
        use egui_kittest::kittest::Queryable;
        use serde_json::json;

        for (tool, args, expected) in [
            (
                "run",
                json!({"script": concat!(
                    "layer.visibility {\"layer\":\"Countries\",\"visible\":false}\n",
                    "layer.visibility {\"visible\":false}\n",
                    "map.addTiles {\"name\":\"Synthetic tiles\",\"url\":\"https://tiles.invalid/{z}/{x}/{y}.png\"}"
                )}),
                vec!["3 project edits", "line 3 (map.addTiles)", "World / Countries", "https://tiles.invalid/{z}/{x}/{y}.png"],
            ),
            (
                "run_command",
                json!({"command":"layer.addSql","params":{"sql":"FROM t","database":"/synthetic/views.duckdb"}}),
                vec!["views in the database /synthetic/views.duckdb", "Destinations"],
            ),
        ] {
            let mut app = RustGisApp::with_sample();
            app.session.run("map.basemap", &json!({"name":"ocean"})).unwrap();
            app.session.run("layer.select", &json!({"layer":"Cities"})).unwrap();
            let config = Config {
                provider: "custom".into(),
                base_url: "https://provider.invalid/v1".into(),
                model: "synthetic".into(),
                key: String::new(),
                account: String::new(),
                client_id: String::new(),
                session: String::new(),
                reasoning: None,
                tool_output_token_limit: rustgis_assistant::tools::DEFAULT_OUTPUT_TOKENS,
            };
            let mut agent = Agent::new(config, &app.session);
            assert!(matches!(agent.ask("Review only", &app.session), Step::Send(_)));
            let body = json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{
                "id":"compact-card","type":"function","function":{"name":tool,"arguments":args.to_string()}
            }]},"finish_reason":"tool_calls"}]})
            .to_string()
            .into_bytes();
            assert_eq!(
                agent.receive(agent.generation(), Ok(HttpResponse { status: 200, body, headers: vec![] }), &mut app.session),
                Step::Confirm
            );
            let exact = agent.pending_review().unwrap().what.clone();
            let can_always = agent.pending(&app.session).unwrap().2;
            let project = serde_json::to_value(&app.session.project).unwrap();
            app.assistant.agent = Some(agent);
            let mut fonts = false;
            let mut h = egui_kittest::Harness::builder().with_size(vec2(680.0, 600.0)).build_ui_state(
                move |ui, app: &mut RustGisApp| {
                    if !fonts {
                        crate::theme::install_fonts(ui.ctx(), &[], None);
                        fonts = true;
                        ui.ctx().request_repaint();
                        return;
                    }
                    let ctx = ui.ctx().clone();
                    composer(app, ui, &Tokens::light(), true, &ctx);
                },
                app,
            );
            h.run();
            assert!(h.query_by_label(&exact).is_none(), "raw vehicle starts collapsed");
            for text in expected {
                assert!(h.query_by_label_contains(text).is_some(), "missing overview context: {text}");
            }
            assert!(h.query_by_label_contains("Starting active layer: Cities; selected layers: [Cities]").is_some());
            assert_eq!(h.query_by_label("Allow such actions for this conversation").is_some(), can_always);
            h.get_by_label("Details").click();
            h.run();
            h.get_by_label(&exact);
            h.get_by_label("Allow");
            h.get_by_label("Decline");
            h.get_by_label("Details").click();
            h.run();
            assert!(h.query_by_label(&exact).is_none());
            assert_eq!(h.state().assistant.agent.as_ref().unwrap().pending_review().unwrap().what, exact);
            assert_eq!(serde_json::to_value(&h.state().session.project).unwrap(), project);
            assert!(!h.state().assistant.always);
        }
    }

    #[test]
    fn plan_settings_order_and_signed_out_model() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut h = egui_kittest::Harness::builder().with_size(vec2(800.0, 600.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            plan_app(),
        );
        h.run();
        let provider = h.get_by_label("Provider").rect().min.y;
        let account = h.get_by_label("Account").rect().min.y;
        let model = h.get_by_label("Model").rect().min.y;
        assert!(provider < account && account < model);
        assert!(h.query_by_label("Address").is_none());
        assert!(h.get_by_label("Sign in to choose a model.").accesskit_node().is_disabled());
        let mut cfg = Config::from_env("custom");
        cfg.model = "local-model".into();
        h.state_mut().assistant.agent.as_mut().unwrap().config = cfg;
        h.run();
        assert!(h.query_by_label("Address").is_some());
        assert!(h.query_by_label("API key").is_some());
    }

    /// The AI provider section folds away once set up, and the choice is saved.
    #[test]
    fn provider_section_collapses_and_is_remembered() {
        use egui_kittest::kittest::Queryable;
        use std::sync::{Arc, Mutex};
        let mut app = plan_app();
        let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let log = saved.clone();
        app.services.save_pref = Some(Box::new(move |k, v| log.lock().unwrap().push((k.to_string(), v.to_string()))));
        let mut h = egui_kittest::Harness::builder().with_size(vec2(800.0, 600.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run();
        assert!(h.query_by_label("Provider").is_some());
        h.get_by_label("AI provider").click();
        h.run();
        assert!(h.query_by_label("Provider").is_none());
        assert!(h.state().assistant.provider_closed);
        assert!(saved.lock().unwrap().contains(&("assistant_provider_closed".into(), "on".into())));
        h.get_by_label("AI provider").click();
        h.run();
        assert!(h.query_by_label("Provider").is_some());
        assert!(saved.lock().unwrap().contains(&("assistant_provider_closed".into(), String::new())));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn waiting_controls_reopen_copy_and_cancel() {
        use egui_kittest::kittest::Queryable;
        let url = "https://auth.openai.com/synthetic-sign-in";
        let mut h = egui_kittest::Harness::builder().with_size(vec2(260.0, 220.0)).build_ui_state(
            |ui, actions: &mut (bool, bool)| {
                let (reopen, cancel) = waiting_for_browser(ui, &Tokens::light(), url);
                actions.0 |= reopen;
                actions.1 |= cancel;
            },
            (false, false),
        );
        h.step();
        for label in ["Waiting for the browser…", "Open browser again", "Copy sign-in link", "Cancel"] {
            let rect = h.get_by_label(label).rect();
            assert!(rect.max.x <= 260.0 && rect.max.y <= 220.0, "{label}: {rect:?}");
        }
        h.get_by_label("Open browser again").click();
        h.step();
        assert_eq!(*h.state(), (true, false));
        h.get_by_label("Copy sign-in link").click();
        h.step();
        assert!(h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == url)));
        h.get_by_label("Cancel").click();
        h.step();
        assert_eq!(*h.state(), (true, true));
    }

    #[test]
    fn tall_settings_keep_transcript_above_composer() {
        use egui_kittest::kittest::Queryable;
        // A ChatGPT plan and a provider with a key: the settings scroll above the composer either way.
        for (plan, width, height) in [(true, 960.0, 700.0), (true, 800.0, 600.0), (false, 800.0, 420.0)] {
            let mut app = if plan {
                plan_app()
            } else {
                let mut app = RustGisApp::with_sample();
                app.assistant.agent = Some(Agent::new(Config::from_env("custom"), &app.session));
                app.assistant.loaded = true;
                app.run("ui.pane.assistant", serde_json::Value::Null).unwrap();
                app.assistant.settings = true;
                app
            };
            app.assistant.login_error = Some(rustgis_assistant::Error::new(rustgis_assistant::Recovery::Auth, ""));
            let mut h = egui_kittest::Harness::builder().with_size(vec2(width, height)).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.run();
            assert!(h.get_by_label("Send").rect().max.y <= height - 26.0);
            let input_top = h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect().min.y;
            for shape in &h.output().shapes {
                if let egui::epaint::Shape::Text(text) = &shape.shape
                    && SUGGESTIONS.contains(&text.galley.job.text.as_str())
                {
                    assert!(shape.clip_rect.max.y <= input_top, "suggestions overlap the composer");
                }
            }
        }
    }

    /// Runs the test `name` again in a child process whose settings folder is a fresh temporary
    /// one, so ChatGPT credentials and lock files never touch the developer's own (the
    /// environment of this process is left alone). Gives whether this is the child, which runs
    /// the test's body.
    #[cfg(not(target_arch = "wasm32"))]
    fn in_isolated_settings(name: &str) -> bool {
        if std::env::var_os("RUSTGIS_TEST_ISOLATED").is_some() {
            return true;
        }
        let short = name.rsplit("::").next().unwrap_or(name);
        let dir = std::env::temp_dir().join(format!("rustgis-ui-{short}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name])
            .env("RUSTGIS_TEST_ISOLATED", "1")
            .env(rustgis_engine::data::dirs::CONFIG_ENV, &dir)
            .env("RUSTGIS_LANGUAGE", "en")
            .output()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("1 passed"), "the isolated run didn't run {name}:\n{stdout}");
        false
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn saved_accounts_load_models_at_startup_and_on_provider_and_account_switch() {
        use egui_kittest::kittest::Queryable;
        if !in_isolated_settings("assistantpane::tests::saved_accounts_load_models_at_startup_and_on_provider_and_account_switch") {
            return;
        }
        let a = chatgpt::Credentials {
            client_id: "test-personal".into(),
            subject: "test-user".into(),
            email: "same@example.test".into(),
            // No expiry and no refresh token: a request fails at renewal, without network I/O.
            access_token: "synthetic-token".into(),
            scopes: vec![chatgpt::PLAN_SCOPE.into()],
            ..Default::default()
        };
        let b = chatgpt::Credentials { client_id: "test-workspace".into(), ..a.clone() };
        let path = chatgpt::auth_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // As versions before the keychain saved them, with their tokens; this run has no keychain,
        // so they stay there.
        let saved = |c: &chatgpt::Credentials| {
            let mut v = c.to_json();
            v["access_token"] = c.access_token.clone().into();
            v
        };
        std::fs::write(
            &path,
            serde_json::json!({"version":2, "active":{"client_id":a.client_id,"subject":a.subject},
            "accounts":[saved(&a),saved(&b)]})
            .to_string(),
        )
        .unwrap();
        let mut app = RustGisApp::with_sample();
        app.services.load_pref = Some(Box::new(|key| (key == "assistant_provider").then(|| "chatgpt".into())));
        ensure_loaded(&mut app, &egui::Context::default());
        // The sign-in is read on a worker; the catalog request follows once it arrives.
        let ctx = egui::Context::default();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.assistant.models_slot.is_none() && app.assistant.models_error.is_none() {
            assert!(std::time::Instant::now() < end, "the sign-in read never finished");
            std::thread::sleep(std::time::Duration::from_millis(1));
            poll(&mut app, &ctx);
        }
        assert_eq!(app.assistant.account.as_ref().map(|a| a.kept), Some(chatgpt::Kept::SettingsFile));
        app.run("ui.pane.assistant", serde_json::Value::Null).unwrap();
        app.assistant.settings = true;
        // Draw settings without polling, so each click's catalog request remains observable.
        // A fixed number of frames, not `run`: each catalog request ends on a thread that asks for a
        // repaint, which can land mid-run (it did on Windows) and run past `run`'s step limit.
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(400.0, 600.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    ui.ctx().request_repaint();
                    return;
                }
                settings(app, ui, &Tokens::light());
            },
            app,
        );
        h.run_steps(4);
        h.state_mut().assistant.models_slot = None;
        h.get_by_value("ChatGPT (sign in)").click();
        h.run_steps(4);
        h.get_by_label("Ollama (local)").click();
        h.run_steps(4);
        assert!(h.state().assistant.models_slot.is_none());
        h.get_by_value("Ollama (local)").click();
        h.run_steps(4);
        h.get_by_label("ChatGPT (sign in)").click();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while h.state().assistant.models_slot.is_none() {
            assert!(std::time::Instant::now() < end, "the sign-in read never finished");
            std::thread::sleep(std::time::Duration::from_millis(1));
            h.run_steps(1);
        }
        h.state_mut().assistant.models_slot = None;
        // A read begun before choosing an account must not undo the choice when it finishes.
        let stale: Slot<Option<chatgpt::Credentials>> = Slot::default();
        h.state_mut().assistant.sign_in_read = Some((None, stale.clone()));
        h.get_by_value(&a.label()).click();
        h.run_steps(4);
        h.get_by_label(&b.label()).click();
        h.run_steps(4);
        *stale.lock().unwrap() = Some(Some(a.clone()));
        h.run_steps(4);
        assert!(h.state().assistant.models_slot.is_some());
        assert_eq!(h.state().assistant.agent.as_ref().unwrap().config.client_id, b.client_id);
        assert_eq!(chatgpt::load_any().unwrap().client_id, b.client_id);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_sign_in_check_never_switches_accounts_and_a_switch_stops_work() {
        if !in_isolated_settings("assistantpane::tests::a_sign_in_check_never_switches_accounts_and_a_switch_stops_work") {
            return;
        }
        let ctx = egui::Context::default();
        let creds = |client: &str| chatgpt::Credentials {
            client_id: client.into(),
            subject: "user".into(),
            session: "session".into(),
            access_token: "synthetic-token".into(),
            scopes: vec![chatgpt::PLAN_SCOPE.into()],
            ..Default::default()
        };
        let finish = |app: &mut RustGisApp, checking: Option<chatgpt::Binding>, read: Option<chatgpt::Credentials>| {
            let slot: Slot<Option<chatgpt::Credentials>> = Slot::default();
            *slot.lock().unwrap() = Some(read);
            app.assistant.sign_in_read = Some((checking, slot));
            poll_sign_in(app, &ctx);
        };
        let mut app = plan_app();
        let mut first = app.assistant.agent.as_ref().unwrap().config.clone();
        first.sign_in(&creds("first"));
        apply_config(&mut app, first.clone());
        // A check after a failed request that finds another account selected changes nothing:
        // the user chooses.
        app.assistant.retry = Some((1, web_time::Instant::now()));
        finish(&mut app, Some(first.binding()), Some(creds("second")));
        assert_eq!(app.assistant.agent.as_ref().unwrap().config, first);
        assert!(app.assistant.retry.is_some());
        // One that finds the sign-in gone clears it and stops the work bound to it, and a model
        // list still on its way can't bring its catalog back.
        let late: Reply = Slot::default();
        app.assistant.models_slot = Some(late.clone());
        app.assistant.models = vec!["first-model".into()];
        // A message typed while the check was under way, and a catalog request it deferred.
        app.assistant.queued = Some("synthetic question".into());
        app.assistant.catalog_pending = true;
        finish(&mut app, Some(first.binding()), None);
        assert!(app.assistant.agent.as_ref().unwrap().config.key.is_empty());
        assert!(app.assistant.retry.is_none() && app.assistant.settings);
        assert!(app.assistant.models.is_empty() && app.assistant.models_slot.is_none());
        *late.lock().unwrap() = Some(Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: serde_json::json!({"models": [{"slug": "late", "visibility": "list"}]}).to_string().into_bytes(),
        }));
        poll(&mut app, &ctx);
        assert!(app.assistant.models.is_empty() && app.assistant.catalog.is_empty());
        // Neither goes out after the sign-out; the message is back in the input box.
        assert!(app.assistant.queued.is_none() && !app.assistant.catalog_pending);
        assert!(app.assistant.inflight.is_none() && app.assistant.models_slot.is_none());
        assert_eq!(app.assistant.input, "synthetic question");
        // Taking the selected account: another one stops work for the first and drops its catalog.
        apply_config(&mut app, first.clone());
        app.assistant.retry = Some((2, web_time::Instant::now()));
        app.assistant.models = vec!["first-model".into()];
        finish(&mut app, None, Some(creds("second")));
        assert_eq!(app.assistant.agent.as_ref().unwrap().config.client_id, "second");
        assert!(app.assistant.retry.is_none());
        assert!(!app.assistant.models.contains(&"first-model".to_string()));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn sign_in_loads_catalog_and_applies_first_offered_model() {
        if !in_isolated_settings("assistantpane::tests::sign_in_loads_catalog_and_applies_first_offered_model") {
            return;
        }
        let mut app = plan_app();
        let c = chatgpt::Credentials {
            access_token: "synthetic-token".into(),
            client_id: "client".into(),
            subject: "user".into(),
            scopes: vec![chatgpt::PLAN_SCOPE.into()],
            ..Default::default()
        };
        let ctx = egui::Context::default();
        signed_in(&mut app, &c, &ctx);
        assert!(app.assistant.models_slot.is_some());
        let slot = Slot::default();
        *slot.lock().unwrap() = Some(Ok(HttpResponse { status: 200, headers: vec![],
            body: serde_json::json!({"models":[{"slug":"offered", "display_name":"Offered model", "visibility":"list", "supports_reasoning":false}]}).to_string().into_bytes() }));
        app.assistant.models_slot = Some(slot);
        poll(&mut app, &ctx);
        let cfg = &app.assistant.agent.as_ref().unwrap().config;
        assert_eq!(cfg.model, "offered");
        assert_eq!(cfg.reasoning, Some(false));
        assert_eq!(app.assistant.catalog[0].name, "Offered model");
        signed_in(&mut app, &chatgpt::Credentials { scopes: vec![], ..c }, &ctx);
        assert!(app.assistant.models_slot.is_none());
        assert!(app.assistant.agent.as_ref().unwrap().config.key.is_empty());
        assert!(!app.assistant.welcome);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_plan_requests_cannot_bypass_auth_through_host_http() {
        if !in_isolated_settings("assistantpane::tests::native_plan_requests_cannot_bypass_auth_through_host_http") {
            return;
        }
        let mut app = plan_app();
        app.services.http = Some(Box::new(|_, _| panic!("the host hook must not receive plan credentials")));
        let request = HttpRequest { method: "GET", url: format!("{}/models", chatgpt::API), headers: vec![], body: None };
        let slot = dispatch(&mut app, request, Some(chatgpt::Binding::default()), None, &egui::Context::default());
        let start = std::time::Instant::now();
        loop {
            if let Some(result) = take(&slot) {
                assert_eq!(result.unwrap_err().recovery, rustgis_assistant::Recovery::SignIn);
                break;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::yield_now();
        }
    }

    #[test]
    fn usage_error_offers_weekly_usage() {
        use egui_kittest::kittest::Queryable;
        let error = rustgis_assistant::Error::new(rustgis_assistant::Recovery::Usage, "");
        let mut app = plan_app();
        app.assistant.settings = false;
        app.assistant.agent.as_mut().unwrap().transcript.push(Entry::Error(error));
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run();
        assert!(h.query_by_label("Weekly usage").is_some());
        h.get_by_label("Weekly usage").click();
        h.step();
        assert!(
            h.output().platform_output.commands.iter().any(|c| {
                matches!(c, egui::OutputCommand::OpenUrl(url) if url.url == "https://chatgpt.com/settings/usage?tab=overview")
            })
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn late_sign_in_never_changes_another_providers_key() {
        if !in_isolated_settings("assistantpane::tests::late_sign_in_never_changes_another_providers_key") {
            return;
        }
        let mut app = RustGisApp::with_sample();
        let cfg = Config {
            provider: "custom".into(),
            base_url: "http://localhost:1234/v1".into(),
            model: "test".into(),
            key: "original-key".into(),
            account: String::new(),
            client_id: String::new(),
            session: String::new(),
            reasoning: None,
            tool_output_token_limit: rustgis_assistant::tools::DEFAULT_OUTPUT_TOKENS,
        };
        app.assistant.agent = Some(Agent::new(cfg.clone(), &app.session));
        let c = chatgpt::Credentials { access_token: "synthetic-chatgpt-token".into(), ..Default::default() };
        signed_in(&mut app, &c, &egui::Context::default());
        assert_eq!(app.assistant.agent.as_ref().unwrap().config, cfg);
        assert!(app.assistant.models_slot.is_none());
    }

    #[test]
    fn provider_change_stops_the_previous_request() {
        let mut app = RustGisApp::with_sample();
        let cfg = Config {
            provider: "custom".into(),
            base_url: "http://localhost:1234/v1".into(),
            model: "test".into(),
            key: String::new(),
            account: String::new(),
            client_id: String::new(),
            session: String::new(),
            reasoning: None,
            tool_output_token_limit: rustgis_assistant::tools::DEFAULT_OUTPUT_TOKENS,
        };
        let mut agent = Agent::new(cfg.clone(), &app.session);
        assert!(matches!(agent.ask("hello", &app.session), Step::Send(_)));
        let generation = agent.generation();
        app.assistant.agent = Some(agent);
        app.assistant.inflight = Some((generation, Slot::default()));
        apply_config(&mut app, Config { provider: "openai".into(), ..cfg });
        let agent = app.assistant.agent.as_ref().unwrap();
        assert!(!agent.busy());
        assert_ne!(agent.generation(), generation);
        assert!(app.assistant.inflight.is_none());
    }

    /// The right pane's width with `entries` in the conversation and `model` as the model's name.
    fn pane_width(entries: Vec<Entry>, model: &str) -> f32 {
        let h = chat(entries, model);
        right_pane(&h).width()
    }

    fn right_pane(h: &egui_kittest::Harness<'_, RustGisApp>) -> egui::Rect {
        h.state().ui.pane_rects[1].unwrap()
    }

    /// The window with the assistant open and `entries` in the conversation.
    fn chat(entries: Vec<Entry>, model: &str) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        );
        h.state_mut().run("ui.pane.assistant", serde_json::Value::Null).unwrap();
        for _ in 0..4 {
            h.step();
        }
        let app = h.state_mut();
        app.assistant.settings = false;
        let agent = app.assistant.agent.as_mut().unwrap();
        agent.config.model = model.into();
        agent.transcript = entries;
        for _ in 0..6 {
            h.step();
        }
        h
    }

    /// #307: a reply wider than the pane pushed its edge off the window, so the map covered the
    /// chat and the edge could not be dragged. The pane stays in the window and keeps resizing.
    #[test]
    fn wide_reply_keeps_the_pane_in_the_window_and_resizable() {
        let sentence = "The assistant ran the buffer tool on the cities layer and added the result to the map. ".repeat(4);
        let mut h = chat(vec![Entry::Assistant(format!("{}- {sentence}", " ".repeat(400)))], "m");
        for _ in 0..20 {
            h.step();
        }
        let before = right_pane(&h);
        let window = h.ctx.content_rect();
        assert!(before.max.x <= window.max.x + 0.5, "the pane's edge left the window: {before:?} in {window:?}");
        // Drag the pane's edge 150 points towards the map (grabbed just inside the pane, as the
        // map's own drag sense is on top at the boundary).
        let edge = egui::pos2(before.min.x + 2.0, before.center().y);
        h.hover_at(edge);
        h.step();
        h.drag_at(edge);
        h.step();
        for dx in [50.0, 100.0, 150.0] {
            h.hover_at(edge - vec2(dx, 0.0));
            h.step();
        }
        h.event(egui::Event::PointerButton {
            pos: edge - vec2(150.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..4 {
            h.step();
        }
        let after = right_pane(&h);
        assert!((after.min.x - (edge.x - 150.0)).abs() < 3.0, "the pane did not follow the drag: {before:?} to {after:?}");
        assert!(after.max.x <= window.max.x + 0.5, "the pane's edge left the window: {after:?} in {window:?}");
    }

    /// Dragging the grip above the input box up makes the box taller; Send stays in the window.
    #[test]
    fn input_box_resizes_by_its_grip() {
        use egui_kittest::kittest::Queryable;
        let mut h = chat(Vec::new(), "m");
        let input = |h: &egui_kittest::Harness<'_, RustGisApp>| h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect();
        let before = input(&h);
        let grip = egui::pos2(before.center().x, before.min.y - 4.0);
        h.hover_at(grip);
        h.step();
        h.drag_at(grip);
        h.step();
        for dy in [40.0, 80.0, 120.0] {
            h.hover_at(grip - vec2(0.0, dy));
            h.step();
        }
        h.event(egui::Event::PointerButton {
            pos: grip - vec2(0.0, 120.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..4 {
            h.step();
        }
        let after = input(&h);
        assert!((after.height() - before.height() - 120.0).abs() < 4.0, "the box did not follow the drag: {before:?} to {after:?}");
        assert!(h.get_by_label("Send").rect().max.y <= h.ctx.content_rect().max.y);
    }

    #[test]
    fn long_lines_wrap_instead_of_widening_the_pane() {
        let sentence = "The assistant ran the buffer tool on the cities layer and added the result to the map. ".repeat(4);
        let empty = pane_width(Vec::new(), "m");
        let mut widened = Vec::new();
        for (what, entry, model) in [
            ("a list item", Entry::Assistant(format!("- {sentence}")), "m"),
            ("a numbered item", Entry::Assistant(format!("1. {sentence}")), "m"),
            ("a quote", Entry::Assistant(format!("> {sentence}")), "m"),
            ("a word with no spaces", Entry::Assistant(format!("- {}", "x".repeat(400))), "m"),
            ("a deeply indented item", Entry::Assistant(format!("{}- {sentence}", " ".repeat(200))), "m"),
            ("the user's message", Entry::User(sentence.clone()), "m"),
            (
                "a tool call",
                Entry::Tool { name: "run_command".into(), summary: "gp.run ".repeat(20), result: sentence.clone(), error: false },
                "m",
            ),
            (
                "the model's name",
                Entry::Assistant("Done.".into()),
                "provider/a-very-long-model-name-with-many-parts-preview-2026-10-14-extended",
            ),
        ] {
            let w = pane_width(vec![entry], model);
            if (w - empty).abs() > 0.5 {
                widened.push(format!("{what}: {empty} to {w}"));
            }
        }
        assert!(widened.is_empty(), "the pane grew wider with {widened:?}");
    }
}
