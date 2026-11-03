//! Map and Edit › Clipboard: copy, cut and paste features, and copy a layer's data path.
//!
//! Copied features are held by the session (with their schema) and also returned as GeoJSON, which
//! the app puts on the system clipboard; paste takes either.

use rustgis_data::{Dataset, Feature, FieldType, GeometryType, Value as Attr};
use serde_json::{Value, json};

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
        C::new("edit.copy", "Copy", "Edit › Clipboard", |s, _| copy(s)).view().key("Mod+C").when(need_selection),
        C::new("edit.cut", "Cut", "Edit › Clipboard", cut).key("Mod+X").when(super::design::need_editable_selection),
        C::new("edit.paste", "Paste", "Edit › Clipboard", paste)
            .key("Mod+V")
            .when(super::design::need_editable)
            .params(r#"{"layer"?, "text"?: GeoJSON text (the copied features when omitted)}"#),
        C::new("edit.pasteSpecial", "Paste Special", "Edit › Clipboard", paste_special)
            .params(r#"{"name"?: string, "text"?: GeoJSON text (the copied features when omitted)}"#),
        C::new("layer.copyPath", "Copy Path", "Edit › Clipboard", copy_path).view().when(need_layer).params(r#"{"layer"?}"#),
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
    data.features = l.selection.iter().filter_map(|r| l.dataset.features.get(*r).cloned()).collect();
    let n = data.features.len();
    let text = rustgis_data::io::to_geojson(&data).to_string();
    let layer = l.name.clone();
    s.clipboard = Some(Clipboard { data });
    s.notify(format!("Copied {n} features from {layer}"), false);
    Ok(json!({"copied": n, "layer": layer, "text": text}))
}

fn cut(s: &mut Session, _: &Value) -> CmdResult {
    let id = source(s).ok_or("nothing is selected")?;
    let out = copy(s)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let mut sel = std::mem::take(&mut l.selection);
    sel.sort_unstable();
    sel.dedup();
    for r in sel.into_iter().rev() {
        if r < l.dataset.features.len() {
            l.dataset.features.remove(r);
        }
    }
    l.touch_geometry();
    // `copy` left the removed features on the clipboard; the archive gets them too.
    let removed: Vec<super::design::Change> = s
        .clipboard
        .as_ref()
        .map(|c| c.data.features.iter().map(|f| super::design::Change { before: Some(f.clone()), row: None }).collect())
        .unwrap_or_default();
    super::design::apply(s, id, removed)?;
    Ok(out)
}

/// The features to paste: GeoJSON text when given, else the clipboard.
fn clip(s: &Session, p: &Value) -> Result<Dataset, String> {
    match ps(p, "text").map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if t.len() > MAX_TEXT => Err("the pasted text is too large".into()),
        Some(t) => {
            let (ds, _) =
                rustgis_data::io::read_geojson(t, "Pasted").map_err(|e| format!("the pasted text isn't GeoJSON features: {e}"))?;
            if ds.features.is_empty() { Err("the pasted text has no features".into()) } else { Ok(ds) }
        }
        None => s.clipboard.as_ref().map(|c| c.data.clone()).ok_or_else(|| "nothing has been copied".to_string()),
    }
}

fn coerce(v: &Attr, ty: FieldType) -> Attr {
    match v {
        Attr::Null => Attr::Null,
        Attr::Text(t) => Attr::parse(t, ty),
        other => Attr::parse(&other.to_string(), ty),
    }
}

fn paste(s: &mut Session, p: &Value) -> CmdResult {
    let src = clip(s, p)?;
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.is_raster() {
        return Err("features can't be pasted into a raster layer".into());
    }
    let want = l.geometry_type();
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
    let mut next = l.dataset.features.iter().map(|f| f.id).max().unwrap_or(0);
    let first = l.dataset.features.len();
    for ft in src.features {
        if ft.geometry.as_ref().is_some_and(|g| want != GeometryType::None && g.kind() != want) {
            continue;
        }
        next = next.saturating_add(1);
        let attrs = l
            .dataset
            .fields
            .iter()
            .zip(&map)
            .map(|(f, i)| i.and_then(|i| ft.attrs.get(i)).map(|v| coerce(v, f.ty)).unwrap_or(Attr::Null))
            .collect();
        l.dataset.features.push(Feature { id: next, geometry: ft.geometry, attrs });
    }
    let n = l.dataset.features.len() - first;
    l.touch_geometry();
    l.selection = (first..first + n).collect();
    let name = l.name.clone();
    super::design::apply(s, id, (first..first + n).map(|r| super::design::Change { before: None, row: Some(r) }).collect())?;
    s.map_mut().layers.iter_mut().filter(|o| o.id != id).for_each(|o| o.selection.clear());
    s.notify(format!("Pasted {n} features into {name}"), false);
    Ok(json!({"pasted": n, "layer": id}))
}

fn paste_special(s: &mut Session, p: &Value) -> CmdResult {
    let mut ds = clip(s, p)?;
    if let Some(n) = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        ds.name = n.chars().take(200).collect();
    } else {
        ds.name = format!("{} (pasted)", ds.name);
    }
    ds.source = None;
    let n = ds.features.len();
    let id = super::layer::add_dataset(s, ds);
    Ok(json!({"pasted": n, "layer": id}))
}

fn copy_path(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let path = l.raster.as_ref().and_then(|r| r.path.clone()).or_else(|| l.dataset.source.clone());
    let path = path.ok_or_else(|| format!("{} isn't stored in a file", l.name))?;
    // A service layer's address is its source: copied without keys, tokens or its password, as
    // the details pane shows it and a saved project keeps a web address.
    Ok(json!({"text": crate::sources::redact(&path)}))
}
