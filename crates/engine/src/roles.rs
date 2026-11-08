//! Replayable parameters are cleaned by role, never by recursively deleting secret-named data
//! properties. Unknown nested content and foreign notebook cells remain opaque, with honest
//! notices. Logs deliberately have a separate, stricter policy.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::durable::{self, Durable, Kept, Report};

/// A replayable parameter's durable-output role. Permission policy does not consume roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParamRole {
    pub key: &'static str,
    pub role: Role,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Secret,
    Dataset,
    Configuration,
    Sql,
    Opaque,
    ToolParams,
    Script,
    TaskSteps,
    ModelSteps,
    ModelGraph,
    Document(Document),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Document {
    Project,
    Map,
    Task,
    Notebook,
    Presentation,
    Model,
}

/// Compatibility metadata is read before any embedded document uses this version's types.
#[derive(serde::Deserialize)]
struct DocumentFormat {
    format: u64,
}

const WITHHELD: &str = "[parameters withheld]";
const NESTED: &str = "[withheld: nested too deep]";
const DATA: &[&str] = &[
    "text",
    "data",
    "values",
    "value",
    "attributes",
    "properties",
    "features",
    "feature",
    "geometry",
    "rows",
    "expression",
    "where",
    "html",
    "markdown",
    "caption",
    "title",
    "notes",
    "description",
    "label",
    "name",
    "field",
    "fields",
];
const ADDRESS: &[&str] =
    &["url", "urls", "href", "source", "template", "tiles", "store", "endpoint", "address", "uri", "connection", "database"];
const CONFIG: &[&str] = &["headers", "options", "auth", "connection", "source", "sources"];

/// Durable output reads its own field from reviewed builtin registrations. An explicit empty
/// declaration retains the reviewed common scalar/address/configuration rules below.
fn metadata(id: &str) -> Option<&'static [ParamRole]> {
    static ROLES: OnceLock<HashMap<&'static str, &'static [ParamRole]>> = OnceLock::new();
    ROLES.get_or_init(|| crate::cmd::all().into_iter().filter_map(|c| c.roles.map(|r| (c.id, r))).collect()).get(id).copied()
}

/// A conservative bounded read-only check, used solely to announce unchecked/withheld content.
fn suspicious(v: &Value) -> bool {
    crate::sources::holds_secret(v)
        || v.as_str().is_some_and(|s| {
            s.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '%')))
                .take(65_536)
                .any(|word| !word.is_empty() && rustgis_data::address::secret_key(word))
        })
}

/// Keeps invalid executable content invalid while withholding credential-bearing text.
fn invalid(value: &Value, report: &mut Report) -> Option<Value> {
    if suspicious(value) {
        report.withheld = report.withheld.saturating_add(1);
        Some(Value::String(WITHHELD.into()))
    } else {
        None
    }
}

/// Cleans one command without treating parameter JSON strings as executor envelopes.
pub(crate) fn command(id: &str, params: &Value, depth: usize) -> (Option<Value>, Report) {
    let mut report = Report::default();
    let changed = object(id, params, depth, 0, metadata(id).is_some(), &mut report);
    (changed, report)
}

/// Tools use the common parameter rules, including data-valued `input`/`values`; command-specific
/// metadata belongs to command registrations, not the separate tool registry.
pub(crate) fn tool(id: &str, params: &Value, depth: usize) -> (Option<Value>, Report) {
    let mut report = Report::default();
    let builtin = rustgis_analysis::TOOLS.iter().any(|t| t.id == id);
    let changed = object("", params, depth, 0, builtin, &mut report);
    (changed, report)
}

/// Identifiers are structural executable fields, not layer names or user data. Recognized
/// addresses are cleaned; credential-bearing malformed payloads are withheld as a whole.
pub(crate) fn identifier(id: &str, report: &mut Report) -> Option<String> {
    // A colon-separated credential key/value is payload; a qualified plugin/tool id alone is not.
    let colon_value = id
        .split(':')
        .zip(id.split(':').skip(1))
        .take(65_536)
        .any(|(key, value)| !value.is_empty() && rustgis_data::address::secret_key(key));
    match durable::public_address(id) {
        Kept::Changed(public) => Some(public),
        Kept::Withheld(prefix) if prefix != id => {
            report.withheld = report.withheld.saturating_add(1);
            Some(prefix.into())
        }
        _ if (colon_value || id.contains(['=', '{', '[', '"']) || id.contains(char::is_whitespace))
            && suspicious(&Value::String(id.into())) =>
        {
            report.withheld = report.withheld.saturating_add(1);
            Some("[command withheld]".into())
        }
        _ => None,
    }
}

