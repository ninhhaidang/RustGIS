//! Plugins: install, list, enable and remove them, and run their commands against the session.
//!
//! A plugin command (`plugin:<plugin>/<command>`) runs through [`Session::run`] like any other:
//! one undo step, restored if it fails. While it runs, the plugin reaches the project through
//! the requests [`Bridge`] answers: RustGIS commands by id, and a few `host.*` requests for
//! reading layers and fetching documents (`host.fetch`, see [`fetch`]). What it may run depends
//! on the permissions it declared.

use std::sync::Arc;

use rustgis_plugins::{Host, Permission, Plugin};
use serde_json::{Value, json};

use super::{b, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

#[path = "plugin_fetch.rs"]
pub mod fetch;

pub fn commands() -> Vec<C> {
    vec![
        C::new("plugin.list", "Plugins", "Plugins › Manage", list).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view(),
        C::new("plugin.install", "Install Plugin", "Plugins › Manage", install).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"path": "a .rgplugin package, or a plugin folder (for development)"}"#),
        C::new("plugin.uninstall", "Uninstall Plugin", "Plugins › Manage", uninstall).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(r#"{"id": plugin id}"#),
        C::new("plugin.enable", "Enable Plugin", "Plugins › Manage", enable).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(r#"{"id": plugin id, "enabled"?: bool}"#),
        C::new("plugin.registry", "Browse Plugins", "Plugins › Manage", browse).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"url"?: "registry.json (default: https://plugins.rustgis.app/registry.json)"}"#),
        C::new("plugin.installFromRegistry", "Install Plugin From Registry", "Plugins › Manage", install_from_registry).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"id": plugin id, "url"?: registry.json}"#),
        C::new("plugin.panel", "Plugin Panel State", "Plugins", panel).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"id": plugin id, "values"?: {widget id: true/false, a whole number, or a tree node's id (null for none)}, "refresh"?: bool (run the panel's state command)}"#,
        ),
        C::new("plugin.prepare", "Prepare Plugin Environment", "Plugins › Manage", prepare).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved))
            .view()
            .params(r#"{"id": plugin id (a process plugin that declares "python": downloads its Python and packages)}"#),
    ]
}

/// A plugin's panel state (the panel's widgets' values and what the plugin said about them):
/// sets values first, then runs the panel's state command with `refresh`. The panel's front
/// end uses it to open the panel and for widgets that run no command.
fn panel(s: &mut Session, p: &Value) -> CmdResult {
    let id = ps(p, "id").ok_or("id is required")?;
    let plugin = rustgis_plugins::get(id).ok_or_else(|| format!("no plugin '{id}'"))?;
    if !plugin.enabled {
        return Err(format!("the plugin {} is disabled", plugin.manifest.name));
    }
    let decl = plugin.manifest.panel.clone().ok_or_else(|| format!("{} has no panel", plugin.manifest.name))?;
    let before = s.panel_state(id, &decl).clone();
    if let Some(values) = p.get("values") {
        s.panel_state(id, &decl).set_values(&decl, values);
    }
    if b(p, "refresh").unwrap_or(false) && !decl.state.is_empty() {
        // A failed refresh leaves the values as they were, as a failed command does.
        if let Err(e) = s.run(&rustgis_plugins::command_id(id, &decl.state), &json!({})) {
            *s.panel_state(id, &decl) = before;
            return Err(e);
        }
    }
    let state = s.plugin_panel(id).ok_or("the panel is unavailable")?;
    serde_json::to_value(state).map_err(|e| e.to_string())
}

/// Makes a Python plugin's environment (it can take minutes: downloads).
fn prepare(_: &mut Session, p: &Value) -> CmdResult {
    let id = ps(p, "id").ok_or("id is required")?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let python = rustgis_plugins::pyenv::prepare(id, &mut |line| log::info!("plugin {id}: {line}"))?;
        Ok(json!({"id": id, "python": python.display().to_string()}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err(format!("{id}: Python environments need the desktop app or rustgis-cli"))
    }
}

/// Loads the installed plugins (desktop and command line); returns what failed to load.
pub fn load_installed() -> Vec<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        rustgis_plugins::store::load_installed()
    }
    #[cfg(target_arch = "wasm32")]
    {
        Vec::new()
    }
}

