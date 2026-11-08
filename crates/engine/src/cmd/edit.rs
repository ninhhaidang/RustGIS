//! Edit tab: create and delete features, edit attributes and fields.

use rustgis_data::{FieldType, Geometry, GeometryType, Value};
use serde_json::{Value as Json, json};

use super::{need_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("edit.calculateGeometry", "Calculate Geometry", "Attribute Table", crate::geometry_calc::run).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::design::need_editable)
            .params(r#"{"layer"?, "field": string, "create": bool, "overwrite": bool, "property": string, "unit": string, "epsg"?: number, "scope": "all"|"selected"}"#),
        C::new("edit.addFeature", "Create Feature", "Edit › Features", add_feature).inputs(super::design::new_feature_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::design::need_editable)
            .params(r#"{"layer"?, "geometry": GeoJSON geometry (lon/lat), "attributes"?: {field: value}, "template"?: feature template}"#),
        C::new("edit.delete", "Delete", "Edit › Features", delete).roles(&[]).effects(crate::policy::Effects::NONE).key("Delete").when(super::design::need_editable_selection),
        C::new("edit.setValue", "Edit Attribute", "Attribute Table", set_value).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::design::need_editable)
            .params(r#"{"layer"?, "row": number, "field": string, "value": any}"#),
        C::new("edit.addField", "Add Field", "Data › Data Design › Fields", add_field).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layer)
            .params(r#"{"layer"?, "name": string, "type": "long"|"double"|"text"|"boolean"}"#),
        C::new("edit.deleteField", "Delete Field", "Data › Data Design › Fields", delete_field).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layer)
            .params(r#"{"layer"?, "field": string}"#),
        C::new("edit.calculateField", "Calculate Field", "Attribute Table", calculate_field).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::design::need_editable)
            .params(r#"{"layer"?, "field": string, "expression": "an expression: fields, 'text', numbers, + - * / % ^ ||, functions (see the Expressions page)"}"#),
    ]
}

pub(crate) fn add_feature(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let g = geometry_param(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let want = l.geometry_type();
    let ok = want == g.kind() || want == GeometryType::None;
    if !ok {
        return Err(format!("{} can't go in a {} layer", g.kind().label(), want.label()));
    }
    validate(&g)?;
    // A feature template's defaults, then the given attributes.
    let template = match ps(p, "template") {
        Some(t) => {
            Some(l.design.templates.iter().find(|x| x.name.eq_ignore_ascii_case(t)).cloned().ok_or_else(|| format!("no template '{t}'"))?)
        }
        None => None,
    };
    let attrs: Vec<Value> = l
        .dataset
        .fields
        .iter()
        .map(|f| {
            let given = p.get("attributes").and_then(|a| a.get(&f.name)).map(|v| coerce(Value::from_json(v), f.ty));
            let default = template
                .as_ref()
                .and_then(|t| t.defaults.iter().find(|(k, _)| k.eq_ignore_ascii_case(&f.name)))
                .map(|(_, v)| coerce(v.clone(), f.ty));
            given.or(default).unwrap_or(Value::Null)
        })
        .collect();
    l.dataset.push(Some(g), attrs);
    l.touch_geometry();
    let row = l.dataset.features.len() - 1;
    l.selection = vec![row];
    super::design::apply(s, id, vec![super::design::Change { before: None, row: Some(row) }])?;
    Ok(json!({"row": row}))
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

/// `v` as a value of a `ty` field. A number goes into a number field as it is, not through the
/// text it shows (which rounds a decimal to six places); a decimal, or text written as one, into a
/// whole-number field loses its fraction (toward zero); a whole number past 2^53 into a decimal
/// field is the nearest decimal. Not a number is null. Other values go through their text (a
/// decimal into a text field as it's shown, to six places), and what the field can't hold is
/// null. A number past the 64-bit range is held at the range's end, as it was; [`coerce_cut`], for
/// callers that say what was cut, makes it null instead.
pub(crate) fn coerce(v: Value, ty: FieldType) -> Value {
    match convert(v, ty) {
        (_, Cut::Range, Some(f)) => Value::Int(f as i64),
        (v, _, _) => v,
    }
}

/// What [`coerce_cut`] took from a number to put it in a whole-number field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cut {
    /// Nothing.
    None,
    /// Its fraction: 2.7 is 2, −2.7 is −2.
    Fraction,
    /// All of it: a number past the 64-bit range is null.
    Range,
    /// All of it: not a number (NaN) is null.
    NotANumber,
}

/// How many of a field's results [`coerce_cut`] cut, of each kind, for one notice per kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cuts {
    fractions: usize,
    past: usize,
    not_numbers: usize,
}

