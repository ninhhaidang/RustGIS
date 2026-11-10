use super::{
    Effect, Effects,
    decide::{self, Decision},
    scope::{Owner, Scope},
};
use crate::{CommandSpec, Session};
use serde_json::{Value, json};
use std::sync::Arc;

/// Whether a parameter is the address of a file on an Earthdata data host. Walks with a list, not
/// by recursion: parameters are hostile.
fn names_earthdata_host(params: &Value) -> bool {
    let mut left = vec![params];
    while let Some(value) = left.pop() {
        match value {
            Value::String(text) if crate::render::rustgis_tiles::earthdata::is_data_host(text) => return true,
            Value::Array(items) => left.extend(items),
            Value::Object(fields) => left.extend(fields.values()),
            _ => {}
        }
    }
    false
}

pub struct Call {
    pub(super) kind: CallKind,
    pub(super) request: String,
}
pub(super) enum CallKind {
    Command { id: String, params: Value },
    Script(String),
    Adapter { what: &'static str, effects: Effects, params: Value },
}
impl Call {
    /// Owns the raw command request for later resolution; output redactions never become execution data.
    pub fn command(id: &str, params: Value) -> Self {
        Self { kind: CallKind::Command { id: id.into(), params }, request: String::new() }
    }
    /// Uses gp.run as the canonical vehicle for the raw tool request.
    pub fn tool(tool: &str, params: Value) -> Self {
        Self::command("gp.run", json!({"tool": tool, "params": params}))
    }
    /// Owns script text for later bounded expansion by the execution parser.
    pub fn script(text: &str) -> Self {
        Self { kind: CallKind::Script(text.into()), request: String::new() }
    }
    /// Captures the trusted adapter's exact effect and destination for review.
    pub fn adapter(what: &'static str, effects: Effects, params: Value) -> Self {
        Self { kind: CallKind::Adapter { what, effects, params }, request: String::new() }
    }
    /// Binds the trusted adapter's queued request id, independently of remembered operations.
    pub fn with_request(mut self, request: &str) -> Self {
        self.request = request.into();
        self
    }
}
/// Sanitized effective selectors, for presentation only; raw targets remain private.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct ReviewDetail {
    pub operation: String,
    pub targets: Vec<String>,
    pub places: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Review {
    pub what: String,
    pub why: String,
    pub steps: Vec<String>,
    pub remember: bool,
    /// Sanitized actual targets and destinations, counted in the full-card bound.
    pub details: Vec<ReviewDetail>,
    /// Project-only edit count; presentation layers supply their own translated wording.
    pub edits: usize,
    /// Sanitized initial active/selected names, separate from execution identity.
    pub defaults: Option<(Option<String>, Vec<String>)>,
    /// Stable explanation template and sanitized substitutions for translated cards.
    pub explanation: (&'static str, Vec<(&'static str, String)>),
    /// Script line numbers and command ids needing review, for translated reason prefixes.
    pub lines: Vec<(usize, String)>,
}
pub(super) const MAX_NODES: usize = 1_000;
const MAX_DEPTH: usize = 4;
const MAX_REVIEW: usize = 4_096;
const MAX_PLAN_BYTES: usize = 64 * 1024;

