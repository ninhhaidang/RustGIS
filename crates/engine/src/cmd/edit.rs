//! Edit tab: create and delete features, edit attributes and fields, of a layer or of a
//! standalone table (`"table"`).

use rustgis_data::loss::LossCode;
use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::{Value as Json, json};
use std::borrow::Cow;

use super::{need_layer_or_table, ps};
use crate::notice_text::NoticeText;
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    let box_roles: &[crate::ParamRole] =
        &[crate::ParamRole { key: "box", role: crate::Role::Opaque }, crate::ParamRole { key: "attributes", role: crate::Role::Opaque }];
    vec![
        C::new("edit.previewShapeConversion", "Preview Shape Conversion", "Edit › Features", crate::shape_conversion::preview).inputs(crate::shape_conversion::preview_inputs)
            .roles(&[crate::ParamRole {key:"frame",role:crate::Role::Opaque}]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).view()
            .when(crate::cmd::design::need_editable).params(r#"{"layer"?:name|id,"row":number,"featureId"?:number,"kind":"circle|rotatedBox","request"?:number (preview correlation only),"image"?:GeoTIFF layer name|id,"frame"?:construction grid as edit.addBox.box.frame (otherwise the layer's own grid when it is held in a projected EPSG system, else Web Mercator; give {"epsg":3857} for Web Mercator)}"#),
        C::new("edit.applyShapeConversion", "Apply Shape Conversion", "Edit › Features", crate::shape_conversion::apply).inputs(crate::shape_conversion::apply_inputs)
            .roles(&[crate::ParamRole {key:"preview",role:crate::Role::Opaque}]).effects(crate::policy::Effects::NONE).atomic()
            .when(crate::shape_conversion::need_editing).params(r#"{"preview":result of edit.previewShapeConversion,"confirm":true}"#),
        C::new("edit.calculateGeometry", "Calculate Geometry", "Attribute Table", crate::geometry_calc::run).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::design::need_editable)
            .params(r#"{"layer"?, "field": string, "create": bool, "overwrite": bool, "property": string, "unit": string, "epsg"?: number, "scope": "all"|"selected"}"#),
        C::new("edit.addPoint", "Create Point by Coordinates", "Edit › Features", add_point).inputs(point_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"x": number or degrees text, "y": number or degrees text, "crs"?: coordinate system|"map"|layer name (omitted: longitude/latitude, including degrees text), "layer"?: name|id (otherwise the active loaded point layer, or a new Points layer), "name"?: text (requires a text field named name in an existing layer)}"#),
        C::new("edit.addFeature", "Create Feature", "Edit › Features", add_feature).inputs(super::design::new_feature_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::design::need_editable_or_table)
            .params(r#"{"layer"?, "table"?: name|id (a standalone table: adds a row, without geometry or template), "geometry": GeoJSON geometry (lon/lat; left out for a table), "attributes"?: {field: value}, "template"?: feature template}"#),
        C::new("edit.delete", "Delete", "Edit › Features", delete).roles(&[]).effects(crate::policy::Effects::NONE).key("Delete").when(super::design::need_editable_selection),
        C::new("edit.setValue", "Edit Attribute", "Attribute Table", set_value).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::design::need_editable_or_table)
            .params(r#"{"layer"?, "table"?: name|id (a standalone table, instead of a layer), "row": number, "field": string, "value": any}"#),
        C::new("edit.addField", "Add Field", "Data › Data Design › Fields", add_field).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layer_or_table)
            .params(r#"{"layer"?, "table"?: name|id (a standalone table, instead of a layer), "name": string, "type": "long"|"double"|"text"|"boolean"|"date"|"datetime"}"#),
        C::new("edit.deleteField", "Delete Field", "Data › Data Design › Fields", delete_field).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layer_or_table)
            .params(r#"{"layer"?, "table"?: name|id (a standalone table, instead of a layer), "field": string}"#),
        C::new("edit.convertField", "Convert Field to Dates", "Attribute Table", convert_field).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layer_or_table)
            .params(r#"{"layer"?, "table"?: name|id (a standalone table, instead of a layer), "field": string, "type": "date"|"datetime", "format"?: "iso" (default)|strftime numeric pattern|"excel1900"|"excel1904", "pivotYear"?: first year of the 100-year window for %y}"#),
        C::new("edit.calculateField", "Calculate Field", "Attribute Table", calculate_field).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::design::need_editable_or_table)
            .params(r#"{"layer"?, "table"?: name|id (a standalone table, instead of a layer: every row), "field": string, "expression": expression (fields, 'text', numbers, + - * / % ^ ||, functions: see the Expressions page)}"#),
        C::new("edit.addBox", "Add Rotated Box", "Edit › Features", crate::box_edit::add).inputs(crate::box_edit::new_box_inputs).roles(box_roles).effects(crate::policy::Effects::NONE)
            .when(crate::cmd::design::need_editable).params(r#"{"layer"?: name|id, "box": {"center":[x,y],"width":number,"height":number,"angle":clockwise degrees,"frame"?:{"epsg":number,"width":pixels,"height":pixels,"affine":[x0,xc,xr,y0,yc,yr]}},"attributes"?:{field:value},"template"?:string}"#),
        C::new("edit.updateBox", "Update Rotated Box", "Edit › Features", crate::box_edit::update).roles(&[crate::ParamRole { key: "box", role: crate::Role::Opaque }]).effects(crate::policy::Effects::NONE)
            .when(crate::cmd::design::need_editable).params(r#"{"layer"?:name|id,"row":number,"featureId"?:number,"box":as edit.addBox}"#),
        C::new("edit.deleteBox", "Delete Box", "Edit › Features", crate::box_edit::delete_box).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(crate::cmd::design::need_editable).params(r#"{"layer"?:name|id,"row":number,"featureId"?:number}"#),
        C::new("edit.boxInfo", "Box Construction", "Edit › Features", crate::box_edit::info).roles(&[]).effects(crate::policy::Effects::NONE).view()
            .params(r#"{"layer"?:name|id,"row":number}"#),
        C::new("edit.addCircle", "Add Circle", "Edit › Features", crate::circle_edit::add).inputs(crate::circle_edit::new_circle_inputs).roles(&[crate::ParamRole { key: "circle", role: crate::Role::Opaque }, crate::ParamRole { key: "attributes", role: crate::Role::Opaque }]).effects(crate::policy::Effects::NONE)
            .when(crate::cmd::design::need_editable).params(r#"{"layer"?: name|id, "circle": {"center":[x,y],"radius":number,"tolerance"?:number (default 0.25 grid units),"frame"?:{"epsg":number,"width":pixels,"height":pixels,"affine":[x0,xc,xr,y0,yc,yr]}},"attributes"?:{field:value},"template"?:string}"#),
        C::new("edit.updateCircle", "Update Circle", "Edit › Features", crate::circle_edit::update).roles(&[crate::ParamRole { key: "circle", role: crate::Role::Opaque }]).effects(crate::policy::Effects::NONE)
            .when(crate::cmd::design::need_editable).params(r#"{"layer"?:name|id,"row":number,"featureId"?:number,"circle":as edit.addCircle}"#),
        C::new("edit.deleteCircle", "Delete Circle", "Edit › Features", crate::circle_edit::delete_circle).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(crate::cmd::design::need_editable).params(r#"{"layer"?:name|id,"row":number,"featureId"?:number}"#),
        C::new("edit.circleInfo", "Circle Construction", "Edit › Features", crate::circle_edit::info).roles(&[]).effects(crate::policy::Effects::NONE).view()
            .params(r#"{"layer"?:name|id,"row":number}"#),
    ]
}

/// Whether this layer can receive a coordinate point without loading a streamed source.
pub fn accepts_coordinate_point(l: &rustgis_map::Layer) -> bool {
    !l.is_raster() && !l.graphics && !l.dataset.windowed && l.geometry_type() == GeometryType::Point
}

/// A loaded point layer can accept a single point without reading or materializing a source.
fn point_target(input: &impl super::InputSource, p: &Json) -> Result<Option<u32>, String> {
    let s = input.input_context();
    let explicit = match p.get("layer") {
        None | Some(Json::Null) => false,
        Some(Json::String(_) | Json::Number(_)) => true,
        _ => return Err("layer must be a name or id".into()),
    };
    let id = if explicit { Some(s.layer_id(p)?) } else { s.active_layer };
    let usable = id.filter(|id| s.map().layer(*id).is_some_and(accepts_coordinate_point));
    if explicit && usable.is_none() {
        return Err("choose a loaded point layer".into());
    }
    Ok(usable)
}

/// Declare the actual target and the elevation surface, including for a new layer.
fn point_inputs(input: &super::InputContext<'_>, p: &Json) -> Result<super::Inputs, String> {
    let target = point_target(input, p)?;
    let mut inputs = super::Inputs::layers(target.and_then(|id| input.map().layer(id)).map(|l| (input.map(), l)));
    inputs.uses_default = p.get("layer").is_none_or(Json::is_null);
    super::design::include_new_feature_surface(input, &mut inputs);
    // Match typed_place's resolution order: map, a layer name, then a literal system.
    // The planner already binds private selector state to the displayed approval.
    if let Some(name) = ps(p, "crs").map(str::trim) {
        if name.eq_ignore_ascii_case("map") {
            inputs.state = json!({"feature":inputs.state,"projection":input.map().projection});
        } else if let Some(layer) = input.map().layer_by_name(name) {
            let key = (input.map().instance_id(), layer.id);
            if !inputs.layers.contains(&key) {
                inputs.layers.push(key);
            }
            inputs.state = json!({"feature":inputs.state,"crs":super::navigate::layer_coordinates_system(layer).unwrap_or(rustgis_data::raster::Crs::LonLat)});
        }
    }
    Ok(inputs)
}

/// Creates a point through ordinary edit rules in one Undo step, including a fallback
/// layer. Like other feature creation, editing must be on. Runs inline like `edit.addFeature`;
/// snapping to stored places in a projected layer can search its existing features.
fn add_point(s: &mut Session, p: &Json) -> CmdResult {
    editing_on(s)?;
    // Keep this command's ingress to the documented X/Y pair; the shared parser also serves
    // commands accepting a combined position, which this form does not offer.
    if ["position", "lon", "lat"].iter().any(|key| p.get(key).is_some_and(|v| !v.is_null())) {
        return Err("use x and y for a coordinate point".into());
    }
    for key in ["x", "y"] {
        if p.get(key).is_none_or(Json::is_null) {
            return Err(format!("{key} is required"));
        }
    }
    // Combined positions are rejected above; the X/Y path produces no approximation notice.
    let (at, _) = super::navigate::typed_place(s.map(), p, false)?;
    let target = point_target(s, p)?;
    let name = match p.get("name") {
        None | Some(Json::Null) => "",
        Some(Json::String(t)) => t.trim(),
        _ => return Err("name must be text".into()),
    };
    let field = if let Some(id) = target {
        let l = s.map().layer(id).ok_or("no layer")?;
        l.dataset.fields.iter().find(|f| f.name.eq_ignore_ascii_case("name") && f.ty == FieldType::Text).map(|f| f.name.clone())
    } else {
        Some("name".into())
    };
    if !name.is_empty() && field.is_none() {
        return Err("this layer has no text field named name; leave Name blank or choose another point layer".into());
    }
    let create = |s: &mut Session| {
        let id = match target {
            Some(id) => id,
            None => {
                super::items::layer_template(s, &json!({"template":"points"}))?;
                s.active_layer.ok_or("the point layer was not created")?
            }
        };
        let mut attributes = serde_json::Map::new();
        if !name.is_empty()
            && let Some(field) = field
        {
            attributes.insert(field, json!(name));
        }
        let result = add_geometry(s, id, &json!({"attributes":attributes}), Geometry::Point(at))?;
        if target.is_none() {
            // The UI explains the fallback before submission; non-UI callers need it too.
            s.notify("Created a Points layer because no loaded point layer was selected", false);
        }
        Ok(json!({"layer":id,"row":result.get("row")}))
    };
    // Existing-layer refusals are rolled back by add_geometry without a row snapshot.
    // Fallback also restores the layer it created if a plugin handles the error.
    if target.is_none() { s.savepoint("Create Point by Coordinates", create) } else { create(s) }
}

/// What an edit command changes: a layer, or a standalone table (by its position among the
/// project's tables).
#[derive(Clone, Copy)]
enum Target {
    Layer(u32),
    Table(usize),
}

/// The layer (`"layer"`, or the active one) or standalone table (`"table"`) the parameters name.
/// Naming both is refused.
fn target(s: &Session, p: &Json) -> Result<Target, String> {
    if p.get("table").is_none_or(Json::is_null) {
        return s.layer_id(p).map(Target::Layer);
    }
    if p.get("layer").is_some_and(|l| !l.is_null()) {
        return Err("give a layer or a standalone table (\"table\"), not both".into());
    }
    // Native tasks invoke handlers inline, including through nested scripts. Refuse here,
    // before any row scan; tasks have no worker to supply progress and Cancel.
    #[cfg(not(target_arch = "wasm32"))]
    if s.task_running {
        return Err(
            "tasks cannot write standalone tables: run the table edit directly or in a separate script for progress and Cancel".into()
        );
    }
    s.table_index(p).map(Target::Table)
}

/// A synchronous edit's target. Native scoped callers and notebooks need an owned worker result,
/// which handlers land before reaching here; an unprepared call must never scan live rows.
fn write_target(s: &Session, p: &Json) -> Result<Target, String> {
    let target = target(s, p)?;
    #[cfg(not(target_arch = "wasm32"))]
    if matches!(target, Target::Table(_)) && (!s.policy_user() || s.notebook_running) {
        return Err("this caller must prepare the table edit on a worker; run it from the table view instead of synchronously".into());
    }
    Ok(target)
}

/// The data of `t`.
fn data(s: &Session, t: Target) -> Result<&Dataset, String> {
    match t {
        Target::Layer(id) => s.map().layer(id).map(|l| &l.dataset).ok_or_else(|| "no layer".to_string()),
        Target::Table(i) => s.project.tables.get(i).map(|t| &t.dataset).ok_or_else(|| "no such table".to_string()),
    }
}

/// The data of `t`, to edit.
fn data_mut(s: &mut Session, t: Target) -> Result<&mut Dataset, String> {
    match t {
        Target::Layer(id) => s.map_mut().layer_mut(id).map(|l| &mut l.dataset).ok_or_else(|| "no layer".to_string()),
        Target::Table(i) => s.project.tables.get_mut(i).map(|t| &mut t.dataset).ok_or_else(|| "no such table".to_string()),
    }
}

/// Field `name`'s index in the data of `t`: the first matching it ignoring case, as expressions
/// and queries find fields.
fn field_of(s: &Session, t: Target, name: &str) -> Result<usize, String> {
    data(s, t)?.field_index(name).ok_or_else(|| format!("no field named '{name}'"))
}

/// A table's rows change only while editing is on, as a layer's features do
/// ([`super::design::apply`]); its fields, as a layer's, change either way.
fn editing_on(s: &Session) -> Result<(), String> {
    if s.map().editing.settings.enabled { Ok(()) } else { Err("editing is turned off (Edit › Edit)".into()) }
}

/// `edit.addFeature`: a feature of the given geometry and attributes (else its template's
/// defaults) in a layer, selected and checked against the layer's design; or a row of a
/// standalone table ([`add_row`]), on the desktop table worker ([`table_ahead`]).
pub(crate) fn add_feature(s: &mut Session, p: &Json) -> CmdResult {
    if let Some(result) = land_table_edit(s, p, "edit.addFeature") {
        return result;
    }
    let id = match write_target(s, p)? {
        Target::Table(i) => return add_row(s, p, i),
        Target::Layer(id) => id,
    };
    let g = geometry_param(p)?;
    add_geometry(s, id, p, g)
}

/// [`add_feature`] with `g` in layer `id`'s own coordinates, written as it is: for a shape made
/// in its own grid (a traverse, #2032 C2f-6).
pub(crate) fn add_feature_own(s: &mut Session, id: u32, p: &Json, g: Geometry) -> CmdResult {
    fits(s, id, &g)?;
    insert(s, id, p, g)
}

/// Refuses `g` where layer `id` can't take it: of another kind, or too few vertices.
fn fits(s: &Session, id: u32, g: &Geometry) -> Result<(), String> {
    let want = s.map().layer(id).ok_or("no layer")?.geometry_type();
    if want != g.kind() && want != GeometryType::None {
        return Err(format!("{} can't go in a {} layer", g.kind().label(), want.label()));
    }
    validate(g)
}

/// Inserts an already validated construction through the same attributes, templates and rules.
pub(crate) fn add_geometry(s: &mut Session, id: u32, p: &Json, g: Geometry) -> CmdResult {
    // A place snapped to a stored one stays as stored.
    let held = super::design::held_places(s, id, &[], &[&g]);
    fits(s, id, &g)?;
    // Given in longitude/latitude, written in the layer's own coordinates.
    let g = s.map().layer(id).ok_or("no layer")?.dataset.edit_from_lonlat(g, &held)?;
    insert(s, id, p, g)
}

/// Inserts `g`, in layer `id`'s own coordinates, with a feature template's defaults and the given
/// attributes, as one edit.
fn insert(s: &mut Session, id: u32, p: &Json, g: Geometry) -> CmdResult {
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    // A feature template's defaults, then the given attributes.
    let template = match ps(p, "template") {
        Some(t) => {
            Some(l.design.templates.iter().find(|x| x.name.eq_ignore_ascii_case(t)).cloned().ok_or_else(|| format!("no template '{t}'"))?)
        }
        None => None,
    };
    let layer = l.dataset.name.clone();
    let defaults = template.as_ref().map_or(&[][..], |t| &t.defaults[..]);
    let (attrs, losses, left_out) = new_row(&l.dataset.fields, p, defaults)?;
    let row = l.dataset.features.len();
    let fields = l.dataset.fields.len();
    l.dataset.push(Some(g), attrs);
    if let Err(error) = super::design::apply(s, id, vec![super::design::Change { before: None, row: Some(row) }]) {
        // Design only writes the new row, apart from appending an elevation column of nulls
        // to older rows. Remove both on refusal without retaining another full row vector.
        if let Some(l) = s.map_mut().layer_mut(id) {
            l.dataset.features.truncate_rows(row);
            for field in (fields..l.dataset.fields.len()).rev() {
                l.dataset.features.remove_column(field);
            }
            l.dataset.fields.truncate(fields);
        }
        return Err(error);
    }
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    l.touch_geometry();
    l.selection = vec![row];
    for (field, key) in losses {
        notify_loss(s, key, &layer, field, row);
    }
    notify_left_out(s, &layer, &left_out);
    Ok(json!({"row": row}))
}

/// `edit.addFeature` on standalone table `i`: a row of the given attributes, made only while
/// editing is on, as a feature is. A table has no geometry, nor templates (a layer's design).
fn add_row(s: &mut Session, p: &Json, i: usize) -> CmdResult {
    if p.get("geometry").is_some_and(|g| !g.is_null()) {
        return Err("a standalone table holds no geometry: leave out \"geometry\"".into());
    }
    if p.get("template").is_some_and(|t| !t.is_null()) {
        return Err("feature templates belong to layers: give a table's values as \"attributes\"".into());
    }
    editing_on(s)?;
    let t = data_mut(s, Target::Table(i))?;
    let (attrs, losses, left_out) = new_row(&t.fields, p, &[])?;
    t.push(None, attrs);
    let row = t.features.len().saturating_sub(1);
    let table = t.name.clone();
    for (field, key) in losses {
        notify_loss(s, key, &table, field, row);
    }
    notify_left_out(s, &table, &left_out);
    Ok(json!({"row": row}))
}

/// The given attributes a new row leaves out: those naming no field, and those naming a field
/// another attribute already gives ([`given_attributes`]).
#[derive(Default)]
struct LeftOut {
    no_field: Vec<String>,
    taken: Vec<String>,
}

/// Each field's value in `"attributes"`, by field. A key gives one field: the one named exactly
/// so, else the only field named so ignoring case (ambiguous keys are refused); the keys named
/// exactly go first, so `Name` fills `Name` even when `name` comes first. A key naming no field,
/// or a field another key gives, is left out and said ([`notify_left_out`]). Non-object values
/// other than null are refused before any row is inserted.
fn given_attributes<'a>(fields: &[Field], p: &'a Json) -> Result<(Vec<Option<&'a Json>>, LeftOut), String> {
    let mut given = vec![None; fields.len()];
    let mut left = LeftOut::default();
    let attributes = match p.get("attributes") {
        None | Some(Json::Null) => return Ok((given, left)),
        Some(Json::Object(attributes)) => attributes,
        Some(_) => return Err("attributes is an object of field names and values".into()),
    };
    let exact = |k: &str| fields.iter().position(|f| f.name == k);
    for (k, v) in attributes.iter().filter(|(k, _)| exact(k).is_some()) {
        match exact(k).and_then(|i| given.get_mut(i)) {
            Some(slot @ None) => *slot = Some(v),
            _ => left.taken.push(k.clone()),
        }
    }
    for (k, v) in attributes.iter().filter(|(k, _)| exact(k).is_none()) {
        let mut matches = fields.iter().enumerate().filter(|(_, f)| f.name.eq_ignore_ascii_case(k));
        let first = matches.next().map(|(i, _)| i);
        if matches.next().is_some() {
            return Err(format!("attribute '{k}' matches more than one field: use an exact field name"));
        }
        match first.map(|i| given.get_mut(i)) {
            None => left.no_field.push(k.clone()),
            Some(Some(slot @ None)) => *slot = Some(v),
            Some(_) => left.taken.push(k.clone()),
        }
    }
    Ok((given, left))
}

/// Says, once per kind, which given attributes a new row in layer or table `name` left out.
fn notify_left_out(s: &mut Session, name: &str, left: &LeftOut) {
    for (keys, why) in [(&left.no_field, "naming no field"), (&left.taken, "naming a field another attribute already gives")] {
        if keys.is_empty() {
            continue;
        }
        let shown: Vec<String> = keys.iter().take(5).map(|k| format!("'{}'", k.chars().take(40).collect::<String>())).collect();
        let more = if keys.len() > 5 { format!(" and {} more", keys.len() - 5) } else { String::new() };
        let n = if keys.len() == 1 { "1 attribute".to_string() } else { format!("{} attributes", keys.len()) };
        s.notify(format!("{name}: the new row leaves out {n} {why}: {}{more}", shown.join(", ")), false);
    }
}

/// What writing a value took from it: a notice key ([`notify_loss`]) and the loss's code.
type CellLoss = (&'static str, LossCode);

/// What writing a row's values took from them, by field.
type Losses = Vec<(String, CellLoss)>;

/// A new row's values for `fields`: those given (`"attributes"`, [`given_attributes`]), else
/// `defaults` (a feature template's), each as its field holds it, with what that took from each
/// and the attributes left out. A value a field can't hold at all (`soon` in a date field) is
/// refused.
fn new_row(fields: &[Field], p: &Json, defaults: &[(String, Value)]) -> Result<(Vec<Value>, Losses, LeftOut), String> {
    let mut losses = Vec::new();
    let (given, left_out) = given_attributes(fields, p)?;
    let attrs = fields
        .iter()
        .zip(given)
        .map(|(f, given)| {
            let given = given.map(Value::from_json);
            let default = defaults.iter().find(|(k, _)| k.eq_ignore_ascii_case(&f.name)).map(|(_, v)| v.clone());
            let value = given.or(default).unwrap_or(Value::Null);
            if non_finite_input(&value, f.ty) {
                return Err(format!("field '{}' requires a finite number", f.name));
            }
            let coerced = coerce(value.clone(), f.ty);
            // A value a date field can't hold (`soon`, `20260305`) is refused, not stored as null.
            if f.ty.is_temporal() && coerced.is_null() && !value.asks_null() {
                return Err(format!("field '{}': '{value}' isn't a {} value", f.name, f.ty.label()));
            }
            if let Some(key) = numeric_loss(&value, &coerced, f.ty) {
                losses.push((f.name.clone(), key));
            }
            // A date-time (a native caller's template default) keeps only its day in a date field.
            if matches!(value, Value::DateTime(t) if f.ty == FieldType::Date && !t.is_date()) {
                losses.push((
                    f.name.clone(),
                    ("{layer}, field '{field}', row {row}: 1 attribute value kept only its day", LossCode::DroppedTime),
                ));
            }
            // A date (a native caller's template default) is no yes/no value: null, and said.
            if f.ty == FieldType::Boolean && coerced.is_null() && !value.asks_null() {
                losses.push((
                    f.name.clone(),
                    (
                        "{layer}, field '{field}', row {row}: 1 attribute value was written as null because it isn't a yes/no value",
                        LossCode::DroppedValue,
                    ),
                ));
            }
            Ok(coerced)
        })
        .collect::<Result<Vec<Value>, String>>()?;
    Ok((attrs, losses, left_out))
}

/// Says what writing a value of `field`, in row `row` of layer or table `name`, took from it.
fn notify_loss(s: &mut Session, (key, code): CellLoss, name: &str, field: String, row: usize) {
    s.notify_loss(
        code,
        crate::notice_text::NoticeText::format(
            key,
            [("layer", name.to_string().into()), ("field", field.into()), ("row", row.saturating_add(1).to_string().into())],
        ),
    );
}

/// The `geometry` parameter as a geometry. A GeometryCollection of more or fewer than one member
/// (at any depth) is refused as such, not as something that isn't GeoJSON.
pub(crate) fn geometry_param(p: &Json) -> Result<Geometry, String> {
    let given = p.get("geometry");
    if let Some(g) = given.and_then(rustgis_data::io::geometry_from_geojson) {
        return Ok(g);
    }
    let mut g = given;
    // Bounded by the JSON's own depth: each round goes one level in.
    while let Some(c) = g.filter(|g| g.get("type").and_then(Json::as_str) == Some("GeometryCollection")) {
        match c.get("geometries").and_then(Json::as_array).map(Vec::as_slice) {
            Some([only]) => g = Some(only),
            Some(_) => return Err("a GeometryCollection must hold exactly one geometry".into()),
            None => break,
        }
    }
    Err("geometry must be a GeoJSON geometry".into())
}

/// Rejects empty or degenerate geometry, which would break extents, labels and rendering.
pub(crate) fn validate(g: &Geometry) -> Result<(), String> {
    match g {
        Geometry::Point(_) => Ok(()),
        Geometry::MultiPoint(v) if v.is_empty() => Err("a multipoint needs at least one point".into()),
        Geometry::MultiPoint(_) => Ok(()),
        Geometry::Line(parts) if parts.is_empty() || parts.iter().any(|p| p.len() < 2) => Err("a line needs at least two points".into()),
        Geometry::Line(_) => Ok(()),
        Geometry::Polygon(polys) if polys.is_empty() || polys.iter().any(|rings| rings.is_empty() || rings.iter().any(|r| r.len() < 4)) => {
            Err("a polygon needs at least three points".into())
        }
        Geometry::Polygon(_) => Ok(()),
    }
}

/// `v` as a value of a `ty` field. A date-time into a date field is the day it shows. A number
/// goes into a number field as it is, not through the text it shows (which rounds a decimal to six
/// places); a decimal, or text written as one, into a
/// whole-number field loses its fraction (toward zero); a whole number past 2^53 into a decimal
/// field is the nearest decimal. Not a number, and an infinity in a whole-number field, are null.
/// A decimal goes into a text field in full, as a file writes it. Other values go through their
/// text, and what the field can't hold is null. A number past the 64-bit range is held at the range's end, as it was; [`coerce_cut`], for
/// callers that say what was cut, makes it null instead.
pub(crate) fn coerce(v: Value, ty: FieldType) -> Value {
    match convert(v, ty) {
        (_, Cut::Range, Some(f)) => Value::Int(f as i64),
        (v, _, _) => v,
    }
}

/// What [`coerce_cut`] took from a number to put it in a whole-number field, or from a date-time
/// to put it in a date field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Cut {
    /// Nothing.
    None,
    /// Its fraction: 2.7 is 2, −2.7 is −2.
    Fraction,
    /// All of it: a number past the 64-bit range is null.
    Range,
    /// All of it: not a number (NaN), or a word read as one, is null.
    NotANumber,
    /// All of it: an infinity, given as a number or as text ("inf"), is null in a field that
    /// holds only finite numbers.
    NotFinite,
    /// A date-time's time of day or zone: a date field keeps its day.
    Time,
    /// Digits past those a decimal holds: a whole number past 2^53 in a decimal field, or text
    /// with more significant digits than the decimal it is read through
    /// ([`Value::rounds_as_double`], [`double_holds`](rustgis_data::io::double_holds)).
    Rounded,
}

/// How many of a field's results [`coerce_cut`] cut, of each kind, for one notice per kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cuts {
    fractions: usize,
    past: usize,
    not_numbers: usize,
    not_finite: usize,
    times: usize,
    rounded: usize,
}

impl Cuts {
    pub(crate) fn add(&mut self, cut: Cut) {
        match cut {
            Cut::None => {}
            Cut::Fraction => self.fractions += 1,
            Cut::Range => self.past += 1,
            Cut::NotANumber => self.not_numbers += 1,
            Cut::NotFinite => self.not_finite += 1,
            Cut::Time => self.times += 1,
            Cut::Rounded => self.rounded += 1,
        }
    }

    /// The notices for what writing the whole-number or date field `field` cut, each starting with `by`
    /// (the command or rule that wrote it). Empty when nothing was cut.
    #[cfg(test)]
    pub(crate) fn notices(&self, by: &str, field: &str) -> Vec<String> {
        self.classified(by, field).into_iter().map(|(_, text)| text).collect()
    }

    /// Pairs each coercion message with its cause at the point the loss is counted.
    pub(crate) fn classified(&self, by: &str, field: &str) -> Vec<(LossCode, String)> {
        let results = |n: usize| if n == 1 { "1 result".to_string() } else { format!("{n} results") };
        let mut out = Vec::new();
        if self.fractions > 0 {
            let their = if self.fractions == 1 { "its" } else { "their" };
            out.push((
                LossCode::ChangedTruncated,
                format!("{by}: '{field}' holds whole numbers, so {} lost {their} fraction", results(self.fractions)),
            ));
        }
        if self.past > 0 {
            out.push((LossCode::DroppedValue, format!("{by}: '{field}' is null for {} past the 64-bit integer range", results(self.past))));
        }
        if self.not_numbers > 0 {
            let are = if self.not_numbers == 1 { "isn't a number" } else { "aren't numbers" };
            out.push((LossCode::DroppedValue, format!("{by}: '{field}' is null for {} that {are}", results(self.not_numbers))));
        }
        if self.not_finite > 0 {
            let are = if self.not_finite == 1 { "isn't a finite number" } else { "aren't finite numbers" };
            out.push((LossCode::DroppedValue, format!("{by}: '{field}' is null for {} that {are}", results(self.not_finite))));
        }
        if self.times > 0 {
            let their = if self.times == 1 { "its" } else { "their" };
            out.push((LossCode::DroppedTime, format!("{by}: '{field}' holds dates, so {} kept only {their} day", results(self.times))));
        }
        if self.rounded > 0 {
            let were = if self.rounded == 1 { "was" } else { "were" };
            out.push((LossCode::ChangedRounded, format!("{by}: {} {were} rounded to the digits '{field}' holds", results(self.rounded))));
        }
        out
    }
}

/// The rows a notice lists by number, at most.
const LISTED_ROWS: usize = 3;

/// What writing a calculation's results into one field lost, for its notices: what the field cut
/// ([`Cuts`]), results it can't hold (text that isn't a number or a date), and nulls an
/// expression's error gave (a division by zero), by kind with the first rows of each.
#[derive(Clone, Debug, Default)]
pub(crate) struct WriteLosses {
    cuts: Cuts,
    unread: usize,
    /// Each kind of error met, in the order met: how many rows, and the first [`LISTED_ROWS`].
    errors: Vec<(rustgis_data::query::Fault, usize, Vec<usize>)>,
    /// Each row and kind of loss counted, for an edit that may write a row more than once
    /// ([`once_per_row`]); none where each row is written once (Calculate Field), so nothing
    /// grows with the rows.
    ///
    /// [`once_per_row`]: WriteLosses::once_per_row
    counted: Option<std::collections::HashSet<(usize, Loss)>>,
}

/// A kind of loss [`WriteLosses`] counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Loss {
    Cut(Cut),
    Unread,
    Error(rustgis_data::query::Fault),
}

impl Loss {
    /// The loss as a preview names it; `None` for a cut that took nothing.
    fn previewed(self) -> Option<PreviewLoss> {
        Some(match self {
            Loss::Cut(Cut::None) => return None,
            Loss::Cut(Cut::Fraction) => PreviewLoss::Fraction,
            Loss::Cut(Cut::Range) => PreviewLoss::Range,
            Loss::Cut(Cut::NotANumber) => PreviewLoss::NotANumber,
            Loss::Cut(Cut::NotFinite) => PreviewLoss::NotFinite,
            Loss::Cut(Cut::Time) => PreviewLoss::Time,
            Loss::Cut(Cut::Rounded) => PreviewLoss::Rounded,
            Loss::Unread => PreviewLoss::Unheld,
            Loss::Error(fault) => PreviewLoss::Error(fault),
        })
    }
}

/// What writing a row's result into its field loses, as a Calculate Field preview says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewLoss {
    /// A whole-number field drops the fraction.
    Fraction,
    /// A number past the 64-bit integer range is null.
    Range,
    /// Not a number (NaN), or a word read as one, is null.
    NotANumber,
    /// An infinity is null.
    NotFinite,
    /// A date field keeps a date-time's day.
    Time,
    /// Digits past those the field holds are rounded.
    Rounded,
    /// A result the field can't hold (text that isn't a number or a date) is null.
    Unheld,
    /// The expression's error made it null.
    Error(rustgis_data::query::Fault),
}

/// A result as a field holds it, with what holding it lost.
struct Held {
    value: Value,
    /// What the coercion to the field's type took.
    cut: Option<Loss>,
    /// The error that made it null or, for a null nobody asked for, the value not read.
    other: Option<Loss>,
}

/// A result and the error that made it null, if one did, as a `ty` field holds it.
fn held((v, fault): (Value, Option<rustgis_data::query::Fault>), ty: FieldType) -> Held {
    let asked_null = v.asks_null();
    let (value, cut) = coerce_cut(v, ty);
    let other = match fault.filter(|_| value.is_null()) {
        Some(fault) => Some(Loss::Error(fault)),
        None => (!asked_null && value.is_null() && cut == Cut::None).then_some(Loss::Unread),
    };
    Held { value, cut: (cut != Cut::None).then_some(Loss::Cut(cut)), other }
}

impl WriteLosses {
    /// A tally for an edit that may write the same row more than once, counting each kind of
    /// loss once per row: a later write that loses something else is still counted.
    pub(crate) fn once_per_row() -> WriteLosses {
        WriteLosses { counted: Some(Default::default()), ..Default::default() }
    }

    /// Whether `row`'s `loss` is yet to be counted, marking it counted.
    fn first(&mut self, row: usize, loss: Loss) -> bool {
        self.counted.as_mut().is_none_or(|c| c.insert((row, loss)))
    }

    /// Row `row`'s result and the error that made it null, if one did, as a `ty` field holds it,
    /// counting what that lost.
    pub(crate) fn write(&mut self, row: usize, result: (Value, Option<rustgis_data::query::Fault>), ty: FieldType) -> Value {
        let Held { value, cut, other } = held(result, ty);
        for loss in [cut, other].into_iter().flatten() {
            if !self.first(row, loss) {
                continue;
            }
            match loss {
                Loss::Cut(cut) => self.cuts.add(cut),
                Loss::Unread => self.unread += 1,
                Loss::Error(fault) => match self.errors.iter_mut().find(|(f, ..)| *f == fault) {
                    Some((_, n, rows)) => {
                        *n += 1;
                        if rows.len() < LISTED_ROWS {
                            rows.push(row);
                        }
                    }
                    None => self.errors.push((fault, 1, vec![row])),
                },
            }
        }
        value
    }

    /// What a tally of one row's write says, as a preview names it, in the order [`held`] gives
    /// them: the error that made it null, a result the field couldn't hold, then what a cut took.
    /// `None` for a clean write.
    pub(crate) fn previewed(&self) -> Option<PreviewLoss> {
        let cuts = &self.cuts;
        if let Some((fault, ..)) = self.errors.first() {
            return Some(PreviewLoss::Error(*fault));
        }
        [
            (self.unread, PreviewLoss::Unheld),
            (cuts.fractions, PreviewLoss::Fraction),
            (cuts.past, PreviewLoss::Range),
            (cuts.not_numbers, PreviewLoss::NotANumber),
            (cuts.not_finite, PreviewLoss::NotFinite),
            (cuts.times, PreviewLoss::Time),
            (cuts.rounded, PreviewLoss::Rounded),
        ]
        .into_iter()
        .find_map(|(n, loss)| (n > 0).then_some(loss))
    }

    /// The notices for what writing `field`, of type `ty`, lost, each starting with `by` (the
    /// command or rule that wrote it). Rows are numbered from 1, as `@row_number` numbers them.
    /// The error notices are templates a translated interface shows in its language.
    pub(crate) fn notices(&self, by: NoticeText, field: &str, ty: FieldType) -> Vec<(LossCode, NoticeText)> {
        let rows = |n: usize| if n == 1 { "1 row".to_string() } else { format!("{n} rows") };
        let english = by.english();
        let mut out: Vec<_> = self.cuts.classified(&english, field).into_iter().map(|(code, text)| (code, text.into())).collect();
        if self.unread > 0 {
            out.push((
                LossCode::DroppedValue,
                format!("{english}: '{field}' is null in {}, whose result isn't a {} value", rows(self.unread), ty.label()).into(),
            ));
        }
        for (fault, n, listed) in &self.errors {
            let numbers: Vec<String> = listed.iter().map(|r| r.saturating_add(1).to_string()).collect();
            let key = match (*n, *n > listed.len()) {
                (1, _) => "{by}: '{field}' is null in 1 row because of {cause} (row {rows})",
                (_, false) => "{by}: '{field}' is null in {count} rows because of {cause} (rows {rows})",
                (_, true) => "{by}: '{field}' is null in {count} rows because of {cause} (rows {rows}…)",
            };
            out.push((
                LossCode::DroppedCalculationResult,
                NoticeText::format(
                    key,
                    [
                        ("by", by.clone()),
                        ("field", field.to_string().into()),
                        ("count", n.to_string().into()),
                        ("cause", NoticeText::term(fault.describe())),
                        ("rows", numbers.join(", ").into()),
                    ],
                ),
            ));
        }
        out
    }
}

/// [`coerce`], with a number past the 64-bit range null, and what it took from a number to put it
/// in a whole-number field.
pub(crate) fn coerce_cut(v: Value, ty: FieldType) -> (Value, Cut) {
    if ty == FieldType::Double && non_finite_input(&v, ty) {
        return (Value::Null, if infinite(&v) { Cut::NotFinite } else { Cut::NotANumber });
    }
    let (v, cut, _) = convert(v, ty);
    (v, cut)
}

/// Text and native numbers that cannot be retained as numeric field values.
/// Ordinary invalid words keep the existing parser's behavior; non-finite numbers are explicit.
pub(crate) fn non_finite_input(value: &Value, ty: FieldType) -> bool {
    ty.is_numeric()
        && match value {
            Value::Float(f) => !f.is_finite(),
            Value::Text(text) => text.trim().parse::<f64>().is_ok_and(|f| !f.is_finite()),
            _ => false,
        }
}

/// A value set once for a field and used again (a template's or subtype's default, a domain's
/// code, a contingent combination) as a `ty` field holds it, refused when holding it would lose
/// any of it: one that isn't finite or isn't a value of the field's type, or that the field would
/// cut, clamp or round. It is checked when it is set, as no later edit could say what was lost.
pub(crate) fn held_default(field: &str, value: &Value, ty: FieldType) -> Result<Value, String> {
    if non_finite_input(value, ty) {
        return Err(format!("field '{field}' requires a finite number"));
    }
    let coerced = coerce(value.clone(), ty);
    // Named as given, not as shown (which rounds a decimal to six places).
    let given = value.to_file_text();
    if coerced.is_null() && !value.asks_null() {
        return Err(format!("field '{field}': '{given}' isn't a {} value", ty.label()));
    }
    if numeric_loss(value, &coerced, ty).is_some() || matches!(value, Value::DateTime(t) if ty == FieldType::Date && !t.is_date()) {
        return Err(format!("field '{field}': a {} field can't hold '{given}' as given", ty.label()));
    }
    Ok(coerced)
}

/// Whether a number, or a number written as text, is an infinity (not NaN).
fn infinite(value: &Value) -> bool {
    match value {
        Value::Float(f) => f.is_infinite(),
        Value::Text(text) => text.trim().parse::<f64>().is_ok_and(f64::is_infinite),
        _ => false,
    }
}

/// [`coerce_cut`], and for a number past the range the decimal it was read as.
fn convert(v: Value, ty: FieldType) -> (Value, Cut, Option<f64>) {
    match (v, ty) {
        (Value::Null, _) => (Value::Null, Cut::None, None),
        (Value::Int(i), FieldType::Integer) => (Value::Int(i), Cut::None, None),
        (Value::Float(f), FieldType::Integer) if f.is_infinite() => (Value::Null, Cut::NotFinite, None),
        (Value::Float(f), FieldType::Integer) => whole(f),
        (Value::Float(f), FieldType::Double) => (Value::Float(f), Cut::None, None),
        (Value::Int(i), FieldType::Double) => {
            (Value::Float(i as f64), if Value::Int(i).rounds_as_double() { Cut::Rounded } else { Cut::None }, None)
        }
        // A date-time into a date field is the day it shows, and said so when it had a time or zone.
        (Value::DateTime(t), FieldType::Date) => (Value::Date(t.date()), if t.is_date() { Cut::None } else { Cut::Time }, None),
        // A date is no number or yes/no: null (and said by the caller), not read as false.
        (Value::Date(_) | Value::DateTime(_), FieldType::Integer | FieldType::Double | FieldType::Boolean) => {
            (Value::Null, Cut::None, None)
        }
        // Text written as a whole number, or as a decimal without an exponent, is read from its
        // digits; any other number written as text, as a decimal.
        (Value::Text(t), FieldType::Integer) => match t.trim().parse::<i64>() {
            Ok(i) => (Value::Int(i), Cut::None, None),
            Err(_) => match (decimal_text(t.trim()), t.trim().parse::<f64>()) {
                (Some((v, cut)), Ok(f)) => (v, cut, Some(f)),
                // A word the decimal reader takes ("inf", "NaN") isn't a number written as text:
                // null, and said so, as not a number.
                // A fraction the decimal rounded away (`2.0000000000000000001e0`) is still one.
                // A fraction the decimal rounded away (`2.0000000000000000001e0`) is still one, and
                // digits past those it holds (`9007199254740993e0`) are rounded.
                (None, Ok(f)) if f.is_finite() || t.bytes().any(|b| b.is_ascii_digit()) => match whole(f) {
                    (v @ Value::Int(_), Cut::None, x) if Value::text_has_fraction(&t) => (v, Cut::Fraction, x),
                    (v @ Value::Int(_), Cut::None, x) if !rustgis_data::io::double_holds(t.trim()) => (v, Cut::Rounded, x),
                    held => held,
                },
                (None, Ok(f)) if f.is_infinite() => (Value::Null, Cut::NotFinite, None),
                (None, Ok(_)) => (Value::Null, Cut::NotANumber, None),
                _ => (Value::parse(&t, ty), Cut::None, None),
            },
        },
        // A decimal goes into a text field in full, not as the six places it's shown to.
        (Value::Float(f), FieldType::Text) if f.is_finite() => (Value::Text(Value::Float(f).to_file_text()), Cut::None, None),
        // A whole number is yes when it isn't 0, as a shapefile writes it.
        (Value::Int(i), FieldType::Boolean) => (Value::Bool(i != 0), Cut::None, None),
        (Value::Float(f), FieldType::Boolean) if f.fract() == 0.0 => (Value::Bool(f != 0.0), Cut::None, None),
        // A fraction is no yes/no value: null, not the text it's shown as (`0.0000001` shows `0`).
        (Value::Float(_), FieldType::Boolean) => (Value::Null, Cut::None, None),
        (Value::Text(t), FieldType::Double) => match Value::parse_source(&t, ty) {
            v @ Value::Float(_) if Value::Text(t).rounds_as_double() => (v, Cut::Rounded, None),
            v => (v, Cut::None, None),
        },
        // Uncounted coercion retains source numbers until output can explain any loss.
        (Value::Text(t), _) => (Value::parse_source(&t, ty), Cut::None, None),
        (other, _) => (Value::parse(&other.to_string(), ty), Cut::None, None),
    }
}

/// A decimal as a whole number, toward zero, and what that took: null past the 64-bit range.
fn whole(f: f64) -> (Value, Cut, Option<f64>) {
    // 2^63: i64::MIN is exactly a decimal; i64::MAX rounds up to 2^63, which is past the range.
    const TWO_63: f64 = 9_223_372_036_854_775_808.0;
    // Not a number is no whole number, nor one past the range: null, and said so.
    if f.is_nan() {
        return (Value::Null, Cut::NotANumber, None);
    }
    let t = f.trunc();
    if !(-TWO_63..TWO_63).contains(&t) {
        return (Value::Null, Cut::Range, Some(f));
    }
    (Value::Int(t as i64), if t == f { Cut::None } else { Cut::Fraction }, None)
}

/// Text written as a whole number or as a decimal without an exponent ("-12.50", ".5"), as a
/// whole number toward zero read from its digits, exactly up to the 64-bit range's ends, and what
/// that took. None for other text. A whole number just past i64::MIN is past the range here, where
/// read as a decimal it would round to -2^63, which is i64::MIN.
fn decimal_text(t: &str) -> Option<(Value, Cut)> {
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    let digits = int.strip_prefix(['-', '+']).unwrap_or(int);
    if !digits.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) || digits.len() + frac.len() == 0 {
        return None;
    }
    let cut = if frac.bytes().any(|b| b != b'0') { Cut::Fraction } else { Cut::None };
    // "-.5" and ".5" are 0 and a fraction; the sign stays with the digits for i64::MIN.
    let whole = if digits.is_empty() { Ok(0) } else { int.parse::<i64>() };
    Some(match whole {
        Ok(i) => (Value::Int(i), cut),
        Err(_) => (Value::Null, Cut::Range),
    })
}

fn delete(s: &mut Session, _: &Json) -> CmdResult {
    let mut n = 0;
    let mut removed: Vec<(u32, Vec<super::design::Change>)> = Vec::new();
    for l in &mut s.map_mut().layers {
        if l.selection.is_empty() {
            continue;
        }
        let mut sel = std::mem::take(&mut l.selection);
        sel.sort_unstable();
        sel.dedup();
        let mut changes = Vec::new();
        for r in sel.into_iter().rev() {
            if r < l.dataset.features.len() {
                changes.push(super::design::Change { before: Some(l.dataset.features.remove(r)), row: None });
                n += 1;
            }
        }
        l.touch_geometry();
        removed.push((l.id, changes));
    }
    for (id, changes) in removed {
        super::design::apply(s, id, changes)?;
    }
    s.notify(format!("Deleted {n} features"), false);
    Ok(json!({"deleted": n}))
}

/// A numeric conversion's loss, shared by edits and feature creation. Null entered explicitly is quiet.
fn numeric_loss(input: &Value, v: &Value, ty: FieldType) -> Option<CellLoss> {
    if ty.is_numeric() && v.is_null() && !input.asks_null() {
        Some((
            "{layer}, field '{field}', row {row}: 1 attribute value was written as null because it isn't a number",
            LossCode::DroppedValue,
        ))
    } else if ty == FieldType::Integer {
        let (exact, cut) = coerce_cut(input.clone(), ty);
        match cut {
            Cut::Fraction => Some(("{layer}, field '{field}', row {row}: 1 attribute value lost its fraction", LossCode::ChangedTruncated)),
            // The value given is gone: the range's limit stands in its place.
            Cut::Range => Some((
                "{layer}, field '{field}', row {row}: 1 attribute value was clamped to the 64-bit integer range",
                LossCode::DroppedValue,
            )),
            Cut::Rounded => {
                Some(("{layer}, field '{field}', row {row}: 1 attribute value lost integer precision", LossCode::ChangedRounded))
            }
            Cut::None if &exact != v => {
                Some(("{layer}, field '{field}', row {row}: 1 attribute value lost integer precision", LossCode::ChangedRounded))
            }
            _ => None,
        }
    } else if ty == FieldType::Double && matches!(v, Value::Float(_)) && coerce_cut(input.clone(), ty).1 == Cut::Rounded {
        Some(("{layer}, field '{field}', row {row}: 1 attribute value was rounded to the digits a decimal holds", LossCode::ChangedRounded))
    } else {
        None
    }
}

/// `edit.setValue`: one cell of a layer (checked against its design, which may undo it) or of a
/// standalone table, as its field holds it ([`typed`]), saying what that lost. A table's first
/// copy of shared rows runs on the desktop table worker ([`table_ahead`]).
fn set_value(s: &mut Session, p: &Json) -> CmdResult {
    if let Some(result) = land_table_edit(s, p, "edit.setValue") {
        return result;
    }
    let t = write_target(s, p)?;
    // A row past `usize` (on a 32-bit build) is no row, not a row it wraps round to.
    let row = usize::try_from(p.get("row").and_then(Json::as_u64).ok_or("row is required")?).map_err(|_| "no such row")?;
    let fi = field_of(s, t, ps(p, "field").ok_or("field is required")?)?;
    let raw = p.get("value").cloned().unwrap_or(Json::Null);
    let input = Value::from_json(&raw);
    let id = match t {
        Target::Layer(id) => id,
        // A table's row: the value, said if it lost something; it has no design to check it against.
        Target::Table(_) => {
            editing_on(s)?;
            let ds = data_mut(s, t)?;
            let (ty, field) = ds.fields.get(fi).map(|f| (f.ty, f.name.clone())).ok_or("no field")?;
            let v = typed(raw, ty)?;
            let loss = numeric_loss(&input, &v, ty);
            // Looked at before writing: a write copies rows shared with the Undo snapshot (#2431).
            ds.features.get(row).and_then(|f| f.attrs.get(fi)).ok_or("no such row")?;
            let cell = ds.features.get_mut(row).and_then(|f| f.attrs.get_mut(fi)).ok_or("no such row")?;
            *cell = v;
            let table = ds.name.clone();
            if let Some(key) = loss {
                notify_loss(s, key, &table, field, row);
            }
            return Ok(Json::Null);
        }
    };
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let ty = l.dataset.fields.get(fi).map(|f| f.ty).ok_or("no field")?;
    let field = l.dataset.fields.get(fi).map(|f| f.name.clone()).ok_or("no field")?;
    let layer = l.dataset.name.clone();
    let v = typed(raw, ty)?;
    // Keep the existing finite coercion, but say when it discards information.
    let loss = numeric_loss(&input, &v, ty);
    // Looked at before writing: a write copies rows shared with the Undo snapshot (#2431).
    l.dataset.features.get(row).and_then(|f| f.attrs.get(fi)).ok_or("no such row")?;
    let f = l.dataset.features.get_mut(row).ok_or("no such row")?;
    let before = f.clone();
    let cell = f.attrs.get_mut(fi).ok_or("no such row")?;
    *cell = v;
    super::design::apply(s, id, vec![super::design::Change { before: Some(before), row: Some(row) }])?;
    if let Some(key) = loss {
        notify_loss(s, key, &layer, field, row);
    }
    Ok(Json::Null)
}

/// `raw` (an `edit.setValue` value) as a value of a `ty` field. A number that isn't finite, a
/// value a date field can't hold, and text that isn't a yes/no word in a yes/no field, are refused.
fn typed(raw: Json, ty: FieldType) -> Result<Value, String> {
    Ok(match raw {
        // A number that isn't finite (NaN, infinity) is no value a number field holds, nor one a
        // project can save: refused, as the attribute table refuses it.
        Json::String(t) if ty.is_numeric() && t.trim().parse::<f64>().is_ok_and(|f| !f.is_finite()) => {
            return Err(format!("'{}' isn't a finite number, which a {} field holds", t.trim(), ty.label()));
        }
        // Text a date or yes/no field can't hold (`soon`, `maybe`) is refused, not stored as null.
        Json::String(t) => match Value::parse(&t, ty) {
            Value::Null
                if (ty.is_temporal() || ty == FieldType::Boolean) && !t.trim().is_empty() && !t.trim().eq_ignore_ascii_case("<null>") =>
            {
                return Err(format!("'{}' isn't a {} value", t.trim(), ty.label()));
            }
            v => v,
        },
        // So is any other value a date or yes/no field can't hold (`20260305`, `0.5`).
        other => match coerce(Value::from_json(&other), ty) {
            Value::Null if (ty.is_temporal() || ty == FieldType::Boolean) && !other.is_null() => {
                return Err(format!("'{other}' isn't a {} value", ty.label()));
            }
            v => v,
        },
    })
}

/// Adds an empty field of a type: long, double, text, boolean, date or datetime. Table rows
/// are filled on the desktop worker ([`table_ahead`]), with progress and Cancel.
fn add_field(s: &mut Session, p: &Json) -> CmdResult {
    if let Some(result) = land_table_edit(s, p, "edit.addField") {
        return result;
    }
    let t = write_target(s, p)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?;
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_') || name.chars().count() > 64 {
        return Err("field names use letters, digits and _ (at most 64)".into());
    }
    let ty = match ps(p, "type").unwrap_or("double").to_ascii_lowercase().as_str() {
        "long" | "integer" | "int" | "short" => FieldType::Integer,
        "double" | "float" | "number" => FieldType::Double,
        "text" | "string" => FieldType::Text,
        "boolean" | "bool" => FieldType::Boolean,
        "date" => FieldType::Date,
        "datetime" | "date-time" | "timestamp" => FieldType::DateTime,
        other => return Err(format!("unknown field type '{other}'")),
    };
    let ds = data_mut(s, t)?;
    if ds.field_index(name).is_some() {
        return Err(format!("a field named '{name}' already exists"));
    }
    let table = matches!(t, Target::Table(_));
    ds.fields.push(Field::new(name, ty));
    if table {
        rustgis_data::progress::begin(ds.features.len());
    }
    for row in &mut ds.features {
        if table {
            rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        }
        // Adding one column to shared rows should not double every row's attribute capacity.
        row.attrs.reserve_exact(1);
        row.attrs.push(Value::Null);
    }
    Ok(Json::Null)
}

/// `edit.convertField`: makes a text field a date or date-time field, reading each value as ISO
/// 8601 by default, or an explicit numeric pattern or spreadsheet epoch (`2026-03-05`;
/// `2026-03-05T14:30:00+02:00`, or with a space for the `T`). A date field
/// also takes a date-time at midnight without a zone. Values that aren't dates become null, said
/// with their count and the first few, and undo brings them back; digits past the microsecond are
/// rounded off, and said. A field with text and none of it a date is refused and stays text. Refused while a data design
/// (domain, subtype, contingent values) names the field or a definition query or display filter
/// would stop reading with it. Layer and table conversions run on the desktop worker
/// ([`table_ahead`]), with progress and Cancel; a layer read in place is refused as every
/// `edit.*` command is. The shared rows' first copy cannot be interrupted.
fn convert_field(s: &mut Session, p: &Json) -> CmdResult {
    // A prepared result has one concrete type: do not consume it while probing another type.
    let landed = match target(s, p)? {
        Target::Layer(_) => land_layer_dates(s, p),
        Target::Table(_) => land_table_edit(s, p, "edit.convertField"),
    };
    if let Some(result) = landed {
        return result;
    }
    let t = write_target(s, p)?;
    let fi = field_of(s, t, ps(p, "field").ok_or("field is required")?)?;
    let ty = match ps(p, "type").ok_or("type is required")?.to_ascii_lowercase().as_str() {
        "date" => FieldType::Date,
        "datetime" | "date-time" | "timestamp" => FieldType::DateTime,
        other => return Err(format!("'{other}' isn't a type a field converts to: date or datetime")),
    };
    let format = match p.get("format") {
        None => "iso",
        Some(v) => v.as_str().ok_or("format must be a string")?,
    };
    let pivot = p.get("pivotYear").map(|v| v.as_i64().ok_or("pivotYear must be an integer")).transpose()?;
    let reader = rustgis_data::date_format::DateFormat::new(format, pivot)?;
    let (name, Dates { converted, nulled, rounded, examples }) = match t {
        Target::Table(_) => {
            let ds = data_mut(s, t)?;
            let field = ds.fields.get(fi).ok_or("no such field")?;
            let name = field.name.clone();
            if field.ty != FieldType::Text {
                return Err(format!("'{name}' is a {} field: only a text field converts to dates", field.ty.label()));
            }
            (name, to_dates(ds, fi, ty, &reader)?)
        }
        Target::Layer(id) => convert_layer_field(s, id, fi, ty, &reader)?,
    };
    let (kind, one) = if ty == FieldType::Date { ("dates", "a date") } else { ("date-times", "a date-time") };
    if nulled > 0 {
        let rows = if nulled == 1 { "1 row".to_string() } else { format!("{nulled} rows") };
        // Text that asks for null isn't counted (`to_dates`): only text that isn't a date.
        s.say(rustgis_data::loss::Note::loss(
            LossCode::DroppedValue,
            format!("Convert Field: '{name}' holds {kind} now, and is null in {rows} whose text isn't {one}: {}", examples.join(", ")),
        ));
    }
    if rounded > 0 {
        let values = if rounded == 1 { "1 value was".to_string() } else { format!("{rounded} values were") };
        s.say(rustgis_data::loss::Note::loss(
            LossCode::ChangedRounded,
            format!("Convert Field: {values} rounded to the microsecond in '{name}', as more digits than that aren't kept"),
        ));
    }
    Ok(json!({"converted": converted, "null": nulled}))
}

/// Field `fi` of layer `id` as dates ([`to_dates`]) and its name, unless something that reads it
/// would stop reading it (see [`convert_field`]).
fn convert_layer_field(
    s: &mut Session,
    id: u32,
    fi: usize,
    ty: FieldType,
    reader: &rustgis_data::date_format::DateFormat,
) -> Result<(String, Dates), String> {
    let name = check_layer_dates(s, id, fi, ty)?;
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    Ok((name, to_dates(&mut l.dataset, fi, ty, reader)?))
}

/// Checks every schema consumer again when prepared dates land, so concurrent design edits
/// cannot make the new type unreadable. Does not scan or copy feature rows.
fn check_layer_dates(s: &Session, id: u32, fi: usize, ty: FieldType) -> Result<String, String> {
    let l = s.map().layer(id).ok_or("no layer")?;
    let field = l.dataset.fields.get(fi).ok_or("no such field")?;
    let name = field.name.clone();
    if field.ty != FieldType::Text {
        return Err(format!("'{name}' is a {} field: only a text field converts to dates", field.ty.label()));
    }
    let d = &l.design;
    let designed = d.domains.iter().any(|x| x.field.eq_ignore_ascii_case(&name))
        || d.subtypes.as_ref().is_some_and(|st| st.field.eq_ignore_ascii_case(&name))
        || d.contingent.iter().any(|c| c.fields.iter().any(|f| f.eq_ignore_ascii_case(&name)));
    if designed {
        return Err(format!("'{name}' has a domain, subtypes or contingent values, which hold text: remove them first"));
    }
    // Saving edits writes values, not column types: the table's column would stay text.
    if l.gpkg.is_some() {
        return Err(format!(
            "'{name}' is a text column of the GeoPackage table being edited, and saving can't change a column's type: export the layer, or convert it in the file"
        ));
    }
    if l.postgis.is_some() {
        return Err(format!(
            "'{name}' is a text column of the PostGIS table being edited, and saving can't change a column's type: change it in the database"
        ));
    }
    // A filter that compares the text with a number stops reading once it's a date.
    let mut schema = rustgis_data::Dataset::new(l.dataset.name.clone(), l.dataset.geometry_type, l.dataset.fields.clone());
    if let Some(f) = schema.fields.get_mut(fi) {
        f.ty = ty;
    }
    let reads =
        |q: &str| rustgis_data::query::parse_fields(q, &l.dataset).is_err() || rustgis_data::query::parse_fields(q, &schema).is_ok();
    if let Some(q) = l.definition_query.as_deref().filter(|q| !reads(q)) {
        return Err(format!("the definition query ({q}) wouldn't read '{name}' as dates: change or clear it first"));
    }
    if let Some(f) = l.display_filters.iter().find(|f| !reads(&f.where_)) {
        return Err(format!("display filter '{}' wouldn't read '{name}' as dates: change or remove it first", f.name));
    }
    // Classes or labels by an expression would stop drawing by it, as Delete Field says.
    if l.renderer.expression().is_some_and(|e| !reads(e)) {
        return Err(format!("the symbology's expression wouldn't read '{name}' as dates: change it first"));
    }
    for c in std::iter::once(&l.labels.default).chain(&l.labels.classes) {
        if c.by.text.as_deref().is_some_and(|t| !reads(t)) {
            return Err(format!("the labels' text expression wouldn't read '{name}' as dates: change it first"));
        }
        if c.where_.as_deref().is_some_and(|w| !reads(w)) {
            return Err(format!("label class '{}' wouldn't read '{name}' as dates: change its clause first", c.name));
        }
    }
    // An attribute rule that stops reading would stop checking or calculating, without a word.
    let rule_reads = |r: &rustgis_map::design::AttributeRule, ds: &rustgis_data::Dataset| match r.kind {
        rustgis_map::design::RuleKind::Calculation => Calculation::parse(&r.expression, ds).is_ok(),
        rustgis_map::design::RuleKind::Constraint => rustgis_data::query::parse_fields(&r.expression, ds).is_ok(),
    };
    if let Some(r) = l.design.rules.iter().find(|r| rule_reads(r, &l.dataset) && !rule_reads(r, &schema)) {
        return Err(format!("attribute rule '{}' wouldn't read '{name}' as dates: change or remove it first", r.name));
    }
    Ok(name)
}

/// What turning a text field into dates did ([`to_dates`]).
struct Dates {
    /// Values that became dates.
    converted: usize,
    /// Values whose text wasn't a date, now null.
    nulled: usize,
    /// Date-times whose digits past the microsecond were rounded off.
    rounded: usize,
    /// The first few texts that weren't dates.
    examples: Vec<String>,
}

/// Makes text field `fi` of `ds` a `ty` (date or date-time) field: how many values became dates,
/// how many became null, how many were rounded to the microsecond, and the first few nulled.
/// A field with text and none of it a date is refused and left as it is.
fn to_dates(ds: &mut Dataset, fi: usize, ty: FieldType, reader: &rustgis_data::date_format::DateFormat) -> Result<Dates, String> {
    let name = ds.fields.get(fi).map(|f| f.name.clone()).unwrap_or_default();
    let mut out = Dates { converted: 0, nulled: 0, rounded: 0, examples: Vec::new() };
    let mut dates = Vec::with_capacity(ds.features.len());
    rustgis_data::progress::begin(ds.features.len());
    for f in &ds.features {
        rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        let Some(v) = f.attrs.get(fi) else {
            dates.push(None);
            continue;
        };
        let (date, cut) = match v {
            v if v.asks_null() => (Value::Null, false),
            Value::Text(t) => reader.parse(t, ty),
            other => reader.parse(&other.to_string(), ty),
        };
        if date.is_null() && !v.asks_null() {
            out.nulled += 1;
            if out.examples.len() < 3 {
                let text: String = v.to_string().chars().map(rustgis_data::io::dbf_notice_char).collect();
                let short: String = text.chars().take(40).collect();
                out.examples.push(if short.len() < text.len() { format!("'{short}…'") } else { format!("'{short}'") });
            }
        }
        out.converted += usize::from(!date.is_null());
        out.rounded += usize::from(cut);
        dates.push(Some(date));
    }
    if out.converted == 0 && out.nulled > 0 {
        let one = if ty == FieldType::Date { "a date" } else { "a date-time" };
        return Err(format!(
            "'{name}' stays text: none of its values is {one} ({}), first {}",
            reader.description(),
            out.examples.first().map_or("", String::as_str)
        ));
    }
    for (f, date) in ds.features.iter_mut().zip(dates) {
        if let (Some(v), Some(date)) = (f.attrs.get_mut(fi), date) {
            *v = date;
        }
    }
    if let Some(f) = ds.fields.get_mut(fi) {
        f.ty = ty;
    }
    Ok(out)
}

/// `edit.deleteField`: removes a field and the references to it (symbology and its expressions
/// by feature, labels and their expressions by feature, the data design, diagrams, the
/// extrusion's height and base, the marker rotation), or refuses while the definition query, a
/// display filter or the time slider reads it.
/// A standalone table's field just goes: nothing refers to it. Its rows are changed on the
/// desktop worker ([`table_ahead`]), with progress and Cancel.
fn delete_field(s: &mut Session, p: &Json) -> CmdResult {
    if let Some(result) = land_table_edit(s, p, "edit.deleteField") {
        return result;
    }
    let t = write_target(s, p)?;
    let fi = field_of(s, t, ps(p, "field").ok_or("field is required")?)?;
    let Target::Layer(id) = t else {
        let ds = data_mut(s, t)?;
        if fi < ds.fields.len() {
            ds.fields.remove(fi);
            rustgis_data::progress::begin(ds.features.len());
            for f in &mut ds.features {
                rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
                if fi < f.attrs.len() {
                    f.attrs.remove(fi);
                }
            }
        }
        return Ok(json!({"rulesRemoved": 0}));
    };
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    // A definition query or display filter that reads the field would stop reading without it,
    // and the layer would then draw and export the rows it leaves out: refused, naming it. What a
    // filter reads is the field its parse resolves to, so a name matching another but for case,
    // or a field named like a keyword, is judged by the field it actually reads.
    let name = l.dataset.fields.get(fi).map(|f| f.name.clone()).unwrap_or_default();
    let needs = |q: &str| rustgis_data::query::parse_fields(q, &l.dataset).is_ok_and(|e| rustgis_data::query::reads_field(&e, fi));
    if let Some(q) = l.definition_query.as_deref().filter(|q| needs(q)) {
        return Err(format!("the definition query ({q}) uses '{name}': change or clear it first"));
    }
    if let Some(f) = l.display_filters.iter().find(|f| needs(&f.where_)) {
        return Err(format!("display filter '{}' uses '{name}': change or remove it first", f.name));
    }
    // The time slider shows the rows whose dates fall in its window: without its field it would
    // show every row, so it is refused like a filter (#2147). Judged by the field the binding
    // reads, which may be named in another case.
    if l.time.as_ref().is_some_and(|t| l.dataset.field_index(&t.field) == Some(fi)) {
        return Err(format!("the layer is bound to the time slider by '{name}': unbind it first"));
    }
    let read_before = l.renderer.expression().is_some_and(|e| rustgis_data::query::parse_fields(e, &l.dataset).is_ok());
    // The symbol's expressions by feature that read the field, found while it is there (#2081).
    let by = &l.symbol_by;
    let by_read = [&by.size, &by.fill, &by.stroke, &by.stroke_width, &by.angle].map(|t| t.as_deref().is_some_and(needs));
    // And the labels' expressions by feature that read it, class by class, Default first (#2147).
    let label_read: Vec<[bool; 4]> = std::iter::once(&l.labels.default)
        .chain(&l.labels.classes)
        .map(|c| [&c.by.text, &c.by.size, &c.by.color, &c.by.angle].map(|t| t.as_deref().is_some_and(needs)))
        .collect();
    // A Default class still as the layer made it loses nothing by starting again: that restart isn't said.
    let default_untouched = l.labels.default == rustgis_map::LabelClass::default_for(&l.dataset);
    let removed = l.dataset.fields.remove(fi).name;
    for f in &mut l.dataset.features {
        if fi < f.attrs.len() {
            f.attrs.remove(fi);
        }
    }
    // Nothing may keep referring to the deleted field.
    // (Classes by an expression that read until now go the same way.)
    let unread = read_before && l.renderer.expression().is_some_and(|e| rustgis_data::query::parse_fields(e, &l.dataset).is_err());
    let mut single = None;
    if unread || l.renderer.field().is_some_and(|f| f.eq_ignore_ascii_case(&removed)) {
        l.renderer = rustgis_map::Renderer::Single(l.renderer.base_symbol());
        single = Some(l.name.clone());
    }
    // Nor the labels' expressions by feature (#2147): one that read the field is removed, and the
    // label draws as without it. Judged by the field it read, as the symbol's are. Said once for
    // the layer, naming each class's parts.
    let mut label_gone = Vec::new();
    let classes = std::iter::once(&mut l.labels.default).chain(l.labels.classes.iter_mut());
    for (c, read) in classes.zip(&label_read) {
        let by = &mut c.by;
        let parts = [(&mut by.text, "text"), (&mut by.size, "size"), (&mut by.color, "colour"), (&mut by.angle, "angle")];
        let mut gone = Vec::new();
        for ((text, part), read) in parts.into_iter().zip(read) {
            if *read {
                *text = None;
                gone.push(part);
            }
        }
        if !gone.is_empty() {
            label_gone.push((format!("{}'s {}", c.name, and_list(&gone)), gone.len()));
        }
    }
    let label_note = match label_gone.as_slice() {
        [] => None,
        [(one, n)] => {
            let (what, it) = if *n == 1 { ("expression", "it is") } else { ("expressions", "they are") };
            Some(format!("{}: its label class {one} {what} read the deleted field {removed}, so {it} removed", l.name))
        }
        many => {
            let each: Vec<&str> = many.iter().map(|(class, _)| class.as_str()).collect();
            Some(format!("{}: its label expressions read the deleted field {removed}, so they are removed: {}", l.name, each.join("; ")))
        }
    };
    // The layer's labels turn off if the Default class was on it, which starts again from the
    // layer's first name-like field (keeping its name, clause, visibility and the expressions left),
    // said; by a text expression that still reads, Default only loses the field. Another class on
    // it labels with Default's field instead. A class clause naming it no longer reads, and takes
    // no features, which go on to the next class: it is kept to edit, not dropped with its text
    // symbol.
    let mut restart_note = None;
    if l.labels.default.field.eq_ignore_ascii_case(&removed) {
        let by_text = l.labels.default.by.text.as_deref().is_some_and(|t| rustgis_data::query::parse_fields(t, &l.dataset).is_ok());
        if by_text {
            l.labels.default.field.clear();
        } else {
            let was_on = l.labels.enabled;
            l.labels.restart_default(&l.dataset);
            let d = &l.labels.default;
            let on = if d.field.is_empty() { "with no field and".to_string() } else { format!("on field {} with", d.field) };
            let off = if was_on { ", and the labels are turned off" } else { "" };
            restart_note = (was_on || !default_untouched).then(|| {
                format!(
                    "{}: its label class {} labelled the deleted field {removed}, so it starts again {on} the default text symbol{off}",
                    l.name, d.name
                )
            });
        }
    }
    l.labels.repoint_missing_fields(&l.dataset);
    // A definition query that reads the field was refused above; one that already didn't read is
    // kept as it is, so its text isn't lost.
    // Nor its data design: domains, subtypes, contingent values, and rules that used it.
    let d = &mut l.design;
    d.domains.retain(|x| !x.field.eq_ignore_ascii_case(&removed));
    if d.subtypes.as_ref().is_some_and(|st| st.field.eq_ignore_ascii_case(&removed)) {
        d.subtypes = None;
    }
    if let Some(st) = &mut d.subtypes {
        for sub in &mut st.subtypes {
            sub.defaults.retain(|(f, _)| !f.eq_ignore_ascii_case(&removed));
        }
    }
    d.contingent.retain(|c| !c.fields.iter().any(|f| f.eq_ignore_ascii_case(&removed)));
    let ds = rustgis_data::Dataset::new(l.dataset.name.clone(), l.dataset.geometry_type, l.dataset.fields.clone());
    // The rules that no longer read without the field are removed, by name, and said below.
    let mut dropped: Vec<String> = Vec::new();
    d.rules.retain(|r| {
        let reads = !r.field.eq_ignore_ascii_case(&removed)
            && match r.kind {
                rustgis_map::design::RuleKind::Calculation => Calculation::parse(&r.expression, &ds).is_ok(),
                rustgis_map::design::RuleKind::Constraint => rustgis_data::query::parse_fields(&r.expression, &ds).is_ok(),
            };
        if !reads {
            dropped.push(r.name.clone());
        }
        reads
    });
    let removed_rules = dropped.len();
    let rules_note = (!dropped.is_empty()).then(|| {
        dropped.sort();
        format!(
            "{}: deleting field {removed} removed {} attribute rule(s) that used it or no longer read: {}",
            l.dataset.name,
            removed_rules,
            dropped.join(", ")
        )
    });
    // Nor its diagram: the field's slice or bar goes, the range of sums a scaled size grows over is
    // measured again without it, and a diagram left with no field goes too. Said once.
    let mut diagram_note = None;
    if let Some(dg) = &mut l.diagram
        && dg.categories.iter().any(|c| c.field.eq_ignore_ascii_case(&removed))
    {
        dg.categories.retain(|c| !c.field.eq_ignore_ascii_case(&removed));
        if dg.categories.is_empty() {
            l.diagram = None;
            diagram_note = Some(format!("{}: its diagram drew only the deleted field {removed}, so it is removed", l.name));
        } else {
            if let rustgis_map::DiagramSize::Scaled { min_size, max_size, .. } = dg.size {
                let (min, max) = super::diagram::sum_range(dg, &l.dataset);
                dg.size = rustgis_map::DiagramSize::Scaled { min_size, max_size, min, max };
            }
            diagram_note = Some(format!("{}: its diagram drew the deleted field {removed}, so that slice or bar is removed", l.name));
        }
    }
    // Nor its extrusion (#2081): a height or base read from the field goes back to its constant,
    // said once for both. Nor its marker rotation, which goes: the markers stop turning.
    let mut extrusion_note = None;
    if let Some(x) = &mut l.extrusion {
        let mut what = Vec::new();
        for (read, name) in [(&mut x.field, "height"), (&mut x.base_field, "base")] {
            if read.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(&removed)) {
                *read = None;
                what.push(name);
            }
        }
        if !what.is_empty() {
            let what = what.join(" and ");
            extrusion_note =
                Some(format!("{}: its extrusion's {what} read the deleted field {removed}, so it uses its constant {what}", l.name));
        }
    }
    let mut rotation_note = None;
    if l.marker_rotation.as_ref().is_some_and(|r| r.field.eq_ignore_ascii_case(&removed)) {
        l.marker_rotation = None;
        rotation_note = Some(format!("{}: its markers turned by the deleted field {removed}, so they are no longer turned", l.name));
    }
    let mut size_note = None;
    if l.size_by.as_ref().is_some_and(|b| b.field.eq_ignore_ascii_case(&removed)) {
        l.size_by = None;
        size_note = Some(format!("{}: its markers were sized by the deleted field {removed}, so they keep their class sizes", l.name));
    }
    // Nor its expressions by feature: one that read the field is removed, and the symbol draws as
    // without it. Judged by the field it read, not by whether its text still parses: a field
    // named like a keyword (`null = 'x'`) would parse again as the keyword, and a name matching
    // another field but for case would read that one. Said once for all of them.
    let by = &mut l.symbol_by;
    let parts = [
        (&mut by.size, "size"),
        (&mut by.fill, "fill colour"),
        (&mut by.stroke, "outline colour"),
        (&mut by.stroke_width, "outline width"),
        (&mut by.angle, "angle"),
    ];
    let mut gone = Vec::new();
    for ((text, part), read) in parts.into_iter().zip(by_read) {
        if read {
            *text = None;
            gone.push(part);
        }
    }
    let by_note = match gone.as_slice() {
        [] => None,
        [part] => Some(format!("{}: its symbol's {part} expression read the deleted field {removed}, so it is removed", l.name)),
        many => {
            Some(format!("{}: its symbol's {} expressions read the deleted field {removed}, so they are removed", l.name, and_list(many)))
        }
    };
    for note in [extrusion_note, rotation_note, size_note, by_note, label_note, restart_note].into_iter().flatten() {
        s.notify(note, false);
    }
    if let Some(note) = diagram_note {
        s.notify(note, false);
    }
    if let Some(name) = single {
        s.notify(format!("{name}: its classes read the deleted field {removed}, so it is drawn with a single symbol"), false);
    }
    // Nor the terrain's water, when its levels came from the field (#1915): its lakes go back to
    // their shores' lowest heights, said.
    let layer_name = s.map().layer(id).map(|l| l.name.clone()).unwrap_or_default();
    if let Some(w) = s.map_mut().view.terrain.as_mut().and_then(|t| t.water.as_mut())
        && w.layer.eq_ignore_ascii_case(&layer_name)
        && w.level.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(&removed))
    {
        w.level = None;
        s.notify(
            format!("Water: its levels came from the deleted field {removed}, so its lakes lie at their shores' lowest heights"),
            false,
        );
    }
    // Nor a block's walls, when the depths of that water came from the field (#2317): the block's
    // depth for all of it if it has one, else no water in the walls, said.
    if let Some(t) = s.map_mut().view.terrain.as_mut()
        && t.water.as_ref().is_some_and(|w| w.layer.eq_ignore_ascii_case(&layer_name))
        && let Some(b) = t.block.as_mut()
        && let Some(bw) = b.water.as_mut()
        && bw.field.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(&removed))
    {
        let note = match bw.depth {
            Some(d) => {
                bw.field = None;
                format!("Block: its water's depths came from the deleted field {removed}, so its walls show all of it {d} m deep")
            }
            None => {
                b.water = None;
                format!("Block: its water's depths came from the deleted field {removed}, so its walls show no water")
            }
        };
        s.notify(note, false);
    }
    if let Some(note) = rules_note {
        s.notify(note, false);
    }
    Ok(json!({"rulesRemoved": removed_rules}))
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(parts: &[&str]) -> String {
    match parts.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Table writes use one admitted worker path, including the first copy of shared rows. The
/// handler runs on a session containing only the input table, never the caller's layers or secrets.
pub(crate) const TABLE_EDITS: &[(&str, &str, fn(&mut Session, &Json) -> CmdResult)] = &[
    ("edit.setValue", "Edit Attribute", set_value),
    ("edit.addFeature", "Add Row", add_feature),
    ("edit.addField", "Add Field", add_field),
    ("edit.deleteField", "Delete Field", delete_field),
    ("edit.convertField", "Convert Field", convert_field),
    ("edit.calculateField", "Calculate Field", calculate_field),
];

/// The registered command and progress label for a table write. Layer invocations keep their
/// existing path; the desktop uses this worker only when parameters name a standalone table.
pub fn table_edit_in_background(command: &str) -> Option<(&'static str, &'static str)> {
    TABLE_EDITS.iter().find(|(id, ..)| *id == command).map(|(id, name, _)| (*id, *name))
}

/// One table edit's input and completed result. Shared rows let landing detect concurrent edits
/// without scanning them. Only fields and rows are installed; unrelated table metadata is kept.
struct TableEdit {
    opened: u64,
    id: u32,
    input: Dataset,
    result: Result<(Dataset, Json, Vec<crate::Notice>), String>,
}

#[cfg(not(target_arch = "wasm32"))]
/// A table-write line recorded by a script worker for that same line to consume on landing.
pub(crate) struct ScriptTableEdit {
    command: String,
    params: Json,
    edit: TableEdit,
}

#[cfg(not(target_arch = "wasm32"))]
/// Records a table line's actual rows and notices on the worker, or invokes the live line with
/// those rows prepared. A missing or mismatched result is refused, never recomputed on the frame.
/// The caller still authorizes and invokes the original operation under the live script owner.
pub(crate) fn script_table(
    s: &mut Session,
    command: &str,
    p: &Json,
    prepared: bool,
    run: impl FnOnce(&mut Session) -> CmdResult,
) -> CmdResult {
    use super::gp::{Ahead, RanAhead};
    if prepared {
        let next = match &mut s.tools_ahead {
            Some(Ahead::Taking(ran, _)) => ran.pop_front(),
            _ => None,
        };
        let Some(RanAhead::Table(next)) = next else {
            return Err("the script's table input changed: run the script again".into());
        };
        let ScriptTableEdit { command: given, params, edit } = *next;
        if given != command || params != *p {
            return Err("the script's table command changed: run the script again".into());
        }
        return super::reads::taking(s, command, edit, run);
    }
    if !matches!(s.tools_ahead, Some(Ahead::Recording(_))) {
        return run(s);
    }
    rustgis_data::progress::begin(0);
    rustgis_data::progress::check().map_err(|e| e.to_string())?;
    let Target::Table(i) = target(s, p)? else { return run(s) };
    let table = s.project.tables.get(i).ok_or("no such table")?;
    let (opened, id, input, mark) = (s.opened, table.id, table.dataset.clone(), s.notice_mark());
    let answer = run(s).and_then(|answer| {
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        Ok(answer)
    });
    let result = answer.clone().and_then(|answer| {
        let output = s.project.table(id).ok_or("no such table")?.dataset.clone();
        Ok((output, answer, s.notices_since(mark).to_vec()))
    });
    if let Some(Ahead::Recording(ran)) = &mut s.tools_ahead {
        ran.push(RanAhead::Table(Box::new(ScriptTableEdit {
            command: command.into(),
            params: p.clone(),
            edit: TableEdit { opened, id, input, result },
        })));
    }
    answer
}

/// Prepares a pure table write after admission, with no caller state on the worker except the
/// table and editing flag. Cancel is checked between rows and before/after the handler. The
/// shared row storage's first copy and one expression evaluation cannot be interrupted.
pub(crate) fn table_ahead(s: &Session, command: &str, p: &Json) -> Result<Option<super::reads::ReadWork>, String> {
    let Target::Table(i) = target(s, p)? else {
        return if command == "edit.convertField" { layer_dates_ahead(s, p).map(Some) } else { Ok(None) };
    };
    let &(_, name, handler) = TABLE_EDITS.iter().find(|(id, ..)| *id == command).ok_or("not a table edit")?;
    let table = s.project.tables.get(i).ok_or("no such table")?.clone();
    let (opened, id, input, p, editing) = (s.opened, table.id, table.dataset.clone(), p.clone(), s.map().editing.settings.enabled);
    Ok(Some(super::reads::ahead(name, command.into(), move || {
        let result = (|| {
            rustgis_data::progress::begin(0);
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let mut project = crate::Project::default();
            project.tables.push(table);
            let mut copy = Session::new(project);
            copy.map_mut().editing.settings.enabled = editing;
            let answer = handler(&mut copy, &p)?;
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let output = copy.project.tables.into_iter().next().ok_or("no such table")?.dataset;
            Ok((output, answer, copy.notices))
        })();
        TableEdit { opened, id, input, result }
    })))
}

/// A layer conversion installs only its rows and fields, after checking its identity and schema.
struct LayerDates {
    opened: u64,
    id: u32,
    instance: u64,
    input: Dataset,
    result: Result<(Dataset, Json, Vec<crate::Notice>), String>,
}

/// Captures shared rows only; the worker uses an isolated table session. Layer schema
/// consumers are checked before capture and again at landing, without cloning edit archives.
fn layer_dates_ahead(s: &Session, p: &Json) -> Result<super::reads::ReadWork, String> {
    let id = s.layer_id(p)?;
    let layer = s.map().layer(id).ok_or("no layer")?;
    let fi = field_of(s, Target::Layer(id), ps(p, "field").ok_or("field is required")?)?;
    let ty = match ps(p, "type").unwrap_or("").to_ascii_lowercase().as_str() {
        "date" => FieldType::Date,
        "datetime" | "date-time" | "timestamp" => FieldType::DateTime,
        _ => return Err("type must be date or datetime".into()),
    };
    check_layer_dates(s, id, fi, ty)?;
    let (opened, instance, input) = (s.opened, layer.instance_id(), layer.dataset.clone());
    let (dataset, mut p) = (input.clone(), p.clone());
    Ok(super::reads::ahead("Convert Field", "edit.convertField".into(), move || {
        let result = (|| {
            rustgis_data::progress::begin(0);
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let mut copy = Session::new(crate::Project::default());
            let table = copy.project.put_table(dataset);
            p.as_object_mut().ok_or("parameters must be an object")?.remove("layer");
            p["table"] = json!(table);
            let answer = convert_field(&mut copy, &p)?;
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let output = copy.project.table(table).ok_or("no table")?.dataset.clone();
            Ok((output, answer, copy.notices))
        })();
        LayerDates { opened, id, instance, input, result }
    }))
}

/// Rejects stale worker output and rechecks current design constraints before installing one
/// conversion as one undo step. Changes to unrelated map properties remain in place.
fn land_layer_dates(s: &mut Session, p: &Json) -> Option<CmdResult> {
    let ahead = super::reads::take::<LayerDates>(s, "edit.convertField")?;
    Some((|| {
        let id = s.layer_id(p)?;
        let layer = s.map().layer(id).ok_or("no layer")?;
        if s.opened != ahead.opened
            || id != ahead.id
            || layer.instance_id() != ahead.instance
            || layer.dataset.fields != ahead.input.fields
            || !layer.dataset.features.shares(&ahead.input.features)
        {
            return Err("the layer changed while it was converted: run the conversion again".into());
        }
        let (output, answer, notices) = ahead.result?;
        let fi = field_of(s, Target::Layer(id), ps(p, "field").ok_or("field is required")?)?;
        let ty = output.fields.get(fi).ok_or("no field")?.ty;
        check_layer_dates(s, id, fi, ty)?;
        let layer = s.map_mut().layer_mut(id).ok_or("no layer")?;
        (layer.dataset.fields, layer.dataset.features) = (output.fields, output.features);
        // Each with its code, as a table's edit lands.
        for notice in notices {
            s.relay_notice(notice);
        }
        Ok(answer)
    })())
}

/// Installs only this command's prepared table edit, rejecting changed input instead of doing
/// the work on the frame. The ordinary command transaction supplies history and one undo step.
fn land_table_edit(s: &mut Session, p: &Json, command: &str) -> Option<CmdResult> {
    let ahead = super::reads::take::<TableEdit>(s, command)?;
    Some((|| {
        let Target::Table(i) = target(s, p)? else { return Err("the edit no longer names a table".into()) };
        let table = s.project.tables.get_mut(i).ok_or("no such table")?;
        if ahead.opened != s.opened
            || ahead.id != table.id
            || !ahead.input.features.shares(&table.dataset.features)
            || ahead.input.fields != table.dataset.fields
        {
            return Err("the table changed while it was edited: run the edit again".into());
        }
        let (output, answer, notices) = ahead.result?;
        (table.dataset.fields, table.dataset.features) = (output.fields, output.features);
        for notice in notices {
            s.relay_notice(notice);
        }
        Ok(answer)
    })())
}

/// Evaluates and writes a table on its worker, checking Cancel between rows. An expression's
/// evaluation on one row and the shared row storage's first copy cannot be interrupted;
/// cancellation never installs partial output.
fn calculate_table(input: &Dataset, p: &Json) -> Result<(Dataset, WriteLosses), String> {
    let fi = input.field_index(ps(p, "field").ok_or("field is required")?).ok_or("no field")?;
    let ty = input.fields.get(fi).map(|f| f.ty).ok_or("no field")?;
    let expr = ps(p, "expression").map(str::trim).filter(|e| !e.is_empty()).ok_or("expression is required")?;
    let calc = Calculation::parse(expr, input)?;
    let (out, losses) = evaluate(&calc, input, (0..input.features.len()).collect(), ty)?;
    let mut result = input.clone();
    rustgis_data::progress::begin(out.len());
    for (r, v) in out {
        rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        if let Some(c) = result.features.get_mut(r).and_then(|f| f.attrs.get_mut(fi)) {
            *c = v;
        }
    }
    Ok((result, losses))
}

/// Sets a field from an expression, on the selected rows or all of them (all of a standalone
/// table's). Desktop table calculations use the read worker with progress and Cancel; landing
/// refuses changed input instead of recalculating on the frame. Layer edits retain the existing
/// synchronous path so their editing rules and per-feature undo changes run together.
fn calculate_field(s: &mut Session, p: &Json) -> CmdResult {
    if let Some(result) = land_table_edit(s, p, "edit.calculateField") {
        return result;
    }
    let t = write_target(s, p)?;
    let field = ps(p, "field").ok_or("field is required")?;
    let fi = data(s, t)?.field_index(field).ok_or_else(|| format!("no field named '{field}'; use Add Field (edit.addField) first"))?;
    let expr = ps(p, "expression").map(str::trim).filter(|e| !e.is_empty()).ok_or("expression is required")?.to_string();
    let Target::Layer(id) = t else {
        editing_on(s)?;
        let (ty, field) = data(s, t)?.fields.get(fi).map(|f| (f.ty, f.name.clone())).ok_or("no field")?;
        let (result, losses) = calculate_table(data(s, t)?, p)?;
        let n = result.features.len();
        data_mut(s, t)?.features = result.features;
        calculation_notices(s, &field, ty, &losses);
        return Ok(json!({"rows": n}));
    };
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let ty = l.dataset.fields.get(fi).map(|f| f.ty).ok_or("no field")?;
    let calc = Calculation::parse(&expr, &l.dataset)?;
    let rows: Vec<usize> = if l.selection.is_empty() { (0..l.dataset.features.len()).collect() } else { l.selection.clone() };
    let n = rows.len();
    let (out, losses) = evaluate(&calc, &l.dataset, rows, ty)?;
    let mut changes = Vec::with_capacity(out.len());
    let original = l.dataset.features.clone();
    let archiving = l.design.archive.is_some();
    for (r, v) in out {
        if let Some(old) = original.get(r)
            && let Some(f) = l.dataset.features.get_mut(r)
        {
            // An archive records each edit's immediate preimage, including repeated rows.
            // Other checks can read the original rows already held by the transaction.
            let before = if archiving { Cow::Owned(f.clone()) } else { Cow::Borrowed(old) };
            if let Some(c) = f.attrs.get_mut(fi) {
                *c = v;
            }
            changes.push(super::design::Change { before: Some(before), row: Some(r) });
        }
    }
    super::design::apply_with(s, id, changes, false)?;
    let field = s.map().layer(id).and_then(|l| l.dataset.fields.get(fi)).map(|f| f.name.clone()).unwrap_or_default();
    calculation_notices(s, &field, ty, &losses);
    Ok(json!({"rows": n}))
}

/// `calc` on `rows` of `ds`, each result as a `ty` field holds it: the rows' new values, and what
/// writing them loses ([`WriteLosses`]).
fn evaluate(calc: &Calculation, ds: &Dataset, rows: Vec<usize>, ty: FieldType) -> Result<(Vec<(usize, Value)>, WriteLosses), String> {
    let mut out = Vec::with_capacity(rows.len());
    let mut losses = WriteLosses::default();
    rustgis_data::progress::begin(rows.len());
    for r in rows {
        rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        out.push((r, losses.write(r, calc.eval_or_fault(ds, r), ty)));
    }
    Ok((out, losses))
}

/// Says what Calculate Field's results lost in `field` ([`evaluate`]), even when a calculation
/// rule writes the field afterwards: the rule may read the cut value, and says what it cuts itself.
fn calculation_notices(s: &mut Session, field: &str, ty: FieldType, losses: &WriteLosses) {
    for (code, notice) in losses.notices(NoticeText::term("Calculate Field"), field, ty) {
        s.notify_loss(code, notice);
    }
}

/// Whether Calculate Field reads `expr` (the error is the expression language's).
pub fn check_calculation(expr: &str, ds: &rustgis_data::Dataset) -> Result<(), String> {
    Calculation::parse(expr, ds).map(|_| ())
}

/// One row of a Calculate Field preview: the value the field would hold, what writing it loses,
/// the kind the run's notices count, and the layer's check that would refuse the edit (#2837).
/// A calculation rule that writes the field overwrites the expression's result on the run, so the
/// value is the last such rule's result, as the field holds it, with what that write loses (#2884).
#[derive(Clone, Debug, PartialEq)]
pub struct CalculationPreview {
    pub row: usize,
    pub value: Value,
    pub lost: Option<PreviewLoss>,
    /// What the run would refuse the edit with, when the layer's checks refuse this row: its
    /// error, word for word, for the checks the preview runs. An edit whose area reads are
    /// ambiguous is refused before them, with that error instead, which the preview doesn't give.
    pub refused: Option<String>,
}

/// What Calculate Field would write with `expr` into field `field` of `rows` (rows past the end
/// left out), parsed once as [`check_calculation`] reads it: the error is the same. With a
/// layer's `design`, each row is run as an edit runs it (`apply_with`, in `design`) on a copy of
/// that row alone, its value written, which reads `@row_number` as the layer's row: the layer's
/// calculation rules first, in order, so a rule that writes `field` gives the value and what
/// writing it loses (the later of two rules wins; what Calculate's own result lost, which the run
/// says in its notices, isn't shown then), then each row says which of its domains, allowed
/// combinations and constraint rules would refuse it. The copy holds that row alone, with the
/// layer's fields and coordinate system: nothing else of the layer is copied. Not previewed: an
/// edit refused because an area it reads is ambiguous, which needs the whole edit's geometry, and
/// a rule that no longer reads, which the run skips and says.
pub fn preview_calculation(
    expr: &str,
    ds: &rustgis_data::Dataset,
    field: usize,
    rows: &[usize],
    design: Option<&rustgis_map::design::Design>,
) -> Result<Vec<CalculationPreview>, String> {
    use rustgis_map::design::RuleKind;
    let calc = Calculation::parse(expr, ds)?;
    let ty = ds.fields.get(field).map(|f| f.ty).ok_or("no field")?;
    // The dialog asks every frame, so the one-row copy and the rules are skipped unless they
    // change the answer: the layer has a check (a domain, an allowed combination or a constraint
    // rule, which any calculation rule's result may feed), or a calculation rule writes `field`.
    // A layer with only calculation rules on other fields gets the expression's value, as it
    // would from running them.
    let ruled = design.filter(|d| {
        !d.domains.is_empty()
            || !d.contingent.is_empty()
            || d.rules.iter().any(|r| match r.kind {
                RuleKind::Constraint => true,
                RuleKind::Calculation => ds.field_index(&r.field) == Some(field),
            })
    });
    // The layer's fields, which its rules are read against, and its constraint rules.
    let prepared = ruled.map(|d| {
        let metadata = rustgis_data::Dataset::new(ds.name.clone(), ds.geometry_type, ds.fields.clone());
        let constraints = super::design::constraint_rules(d, &metadata);
        (d, metadata, constraints)
    });
    // What the layer's rules leave in `field` of `row` with `value` written (the value and what
    // the last rule writing it lost, when one does), and what its checks would refuse the edit with.
    let ruled_row = |row: usize, value: &Value| {
        let (d, metadata, constraints) = prepared.as_ref()?;
        let before = ds.features.row(row)?.to_feature();
        // Shares the layer's rows (an `Arc`) until `one.features` is replaced on the next line, so
        // no row of the layer is copied: keep that replacement before any change to `one`.
        let mut one = ds.clone();
        one.features = vec![before.clone()].into();
        one.features.set_value(0, field, value.clone());
        let (cuts, _) = super::design::calculation_rules(d, &mut one, metadata, &[0], row);
        let written = cuts.iter().rfind(|(_, name, ..)| ds.field_index(name) == Some(field)).map(|(.., losses, _)| losses.previewed());
        let refusal = super::design::refusal(d, &one, constraints, 0, Some(&before), row);
        Some((written.map(|lost| (one.value(0, field).clone(), lost)), refusal))
    };
    Ok(rows
        .iter()
        .filter(|r| **r < ds.features.len())
        .map(|&row| {
            let Held { value, cut, other } = held(calc.eval_or_fault(ds, row), ty);
            let (written, refused) = ruled_row(row, &value).unwrap_or((None, None));
            // The error is why the value is null; a cut alone says what it took.
            let own = other.or(cut).and_then(Loss::previewed);
            // A rule's write replaces the expression's: its value, and what it lost.
            let (value, lost) = written.unwrap_or((value, own));
            CalculationPreview { row, value, lost, refused }
        })
        .collect())
}

/// A Calculate Field expression: the expression language, or, for text it doesn't read, the
/// earlier calculator's `a op b` (whose bare field names may hold spaces: `Pop Est * 2`).
pub(crate) enum Calculation {
    Expr(rustgis_data::query::Expr),
    Calc(Calc),
}

impl Calculation {
    /// Parses `expr`; when neither reads it, the error is the expression language's.
    pub(crate) fn parse(expr: &str, ds: &rustgis_data::Dataset) -> Result<Calculation, String> {
        // The language's length limit holds for the earlier calculator too, which both routes to it
        // below would otherwise skip: a rule or Calculate Field reads no longer text than a query.
        if expr.chars().count() > rustgis_data::query::MAX_LEN {
            return Err("the expression is too long".into());
        }
        // A field named like a literal was read as the field (`null + 1`), so what the earlier
        // calculator reads with such a field keeps its meaning (quoted text needn't).
        // A date field in arithmetic, which the language refuses, isn't the earlier calculator's
        // to do either.
        let date_field = |c: &Calc| {
            c.op.as_ref()?;
            [Some(&c.a), c.b.as_ref()].into_iter().flatten().find_map(|t| match t {
                Term::Field(i) => ds.fields.get(*i).filter(|f| f.ty.is_temporal()),
                _ => None,
            })
        };
        let named_like_a_literal: Vec<usize> = ["true", "false", "null"].iter().filter_map(|k| ds.field_index(k)).collect();
        if !named_like_a_literal.is_empty()
            && let Ok(c) = Calc::parse(expr, ds)
            && [Some(&c.a), c.b.as_ref()].into_iter().flatten().any(|t| matches!(t, Term::Field(i) if named_like_a_literal.contains(i)))
        {
            // The language would read the name as its literal, so it is refused here, as the
            // language refuses arithmetic on any other date field.
            if let Some(f) = date_field(&c) {
                return Err(format!("'{}' holds dates, and arithmetic on dates isn't supported yet", f.name));
            }
            return Ok(Calculation::Calc(c));
        }
        match rustgis_data::query::parse(expr, ds) {
            Ok(e) => Ok(Calculation::Expr(e)),
            Err(err) => match Calc::parse(expr, ds) {
                Ok(c) if date_field(&c).is_some() => Err(err),
                Ok(c) => Ok(Calculation::Calc(c)),
                Err(_) => Err(err),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn eval(&self, ds: &rustgis_data::Dataset, r: usize) -> Value {
        self.eval_or_fault(ds, r).0
    }

    /// Row `r`'s result, with the error that made it null, if one did.
    pub(crate) fn eval_or_fault(&self, ds: &rustgis_data::Dataset, r: usize) -> (Value, Option<rustgis_data::query::Fault>) {
        self.eval_or_fault_at(ds, r, 0)
    }

    /// [`Calculation::eval_or_fault`] for a row with `offset` rows before `ds`'s first in its
    /// layer ([`rustgis_data::query::value_or_fault_at`]). `Calc` reads no row number, so it takes
    /// no offset.
    pub(crate) fn eval_or_fault_at(
        &self,
        ds: &rustgis_data::Dataset,
        r: usize,
        offset: usize,
    ) -> (Value, Option<rustgis_data::query::Fault>) {
        match self {
            Calculation::Expr(e) => rustgis_data::query::value_or_fault_at(e, ds, r, &rustgis_data::query::Env::default(), offset),
            Calculation::Calc(c) => c.eval_or_fault(ds, r),
        }
    }
}

enum Term {
    Num(f64),
    Text(String),
    Field(usize),
}

/// The earlier calculator: `a` or `a op b`.
pub(crate) struct Calc {
    a: Term,
    op: Option<char>,
    b: Option<Term>,
}

impl Calc {
    pub(crate) fn parse(expr: &str, ds: &rustgis_data::Dataset) -> Result<Calc, String> {
        let (lhs, op, rhs) = split_op(expr.trim());
        let term = |t: &str| -> Result<Term, String> {
            let t = t.trim();
            if let Some(q) = t.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
                return Ok(Term::Text(q.to_string()));
            }
            // A number too large to represent isn't one (the expression language's error says so).
            if let Ok(n) = t.parse::<f64>()
                && n.is_finite()
            {
                return Ok(Term::Num(n));
            }
            let name = t.trim_matches('!').trim_matches('"');
            ds.field_index(name).map(Term::Field).ok_or_else(|| format!("no field named '{name}'"))
        };
        Ok(Calc { a: term(lhs)?, op, b: rhs.map(term).transpose()? })
    }

    #[cfg(test)]
    pub(crate) fn eval(&self, ds: &rustgis_data::Dataset, r: usize) -> Value {
        self.eval_or_fault(ds, r).0
    }

    /// Row `r`'s result, with the error that made it null: dividing by zero, or `-`, `*`, `/` on
    /// a value that isn't a number.
    pub(crate) fn eval_or_fault(&self, ds: &rustgis_data::Dataset, r: usize) -> (Value, Option<rustgis_data::query::Fault>) {
        use rustgis_data::query::Fault;
        let get = |t: &Term| -> Value {
            match t {
                Term::Num(n) => Value::Float(*n),
                Term::Text(s) => Value::Text(s.clone()),
                Term::Field(i) => ds.value(r, *i).clone(),
            }
        };
        // A number result: past the largest number a value holds, it is null, and says why.
        let number = |f: f64| {
            if f.is_finite() {
                (Value::Float(f), None)
            } else {
                (Value::Null, Some(if f.is_nan() { Fault::NotANumber } else { Fault::Overflow }))
            }
        };
        let va = get(&self.a);
        match (&self.b, self.op) {
            (Some(b), Some(op)) => {
                let vb = get(b);
                match (va.as_f64(), vb.as_f64(), op) {
                    (Some(x), Some(y), '+') => number(x + y),
                    (Some(x), Some(y), '-') => number(x - y),
                    (Some(x), Some(y), '*') => number(x * y),
                    (Some(_), Some(0.0), '/') => (Value::Null, Some(Fault::DivisionByZero)),
                    (Some(x), Some(y), '/') => number(x / y),
                    // A stored NaN or infinity isn't text to join.
                    (_, _, '+') if !va.is_null() && !vb.is_null() && !va.is_non_finite() && !vb.is_non_finite() => {
                        (Value::Text(format!("{va}{vb}")), None)
                    }
                    // Empty or `<null>` text is missing to `-`, `*`, `/` (`+` joins it, above).
                    _ if va.is_null() || vb.is_null() || (op != '+' && (va.asks_null() || vb.asks_null())) => (Value::Null, None),
                    // `+` joins text; what it couldn't join is a stored NaN or infinity.
                    (_, _, '+') => (Value::Null, Some(Fault::NotANumber)),
                    _ => (Value::Null, Fault::of_non_number(&va).or(Fault::of_non_number(&vb))),
                }
            }
            _ => (va, None),
        }
    }
}

/// Splits on the first top-level arithmetic operator outside quotes (not a leading minus).
fn split_op(e: &str) -> (&str, Option<char>, Option<&str>) {
    let mut q = false;
    for (i, c) in e.char_indices() {
        if c == '\'' {
            q = !q;
        }
        if !q && i > 0 && matches!(c, '+' | '-' | '*' | '/') {
            let before = e.get(..i).unwrap_or("").trim_end();
            if before.ends_with(['e', 'E']) && before.chars().rev().nth(1).is_some_and(|d| d.is_ascii_digit()) {
                continue;
            }
            return (e.get(..i).unwrap_or(""), Some(c), e.get(i + c.len_utf8()..));
        }
    }
    (e, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Dataset, Field};

    /// What a tally of one write says in a preview (`WriteLosses::previewed`) is what the preview
    /// says of the same result when it holds it (`held`, then `Loss::previewed`): the two readings
    /// of a loss don't drift apart.
    #[test]
    fn a_losses_tally_previews_as_the_result_it_counts() {
        use rustgis_data::query::Fault;
        let at = Value::parse("2026-11-23T10:30:00", FieldType::DateTime);
        let cases: Vec<(Value, Option<Fault>, FieldType, Option<PreviewLoss>)> = vec![
            (Value::Float(1e300), None, FieldType::Integer, Some(PreviewLoss::Range)),
            (Value::Float(2.5), None, FieldType::Integer, Some(PreviewLoss::Fraction)),
            (Value::Float(f64::NAN), None, FieldType::Double, Some(PreviewLoss::NotANumber)),
            (Value::Float(f64::INFINITY), None, FieldType::Double, Some(PreviewLoss::NotFinite)),
            (Value::Text("n/a".into()), None, FieldType::Integer, Some(PreviewLoss::Unheld)),
            (at, None, FieldType::Date, Some(PreviewLoss::Time)),
            (Value::Null, Some(Fault::DivisionByZero), FieldType::Double, Some(PreviewLoss::Error(Fault::DivisionByZero))),
            (Value::Float(2.0), None, FieldType::Integer, None),
            (Value::Null, None, FieldType::Double, None),
        ];
        for (value, fault, ty, expected) in cases {
            let result = (value.clone(), fault);
            let held = held(result.clone(), ty);
            let by_held = held.other.or(held.cut).and_then(Loss::previewed);
            let by_tally = {
                let mut losses = WriteLosses::default();
                losses.write(0, result, ty);
                losses.previewed()
            };
            assert_eq!(by_held, expected, "{value:?} into {ty:?}");
            assert_eq!(by_tally, by_held, "{value:?} into {ty:?}");
        }
    }

    /// A null an expression's error wrote says so, by kind with its count and first rows, on a
    /// layer and a table, from the language and from the earlier calculator; a clean run, and
    /// nulls read from the data, say nothing.
    #[test]
    fn calculation_errors_are_said() {
        for table in [false, true] {
            let mut s = Session::new(crate::Project::default());
            let fields = ["n", "Pop Est", "r"].map(|f| Field::new(f, FieldType::Double)).to_vec();
            let mut ds = Dataset::new("calc", GeometryType::None, fields);
            for n in [Value::Float(2.0), Value::Float(0.0), Value::Null, Value::Float(0.0), Value::Float(0.0), Value::Float(0.0)] {
                ds.push(None, vec![n, Value::Float(5.0), Value::Null]);
            }
            let target = if table {
                json!({"table": s.project.add_table(ds)})
            } else {
                let id = s.map_mut().add_dataset(ds);
                json!({"layer": id})
            };
            for (expression, said) in [
                ("1 / n", vec!["Calculate Field: 'r' is null in 4 rows because of a division by zero (rows 2, 4, 5…)"]),
                ("Pop Est / n", vec!["Calculate Field: 'r' is null in 4 rows because of a division by zero (rows 2, 4, 5…)"]),
                (
                    "if(n = 2, sqrt(0 - n), n * 2)",
                    vec!["Calculate Field: 'r' is null in 1 row because of the square root of a negative number (row 1)"],
                ),
                ("n * 2", vec![]),
            ] {
                let mut p = target.clone();
                p["field"] = json!("r");
                p["expression"] = json!(expression);
                let mark = s.notice_mark();
                s.run("edit.calculateField", &p).unwrap();
                let notices = s.notices_since(mark);
                assert_eq!(notices.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), said, "{expression}, table {table}");
                assert!(notices.iter().all(|n| n.loss() == Some(LossCode::DroppedCalculationResult)
                    && n.severity == rustgis_data::diagnostics::Severity::Warning
                    && !n.error));
            }
        }
        // The earlier calculator doesn't join a stored infinity as text either.
        let mut ds = Dataset::new("odd", GeometryType::None, vec![Field::new("Pop Est", FieldType::Double)]);
        ds.push(None, vec![Value::Float(f64::INFINITY)]);
        let calc = Calc::parse("Pop Est + 1", &ds).unwrap();
        assert_eq!(calc.eval_or_fault(&ds, 0), (Value::Null, Some(rustgis_data::query::Fault::NotANumber)));
        // Nor stores one it overflows to: it is null, too large.
        ds.push(None, vec![Value::Float(1e308)]);
        // A row an edit writes twice is counted once.
        // A later write's different loss is still counted.
        let mut losses = WriteLosses::once_per_row();
        losses.write(4, (Value::Float(1.0), None), FieldType::Double);
        for row in [4, 4] {
            losses.write(row, (Value::Null, Some(rustgis_data::query::Fault::DivisionByZero)), FieldType::Double);
        }
        let said: Vec<String> =
            losses.notices(NoticeText::term("Rule"), "n", FieldType::Double).iter().map(|(_, text)| text.english()).collect();
        assert_eq!(said, ["Rule: 'n' is null in 1 row because of a division by zero (row 5)"]);
        let calc = Calc::parse("'pop ' + Pop Est", &ds).unwrap();
        assert_eq!(calc.eval_or_fault(&ds, 0), (Value::Null, Some(rustgis_data::query::Fault::NotANumber)));
        let calc = Calc::parse("Pop Est * 10", &ds).unwrap();
        assert_eq!(calc.eval_or_fault(&ds, 1), (Value::Null, Some(rustgis_data::query::Fault::Overflow)));
    }

    /// Calculate Field never adds fields: layer and table callers get the same next step.
    #[test]
    fn missing_calculation_field_points_to_add_field() {
        for table in [false, true] {
            let mut s = Session::new(crate::Project::default());
            let mut ds = Dataset::new("calc", GeometryType::None, vec![Field::new("n", FieldType::Double)]);
            ds.push(None, vec![Value::Float(7.0)]);
            let target = if table {
                json!({"table": s.project.add_table(ds)})
            } else {
                let id = s.map_mut().add_dataset(ds);
                s.active_layer = Some(id);
                json!({"layer": id})
            };
            let mut p = target.clone();
            p["field"] = json!("T2");
            p["expression"] = json!("n * 2");
            let error = s.run("edit.calculateField", &p).unwrap_err();
            assert_eq!(error, "no field named 'T2'; use Add Field (edit.addField) first");
            let t = super::target(&s, &target).unwrap();
            assert!(data(&s, t).unwrap().field_index("T2").is_none());
            assert_eq!(data(&s, t).unwrap().value(0, 0), &Value::Float(7.0));
            let mut add = target;
            add["name"] = json!("T2");
            add["type"] = json!("double");
            s.run("edit.addField", &add).unwrap();
            s.run("edit.calculateField", &p).unwrap();
            let ds = data(&s, t).unwrap();
            assert_eq!(ds.value(0, ds.field_index("T2").unwrap()), &Value::Float(14.0));
        }
    }

    /// Not a number is null in a whole-number field, said as such, not past its range; a number
    /// past the range is null where it's said, and at the range's end elsewhere, as before.
    #[test]
    fn coerce_takes_not_a_number_as_null() {
        assert_eq!(coerce_cut(Value::Float(f64::NAN), FieldType::Integer), (Value::Null, Cut::NotANumber));
        assert_eq!(coerce(Value::Float(f64::NAN), FieldType::Integer), Value::Null);
        assert_eq!(coerce_cut(Value::Float(f64::INFINITY), FieldType::Integer), (Value::Null, Cut::NotFinite));
        assert_eq!(coerce(Value::Float(f64::INFINITY), FieldType::Integer), Value::Null);
        assert_eq!(coerce(Value::Float(-1e30), FieldType::Integer), Value::Int(i64::MIN));
        assert_eq!(coerce(Value::Float(0.1 + 0.2), FieldType::Double), Value::Float(0.1 + 0.2));
        // Text past the range, whole or decimal: null where it's said, at the range's end elsewhere.
        let text = |t: &str| Value::Text(t.into());
        assert_eq!(coerce_cut(text("-9223372036854775809"), FieldType::Integer), (Value::Null, Cut::Range));
        assert_eq!(coerce(text("-9223372036854775809"), FieldType::Integer), Value::Int(i64::MIN));
        assert_eq!(coerce(text("9223372036854775808"), FieldType::Integer), Value::Int(i64::MAX));
        assert_eq!(coerce(text("9223372036854775808.0"), FieldType::Integer), Value::Int(i64::MAX));
        // One notice per kind, singular or plural.
        let mut cuts = Cuts::default();
        assert!(cuts.notices("Calculate Field", "n").is_empty());
        for cut in [Cut::None, Cut::Fraction, Cut::Range, Cut::NotANumber, Cut::NotANumber] {
            cuts.add(cut);
        }
        assert_eq!(
            cuts.notices("Calculate Field", "n"),
            [
                "Calculate Field: 'n' holds whole numbers, so 1 result lost its fraction",
                "Calculate Field: 'n' is null for 1 result past the 64-bit integer range",
                "Calculate Field: 'n' is null for 2 results that aren't numbers",
            ]
        );
        cuts = Cuts::default();
        cuts.add(Cut::NotANumber);
        assert_eq!(cuts.notices("Rule", "n"), ["Rule: 'n' is null for 1 result that isn't a number"]);
        // An infinity is said as not finite, not as not a number; NaN stays not a number.
        assert_eq!(coerce_cut(Value::Float(f64::INFINITY), FieldType::Double), (Value::Null, Cut::NotFinite));
        assert_eq!(coerce_cut(text("-inf"), FieldType::Double), (Value::Null, Cut::NotFinite));
        assert_eq!(coerce_cut(text("inf"), FieldType::Integer), (Value::Null, Cut::NotFinite));
        assert_eq!(coerce_cut(Value::Float(f64::NAN), FieldType::Double), (Value::Null, Cut::NotANumber));
        assert_eq!(coerce_cut(text("NaN"), FieldType::Integer), (Value::Null, Cut::NotANumber));
        cuts = Cuts::default();
        cuts.add(Cut::NotFinite);
        cuts.add(Cut::NotFinite);
        assert_eq!(cuts.notices("Calculate Field", "d"), ["Calculate Field: 'd' is null for 2 results that aren't finite numbers"]);
    }

    /// Every form the earlier calculator read gives the same stored values through the
    /// expression language, whatever the field's type (but for the two forms whose quotes now
    /// read as the language's: `'it''s'` and `'a' || 'b'`; and whole-number arithmetic no longer
    /// writes `-0` into a text field).
    #[test]
    fn calculations_the_earlier_calculator_read_are_unchanged() {
        let mut ds = Dataset::new(
            "t",
            GeometryType::None,
            vec![
                Field::new("name", FieldType::Text),
                Field::new("pop", FieldType::Integer),
                Field::new("area", FieldType::Double),
                Field::new("Pop Est", FieldType::Integer),
                Field::new("flag", FieldType::Boolean),
                Field::new("code", FieldType::Text),
            ],
        );
        for (n, p, a, e, f, c) in [
            (
                Value::Text("Paris".into()),
                Value::Int(2_100_000),
                Value::Float(105.4),
                Value::Int(7),
                Value::Bool(true),
                Value::Text("12".into()),
            ),
            (Value::Text("".into()), Value::Int(-3), Value::Float(0.0), Value::Null, Value::Bool(false), Value::Text("x".into())),
            (Value::Null, Value::Null, Value::Null, Value::Int(0), Value::Null, Value::Null),
        ] {
            ds.push(None, vec![n, p, a, e, f, c]);
        }
        let terms = [
            "pop",
            "!pop!",
            "\"pop\"",
            "area",
            "name",
            "code",
            "flag",
            "Pop Est",
            "!Pop Est!",
            "5",
            "2.5",
            "-4",
            "0",
            "1e3",
            "'km'",
            "'7'",
            "''",
        ];
        let mut forms: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
        for a in terms {
            for op in ['+', '-', '*', '/'] {
                for b in terms {
                    forms.push(format!("{a} {op} {b}"));
                    forms.push(format!("{a}{op}{b}"));
                }
            }
        }
        let mut checked = 0;
        for form in &forms {
            let Ok(old) = Calc::parse(form, &ds) else { continue };
            let new = Calculation::parse(form, &ds).unwrap_or_else(|e| panic!("{form}: {e}"));
            for ty in [FieldType::Integer, FieldType::Double, FieldType::Text, FieldType::Boolean] {
                for r in 0..ds.features.len() {
                    // Whole-number arithmetic no longer writes a negative zero.
                    let zero = |v: Value| if v == Value::Text("-0".into()) { Value::Text("0".into()) } else { v };
                    assert_eq!(zero(coerce(new.eval(&ds, r), ty)), zero(coerce(old.eval(&ds, r), ty)), "{form} as {ty:?}, row {r}");
                }
            }
            checked += 1;
        }
        assert!(checked > 1000, "{checked}");
        // The two forms that read differently now.
        let v = |e: &str| Calculation::parse(e, &ds).map(|c| c.eval(&ds, 0)).unwrap_or_default();
        assert_eq!(v("'it''s'"), Value::Text("it's".into()));
        assert_eq!(v("'a' || 'b'"), Value::Text("ab".into()));
        // What the earlier calculator couldn't do.
        assert_eq!(v("round(area * 2) || ' ' || upper(name)"), Value::Text("211 PARIS".into()));
        assert!(Calculation::parse("nosuch * 2", &ds).is_err_and(|e| e == "no field named 'nosuch'"));
        // A number too large to represent is refused by both.
        assert!(Calculation::parse("1e999", &ds).is_err_and(|e| e.contains("too large")), "1e999");
        // A field named like a literal is still the field.
        let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("null", FieldType::Integer)]);
        ds.push(None, vec![Value::Int(4)]);
        let v = |e: &str| Calculation::parse(e, &ds).map(|c| c.eval(&ds, 0)).unwrap_or_default();
        assert_eq!(v("null + 1"), Value::Float(5.0));
        // In what only the expression language reads, the field takes quotes.
        assert_eq!(v("round(\"null\" / 3, 1)"), Value::Float(1.3));
        // Text that doesn't read the field means what it does elsewhere.
        assert_eq!(v("'it''s'"), Value::Text("it's".into()));
        assert_eq!(v("'a' || 'b'"), Value::Text("ab".into()));
    }

    /// Calculate Field's preview (#2817) is what the run writes in each row, as the field holds
    /// it, with what writing it loses; the expression's error is the check's.
    #[test]
    fn a_calculation_preview_is_what_the_run_writes() {
        let fields = vec![
            Field::new("n", FieldType::Double),
            Field::new("whole", FieldType::Integer),
            Field::new("name", FieldType::Text),
            Field::new("day", FieldType::Date),
            Field::new("at", FieldType::DateTime),
        ];
        let mut ds = Dataset::new("t", GeometryType::None, fields);
        for (n, name) in [(2.5, "a"), (1e300, "b"), (-1.5, "c"), (4.0, "d")] {
            let at = Value::parse("2026-11-23T10:30:00", FieldType::DateTime);
            ds.push(None, vec![Value::Float(n), Value::Int(0), Value::Text(name.into()), Value::Null, at]);
        }
        let (n, whole, day) = (0, 1, 3);
        let preview = |e: &str, field: usize, rows: &[usize]| -> Vec<(usize, Value, Option<PreviewLoss>)> {
            let previewed = preview_calculation(e, &ds, field, rows, None).unwrap_or_else(|e| panic!("{e}"));
            previewed.into_iter().map(|p| (p.row, p.value, p.lost)).collect()
        };
        // The rows asked for, in their order; a clean result loses nothing.
        assert_eq!(preview("n * 2", n, &[0, 3]), vec![(0, Value::Float(5.0), None), (3, Value::Float(8.0), None)]);
        // As the field holds it: a fraction into whole numbers, a number past their range.
        assert_eq!(
            preview("n", whole, &[0, 2]),
            vec![(0, Value::Int(2), Some(PreviewLoss::Fraction)), (2, Value::Int(-1), Some(PreviewLoss::Fraction))]
        );
        assert_eq!(preview("n", whole, &[1]), vec![(1, Value::Null, Some(PreviewLoss::Range))]);
        // Text that isn't a number, an expression's error, a date-time into a date.
        assert_eq!(preview("name", whole, &[0]), vec![(0, Value::Null, Some(PreviewLoss::Unheld))]);
        assert_eq!(preview("n / 0", n, &[0]), vec![(0, Value::Null, Some(PreviewLoss::Error(rustgis_data::query::Fault::DivisionByZero)))]);
        let at = preview("at", day, &[0]);
        assert_eq!((at[0].0, at[0].2), (0, Some(PreviewLoss::Time)));
        assert!(matches!(at[0].1, Value::Date(_)), "{:?}", at[0].1);
        // A null asked for loses nothing.
        assert_eq!(preview("null", n, &[0]), vec![(0, Value::Null, None)]);
        // No rows, or rows past the end: nothing previewed.
        assert_eq!(preview("n", n, &[]), vec![]);
        assert_eq!(preview("n", n, &[3, 4, 99]), vec![(3, Value::Float(4.0), None)]);
        // An expression that doesn't read: the check's error.
        assert_eq!(preview_calculation("nosuch * 2", &ds, n, &[0], None), Err(check_calculation("nosuch * 2", &ds).unwrap_err()));
        assert!(preview_calculation("n", &ds, 9, &[0], None).is_err(), "no such field");
    }

    /// The run's notices still count each loss as before the preview shared their reading: a
    /// fraction, a value not read, an error, once each.
    #[test]
    fn calculate_field_notices_count_what_the_preview_reads() {
        let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Double), Field::new("name", FieldType::Text)]);
        for (n, name) in [(2.5, "a"), (0.0, "b"), (3.0, "7")] {
            ds.push(None, vec![Value::Float(n), Value::Text(name.into())]);
        }
        let mut losses = WriteLosses::default();
        let calc = Calculation::parse("if(n = 0, 1 / n, n)", &ds).unwrap();
        let written: Vec<Value> = (0..3).map(|r| losses.write(r, calc.eval_or_fault(&ds, r), FieldType::Integer)).collect();
        assert_eq!(written, vec![Value::Int(2), Value::Null, Value::Int(3)]);
        let calc = Calculation::parse("name", &ds).unwrap();
        assert_eq!(losses.write(0, calc.eval_or_fault(&ds, 0), FieldType::Integer), Value::Null);
        let notices: Vec<String> =
            losses.notices(NoticeText::term("Calculate Field"), "n", FieldType::Integer).into_iter().map(|(_, t)| t.english()).collect();
        assert_eq!(notices.len(), 3, "{notices:?}");
        assert!(notices[0].contains("1 result lost its fraction"), "{notices:?}");
        assert!(notices[1].contains("is null in 1 row, whose result isn't"), "{notices:?}");
        assert!(notices[2].contains("because of a division by zero (row 2)"), "{notices:?}");
    }

    /// A calculation past the expression language's length limit is refused, however the earlier
    /// calculator would read it: as quoted text, or with a field named like a literal (#2783).
    #[test]
    fn a_calculation_longer_than_the_limit_is_refused() {
        use rustgis_data::query::MAX_LEN;
        let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("null", FieldType::Integer)]);
        ds.push(None, vec![Value::Int(4)]);
        // Counted in characters, as the language counts them: `é` is two bytes.
        for (prefix, at) in [("", "quoted"), ("null + ", "joined to a field named null")] {
            let expr = |chars: usize| format!("{prefix}'{}'", "é".repeat(chars - prefix.len() - 2));
            assert!(Calculation::parse(&expr(MAX_LEN), &ds).is_ok(), "{at}, at the limit");
            let refused = Calculation::parse(&expr(MAX_LEN + 1), &ds).err();
            assert_eq!(refused.as_deref(), Some("the expression is too long"), "{at}, one past it");
        }
    }
}