/// Installs a package's bytes: natively it is copied into the plugins folder too; in the
/// browser it is loaded for this session (the web app keeps the package itself).
pub fn install_bytes(bytes: &[u8]) -> Result<Arc<Plugin>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        rustgis_plugins::store::install_bytes(bytes)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let pkg = rustgis_plugins::Package::from_zip(bytes)?;
        rustgis_plugins::load(pkg, "browser")
    }
}

fn list(_: &mut Session, _: &Value) -> CmdResult {
    #[cfg(not(target_arch = "wasm32"))]
    let folder = rustgis_plugins::store::dir().map(|d| d.display().to_string());
    #[cfg(target_arch = "wasm32")]
    let folder: Option<String> = None;
    Ok(json!({"plugins": rustgis_plugins::list().iter().map(|p| p.info()).collect::<Vec<_>>(), "folder": folder}))
}

fn install(_: &mut Session, p: &Value) -> CmdResult {
    let path = ps(p, "path").map(str::trim).filter(|s| !s.is_empty()).ok_or("path is required")?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let plugin = rustgis_plugins::store::install_path(std::path::Path::new(path))?;
        Ok(plugin.info())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        Err("in the browser, install plugins from the Plugin Manager".into())
    }
}

fn uninstall(_: &mut Session, p: &Value) -> CmdResult {
    let id = ps(p, "id").ok_or("id is required")?;
    #[cfg(not(target_arch = "wasm32"))]
    rustgis_plugins::store::uninstall(id)?;
    #[cfg(target_arch = "wasm32")]
    if !rustgis_plugins::unload(id) {
        return Err(format!("no plugin '{id}'"));
    }
    Ok(json!({"uninstalled": id}))
}

fn enable(_: &mut Session, p: &Value) -> CmdResult {
    let id = ps(p, "id").ok_or("id is required")?;
    let on = b(p, "enabled").unwrap_or(true);
    #[cfg(not(target_arch = "wasm32"))]
    rustgis_plugins::store::set_enabled(id, on)?;
    #[cfg(target_arch = "wasm32")]
    rustgis_plugins::set_enabled(id, on)?;
    Ok(json!({"id": id, "enabled": on}))
}

/// The registry at `url` with each plugin's installed version. Applies its blocklist.
#[cfg(not(target_arch = "wasm32"))]
fn fetch_registry(s: &mut Session, url: &str) -> Result<Vec<rustgis_plugins::registry::Entry>, String> {
    use rustgis_plugins::registry;
    let text = registry::fetch(url, registry::MAX_REGISTRY_BYTES)?;
    let entries = registry::parse(&String::from_utf8_lossy(&text), url)?;
    // A blocklist that can't be fetched or read (one that isn't text included: a replaced byte in
    // an id would match nothing) leaves the list in force, with a warning. Another registry may
    // have no blocklist (it blocks nothing); the hosted one always has one.
    let blocklist = registry::blocklist_url(url);
    let applied = match registry::fetch(&blocklist, 4 * 1024 * 1024) {
        Ok(b) => {
            std::str::from_utf8(&b).map_err(|e| format!("it isn't text: {e}")).and_then(|t| rustgis_plugins::store::save_blocklist(url, t))
        }
        Err(e) if registry::is_default(url) => Err(e),
        Err(_) => Ok(Vec::new()),
    };
    match applied {
        Ok(pulled) if !pulled.is_empty() => s.notify(
            format!("{} plugin(s) were withdrawn from the plugin registry and turned off: {}", pulled.len(), pulled.join(", ")),
            true,
        ),
        Ok(_) => {}
        Err(e) => s.warn(format!("{blocklist}: {e}; the last blocklist read, if any, stays in force")),
    }
    Ok(entries)
}