/// Measures serialized shape without allocating a second unbounded copy of hostile content.
fn bounded_size(value: &impl serde::Serialize, limit: usize) -> Result<usize, String> {
    struct Size {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Size {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let size = self.bytes.saturating_add(bytes.len());
            if size > self.limit {
                return Err(std::io::Error::other("plan size bound"));
            }
            self.bytes = size;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut size = Size { bytes: 0, limit };
    serde_json::to_writer(&mut size, value)
        .map_err(|_| "this operation is too large to freeze for review; run it yourself or request fewer steps".to_string())?;
    Ok(size.bytes)
}
#[derive(Clone, PartialEq)]
pub(super) struct Plan {
    pub owner: Owner,
    pub opened: u64,
    pub map: u64,
    pub designated: Vec<LayerKey>,
    pub last_table: u32,
    pub picked: Option<(Option<u32>, Vec<u32>)>,
    pub names: Vec<(String, u32)>,
    pub sources: Vec<(LayerKey, Value)>,
    pub accounts: Value,
    pub s3: Vec<crate::render::rustgis_tiles::s3::Connection>,
    pub root: Node,
}
#[derive(Clone)]
pub(crate) struct PluginPin(pub(super) Arc<rustgis_plugins::Plugin>);
impl PluginPin {
    /// Borrows the exact plugin load already held by the pin.
    pub(crate) fn plugin(&self) -> &Arc<rustgis_plugins::Plugin> {
        &self.0
    }
}
impl PartialEq for PluginPin {
    /// Compares raw operation identity, including exact pinned loads; no digest grants consent.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
#[derive(Clone)]
pub(crate) struct ToolPin {
    pub(super) resolved: rustgis_analysis::ResolvedTool,
    pub(super) plugin: Option<PluginPin>,
    pub(super) runner: Value,
    #[cfg(feature = "geolibre")]
    suite: Option<rustgis_geolibre::ResolvedTool>,
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    native_runner: Option<rustgis_geolibre::runner::ResolvedRunner>,
}
impl PartialEq for ToolPin {
    /// Compares raw operation identity, including exact pinned loads; no digest grants consent.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.resolved.spec(), other.resolved.spec())
            && self.resolved.source() == other.resolved.source()
            && self.plugin == other.plugin
            && self.runner == other.runner
    }
}
impl ToolPin {
    /// Returns the immutable captured schema used for planning and execution.
    pub(crate) fn spec(&self) -> &'static rustgis_analysis::ToolSpec {
        self.resolved.spec()
    }
    /// Returns the immutable analysis schema/runner pair, without another name lookup.
    pub(crate) fn resolved(&self) -> &rustgis_analysis::ResolvedTool {
        &self.resolved
    }
    /// Borrows the exact plugin load already held by the pin.
    pub(crate) fn plugin(&self) -> Option<&PluginPin> {
        self.plugin.as_ref()
    }
    /// Rejects a withdrawn load or reloaded schema before a new stage begins.
    pub(crate) fn current(&self) -> Result<(), String> {
        if self.plugin.is_none() && !self.resolved.is_current() {
            return Err("the tool schema was reloaded since this run was approved".into());
        }
        #[cfg(feature = "geolibre")]
        if let Some(suite) = &self.suite {
            suite.check()?;
        }
        #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
        if let Some(runner) = &self.native_runner {
            runner.check()?;
        }
        if self.plugin.as_ref().is_some_and(|p| !rustgis_plugins::is_current(&p.0)) {
            return Err("the plugin was disabled, updated or removed since this run was approved".into());
        }
        if let Some(pin) = &self.plugin {
            rustgis_plugins::tools::matches_pinned(&pin.0, self.spec())?;
        }
        if self.resolved.source() == Some("geolibre") && self.runner != crate::cmd::gp::geolibre_status() {
            return Err("the GeoLibre runner changed since this run was approved".into());
        }
        Ok(())
    }
    /// Executes the captured runner, including the pinned native suite path and manifest metadata.
    pub(crate) fn run(
        &self,
        inputs: &dyn rustgis_analysis::Inputs,
        params: &Value,
        control: &Arc<rustgis_analysis::control::Control>,
        live: &dyn Fn() -> Result<(), String>,
    ) -> Result<rustgis_analysis::ToolResult, String> {
        self.current()?;
        live()?;
        #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
        if let Some(suite) = &self.suite {
            let runner = self.native_runner.as_ref().ok_or("the GeoLibre runner isn't installed")?;
            return rustgis_analysis::run_under(self.spec().name, control, || {
                rustgis_geolibre::run_pinned(suite, inputs, params, runner, live)
            });
        }
        rustgis_analysis::run_resolved(self.resolved(), inputs, params, control)
    }
    /// Builds browser argv from captured suite metadata after checking its load.
    #[cfg(feature = "geolibre")]
    pub(crate) fn prepare_suite(&self, inputs: &dyn rustgis_analysis::Inputs, params: &Value) -> Result<rustgis_geolibre::Job, String> {
        self.current()?;
        self.suite.as_ref().ok_or("not a pinned GeoLibre tool")?.prepare(inputs, params)
    }
    /// Resolves once, pairing plugin schema with its exact manifest owner before inputs are read.
    pub(super) fn resolve(name: &str) -> Result<Self, String> {
        let resolved = rustgis_analysis::resolve_published(name)
            .ok_or_else(|| format!("no tool '{name}' is published; locate or reload external tools from the native app first"))?;
        let plugin = if resolved.source() == Some(rustgis_plugins::tools::SOURCE) {
            let (id, _) = rustgis_plugins::split_id(resolved.spec().id).ok_or("invalid plugin tool id")?;
            Some(PluginPin(rustgis_plugins::get(id).filter(|p| p.enabled).ok_or("the plugin is disabled or no longer loaded")?))
        } else {
            None
        };
        if let Some(pin) = &plugin {
            rustgis_plugins::tools::matches_pinned(&pin.0, resolved.spec())?;
        }
        let runner = if resolved.source() == Some("geolibre") { crate::cmd::gp::geolibre_status() } else { Value::Null };
        #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
        let native_runner =
            if resolved.source() == Some("geolibre") { rustgis_geolibre::runner::ResolvedRunner::capture().ok() } else { None };
        #[cfg(feature = "geolibre")]
        let suite =
            if resolved.source() == Some("geolibre") { Some(rustgis_geolibre::ResolvedTool::capture(resolved.spec())?) } else { None };
        Ok(Self {
            resolved,
            plugin,
            runner,
            #[cfg(feature = "geolibre")]
            suite,
            #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
            native_runner,
        })
    }
}
#[derive(Clone)]
pub(super) enum Op {
    Command(CommandSpec),
    Plugin(rustgis_plugins::PluginCommand, PluginPin),
    Tool(ToolPin),
    Adapter(&'static str),
}
impl PartialEq for Op {
    /// Compares raw operation identity, including exact pinned loads; no digest grants consent.
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Self::Command(a), Self::Command(b)) => {
                a.id == b.id
                    && a.label == b.label
                    && a.location == b.location
                    && a.shortcut == b.shortcut
                    && a.params == b.params
                    && a.mutates == b.mutates
                    && a.effects == b.effects
                    && match (a.inputs, b.inputs) {
                        (Some(a), Some(b)) => std::ptr::fn_addr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    }
                    && match (a.transition, b.transition) {
                        (Some(a), Some(b)) => std::ptr::fn_addr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    }
                    && a.run.same(b.run)
                    && std::ptr::fn_addr_eq(a.enabled, b.enabled)
            }
            (Self::Plugin(a, p), Self::Plugin(b, q)) => a.decl.id == b.decl.id && p == q,
            (Self::Tool(a), Self::Tool(b)) => a == b,
            (Self::Adapter(a), Self::Adapter(b)) => a == b,
            _ => false,
        }
    }
}
#[derive(Clone, PartialEq)]
pub(super) struct Wire {
    pub key: String,
    pub from: Vec<OutputRef>,
}
#[derive(Clone, PartialEq)]
pub(super) enum OutputRef {
    ExistingLayer(LayerKey),
    ExistingTable(u32),
    Node(u16),
}
#[derive(Clone, PartialEq)]
pub(crate) struct Node {
    pub(super) context: u64,
    pub(super) line: Option<usize>,
    pub(super) op: Op,
    pub(super) params: Value,
    pub(super) effects: Effects,
    pub(super) targets: Vec<Target>,
    pub(super) places: Vec<Value>,
    pub(super) display: ReviewDetail,
    pub(super) children: Vec<Node>,
    pub(super) item: Option<(&'static str, String, bool, Value)>,
    pub(super) generated: bool,
    pub(super) number: u16,
    pub(super) wires: Vec<Wire>,
    pub(super) graph_output: bool,
    pub(super) fixed_tool: Option<ToolPin>,
    pub(super) auth: Option<Arc<super::scope::RuntimeAccounts>>,
}
/// An in-memory layer incarnation qualified by its owning map; numeric ids repeat across maps.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct LayerKey {
    pub map: u64,
    pub id: u32,
    pub instance: u64,
}
impl LayerKey {
    /// Qualifies a layer with its owning map and in-memory incarnation.
    pub fn of(map: &rustgis_map::Map, layer: &rustgis_map::Layer) -> Self {
        Self { map: map.instance_id(), id: layer.id, instance: layer.instance_id() }
    }
    /// Finds that exact incarnation without consulting the active map or a name.
    pub fn get(self, s: &Session) -> Option<&rustgis_map::Layer> {
        s.project
            .maps
            .iter()
            .find(|m| m.instance_id() == self.map)
            .and_then(|m| m.layer(self.id))
            .filter(|l| l.instance_id() == self.instance)
    }
}
#[derive(Clone, PartialEq)]
pub(super) enum Target {
    Default { active: Option<LayerKey>, selected: Vec<LayerKey> },
    Layer { key: LayerKey, source: Option<Value> },
    Table { id: u32, source: Option<Value> },
    MadeLayer { map: u64, name: String },
    MadeTable(String),
    ProducedDataset { producer: u16, map: u64, name: String },
    Map(u64),
    LayerInMap { map: u64, id: u32, instance: u64, source: Option<Value> },
    TimeSources(Vec<(u64, u32, String)>),
}
impl Node {
    /// The captured operation's stable id, used for dispatch checks and sanitized review.
    pub(crate) fn id(&self) -> &str {
        match &self.op {
            Op::Command(spec) => spec.id,
            Op::Plugin(c, _) => &c.id,
            Op::Tool(pin) => pin.spec().id,
            Op::Adapter(what) => what,
        }
    }
    /// Rejects a withdrawn load or reloaded schema before a new stage begins.
    pub(crate) fn current(&self) -> Result<(), String> {
        if self.auth.as_ref().is_some_and(|a| a.restored_since()) {
            return Err("the Earthdata token remembered on this computer was read while this command ran; run it again".into());
        }
        if self.auth.as_ref().is_some_and(|a| !a.is_current()) {
            return Err("the active account changed since this operation was approved".into());
        }
        if let Some(pin) = &self.fixed_tool {
            pin.current()?;
        }
        match &self.op {
            Op::Tool(t) => t.current(),
            Op::Plugin(_, p) if !rustgis_plugins::is_current(&p.0) => Err("the plugin changed since this run was approved".into()),
            _ => Ok(()),
        }
    }
}
struct Builder<'a> {
    s: &'a Session,
    selection: crate::cmd::Selection,
    unresolved_transition: bool,
    designated: Vec<LayerKey>,
    sources: Vec<(LayerKey, Value)>,
    nodes: usize,
    bytes: usize,
    stack: Vec<(&'static str, String)>,
    strict: bool,
    auth: Option<Arc<super::scope::RuntimeAccounts>>,
    symbols: std::collections::BTreeMap<(u64, String), Target>,
    outputs: std::collections::BTreeSet<(u64, String)>,
    table_outputs: std::collections::BTreeSet<String>,
    dataset_outputs: std::collections::BTreeMap<String, (u16, u64)>,
}
impl Session {
    /// Expands a bounded ordered plan and binds project/defaults, namespaces and private identities.
    pub(super) fn plan(&self, scope: &Scope, call: &Call) -> Result<Plan, String> {
        let mut b = Builder {
            s: self,
            selection: crate::cmd::InputSource::input_context(self).selection(),
            unresolved_transition: false,
            designated: Vec::new(),
            sources: Vec::new(),
            nodes: 0,
            bytes: 0,
            stack: Vec::new(),
            strict: true,
            auth: None,
            symbols: Default::default(),
            outputs: Default::default(),
            table_outputs: Default::default(),
            dataset_outputs: Default::default(),
        };
        b.charge(&self.connections)?;
        let accounts = self.policy_accounts();
        b.charge(&accounts)?;
        let s3 = crate::render::rustgis_tiles::s3::connections();
        for c in &s3 {
            b.charge(&(
                &c.name,
                &c.endpoint,
                &c.region,
                &c.buckets,
                c.credentials.as_ref().map(|k| (&k.access_key_id, &k.secret_access_key, &k.session_token)),
            ))?;
        }
        let root = b.call(scope, call)?;
        /// Finds nodes whose execution depends on the initially active or selected layers.
        fn defaults(n: &Node) -> bool {
            n.targets.iter().any(|t| matches!(t, Target::Default { .. })) || n.children.iter().any(defaults)
        }
        Ok(Plan {
            owner: scope.owner.clone(),
            opened: self.opened,
            map: self.map().instance_id(),
            designated: b.designated,
            last_table: self.project.tables.iter().map(|t| t.id).max().unwrap_or(0),
            picked: defaults(&root).then(|| (self.active_layer, self.selected_layers())),
            names: self
                .selected_layers()
                .into_iter()
                .chain(self.active_layer)
                .filter_map(|id| self.map().layer(id))
                .map(|l| (l.name.clone(), l.id))
                .collect(),
            sources: b.sources,
            accounts,
            s3,
            root,
        })
    }
    /// Per-stage resolution/preflight has no approval-card size or project-size limits.
    pub(super) fn call_node(&self, scope: &Scope, call: &Call) -> Result<Node, String> {
        Builder {
            s: self,
            selection: crate::cmd::InputSource::input_context(self).selection(),
            unresolved_transition: false,
            designated: Vec::new(),
            sources: Vec::new(),
            nodes: 0,
            bytes: 0,
            stack: Vec::new(),
            strict: false,
            auth: None,
            symbols: Default::default(),
            outputs: Default::default(),
            table_outputs: Default::default(),
            dataset_outputs: Default::default(),
        }
        .call(scope, call)
    }
    /// Resolves one dispatch stage; only explicit approval planning expands automation recursively.
    pub(super) fn command_node(&self, id: &str, params: &Value) -> Result<Node, String> {
        Builder {
            s: self,
            selection: crate::cmd::InputSource::input_context(self).selection(),
            unresolved_transition: false,
            designated: Vec::new(),
            sources: Vec::new(),
            nodes: 0,
            bytes: 0,
            stack: Vec::new(),
            strict: false,
            auth: None,
            symbols: Default::default(),
            outputs: Default::default(),
            table_outputs: Default::default(),
            dataset_outputs: Default::default(),
        }
        .command(id, params, 0)
    }
    /// Resolves a single checked tool stage from its immutable runtime schema.
    pub(super) fn tool_node(&self, name: &str, params: &Value) -> Result<Node, String> {
        Builder {
            s: self,
            selection: crate::cmd::InputSource::input_context(self).selection(),
            unresolved_transition: false,
            designated: Vec::new(),
            sources: Vec::new(),
            nodes: 0,
            bytes: 0,
            stack: Vec::new(),
            strict: false,
            auth: None,
            symbols: Default::default(),
            outputs: Default::default(),
            table_outputs: Default::default(),
            dataset_outputs: Default::default(),
        }
        .tool(name, params)
    }
}
impl<'a> Builder<'a> {
    /// Borrows live project data with only previously planned scalar map transitions applied.
    fn context(&self) -> crate::cmd::InputContext<'a> {
        crate::cmd::InputContext::at(self.s, self.selection)
    }
    /// Records only actual targets/defaults as possible later default sources, never a namespace.
    fn capture_source(&mut self, key: LayerKey) -> Result<(), String> {
        if !self.sources.iter().any(|(k, _)| *k == key)
            && let Some(layer) = key.get(self.s)
        {
            let source = private_source(layer);
            self.charge(&source)?;
            self.sources.push((key, source));
        }
        Ok(())
    }
    /// The same raw layer selection as the executor, qualified by map and incarnation.
    fn default_target(&mut self) -> Result<Target, String> {
        let s = self.context();
        let active = s.active_layer.and_then(|id| s.map().layer(id)).map(|l| LayerKey::of(s.map(), l));
        let selected =
            s.selected_layers().into_iter().filter_map(|id| s.map().layer(id)).map(|l| LayerKey::of(s.map(), l)).collect::<Vec<_>>();
        for key in selected.iter().copied().chain(active) {
            if !self.designated.contains(&key) {
                self.designated.push(key);
            }
            self.capture_source(key)?;
        }
        Ok(Target::Default { active, selected })
    }
    /// Shares execution parsing and whole-script refusal preflight with the approval planner.
    fn call(&mut self, scope: &Scope, call: &Call) -> Result<Node, String> {
        Ok(match &call.kind {
            CallKind::Command { id, params } => self.command(id, params, 0)?,
            CallKind::Script(script) => {
                self.charge(script)?;
                let mut node = self.adapter("script", Effects::NONE, json!({"script":script}));
                node.children = self.script(script, 0)?;
                if node.children.len() > 1
                    && node.children.iter().any(|n| n.effects.contains(Effect::History))
                    && matches!(scope.kind, super::scope::Kind::Agent { access, .. } if access != super::AgentAccess::Full)
                {
                    return Err("undo and redo run alone, so an agent takes back only its own top step".into());
                }
                node
            }
            CallKind::Adapter { what, effects, params } => {
                self.charge(params)?;
                let effects = if matches!(*what, "snapshot" | "view_map") {
                    effects.union(crate::cmd::tiles::render_reach(self.context().map()))
                } else {
                    *effects
                };
                let mut node = self.adapter(what, effects, params.clone());
                self.bind(&mut node, params, None)?;
                node
            }
        })
    }
    /// Charges borrowed content before freezing copies, including repeatedly expanded items.
    fn charge(&mut self, value: &impl serde::Serialize) -> Result<(), String> {
        if self.strict {
            self.bytes += bounded_size(value, MAX_PLAN_BYTES.saturating_sub(self.bytes))?;
        }
        Ok(())
    }
    /// Bounds expanded operations and nesting before traversing another composite.
    fn count(&mut self, depth: usize) -> Result<(), String> {
        if self.strict && self.unresolved_transition {
            return Err("a later step follows a map change whose targets cannot be frozen; split the request or run it yourself".into());
        }
        self.nodes += 1;
        if self.strict && (self.nodes > MAX_NODES || depth > MAX_DEPTH) {
            return Err("this request has too many steps or nested items; ask for a smaller request".into());
        }
        Ok(())
    }
    /// Constructs a node with no implicit targets or hidden children.
    fn adapter(&self, what: &'static str, effects: Effects, params: Value) -> Node {
        Node {
            context: self.context().map().instance_id(),
            line: None,
            op: Op::Adapter(what),
            params,
            effects,
            targets: Vec::new(),
            places: Vec::new(),
            display: ReviewDetail { operation: String::new(), targets: Vec::new(), places: Vec::new() },
            children: Vec::new(),
            item: None,
            generated: false,
            number: self.nodes as u16,
            wires: Vec::new(),
            graph_output: false,
            fixed_tool: None,
            auth: None,
        }
    }
    /// Parses each line through the execution parser and preserves its order in the plan.
    fn script(&mut self, text: &str, depth: usize) -> Result<Vec<Node>, String> {
        crate::cmd::workflows::script_commands(text)?
            .into_iter()
            .map(|(line, id, p)| {
                let mut node = self.command(&id, &p, depth + 1).map_err(|e| format!("line {line} ({id}): {e}"))?;
                node.line = Some(line);
                Ok(node)
            })
            .collect()
    }
    /// Captures a registration or plugin command, then expands only reviewed composite vehicles.
    fn command(&mut self, id: &str, params: &Value, depth: usize) -> Result<Node, String> {
        self.count(depth)?;
        self.charge(params)?;
        if id.starts_with(rustgis_plugins::PREFIX) {
            let c = rustgis_plugins::command(id).ok_or_else(|| format!("unknown command or disabled plugin command '{id}'"))?;
            let pin = PluginPin(c.plugin.clone());
            let effects =
                if c.mutates() { Effects::NONE.with(Effect::Plugin).with(Effect::Edits) } else { Effects::NONE.with(Effect::Plugin) };
            let mut n = self.adapter("plugin", effects, params.clone());
            n.op = Op::Plugin(c, pin);
            self.bind(&mut n, params, None)?;
            return Ok(n);
        }
        let spec = *self.s.registry.get(id).ok_or_else(|| format!("unknown command '{id}'"))?;
        if self.strict && self.selection.names_changed && (spec.inputs.is_some() || params.get("map").is_some()) {
            return Err(
                "a later map selector follows a rename whose namespace cannot be frozen; split the request or run it yourself".into()
            );
        }
        let effects = super::effects::command(&self.context(), &spec, params, self.primary_layer(params));
        let mut n = self.adapter("command", effects, params.clone());
        n.op = Op::Command(spec);
        self.bind(&mut n, params, None)?;
        match id {
            "gp.bufferPoint" => n.fixed_tool = Some(ToolPin::resolve("buffer")?),
            "gp.run" => {
                let tool = params.get("tool").and_then(Value::as_str).ok_or("tool is required")?;
                n.children.push(self.tool(tool, params.get("params").unwrap_or(&Value::Null))?);
            }
            // The batch's items aren't expanded here: it declares Files and Automation, and adds
            // what its tool reaches beyond a built-in's (a plugin's code, an unreviewed runner).
            "gp.batch" => {
                let tool = params.get("tool").and_then(Value::as_str).ok_or("tool is required")?;
                n.effects = n.effects.union(batch_tool_effects(tool)?);
            }
            "gp.spatialJoin" => n.children.push(self.tool("spatialJoin", params)?),
            "gp.zonalStats" => n.children.push(self.tool("zonalStats", params)?),
            "script.run" if self.strict => {
                n.children = self.script(params.get("script").and_then(Value::as_str).ok_or("script is required")?, depth)?
            }
            "task.run" | "notebook.run" | "model.run" if self.strict => self.item(&mut n, id, params, depth)?,
            "project.importQgis" | "project.importGeolibre" | "map.import" => n.generated = true,
            _ => {}
        }
        // Rename changes a spelling, not the approved object's identity. Later named steps
        // resolve the new spelling to the same id, without simulating the handler's edits.
        if id == "layer.rename"
            && let Some(name) = params.get("name").and_then(Value::as_str)
        {
            let target = n
                .targets
                .iter()
                .find(|t| matches!(t, Target::Layer { .. } | Target::MadeLayer { .. }))
                .cloned()
                .or_else(|| self.context().active_layer.map(|id| self.layer_target(id, false)));
            if let Some(target) = target {
                let spelling = name.trim().chars().take(200).collect::<String>();
                if self.strict
                    && (self.symbol(&spelling).is_some_and(|other| !Self::same_layer_target(&target, other))
                        || self.context().map().layers.iter().any(|layer| {
                            layer.name.eq_ignore_ascii_case(&spelling)
                                && !Self::same_layer_target(
                                    &target,
                                    &Target::Layer { key: LayerKey::of(self.context().map(), layer), source: None },
                                )
                        }))
                {
                    return Err("a layer rename would share another layer's name; choose a unique name before asking for approval".into());
                }
                let target = match target {
                    Target::MadeLayer { .. } => Target::MadeLayer { map: self.context().map().instance_id(), name: spelling.clone() },
                    other => other,
                };
                self.symbols.insert((self.context().map().instance_id(), spelling.to_ascii_lowercase()), target);
            }
        }
        if self.strict
            && matches!(id, "layer.addData" | "layer.addDataText" | "layer.addDatabase" | "layer.addSql" | "layer.addEncodedPolyline")
            && let Some(name) = params.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty())
        {
            self.outputs.insert((self.context().map().instance_id(), name.trim().to_ascii_lowercase()));
            self.table_outputs.insert(name.trim().to_ascii_lowercase());
        }
        if self.strict
            && let Some(transition) = spec.transition
        {
            match transition(&self.context(), params)? {
                Some(selection) => self.selection = selection,
                None => self.unresolved_transition = true,
            }
        }
        Ok(n)
    }
    /// Captures a tool without symbolic wiring.
    fn tool(&mut self, name: &str, params: &Value) -> Result<Node, String> {
        self.tool_wired(name, params, Vec::new())
    }
    /// Freezes effective defaults and explicit in-run output references before binding inputs.
    fn tool_wired(&mut self, name: &str, params: &Value, wires: Vec<Wire>) -> Result<Node, String> {
        self.count(0)?;
        let pin = ToolPin::resolve(name)?;
        let params = pin.resolved.resolve_params(params)?;
        self.charge(&params)?;
        let mut effects = Effects::NONE.with(Effect::Edits);
        if pin.plugin.is_some() {
            effects = effects.with(Effect::Plugin);
        }
        if matches!(pin.resolved.source(), Some("geolibre"))
            || crate::cmd::gp::reads_files(pin.spec())
            || crate::cmd::gp::writes_files(pin.spec())
        {
            effects = effects.with(Effect::Files);
        }
        if pin.resolved.source().is_some_and(|s| s != "geolibre" && s != rustgis_plugins::tools::SOURCE) {
            effects = effects.with(Effect::Unreviewed);
        }
        // Source effects follow the same whole-read decision as the executor, resolving
        // prior renames by their bound identity in fast preflight and strict review alike.
        for name in crate::cmd::gp::layer_names(Some(pin.spec()), &params) {
            if let Some(layer) = self.named_layer(name).filter(|layer| crate::cmd::gp::reads_full_source(layer, Some(pin.spec())))
                && let Some(source) = &layer.source
            {
                effects = effects.union(super::effects::source_reach(source));
            }
        }
        let mut n = self.adapter("tool", effects, params.clone());
        n.wires = wires;
        self.bind(&mut n, &params, Some(pin.spec()))?;
        if !crate::cmd::gp::writes_files(pin.spec())
            && let Some(name) = params.get("output").and_then(Value::as_str).filter(|n| !n.trim().is_empty())
        {
            self.outputs.insert((self.context().map().instance_id(), name.trim().to_ascii_lowercase()));
            self.table_outputs.insert(name.trim().to_ascii_lowercase());
            self.dataset_outputs.insert(name.trim().to_ascii_lowercase(), (n.number, self.context().map().instance_id()));
        }
        n.op = Op::Tool(pin);
        n.display.operation = n.id().to_string();
        Ok(n)
    }
    /// Binds dataset identities through executor resolvers and keeps private places separate from review.
    fn bind(&mut self, n: &mut Node, params: &Value, tool: Option<&rustgis_analysis::ToolSpec>) -> Result<(), String> {
        let reads = n.effects.iter().any(|e| matches!(e, Effect::Files | Effect::Network | Effect::Plugin));
        let session = self.s;
        if reads {
            if self.auth.is_none() {
                // The command line and Python read the remembered token only for a command that
                // needs it, and it must be read before this snapshot or the command's first
                // request changes the account and cancels it (#1626). The app reads it at launch.
                if session.saved_secrets_opted_in && names_earthdata_host(params) {
                    crate::render::rustgis_tiles::earthdata::restore();
                }
                self.auth = Some(Arc::new(super::scope::RuntimeAccounts::capture(session)?));
            }
            n.auth = self.auth.clone();
            self.charge(&session.connections)?;
        }
        if let Some(spec) = tool {
            for p in spec.params {
                if n.wires.iter().any(|w| w.key == p.name) {
                    continue;
                }
                use rustgis_analysis::ParamKind::*;
                if matches!(p.kind, Layer | PolygonLayer | PointLayer | VectorLayer | RasterLayer | VectorLayers | RasterLayers) {
                    let names: Vec<&str> = match params.get(p.name) {
                        Some(Value::String(v)) => v.split(';').map(str::trim).filter(|s| !s.is_empty()).collect(),
                        Some(Value::Array(v)) => {
                            if self.strict && v.iter().any(|v| !v.is_string()) {
                                return Err("layer inputs must be names or file paths".into());
                            }
                            v.iter().filter_map(Value::as_str).collect()
                        }
                        Some(Value::Null) | None => Vec::new(),
                        _ if self.strict => return Err("layer inputs must be names or file paths".into()),
                        _ => Vec::new(),
                    };
                    for name in names {
                        if let Some(target) = self.symbol(name) {
                            n.targets.push(self.bound_symbol(target, reads));
                        } else if matches!(p.kind, VectorLayers) && crate::cmd::gp::vector_file_input(spec, self.context().map(), name) {
                            n.places.push(json!({p.name:name}));
                        } else if let Some(input) = crate::cmd::gp::project_input(
                            self.context().map(),
                            &self.s.project.tables,
                            name,
                            matches!(p.kind, RasterLayer | RasterLayers),
                        ) {
                            match input {
                                crate::cmd::gp::ProjectInput::Layer(l) => n.targets.push(Target::Layer {
                                    key: LayerKey::of(self.context().map(), l),
                                    source: reads.then(|| private_source(l)),
                                }),
                                crate::cmd::gp::ProjectInput::Table(t) => {
                                    n.targets.push(Target::Table { id: t.id, source: reads.then(|| json!(t.dataset.source)) })
                                }
                            }
                        } else if matches!(p.kind, VectorLayers) || spec.id.starts_with("geolibre:") {
                            // These schemas explicitly accept files as well as project datasets.
                            // Keep the original path as private equality data, never a guessed layer.
                            n.places.push(json!({p.name:name}));
                        } else if let Some(&(producer, map)) = self.dataset_outputs.get(&name.to_ascii_lowercase()) {
                            n.targets.push(Target::ProducedDataset { producer, map, name: name.into() });
                        } else if self.strict && !self.outputs.contains(&(self.context().map().instance_id(), name.to_ascii_lowercase())) {
                            return Err(format!("no layer or table named '{name}' and no earlier output"));
                        } else {
                            n.targets.push(Target::MadeLayer { map: self.context().map().instance_id(), name: name.into() });
                        }
                    }
                }
            }
        } else {
            // Image adapters consume the same map and raw tile routes as registered exports.
            // Their caller-supplied effects can add reach, but cannot remove these owner inputs.
            let selected = match &n.op {
                Op::Command(spec) => spec.inputs.map(|select| select(&self.context(), params)),
                Op::Adapter("snapshot" | "view_map") => Some(Ok(crate::cmd::tiles::render_inputs(self.context().map()))),
                _ => None,
            };
            let owner_selected = selected.is_some();
            if let Some(inputs) = selected {
                let inputs = match inputs {
                    Ok(inputs) => inputs,
                    Err(_) if !self.strict => crate::cmd::Inputs::unresolved(),
                    Err(e) => return Err(e),
                };
                if self.strict && inputs.unresolved {
                    return Err("this operation's targets cannot be resolved before reading its file; run it yourself".into());
                }
                self.charge(&(&inputs.layers, &inputs.maps, &inputs.tables, &inputs.future_layers))?;
                self.charge(&inputs.state)?;
                n.places.push(inputs.state);
                n.display.places.extend(inputs.review_places);
                let uses_default = inputs.uses_default;
                for name in inputs.future_layers {
                    if let Some(target) = self.symbol(&name) {
                        // Owner callbacks keep the same map-qualified representation after an in-run rename.
                        n.targets.push(match self.bound_symbol(target, reads) {
                            Target::Layer { key, source } => {
                                Target::LayerInMap { map: key.map, id: key.id, instance: key.instance, source }
                            }
                            other => other,
                        });
                    } else if self.outputs.contains(&(self.context().map().instance_id(), name.to_ascii_lowercase())) {
                        n.targets.push(Target::MadeLayer { map: self.context().map().instance_id(), name });
                    } else if self.strict {
                        return Err(format!("no layer '{name}' and no earlier output"));
                    }
                }
                for id in inputs.tables {
                    let table = self.s.project.tables.iter().find(|t| t.id == id).ok_or("an input table disappeared")?;
                    n.targets.push(Target::Table { id, source: reads.then(|| json!(table.dataset.source)) });
                }
                n.targets.extend(inputs.maps.into_iter().map(Target::Map));
                for (map, id) in inputs.layers {
                    let layer = self
                        .s
                        .project
                        .maps
                        .iter()
                        .find(|m| m.instance_id() == map)
                        .and_then(|m| m.layer(id))
                        .ok_or("an input disappeared")?;
                    n.targets.push(Target::LayerInMap {
                        map,
                        id,
                        instance: layer.instance_id(),
                        source: reads.then(|| private_source(layer)),
                    });
                }
                if uses_default {
                    n.targets.push(self.default_target()?);
                }
            } else {
                if crate::cmd::layer::external_dataset_selector(n.id()) {
                    // This is a dataset/sheet inside the frozen external origin, even when empty.
                    n.places.push(json!({"dataset": params.get("layer")}));
                    n.targets.push(self.default_target()?);
                } else {
                    match params.get("layer") {
                        Some(Value::Array(v)) => {
                            // Reuse the command owner's list resolver, including hostile/null semantics.
                            match crate::cmd::layer::layer_ids(&self.context(), params) {
                                Ok(ids) => {
                                    for id in ids {
                                        n.targets.push(self.layer_target(id, reads));
                                    }
                                }
                                Err(_) => {
                                    for v in v {
                                        n.targets.push(self.layer_target_value(v, reads)?);
                                    }
                                }
                            }
                        }
                        Some(v @ (Value::String(_) | Value::Number(_))) => n.targets.push(self.layer_target_value(v, reads)?),
                        _ => n.targets.push(self.default_target()?),
                    }
                }
            }
            if !owner_selected && matches!(params.get("table"), Some(Value::String(_) | Value::Number(_))) {
                match self.s.table_index(params) {
                    Ok(i) => {
                        if let Some(t) = self.s.project.tables.get(i) {
                            n.targets.push(Target::Table { id: t.id, source: reads.then(|| json!(t.dataset.source)) });
                        }
                    }
                    Err(e)
                        if self.strict
                            && !params
                                .get("table")
                                .and_then(Value::as_str)
                                .is_some_and(|n| self.table_outputs.contains(&n.to_ascii_lowercase())) =>
                    {
                        return Err(e);
                    }
                    Err(_) => {
                        if let Some(name) = params.get("table").and_then(Value::as_str) {
                            n.targets.push(Target::MadeTable(name.into()));
                        }
                    }
                }
            }
        }
        for target in &n.targets {
            match target {
                Target::Layer { key, source } => self.charge(&(key.map, key.id, key.instance, source))?,
                Target::LayerInMap { map, id, instance, source } => self.charge(&(map, id, instance, source))?,
                Target::Table { id, source } => self.charge(&(id, source))?,
                Target::Default { active, selected } => self.charge(&(
                    active.map(|k| (k.map, k.id, k.instance)),
                    selected.iter().map(|k| (k.map, k.id, k.instance)).collect::<Vec<_>>(),
                ))?,
                Target::MadeLayer { map, name } => self.charge(&(map, name))?,
                Target::MadeTable(name) => self.charge(name)?,
                Target::ProducedDataset { producer, map, name } => self.charge(&(producer, map, name))?,
                Target::Map(map) => self.charge(map)?,
                Target::TimeSources(sources) => self.charge(sources)?,
            }
            let key = match target {
                Target::Layer { key, .. } => Some(*key),
                Target::LayerInMap { map, id, instance, .. } => Some(LayerKey { map: *map, id: *id, instance: *instance }),
                _ => None,
            };
            if let Some(key) = key {
                self.capture_source(key)?;
            }
        }
        if n.id() == "locator.find" {
            let path = crate::cmd::sharing::locator_path(self.s, params)?;
            n.display.places.push(path.clone());
            n.places.push(json!({"locator":path}));
        }
        if crate::cmd::time::follows_sources(n.id()) {
            n.targets.push(Target::TimeSources(
                crate::cmd::time::following_sources(self.s).into_iter().map(|(m, l, s)| (m, l, s.into())).collect(),
            ));
        }
        if reads {
            if let Some(o) = params.as_object() {
                for (k, v) in o {
                    if super::effects::PLACE_KEYS.contains(&k.as_str()) && !n.wires.iter().any(|w| &w.key == k) {
                        n.places.push(json!({k:v}));
                        if let Some(place) = v.as_str() {
                            n.display.places.push(place.to_string());
                        }
                    }
                }
            }
            // Saved definitions and live bindings are private equality data, never Review data.
            n.places.push(serde_json::to_value(&self.s.connections).map_err(|e| e.to_string())?);
        }
        n.display.operation = n.id().to_string();
        for target in &n.targets {
            match target {
                Target::Layer { .. } | Target::LayerInMap { .. } => {
                    let key = match target {
                        Target::Layer { key, .. } => *key,
                        Target::LayerInMap { map, id, instance, .. } => LayerKey { map: *map, id: *id, instance: *instance },
                        _ => continue,
                    };
                    if let Some(map) = self.s.project.maps.iter().find(|m| m.instance_id() == key.map)
                        && let Some(layer) = key.get(self.s)
                    {
                        n.display.targets.push(format!("{} / {}", map.name, layer.name));
                    }
                }
                Target::Table { id, .. } => {
                    if let Some(table) = self.s.project.tables.iter().find(|t| t.id == *id) {
                        n.display.targets.push(table.dataset.name.clone());
                    }
                }
                Target::Map(id) => {
                    if let Some(map) = self.s.project.maps.iter().find(|m| m.instance_id() == *id) {
                        n.display.targets.push(map.name.clone());
                    }
                }
                Target::MadeLayer { name, .. } | Target::MadeTable(name) | Target::ProducedDataset { name, .. } => {
                    n.display.targets.push(name.clone())
                }
                Target::TimeSources(sources) => {
                    for (map_id, layer_id, _) in sources {
                        if let Some(map) = self.s.project.maps.iter().find(|m| m.instance_id() == *map_id)
                            && let Some(layer) = map.layer(*layer_id)
                        {
                            n.display.targets.push(format!("{} / {}", map.name, layer.name));
                        }
                    }
                }
                _ => {}
            }
        }
        for value in n.display.targets.iter_mut().chain(n.display.places.iter_mut()) {
            *value = rustgis_data::diagnostics::Redacted::new(value).into_string();
        }
        n.display.targets.dedup();
        n.display.places.dedup();
        Ok(())
    }
    /// Compares proven layer identity without treating source metadata as identity.
    fn same_layer_target(a: &Target, b: &Target) -> bool {
        let key = |target: &Target| match target {
            Target::Layer { key, .. } => Some(*key),
            Target::LayerInMap { map, id, instance, .. } => Some(LayerKey { map: *map, id: *id, instance: *instance }),
            _ => None,
        };
        match (key(a), key(b)) {
            (Some(a), Some(b)) => a == b,
            _ => matches!((a, b), (Target::MadeLayer { map: a, name: an }, Target::MadeLayer { map: b, name: bn })
                if a == b && an.eq_ignore_ascii_case(bn)),
        }
    }
    /// Map-qualified planned spellings match the executor's ASCII case-insensitive lookup.
    fn symbol(&self, name: &str) -> Option<&Target> {
        self.symbols.get(&(self.context().map().instance_id(), name.to_ascii_lowercase()))
    }
    /// Literal names retain the executor's map-layer precedence; known future outputs never
    /// fall back to a same-named old layer as if they already had a source.
    fn named_layer(&self, name: &str) -> Option<&rustgis_map::Layer> {
        match self.symbol(name) {
            Some(Target::Layer { key, .. }) => key.get(self.s),
            Some(Target::LayerInMap { map, id, instance, .. }) => LayerKey { map: *map, id: *id, instance: *instance }.get(self.s),
            Some(_) => None,
            None => self.context().map().layer_by_name(name),
        }
    }
    /// Uses the execution owner's name-before-id/default selector, with prior bound renames.
    fn primary_layer(&self, params: &Value) -> Option<&rustgis_map::Layer> {
        if let Some(name) = params.get("layer").and_then(Value::as_str)
            && self.symbol(name).is_some()
        {
            return self.named_layer(name);
        }
        self.context().layer_id(params).ok().and_then(|id| self.context().map().layer(id))
    }
    /// Uses Session's real name-before-id and numeric/default selector semantics.
    fn layer_target_value(&self, value: &Value, reads: bool) -> Result<Target, String> {
        if let Some(t) = value.as_str().and_then(|name| self.symbol(name)) {
            return Ok(self.bound_symbol(t, reads));
        }
        match self.context().layer_id(&json!({"layer":value})) {
            Ok(id) => Ok(self.layer_target(id, reads)),
            Err(error)
                if self.strict
                    && !value
                        .as_str()
                        .is_some_and(|n| self.outputs.contains(&(self.context().map().instance_id(), n.to_ascii_lowercase()))) =>
            {
                Err(error)
            }
            Err(_) => value
                .as_str()
                .map(|n| Target::MadeLayer { map: self.context().map().instance_id(), name: n.into() })
                .ok_or("unresolved layer target".into()),
        }
    }
    /// An approved rename retains the original incarnation and adds its source when later read.
    fn bound_symbol(&self, target: &Target, reads: bool) -> Target {
        match target {
            Target::Layer { key, .. } => Target::Layer { key: *key, source: reads.then(|| key.get(self.s).map(private_source)).flatten() },
            other => other.clone(),
        }
    }
    /// Edit-only targets still bind map/id/incarnation; source identity is additive for external reach.
    fn layer_target(&self, id: u32, reads: bool) -> Target {
        let map = self.context().map();
        Target::Layer {
            key: LayerKey { map: map.instance_id(), id, instance: map.layer(id).map_or(0, |l| l.instance_id()) },
            source: reads.then(|| map.layer(id).map(private_source)).flatten(),
        }
    }
    /// Freezes selected task/cell/model content and provenance with bounded cycle-aware expansion.
    fn item(&mut self, n: &mut Node, id: &str, params: &Value, depth: usize) -> Result<(), String> {
        let kind = id.split('.').next().unwrap_or("");
        let kind: &'static str = match kind {
            "task" => "task",
            "notebook" => "notebook",
            _ => "model",
        };
        let name = self.s.item_name(kind, params)?;
        let session = self.s;
        match kind {
            "task" => self.charge(&crate::cmd::items::task_ref(session, params)?.steps)?,
            "notebook" => {
                let nb = session.project.notebooks.get(crate::cmd::notebook::index(session, params)?).ok_or("no such notebook")?;
                for cell in &nb.cells {
                    self.charge(&(&cell.kind, &cell.source))?;
                }
            }
            _ if params.get("graph").is_none_or(Value::is_null) => {
                let model = session.project.models.iter().find(|m| m.name.eq_ignore_ascii_case(&name)).ok_or("no such model")?;
                self.charge(model)?;
            }
            _ => {}
        }
        let ad_hoc = kind == "model" && params.get("graph").is_some_and(|g| !g.is_null());
        let content = if ad_hoc {
            params.get("graph").cloned().unwrap_or(Value::Null)
        } else {
            self.s.item_content(kind, &name).ok_or("the item no longer exists")?
        };
        let trusted = ad_hoc || self.s.item_trusted(kind, &name);
        if self.stack.iter().any(|(k, n)| *k == kind && n.eq_ignore_ascii_case(&name)) {
            return Err("this request contains an item cycle".into());
        }
        self.stack.push((kind, name.clone()));
        n.item = Some((kind, name.clone(), trusted, content));
        match kind {
            "task" => {
                let t = crate::cmd::items::task_ref(self.s, params)?;
                for (i, st) in t
                    .steps
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| params.get("step").filter(|v| !v.is_null()).is_none_or(|v| v.as_u64() == Some(*i as u64)))
                {
                    if st.command.starts_with("task.") {
                        return Err(format!("step {} would run another task", i + 1));
                    }
                    n.children.push(self.command(&st.command, &st.params, depth + 1)?);
                }
            }
            "notebook" => {
                let i = crate::cmd::notebook::index(self.s, params)?;
                let nb = self.s.project.notebooks.get(i).ok_or("no notebook")?;
                if nb.language().is_none() {
                    for (i, c) in
                        nb.cells.iter().enumerate().filter(|(i, _)| params.get("cell").is_none_or(|v| v.as_u64() == Some(*i as u64)))
                    {
                        let _ = i;
                        if c.kind == crate::cmd::notebook::CellKind::Code {
                            n.children.extend(self.script(&c.source, depth + 1)?);
                        }
                    }
                }
            }
            _ => {
                let stored = (!ad_hoc).then(|| self.s.project.models.iter().find(|m| m.name.eq_ignore_ascii_case(&name))).flatten();
                if let Some(model) = stored.filter(|m| m.graph.is_none()) {
                    let mut outputs = Vec::new();
                    for step in &model.steps {
                        let pin = ToolPin::resolve(&step.tool)?;
                        let mut p = step.params.clone();
                        let mut wires = Vec::new();
                        if let Some(o) = p.as_object_mut() {
                            for (key, v) in o {
                                if let Some(index) = v.as_str().and_then(|t| t.strip_prefix('%')).and_then(|n| n.parse::<usize>().ok()) {
                                    let layer = pin.spec().params.iter().any(|p| p.name == key && layer_kind(p.kind));
                                    if !layer {
                                        return Err(
                                            "a model's dynamic external target cannot be shown before it runs; run the model yourself"
                                                .into(),
                                        );
                                    }
                                    let from = outputs
                                        .get(index.wrapping_sub(1))
                                        .copied()
                                        .ok_or("a model references an output that has not run")?;
                                    wires.push(Wire { key: key.clone(), from: vec![OutputRef::Node(from)] });
                                } else if dynamic(v, 0) {
                                    return Err("a model's nested dynamic target cannot be resolved for approval".into());
                                }
                            }
                        }
                        let child = self.tool_wired(&step.tool, &p, wires)?;
                        outputs.push(child.number);
                        n.children.push(child);
                    }
                } else {
                    let (_, g, _) = crate::cmd::workflows::model_graph(self.s, params)?;
                    let order = crate::model::topological_order(&g).ok_or("the model contains a loop")?;
                    let mut outputs = std::collections::HashMap::<String, OutputRef>::new();
                    for i in order {
                        let node = g.nodes.get(i).ok_or("missing model node")?;
                        use crate::project::ModelNodeKind;
                        match node.kind {
                            ModelNodeKind::Input => {
                                let name = node.layer.as_deref().ok_or("model input has no layer")?;
                                let from = if let Some(l) = self.context().map().layer_by_name(name) {
                                    OutputRef::ExistingLayer(LayerKey::of(self.context().map(), l))
                                } else if let Some(t) =
                                    crate::project::find_table(&self.s.project.tables, name).and_then(|i| self.s.project.tables.get(i))
                                {
                                    OutputRef::ExistingTable(t.id)
                                } else {
                                    return Err(format!("no model input layer or table '{name}'"));
                                };
                                outputs.insert(node.id.clone(), from);
                            }
                            ModelNodeKind::Output => {
                                let edge = g
                                    .edges
                                    .iter()
                                    .find(|e| e.to == node.id && e.to_port == crate::model::IN)
                                    .ok_or("model output has no input")?;
                                let from = outputs.get(&edge.from).cloned().ok_or("model output's source is unresolved")?;
                                outputs.insert(node.id.clone(), from);
                            }
                            ModelNodeKind::Tool => {
                                let tool = node.tool.as_deref().ok_or("model tool missing")?;
                                let pin = ToolPin::resolve(tool)?;
                                let mut p = match &node.params {
                                    Value::Object(o) => Value::Object(o.clone()),
                                    _ => json!({}),
                                };
                                let mut wires = Vec::new();
                                for param in pin.spec().params.iter().filter(|p| layer_kind(p.kind)) {
                                    let edges = g.edges.iter().filter(|e| e.to == node.id && e.to_port == param.name).collect::<Vec<_>>();
                                    if edges.is_empty() {
                                        continue;
                                    }
                                    let from = edges
                                        .iter()
                                        .map(|e| {
                                            outputs.get(&e.from).cloned().ok_or_else(|| "the model's wired input is unresolved".to_string())
                                        })
                                        .collect::<Result<Vec<_>, _>>()?;
                                    p[param.name] = json!("[output of an earlier model node]");
                                    wires.push(Wire { key: param.name.into(), from });
                                }
                                let mut child = self.tool_wired(tool, &p, wires)?;
                                child.graph_output = true;
                                outputs.insert(node.id.clone(), OutputRef::Node(child.number));
                                n.children.push(child);
                            }
                        }
                    }
                }
            }
        }
        self.stack.pop();
        Ok(())
    }
}
impl Node {
    /// Checks every known child before a script's first irreversible operation, using one engine table.
    pub(super) fn decision(&self, s: &Session, scope: &Scope) -> Result<Decision, String> {
        /// Visits ordered children under their content-bound item restriction.
        fn visit(s: &Session, scope: &Scope, n: &Node) -> Result<Decision, String> {
            let own = decide::decide(s, scope, n);
            let mut d = Decision::Allow;
            let scope = if let Some((kind, name, false, _)) = &n.item {
                let mut scope = scope.clone();
                scope.kind = super::scope::Kind::Item { kind, name: name.clone(), within: Box::new(scope.kind) };
                scope
            } else {
                scope.clone()
            };
            // Report the actual pinned child operation before its vehicle's broad effects.
            // Both decisions still finish before the vehicle's first effect.
            for c in &n.children {
                let child =
                    visit(s, &scope, c).map_err(|e| c.line.map_or_else(|| e.clone(), |line| format!("line {line} ({}): {e}", c.id())))?;
                d = d.max(child);
            }
            Ok(d.max(own?))
        }
        visit(s, scope, self)
    }
}
impl Plan {
    /// Exact approval uses the same decision as unbounded allowed-path preflight.
    pub(super) fn decision(&self, s: &Session, scope: &Scope) -> Result<Decision, String> {
        self.root.decision(s, scope)
    }
    /// Produces a bounded sanitized display copy; raw equality data remains private.
    pub(super) fn review(&self, remember: bool) -> Result<Review, String> {
        /// Refuses parameters too large to present before constructing an approval card.
        fn raw_size(n: &Node) -> usize {
            n.params.to_string().len().saturating_add(n.children.iter().map(raw_size).sum::<usize>())
        }
        if raw_size(&self.root) > MAX_REVIEW {
            return Err("too long to show in full; run it yourself or ask for a smaller request".into());
        }
        /// Lists every asking node and counts project-only edits without exposing private bindings.
        fn shown(n: &Node, steps: &mut Vec<String>, edits: &mut usize, details: &mut Vec<ReviewDetail>) -> Result<(), String> {
            if !n.display.targets.is_empty() || !n.display.places.is_empty() {
                details.push(n.display.clone());
            }
            if n.effects.iter().any(|e| !matches!(e, Effect::Edits | Effect::History)) {
                let mut line = format!("{} {}", n.id(), crate::project_log::review_params(&n.params)?);
                if let Op::Plugin(_, p) = &n.op {
                    line.push_str(&format!(
                        " — {} {:?}, SHA-256 {}, permissions {:?}",
                        crate::project_log::review_params(&json!(p.0.manifest.name))?,
                        p.0.manifest.runtime,
                        p.0.sha256,
                        p.0.manifest.permissions
                    ));
                }
                if let Op::Tool(t) = &n.op
                    && let Some(p) = &t.plugin
                {
                    line.push_str(&format!(
                        " — {} {:?}, SHA-256 {}, permissions {:?}",
                        crate::project_log::review_params(&json!(p.0.manifest.name))?,
                        p.0.manifest.runtime,
                        p.0.sha256,
                        p.0.manifest.permissions
                    ));
                }
                steps.push(line);
            } else {
                *edits += 1;
            }
            for c in &n.children {
                shown(c, steps, edits, details)?;
            }
            Ok(())
        }
        let mut steps = Vec::new();
        let mut edits = 0;
        let mut details = Vec::new();
        shown(&self.root, &mut steps, &mut edits, &mut details)?;
        let defaults = self.picked.as_ref().map(|(active, selection)| {
            let name = |id: &u32| {
                rustgis_data::diagnostics::Redacted::new(
                    self.names.iter().find(|(_, n)| n == id).map(|(name, _)| name.as_str()).unwrap_or("(missing layer)"),
                )
                .into_string()
            };
            (active.as_ref().map(name), selection.iter().map(name).collect())
        });
        let what = format!("{} {}", self.root.id(), crate::project_log::review_params(&self.root.params)?);
        /// Finds the strongest reason across all frozen steps, including nested tools.
        fn all_effects(n: &Node) -> Effects {
            n.children.iter().fold(n.effects, |e, c| e.union(all_effects(c)))
        }
        let explanation = self.root.explanation(all_effects(&self.root));
        let mut why = explanation.0.to_string();
        for (key, value) in &explanation.1 {
            why = why.replace(&format!("{{{key}}}"), value);
        }
        // Both reason-only callers and translated cards use the same frozen script locations.
        let lines = self
            .root
            .children
            .iter()
            .filter_map(|n| {
                n.line
                    .filter(|_| n.effects.iter().any(|e| !matches!(e, Effect::Edits | Effect::History)))
                    .map(|line| (line, n.id().to_string()))
            })
            .collect::<Vec<_>>();
        if !lines.is_empty() {
            let locations = lines.iter().map(|(line, id)| format!("line {line} ({id})")).collect::<Vec<_>>();
            why = format!("{}: {why}", locations.join("; "));
        }
        if bounded_size(&(&what, &why, &steps, &details, &defaults, &explanation, &lines), MAX_REVIEW).is_err() {
            return Err("too long to show in full; run it yourself or ask for a smaller request".into());
        }
        Ok(Review { what, why, steps, remember, edits, defaults, explanation, details, lines })
    }
}

