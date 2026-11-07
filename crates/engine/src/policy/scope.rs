use super::{
    Effects, Review,
    plan::{Node, Plan, PluginPin},
};
use std::sync::Arc;

/// Exact in-memory authentication state. Never serialized, displayed or used as a public label.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct RuntimeAccounts {
    arcgis: crate::render::rustgis_tiles::arcgis_auth::AccountIdentity,
    earthdata: crate::render::rustgis_tiles::earthdata::AccountIdentity,
    web: crate::render::rustgis_tiles::logins::AccountIdentity,
}
impl RuntimeAccounts {
    /// Dynamic commands may choose multiple URLs, so their entire active stores are bound.
    pub(super) fn capture(s: &crate::Session) -> Result<Self, String> {
        Ok(Self {
            arcgis: crate::render::rustgis_tiles::arcgis_auth::account_identity()?,
            earthdata: crate::render::rustgis_tiles::earthdata::account_identity()?,
            web: crate::render::rustgis_tiles::logins::account_identity(s.web_logins.session_id())?,
        })
    }
    /// A rotation/replacement, including renewal, invalidates old consent and held sends.
    pub(super) fn is_current(&self) -> bool {
        self.arcgis.is_current() && self.earthdata.is_current() && self.web.is_current()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    User,
    Assistant,
    Agent { access: AgentAccess, users_project: bool },
    Plugin { id: String },
    Item { kind: &'static str, name: String },
    Peer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentAccess {
    Refuse,
    Review,
    Allow,
    Full,
}
/// Live authority, checked at dispatch and at the actual start and application of late work.
pub trait Revocation: Send + Sync {
    /// Answers from live adapter state at each new start or application, including workers.
    fn allows(&self, effects: Effects) -> bool;
}
#[derive(Clone)]
pub struct Scope {
    pub(super) kind: Kind,
    pub(super) owner: Owner,
    pub(super) live: Option<Arc<dyn Revocation>>,
}
#[derive(Clone)]
pub(super) enum Kind {
    User,
    Assistant,
    Agent { access: AgentAccess, users_project: bool, actor: Option<String> },
    Plugin { pin: PluginPin, within: Box<Kind> },
    Item { kind: &'static str, name: String, within: Box<Kind> },
    Peer,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Owner {
    pub kind: &'static str,
    pub id: String,
}
impl Scope {
    /// Undo attribution follows the initiating agent through plugin/item narrowing.
    pub(super) fn actor(&self) -> Option<String> {
        fn actor(kind: &Kind) -> Option<&str> {
            match kind {
                Kind::Agent { actor, .. } => actor.as_deref(),
                Kind::Plugin { within, .. } | Kind::Item { within, .. } => actor(within),
                _ => None,
            }
        }
        actor(&self.kind).map(str::to_owned)
    }
    /// Names one assistant conversation and its revocable lifetime.
    pub fn assistant(conversation: u64, live: Arc<dyn Revocation>) -> Self {
        Self { kind: Kind::Assistant, owner: Owner { kind: "assistant", id: conversation.to_string() }, live: Some(live) }
    }
    /// Names an explicit ingress grant, actor and access setting; payload JSON cannot create it.
    pub fn agent(access: AgentAccess, users_project: bool, grant: &str, actor: Option<String>, live: Arc<dyn Revocation>) -> Self {
        Self { kind: Kind::Agent { access, users_project, actor }, owner: Owner { kind: "agent", id: grant.into() }, live: Some(live) }
    }
    /// Names project-text exchange as untrusted peer provenance.
    pub fn peer() -> Self {
        Self { kind: Kind::Peer, owner: Owner { kind: "peer", id: String::new() }, live: None }
    }
    /// Returns a display/debug identity without exposing authority construction fields.
    pub fn principal(&self) -> Principal {
        match &self.kind {
            Kind::User => Principal::User,
            Kind::Assistant => Principal::Assistant,
            Kind::Agent { access, users_project, .. } => Principal::Agent { access: *access, users_project: *users_project },
            Kind::Plugin { pin, .. } => Principal::Plugin { id: pin.0.manifest.id.clone() },
            Kind::Item { kind, name, .. } => Principal::Item { kind, name: name.clone() },
            Kind::Peer => Principal::Peer,
        }
    }
    /// Compares owner and narrowing chain, including exact plugin load incarnation.
    pub(super) fn same_caller(&self, other: &Self) -> bool {
        /// Recurses through plugin/item intersections without allowing a less restricted completion.
        fn same(a: &Kind, b: &Kind) -> bool {
            match (a, b) {
                (Kind::Plugin { pin: a, within: x }, Kind::Plugin { pin: b, within: y }) => a == b && same(x, y),
                (Kind::Item { kind: a, name: n, within: x }, Kind::Item { kind: b, name: m, within: y }) => a == b && n == m && same(x, y),
                (Kind::Plugin { .. } | Kind::Item { .. }, _) | (_, Kind::Plugin { .. } | Kind::Item { .. }) => false,
                _ => true,
            }
        }
        self.owner == other.owner && same(&self.kind, &other.kind)
    }
    /// The private conventional root used only when trusted native dispatch has no ingress scope.
    pub(super) fn user() -> Self {
        Self { kind: Kind::User, owner: Owner { kind: "user", id: String::new() }, live: None }
    }
    /// Lets adapters recheck live state immediately before their own late side effects.
    pub fn check_live(&self, effects: Effects) -> Result<(), String> {
        self.check(effects)
    }
    /// Checks adapter revocation and every plugin load in the narrowing chain.
    pub(super) fn check(&self, effects: Effects) -> Result<(), String> {
        if self.live.as_ref().is_some_and(|live| !live.allows(effects)) {
            return Err("this request's authority was revoked".into());
        }
        /// Checks current enabled plugin loads through nested item and plugin scopes.
        fn current(k: &Kind) -> bool {
            match k {
                Kind::Plugin { pin, within } => rustgis_plugins::is_current(&pin.0) && current(within),
                Kind::Item { within, .. } => current(within),
                _ => true,
            }
        }
        if !current(&self.kind) {
            return Err("the plugin was disabled, updated or removed since this request started".into());
        }
        Ok(())
    }
}
pub enum Outcome {
    Allowed,
    NeedsApproval(Pending),
    Refused(String),
}
pub struct Pending {
    pub(super) plan: Arc<Plan>,
    pub(super) review: Box<Review>,
    pub(super) request: String,
}
pub struct Permit {
    pub(super) plan: Arc<Plan>,
    pub(super) request: String,
}
#[derive(Clone)]
pub struct Remembered {
    pub(super) plan: Arc<Plan>,
}
impl Remembered {
    /// Prunes remembered operations after project replacement.
    pub fn belongs_to_generation(&self, generation: u64) -> bool {
        self.plan.opened == generation
    }
}
impl Pending {
    /// Borrows the sanitized card frozen when approval was first requested.
    pub fn review(&self) -> &Review {
        &self.review
    }
    /// Consumes this pending request into a non-cloneable, one-use permit.
    pub fn allow(self) -> Permit {
        Permit { plan: self.plan, request: self.request }
    }
    /// Retains exact consent only for rememberable Files/Network operations.
    pub fn remember(&self) -> Option<Remembered> {
        self.review.remember.then(|| Remembered { plan: self.plan.clone() })
    }
    /// Compares the complete raw plan, independently of the queued request id.
    pub fn covered_by(&self, remembered: &Remembered) -> bool {
        self.plan == remembered.plan
    }
}
/// Engine-owned authority for an already authorized operation. Never reconstructed from JSON.
#[derive(Clone)]
pub(crate) struct Continuation {
    pub(super) scope: Scope,
    pub(super) node: Arc<Node>,
    pub(super) opened: u64,
    pub(super) map: u64,
    pub(super) epoch: Arc<std::sync::atomic::AtomicU64>,
    pub(super) accounts: serde_json::Value,
    pub(super) sources: Vec<(super::plan::LayerKey, serde_json::Value)>,
    pub(super) tables: Vec<(u32, serde_json::Value)>,
    pub(super) time_sources: Option<Vec<(u64, u32, String)>>,
    pub(super) s3: Option<Vec<crate::render::rustgis_tiles::s3::AccountIdentity>>,
}
impl Continuation {
    /// Only an engine-created root native User may deliver owned tool snapshots this way.
    pub(crate) fn native_tool_owner(&self) -> bool {
        matches!(self.scope.kind, Kind::User)
    }
    /// Distinguishes two invocations of the same command without exposing their raw nodes.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn same_run(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
    }
    /// Checks adapter revocation and every plugin load in the narrowing chain.
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.epoch.load(std::sync::atomic::Ordering::SeqCst) != self.opened {
            return Err("this run belongs to a replaced project".into());
        }
        if self.s3.as_ref().is_some_and(|identities| identities.iter().any(|identity| !identity.is_current())) {
            return Err("the live storage account changed since this request started".into());
        }
        self.scope.check(self.node.effects)?;
        self.node.current()
    }
}
/// One-use completion for a native adapter's admitted asynchronous operation.
/// Fields and initiating authority are private; payloads cannot construct or widen it.
pub struct Deferred(pub(super) Continuation);

pub(crate) enum Narrow {
    Plugin(PluginPin),
    Item { kind: &'static str, name: String },
}
