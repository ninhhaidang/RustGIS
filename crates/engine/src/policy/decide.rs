use super::{
    Effect, Effects,
    plan::{Node, Op},
    scope::{AgentAccess, Kind, Scope},
};
use crate::Session;
use rustgis_plugins::manifest::Permission;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Decision {
    Allow,
    Remember,
    Ask,
    Deny,
}
/// The one permission table. A new effect requires an explicit disposition here.
pub(super) fn decide(s: &Session, scope: &Scope, node: &Node) -> Result<Decision, String> {
    // Base access refusal supplies actionable guidance even for an attached read-only caller.
    // An admitted operation still checks live settings and its captured load before any work.
    let d = kind(s, &scope.kind, node);
    if d == Decision::Deny {
        return Err(refusal(s, &scope.kind, node));
    }
    scope.check(node.effects)?;
    node.current()?;
    Ok(d)
}
/// Applies one table to the current principal and its narrowing intersections.
fn kind(s: &Session, k: &Kind, node: &Node) -> Decision {
    use Decision::*;
    use Effect::*;
    match k {
        Kind::User => Allow,
        Kind::Peer => {
            if node.effects.contains(Replaces) {
                Allow
            } else {
                Deny
            }
        }
        Kind::Assistant => node
            .effects
            .iter()
            .map(|e| match e {
                Edits | History => Allow,
                Files | Network => Remember,
                Plugin | Automation | Reserved | Replaces => Ask,
                Unreviewed => Deny,
            })
            .max()
            .unwrap_or(Allow),
        Kind::Agent { access: AgentAccess::Full, .. } => Allow,
        Kind::Agent { access, users_project, actor } => node
            .effects
            .iter()
            .map(|e| match e {
                Edits => Allow,
                Files | Network | Plugin => match access {
                    AgentAccess::Refuse => Deny,
                    AgentAccess::Review => Ask,
                    AgentAccess::Allow | AgentAccess::Full => Allow,
                },
                Automation | Reserved | Unreviewed => Deny,
                Replaces => {
                    if *users_project {
                        Deny
                    } else {
                        Allow
                    }
                }
                History => {
                    let top = match &node.op {
                        Op::Command(spec) if spec.id == "edit.undo" => s.undo_actor(),
                        _ => s.redo_actor(),
                    };
                    if (*users_project && actor.is_none()) || actor.as_deref().is_some_and(|me| top.is_some_and(|by| by != Some(me))) {
                        Deny
                    } else {
                        Allow
                    }
                }
            })
            .max()
            .unwrap_or(Allow),
        Kind::Item { within, .. } => {
            let own = node.effects.iter().any(|e| !matches!(e, Edits | History))
                || match &node.op {
                    Op::Tool(pin) => super::effects::imported("gp.run", &serde_json::json!({"tool":pin.spec().id,"params":node.params})),
                    _ => super::effects::imported(node.id(), &node.params),
                };
            if own { Deny } else { kind(s, within, node) }
        }
        Kind::Plugin { pin, within } => {
            let m = &pin.0.manifest;
            let own = node.effects.iter().all(|e| match e {
                Edits => m.has(Permission::Edit),
                Files => {
                    m.has(Permission::Files)
                        || (node.id() == "raster.mosaic" && crate::cmd::imagery::mosaic_stays_in_project(s, &node.params))
                }
                Network => m.has(Permission::Network),
                Plugin => matches!(&node.op, Op::Tool(t) if t.plugin().is_some_and(|p| p.0.manifest.id == m.id)) && m.has(Permission::Edit),
                Automation | Reserved | Replaces | History | Unreviewed => false,
            });
            if !own {
                return Deny;
            }
            // Approval of this plugin admits its manifest's host requests. Item restrictions
            // remain an intersection; the outer caller cannot grant an imported item authority.
            /// Retains imported-item restrictions inside an approved plugin manifest.
            fn item(s: &Session, k: &Kind, n: &Node) -> Decision {
                match k {
                    Kind::Item { .. } => kind(s, k, n),
                    Kind::Plugin { within, .. } => item(s, within, n),
                    _ => Allow,
                }
            }
            item(s, within, node)
        }
    }
}
/// Names the strongest external effect for an approval/refusal.
pub(super) fn reason(effects: Effects) -> &'static str {
    use Effect::*;
    if effects.contains(Unreviewed) {
        "it uses an unreviewed external runner or plugin"
    } else if effects.contains(Plugin) {
        "it runs a plugin's code; approve each run"
    } else if effects.contains(Replaces) {
        "it replaces the open project"
    } else if effects.contains(Automation) || effects.contains(Reserved) {
        "it is reserved to you (plugins, scripts, tasks, models and the GeoLibre runner)"
    } else if effects.contains(Files) && effects.contains(Network) {
        "it reads files and connects to addresses on the internet"
    } else if effects.contains(Files) {
        "it reads or writes files on this computer"
    } else if effects.contains(Network) {
        "it connects to an address on the internet"
    } else {
        "this operation needs your approval"
    }
}
/// Explains denial without exposing private source or account equality data.
fn refusal(s: &Session, k: &Kind, n: &Node) -> String {
    match k {
        Kind::Item { kind, name, .. } => format!(
            "{}: this {kind} '{name}' came from a file, or was made or changed by an assistant, agent or plugin, so its steps may only edit the project; recreate the step yourself to run it",
            n.id()
        ),
        Kind::Plugin { pin, .. } => {
            let m = &pin.0.manifest;
            if let Op::Tool(tool) = &n.op
                && tool.plugin().is_some_and(|p| p.0.manifest.id != m.id)
            {
                return format!("{} cannot run '{}' (another plugin's code)", m.name, n.id());
            }
            let missing = if n.effects.contains(Effect::Edits) && !m.has(Permission::Edit) {
                Some("'edit' permission")
            } else if n.effects.contains(Effect::Files)
                && n.effects.contains(Effect::Network)
                && (!m.has(Permission::Files) || !m.has(Permission::Network))
            {
                Some("'files' and 'network' permissions")
            } else if n.effects.contains(Effect::Files) && !m.has(Permission::Files) {
                Some("'files' permission")
            } else if n.effects.contains(Effect::Network) && !m.has(Permission::Network) {
                Some("'network' permission")
            } else {
                None
            };
            match missing {
                Some(permission) => format!("{} needs the {permission} to run '{}'", m.name, n.id()),
                None => {
                    format!("{} cannot run '{}' (plugins can't run another plugin's code, automation or reserved actions)", m.name, n.id())
                }
            }
        }
        _ if n.effects.contains(Effect::History) => {
            let top = if n.id() == "edit.undo" { s.undo_actor() } else { s.redo_actor() };
            let whose = if top == Some(None) { "the user's" } else { "another agent's" };
            format!("an agent cannot undo or redo {whose} step; it undoes and redoes only its own top step")
        }
        _ => format!(
            "'{}' was not run: {}. This caller needs approval or --yes; these actions are reserved to the user unless this caller has full access",
            n.id(),
            reason(n.effects)
        ),
    }
}