/// A registry entry with what's installed, for the Plugin Manager and the command line.
pub fn entry_status(e: &rustgis_plugins::registry::Entry) -> Value {
    let installed = rustgis_plugins::get(&e.id).map(|p| p.manifest.version.clone());
    let update = installed.as_deref().is_some_and(|v| rustgis_plugins::manifest::version_cmp(&e.version, v).is_gt());
    let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("installed".into(), json!(installed));
        o.insert("update".into(), json!(update));
        o.insert("blocked".into(), json!(rustgis_plugins::blocked(&e.id, &e.sha256).map(|b| b.reason)));
        // Said for every plugin (the registry leaves it out for WebAssembly ones): a process
        // plugin isn't sandboxed.
        o.insert("runtime".into(), json!(e.runtime));
    }
    v
}

/// `plugin.registry`: the registry's plugins, each with its installed version (see
/// [`fetch_registry`]).
fn browse(s: &mut Session, p: &Value) -> CmdResult {
    let url = ps(p, "url").map(str::trim).filter(|s| !s.is_empty()).unwrap_or(rustgis_plugins::registry::DEFAULT_URL);
    #[cfg(not(target_arch = "wasm32"))]
    {
        Ok(Value::Array(fetch_registry(s, url)?.iter().map(entry_status).collect()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, url);
        Err("in the browser, browse plugins in the Plugin Manager".into())
    }
}

/// Checks a downloaded package against its registry entry, then installs it.
pub fn install_checked(e: &rustgis_plugins::registry::Entry, bytes: &[u8]) -> Result<Arc<Plugin>, String> {
    let got = rustgis_plugins::package::sha256_hex(bytes);
    if !got.eq_ignore_ascii_case(&e.sha256) {
        return Err(format!("{}: the download doesn't match the registry (SHA-256 {got}); it wasn't installed", e.name));
    }
    let pkg = rustgis_plugins::Package::from_zip(bytes)?;
    if pkg.manifest.id != e.id {
        return Err(format!("{}: the package is '{}', not '{}'", e.name, pkg.manifest.id, e.id));
    }
    // A registry that called a program sandboxed (or the other way round) is out of date: what
    // the user was shown before installing must be what runs.
    if pkg.manifest.runtime != e.runtime {
        use rustgis_plugins::manifest::Runtime;
        let runtime = |r| match r {
            Runtime::Wasm => "WebAssembly",
            Runtime::Process => "process",
            Runtime::Skills => "skills",
        };
        return Err(format!(
            "{}: the package is a {} plugin, but the registry lists it as a {} plugin; it wasn't installed",
            e.name,
            runtime(pkg.manifest.runtime),
            runtime(e.runtime)
        ));
    }
    install_bytes(bytes)
}

/// Installs the plugin `id` from the registry, checked against its entry (see [`install_checked`]),
/// after applying the registry's blocklist.
fn install_from_registry(s: &mut Session, p: &Value) -> CmdResult {
    let id = ps(p, "id").ok_or("id is required")?;
    let url = ps(p, "url").map(str::trim).filter(|s| !s.is_empty()).unwrap_or(rustgis_plugins::registry::DEFAULT_URL);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let entries = fetch_registry(s, url)?;
        let e = entries.iter().find(|e| e.id == id).ok_or_else(|| format!("the registry has no plugin '{id}'"))?;
        let bytes = rustgis_plugins::registry::fetch(&e.package, rustgis_plugins::package::MAX_PACKAGE)?;
        Ok(install_checked(e, &bytes)?.info())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, id, url);
        Err("in the browser, install plugins from the Plugin Manager".into())
    }
}

/// A `'static` copy of a label (undo entries keep `&'static str`), shared between calls.
pub(crate) fn leak(s: &str) -> &'static str {
    static STRINGS: std::sync::Mutex<Option<std::collections::HashSet<&'static str>>> = std::sync::Mutex::new(None);
    let Ok(mut set) = STRINGS.lock() else { return Box::leak(s.to_string().into_boxed_str()) };
    let set = set.get_or_insert_with(Default::default);
    if let Some(&have) = set.get(s) {
        return have;
    }
    let new: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(new);
    new
}