impl Cuts {
    pub(crate) fn add(&mut self, cut: Cut) {
        match cut {
            Cut::None => {}
            Cut::Fraction => self.fractions += 1,
            Cut::Range => self.past += 1,
            Cut::NotANumber => self.not_numbers += 1,
        }
    }

    /// The notices for what writing the whole-number field `field` cut, each starting with `by`
    /// (the command or rule that wrote it). Empty when nothing was cut.
    pub(crate) fn notices(&self, by: &str, field: &str) -> Vec<String> {
        let results = |n: usize| if n == 1 { "1 result".to_string() } else { format!("{n} results") };
        let mut out = Vec::new();
        if self.fractions > 0 {
            let their = if self.fractions == 1 { "its" } else { "their" };
            out.push(format!("{by}: '{field}' holds whole numbers, so {} lost {their} fraction", results(self.fractions)));
        }
        if self.past > 0 {
            out.push(format!("{by}: '{field}' is null for {} past the 64-bit integer range", results(self.past)));
        }
        if self.not_numbers > 0 {
            let are = if self.not_numbers == 1 { "isn't a number" } else { "aren't numbers" };
            out.push(format!("{by}: '{field}' is null for {} that {are}", results(self.not_numbers)));
        }
        out
    }
}

/// [`coerce`], with a number past the 64-bit range null, and what it took from a number to put it
/// in a whole-number field.
pub(crate) fn coerce_cut(v: Value, ty: FieldType) -> (Value, Cut) {
    let (v, cut, _) = convert(v, ty);
    (v, cut)
}