/// Lazily copies only changed owning parameter objects. Configuration recursion is bounded.
fn object(id: &str, value: &Value, depth: usize, config_depth: usize, registered: bool, report: &mut Report) -> Option<Value> {
    if value.is_null() {
        return None;
    }
    let Some(obj) = value.as_object() else { return invalid(value, report) };
    if config_depth >= 8 {
        return invalid(value, report);
    }
    let mut replacement = None;
    let mut unchecked = false;
    for (key, v) in obj {
        unchecked |= !matches!(durable::public_address(key), Kept::Unchanged);
        let k = rustgis_data::address::normalized_name(key).unwrap_or_else(|| key.to_ascii_lowercase());
        let role = if registered && config_depth == 0 {
            metadata(id).and_then(|roles| roles.iter().find(|r| r.key == k).map(|r| r.role))
        } else {
            None
        };
        let clean = if let Some(
            role @ (Role::ToolParams | Role::Script | Role::TaskSteps | Role::ModelSteps | Role::ModelGraph | Role::Document(_)),
        ) = role
        {
            if depth >= 4 {
                report.withheld = report.withheld.saturating_add(1);
                Some(Value::String(NESTED.into()))
            } else {
                envelope(role, v, obj, depth + 1, report)
            }
        } else if role == Some(Role::Opaque) {
            unchecked |= suspicious(v);
            None
        } else if (role == Some(Role::Secret) || rustgis_data::address::secret_key(key))
            && (k != "key"
                || crate::sources::looks_like_a_key(v.as_str().unwrap_or(""))
                || obj.iter().any(|(other, value)| other != key && !DATA.contains(&other.as_str()) && address_like(value)))
        {
            match v {
                Value::String(s) if !s.is_empty() => Some(Value::String(String::new())),
                Value::Number(_) => Some(Value::String(String::new())),
                // Unknown structured credentials keep their shape and are reported as unchecked.
                Value::Object(o) if !o.is_empty() => {
                    unchecked = true;
                    None
                }
                Value::Array(a) if !a.is_empty() => {
                    unchecked = true;
                    None
                }
                _ => None,
            }
        } else if role == Some(Role::Dataset) || DATA.contains(&k.as_str()) {
            None
        } else if role == Some(Role::Sql) {
            v.as_str().and_then(|s| {
                let clean = crate::project_log::sql_secrets_removed(s);
                (clean != s).then_some(Value::String(clean))
            })
        } else if ADDRESS.contains(&k.as_str()) || address_like(v) {
            if v.is_object() && registered && (role == Some(Role::Configuration) || CONFIG.contains(&k.as_str())) {
                object("", v, depth, config_depth + 1, true, report)
            } else {
                // String leaves are cleaned below; opaque shapes keep their meaning.
                unchecked |= match v {
                    Value::Object(_) => suspicious(v),
                    Value::Array(a) => a.iter().any(|v| !v.is_string() && suspicious(v)),
                    _ => false,
                };
                address(v, report)
            }
        } else if registered && (role == Some(Role::Configuration) || CONFIG.contains(&k.as_str())) && v.is_object() {
            object("", v, depth, config_depth + 1, true, report)
        } else {
            unchecked |= suspicious(v);
            None
        };
        if !registered && !DATA.contains(&k.as_str()) && (v.is_object() || v.is_array()) {
            unchecked |= suspicious(v);
        }
        if let Some(clean) = clean.filter(|clean| clean != v) {
            replacement.get_or_insert_with(|| obj.clone()).insert(key.clone(), clean);
        }
    }
    if unchecked {
        report.unchecked = report.unchecked.saturating_add(1);
    }
    replacement.map(Value::Object)
}

/// Recognition precedes parsing, including malformed wrappers and differently cased prefixes.
fn address_like(v: &Value) -> bool {
    match v {
        Value::String(s) => {
            let s = s.trim_start();
            [
                "http:",
                "https:",
                "s3:",
                "rustgis-login:",
                "rustgis-anonymous:",
                "rustgis-s3:",
                "pmtiles:",
                "cog:",
                "postgres:",
                "postgresql:",
                "pg:",
            ]
            .iter()
            .any(|p| s.get(..p.len()).is_some_and(|s| s.eq_ignore_ascii_case(p)))
        }
        Value::Array(a) => a.iter().any(|v| v.is_string() && address_like(v)),
        _ => false,
    }
}

