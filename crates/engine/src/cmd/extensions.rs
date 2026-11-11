//! Extensions (#1752): list them, and turn them on or off. See [`crate::extensions`].

use serde_json::{Value, json};

use super::{bool_value, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    use crate::policy::{Effect, Effects};
    vec![
        // Listing reads nothing of the user's and changes nothing; turning one on or off is
        // Reserved, as `plugin.enable` is.
        C::new("extension.list", "Extensions", "Plugins › Extensions", list).roles(&[]).effects(Effects::NONE).view(),
        C::new("extension.enable", "Enable Extension", "Plugins › Extensions", enable)
            .roles(&[])
            .effects(Effects::NONE.with(Effect::Reserved))
            .view()
            .params(r#"{"id": extension id (see extension.list), "enabled"?: bool (default true)}"#),
        C::new("extension.disable", "Disable Extension", "Plugins › Extensions", disable)
            .roles(&[])
            .effects(Effects::NONE.with(Effect::Reserved))
            .view()
            .params(r#"{"id": extension id (see extension.list)}"#),
    ]
}

fn list(s: &mut Session, _: &Value) -> CmdResult {
    Ok(s.extensions().list())
}

fn enable(s: &mut Session, p: &Value) -> CmdResult {
    // Text that isn't clearly true or false is refused, not read as false: it would turn a whole
    // part of RustGIS off.
    let on = match p.get("enabled") {
        None | Some(Value::Null) => true,
        Some(v) => bool_value(v).ok_or("enabled must be true or false")?,
    };
    set(s, p, on)
}

fn disable(s: &mut Session, p: &Value) -> CmdResult {
    set(s, p, false)
}

fn set(s: &mut Session, p: &Value, on: bool) -> CmdResult {
    let id = ps(p, "id").map(str::trim).filter(|id| !id.is_empty()).ok_or("id is required (see extension.list)")?;
    let e = s.set_extension(id, on)?;
    Ok(json!({"id": e.id, "name": e.name, "enabled": on, "tab": e.tab}))
}