/// Parameter locations shared with the log's reach heuristic.
pub(crate) const PLACE_KEYS: &[&str] = crate::policy::PLACE_KEYS;
pub const SQL: &str = "layer.addSql";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Files,
    Network,
    Plugin(&'static str),
}
/// Compatibility projection of the engine's additive declarations.
pub fn user_only(id: &str) -> bool {
    use crate::policy::Effect;
    id.starts_with("plugin.")
        || crate::cmd::all().iter().find(|c| c.id == id).is_some_and(|c| {
            [Effect::Automation, Effect::Reserved, Effect::Replaces, Effect::History].into_iter().any(|e| c.effects.contains(e))
        })
}
pub fn reaches(spec: &C, params: &Value) -> Option<Reach> {
    use crate::policy::Effect;
    let e = crate::policy::command_effects(spec, params);
    if e.contains(Effect::Plugin) {
        return ps(params, "tool").and_then(super::gp::resolve_tool).map(|t| Reach::Plugin(t.id));
    }
    if e.contains(Effect::Files) {
        Some(Reach::Files)
    } else if e.contains(Effect::Network) {
        Some(Reach::Network)
    } else {
        None
    }
}

/// Answers a running plugin's requests.
struct Bridge<'a> {
    s: &'a mut Session,
    plugin: Arc<Plugin>,
    /// What this call fetched (`host.fetch`).
    fetches: fetch::Calls,
    /// Whether a request could have changed the project (a command that mutates, a layer added).
    changed: bool,
    /// Whether a command left a request for the front end instead ([`Session::request_url`]).
    requested: bool,
}