/// Array addresses are string leaves only; data and nested opaque objects are never walked.
fn address(v: &Value, report: &mut Report) -> Option<Value> {
    match v {
        Value::String(s) => match durable::public_address(s) {
            Kept::Unchanged => None,
            Kept::Changed(s) => Some(Value::String(s)),
            Kept::Withheld(prefix) if prefix != s => {
                report.withheld = report.withheld.saturating_add(1);
                Some(Value::String(prefix.into()))
            }
            Kept::Withheld(_) => None,
        },
        Value::Array(a) => {
            let mut changed = None;
            for (i, v) in a.iter().enumerate() {
                if v.is_string()
                    && let Some(clean) = address(v, report)
                    && let Some(slot) = changed.get_or_insert_with(|| a.clone()).get_mut(i)
                {
                    *slot = clean;
                }
            }
            changed.map(Value::Array)
        }
        _ => None,
    }
}

/// Explicit executable and document roles are the only route into nested executable content.
fn envelope(role: Role, v: &Value, siblings: &serde_json::Map<String, Value>, depth: usize, report: &mut Report) -> Option<Value> {
    match role {
        Role::ToolParams => {
            let (clean, r) = tool(siblings.get("tool").and_then(Value::as_str).unwrap_or(""), v, depth);
            report.merge(&r);
            clean
        }
        Role::Script => match v.as_str() {
            Some(s) => {
                let (clean, r) = script(s, depth);
                report.merge(&r);
                clean.map(Value::String)
            }
            None => invalid(v, report),
        },
        Role::TaskSteps => typed_value::<VecTask>(v, depth, report),
        Role::ModelSteps => typed_value::<VecSteps>(v, depth, report),
        Role::ModelGraph => typed_value::<crate::project::ModelGraph>(v, depth, report),
        Role::Document(kind) => document(kind, v, depth, report),
        _ => None,
    }
}

