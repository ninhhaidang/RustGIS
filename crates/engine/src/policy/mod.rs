//! Permission decisions and execution authority for commands and their continuations.
//!
//! Native user entry points may call `Session::run`. Untrusted adapters select a named
//! `Scope`, show the engine's `Review`, and consume its permit; nested work only narrows it.
mod decide;
mod dispatch;
pub(crate) mod downloads;
mod effects;
pub mod handler;
mod plan;
mod provenance;
mod scope;

pub use dispatch::PreparedCommand;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use dispatch::RasterOp;
pub(crate) use dispatch::{Authorized, CommandOp, ModelOp, State, ToolOp, Txn};
pub(crate) use effects::PLACE_KEYS;
pub use effects::{Effect, Effects};
pub use handler::Handler;
pub use plan::{Call, Review, ReviewDetail};
pub(crate) use plan::{Node, PluginPin, ToolPin};
pub(crate) use provenance::Trust;
pub use scope::{AgentAccess, Deferred, Outcome, Pending, Permit, Principal, Remembered, Revocation, Scope};
pub(crate) use scope::{Continuation, Narrow};

/// Compatibility projection for native metadata consumers; policy itself keeps additive effects.
pub fn command_effects(spec: &crate::CommandSpec, params: &serde_json::Value) -> Effects {
    let effects = effects::declared(spec, params);
    if spec.id == "gp.run"
        && let Some(tool) = params.get("tool").and_then(serde_json::Value::as_str)
        && let Ok(node) = plan::tool_effect_node(tool, params.get("params").unwrap_or(&serde_json::Value::Null))
    {
        effects.union(node)
    } else if spec.id == "gp.batch"
        && let Some(tool) = params.get("tool").and_then(serde_json::Value::as_str)
        && let Ok(tool) = plan::batch_tool_effects(tool)
    {
        effects.union(tool)
    } else {
        effects
    }
}

/// Retains the reviewed conservative imported-item restriction separately from declared effects.
pub(crate) fn imported_heuristic(id: &str, p: &serde_json::Value) -> bool {
    effects::imported(id, p)
}

/// Raw in-memory source identity for an executor-owned clipboard snapshot; never a display copy.
pub(crate) fn input_source(layer: &rustgis_map::Layer) -> serde_json::Value {
    plan::private_source(layer)
}
