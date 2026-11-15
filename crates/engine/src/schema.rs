//! The JSON Schema of the project file (`.rgis`), generated from the types it is saved from
//! (#1853 piece 2). `cargo xtask schema` writes it to [`PATH`], and `cargo xtask schema --check`
//! fails when the committed file and the types disagree, so a change to what is saved updates both.
//!
//! The schema says what a reader accepts: a field the reader fills in when it is absent isn't
//! required. Parts not described yet ([`Open`]) are any JSON object; they are described in later
//! pull requests, crate by crate.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

use crate::project::{CAPABILITIES, FORMAT, MAX_REQUIRES, Project, READ_FORMAT};

/// Where the schema is kept, from the repository's root.
pub const PATH: &str = "schema/project.schema.json";

/// A part of the project file this schema doesn't describe: any JSON object (described in later
/// pull requests of #1853 piece 2, crate by crate).
pub struct Open;

impl JsonSchema for Open {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Open".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "description": "Any object: this schema doesn't describe its fields."
        })
    }
}

/// The schema as [`PATH`] holds it: JSON Schema 2020-12, with the formats it covers (the one this
/// RustGIS writes and the newest it reads) and the capabilities it knows, so changing any of them
/// changes the file too.
///
/// An error when the generated schema has no `properties.format` to bound, so a change to how
/// the project is saved can't drop the constraints below without notice.
pub fn project() -> Result<serde_json::Value, String> {
    let mut schema = schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<Project>();
    schema.insert("$id".into(), format!("https://raw.githubusercontent.com/opengeos/RustGIS/main/{PATH}").into());
    schema.insert("title".into(), "RustGIS project (.rgis)".into());
    schema.insert(
        "description".into(),
        format!(
            "A RustGIS project file, as RustGIS reads it. Generated from the Rust types by `cargo xtask schema`; don't edit it by hand. It writes format {FORMAT} and reads formats up to {READ_FORMAT}."
        )
        .into(),
    );
    schema.insert("x-rustgis-writes-format".into(), FORMAT.into());
    schema.insert("x-rustgis-reads-format".into(), READ_FORMAT.into());
    schema.insert("x-rustgis-capabilities".into(), CAPABILITIES.into());
    let mut value = schema.to_value();
    let properties = value
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("the project schema has no `properties`: the project is no longer saved as an object")?;
    // A newer format is refused (`check_format`), and so is a `requires` naming a capability this
    // RustGIS doesn't know, listing more than MAX_REQUIRES or holding anything but names (null is
    // nothing required).
    properties
        .get_mut("format")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("the project schema has no `format` property to bound by READ_FORMAT")?
        .insert("maximum".into(), READ_FORMAT.into());
    if properties.contains_key("requires") {
        return Err("the project schema already has a `requires` property: the capability bound would replace it".into());
    }
    properties.insert(
        "requires".into(),
        serde_json::json!({
            "description": "The capabilities this file needs past its format, each one this RustGIS knows.",
            "type": ["array", "null"],
            "maxItems": MAX_REQUIRES,
            "items": { "type": "string", "enum": CAPABILITIES }
        }),
    );
    bound_integers(&mut value);
    Ok(value)
}

/// Gives every fixed-width integer its range, which the reader holds it to: schemars gives
/// `minimum: 0` to unsigned ones and the range only to those of 8 and 16 bits, and a `format`
/// alone doesn't constrain a value.
fn bound_integers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let range: Option<(serde_json::Value, serde_json::Value)> = match map.get("format").and_then(serde_json::Value::as_str) {
                Some("int32") => Some((i32::MIN.into(), i32::MAX.into())),
                Some("int64" | "int") => Some((i64::MIN.into(), i64::MAX.into())),
                Some("uint32") => Some((0.into(), u32::MAX.into())),
                Some("uint64" | "uint") => Some((0.into(), u64::MAX.into())),
                _ => None,
            };
            if let Some((min, max)) = range {
                map.entry("minimum").or_insert(min);
                map.entry("maximum").or_insert(max);
            }
            map.values_mut().for_each(bound_integers);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(bound_integers),
        _ => {}
    }
}

/// [`project`] as the file holds it: pretty-printed, with a final newline.
pub fn project_text() -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(&project()?).map_err(|e| format!("the project schema: {e}"))?;
    text.push('\n');
    Ok(text)
}
