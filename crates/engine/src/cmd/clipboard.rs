//! Map and Edit › Clipboard: copy, cut and paste features, and copy a layer's data path.
//!
//! Copied features are held by the session (with their schema) and also returned as GeoJSON, which
//! the app puts on the system clipboard; paste takes either.

use rustgis_data::raster::Crs;
use rustgis_data::{Dataset, Feature, FieldType, GeometryType, Value as Attr};
use serde_json::{Value, json};

use super::edit::{Cut, Cuts};
use super::{need_layer, need_selection, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Largest GeoJSON text accepted by paste (64 MiB).
const MAX_TEXT: usize = 64 << 20;

/// Features on the clipboard, with the schema of the layer they came from.
#[derive(Clone, Debug)]
pub struct Clipboard {
    pub data: Dataset,
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("edit.copy", "Copy", "Edit › Clipboard", |s, _| copy(s))
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .view()
            .key("Mod+C")
            .when(need_selection),
        C::new("edit.cut", "Cut", "Edit › Clipboard", cut)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .key("Mod+X")
            .when(super::design::need_editable_selection),
        C::new("edit.paste", "Paste", "Edit › Clipboard", paste)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .key("Mod+V")
            .when(super::design::need_editable)
            .params(r#"{"layer"?, "text"?: GeoJSON text (the copied features when omitted)}"#),
        C::new("edit.pasteSpecial", "Paste Special", "Edit › Clipboard", paste_special)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .params(r#"{"name"?: string, "text"?: GeoJSON text (the copied features when omitted)}"#),
        C::new("layer.copyPath", "Copy Path", "Edit › Clipboard", copy_path)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .view()
            .when(need_layer)
            .params(r#"{"layer"?}"#),
    ]
}

/// The layer whose selection is copied: the active layer if it has one, else the first layer
/// with selected features.
fn source(s: &Session) -> Option<u32> {
    let m = s.map();
    s.active_layer
        .and_then(|id| m.layer(id))
        .filter(|l| !l.selection.is_empty())
        .or_else(|| m.layers.iter().find(|l| !l.selection.is_empty()))
        .map(|l| l.id)
}

fn copy(s: &mut Session) -> CmdResult {
    let id = source(s).ok_or("nothing is selected")?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let mut data = Dataset::new(l.name.clone(), l.geometry_type(), l.dataset.fields.clone());
    // The clipboard holds them as the layer does, in its system; its GeoJSON is in
    // longitude/latitude, converted exactly through WGS 84, whatever map they were copied from.
    data.held_in = l.dataset.coord_crs();
    data.crs = l.dataset.crs;
    // What the import lost, Define Projection can't recover in a layer pasted from it either.
    data.crs_incomplete = l.dataset.crs_incomplete;
    data.features = l.selection.iter().filter_map(|r| l.dataset.features.get(*r).cloned()).collect::<Vec<_>>().into();
    let n = data.features.len();
    // A feature with a place that can't be given in longitude/latitude is in the text without its
    // geometry, and said; the clipboard keeps it whole, as the layer holds it.
    let (lonlat, unplaced) = data.in_lonlat_leaving_out()?;
    let text = rustgis_data::io::to_geojson(&lonlat).to_string();
    drop(lonlat);
    let layer = l.name.clone();
    s.clipboard = Some(Clipboard { data });
    s.notify(format!("Copied {n} features from {layer}"), false);
    if let Some(note) = rustgis_data::unplaced_note("Copied text", &layer, unplaced) {
        s.say(note);
    }
    Ok(json!({"copied": n, "layer": layer, "text": text}))
}

fn cut(s: &mut Session, _: &Value) -> CmdResult {
    let id = source(s).ok_or("nothing is selected")?;
    let out = copy(s)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let copied = std::mem::take(&mut l.selection);
    let mut sel = copied.clone();
    sel.sort_unstable();
    sel.dedup();
    let mut gone = std::collections::HashMap::new();
    for r in sel.into_iter().rev() {
        if r < l.dataset.features.len() {
            gone.insert(r, l.dataset.features.remove(r));
        }
    }
    l.touch_geometry();
    // The archive gets the removed features in the order they were copied, as stored, in the
    // layer's own coordinates, as the clipboard holds them.
    let removed: Vec<super::design::Change> =
        copied.iter().filter_map(|r| gone.get(r)).map(|f| super::design::Change { before: Some(f.clone()), row: None }).collect();
    super::design::apply(s, id, removed)?;
    Ok(out)
}

/// The features to paste: GeoJSON text when given, with what its reader says it left out, else
/// the clipboard.
fn clip(s: &Session, p: &Value) -> Result<(Dataset, rustgis_data::loss::Notes), String> {
    match ps(p, "text").map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if t.len() > MAX_TEXT => Err("the pasted text is too large".into()),
        Some(t) => {
            // Held as the text gives them: in the system a legacy `crs` member names, else in
            // longitude/latitude.
            let (ds, report) =
                rustgis_data::io::read_geojson(t, "Pasted").map_err(|e| format!("the pasted text isn't GeoJSON features: {e}"))?;
            let said = report.messages.prefixed("Pasted text");
            if ds.features.is_empty() { Err("the pasted text has no features".into()) } else { Ok((ds, said)) }
        }
        None => s.clipboard.as_ref().map(|c| (c.data.clone(), Default::default())).ok_or_else(|| "nothing has been copied".to_string()),
    }
}

/// A pasted value in a field of type `ty`, and what that took from it, as `edit.setValue` and
/// Calculate Field convert: a fraction or a number past the range of a whole-number field, a
/// date-time's time in a date field.
fn coerce(v: &Attr, ty: FieldType) -> (Attr, Cut) {
    match v {
        // Text into a text field is kept as it is, `<null>` and empty text too: it was a value.
        Attr::Text(t) if ty == FieldType::Text => (Attr::Text(t.clone()), Cut::None),
        v => super::edit::coerce_cut(v.clone(), ty),
    }
}

fn paste(s: &mut Session, p: &Value) -> CmdResult {
    let (mut src, said) = clip(s, p)?;
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.is_raster() {
        return Err("features can't be pasted into a raster layer".into());
    }
    let want = l.geometry_type();
    // The shapes the layer takes, written in its own coordinates from the system the clipboard
    // holds them in: as they are in a layer of that system (pasted where they were copied from,
    // nothing moves), else converted by the operation between the two, which doesn't depend on
    // the map. All converted before anything changes; one with a place the layer's system can't
    // hold is left out, counted and said, rather than the paste refused.
    let (from, own) = (src.coord_crs(), l.dataset.coord_crs());
    // Into Web Mercator by way of longitude/latitude, where the latitudes it can't hold are found.
    let steps = if from == own {
        vec![]
    } else if own == Crs::WebMercator && from != Crs::LonLat {
        vec![from.to(Crs::LonLat)?, Crs::LonLat.to(own)?]
    } else {
        vec![from.to(own)?]
    };
    let mut placed = Vec::with_capacity(src.features.len());
    if !steps.is_empty() {
        let none = rustgis_data::HeldPlaces::default();
        for r in 0..src.features.len() {
            let fits = src.features.row(r).and_then(|x| x.geometry()).is_some_and(|g| want == GeometryType::None || g.kind() == want);
            let Some(Some(g)) = fits.then(|| src.features.set_geometry(r, None)).flatten() else {
                placed.push(true);
                continue;
            };
            match steps.iter().try_fold(g, |g, to| Dataset::edit_from_lonlat_by(to, g, &none)) {
                Ok(g) => {
                    src.features.set_geometry(r, Some(g));
                    placed.push(true);
                }
                Err(_) => placed.push(false),
            }
        }
    }
    let unplaced = placed.iter().filter(|p| !**p).count();
    if unplaced > 0 {
        let mut keep = placed.into_iter();
        src.features.retain_rows(|_| keep.next().unwrap_or(true));
        if src.features.is_empty() {
            return Err(format!("Paste: the copied feature(s) have a place {} ({}) can't hold", l.name, own.label()));
        }
    }
    // Ids for every feature the layer takes (one of another shape is left out below), above every id
    // it has or keeps new rows above, refused before anything changes when they would run past the
    // largest id.
    let taking = src.features.rows().filter(|r| !r.geometry().is_some_and(|g| want != GeometryType::None && g.kind() != want)).count();
    let mut next = l
        .dataset
        .features
        .free_ids(taking)
        .ok_or_else(|| format!("{}: no ids are left above the layer's highest for the pasted features", l.name))?
        .saturating_sub(1);
    if want != GeometryType::None && src.geometry_type != GeometryType::None && want != src.geometry_type {
        return Err(format!(
            "{} features can't go in a {} layer; use Paste Special to paste them as a new layer",
            src.geometry_type.label(),
            want.label()
        ));
    }
    // Pasted graphics (rows whose `kind` is a graphic's) keep their style in a graphics layer made
    // before a style field (#583): it gains each field one of them gives a value, as
    // `graphics.add` and `graphics.edit` add one when it is first set. Other features pasted with
    // or without them add none, though they have a `kind` or a field so named of their own.
    if l.graphics {
        use rustgis_map::graphics::{FIELDS, SHAPES, kind};
        let rows: Vec<usize> = (0..src.features.len()).filter(|r| SHAPES.contains(&kind(&src, *r))).collect();
        for (name, ty) in FIELDS {
            if src.field_index(name).is_some_and(|fi| rows.iter().any(|r| !src.value(*r, fi).is_null())) {
                l.dataset.ensure_field(name, *ty);
            }
        }
    }
    // Fields are matched by name; the rest are left empty.
    let map: Vec<Option<usize>> = l.dataset.fields.iter().map(|f| src.field_index(&f.name)).collect();
    let first = l.dataset.features.len();
    // Per field, what converting the pasted values cut, and the values that aren't null but that
    // the field can't hold.
    let mut unfit = vec![(Cuts::default(), 0usize); l.dataset.fields.len()];
    let taken: Vec<Feature> = src
        .features
        .into_iter()
        .filter(|ft| !ft.geometry.as_ref().is_some_and(|g| want != GeometryType::None && g.kind() != want))
        .collect();
    for ft in taken {
        next = next.saturating_add(1);
        let attrs = l
            .dataset
            .fields
            .iter()
            .zip(&map)
            .zip(unfit.iter_mut())
            .map(|((f, i), (cuts, unfit))| match i.and_then(|i| ft.attrs.get(i)) {
                Some(v) => {
                    let (out, cut) = coerce(v, f.ty);
                    cuts.add(cut);
                    *unfit += usize::from(out.is_null() && cut == Cut::None && !v.asks_null());
                    out
                }
                None => Attr::Null,
            })
            .collect();
        l.dataset.features.push(Feature { id: next, geometry: ft.geometry, attrs });
    }
    let n = l.dataset.features.len() - first;
    l.touch_geometry();
    l.selection = (first..first + n).collect();
    let name = l.name.clone();
    let features = |n: usize| if n == 1 { "1 feature".to_string() } else { format!("{n} features") };
    use rustgis_data::loss::{LossCode, Note};
    let mut notices = Vec::new();
    if unplaced > 0 {
        let (have, they) = if unplaced == 1 { ("has", "it wasn't") } else { ("have", "they weren't") };
        notices.push(Note::loss(
            LossCode::DroppedFeature,
            format!("Paste: {} {have} a place that layer {name} ({}) can't hold, so {they} pasted", features(unplaced), own.label()),
        ));
    }
    for (f, (cuts, n)) in l.dataset.fields.iter().zip(unfit) {
        notices.extend(cuts.classified("Paste", &f.name).into_iter().map(|(code, text)| Note::loss(code, text)));
        // A value that asks for null isn't counted: only values the field couldn't hold.
        if n > 0 {
            notices.push(Note::loss(
                LossCode::DroppedValue,
                format!("Paste: '{}' is null in {}, whose value isn't a {} value", f.name, features(n), f.ty.label()),
            ));
        }
    }
    super::design::apply(s, id, (first..first + n).map(|r| super::design::Change { before: None, row: Some(r) }).collect())?;
    s.map_mut().layers.iter_mut().filter(|o| o.id != id).for_each(|o| o.selection.clear());
    s.notify(format!("Pasted {n} features into {name}"), false);
    for notice in said.into_iter().chain(notices) {
        s.say(notice);
    }
    Ok(json!({"pasted": n, "layer": id}))
}

fn paste_special(s: &mut Session, p: &Value) -> CmdResult {
    // The name given, checked before the clipboard is read.
    let name = super::name_given(p, "a layer's name", 200)?;
    let (mut ds, said) = clip(s, p)?;
    ds.name = name.unwrap_or_else(|| format!("{} (pasted)", ds.name));
    ds.source = None;
    let n = ds.features.len();
    let id = super::layer::add_dataset(s, ds);
    for m in said {
        s.say(m);
    }
    Ok(json!({"pasted": n, "layer": id}))
}

fn copy_path(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let path = l.raster.as_ref().and_then(|r| r.path.clone()).or_else(|| l.dataset.source.clone());
    let path = path.ok_or_else(|| format!("{} isn't stored in a file", l.name))?;
    // A service layer's address is its source: copied without keys, tokens or its password, as
    // the details pane shows it and a saved project keeps a web address.
    let text = crate::sources::redact(&path);
    if text != path {
        // Not the layer's name: it may hold an address with a key of its own.
        s.notify("The copied address leaves out part of the layer's address (keys, tokens, a password, a login or a fragment), so it may not work as it is", false);
    }
    Ok(json!({"text": text}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paste that would need ids past the largest there is is refused, with nothing pasted,
    /// rather than giving pasted features one id between them.
    #[test]
    fn a_paste_with_no_ids_left_is_refused() {
        let mut s = Session::new(crate::sample::world_project());
        let id = s.map().layer_by_name("Cities").unwrap().id;
        s.run("select.rows", &json!({"layer": id, "rows": [0, 1]})).unwrap();
        s.run("edit.copy", &Value::Null).unwrap();
        let ds = &mut s.map_mut().layer_mut(id).unwrap().dataset;
        let fields = ds.fields.len();
        // One id left below `i64::MAX`, the largest a GeoPackage or PostGIS feature can have.
        let top = i64::MAX.unsigned_abs() - 2;
        ds.features.push_parts(top, None, vec![Attr::Null; fields]);
        let before = s.map().layer(id).unwrap().dataset.features.len();
        let refused = s.run("edit.paste", &json!({"layer": id})).unwrap_err();
        assert!(refused.contains("no ids are left"), "{refused}");
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), before);
        // A polygon and a point pasted into a polygon layer with room for one id: the point is
        // left out, as ever, and the polygon takes the one id left.
        let countries = s.map().layer_by_name("Countries").unwrap().id;
        let ds = &mut s.map_mut().layer_mut(countries).unwrap().dataset;
        let fields = ds.fields.len();
        ds.features.push_parts(top, None, vec![Attr::Null; fields]);
        let mixed = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}},
            {"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[0.5,0.5]}}]}"#;
        let pasted = s.run("edit.paste", &json!({"layer": countries, "text": mixed})).unwrap();
        assert_eq!(pasted["pasted"], 1, "{pasted}");
        assert!(s.map().layer(countries).unwrap().dataset.features.iter().any(|f| f.id == top + 1));
        // With room for them, they paste.
        s.map_mut().layer_mut(id).unwrap().dataset.features.retain_rows(|r| r.id() != top);
        s.run("edit.paste", &json!({"layer": id})).unwrap();
    }

    #[test]
    fn a_moment_pasted_into_a_date_field_keeps_its_day() {
        // 00:30 at +02:00 is the evening before in UTC: the day shown is kept.
        let t = rustgis_data::DateTime::parse("2026-03-05T00:30:00+02:00").unwrap();
        assert_eq!(coerce(&Attr::DateTime(t), FieldType::Date), (Attr::Date(t.date()), Cut::Time));
        assert_eq!(t.date().to_string(), "2026-03-05");
        assert_eq!(coerce(&Attr::Text("2026-03-05".into()), FieldType::Date).0.to_string(), "2026-03-05");
        assert!(coerce(&Attr::Text("soon".into()), FieldType::Date).0.is_null());
        assert_eq!(coerce(&Attr::Text("<null>".into()), FieldType::Text), (Attr::Text("<null>".into()), Cut::None));
        // A date is no yes/no or number: null, which paste counts and says.
        for ty in [FieldType::Boolean, FieldType::Integer, FieldType::Double] {
            assert_eq!(coerce(&Attr::DateTime(t), ty), (Attr::Null, Cut::None), "{ty:?}");
        }
    }

    #[test]
    fn a_number_pasted_into_a_whole_number_field_says_what_it_cut() {
        assert_eq!(coerce(&Attr::Text("2".into()), FieldType::Integer), (Attr::Int(2), Cut::None));
        assert_eq!(coerce(&Attr::Text("2.5".into()), FieldType::Integer), (Attr::Int(2), Cut::Fraction));
        assert_eq!(coerce(&Attr::Float(2.5), FieldType::Integer), (Attr::Int(2), Cut::Fraction));
        assert_eq!(coerce(&Attr::Text("1e30".into()), FieldType::Integer), (Attr::Null, Cut::Range));
    }
}