/// Identifies schema roles that can carry model output wires.
fn layer_kind(kind: rustgis_analysis::ParamKind) -> bool {
    use rustgis_analysis::ParamKind::*;
    matches!(kind, Layer | PolygonLayer | PointLayer | VectorLayer | RasterLayer | VectorLayers | RasterLayers)
}
/// Rejects nested/external interpolation that cannot be resolved to a dataset wire.
fn dynamic(v: &Value, depth: usize) -> bool {
    if depth > 8 {
        return true;
    }
    match v {
        Value::String(t) => t.strip_prefix('%').is_some_and(|n| n.parse::<usize>().is_ok()),
        Value::Array(a) => a.iter().any(|v| dynamic(v, depth + 1)),
        Value::Object(o) => o.values().any(|v| dynamic(v, depth + 1)),
        _ => false,
    }
}

/// Captures live source identity, including streamed and raster locations, without redaction.
pub(super) fn private_source(l: &rustgis_map::Layer) -> Value {
    json!({"data":l.dataset.source,"streamed":l.source.as_ref().map(|s|(&s.path,&s.table,&s.gdal,&s.duckdb,&s.postgis,&s.mysql,&s.sqlserver)),"raster":l.raster.as_ref().and_then(|r|r.path.as_ref()),
        "zarr":l.raster.as_ref().and_then(|r|r.time_zarr.as_ref()).map(|z|(&z.store,&z.variable,&z.index,&z.dim,z.offset,&z.crs)),
        "postgis":l.postgis.as_ref().map(|p|(&p.origin,&p.key,p.srid,p.writable,&p.condition,p.limit)),
        "gpkg":l.gpkg.as_ref().map(|p|(&p.path,&p.table,&p.key,&p.geometry,p.srs_id,&p.geometry_type,p.z,p.m,&p.revision,p.writable,p.limit))})
}
impl Session {
    /// Captures only already available connection state; never reads the secret store.
    pub(super) fn policy_accounts(&self) -> Value {
        json!({"connections":
            self.connections
                .connections
                .iter()
                .map(|c| json!({"definition":c,"password":self.database_logins.known_password(&c.id).map(|p|p.expose())}))
                .collect::<Vec<_>>(),
            "web":{"selected":self.database_logins.web_selections(),"active":self.web_logins.active_ids()}
        })
    }
}

