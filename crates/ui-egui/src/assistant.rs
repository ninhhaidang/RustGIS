//! The AI Assistant's conversation, without a layout: its messages, the action waiting for
//! approval, the provider's settings and keys, and sending, deciding and stopping. The desktop
//! pane (`assistantpane`) draws it, and another interface can drive it the same way:
//!
//! - [`open`] on each frame that shows it, which loads the chosen provider the first time;
//! - [`AssistantState::config`] and [`key_loading`] say which provider is chosen, and
//!   [`rustgis_assistant::Config::missing`] why it isn't set up;
//! - [`AssistantState::messages`] and [`AssistantState::preview`] are what to draw, and
//!   [`AssistantState::busy`] and [`AssistantState::waiting`] what is going on;
//! - [`send`] a message, [`stop`] the run, start a [`new_conversation`] or [`compact`] it;
//! - [`pending`] is the action waiting for approval, which [`decide`] answers.
//!
//! Requests go out off the frame: on a thread on the desktop, through `Services::http` (the
//! browser's fetch) on the web. Their replies are taken in by [`RustGisApp::logic`] each frame.

use rustgis_assistant::{Agent, Config, Entry, HttpRequest, HttpResponse, PRESETS, Step, chatgpt, provider};

use crate::{RustGisApp, Slot};

/// Where a response lands.
pub(crate) type Reply = Slot<Result<HttpResponse, rustgis_assistant::Error>>;
/// A streamed reply's pieces, as they arrive.
pub(crate) type Chunks = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

/// The Assistant's conversation and settings.
#[derive(Default)]
pub struct AssistantState {
    pub(crate) agent: Option<Agent>,
    /// The message being written.
    pub input: String,
    /// The pictures attached to the message being written.
    pub(crate) pictures: crate::assistantpictures::Pictures,
    /// The input box's height, set by dragging the grip above it (0 until dragged: two lines).
    pub(crate) input_height: f32,
    /// A message to send on the next frame (`ui.pane.assistant {"ask": …}`), or once the key or a
    /// picture it waits for is read.
    pub queued: Option<String>,
    /// The message waiting is the input box's own draft (sent from the box, which keeps it until
    /// it goes): nothing to put back if it isn't sent.
    pub(crate) queued_from_box: bool,
    /// Pictures that landed while the extension was off, to be said once.
    pub(crate) late_pictures: usize,
    /// The request out: its generation and where its response lands.
    pub(crate) inflight: Option<(u64, Reply)>,
    /// The pieces of the request out's reply, as they stream in (desktop).
    pub(crate) chunks: Option<(u64, Chunks)>,
    /// The settings form is open.
    pub settings: bool,
    /// Startup layout waits for the initial key read unless the user chooses it first.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) settings_pending: Option<String>,
    pub(crate) show_key: bool,
    /// Keys are remembered only when the user says so.
    pub(crate) remember_key: bool,
    /// Desktop keychain operations and session keys, separate from preference storage.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) keys: crate::assistantkeys::Keys,
    /// Legacy migration is scheduled once after the platform preferences are available.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) keys_migrated: bool,
    /// The AI provider section of the settings is folded away (kept in the preferences).
    pub(crate) provider_closed: bool,
    pub(crate) models: Vec<String>,
    pub(crate) catalog: Vec<provider::Model>,
    /// The catalog is fetched once per provider/account selection.
    pub(crate) catalog_pending: bool,
    pub(crate) models_slot: Option<Reply>,
    pub(crate) models_error: Option<rustgis_assistant::Error>,
    /// Earlier messages, for Up/Down in the composer.
    pub(crate) history: Vec<String>,
    pub(crate) history_pos: Option<usize>,
    /// "Allow for this conversation" in the approval card.
    pub(crate) always: bool,
    pub(crate) loaded: bool,
    /// Settings changed and not yet saved: they are saved once no settings field is being typed in.
    pub(crate) unsaved: bool,
    /// A ChatGPT sign-in under way in the browser (desktop only).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) login: Option<chatgpt::Login>,
    /// The ChatGPT account signed in, for the settings.
    pub(crate) account: Option<chatgpt::Account>,
    pub(crate) login_error: Option<rustgis_assistant::Error>,
    /// A sign-out's revocation at OpenAI, under way (desktop only).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) signout: Option<Slot<Result<(), rustgis_assistant::Error>>>,
    /// The ChatGPT sign-in, being read on a worker: its tokens are in the system keychain, which
    /// may block or ask (desktop only).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) sign_in_read: Option<SignInRead>,
    /// A refused saved-account store is reported once through this app session's notices.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) sign_in_store_error_said: bool,
    /// The first sign-in's confirmation that the ChatGPT plan is in use, until dismissed.
    pub(crate) welcome: bool,
    /// A failed request to send again: its generation and when.
    pub(crate) retry: Option<(u64, web_time::Instant)>,
    /// The user lets the assistant hand tasks to local agents (Claude Code, Codex).
    pub(crate) local_agents: bool,
    /// Agent threads on this computer: their processes, windows and requests (desktop).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) threads: crate::agentthreads::Threads,
    /// The user lets agents' requests through without asking (off unless chosen).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) agents_auto: bool,
    /// The saved ChatGPT registrations, read once (the file is locked across processes) and
    /// again after a sign-in, sign-out or selection.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) accounts: Option<Vec<chatgpt::Credentials>>,
    /// The last sign-in added a second registration for an account already saved.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) duplicate: bool,
    /// The plugins' revision when the agent's skills were read: plugins carry skills, so they're
    /// read again when plugins change.
    pub(crate) skills_rev: u64,
    /// The settings' Skills list, read when shown (desktop).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) skills_list: Option<(Vec<rustgis_assistant::skills::Listed>, Vec<String>)>,
    /// What the last change to the skills did: whether it worked, and what to say.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) skills_message: Option<(bool, String)>,
    /// The agents on this computer, looked for when the settings open (a search of `PATH`, not
    /// every frame) and again when the switch changes.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) agents_found: Option<rustgis_assistant::acp::Installed>,
}