/// [`coerce_cut`], and for a number past the range the decimal it was read as.
fn convert(v: Value, ty: FieldType) -> (Value, Cut, Option<f64>) {
    match (v, ty) {
        (Value::Null, _) => (Value::Null, Cut::None, None),
        (Value::Int(i), FieldType::Integer) => (Value::Int(i), Cut::None, None),
        (Value::Float(f), FieldType::Integer) => whole(f),
        (Value::Float(f), FieldType::Double) => (Value::Float(f), Cut::None, None),
        (Value::Int(i), FieldType::Double) => (Value::Float(i as f64), Cut::None, None),
        // Text written as a whole number, or as a decimal without an exponent, is read from its
        // digits; any other number written as text, as a decimal.
        (Value::Text(t), FieldType::Integer) => match t.trim().parse::<i64>() {
            Ok(i) => (Value::Int(i), Cut::None, None),
            Err(_) => match (decimal_text(t.trim()), t.trim().parse::<f64>()) {
                (Some((v, cut)), Ok(f)) => (v, cut, Some(f)),
                // A word the decimal reader takes ("inf", "NaN") isn't a number written as text:
                // null, and said so, as not a number.
                (None, Ok(f)) if f.is_finite() || t.bytes().any(|b| b.is_ascii_digit()) => whole(f),
                (None, Ok(_)) => (Value::Null, Cut::NotANumber, None),
                _ => (Value::parse(&t, ty), Cut::None, None),
            },
        },
        (Value::Text(t), _) => (Value::parse(&t, ty), Cut::None, None),
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

fn field_index(s: &Session, id: u32, name: &str) -> Result<usize, String> {
    s.map().layer(id).and_then(|l| l.dataset.field_index(name)).ok_or_else(|| format!("no field named '{name}'"))
}

fn set_value(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let row = p.get("row").and_then(Json::as_u64).ok_or("row is required")? as usize;
    let fi = field_index(s, id, ps(p, "field").ok_or("field is required")?)?;
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let ty = l.dataset.fields.get(fi).map(|f| f.ty).ok_or("no field")?;
    let raw = p.get("value").cloned().unwrap_or(Json::Null);
    let v = match raw {
        Json::String(t) => Value::parse(&t, ty),
        other => coerce(Value::from_json(&other), ty),
    };
    let f = l.dataset.features.get_mut(row).ok_or("no such row")?;
    let before = f.clone();
    let cell = f.attrs.get_mut(fi).ok_or("no such row")?;
    *cell = v;
    super::design::apply(s, id, vec![super::design::Change { before: Some(before), row: Some(row) }])?;
    Ok(Json::Null)
}

fn add_field(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?;
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_') || name.chars().count() > 64 {
        return Err("field names use letters, digits and _ (at most 64)".into());
    }
    let ty = match ps(p, "type").unwrap_or("double").to_ascii_lowercase().as_str() {
        "long" | "integer" | "int" | "short" => FieldType::Integer,
        "double" | "float" | "number" => FieldType::Double,
        "text" | "string" => FieldType::Text,
        "boolean" | "bool" => FieldType::Boolean,
        other => return Err(format!("unknown field type '{other}'")),
    };
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    if l.dataset.field_index(name).is_some() {
        return Err(format!("a field named '{name}' already exists"));
    }
    l.dataset.ensure_field(name, ty);
    Ok(Json::Null)
}

fn delete_field(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let fi = field_index(s, id, ps(p, "field").ok_or("field is required")?)?;
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let read_before = l.renderer.expression().is_some_and(|e| rustgis_data::query::parse(e, &l.dataset).is_ok());
    let removed = l.dataset.fields.remove(fi).name;
    for f in &mut l.dataset.features {
        if fi < f.attrs.len() {
            f.attrs.remove(fi);
        }
    }
    // Nothing may keep referring to the deleted field.
    // (Classes by an expression that read until now go the same way.)
    let unread = read_before && l.renderer.expression().is_some_and(|e| rustgis_data::query::parse(e, &l.dataset).is_err());
    let mut single = None;
    if unread || l.renderer.field().is_some_and(|f| f.eq_ignore_ascii_case(&removed)) {
        l.renderer = rustgis_map::Renderer::Single(l.renderer.base_symbol());
        single = Some(l.name.clone());
    }
    // The layer's labels turn off if the Default class was on it, which starts again from the
    // layer's first name-like field (keeping its name, clause and visibility); by a text expression
    // that still reads, Default only loses the field. Another class on it labels with Default's
    // field instead. A class clause naming it no longer reads, and takes no features, which go on
    // to the next class: it is kept to edit, not dropped with its text symbol.
    if l.labels.default.field.eq_ignore_ascii_case(&removed) {
        let by_text = l.labels.default.by.text.as_deref().is_some_and(|t| rustgis_data::query::parse(t, &l.dataset).is_ok());
        if by_text {
            l.labels.default.field.clear();
        } else {
            l.labels.restart_default(&l.dataset);
        }
    }
    l.labels.repoint_missing_fields(&l.dataset);
    if let Some(q) = &l.definition_query
        && rustgis_data::query::parse(q, &l.dataset).is_err()
    {
        l.definition_query = None;
    }
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
    let ds = &l.dataset;
    let before = d.rules.len();
    d.rules.retain(|r| {
        !r.field.eq_ignore_ascii_case(&removed)
            && match r.kind {
                rustgis_map::design::RuleKind::Calculation => Calculation::parse(&r.expression, ds).is_ok(),
                rustgis_map::design::RuleKind::Constraint => rustgis_data::query::parse(&r.expression, ds).is_ok(),
            }
    });
    let removed_rules = before - d.rules.len();
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
    if let Some(note) = diagram_note {
        s.notify(note, false);
    }
    if let Some(name) = single {
        s.notify(format!("{name}: its classes read the deleted field {removed}, so it is drawn with a single symbol"), false);
    }
    Ok(json!({"rulesRemoved": removed_rules}))
}

/// Sets a field from an expression, on the selected rows or all of them.
fn calculate_field(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let fi = field_index(s, id, ps(p, "field").ok_or("field is required")?)?;
    let expr = ps(p, "expression").map(str::trim).filter(|e| !e.is_empty()).ok_or("expression is required")?.to_string();
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let ty = l.dataset.fields.get(fi).map(|f| f.ty).ok_or("no field")?;
    let calc = Calculation::parse(&expr, &l.dataset)?;
    let rows: Vec<usize> = if l.selection.is_empty() { (0..l.dataset.features.len()).collect() } else { l.selection.clone() };
    let mut out = Vec::new();
    // The results a whole-number field cut (their fraction, or whole: past its range, or not a
    // number), said even when a calculation rule writes the field afterwards: the rule may read
    // the cut value, and says what it cuts itself.
    let mut cuts = Cuts::default();
    for r in &rows {
        let (v, cut) = coerce_cut(calc.eval(&l.dataset, *r), ty);
        cuts.add(cut);
        out.push((*r, v));
    }
    let mut changes = Vec::new();
    for (r, v) in out {
        if let Some(f) = l.dataset.features.get_mut(r) {
            let before = f.clone();
            if let Some(c) = f.attrs.get_mut(fi) {
                *c = v;
            }
            changes.push(super::design::Change { before: Some(before), row: Some(r) });
        }
    }
    super::design::apply(s, id, changes)?;
    let field = s.map().layer(id).and_then(|l| l.dataset.fields.get(fi)).map(|f| f.name.clone()).unwrap_or_default();
    for notice in cuts.notices("Calculate Field", &field) {
        s.notify(notice, false);
    }
    Ok(json!({"rows": rows.len()}))
}

/// Whether Calculate Field reads `expr` (the error is the expression language's).
pub fn check_calculation(expr: &str, ds: &rustgis_data::Dataset) -> Result<(), String> {
    Calculation::parse(expr, ds).map(|_| ())
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
        // A field named like a literal was read as the field (`null + 1`), so what the earlier
        // calculator reads with such a field keeps its meaning (quoted text needn't).
        let named_like_a_literal: Vec<usize> = ["true", "false", "null"].iter().filter_map(|k| ds.field_index(k)).collect();
        if !named_like_a_literal.is_empty()
            && let Ok(c) = Calc::parse(expr, ds)
            && [Some(&c.a), c.b.as_ref()].into_iter().flatten().any(|t| matches!(t, Term::Field(i) if named_like_a_literal.contains(i)))
        {
            return Ok(Calculation::Calc(c));
        }
        match rustgis_data::query::parse(expr, ds) {
            Ok(e) => Ok(Calculation::Expr(e)),
            Err(err) => Calc::parse(expr, ds).map(Calculation::Calc).map_err(|_| err),
        }
    }

    pub(crate) fn eval(&self, ds: &rustgis_data::Dataset, r: usize) -> Value {
        match self {
            Calculation::Expr(e) => rustgis_data::query::value(e, ds, r, &rustgis_data::query::Env::default()),
            Calculation::Calc(c) => c.eval(ds, r),
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

    pub(crate) fn eval(&self, ds: &rustgis_data::Dataset, r: usize) -> Value {
        let get = |t: &Term| -> Value {
            match t {
                Term::Num(n) => Value::Float(*n),
                Term::Text(s) => Value::Text(s.clone()),
                Term::Field(i) => ds.value(r, *i).clone(),
            }
        };
        let va = get(&self.a);
        match (&self.b, self.op) {
            (Some(b), Some(op)) => {
                let vb = get(b);
                match (va.as_f64(), vb.as_f64(), op) {
                    (Some(x), Some(y), '+') => Value::Float(x + y),
                    (Some(x), Some(y), '-') => Value::Float(x - y),
                    (Some(x), Some(y), '*') => Value::Float(x * y),
                    (Some(x), Some(y), '/') if y != 0.0 => Value::Float(x / y),
                    (_, _, '+') if !va.is_null() && !vb.is_null() => Value::Text(format!("{va}{vb}")),
                    _ => Value::Null,
                }
            }
            _ => va,
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

    /// Not a number is null in a whole-number field, said as such, not past its range; a number
    /// past the range is null where it's said, and at the range's end elsewhere, as before.
    #[test]
    fn coerce_takes_not_a_number_as_null() {
        assert_eq!(coerce_cut(Value::Float(f64::NAN), FieldType::Integer), (Value::Null, Cut::NotANumber));
        assert_eq!(coerce(Value::Float(f64::NAN), FieldType::Integer), Value::Null);
        assert_eq!(coerce_cut(Value::Float(f64::INFINITY), FieldType::Integer), (Value::Null, Cut::Range));
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
}