/// What a batch's tool reaches beyond a built-in's, without its per-item parameters.
pub(super) fn batch_tool_effects(name: &str) -> Result<Effects, String> {
    let pin = ToolPin::resolve(name)?;
    let mut e = Effects::NONE;
    if pin.plugin.is_some() {
        e = e.with(Effect::Plugin);
    }
    if pin.resolved.source().is_some_and(|s| s != "geolibre" && s != rustgis_plugins::tools::SOURCE) {
        e = e.with(Effect::Unreviewed);
    }
    Ok(e)
}

/// Projects pinned tool effects for compatibility consumers; execution still uses its own token.
pub(super) fn tool_effect_node(name: &str, params: &Value) -> Result<Effects, String> {
    let pin = ToolPin::resolve(name)?;
    let _ = pin.resolved.resolve_params(params)?;
    let mut e = Effects::NONE.with(Effect::Edits);
    if pin.plugin.is_some() {
        e = e.with(Effect::Plugin);
    }
    if pin.resolved.source() == Some("geolibre") || crate::cmd::gp::reads_files(pin.spec()) || crate::cmd::gp::writes_files(pin.spec()) {
        e = e.with(Effect::Files);
    }
    if pin.resolved.source().is_some_and(|s| s != "geolibre" && s != rustgis_plugins::tools::SOURCE) {
        e = e.with(Effect::Unreviewed);
    }
    Ok(e)
}