impl AssistantState {
    /// The conversation's messages, oldest first: the user's, the replies, each command run with
    /// what came back, errors and notes.
    pub fn messages(&self) -> &[Entry] {
        self.agent.as_ref().map_or(&[], |a| a.transcript.as_slice())
    }

    /// The text so far of a reply still streaming in.
    pub fn preview(&self) -> Option<&str> {
        self.agent.as_ref().and_then(Agent::preview)
    }

    /// A request is out, or an action waits for approval.
    pub fn busy(&self) -> bool {
        self.agent.as_ref().is_some_and(Agent::busy)
    }

    /// A reply is on its way.
    pub fn waiting(&self) -> bool {
        self.agent.as_ref().is_some_and(Agent::waiting)
    }

    /// The chosen provider's settings; `None` until it is loaded, which [`open`] and [`send`] do.
    pub fn config(&self) -> Option<&Config> {
        self.agent.as_ref().map(|a| &a.config)
    }
}

/// The ids of the settings fields typed into: changes are saved once none has focus.
pub(crate) const FIELDS: [&str; 3] = ["assistant_model", "assistant_url", "assistant_key"];

/// Whether a provider settings field still owns the keyboard focus.
fn typing(ctx: &egui::Context) -> bool {
    ctx.memory(|m| FIELDS.iter().any(|f| m.has_focus(egui::Id::unique(f))))
}

/// Reads a nonempty preference, including legacy keys awaiting migration.
pub(crate) fn pref(app: &RustGisApp, key: &str) -> Option<String> {
    app.services.load_pref.as_ref().and_then(|l| l(key)).filter(|v| !v.is_empty())
}