#[cfg(test)]
mod point_tests {
    use super::*;

    /// A caller that handles a command error must not keep a point refused by its surface.
    #[test]
    fn coordinate_point_surface_refusal_does_not_append_in_a_shared_call() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let mut s = Session::new(crate::Project::default());
        s.run("edit.enable", &json!({"on":true})).unwrap();
        s.run("layer.template", &json!({"template":"points"})).unwrap();
        let id = s.active_layer.unwrap();
        s.run("edit.addPoint", &json!({"x":1,"y":1})).unwrap();
        let kept = s.map().layer(id).unwrap().dataset.clone();
        let selection = s.map().layer(id).unwrap().selection.clone();
        let counts = s.edit_counts.clone();
        let raster = Raster::new(
            "south",
            1,
            1,
            vec![vec![12.0]],
            GeoTransform { origin_x: -1.0, origin_y: 1.0, pixel_width: 2.0, pixel_height: -2.0 },
            Crs::Epsg(3031),
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        s.map_mut().add_raster("south", rustgis_map::RasterLayer::from_path(raster, "south.tif"));
        s.map_mut().editing.settings.elevation = rustgis_map::design::Elevation::Surface("south".into());
        s.active_layer = Some(id);
        let error = add_point(&mut s, &json!({"x":0,"y":90})).unwrap_err();
        assert!(error.contains("surface"), "{error}");
        assert_eq!(s.map().layer(id).unwrap().dataset, kept, "a caught refusal must not retain the appended row");
        assert_eq!(s.map().layer(id).unwrap().selection, selection);
        assert_eq!(s.edit_counts, counts);
        s.map_mut().editing.settings.elevation = rustgis_map::design::Elevation::Constant(5.0);
        s.map_mut().layer_mut(id).unwrap().design.rules.push(rustgis_map::design::AttributeRule {
            name: "Reject".into(),
            kind: rustgis_map::design::RuleKind::Constraint,
            field: String::new(),
            expression: "1 = 0".into(),
        });
        assert!(add_point(&mut s, &json!({"x":2,"y":2})).unwrap_err().contains("Reject"));
        assert_eq!(s.map().layer(id).unwrap().dataset, kept, "a late refusal also removes the added z column from older rows");
        assert_eq!(s.map().layer(id).unwrap().selection, selection);
        assert_eq!(s.edit_counts, counts);
    }

