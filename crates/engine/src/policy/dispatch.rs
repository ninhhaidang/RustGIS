use super::plan::OutputRef;
use super::{
    Call, Node, Outcome, Pending, Permit, PluginPin, Remembered, Scope, ToolPin,
    decide::{self, Decision},
    plan::{CallKind, LayerKey, Op, Plan, Target},
    scope::{Continuation, Kind, Narrow},
};
use crate::{CmdResult, CommandSpec, Session};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) enum Txn {
    Own,
    Shared,
}
/// The only key accepted by raw runners; construction is private to this module.
pub(crate) struct Authorized<T> {
    op: T,
    node: Arc<Node>,
}
impl<T> Authorized<T> {
    /// Borrows the immutable admitted operation; possession never permits replacing its node.
    pub(crate) fn op(&self) -> &T {
        &self.op
    }
}
pub(crate) enum CommandOp {
    Registered { spec: CommandSpec, params: Value },
    Plugin { cmd: rustgis_plugins::PluginCommand, pin: PluginPin, params: Value },
}
/// A single plugin invocation held before handler dispatch. Its operation and authority are
/// engine-owned; it cannot be cloned, serialized or captured from a handler that may roll back.
pub struct PreparedCommand {
    op: Authorized<CommandOp>,
    completion: super::Deferred,
}
impl PreparedCommand {
    /// Checks the held invocation without consuming it or granting a replacement load authority.
    pub fn is_current(&self) -> bool {
        self.completion.0.check().is_ok()
    }
}
pub(crate) struct ToolOp {
    pub(crate) pin: ToolPin,
    pub(crate) params: Value,
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct RasterOp {
    pub(crate) spec: CommandSpec,
    pub(crate) params: Value,
}
pub(crate) struct ModelOp {
    pub(crate) name: String,
    pub(crate) graph: crate::project::ModelGraph,
}
#[derive(Default)]
pub(crate) struct State {
    scopes: Vec<Scope>,
    nodes: Vec<Arc<Node>>,
    cursor: Option<Cursor>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    pub(super) work: super::downloads::Work,
}
struct Cursor {
    plan: Arc<Plan>,
    frames: Vec<(Vec<Node>, usize)>,
    designated: Vec<LayerKey>,
    outputs: std::collections::BTreeMap<u16, OutputRef>,
    made_layers: Vec<LayerKey>,
    made_tables: Vec<u32>,
}
impl Session {
    /// Holds a plugin command at the pre-handler UI queue boundary, inheriting the current scope.
    /// A handler or approved composite must run its children synchronously through normal dispatch.
    pub fn prepare_command(&mut self, id: &str, params: &Value) -> Result<PreparedCommand, String> {
        if self.depth > 0 || !self.policy.nodes.is_empty() || self.policy.cursor.is_some() {
            return Err("plugin commands can only be queued before handler dispatch".into());
        }
        if !id.starts_with(rustgis_plugins::PREFIX) {
            return Err("only plugin commands can wait in the UI command queue".into());
        }
        let op = self.authorize(id, params)?;
        let completion = super::Deferred(self.continuation_for(op.node.clone())?);
        Ok(PreparedCommand { op, completion })
    }
    /// Consumes the held invocation under its initiating owner, with ordinary command bookkeeping.
    pub fn run_prepared(&mut self, prepared: PreparedCommand) -> CmdResult {
        let PreparedCommand { op, completion } = prepared;
        let (id, params) = match op.op() {
            CommandOp::Plugin { cmd, params, .. } => (cmd.id.clone(), params.clone()),
            _ => return Err("not a prepared plugin command".into()),
        };
        self.with_deferred_downloads(|s| {
            s.command_epilogue(&id, &params, |s| {
                s.check_delivery(&completion.0)?;
                // Native clicks retain dynamic plugin host calls, including a second queued click
                // that names the first one's output. Restricted callers keep their exact targets.
                if !completion.0.native_tool_owner() && *op.node != s.command_node(&id, &params)? {
                    return Err("the queued command changed since it was authorized".into());
                }
                s.resume(&completion.0, |s| s.invoke(op, Txn::Own))?
            })
        })
    }
    /// Admits a file read before its picker, freezes Load All's selector, and seals its owner.
    pub(crate) fn start_read(&mut self, command: &str, params: &Value) -> Result<crate::cmd::reads::ReadAhead, String> {
        if self.depth > 0 || !self.policy.nodes.is_empty() || self.policy.cursor.is_some() {
            return Err("background reads can only start before handler dispatch".into());
        }
        if !matches!(command, "layer.addData" | "layer.loadAll" | "model.run" | "script.run") {
            return Err("only Add Data, Load All, models and scripts can prepare background work".into());
        }
        let mut params = params.as_object().cloned().ok_or("read parameters must be an object")?;
        if command == "layer.loadAll" {
            let id = self.layer_id(&Value::Object(params.clone()))?;
            params.insert("layer".into(), json!(id));
        }
        let mut op = self.authorize(command, &Value::Object(params))?;
        self.check_read_command(&op)?;
        let copy_command = matches!(command, "model.run" | "script.run") && self.policy_user();
        if copy_command {
            // A model's or script's native copy may read through several tools. Bind the original
            // session's runtime stores, including web logins, before that copy exists.
            let mut node = (*op.node).clone();
            node.auth = Some(Arc::new(super::scope::RuntimeAccounts::capture(self)?));
            op.node = Arc::new(node);
        }
        let node = op.node.clone();
        let mut owner = self.continuation_for(node.clone())?;
        if copy_command {
            owner.accounts = self.policy_accounts();
            owner.s3 = Some(vec![crate::render::rustgis_tiles::s3::account_identity()?]);
        }
        owner.check()?;
        let current = owner.clone();
        let still = Arc::new(move || current.check().is_ok());
        self.with_node(node, |s| crate::render::rustgis_tiles::cache::with_still(still, || crate::cmd::reads::prepare(s, op, owner)))
    }
    /// Lands only the original read invocation; no result can be claimed by another command.
    pub(crate) fn finish_read(&mut self, read: crate::cmd::reads::Read) -> CmdResult {
        let crate::cmd::reads::Read { op, owner, source, ready } = read;
        let (command, params) = match op.op() {
            CommandOp::Registered { spec, params } => (spec.id, params.clone()),
            _ => return Err("not a registered read command".into()),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.with_deferred_downloads(|s| {
                s.command_epilogue(command, &params, |s| {
                    s.check_delivery(&owner)?;
                    if let Some(source) = &source {
                        let layer = s.map().layer(s.layer_id(&params)?).ok_or("no such layer")?;
                        if !layer.source.as_ref().is_some_and(|current| current.same_opening(source)) {
                            return Err("the live source changed since this read started".into());
                        }
                    }
                    s.resume(&owner, |s| {
                        if !owner.native_tool_owner() && *op.node != s.command_node(command, &params)? {
                            return Err("the read command changed since it was authorized".into());
                        }
                        s.check_read_command(&op)?;
                        s.read_ahead = ready;
                        s.invoke(op, Txn::Own)
                    })?
                })
            })
        }));
        self.read_ahead = None;
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
    /// Preserves ordinary command conditions for the admitted read's explicit layer selector.
    fn check_read_command(&mut self, op: &Authorized<CommandOp>) -> Result<(), String> {
        let CommandOp::Registered { spec, params } = op.op() else { return Err("not a registered read command".into()) };
        let named = self.layer_id(params).ok();
        let active = self.active_layer;
        if named.is_some() {
            self.active_layer = named;
        }
        let blocked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (spec.enabled)(self))).unwrap_or(Some("unavailable"));
        self.active_layer = active;
        if let Some(why) = blocked {
            return Err(format!("{}: {why}", spec.label));
        }
        if let Some(why) = self.streamed_edit(spec.id, spec.mutates, spec.label, named.or(active)) {
            return Err(why);
        }
        Ok(())
    }
    /// Admits an export before its preparation reads any input or creates off-thread work.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn start_export(&mut self, command: &str, params: &Value) -> Result<crate::cmd::exports::ExportRun, String> {
        if self.depth > 0 || !self.policy.nodes.is_empty() || self.policy.cursor.is_some() {
            return Err("background exports can only start before handler dispatch".into());
        }
        let op = self.authorize(command, params)?;
        let node = op.node.clone();
        // Prepared outside a command: what it couldn't read as true or false is said here (#1600).
        let source = self.registry.get(command).map(|spec| spec.id);
        self.with_unread_bools(source, |s| s.with_node(node, |s| crate::cmd::exports::export_start_raw(s, op)))
    }
    /// Exports own their input snapshot. Native delivery may report it after a layer/map switch;
    /// restricted delivery still binds the original live map and source identities.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn finish_export<T>(&mut self, c: &Continuation, f: impl FnOnce(&mut Session) -> T) -> Result<T, String> {
        self.check_export_delivery(c)?;
        self.resume_checked(c, f)
    }
    /// Exported bytes own their snapshot; a native map switch cannot retarget their delivery.
    /// Scoped delivery retains its original map, sources, accounts and live owner.
    pub(crate) fn check_export_delivery(&self, c: &Continuation) -> Result<(), String> {
        self.check_delivery_owner(c)?;
        if c.native_tool_owner() {
            self.check_continuation_accounts(c)?;
            for (key, source) in &c.sources {
                if key.get(self).is_some_and(|l| super::plan::private_source(l) != *source) {
                    return Err("the live source or account changed since this request started".into());
                }
            }
            Ok(())
        } else {
            self.check_continuation(c)
        }
    }
    /// A JSON file acknowledgment must already be executing as the captured owner.
    pub(crate) fn gp_file_owner(&self, c: &Continuation) -> Result<(), String> {
        if !self.scope().same_caller(&c.scope) {
            return Err("this run belongs to another caller".into());
        }
        self.check_export_delivery(c)
    }
    /// Distinguishes a direct native URL ingress from a handler already holding admission.
    pub(super) fn policy_has_no_operation(&self) -> bool {
        self.policy.nodes.is_empty()
    }
    /// Direct native URL requests use the same effect decision as their command ingress.
    pub(super) fn request_url_authorized(&mut self, p: &Value) -> CmdResult {
        let op = self.authorize("layer.addUrl", p)?;
        self.with_node(op.node, |s| s.enqueue_url(p))
    }
    /// The import executor's native entry admits the import before creating an internal claim.
    pub(super) fn request_import_url_authorized(&mut self, p: &Value) -> CmdResult {
        let op = self.authorize("project.importGeolibre", p)?;
        self.with_node(op.node, |s| s.request_import_url(p))
    }
    /// Native completion helpers may resume an owner only at a root ingress.
    pub(crate) fn policy_ingress_idle(&self) -> bool {
        self.policy.scopes.is_empty()
    }
    /// Only the native user scope may record executable-content provenance.
    pub(crate) fn policy_user(&self) -> bool {
        self.policy.scopes.last().is_none_or(|s| matches!(s.kind, Kind::User))
    }
    /// Inherits the current named scope; absence is the documented trusted native convention.
    fn scope(&self) -> Scope {
        self.policy.scopes.last().cloned().unwrap_or_else(Scope::user)
    }
    /// Named untrusted ingress. Cannot be used to widen an existing scope, including on panic.
    pub fn with_scope<T>(&mut self, scope: Scope, f: impl FnOnce(&mut Session) -> T) -> Result<T, String> {
        if !self.policy.scopes.is_empty() {
            return Err("an ingress scope cannot be nested or widened".into());
        }
        scope.check(super::Effects::NONE)?;
        self.in_scope(scope, f)
    }
    /// Restores scope depth after success, refusal or an unwinding handler panic.
    fn in_scope<T>(&mut self, scope: Scope, f: impl FnOnce(&mut Session) -> T) -> Result<T, String> {
        let n = self.policy.scopes.len();
        let actor = std::mem::replace(&mut self.actor, scope.actor());
        self.policy.scopes.push(scope);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        self.policy.scopes.truncate(n);
        self.actor = actor;
        match r {
            Ok(r) => Ok(r),
            Err(p) => std::panic::resume_unwind(p),
        }
    }
    /// Runs what a session plugin's program asks between its commands with the plugin's own
    /// authority, as its commands run: the exact load, still current, narrowing the scope the app
    /// is in (none at the frame's root, the trusted native convention).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn as_session_plugin<T>(
        &mut self,
        plugin: Arc<rustgis_plugins::Plugin>,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Result<T, String> {
        if !rustgis_plugins::is_current(&plugin) {
            return Err("the plugin was disabled, updated or removed".into());
        }
        self.narrowed(Narrow::Plugin(super::plan::PluginPin(plugin)), f)
    }
    /// Intersects nested plugin/item authority with its initiating scope.
    pub(crate) fn narrowed<T>(&mut self, to: Narrow, f: impl FnOnce(&mut Session) -> T) -> Result<T, String> {
        let mut scope = self.scope();
        scope.kind = match to {
            Narrow::Plugin(pin) => Kind::Plugin { pin, within: Box::new(scope.kind) },
            Narrow::Item { kind, name } => Kind::Item { kind, name, within: Box::new(scope.kind) },
        };
        self.in_scope(scope, f)
    }
    /// Read-only planning and the one engine decision for this call.
    pub fn review(&self, scope: &Scope, call: &Call) -> Outcome {
        let result = (|| {
            let node = self.call_node(scope, call)?;
            if node.decision(self, scope)? == Decision::Allow {
                return Ok(Outcome::Allowed);
            }
            let plan = self.plan(scope, call)?;
            let decision = plan.decision(self, scope)?;
            /// Finds imports whose operations cannot be enumerated before external content is read.
            fn generated(n: &Node) -> bool {
                n.generated || n.children.iter().any(generated)
            }
            if generated(&plan.root) {
                return Err(format!(
                    "{}; RustGIS can't show its operations before reading it; import it yourself",
                    if plan.root.id() == "project.importQgis" {
                        "it adds the files and web addresses a QGIS project names"
                    } else {
                        "it adds the files and web addresses a GeoLibre project names"
                    }
                ));
            }
            let review = plan.review(decision == Decision::Remember)?;
            Ok(Outcome::NeedsApproval(Pending { plan: Arc::new(plan), review: Box::new(review), request: call.request.clone() }))
        })();
        result.unwrap_or_else(Outcome::Refused)
    }
    /// Consumes one displayed approval after checking its queued request identity.
    pub fn run_permitted(&mut self, scope: Scope, permit: Permit, call: &Call) -> CmdResult {
        if permit.request != call.request {
            return Err("this approval belongs to another request".into());
        }
        self.run_plan(scope, permit.plan, call)
    }
    /// Reuses consent only when the entire current operation equals the remembered plan.
    pub fn run_remembered(&mut self, scope: Scope, remembered: &Remembered, call: &Call) -> CmdResult {
        self.run_plan(scope, remembered.plan.clone(), call)
    }
    /// Replans before any effect and consumes ordered frozen nodes during execution.
    fn run_plan(&mut self, scope: Scope, plan: Arc<Plan>, call: &Call) -> CmdResult {
        self.run_plan_with(scope, plan, call, |s| s.run_call(call))
    }
    /// Shared exact-plan entry for command execution and one-use adapter capture.
    fn run_plan_with<T>(
        &mut self,
        scope: Scope,
        plan: Arc<Plan>,
        call: &Call,
        f: impl FnOnce(&mut Session) -> Result<T, String>,
    ) -> Result<T, String> {
        if !self.policy.scopes.is_empty() || self.policy.cursor.is_some() {
            return Err("an approval cannot widen a nested scope".into());
        }
        let current = self.plan(&scope, call)?;
        if *plan != current {
            return Err("changed since you approved it; ask again".into());
        }
        // Recheck live denial at use, without turning the approval into another policy setting.
        plan.decision(self, &scope)?;
        let designated = plan.designated.clone();
        self.policy.cursor = Some(Cursor {
            plan: plan.clone(),
            frames: vec![(vec![plan.root.clone()], 0)],
            designated,
            outputs: Default::default(),
            made_layers: Vec::new(),
            made_tables: Vec::new(),
        });
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.with_scope(scope, f)));
        self.policy.cursor = None;
        match r {
            Ok(r) => r?,
            Err(p) => std::panic::resume_unwind(p),
        }
    }
    /// Captures one adapter's original authority and generation before handing it to a queue.
    pub fn defer(&mut self, scope: Scope, call: &Call, permit: Option<Permit>) -> Result<super::Deferred, String> {
        if !matches!(call.kind, CallKind::Adapter { .. }) {
            return Err("only an adapter can defer its completion".into());
        }
        let start = |s: &mut Session| {
            let node = s.call_node(&s.scope(), call)?;
            let node = s.admit(node)?;
            s.with_node(node, |s| s.continuation().map(super::Deferred))
        };
        match permit {
            Some(p) => {
                if p.request != call.request {
                    return Err("this approval belongs to another request".into());
                }
                self.run_plan_with(scope, p.plan, call, start)
            }
            None => self.with_scope(scope, start)?,
        }
    }
    /// Consumes a stored completion at its real landing; root UI delivery may resume its owner.
    pub fn finish_deferred<T>(&mut self, completion: super::Deferred, f: impl FnOnce(&mut Session) -> T) -> Result<T, String> {
        self.check_delivery(&completion.0)?;
        if let Op::Adapter(what @ ("snapshot" | "view_map")) = &completion.0.node.op {
            // These callbacks render the live map, rather than an owned export snapshot.
            let call = Call::adapter(what, completion.0.node.effects, completion.0.node.params.clone());
            if *completion.0.node != self.call_node(&completion.0.scope, &call)? {
                return Err("the map sources changed since this image request was approved".into());
            }
        }
        self.resume(&completion.0, f)
    }
    /// Executes a call already admitted by its ingress scope; Ask at dispatch still refuses.
    pub fn run_scoped(&mut self, scope: Scope, call: &Call) -> CmdResult {
        self.with_scope(scope, |s| s.run_call(call))?
    }
    /// Dispatches command, script or adapter vehicles without promoting their ingress.
    fn run_call(&mut self, call: &Call) -> CmdResult {
        match &call.kind {
            CallKind::Command { id, params } => self.run(id, params),
            CallKind::Script(script) => {
                // The MCP vehicle is not the reserved script.run command; its lines use the seam.
                let node = self.call_node(&self.scope(), call)?;
                let decision = node.decision(self, &self.scope())?;
                if self.policy.cursor.is_none() && decision != Decision::Allow {
                    return Err("this operation needs your approval".into());
                }
                let node = self.admit(node)?;
                // Its lines run nested: what they couldn't read as true or false is said here (#1600).
                self.with_unread_bools(Some("script.run"), |s| {
                    s.with_node(node, |s| {
                        s.guarded("Run Script", true, |s| crate::cmd::workflows::run_script(s, &json!({"script":script})))
                    })
                })
            }
            CallKind::Adapter { .. } => {
                let node = self.call_node(&self.scope(), call)?;
                let decision = node.decision(self, &self.scope())?;
                if self.policy.cursor.is_none() && decision != Decision::Allow {
                    return Err("this operation needs your approval".into());
                }
                let node = self.admit(node)?;
                self.with_node(node, |_| Ok(Value::Null))
            }
        }
    }
    /// Checks live authority, ordered operation, effective params and bound targets at each stage.
    fn admit(&mut self, mut node: Node) -> Result<Arc<Node>, String> {
        let scope = self.scope();
        let decision = node.decision(self, &scope)?;
        let current_sources = self
            .project
            .maps
            .iter()
            .flat_map(|m| m.layers.iter().map(move |l| (LayerKey::of(m, l), super::plan::private_source(l))))
            .collect::<Vec<_>>();
        let selected = self
            .selected_layers()
            .into_iter()
            .chain(self.active_layer)
            .filter_map(|id| self.map().layer(id))
            .map(|l| LayerKey::of(self.map(), l))
            .collect::<Vec<_>>();
        let layers = self.map().layers.iter().map(|l| (l.name.clone(), LayerKey::of(self.map(), l))).collect::<Vec<_>>();
        let tables = self.project.tables.iter().map(|t| (t.dataset.name.clone(), t.id)).collect::<Vec<_>>();
        let expected = self.policy.cursor.as_ref().and_then(|c| c.frames.last()).and_then(|(n, at)| n.get(*at)).cloned();
        // A composite may not replace a later item's code (or its own next cell) after review.
        for n in self.policy.nodes.iter().map(Arc::as_ref).chain(expected.as_ref()) {
            if let Some((kind, name, trusted, content)) = &n.item {
                let graph = *kind == "model" && n.params.get("graph").is_some_and(|g| !g.is_null());
                let current = if graph { n.params.get("graph").cloned() } else { self.item_content(kind, name) };
                if current.as_ref() != Some(content) || (!graph && self.item_trusted(kind, name) != *trusted) {
                    return Err("the task, notebook or model changed since approval; ask again".into());
                }
            }
        }
        let mut wanted = expected.as_ref().map(|n| n.params.clone()).unwrap_or(Value::Null);
        if let Some(expected) = &expected {
            for wire in &expected.wires {
                let mut names = Vec::new();
                for from in &wire.from {
                    let from = match from {
                        OutputRef::Node(n) => self
                            .policy
                            .cursor
                            .as_ref()
                            .and_then(|c| c.outputs.get(n))
                            .ok_or("the model's earlier node made no layer or table")?,
                        other => other,
                    };
                    let name = match from {
                        OutputRef::ExistingLayer(key) => layers.iter().find(|(_, n)| n == key).map(|(name, _)| name),
                        OutputRef::ExistingTable(id) => tables.iter().find(|(_, n)| n == id).map(|(name, _)| name),
                        OutputRef::Node(_) => None,
                    }
                    .ok_or("the model's wired output no longer exists")?;
                    names.push(name.clone());
                }
                wanted[&wire.key] = json!(names.join(";"));
                // The wire is a data target, never an externally chosen place.
                node.places.retain(|v| v.get(&wire.key).is_none());
            }
        }
        let opened = self.opened;
        let map = self.map().instance_id();
        let accounts = self.policy_accounts();
        let s3 = crate::render::rustgis_tiles::s3::connections();
        if let Some(cursor) = &mut self.policy.cursor {
            if opened != cursor.plan.opened {
                return Err("the project or active map changed since approval".into());
            }
            if cursor.plan.accounts != accounts || cursor.plan.s3 != s3 {
                return Err("the live source or account changed since approval".into());
            }
            let (nodes, at) = cursor.frames.last_mut().ok_or("the approved plan has finished")?;
            let expected = nodes.get(*at).ok_or("an unplanned operation was requested; ask again")?;
            if map != expected.context {
                return Err("the active map does not match this approved stage".into());
            }
            if expected.op != node.op || wanted != node.params || expected.effects != node.effects || expected.places != node.places {
                return Err("changed since you approved it; ask again".into());
            }
            for target in &expected.targets {
                match target {
                    Target::Default { active, .. } => {
                        if expected
                            .effects
                            .iter()
                            .any(|e| matches!(e, super::Effect::Files | super::Effect::Network | super::Effect::Plugin))
                            && selected.iter().any(|key| {
                                cursor.plan.sources.iter().find(|(k, _)| k == key).is_some_and(|old| !current_sources.contains(old))
                            })
                        {
                            return Err("the default layer's private source changed since approval".into());
                        }
                        if selected.iter().any(|key| !cursor.designated.contains(key) && !cursor.made_layers.contains(key)) {
                            return Err("this step acts on whichever layer an earlier step left active; name the layer in the step".into());
                        }
                        if self.active_layer.is_none() && active.is_some() {
                            return Err("the planned default layer no longer exists; name the layer in the step".into());
                        }
                    }
                    Target::Layer { key, source } => {
                        if !node
                            .targets
                            .iter()
                            .any(|t| matches!(t,Target::Layer{key:now,source:now_source}if now==key && source==now_source))
                        {
                            return Err("the step's layer changed since approval".into());
                        }
                        cursor.designated.push(*key);
                    }
                    Target::Table { id, source } => {
                        if !node.targets.iter().any(|t| matches!(t,Target::Table{id:now,source:now_source}if now==id && source==now_source))
                        {
                            return Err("the step's table changed since approval".into());
                        }
                    }
                    Target::Map(_) | Target::TimeSources(_) | Target::LayerInMap { .. } => {
                        if !node.targets.contains(target) {
                            return Err("the step's map or time source changed since approval".into());
                        }
                        if let Target::LayerInMap { map, id, instance, .. } = target {
                            cursor.designated.push(LayerKey { map: *map, id: *id, instance: *instance });
                        }
                    }
                    Target::MadeLayer { map, name } => {
                        if !layers
                            .iter()
                            .any(|(n, key)| n.eq_ignore_ascii_case(name) && key.map == *map && cursor.made_layers.contains(key))
                        {
                            return Err("the planned output layer was not made by this run".into());
                        }
                    }
                    Target::ProducedDataset { producer, map, .. } => {
                        let matches = match cursor.outputs.get(producer) {
                            Some(OutputRef::ExistingLayer(key)) if key.map == *map => {
                                node.targets.iter().any(|t| matches!(t, Target::Layer { key: now, .. } if now == key))
                            }
                            Some(OutputRef::ExistingTable(id)) => {
                                node.targets.iter().any(|t| matches!(t, Target::Table { id: now, .. } if now == id))
                            }
                            _ => false,
                        };
                        if !matches {
                            return Err("the planned dataset was not produced by its approved tool".into());
                        }
                    }
                    Target::MadeTable(name) => {
                        if !crate::project::find_table(&self.project.tables, name)
                            .and_then(|i| self.project.tables.get(i))
                            .is_some_and(|t| t.id > cursor.plan.last_table && cursor.made_tables.contains(&t.id))
                        {
                            return Err("the planned output table was not made by this run".into());
                        }
                    }
                }
            }
            *at += 1;
            node = expected.clone();
        } else if decision != Decision::Allow {
            return Err("this operation needs your approval".into());
        }
        Ok(Arc::new(node))
    }
    /// Keeps the admitted node and synchronous request predicate through nested execution; restores both on panic.
    fn with_node<T>(&mut self, node: Arc<Node>, f: impl FnOnce(&mut Session) -> Result<T, String>) -> Result<T, String> {
        let before_layers = self.project.maps.iter().flat_map(|m| m.layers.iter().map(move |l| LayerKey::of(m, l))).collect::<Vec<_>>();
        let before_tables = self.project.tables.iter().map(|t| t.id).collect::<Vec<_>>();
        let continuation = (!matches!(self.scope().kind, Kind::User)).then(|| self.continuation_for(node.clone())).transpose()?;
        let depth = self.policy.nodes.len();
        self.policy.nodes.push(node.clone());
        if let Some(cursor) = &mut self.policy.cursor {
            cursor.frames.push((node.children.clone(), 0));
        }
        let still = continuation.map(|c| Arc::new(move || c.check().is_ok()) as crate::render::rustgis_tiles::cache::Still);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::render::rustgis_tiles::cache::with_optional_still(still, || f(self))
        }));
        let complete =
            if let Some(cursor) = &mut self.policy.cursor { cursor.frames.pop().is_none_or(|(n, at)| n.len() == at) } else { true };
        if let Some(cursor) = &mut self.policy.cursor {
            cursor.made_layers.extend(
                self.project
                    .maps
                    .iter()
                    .flat_map(|m| m.layers.iter().map(move |l| LayerKey::of(m, l)))
                    .filter(|key| !before_layers.contains(key)),
            );
            cursor.made_tables.extend(self.project.tables.iter().map(|t| t.id).filter(|id| !before_tables.contains(id)));
        }
        self.policy.nodes.truncate(depth);
        match r {
            Ok(Ok(_)) if !complete => Err("the approved plan did not run all its frozen steps".into()),
            Ok(r) => r,
            Err(p) => std::panic::resume_unwind(p),
        }
    }
    /// Executes only the engine-held download owner and its original address.
    pub(super) fn run_url_completion(&mut self, id: u64, command: &str, params: &Value) -> CmdResult {
        let c = self.claim_for(id).ok_or("no claimed download with that id")?;
        self.continuation_owner(&c)?;
        let params = self.download_params(id, params, command == "layer.addUrl")?;
        let spec = *self.registry.get(command).ok_or("no URL completion command")?;
        let op = Authorized { op: CommandOp::Registered { spec, params: params.clone() }, node: c.node.clone() };
        self.invoke(op, Txn::Own)
    }
    /// Resolves the command once or retrieves a restricted owner-only completion token.
    pub(crate) fn authorize(&mut self, id: &str, params: &Value) -> Result<Authorized<CommandOp>, String> {
        if let Some(id_of_run) = params.get("ran").and_then(Value::as_u64)
            && let Some(c) = self.raster_results.iter().find(|(n, _)| *n == id_of_run).and_then(|(_, m)| m.continuation.clone())
        {
            self.continuation_owner(&c)?;
            if id != c.node.id() {
                return Err("that run made another command's raster".into());
            }
            if params.as_object().is_none_or(|p| p.iter().any(|(key, value)| key != "ran" && c.node.params.get(key) != Some(value))) {
                return Err("the raster completion's command or parameters changed".into());
            }
            let mut original = c.node.params.clone();
            original["ran"] = json!(id_of_run);
            let Op::Command(spec) = &c.node.op else { return Err("not a raster command continuation".into()) };
            return Ok(Authorized { op: CommandOp::Registered { spec: *spec, params: original }, node: c.node.clone() });
        }

        if id == "layer.addDownload"
            && let Some(key) = params.get("id").and_then(Value::as_u64)
            && let Some(c) = self.claim_for(key)
        {
            self.continuation_owner(&c)?;
            let params = self.download_params(key, params, false)?;
            let spec = *self.registry.get(id).ok_or("no download completion command")?;
            return Ok(Authorized { op: CommandOp::Registered { spec, params: params.clone() }, node: c.node.clone() });
        }
        if id == "gp.finish"
            && let Some(run_id) = params.get("id").and_then(Value::as_u64)
            && let Some(r) = self.pending.iter().find(|r| r.id == run_id)
        {
            self.check_gp_finish(&r.continuation, crate::cmd::gp::landing(&r.result))?;
            let spec = *self.registry.get(id).ok_or("no tool completion command")?;
            return Ok(Authorized { op: CommandOp::Registered { spec, params: params.clone() }, node: r.continuation.node.clone() });
        }
        if id == "gp.fileDelivered"
            && let Some(run_id) = params.get("id").and_then(Value::as_u64)
            && let Some(r) = self.file_outputs.iter().find(|r| r.id == run_id)
        {
            self.gp_file_owner(&r.continuation)?;
            let spec = *self.registry.get(id).ok_or("no file completion command")?;
            return Ok(Authorized { op: CommandOp::Registered { spec, params: params.clone() }, node: r.continuation.node.clone() });
        }
        let node = self.command_node(id, params)?;
        let node = self.admit(node)?;
        let op = match &node.op {
            Op::Command(spec) => CommandOp::Registered { spec: *spec, params: params.clone() },
            Op::Plugin(cmd, pin) => CommandOp::Plugin { cmd: cmd.clone(), pin: pin.clone(), params: params.clone() },
            _ => return Err("not a command".into()),
        };
        Ok(Authorized { op, node })
    }
    /// Pins schema/runner/load and effective parameters before granting a raw tool token.
    pub(crate) fn authorize_tool(&mut self, tool: &str, params: &Value) -> Result<Authorized<ToolOp>, String> {
        let node = self.tool_node(tool, params)?;
        let node = self.admit(node)?;
        let Op::Tool(pin) = &node.op else { return Err("not a tool".into()) };
        Ok(Authorized { op: ToolOp { pin: pin.clone(), params: pin.resolved.resolve_params(params)? }, node })
    }
    /// The synthetic clicked-point input uses only the builtin captured by its command plan.
    pub(crate) fn point_tool(&self, params: &Value) -> Result<Authorized<ToolOp>, String> {
        let node = self.policy.nodes.last().cloned().ok_or("a point buffer needs its admitted command")?;
        if node.id() != "gp.bufferPoint" {
            return Err("not a point buffer command".into());
        }
        let pin = node.fixed_tool.as_ref().ok_or("the command has no captured buffer tool")?;
        pin.current()?;
        Ok(Authorized { op: ToolOp { pin: pin.clone(), params: pin.resolved.resolve_params(params)? }, node })
    }
    /// Runs a pinned tool and records symbolic output identities for later frozen wires.
    pub(crate) fn invoke_tool(&mut self, op: Authorized<ToolOp>, control: &Arc<rustgis_analysis::control::Control>) -> CmdResult {
        self.invoke_tool_with(op, control, &crate::cmd::gp::RunOpts::default()).map(|(value, _)| value)
    }
    /// [`Self::invoke_tool`] applied as `opts` says (a batch item), returning the run's messages.
    pub(crate) fn invoke_tool_with(
        &mut self,
        op: Authorized<ToolOp>,
        control: &Arc<rustgis_analysis::control::Control>,
        opts: &crate::cmd::gp::RunOpts,
    ) -> Result<(Value, Vec<String>), String> {
        let node = op.node.clone();
        let number = node.number;
        let result = self.with_node(node, |s| crate::cmd::gp::execute(s, op, control, opts));
        if let Ok((value, _)) = &result {
            // Engine apply's result identifies the actual output kind. An identically named
            // layer must never substitute for a produced standalone table (or vice versa).
            let layer = value
                .get("layer")
                .or_else(|| value.get("layers").and_then(|v| v.get(0)))
                .and_then(Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
                .and_then(|id| self.map().layer(id))
                .map(|l| OutputRef::ExistingLayer(LayerKey::of(self.map(), l)));
            let table = value
                .get("table")
                .or_else(|| value.get("tables").and_then(|v| v.get(0)))
                .and_then(Value::as_str)
                .and_then(|name| self.project.tables.iter().find(|t| t.dataset.name == name))
                .map(|t| OutputRef::ExistingTable(t.id));
            if let (Some(cursor), Some(out)) = (&mut self.policy.cursor, layer.or(table)) {
                cursor.outputs.insert(number, out);
            }
        }
        result
    }
    /// Native raster starters admit the command before reading any inputs or preparing work.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn start_raster(&mut self, command: &str, params: &Value) -> Result<crate::cmd::workflows::RasterRun, String> {
        let op = self.authorize(command, params)?;
        let CommandOp::Registered { spec, params } = op.op else { return Err("not a registered raster command".into()) };
        let named = self.layer_id(&params).ok();
        let active = self.active_layer;
        if named.is_some() {
            self.active_layer = named;
        }
        let enabled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (spec.enabled)(self))).unwrap_or(Some("unavailable"));
        self.active_layer = active;
        if let Some(why) = enabled {
            return Err(format!("{}: {why}", spec.label));
        }
        if let Some(error) = self.streamed_edit(spec.id, spec.mutates, spec.label, named.or(active)) {
            return Err(error);
        }
        let node = op.node;
        let source = Some(spec.id);
        let op = Authorized { op: RasterOp { spec, params }, node: node.clone() };
        // Prepared outside a command: what it couldn't read as true or false is said here (#1600).
        self.with_unread_bools(source, |s| s.with_node(node, |s| crate::cmd::workflows::raster_start_raw(s, op)))
    }
    /// A UI completion resumes the stored owner; scoped callers must already be that owner.
    pub fn finish_raster(&mut self, command: &str, id: u64, params: &Value) -> CmdResult {
        let c = self
            .raster_results
            .iter()
            .find(|(n, _)| *n == id)
            .and_then(|(_, m)| m.continuation.clone())
            .ok_or("no pending raster with that id")?;
        if !self.policy.scopes.is_empty() {
            self.continuation_owner(&c)?;
        }
        let mut params = params.clone();
        params.as_object_mut().ok_or("raster completion parameters must be an object")?.insert("ran".into(), json!(id));
        self.resume(&c, |s| s.run(command, &params))?
    }
    /// Builds a raw model token only inside its admitted model.run node.
    pub(crate) fn authorize_model(&mut self, p: &Value) -> Result<Authorized<ModelOp>, String> {
        let (name, graph, _) = crate::cmd::workflows::model_graph(self, p)?;
        let node = self.policy.nodes.last().cloned().ok_or("a model must run through model.run")?;
        Ok(Authorized { op: ModelOp { name, graph }, node })
    }
    /// The sole Handler call seam; preserves transaction ownership and provenance rollback.
    pub(crate) fn invoke(&mut self, op: Authorized<CommandOp>, txn: Txn) -> CmdResult {
        let node = op.node.clone();
        match &op.op {
            CommandOp::Plugin { pin, .. } => {
                let pin = pin.clone();
                // Plugin host calls are checked against the pinned manifest, not an invented
                // finite list of calls made by code that has yet to run.
                let cursor = self.policy.cursor.take();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.with_node(node, |s| s.narrowed(Narrow::Plugin(pin), |s| s.run_plugin(op))?)
                }));
                self.policy.cursor = cursor;
                match r {
                    Ok(r) => r,
                    Err(p) => std::panic::resume_unwind(p),
                }
            }
            CommandOp::Registered { spec, params } => {
                let (spec, params) = (*spec, params.clone());
                if let Some(error) = self.url_completion_refusal(spec.id, &params)? {
                    return Err(error);
                }
                let run = |s: &mut Session| {
                    s.with_node(node, |s| {
                        let editor = s.trust_editor(spec.id, &params);
                        let add = |s: &mut Session| {
                            let before = crate::service_logins::before(s, spec.id, &params)?;
                            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spec.run.call(s, &params)))
                                .unwrap_or_else(|_| Err(format!("{} failed unexpectedly", spec.label)));
                            // A nested plugin command may fail after making additions. Its outer
                            // transaction owns rollback; mark those additions anonymous meanwhile.
                            if let Some(before) = before {
                                crate::service_logins::tag(s, &params, before, result.as_ref().unwrap_or(&Value::Null))?;
                            }
                            result
                        };
                        let anonymous = s.web_logins.is_anonymous()
                            || (matches!(spec.id, "layer.addUrl" | "layer.addDownload")
                                && params.get("anonymous").and_then(Value::as_bool) == Some(true));
                        let result = crate::service_logins::anonymously(s, anonymous, add)
                            .unwrap_or_else(|_| Err(format!("{} failed unexpectedly", spec.label)))
                            .map_err(|e| crate::durable::generated_text(&e));
                        if let Ok(value) = &result {
                            s.trust_after(spec.id, &params, value, editor);
                        }
                        result
                    })
                };
                let result = match txn {
                    Txn::Own => self.guarded(spec.label, spec.mutates, run),
                    Txn::Shared => run(self),
                };
                self.url_completion_landed(spec.id, &params, &result);
                result
            }
        }
    }
    /// Invalidates off-thread predicates whenever the active project generation changes.
    pub(crate) fn sync_policy_generation(&self) {
        self.policy.epoch.store(self.opened, std::sync::atomic::Ordering::SeqCst);
    }
    /// Copies the admitted tool authority without consulting its public name again.
    pub(crate) fn tool_continuation(&self, op: &Authorized<ToolOp>) -> Result<Continuation, String> {
        self.continuation_for(op.node.clone())
    }
    /// Own private identity snapshots cross workers; display copies never become authority.
    fn continuation_for(&self, node: Arc<Node>) -> Result<Continuation, String> {
        let reads = node.effects.iter().any(|e| matches!(e, super::Effect::Files | super::Effect::Network | super::Effect::Plugin));
        let mut sources = Vec::new();
        let mut tables = Vec::new();
        if reads {
            for target in &node.targets {
                match target {
                    Target::Layer { key, .. } => {
                        if let Some(layer) = key.get(self) {
                            sources.push((*key, super::plan::private_source(layer)));
                        }
                    }
                    Target::Table { id, .. } => {
                        if let Some(t) = self.project.tables.iter().find(|t| t.id == *id) {
                            tables.push((*id, json!(t.dataset.source)));
                        }
                    }
                    Target::LayerInMap { map, id, .. } => {
                        if let Some(layer) = self.project.maps.iter().find(|m| m.instance_id() == *map).and_then(|m| m.layer(*id)) {
                            sources
                                .push((LayerKey { map: *map, id: *id, instance: layer.instance_id() }, super::plan::private_source(layer)));
                        }
                    }
                    Target::Default { .. } => {
                        for id in self.selected_layers().into_iter().chain(self.active_layer) {
                            if let Some(layer) = self.map().layer(id) {
                                sources.push((LayerKey::of(self.map(), layer), super::plan::private_source(layer)));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(Continuation {
            scope: self.scope(),
            node: node.clone(),
            opened: self.opened,
            map: self.map().instance_id(),
            epoch: self.policy.epoch.clone(),
            accounts: if reads { self.policy_accounts() } else { Value::Null },
            sources,
            time_sources: node_time_sources(&node),
            tables,
            s3: reads.then(|| crate::render::rustgis_tiles::s3::account_identity().map(|identity| vec![identity])).transpose()?,
        })
    }
    /// Native UI delivery re-enters the engine-held owner; a JSON completion cannot select it.
    pub fn finish_gp(&mut self, id: u64) -> CmdResult {
        let r = self.pending.iter().find(|r| r.id == id).ok_or("no finished run with that id")?;
        let (c, landing) = (r.continuation.clone(), crate::cmd::gp::landing(&r.result));
        self.check_gp_delivery(&c)?;
        if c.native_tool_owner() && self.policy.scopes.is_empty() {
            // Root is already the same native User, including for failure-only delivery.
            return self.resume_gp(&c, landing, |s| s.run("gp.finish", &json!({"id":id})))?;
        }
        self.resume(&c, |s| s.run("gp.finish", &json!({"id":id})))?
    }
    /// Applies delivery status under the stored owner, after a live/generation check.
    pub fn acknowledge_gp_file(&mut self, id: u64, error: Option<&str>) -> CmdResult {
        let c = self.file_outputs.iter().find(|r| r.id == id).ok_or("no pending file with that id")?.continuation.clone();
        self.check_export_delivery(&c)?;
        self.resume_checked(&c, |s| s.run("gp.fileDelivered", &json!({"id":id,"error":error})))?
    }
    /// Retries the same pinned plugin command and exact parameters under its initiating scope.
    pub(crate) fn retry_continuation(&mut self, c: &Continuation, params: &Value) -> CmdResult {
        self.command_epilogue(c.node.id(), params, |s| {
            s.check_continuation(c)?;
            if &c.node.params != params {
                return Err("the retry's parameters changed".into());
            }
            let Op::Plugin(cmd, pin) = &c.node.op else { return Err("not a plugin continuation".into()) };
            let op =
                Authorized { op: CommandOp::Plugin { cmd: cmd.clone(), pin: pin.clone(), params: params.clone() }, node: c.node.clone() };
            s.resume(c, |s| s.run_plugin(op))?
        })
    }
    /// Applies the engine decision to non-command plugin host capabilities.
    pub(crate) fn host_effect(&self, effects: super::Effects, what: &'static str) -> Result<(), String> {
        let node = Node {
            context: self.map().instance_id(),
            line: None,
            op: Op::Adapter(what),
            params: Value::Null,
            effects,
            targets: Vec::new(),
            places: Vec::new(),
            display: Default::default(),
            children: Vec::new(),
            item: None,
            generated: false,
            number: 0,
            wires: Vec::new(),
            graph_output: false,
            fixed_tool: None,
            auth: None,
        };
        if decide::decide(self, &self.scope(), &node)? != Decision::Allow {
            return Err("this host request needs approval".into());
        }
        Ok(())
    }
    /// Captures restricted authority only while an admitted operation is executing.
    pub(crate) fn continuation(&self) -> Result<Continuation, String> {
        let node = self.policy.nodes.last().cloned().ok_or("late work needs an authorized initiating operation")?;
        self.continuation_for(node)
    }
    /// Checks generation, map, revocation, current load and private source/account identity.
    pub(crate) fn check_continuation(&self, c: &Continuation) -> Result<(), String> {
        self.check_continuation_origin(c)?;
        if c.map != self.map().instance_id() {
            return Err("this run belongs to a replaced project or another map".into());
        }
        Ok(())
    }
    /// Checks live authority and the original identities without routing into the currently displayed map.
    pub(crate) fn check_continuation_origin(&self, c: &Continuation) -> Result<(), String> {
        self.check_continuation_accounts(c)?;
        if c.sources.iter().any(|(key, source)| key.get(self).is_none_or(|l| super::plan::private_source(l) != *source)) {
            return Err("the live source or account changed since this request started".into());
        }
        if c.time_sources.as_ref().is_some_and(|old| {
            *old != crate::cmd::time::following_sources(self).into_iter().map(|(m, l, s)| (m, l, s.to_string())).collect::<Vec<_>>()
        }) {
            return Err("the live map or time source changed since this request started".into());
        }
        if c.tables
            .iter()
            .any(|(id, source)| self.project.tables.iter().find(|t| t.id == *id).is_none_or(|t| json!(t.dataset.source) != *source))
        {
            return Err("the live table source changed since this request started".into());
        }
        Ok(())
    }
    /// Lifetime, load and exact active accounts apply even to failure-only owned tool delivery.
    pub(crate) fn check_continuation_accounts(&self, c: &Continuation) -> Result<(), String> {
        c.check()?;
        if c.opened != self.opened {
            return Err("this run belongs to a replaced project".into());
        }
        if !c.accounts.is_null() && c.accounts != self.policy_accounts() {
            return Err("the live source or account changed since this request started".into());
        }
        Ok(())
    }
    /// GP owns its input snapshot. Native delivery still checks lifetime, loads and accounts;
    /// the GP owner fits its result to the original map and source before application.
    pub(crate) fn check_gp_delivery(&self, c: &Continuation) -> Result<(), String> {
        if !self.policy.scopes.is_empty() && !self.scope().same_caller(&c.scope) {
            return Err("this run belongs to another caller".into());
        }
        if c.native_tool_owner() { self.check_continuation_accounts(c) } else { self.check_continuation(c) }
    }
    /// Rebinds only native owned inputs after the GP owner has checked actual source identity.
    /// Missing inputs are valid for a new output from the owned snapshot; updates are fitted by GP.
    pub(crate) fn refit_gp_sources(
        &self,
        c: &mut Continuation,
        read: &std::collections::HashMap<String, crate::cmd::gp::Read>,
    ) -> Result<(), String> {
        if !c.native_tool_owner() {
            return self.check_continuation(c);
        }
        let mut sources = Vec::new();
        for (key, old) in &c.sources {
            if let Some(layer) = key.get(self) {
                let now = super::plan::private_source(layer);
                if now != *old && !read.values().any(|r| r.materialized(key.id, key.instance, layer)) {
                    return Err("the live source or account changed since this request started".into());
                }
                sources.push((*key, now));
            }
        }
        let mut tables = Vec::new();
        for (id, old) in &c.tables {
            if let Some(table) = self.project.tables.iter().find(|t| t.id == *id) {
                if json!(table.dataset.source) != *old {
                    return Err("the live table source changed since this request started".into());
                }
                tables.push((*id, old.clone()));
            }
        }
        c.sources = sources;
        c.tables = tables;
        Ok(())
    }
    /// The native GP owner distinguishes map edits, project-wide outputs and failure-only delivery.
    /// Its sealed result chooses the route; source/account/lifetime checks remain at the applicable boundary.
    pub(crate) fn check_gp_finish(&self, c: &Continuation, landing: crate::cmd::gp::Landing) -> Result<(), String> {
        if !self.scope().same_caller(&c.scope) {
            return Err("this run belongs to another caller".into());
        }
        if c.native_tool_owner() {
            match landing {
                crate::cmd::gp::Landing::Failure => self.check_continuation_accounts(c),
                crate::cmd::gp::Landing::Project => self.check_continuation_origin(c),
                crate::cmd::gp::Landing::Map => self.check_continuation(c),
            }
        } else {
            self.check_continuation(c)
        }
    }
    /// Re-enters an owned GP completion, retaining actor and panic-safe scope restoration.
    pub(crate) fn resume_gp<T>(
        &mut self,
        c: &Continuation,
        landing: crate::cmd::gp::Landing,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Result<T, String> {
        self.check_gp_finish(c, landing)?;
        self.resume_checked(c, f)
    }
    /// Refuses another caller without consuming the rightful owner's result.
    pub(crate) fn continuation_owner(&self, c: &Continuation) -> Result<(), String> {
        if !self.scope().same_caller(&c.scope) {
            return Err("this run belongs to another caller".into());
        }
        self.check_continuation(c)
    }
    /// Refuses foreign ingress before a failed start may retire its owner's queued work.
    pub(crate) fn check_delivery_owner(&self, c: &Continuation) -> Result<(), String> {
        if !self.policy.scopes.is_empty() && !self.scope().same_caller(&c.scope) {
            return Err("this run belongs to another caller".into());
        }
        Ok(())
    }
    /// Native workers may deliver at root; an existing ingress must own the stored continuation.
    pub(crate) fn check_delivery(&self, c: &Continuation) -> Result<(), String> {
        self.check_delivery_owner(c)?;
        self.check_continuation(c)
    }
    /// Re-enters stored authority synchronously and restores stacks even on panic.
    pub(crate) fn resume<T>(&mut self, c: &Continuation, f: impl FnOnce(&mut Session) -> T) -> Result<T, String> {
        self.check_continuation(c)?;
        self.resume_checked(c, f)
    }
    /// Restores the stored principal and actor after the owner-specific admission has succeeded.
    fn resume_checked<T>(&mut self, c: &Continuation, f: impl FnOnce(&mut Session) -> T) -> Result<T, String> {
        let n = self.policy.nodes.len();
        self.policy.nodes.push(c.node.clone());
        let owned = c.clone();
        let still = Arc::new(move || owned.check().is_ok()) as crate::render::rustgis_tiles::cache::Still;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::render::rustgis_tiles::cache::with_still(still, || self.in_scope(c.scope.clone(), f))
        }));
        self.policy.nodes.truncate(n);
        match r {
            Ok(r) => r,
            Err(p) => std::panic::resume_unwind(p),
        }
    }
}

/// Retains exactly the time-following set chosen at initiation, including secondary maps.
fn node_time_sources(n: &Node) -> Option<Vec<(u64, u32, String)>> {
    n.targets.iter().find_map(|t| match t {
        Target::TimeSources(s) => Some(s.clone()),
        _ => None,
    })
}