/// A typed envelope is prepared on its own copy, preserving the live outer params as a whole.
fn typed_value<T: DeserializeOwned + Serialize + Durable>(v: &Value, depth: usize, report: &mut Report) -> Option<Value> {
    let Ok(parsed) = serde_json::from_value::<T>(v.clone()) else { return invalid(v, report) };
    let Ok((clean, r)) = durable::output_owned_changed(parsed, depth, |v, changed| {
        if changed { serde_json::to_value(v).map(Some).map_err(|e| e.to_string()) } else { Ok(None) }
    }) else {
        return invalid(v, report);
    };
    report.merge(&r);
    clean
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct VecTask(Vec<crate::project::TaskStep>);

impl Durable for VecTask {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut durable::Prepare<'_>) {
        for (i, st) in self.0.iter().enumerate() {
            p.command(durable::Leaf::Task(i), durable::Leaf::TaskCommand(i), &st.command, &st.params);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[durable::Owner], leaf: &durable::Leaf) -> Option<durable::FieldMut<'_>> {
        if !owners.is_empty() {
            return None;
        }
        match leaf {
            durable::Leaf::Task(i) => Some(durable::FieldMut::Json(&mut self.0.get_mut(*i)?.params)),
            durable::Leaf::TaskCommand(i) => Some(durable::FieldMut::Text(&mut self.0.get_mut(*i)?.command)),
            _ => None,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct VecSteps(Vec<crate::project::ModelStep>);

impl Durable for VecSteps {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut durable::Prepare<'_>) {
        for (i, st) in self.0.iter().enumerate() {
            p.tool(durable::Leaf::ModelStep(i), durable::Leaf::ModelStepTool(i), &st.tool, &st.params);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[durable::Owner], leaf: &durable::Leaf) -> Option<durable::FieldMut<'_>> {
        if !owners.is_empty() {
            return None;
        }
        match leaf {
            durable::Leaf::ModelStep(i) => Some(durable::FieldMut::Json(&mut self.0.get_mut(*i)?.params)),
            durable::Leaf::ModelStepTool(i) => Some(durable::FieldMut::Text(&mut self.0.get_mut(*i)?.tool)),
            _ => None,
        }
    }
}

/// Reads documents at the existing reader bound, without normalization, trust or restore-cache
/// access. Unchanged documents keep their exact text; only changed typed copies are serialized.
fn document(kind: Document, v: &Value, depth: usize, report: &mut Report) -> Option<Value> {
    let Some(text) = v.as_str().filter(|s| s.len() as u64 <= crate::project::max_bytes()) else { return invalid(v, report) };
    if serde_json::from_str::<DocumentFormat>(text).is_ok_and(|f| f.format > u64::from(crate::project::FORMAT)) {
        report.unchecked = report.unchecked.saturating_add(1);
        return None;
    }
    let result = match kind {
        Document::Project => text_document::<crate::Project>(text, depth),
        Document::Map => {
            if serde_json::from_str::<crate::Project>(text).is_ok() {
                text_document::<crate::Project>(text, depth)
            } else {
                text_document::<rustgis_map::Map>(text, depth)
            }
        }
        Document::Task => text_document::<crate::project::Task>(text, depth),
        Document::Presentation => text_document::<rustgis_map::presentation::Presentation>(text, depth),
        Document::Notebook => crate::cmd::notebook::durable_document(text, depth),
        Document::Model => model_document(text, depth),
    };
    match result {
        Ok((clean, r)) => {
            let changed = clean != text;
            report.merge(&r);
            changed.then_some(Value::String(clean))
        }
        Err(_) => invalid(v, report),
    }
}

/// Uses the owning type's ordinary deserializer, without losing unchanged text representation.
fn text_document<T: DeserializeOwned + Serialize + Durable>(text: &str, depth: usize) -> Result<(String, Report), String> {
    let parsed = serde_json::from_str::<T>(text).map_err(|e| e.to_string())?;
    durable::output_owned_changed(parsed, depth, |value, changed| {
        if changed { serde_json::to_string(value).map_err(|e| e.to_string()) } else { Ok(text.into()) }
    })
}

/// Model import has two envelopes; keeps the envelope's other fields and cleans only its model.
fn model_document(text: &str, depth: usize) -> Result<(String, Report), String> {
    let mut v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut report = Report::default();
    let Some(obj) = v.as_object_mut() else { return Err("not a model".into()) };
    let (key, graph) = if obj.contains_key("graph") { ("graph", true) } else { ("steps", false) };
    let Some(field) = obj.get(key) else { return Err("not a model".into()) };
    let clean = if graph {
        typed_value::<crate::project::ModelGraph>(field, depth, &mut report)
    } else {
        typed_value::<VecSteps>(field, depth, &mut report)
    };
    if let Some(clean) = clean {
        obj.insert(key.into(), clean);
        Ok((v.to_string(), report))
    } else {
        Ok((text.into(), report))
    }
}

/// Uses the executor's line parser, preserving comments, blank lines and unchanged formatting.
pub(crate) fn script(source: &str, depth: usize) -> (Option<String>, Report) {
    let mut report = Report::default();
    let mut out = None;
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        let ending = if line.ends_with("\r\n") {
            "\r\n"
        } else if line.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let replacement = match crate::script_line(line) {
            None => None,
            Some(Ok((id, params))) => {
                let public_id = identifier(&id, &mut report);
                let (clean, r) = command(&id, &params, depth);
                report.merge(&r);
                match clean {
                    Some(params) => Some(format!("{} {params}{ending}", public_id.as_deref().unwrap_or(&id))),
                    None => public_id.map(|public| line.replacen(&id, &public, 1)),
                }
            }
            Some(Err(_)) if suspicious(&Value::String(line.into())) => {
                report.withheld = report.withheld.saturating_add(1);
                let id = line.trim_start().split(char::is_whitespace).next().unwrap_or("");
                let public_id = identifier(id, &mut report);
                Some(format!("{} {WITHHELD}{ending}", public_id.as_deref().unwrap_or(id)))
            }
            Some(Err(_)) => None,
        };
        if let Some(clean) = replacement {
            report.lines = report.lines.saturating_add(1);
            out.get_or_insert_with(|| source.get(..offset).unwrap_or("").to_string()).push_str(&clean);
        } else if let Some(out) = &mut out {
            out.push_str(line);
        }
        offset += line.len();
    }
    (out, report)
}