    #[test]
    fn coordinate_points_share_a_layer_and_undo_as_single_edits() {
        let mut s = Session::new(crate::Project::default());
        s.run("edit.enable", &json!({"on":true})).unwrap();
        let mark = s.notice_mark();
        let first = s.run("edit.addPoint", &json!({"x":14.3,"y":-41.2,"name":"Survey"})).unwrap();
        let id = first["layer"].as_u64().unwrap() as u32;
        let l = s.map().layer(id).unwrap();
        assert_eq!(l.dataset.features.len(), 1);
        assert_eq!(l.dataset.value(0, 0), &Value::Text("Survey".into()));
        assert_eq!(l.dataset.features.geometry(0), Some(&Geometry::Point(rustgis_geom::Coord::new(14.3, -41.2))));
        assert!(s.map().editing.settings.enabled);
        assert_eq!(
            s.notices_since(mark).iter().filter(|n| n.text == "Created a Points layer because no loaded point layer was selected").count(),
            1
        );
        s.run("edit.addPoint", &json!({"x":0,"y":0})).unwrap();
        assert_eq!(s.map().layers.len(), 1);
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 2);
        assert_eq!(
            s.notices_since(mark).iter().filter(|n| n.text == "Created a Points layer because no loaded point layer was selected").count(),
            1
        );
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
        s.run("edit.undo", &json!({})).unwrap();
        assert!(s.map().layers.is_empty());
        s.run("edit.redo", &json!({})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
    }