impl Bridge<'_> {
    fn need(&self, p: Permission, what: &str) -> Result<(), String> {
        if p == Permission::Edit {
            return self.s.host_effect(crate::policy::Effects::NONE.with(crate::policy::Effect::Edits), "host.addLayer");
        }
        if p == Permission::Network {
            return self.s.host_effect(crate::policy::Effects::NONE.with(crate::policy::Effect::Network), "host.fetch");
        }
        if self.plugin.manifest.has(p) {
            Ok(())
        } else {
            Err(format!("{} needs the '{}' permission to {what}", self.plugin.manifest.name, p.code()))
        }
    }

    fn layers(&self) -> Value {
        let active = self.s.active_layer;
        Value::Array(
            self.s
                .map()
                .layers
                .iter()
                .map(|l| {
                    json!({
                        "id": l.id, "name": l.name, "raster": l.is_raster(), "visible": l.visible,
                        "geometry": format!("{:?}", l.dataset.geometry_type), "features": l.dataset.features.len(),
                        "selected": l.selection.len(), "active": Some(l.id) == active,
                        "fields": l.dataset.fields.iter().map(|f| json!({"name": f.name, "type": format!("{:?}", f.ty)})).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }

    /// A layer's features as GeoJSON: all of them, or its selection (`"selected": true`), at
    /// most `limit`.
    fn features(&self, p: &Value) -> CmdResult {
        let id = self.s.layer_id(p)?;
        let l = self.s.map().layer(id).ok_or("no such layer")?;
        if l.is_raster() {
            return Err(format!("{} is a raster layer", l.name));
        }
        let limit = uint(p, "limit").and_then(|n| usize::try_from(n).ok()).unwrap_or(usize::MAX);
        let rows: Vec<usize> = if b(p, "selected") == Some(true) { l.selection.clone() } else { (0..l.dataset.features.len()).collect() };
        let ds = rustgis_data::Dataset {
            name: l.name.clone(),
            geometry_type: l.dataset.geometry_type,
            fields: l.dataset.fields.clone(),
            features: rows.iter().take(limit).filter_map(|&i| l.dataset.features.get(i).cloned()).collect(),
            source: None,
            crs: None,
            crs_incomplete: false,
            zm_dropped: false,
            windowed: false,
        };
        let mut gj = rustgis_data::io::to_geojson(&ds);
        // The row of each feature, for selecting or editing it afterwards.
        if let Some(fs) = gj.get_mut("features").and_then(Value::as_array_mut) {
            for (f, row) in fs.iter_mut().zip(rows.iter()) {
                if let Some(o) = f.as_object_mut() {
                    o.insert("row".into(), json!(row));
                }
            }
        }
        Ok(gj)
    }

    fn add_layer(&mut self, p: &Value) -> CmdResult {
        self.need(Permission::Edit, "add layers")?;
        let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&self.plugin.manifest.name).to_string();
        let gj = p.get("geojson").ok_or("geojson is required")?;
        let (ds, report) = rustgis_data::io::read_geojson(&gj.to_string(), &name).map_err(|e| e.to_string())?;
        self.changed = true;
        let id = super::layer::add_dataset(self.s, ds);
        Ok(json!({"layer": id, "skipped": report.skipped}))
    }

    fn command(&mut self, id: &str, params: &Value) -> CmdResult {
        if id.starts_with(rustgis_plugins::PREFIX) {
            return Err("plugins can't run other plugins' commands".into());
        }
        // Routing receipts belong to engine-issued downloads, never guest JSON.
        let unnumbered;
        let params = if matches!(id, "layer.addUrl" | "layer.addDownload") && params.get("request").is_some() {
            unnumbered = fetch::without_request(params);
            &unnumbered
        } else {
            params
        };
        if let Some(connection) = params.get("connection").and_then(Value::as_str).map(str::trim)
            && self.s.connections.connections.iter().any(|c| c.id == connection && crate::service_logins::is_service(c))
        {
            return Err(format!("plugins can't use a saved web-service connection's login ('{id}')"));
        }
        let spec = self.s.registry.get(id).ok_or_else(|| format!("unknown command '{id}'"))?;
        let (enabled, label, mutates) = (spec.enabled, spec.label, spec.mutates);
        let named = params.get("layer").and_then(|_| self.s.layer_id(params).ok());
        let active = self.s.active_layer;
        if named.is_some() {
            self.s.active_layer = named;
        }
        let blocked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| enabled(self.s))).unwrap_or(Some("unavailable"));
        self.s.active_layer = active;
        if let Some(why) = blocked {
            return Err(format!("{label}: {why}"));
        }
        // Without `network`, no command may bring in a tile address (a map file read from text
        // carries its tile layers), or change where an existing layer fetches from: any such
        // layer is taken out again. Where a layer fetches from is decided by its address, kind,
        // subdomains and row order. Its name, attribution and opacity never reach a server, and
        // its tile size reaches one only as `{width}`/`{height}` (256 or 512).
        let fetched = |t: &rustgis_map::TileLayer| (t.url.clone(), t.kind, t.subdomains.clone(), t.tms);
        let tile_sources =
            |s: &Session| -> std::collections::HashSet<_> { s.project.maps.iter().flat_map(|m| &m.tiles).map(fetched).collect() };
        let before = (!self.plugin.manifest.has(Permission::Network)).then(|| tile_sources(self.s));
        // The plugin command took the undo snapshot: the commands it runs are part of it. A
        // command that only left a request for the front end ([`Session::request_url`]) changed
        // nothing; a later one must not erase the undo step for an earlier edit.
        self.s.request_only = false;
        let mut res = crate::service_logins::anonymously(self.s, true, |s| {
            let op = s.authorize(id, params)?;
            super::workflows::steps_of(s, |s| s.invoke(op, crate::policy::Txn::Shared))
        })
        .unwrap_or_else(|_| Err(format!("{label} failed unexpectedly")));
        let requested = std::mem::take(&mut self.s.request_only);
        self.requested |= requested;
        self.changed |= mutates && !requested;
        if let Some(before) = before {
            let mut dropped = false;
            for m in &mut self.s.project.maps {
                let n = m.tiles.len();
                m.tiles.retain(|t| before.contains(&fetched(t)));
                dropped |= m.tiles.len() != n;
            }
            if dropped {
                res = Err(format!("{} needs the 'network' permission to add tile layers", self.plugin.manifest.name));
            }
        }
        self.s.fix_active_layer();
        res
    }
}

impl Host for Bridge<'_> {
    fn call(&mut self, op: &str, params: &Value) -> Result<Value, String> {
        match op {
            "host.info" => Ok(json!({"rustgis": rustgis_plugins::manifest::RUSTGIS_VERSION, "abi": rustgis_plugins::manifest::ABI,
                "plugin": self.plugin.manifest.id, "permissions": self.plugin.manifest.permissions})),
            "host.layers" => Ok(self.layers()),
            "host.features" => self.features(params),
            "host.view" => {
                let c = &self.s.map().camera;
                let center = c.center_lonlat();
                Ok(json!({"center": [center.x, center.y], "scale": c.scale(), "projection": self.s.map().projection.code()}))
            }
            "host.notify" => {
                let text: String = ps(params, "text").unwrap_or_default().chars().take(500).collect();
                let error = b(params, "error").unwrap_or(false);
                self.s.notify(format!("{}: {text}", self.plugin.manifest.name), error);
                Ok(Value::Null)
            }
            "host.addLayer" => self.add_layer(params),
            "host.fetch" => {
                // The address, and what the plugin answers, can carry what it read from the project.
                self.need(Permission::Network, "fetch from the web")?;
                let c = self.s.continuation()?;
                let still = Arc::new(move || c.check().is_ok());
                crate::render::rustgis_tiles::cache::with_still(still, || self.fetches.fetch(params))
            }
            _ if op.starts_with("host.") => Err(format!("unknown request '{op}'")),
            _ => self.command(op, params),
        }
    }

    fn log(&mut self, text: &str) {
        log::info!("{}: {text}", self.plugin.manifest.id);
    }
}