/// Saves a preference through the platform hook.
pub(crate) fn save_pref(app: &RustGisApp, key: &str, value: &str) {
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
pub(crate) fn ensure_loaded(app: &mut RustGisApp, ctx: &egui::Context) {
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
pub(crate) fn toggle_settings(app: &mut RustGisApp) {
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
pub(crate) fn sync_skills(app: &mut RustGisApp, force: bool) {
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

/// A saved account and the reason its store or tokens could not be read.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type SignInResult = (Option<chatgpt::Credentials>, Option<rustgis_assistant::Error>);
/// A read under way: the account checked (`None`: whichever is selected) and its result slot.
#[cfg(not(target_arch = "wasm32"))]
type SignInRead = (Option<chatgpt::Binding>, Slot<SignInResult>);

/// Starts reading the selected ChatGPT sign-in on a worker; [`poll_sign_in`] applies it. With
/// `checking`, it only says whether that sign-in is still there (after a failed request).
#[cfg(not(target_arch = "wasm32"))]
fn read_sign_in(app: &mut RustGisApp, checking: Option<chatgpt::Binding>, ctx: &egui::Context) {
    let slot: Slot<SignInResult> = Slot::default();
    let (out, ctx) = (slot.clone(), ctx.clone());
    let spawned = std::thread::Builder::new().name("rustgis-chatgpt-sign-in".into()).spawn(move || {
        let c = chatgpt::load_any_report();
        if let Ok(mut s) = out.lock() {
            *s = Some(c);
        }
        ctx.request_repaint();
    });
    match spawned {
        Ok(_) => app.assistant.sign_in_read = Some((checking, slot)),
        // Not read is not signed out: the sign-in stays as it is, and the refusal is said.
        Err(e) => sign_in_store_error(
            app,
            crate::i18n::fmt("the system refused a thread to read them on ({error})", &[("error", &e.to_string())]).into(),
        ),
    }
}

/// Keeps account-store refusal visible without emitting the same failure on every poll.
#[cfg(not(target_arch = "wasm32"))]
fn sign_in_store_error(app: &mut RustGisApp, error: rustgis_assistant::Error) {
    if !app.assistant.sign_in_store_error_said {
        app.assistant.sign_in_store_error_said = true;
        app.session.warn(crate::i18n::fmt("Could not read saved ChatGPT sign-ins: {reason}", &[("reason", &error.to_string())]));
    }
    app.assistant.login_error = Some(error.auth_context());
}

/// Reads the account list while preserving the same refusal notice as the background read.
#[cfg(not(target_arch = "wasm32"))]
fn saved_accounts(app: &mut RustGisApp) -> Vec<chatgpt::Credentials> {
    match chatgpt::accounts() {
        Ok(accounts) => accounts,
        Err(error) => {
            sign_in_store_error(app, error);
            Vec::new()
        }
    }
}

/// Applies a finished sign-in read, if the sign-in provider is still the one chosen (account
/// actions drop a read begun before them). A check after a failed request never takes another
/// account; when its sign-in is gone (expired, revoked, signed out elsewhere) the key is cleared
/// and the settings open to sign in again. A read taking the selected account goes through
/// [`apply_config`], which stops work bound to another account, and loads the new catalog.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn poll_sign_in(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some((checking, slot)) = app.assistant.sign_in_read.as_ref() else { return };
    let Some(read) = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else { return };
    let checking = checking.clone();
    app.assistant.sign_in_read = None;
    let Some(cfg) = app.assistant.agent.as_ref().map(|a| a.config.clone()).filter(Config::login) else { return };
    let (read, warning) = read;
    if let Some(warning) = warning {
        sign_in_store_error(app, warning);
    }
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
pub(crate) fn save_config(app: &mut RustGisApp, c: &Config, ctx: &egui::Context) {
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
pub(crate) fn poll_keys(app: &mut RustGisApp, ctx: &egui::Context) {
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
pub(crate) fn dispatch(
    app: &mut RustGisApp,
    req: HttpRequest,
    login: Option<chatgpt::Binding>,
    chunks: Option<Chunks>,
    ctx: &egui::Context,
) -> Reply {
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
            crate::i18n::fmt(
                "{url}: only HTTPS addresses, or plain HTTP on this computer or network, are allowed",
                &[("url", &rustgis_engine::sources::address_in_message(&req.url))],
            ),
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
        crate::work_off_frame("rustgis-assistant-request", &slot, move || {
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
pub(crate) fn list_models(app: &mut RustGisApp, cfg: &Config, ctx: &egui::Context) {
    if key_loading(app) {
        app.assistant.catalog_pending = true;
        return;
    }
    app.assistant.catalog_pending = false;
    let req = provider::models_request(cfg);
    app.assistant.models_slot = Some(dispatch(app, req, cfg.login().then(|| cfg.binding()), None, ctx));
}

/// Replaces provider settings and stops work bound to a different provider or sign-in.
pub(crate) fn apply_config(app: &mut RustGisApp, cfg: Config) {
    let another = app
        .assistant
        .config()
        .is_some_and(|c| c.provider != cfg.provider || (c.login() && (c.binding() != cfg.binding() || c.key != cfg.key)));
    if another {
        stop(app);
    }
    if let Some(agent) = app.assistant.agent.as_mut() {
        agent.config = cfg;
    }
}

/// Forgets the request out: its response, the pieces of it and a retry waiting.
fn drop_request(state: &mut AssistantState) {
    state.inflight = None;
    state.chunks = None;
    state.retry = None;
}

/// A ChatGPT account signed in: the assistant uses its plan from now on.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn signed_in(app: &mut RustGisApp, c: &chatgpt::Credentials, ctx: &egui::Context) {
    app.assistant.sign_in_read = None;
    app.assistant.account = Some(c.account());
    app.assistant.login_error = None;
    let saved = saved_accounts(app);
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

/// A response that has landed, taken out of its slot.
pub(crate) fn take(slot: &Reply) -> Option<Result<HttpResponse, rustgis_assistant::Error>> {
    slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
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
    // A command it ran, approved, may have turned its own extension off: nothing more is sent,
    // and the next frame stops the run.
    if !app.session.extensions().is_on(rustgis_engine::extensions::ASSISTANT.id) {
        return;
    }
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

/// The AI Assistant extension was turned off: everything it would still do on its own stops
/// (the run, a message waiting to be sent, the agent threads, a sign-in under way), and the
/// conversation is kept for when it's on again. A chat request already sent may still reach its
/// provider; its reply is dropped. A model list under way is kept and applied when it's on again;
/// a sign-in is cancelled. Says what happened, when there was anything to say.
pub(crate) fn turned_off(app: &mut RustGisApp) {
    let busy = app.assistant.busy();
    #[cfg(not(target_arch = "wasm32"))]
    let threads = app.assistant.threads.count();
    #[cfg(target_arch = "wasm32")]
    let threads = 0;
    // Dropping a sign-in stops its callback server, so it can't finish while off.
    #[cfg(not(target_arch = "wasm32"))]
    let signing_in = app.assistant.login.take().is_some();
    #[cfg(target_arch = "wasm32")]
    let signing_in = false;
    // A message not sent yet goes back to the message box.
    unqueue(&mut app.assistant);
    if let Some(agent) = app.assistant.agent.as_mut() {
        agent.suspend();
    }
    drop_request(&mut app.assistant);
    app.assistant.always = false;
    end_threads(app);
    let chat = app.assistant.agent.as_ref().is_some_and(|a| !a.transcript.is_empty())
        || !app.assistant.input.trim().is_empty()
        || app.assistant.pictures.any();
    let mut said = Vec::new();
    if busy {
        said.push(tl!("The AI Assistant's running request was stopped; what it had already done stays.").to_string());
    }
    if threads > 0 {
        said.push(crate::i18n::fmt("Agent threads stopped: {n}.", &[("n", &threads.to_string())]));
    }
    if signing_in {
        said.push(tl!("The ChatGPT sign-in under way was cancelled.").to_string());
    }
    if chat {
        said.push(tl!("The AI Assistant's conversation is kept until RustGIS closes: turn the extension on again to see it.").to_string());
    }
    // Still running: for agents the user runs, which the extension doesn't cover.
    #[cfg(not(target_arch = "wasm32"))]
    if app.relay.is_some() || app.file_bridge.is_some() {
        said.push(
            tl!("Agents on this computer can still control RustGIS: that isn't part of the extension. Click its status in the status bar to stop it.")
                .to_string(),
        );
    }
    if !said.is_empty() {
        app.session.notify(said.join(" "), false);
    }
}

/// Per-frame: responses that arrived, a message queued by a command, and settings to save.
pub(crate) fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    // While the extension is off nothing is sent or run: what is already on its way (a model
    // list, a picture being read) waits for it to be on again. Keys and settings are still kept.
    let on = app.session.extensions().is_on(rustgis_engine::extensions::ASSISTANT.id);
    let late = std::mem::take(&mut app.assistant.late_pictures);
    if late > 0 {
        let said = crate::i18n::fmt("The AI Assistant is off. Pictures not attached: {n}.", &[("n", &late.to_string())]);
        app.session.notify(said, false);
    }
    #[cfg(not(target_arch = "wasm32"))]
    poll_keys(app, ctx);
    #[cfg(not(target_arch = "wasm32"))]
    if on {
        poll_sign_in(app, ctx);
    }
    if on
        && app.assistant.catalog_pending
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
    if !on {
        return;
    }
    // Pictures read off the frame are taken in here, not only by the desktop composer, so a
    // message waiting for one goes even while no composer is drawn.
    if crate::assistantpictures::take_read(app) {
        ctx.request_repaint_after(web_time::Duration::from_millis(100));
    }
    let from_box = std::mem::take(&mut app.assistant.queued_from_box);
    if let Some(text) = app.assistant.queued.take() {
        send_from(app, text, from_box, ctx);
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
    if let Some(r) = app.assistant.signout.as_ref().and_then(|s| s.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()) {
        app.assistant.signout = None;
        if r.is_err() {
            app.assistant.login_error = Some(
                tl!("Signed out on this computer, but OpenAI didn't confirm it. You can disconnect RustGIS in ChatGPT's settings.").into(),
            );
        }
    }
}

/// Makes the conversation ready to show: the first time, loads the chosen provider (its key is
/// read off the frame: see [`key_loading`]); then reads changed skills and asks for a model list
/// still wanted. Call it on each frame that shows the conversation.
pub fn open(app: &mut RustGisApp, ctx: &egui::Context) {
    ensure_loaded(app, ctx);
    sync_skills(app, false);
    if app.assistant.catalog_pending
        && let Some(cfg) = app.assistant.agent.as_ref().map(|a| a.config.clone())
    {
        list_models(app, &cfg, ctx);
    }
}

/// Sends the user's message, with the pictures attached to it, to the selected provider (loading
/// it the first time), and clears the draft ([`AssistantState::input`]). While the key or a picture
/// is still being read, the message waits in [`AssistantState::queued`] and goes once it is;
/// another sent meanwhile takes its place, the one it replaces going back to the box, and the
/// draft is left as it is until then. Once sent, the draft is cleared only if it still holds that
/// message: text typed while it waited stays. While the assistant is busy it isn't sent: a notice
/// says so, and the message is in the box to send again.
pub fn send(app: &mut RustGisApp, text: String, ctx: &egui::Context) {
    let from_box = text.trim() == app.assistant.input.trim();
    send_from(app, text, from_box, ctx);
}

/// [`send`], told whether `text` is the input box's own draft (`from_box`), which the box keeps
/// until it goes: a message from elsewhere (a suggestion, a command's) is put in the box when it
/// isn't sent, the box's own is already there.
fn send_from(app: &mut RustGisApp, text: String, from_box: bool, ctx: &egui::Context) {
    ensure_loaded(app, ctx);
    // No text and no picture, ready or being read (Enter on an empty box): nothing to send, and
    // nothing waiting is disturbed.
    let pictures = &app.assistant.pictures;
    if text.trim().is_empty() && !pictures.any() && !pictures.reading() {
        return;
    }
    if app.assistant.busy() {
        if !from_box {
            to_box(&mut app.assistant, &text);
        }
        app.session.notify(tl!("The assistant is busy; stop it or wait for it to finish."), true);
        return;
    }
    // A message waits for the key, and for a picture still being read, which goes with it (a
    // suggestion clicked meanwhile, say).
    if key_loading(app) || app.assistant.pictures.reading() {
        unqueue(&mut app.assistant);
        app.assistant.queued = Some(text);
        app.assistant.queued_from_box = from_box;
        return;
    }
    let text = text.trim().to_string();
    if text.is_empty() && !app.assistant.pictures.any() {
        return;
    }
    if !text.is_empty() {
        app.assistant.history.retain(|h| *h != text);
        app.assistant.history.push(text.clone());
    }
    app.assistant.history_pos = None;
    app.assistant.always = false;
    let session = &app.session;
    // `ensure_loaded` gave it an agent; were there none, the message stays to send again.
    let Some(agent) = app.assistant.agent.as_mut() else {
        if !from_box {
            to_box(&mut app.assistant, &text);
        }
        return;
    };
    let step = agent.ask_with(&text, app.assistant.pictures.take(), session);
    if app.assistant.input.trim() == text {
        app.assistant.input.clear();
    }
    follow(app, step, ctx);
}

/// An action waiting for the user's approval.
pub struct Approval {
    /// What would run, in words.
    pub what: String,
    /// Why it is asked about.
    pub why: String,
    /// Whether actions like it may be allowed for the rest of the conversation.
    pub can_always: bool,
    /// The engine's structured card of it, where there is one, taken before it was asked about.
    pub review: Option<rustgis_engine::policy::Review>,
}

/// The action waiting for approval, if one is.
pub fn pending(app: &RustGisApp) -> Option<Approval> {
    let agent = app.assistant.agent.as_ref()?;
    let (what, why, can_always) = agent.pending(&app.session)?;
    Some(Approval { what, why, can_always, review: agent.pending_review().cloned() })
}

/// Answers the [`pending`] action: runs it, or declines it. With `always`, actions like it run
/// without asking for the rest of the conversation, where [`Approval::can_always`] says so.
pub fn decide(app: &mut RustGisApp, allow: bool, always: bool, ctx: &egui::Context) {
    if let Some(step) = with_agent(app, |a, s| a.decide(allow, always, s)) {
        follow(app, step, ctx);
    }
}

/// Stops the run: the request out is forgotten, and calls not yet run are skipped.
pub fn stop(app: &mut RustGisApp) {
    if let Some(a) = app.assistant.agent.as_mut() {
        a.stop();
    }
    drop_request(&mut app.assistant);
}

/// Puts a message still waiting to be sent back into the input box, so it is never lost: as the
/// draft when the box is empty or already holds it, above newer typing otherwise.
pub(crate) fn unqueue(state: &mut AssistantState) {
    let from_box = std::mem::take(&mut state.queued_from_box);
    // The box's own draft is there still, or what the user typed over it; one they cleared
    // comes back.
    if let Some(text) = state.queued.take()
        && (!from_box || state.input.trim().is_empty())
    {
        to_box(state, &text);
    }
}

/// Puts a message that isn't being sent into the input box: as the draft when the box is empty
/// or already holds it, above newer typing otherwise. A message of pictures alone adds no line.
fn to_box(state: &mut AssistantState, text: &str) {
    let (text, draft) = (text.trim(), state.input.trim());
    if text.is_empty() || draft == text {
        return;
    }
    state.input = if draft.is_empty() { text.to_string() } else { format!("{text}\n{}", state.input) };
}

/// Forgets the conversation, and ends the agent threads it started. A message still waiting to
/// be sent ([`AssistantState::queued`]) isn't sent into the new one: it goes back to the box.
pub fn new_conversation(app: &mut RustGisApp) {
    if let Some(a) = app.assistant.agent.as_mut() {
        a.clear();
    }
    unqueue(&mut app.assistant);
    drop_request(&mut app.assistant);
    end_threads(app);
}

/// Summarises the conversation so far, so a long one stays within the model's context. Does
/// nothing while the assistant is busy.
pub fn compact(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.assistant.busy() {
        return;
    }
    if let Some(step) = with_agent(app, |a, _| a.compact()) {
        follow(app, step, ctx);
    }
}

/// Whether the provider's key or sign-in is still being read, which requests wait for. Only an
/// initial keychain read delays them; a known legacy key works during migration.
pub fn key_loading(app: &RustGisApp) -> bool {
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

/// Folds the settings' AI provider section away, or opens it, and remembers which. Like opening
/// or closing the settings, the choice wins over the default still waiting for the key.
pub(crate) fn fold_provider(app: &mut RustGisApp, closed: bool) {
    app.assistant.provider_closed = closed;
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.assistant.settings_pending = None;
    }
    save_pref(app, "assistant_provider_closed", if closed { "on" } else { "" });
}

/// Puts another provider into the settings being edited (`cfg`, which [`apply_settings`] then
/// takes), and drops what was read for the one before.
pub(crate) fn choose_provider(app: &mut RustGisApp, cfg: &mut Config, id: &str, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.assistant.login = None;
    }
    // Switching providers is also a focus change: save the previous key first.
    if app.assistant.unsaved {
        save_config(app, cfg, ctx);
    }
    *cfg = load_config(app, id, ctx);
    app.assistant.models_slot = None;
    app.assistant.models.clear();
    app.assistant.catalog.clear();
    app.assistant.models_error = None;
    app.assistant.login_error = None;
    #[cfg(not(target_arch = "wasm32"))]
    if cfg.login() {
        read_sign_in(app, None, ctx);
    }
}

/// Applies what a settings form made of the provider's settings and of "Remember the key"; `before`
/// is what the form started from. Nothing changes when they are the same.
pub(crate) fn apply_settings(app: &mut RustGisApp, mut cfg: Config, before: (Config, bool), ctx: &egui::Context) {
    if cfg.login() {
        provider::use_catalog(&mut cfg, &app.assistant.catalog, true);
    }
    if (cfg.clone(), app.assistant.remember_key) == before {
        return;
    }
    if app.assistant.remember_key != before.1 {
        save_pref(app, provider::REMEMBER_KEY_PREF, provider::remember_key_value(app.assistant.remember_key));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if !cfg.login() {
        let key_changed = cfg.provider == before.0.provider && cfg.key != before.0.key;
        let enabling = app.assistant.remember_key && !before.1;
        let legacy = pref(app, &format!("assistant_key_{}", cfg.provider));
        let reading = enabling && !key_changed && app.assistant.keys.enable(&cfg.provider, legacy, app.services.secrets.clone(), ctx);
        if key_changed || (app.assistant.remember_key != before.1 && !reading) {
            app.assistant.keys.edit(&cfg.provider, &cfg.key);
            // Forget immediately on an explicit clear/opt-out, even while the key field has focus.
            let opted_out = !app.assistant.remember_key && before.1;
            if opted_out || (key_changed && cfg.key.trim().is_empty()) {
                save_config(app, &cfg, ctx);
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = ctx;
    app.assistant.unsaved = true;
    apply_config(app, cfg);
}

/// The saved ChatGPT accounts, read once and again after a sign-in, sign-out or selection.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn accounts(app: &mut RustGisApp) -> Vec<chatgpt::Credentials> {
    if app.assistant.accounts.is_none() {
        app.assistant.accounts = Some(saved_accounts(app));
    }
    app.assistant.accounts.clone().unwrap_or_default()
}

/// Selects a saved account for the settings being edited, and asks for its models.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn choose_account(app: &mut RustGisApp, cfg: &mut Config, account: &chatgpt::Credentials, ctx: &egui::Context) {
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
                list_models(app, cfg, ctx);
            }
        }
        Err(e) => app.assistant.login_error = Some(e.auth_context()),
    }
}

/// Signs a saved account out and forgets it on this computer, with its saved sign-in.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn remove_account(app: &mut RustGisApp, cfg: &mut Config, client_id: &str, subject: &str, ctx: &egui::Context) {
    match chatgpt::remove(client_id, subject) {
        Ok(revocation) => {
            revoke_later(app, revocation, ctx);
            forget_sign_in(app, cfg);
            cfg.client_id.clear();
            cfg.account.clear();
            cfg.session.clear();
        }
        Err(e) => app.assistant.login_error = Some(e.auth_context()),
    }
}

/// Signs the selected account out; `false` when that failed, which the settings then say.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn sign_out(app: &mut RustGisApp, cfg: &mut Config, ctx: &egui::Context) -> bool {
    match chatgpt::sign_out_selected(&cfg.binding()) {
        Ok(out) => {
            revoke_later(app, out.revocation, ctx);
            forget_sign_in(app, cfg);
            if out.unrevoked.is_some() {
                app.assistant.login_error = Some(
                    tl!("Signed out on this computer, but the system keychain couldn't be read, so OpenAI wasn't asked to end the session. You can disconnect RustGIS in ChatGPT's settings.")
                        .into(),
                );
            }
            true
        }
        Err(e) => {
            app.assistant.login_error = Some(e.auth_context());
            false
        }
    }
}

/// Revokes a signed-out session at OpenAI: network I/O, off the frame.
#[cfg(not(target_arch = "wasm32"))]
fn revoke_later(app: &mut RustGisApp, revocation: Option<chatgpt::Revocation>, ctx: &egui::Context) {
    let Some(r) = revocation else { return };
    let slot: Slot<Result<(), rustgis_assistant::Error>> = Slot::default();
    let (out, ctx) = (slot.clone(), ctx.clone());
    crate::work_off_frame("rustgis-chatgpt-revoke", &slot, move || {
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
pub(crate) fn forget_sign_in(app: &mut RustGisApp, cfg: &mut Config) {
    app.assistant.sign_in_read = None;
    app.assistant.models_slot = None;
    app.assistant.catalog_pending = false;
    unqueue(&mut app.assistant);
    cfg.key.clear();
    app.assistant.account = None;
    app.assistant.models.clear();
    app.assistant.catalog.clear();
    app.assistant.welcome = false;
    app.assistant.accounts = None;
}

/// Starts a ChatGPT sign-in in the browser: for the account chosen in `cfg`, or with `another`,
/// for a new one.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn start_login(app: &mut RustGisApp, cfg: &mut Config, another: bool) {
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

/// A peer's project cannot inherit a pending approval, provider reply or queued user action.
pub(crate) fn peer_replaced(app: &mut RustGisApp) {
    if let Some(agent) = &mut app.assistant.agent {
        agent.project_replaced();
    }
    app.assistant.inflight = None;
    app.assistant.chunks = None;
    // Not sent into the peer's project: back to the box.
    unqueue(&mut app.assistant);
    app.assistant.always = false;
}