    #[test]
    fn coordinate_point_refusals_leave_no_layer_and_projected_input_is_converted() {
        let mut s = Session::new(crate::Project::default());
        s.run("edit.enable", &json!({"on":true})).unwrap();
        for p in [
            json!({"x":"bad","y":0}),
            json!({"x":0,"y":91}),
            json!({"x":0,"y":0,"crs":"EPSG:0"}),
            json!({"x":0,"y":0,"name":1}),
            json!({"x":0,"y":0,"layer":true}),
        ] {
            assert!(s.run("edit.addPoint", &p).is_err(), "{p}");
            assert!(s.map().layers.is_empty());
        }
        let at = rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(7.44, 46.95));
        s.run("edit.addPoint", &json!({"x":at.x,"y":at.y,"crs":"EPSG:3857"})).unwrap();
        let l = s.map().layers.first().unwrap();
        let Some(Geometry::Point(at)) = l.dataset.features.geometry(0) else { panic!("point expected") };
        assert!((at.x - 7.44).abs() < 1e-8);
        assert!((at.y - 46.95).abs() < 1e-8);
    }

    #[test]
    fn coordinate_points_respect_editing_and_declare_the_surface_for_a_new_layer() {
        use super::super::InputSource;
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let mut s = Session::new(crate::Project::default());
        s.run("edit.enable", &json!({"on":false})).unwrap();
        assert!(s.run("edit.addPoint", &json!({"x":0,"y":0})).unwrap_err().contains("editing is turned off"));
        assert!(s.map().layers.is_empty());
        let raster = Raster::new(
            "dem",
            1,
            1,
            vec![vec![12.0]],
            GeoTransform { origin_x: -1.0, origin_y: 1.0, pixel_width: 2.0, pixel_height: -2.0 },
            Crs::LonLat,
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        let surface = s.map_mut().add_raster("dem", rustgis_map::RasterLayer::from_path(raster, "dem.tif"));
        s.map_mut().editing.settings.elevation = rustgis_map::design::Elevation::Surface("dem".into());
        s.active_layer = None;
        let inputs = point_inputs(&s.input_context(), &json!({"x":0,"y":0})).unwrap();
        assert_eq!(inputs.layers, vec![(s.map().instance_id(), surface)]);
        assert!(inputs.uses_default);
        for crs in ["map", "dem"] {
            let combined = point_inputs(&s.input_context(), &json!({"x":0,"y":0,"crs":crs})).unwrap();
            assert_eq!(
                combined.state.pointer("/feature/surface"),
                Some(&json!("dem")),
                "coordinate inputs retain the feature surface binding: {crs}"
            );
        }
        s.run("edit.enable", &json!({"on":true})).unwrap();
        s.run("edit.addPoint", &json!({"x":0,"y":0})).unwrap();
        let id = s.active_layer.unwrap();
        let held = s.map().layer(id).unwrap().dataset.clone();
        s.run("edit.enable", &json!({"on":false})).unwrap();
        assert!(s.run("edit.addPoint", &json!({"x":1,"y":1})).unwrap_err().contains("editing is turned off"));
        assert_eq!(s.map().layer(id).unwrap().dataset, held);
    }

    #[test]
    fn coordinate_points_inherit_project_storage_and_reopen() {
        use rustgis_data::raster::Crs;
        let mut s = Session::new(crate::Project::default());
        s.run("project.coordinateSystem", &json!({"crs":"EPSG:3857"})).unwrap();
        s.run("edit.addPoint", &json!({"x":14.3,"y":-41.2,"name":"Survey"})).unwrap();
        let expected = Geometry::Point(rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(14.3, -41.2)));
        let layer = s.map().layers.first().unwrap();
        assert_eq!(layer.dataset.coord_crs(), Crs::WebMercator);
        assert_eq!(layer.dataset.features.geometry(0), Some(&expected));
        let (text, _) = s.project.saved_text(None).unwrap();
        let reopened = crate::Project::from_json(&text).unwrap();
        let layer = reopened.maps.first().unwrap().layers.first().unwrap();
        assert_eq!(layer.dataset.coord_crs(), Crs::WebMercator);
        assert_eq!(layer.dataset.features.geometry(0), Some(&expected));
        assert_eq!(layer.dataset.value(0, 0), &Value::Text("Survey".into()));
    }
}