impl Session {
    /// Runs a plugin command (`plugin:<plugin>/<command>`) as one undoable step.
    pub(crate) fn run_plugin(&mut self, op: crate::policy::Authorized<crate::policy::CommandOp>) -> CmdResult {
        let crate::policy::CommandOp::Plugin { cmd, pin, params } = op.op() else { return Err("not a plugin command".into()) };
        let cmd = cmd.clone();
        if !rustgis_plugins::is_current(pin.plugin()) {
            return Err("the plugin was disabled, updated or removed".into());
        }
        let params = params.clone();
        let params = &params;
        let id = rustgis_plugins::command_id(&cmd.plugin.manifest.id, &cmd.decl.id);
        let id = id.as_str();
        let label = leak(&cmd.decl.label);
        let mutates = cmd.mutates();
        let plugin = cmd.plugin.clone();
        // A plugin with a panel gets its values with every command (`panel` in the request),
        // after the ones `params` sets (`{"panel": {"id": value}}`, which is taken out of them),
        // and may answer the panel's new state. A failed command leaves the values as they were.
        let pid = plugin.manifest.id.clone();
        let panel = plugin.manifest.panel.clone();
        let given = params;
        let mut params = params;
        let mut stripped;
        let (before, request) = match &panel {
            Some(decl) => {
                let state = self.panel_state(&pid, decl);
                let before = state.clone();
                let mut changed = Vec::new();
                if let Some(given) = params.get("panel") {
                    changed = state.set_values(decl, given);
                    stripped = params.clone();
                    if let Some(o) = stripped.as_object_mut() {
                        o.remove("panel");
                    }
                    params = &stripped;
                }
                (Some(before), Some(state.request(&changed)))
            }
            None => (None, None),
        };
        // The panel's state command runs each time the panel opens: when nothing it asked for
        // could change the project, it is no undo step. Other commands keep theirs.
        let state_command = panel.as_ref().is_some_and(|d| !d.state.is_empty() && d.state == cmd.decl.id);
        let mut pending = Vec::new();
        let mut requested = Vec::new();
        let mut suspended = Vec::new();
        let queued_before = self.queued_authority();
        // The downloads this call leaves to the app: its own, apart from a calling command's.
        let outer = self.call_requests.replace(Vec::new());
        let mark = self.notice_mark();
        let res = self.guarded(label, mutates, |s| {
            let fetches = fetch::Calls::for_plugin(&plugin.manifest.id);
            let mut bridge = Bridge { s, plugin, fetches, changed: false, requested: false };
            let res = rustgis_plugins::run_command_with_panel(&cmd, &mut bridge, params, request);
            let asked = bridge.s.call_requests.as_mut().map(std::mem::take).unwrap_or_default();
            if res.is_ok() && (!bridge.fetches.pending.is_empty() || !asked.is_empty()) {
                // Waiting for a fetch (the browser) or a download left to the app: whatever the
                // call did is undone, quietly, and the command runs again from the start once
                // the answers are in and the downloads added (#1202).
                pending = std::mem::take(&mut bridge.fetches.pending);
                requested = asked;
                // Retain the exact engine-issued requests only for successful suspension.
                // A genuine guest error/panic keeps guarded's ordinary retirement behavior.
                suspended = bridge.s.suspend_plugin_requests(&queued_before, &requested);
                bridge.s.quiet_failure = true;
                return Err(String::new());
            }
            // Nor is a command that only left downloads for the front end: they add their
            // layers, each its own undo step, when they land.
            bridge.s.request_only = res.is_ok() && (state_command || bridge.requested) && !bridge.changed;
            res
        });
        if let Some(decl) = &panel {
            match (&res, self.plugin_panels.get_mut(&pid)) {
                (Ok(answer), Some(state)) => {
                    if let Some(update) = answer.get("panel") {
                        state.apply(decl, update);
                    }
                }
                (Err(_), Some(state)) => {
                    if let Some(before) = before {
                        *state = before;
                    }
                }
                _ => {}
            }
        }
        self.call_requests = outer;
        if pending.is_empty() && requested.is_empty() {
            return res;
        }
        // Restore only the original opaque authorities removed by intentional suspension.
        // No JSON request can create a fresh User admission after the rollback.
        let suspended_keys: Vec<u64> = suspended.iter().map(|q| q.key).collect();
        let giving_up = self.retry_tries >= fetch::MAX_RETRIES;
        if !giving_up {
            self.restore_plugin_requests(suspended)?;
        }
        // A service's document is read here, once the run is kept: the app adds nothing for it,
        // and with no command waiting nothing would take it.
        let documents: Vec<(u64, Value)> =
            requested.iter().filter(|(n, asked)| !giving_up && fetch::is_document(asked) && !self.url_waits.has(*n)).cloned().collect();
        // The errors this run said stay: the next run, doing the same again, doesn't repeat them.
        let earlier = self.rerun_said.len();
        let said: Vec<String> = self.notices_since(mark).iter().filter(|n| n.error).map(|n| n.text.clone()).collect();
        self.rerun_said.extend(said);
        // The panel's values went back with the failed call: the run again sets them again.
        let res = self.retry_plugin(id, given, pending, requested);
        self.rerun_said.truncate(earlier);
        if let Err(e) = &res {
            self.discard_plugin_requests(&suspended_keys);
            self.notify(e.clone(), true);
        } else {
            for (n, asked) in &documents {
                self.url_waits.read_document(*n, asked);
            }
        }
        res
    }