impl Node {
    /// Preserves the established operation-specific explanation; values are display-only.
    fn explanation(&self, effects: Effects) -> (&'static str, Vec<(&'static str, String)>) {
        let clean = |v: &str| rustgis_data::diagnostics::Redacted::new(v).into_string();
        match self.id() {
            "layer.addSql" | "sql.query" => match self.params.get("database").and_then(Value::as_str).filter(|d| !d.trim().is_empty()) {
                Some(d) => (
                    "it runs an SQL query, which can read files on this computer and addresses on the internet (as can views in the database {database})",
                    vec![("database", clean(d))],
                ),
                None => ("it runs an SQL query, which can read files on this computer and addresses on the internet", vec![]),
            },
            "snapshot" if effects.contains(Effect::Files) => ("it writes a file on this computer", vec![]),
            "project.importGeolibre" => ("it adds the files and web addresses a GeoLibre project names", vec![]),
            "project.importQgis" => ("it adds the files and web addresses a QGIS project names", vec![]),
            "layer.addDownload" => ("it completes a download that may import a project and its referenced sources", vec![]),
            _ => match &self.op {
                Op::Plugin(..) => ("it runs a plugin's command", vec![]),
                Op::Tool(pin) if pin.plugin.is_some() => ("it runs a plugin's tool ({tool})", vec![("tool", clean(pin.spec().id))]),
                _ => {
                    for child in &self.children {
                        let reason = child.explanation(child.effects);
                        if reason.0 != decide::reason(child.effects) {
                            return reason;
                        }
                    }
                    (decide::reason(effects), vec![])
                }
            },
        }
    }
}