    /// A plugin's panel state, made from its manifest the first time.
    pub fn plugin_panel(&mut self, plugin: &str) -> Option<&rustgis_plugins::PanelState> {
        // A plugin uninstalled, disabled or updated without a panel leaves no state behind.
        let Some(p) = rustgis_plugins::get(plugin).filter(|p| p.enabled && p.manifest.panel.is_some()) else {
            self.plugin_panels.remove(plugin);
            return None;
        };
        let decl = p.manifest.panel.as_ref()?;
        Some(self.panel_state(plugin, decl))
    }

    /// The kept state of `plugin`'s panel `decl`: made from the declaration the first time, and
    /// made again when the plugin was updated with other widgets since.
    fn panel_state(&mut self, plugin: &str, decl: &rustgis_plugins::PanelDecl) -> &mut rustgis_plugins::PanelState {
        let state = self.plugin_panels.entry(plugin.to_string()).or_insert_with(|| rustgis_plugins::PanelState::new(decl));
        if !state.matches(decl) {
            *state = rustgis_plugins::PanelState::new(decl);
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that only left a download for the front end changed nothing, and a later one
    /// doesn't hide an earlier edit: the plugin's command keeps its undo step.
    #[test]
    fn a_request_after_an_edit_keeps_the_edit() {
        let pkg = rustgis_plugins::Package::from_zip(&rustgis_plugins::testing::package("eng-bridge", &["edit", "network"])).unwrap();
        rustgis_plugins::load(pkg, "test").unwrap();
        let plugin = rustgis_plugins::get("eng-bridge").unwrap();
        let mut s = Session::new(crate::Project::default());
        s.leave_downloads = true;
        let fetches = fetch::Calls::for_plugin("eng-bridge");
        let mut bridge = Bridge { s: &mut s, plugin, fetches, changed: false, requested: false };
        bridge.call("layer.addUrl", &json!({"url": "https://x.org/a.geojson"})).unwrap();
        assert!(bridge.requested && !bridge.changed, "a request alone changes nothing");
        bridge.call("map.new", &json!({"name": "B"})).unwrap();
        bridge.call("layer.addUrl", &json!({"url": "https://x.org/b.geojson"})).unwrap();
        assert!(bridge.requested && bridge.changed, "the edit is still one");
        assert!(!bridge.s.request_only, "the bridge took the flag");
        assert_eq!(s.take_url_requests().len(), 2);
        rustgis_plugins::unload("eng-bridge");
    }

    /// A plugin can't choose a download's number: the app gives it, so one plugin can't land or
    /// share another command's wait (#1202).
    #[test]
    fn a_plugin_cant_choose_a_downloads_number() {
        let pkg = rustgis_plugins::Package::from_zip(&rustgis_plugins::testing::package("eng-number", &["edit", "network"])).unwrap();
        rustgis_plugins::load(pkg, "test").unwrap();
        let mut s = Session::new(crate::Project::default());
        s.run_leaving_downloads(
            "plugin:eng-number/f",
            &json!({
                "op":"layer.addUrl", "params":{"url":"https://x.org/a.geojson", "request":99}
            }),
        )
        .unwrap();
        let taken = s.take_url_requests();
        assert_eq!(taken.len(), 1);
        assert_ne!(taken[0].params()["request"], json!(99));
        rustgis_plugins::unload("eng-number");
    }

    /// A command run again answers one landed download as often as it asks for it in that run:
    /// the same address asked twice isn't downloaded again (#1202).
    #[test]
    fn a_rerun_answers_a_download_asked_twice() {
        let mut s = Session::new(crate::sample::world_project());
        let l7 = u64::from(s.map().layers[0].id);
        s.leave_downloads = true;
        let asked = json!({"url": "https://x.org/a.geojson"});
        s.rerun_landed = vec![(asked.clone(), Ok(json!({"layer": l7})), Default::default())];
        for _ in 0..2 {
            assert_eq!(s.run("layer.addUrl", &asked).unwrap(), json!({"layer": l7}));
        }
        assert!(s.take_url_requests().is_empty(), "nothing asked again");
        // Asked with spaces around the address (kept trimmed): the same answer.
        let padded = json!({"url": "  https://x.org/a.geojson "});
        assert_eq!(s.run("layer.addUrl", &padded).unwrap(), json!({"layer": l7}));
        assert!(s.take_url_requests().is_empty(), "nothing asked again");
        // A layer undone while the command waited: said, not named.
        s.rerun_landed = vec![(asked.clone(), Ok(json!({"layer": 4_000_000})), Default::default())];
        let e = s.run("layer.addUrl", &asked).unwrap_err();
        assert!(e.contains("removed or undone"), "{e}");
    }
}
