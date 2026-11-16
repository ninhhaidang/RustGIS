//! Runtime construction for boxes on ordinary polygon layers. No fields or saved metadata are
//! added to a dataset. Construction snapshots accompany undo/redo. Handles require the recorded
//! geometry to match the feature; foreign geometry is never fitted to a new box. Insert/delete
//! edits through `design::apply` forget records, while sync and restore paths can retain them
//! subject to that geometry-equality check.
use rustgis_data::raster::{Crs, ImageGrid, Transformer};
use rustgis_data::{Geometry, GeometryType};
use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::construction::{Cache, Construction, Record};
use crate::{CmdResult, Session};

/// A frozen planar grid, with x right and y down. The default is Web Mercator grid metres,
/// not ground metres. A GeoTIFF's original grid can be supplied through [`Self::from_grid`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Frame {
    pub epsg: u32,
    pub width: u32,
    pub height: u32,
    pub affine: [f64; 6],
}
impl Default for Frame {
    fn default() -> Self {
        Self { epsg: 3857, width: 1, height: 1, affine: [0., 1., 0., 0., 0., -1.] }
    }
}
thread_local! {
    static PROJECTION: std::cell::RefCell<Option<(u32, std::rc::Rc<Transformer>)>> = const { std::cell::RefCell::new(None) };
}
impl Frame {
    pub fn from_grid(grid: &ImageGrid) -> Result<Self, String> {
        let epsg = u32::from(grid.crs().epsg());
        if epsg == 0 {
            return Err("construction grids currently require a recognised EPSG coordinate system".into());
        }
        Ok(Self { epsg, width: grid.width(), height: grid.height(), affine: grid.affine() })
    }
    fn grid(&self) -> Result<ImageGrid, String> {
        let crs = Crs::from_epsg(self.epsg)
            .filter(|c| !c.lacks_datum_shift())
            .ok_or("construction grid needs a supported EPSG coordinate system")?;
        ImageGrid::new(self.width, self.height, crs, self.affine).map_err(|e| e.to_string())
    }
    fn projection(&self) -> Option<std::rc::Rc<Transformer>> {
        PROJECTION.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some((_, t)) = cache.as_ref().filter(|(e, _)| *e == self.epsg) {
                return Some(std::rc::Rc::clone(t));
            }
            let t = std::rc::Rc::new(self.grid().ok()?.crs().transformer().ok()?);
            *cache = Some((self.epsg, std::rc::Rc::clone(&t)));
            Some(t)
        })
    }
    pub fn to_lonlat(&self, p: [f64; 2]) -> Option<Coord> {
        self.projection()?.to_lonlat(self.grid().ok()?.to_world(p[0], p[1])?)
    }
    pub fn to_local(&self, c: Coord) -> Option<[f64; 2]> {
        let (x, y) = self.grid().ok()?.to_pixel(self.projection()?.from_lonlat(c)?)?;
        Some([x, y])
    }
}

/// Clockwise degrees from the grid's positive x axis. Every edit regenerates four corners from
/// these parameters, never repeatedly rotating the previous polygon's vertices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoxSpec {
    #[serde(default)]
    pub frame: Frame,
    pub center: [f64; 2],
    pub width: f64,
    pub height: f64,
    pub angle: f64,
}
impl BoxSpec {
    pub fn axes(&self) -> ([f64; 2], [f64; 2]) {
        let (s, c) = self.angle.rem_euclid(360.).to_radians().sin_cos();
        ([c, s], [-s, c])
    }
    pub fn point(&self, x: f64, y: f64) -> [f64; 2] {
        let (u, v) = self.axes();
        [self.center[0] + u[0] * x + v[0] * y, self.center[1] + u[1] * x + v[1] * y]
    }
    pub fn corners(&self) -> [[f64; 2]; 4] {
        let (w, h) = (self.width / 2., self.height / 2.);
        [self.point(-w, -h), self.point(w, -h), self.point(w, h), self.point(-w, h)]
    }
    pub fn geometry(&self) -> Result<Geometry, String> {
        self.frame.grid()?;
        if !self.center.iter().chain([&self.width, &self.height, &self.angle]).all(|v| v.is_finite() && v.abs() <= 1e12)
            || self.width < 1e-6
            || self.height < 1e-6
        {
            return Err("a box needs finite coordinates, angle and positive dimensions".into());
        }
        let mut ring = Vec::with_capacity(5);
        for p in self.corners() {
            ring.push(
                self.frame
                    .to_lonlat(p)
                    .filter(|c| c.is_finite() && c.x.abs() <= 180. && c.y.abs() < 85.05)
                    .ok_or("box crosses an unsupported projection boundary or the polar editing limit")?,
            );
        }
        let span = ring.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max) - ring.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        if span > 180. {
            return Err("boxes crossing the antimeridian are not supported".into());
        }
        let mut sign = 0.;
        for i in 0..4 {
            let (Some(a), Some(b), Some(c)) = (ring.get(i), ring.get((i + 1) % 4), ring.get((i + 2) % 4)) else {
                return Err("missing box corner".into());
            };
            let turn = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
            if !turn.is_finite() || turn == 0. || (sign != 0. && turn.signum() != sign) {
                return Err("box folds or collapses after transformation; use a smaller box".into());
            }
            sign = turn.signum();
        }
        if sign < 0. {
            ring.reverse();
        }
        let first = ring.first().copied().ok_or("missing box corner")?;
        ring.push(first);
        Ok(Geometry::Polygon(vec![vec![ring]]))
    }
}

impl Session {
    /// Only unchanged boxes constructed in this session get constrained handles. Imported
    /// polygons and boxes whose vertices were independently edited are never silently refitted.
    pub fn box_spec(&self, layer: u32, row: usize) -> Option<BoxSpec> {
        let g = self.map().layer(layer)?.dataset.features.get(row)?.geometry.as_ref()?;
        self.constructions.get(Cache::key(self, layer, row)?, g).and_then(|r| r.spec.box_spec().cloned())
    }
}

// Shared feature intake also supports standalone tables; a box always targets a layer.
fn reject_table(p: &Value) -> Result<(), String> {
    if p.get("table").is_some_and(|t| !t.is_null()) {
        return Err("rotated boxes require a polygon layer; the 'table' parameter is unsupported".into());
    }
    Ok(())
}
pub(crate) fn new_box_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    reject_table(p)?;
    crate::cmd::design::new_feature_inputs(input, p)
}
pub fn check_layer(s: &Session, id: u32) -> Result<(), String> {
    if s.map().body != "earth" {
        return Err("rotated box grids currently require an Earth map".into());
    }
    if !s.map().editing.settings.enabled {
        return Err("turn on editing first (Edit › Edit)".into());
    }
    let l = s.map().layer(id).ok_or("select an editable polygon layer")?;
    if l.is_streamed() || l.graphics || l.raster.is_some() || !matches!(l.geometry_type(), GeometryType::Polygon | GeometryType::None) {
        return Err("choose a loaded editable polygon layer".into());
    }
    Ok(())
}
/// Interactive activation additionally requires a view the pointer-based editor supports.
/// Geometry commands only check the layer: their frozen construction grid is view-independent.
pub fn check_tool(s: &Session, id: u32) -> Result<(), String> {
    check_layer(s, id)?;
    if s.map().projection.is_globe() || s.map().camera.pitch != 0. {
        return Err("the Rotated Box tool requires a flat Earth map with zero pitch".into());
    }
    Ok(())
}
fn parsed(p: &Value, previous_frame: Option<&Frame>) -> Result<BoxSpec, String> {
    let value = p.get("box").ok_or("box is required")?;
    let mut b: BoxSpec = BoxSpec::deserialize(value).map_err(|e| e.to_string())?;
    if value.get("frame").is_none()
        && let Some(frame) = previous_frame
    {
        b.frame = frame.clone();
    }
    if !b.angle.is_finite() || b.angle.abs() > 1e12 {
        return Err("box angle must be finite".into());
    }
    b.angle = b.angle.rem_euclid(360.);
    b.geometry()?;
    Ok(b)
}
fn row(p: &Value) -> Result<usize, String> {
    p.get("row").and_then(Value::as_u64).and_then(|r| usize::try_from(r).ok()).ok_or_else(|| "row is required".into())
}
pub(crate) fn add(s: &mut Session, p: &Value) -> CmdResult {
    reject_table(p)?;
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    s.constructions.room()?;
    let b = parsed(p, None)?;
    if let Some(attributes) = p.get("attributes") {
        let attributes = attributes.as_object().ok_or("attributes must be an object")?;
        let layer = s.map().layer(id).ok_or("no layer")?;
        for field in attributes.keys() {
            if !layer.dataset.fields.iter().any(|f| f.name == *field) {
                return Err(format!("no attribute field '{field}'; add it to the layer first"));
            }
        }
    }
    let g = b.geometry()?;
    let result = crate::cmd::edit::add_geometry(s, id, p, g)?;
    let row = result.get("row").and_then(Value::as_u64).and_then(|r| usize::try_from(r).ok()).ok_or("box was not added")?;
    remember(s, id, row, b.clone())?;
    Ok(json!({"row":row,"box":b}))
}
pub(crate) fn remember(s: &mut Session, id: u32, row: usize, spec: BoxSpec) -> Result<(), String> {
    let key = Cache::key(s, id, row).ok_or("box source changed")?;
    if key.3 == u64::MAX {
        return Err("feature IDs have reached their limit; create a fresh layer for boxes".into());
    }
    let g =
        s.map().layer(id).and_then(|l| l.dataset.features.get(row)).and_then(|f| f.geometry.clone()).ok_or("box geometry is missing")?;
    if g != spec.geometry()? {
        return Err("an edit rule changed the box geometry".into());
    }
    s.constructions.remember(key, Record { spec: Construction::Box(spec), geometry: g })
}
pub(crate) fn update(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    if p.get("attributes").is_some() {
        return Err("edit.updateBox changes the shape only; set attributes with edit.setValue".into());
    }
    let row = row(p)?;
    let key = Cache::key(s, id, row).ok_or("no such feature")?;
    let g = s.map().layer(id).and_then(|l| l.dataset.features.get(row)).and_then(|f| f.geometry.as_ref()).ok_or("missing box geometry")?;
    let before = s
        .constructions
        .get(key, g)
        .and_then(|r| r.spec.box_spec().cloned())
        .ok_or("this feature has no unchanged box construction; use Edit Vertices")?;
    if p.get("featureId").is_some_and(|v| v.as_u64() != Some(key.3)) {
        return Err("the feature changed during this edit".into());
    }
    let b = parsed(p, Some(&before.frame))?;
    if b.frame != before.frame {
        return Err("a box's construction grid is frozen; start another box to change grids".into());
    }
    crate::cmd::design::replace_geometry(s, id, row, b.geometry()?)?;
    remember(s, id, row, b.clone())?;
    Ok(json!({"row":row,"box":b}))
}
pub(crate) fn delete_box(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let row = row(p)?;
    let key = Cache::key(s, id, row).ok_or("no such feature")?;
    if s.box_spec(id, row).is_none() {
        return Err("this feature has no unchanged box construction".into());
    }
    if p.get("featureId").is_some_and(|v| v.as_u64() != Some(key.3)) {
        return Err("the feature changed during this edit".into());
    }
    let layer = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let before = layer.dataset.features.remove(row);
    layer.selection = layer.selection.iter().copied().filter(|r| *r != row).map(|r| if r > row { r - 1 } else { r }).collect();
    layer.touch_geometry();
    crate::cmd::design::apply(s, id, vec![crate::cmd::design::Change { before: Some(before), row: None }])?;
    Ok(json!({"deleted":1,"featureId":key.3}))
}
pub(crate) fn info(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let row = row(p)?;
    let b = s.box_spec(id, row).ok_or("this feature has no unchanged box construction")?;
    Ok(json!({"row":row,"box":b}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Dataset, Field, FieldType};
    fn session() -> (Session, u32) {
        let mut s = Session::new(crate::Project::default());
        let id = s.map_mut().add_dataset(Dataset::new(
            "Boxes",
            GeometryType::Polygon,
            vec![Field::new("class", FieldType::Text), Field::new("_rustgis_box", FieldType::Text)],
        ));
        s.active_layer = Some(id);
        s.run("edit.enable", &json!({"on":true})).unwrap();
        (s, id)
    }
    fn spec() -> BoxSpec {
        BoxSpec { frame: Frame::default(), center: [1000., 1000.], width: 200., height: 100., angle: 30. }
    }
    fn add(s: &mut Session, b: &BoxSpec) -> usize {
        s.run("edit.addBox", &json!({"box":b,"attributes":{"class":"vehicle","_rustgis_box":"user-owned"}})).unwrap()["row"]
            .as_u64()
            .unwrap() as usize
    }
    #[test]
    fn box_edit_preserves_attributes_identity_and_construction_through_history() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let original = s.map().layer(id).unwrap().dataset.features[row].clone();
        let fields = s.map().layer(id).unwrap().dataset.fields.clone();
        let mut next = b.clone();
        next.angle = 112.;
        next.width = 310.;
        s.run("edit.updateBox", &json!({"row":row,"featureId":original.id,"box":next})).unwrap();
        let after = &s.map().layer(id).unwrap().dataset.features[row];
        assert_eq!(after.id, original.id);
        assert_eq!(after.attrs, original.attrs);
        assert_eq!(s.map().layer(id).unwrap().dataset.fields, fields);
        assert_eq!(s.box_spec(id, row), Some(next.clone()));
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.box_spec(id, row), Some(b.clone()));
        assert_eq!(s.map().layer(id).unwrap().dataset.features[row].geometry, original.geometry);
        s.run("edit.redo", &json!({})).unwrap();
        assert_eq!(s.box_spec(id, row), Some(next));
        s.run("edit.delete", &json!({})).unwrap();
        assert!(s.box_spec(id, row).is_none());
        s.run("edit.undo", &json!({})).unwrap();
        assert!(s.box_spec(id, row).is_some());
    }
    #[test]
    fn box_edit_history_branch_does_not_lose_an_unchanged_boxes_construction() {
        let (mut s, id) = session();
        let b = spec();
        let first = add(&mut s, &b);
        let second = add(&mut s, &b);
        let mut next = b.clone();
        next.angle = 80.;
        s.run("edit.updateBox", &json!({"row":first,"box":next})).unwrap();
        s.run("edit.undo", &json!({})).unwrap();
        for n in 0..45 {
            next.angle = f64::from(n);
            s.run("edit.updateBox", &json!({"row":second,"box":next})).unwrap();
        }
        assert_eq!(s.box_spec(id, first), Some(b));
        for _ in 0..40 {
            s.run("edit.undo", &json!({})).unwrap();
        }
        assert!(s.box_spec(id, first).is_some());
        assert!(s.box_spec(id, second).is_some());
    }
    #[test]
    fn box_edit_foreign_geometry_changes_disable_handles_and_undo_restores_them() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let changed = Geometry::Polygon(vec![vec![vec![Coord::new(0., 0.), Coord::new(1., 0.), Coord::new(0., 1.), Coord::new(0., 0.)]]]);
        s.run("edit.setGeometry", &json!({"row":row,"geometry":rustgis_data::io::geometry_to_geojson(&changed)})).unwrap();
        assert!(s.box_spec(id, row).is_none());
        assert!(s.run("edit.updateBox", &json!({"row":row,"box":b})).is_err());
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.box_spec(id, row), Some(b));
    }
    #[test]
    fn box_edit_uses_the_original_affine_grid_without_accumulating_rotation_error() {
        let grid = ImageGrid::new(400, 300, Crs::from_epsg(32633).unwrap(), [500_000., 0.8, 0.2, 1000., 0.1, -0.8]).unwrap();
        let mut b = BoxSpec { frame: Frame::from_grid(&grid).unwrap(), center: [200., 150.], width: 60., height: 30., angle: 0. };
        for angle in [0., 17., 90., 181., 359., 17.] {
            b.angle = angle;
            let Geometry::Polygon(polygons) = b.geometry().unwrap() else { panic!("polygon") };
            let ring = &polygons[0][0];
            assert_eq!(ring.len(), 5);
            assert_eq!(ring.first(), ring.last());
            for corner in b.corners() {
                assert!(ring.iter().take(4).any(|c| {
                    let q = b.frame.to_local(*c).unwrap();
                    (q[0] - corner[0]).abs() < 1e-5 && (q[1] - corner[1]).abs() < 1e-5
                }));
            }
            let c = b.corners();
            let a = [c[1][0] - c[0][0], c[1][1] - c[0][1]];
            let d = [c[2][0] - c[1][0], c[2][1] - c[1][1]];
            assert!((a[0] * d[0] + a[1] * d[1]).abs() < 1e-9);
            assert!((a[0].hypot(a[1]) - 60.).abs() < 1e-10);
            assert!((d[0].hypot(d[1]) - 30.).abs() < 1e-10);
        }
    }
    #[test]
    fn box_edit_refuses_bad_parameters_and_stale_identities_without_partial_edits() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let before = s.map().layer(id).unwrap().dataset.features[row].clone();
        let depth = s.undo_depth();
        for patch in [
            json!({"width":0}),
            json!({"height":-1}),
            json!({"angle":"NaN"}),
            json!({"center":[1e99,0]}),
            json!({"frame":{"epsg":0}}),
            json!({"frame":{"affine":[0,0,0,0,0,0]}}),
        ] {
            let mut bad = serde_json::to_value(&b).unwrap();
            for (k, v) in patch.as_object().unwrap() {
                bad[k] = v.clone();
            }
            assert!(s.run("edit.updateBox", &json!({"row":row,"box":bad})).is_err());
            assert_eq!(s.box_spec(id, row), Some(b.clone()));
            assert_eq!(s.undo_depth(), depth);
        }
        assert!(s.run("edit.updateBox", &json!({"row":row,"featureId":before.id+1,"box":b})).is_err());
        let after = &s.map().layer(id).unwrap().dataset.features[row];
        assert_eq!(after.id, before.id);
        assert_eq!(after.geometry, before.geometry);
        assert_eq!(after.attrs, before.attrs);
    }
    #[test]
    fn box_edit_snapshots_make_export_notices_stable_during_later_edits() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let cache = s.constructions.clone();
        let layer = s.map().layer(id).unwrap().clone();
        let map = s.map().instance_id();
        for n in 0..45 {
            let mut b = b.clone();
            b.angle = f64::from(n);
            s.run("edit.updateBox", &json!({"row":row,"box":b})).unwrap();
        }
        assert!(cache.export_notes(map, &layer).unwrap()[0].starts_with("1 rotated box"));
        assert!(s.constructions.export_notes(map, &layer).unwrap().is_empty());
    }
    #[test]
    fn box_edit_geojson_is_plain_geometry_with_counted_export_notes() {
        let (mut s, id) = session();
        let b = spec();
        add(&mut s, &b);
        add(&mut s, &b);
        let path = std::env::temp_dir().join(format!("rustgis-box-edit-{}.geojson", std::process::id()));
        s.notices.clear();
        s.run("share.exportLayer", &json!({"layer":id,"path":path})).unwrap();
        assert_eq!(s.notices.iter().filter(|n| n.text.starts_with("2 rotated box")).count(), 1);
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(path);
        let (ds, report) = rustgis_data::io::read_geojson(&text, "shared").unwrap();
        assert_eq!(ds.features.len(), 2);
        assert_eq!(report.skipped, 0);
        assert_eq!(ds.features[0].attrs[ds.field_index("class").unwrap()], rustgis_data::Value::Text("vehicle".into()));
        let json: Value = serde_json::from_str(&text).unwrap();
        assert!(json.get("rustgis_annotation").is_none());
        assert_eq!(json["features"][0]["properties"]["_rustgis_box"], "user-owned");
        assert!(json["features"][0]["properties"].as_object().unwrap().get("frame").is_none());
        let mut reopened = Session::new(crate::Project::default());
        let other = reopened.map_mut().add_dataset(ds);
        reopened.active_layer = Some(other);
        assert!(reopened.box_spec(other, 0).is_none());
        let ordinary = Dataset::new("ordinary", GeometryType::Polygon, vec![]);
        let ordinary = rustgis_map::Layer::new(99, ordinary);
        assert!(s.constructions.export_notes(s.map().instance_id(), &ordinary).unwrap().is_empty());
    }
    #[test]
    fn box_edit_non_polygon_formats_keep_their_geometry_loss_notices() {
        let (mut s, id) = session();
        add(&mut s, &spec());
        for ext in ["csv", "gpx"] {
            let path = std::env::temp_dir().join(format!("rustgis-box-format-{}.{ext}", std::process::id()));
            s.notices.clear();
            s.run("share.exportLayer", &json!({"layer":id,"path":path})).unwrap();
            let notes: Vec<_> = s.notices.iter().map(|n| n.text.as_str()).collect();
            let construction = notes.iter().find(|n| n.starts_with("1 rotated box")).unwrap();
            assert!(construction.contains("construction is not included"));
            assert!(!construction.contains("exported as ordinary polygons"));
            assert!(notes.iter().any(|n| if ext == "gpx" { n.contains("GPX has no areas") } else { n.contains("point") }));
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn box_edit_delete_targets_one_object_and_row_id_reuse_never_adopts_a_polygon() {
        let (mut s, id) = session();
        let b = spec();
        let first = add(&mut s, &b);
        let second = add(&mut s, &b);
        s.map_mut().layer_mut(id).unwrap().selection = vec![first, second];
        let depth = s.undo_depth();
        s.run("edit.deleteBox", &json!({"row":first})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
        assert!(s.box_spec(id, 0).is_some());
        assert_eq!(s.undo_depth(), depth + 1);
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 2);
        assert!(s.box_spec(id, first).is_some() && s.box_spec(id, second).is_some());
        s.run("edit.delete", &json!({})).unwrap();
        s.run("edit.addFeature", &json!({"geometry":rustgis_data::io::geometry_to_geojson(&b.geometry().unwrap())})).unwrap();
        assert!(s.box_spec(id, 0).is_none());
    }

    #[test]
    fn box_edit_cut_paste_reused_ids_stay_ordinary_and_history_restores_only_originals() {
        let (mut s, id) = session();
        let b = spec();
        let kept = add(&mut s, &b);
        let cut = add(&mut s, &b);
        let original = s.map().layer(id).unwrap().dataset.features[cut].clone();
        s.run("edit.cut", &json!({})).unwrap();
        assert_eq!(s.constructions.len, 1);
        assert_eq!(s.box_spec(id, kept), Some(b.clone()));
        s.run("edit.paste", &json!({"layer":id})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features[cut], original);
        assert!(s.box_spec(id, cut).is_none());
        assert_eq!(s.box_spec(id, kept), Some(b.clone()));
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.box_spec(id, cut), Some(b.clone()));
        s.run("edit.redo", &json!({})).unwrap();
        s.run("edit.redo", &json!({})).unwrap();
        assert!(s.box_spec(id, cut).is_none());
        assert_eq!(s.box_spec(id, kept), Some(b));
    }

    #[test]
    fn box_edit_update_inherits_omitted_image_grid_before_geometry_validation() {
        let (mut s, id) = session();
        let grid = ImageGrid::new(400, 300, Crs::from_epsg(32633).unwrap(), [500_000., 0.8, 0.2, 1000., 0.1, -0.8]).unwrap();
        let b = BoxSpec { frame: Frame::from_grid(&grid).unwrap(), center: [200., 150.], width: 60., height: 30., angle: 17. };
        let row = add(&mut s, &b);
        let mut next = b.clone();
        next.width = 80.;
        next.angle = 43.;
        let mut value = serde_json::to_value(&next).unwrap();
        value.as_object_mut().unwrap().remove("frame");
        s.run("edit.updateBox", &json!({"row":row,"box":value})).unwrap();
        assert_eq!(s.box_spec(id, row), Some(next.clone()));
        assert_eq!(s.map().layer(id).unwrap().dataset.features[row].geometry, Some(next.geometry().unwrap()));
        let depth = s.undo_depth();
        for bad in [json!(Frame::default()), Value::Null, json!({"epsg":0})] {
            value["frame"] = bad;
            assert!(s.run("edit.updateBox", &json!({"row":row,"box":value})).is_err());
            assert_eq!(s.box_spec(id, row), Some(next.clone()));
            assert_eq!(s.undo_depth(), depth);
        }
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.box_spec(id, row), Some(b));
    }

    #[test]
    fn box_edit_interactive_activation_requires_a_flat_view_but_geometry_does_not() {
        let (mut s, id) = session();
        let flat = s.map().projection.clone();
        let b = spec();
        for (projection, pitch) in [(rustgis_map::Projection::Globe, 0.), (flat.clone(), 30.)] {
            s.map_mut().projection = projection;
            s.map_mut().camera.pitch = pitch;
            let error = s.run("tool.set", &json!({"tool":"rotatedBox"})).unwrap_err();
            assert!(error.contains("zero pitch"));
            assert_eq!(s.tool, crate::Tool::Explore);
            let row = add(&mut s, &b);
            assert_eq!(s.map().layer(id).unwrap().dataset.features[row].geometry, Some(b.geometry().unwrap()));
            let mut changed = b.clone();
            changed.angle = 70.;
            s.run("edit.updateBox", &json!({"row":row,"box":changed})).unwrap();
            assert_eq!(s.box_spec(id, row), Some(changed));
            s.run("edit.deleteBox", &json!({"row":row})).unwrap();
            assert!(s.map().layer(id).unwrap().dataset.features.is_empty());
        }
        s.map_mut().projection = flat;
        s.map_mut().camera.pitch = 0.;
        s.run("tool.set", &json!({"tool":"rotatedBox"})).unwrap();
        assert_eq!(s.tool, crate::Tool::RotatedBox);
    }

    #[test]
    fn box_edit_update_refuses_attributes_without_mutation_and_points_to_set_value() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let before = s.map().layer(id).unwrap().dataset.features[row].clone();
        let depth = s.undo_depth();
        let mut changed = b.clone();
        changed.angle = 70.;
        for attributes in [json!({"class":"other"}), json!({}), Value::Null] {
            let error = s.run("edit.updateBox", &json!({"row":row,"box":changed,"attributes":attributes})).unwrap_err();
            assert!(error.contains("edit.setValue"));
            assert_eq!(s.map().layer(id).unwrap().dataset.features[row], before);
            assert_eq!(s.box_spec(id, row), Some(b.clone()));
            assert_eq!(s.undo_depth(), depth);
        }
        s.run("edit.setValue", &json!({"row":row,"field":"class","value":"other"})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.value(row, 0), &rustgis_data::Value::Text("other".into()));
        assert_eq!(s.box_spec(id, row), Some(b));
    }

    #[test]
    fn box_edit_add_refuses_standalone_table_targets_before_intake_or_mutation() {
        let (mut s, id) = session();
        let depth = s.undo_depth();
        for mut p in [json!({"table":"Rows"}), json!({"layer":id,"table":"Rows"})] {
            p["box"] = json!(spec());
            let error = s.run("edit.addBox", &p).unwrap_err();
            assert!(error.contains("polygon layer"), "{error}");
            assert!(s.map().layer(id).unwrap().dataset.features.is_empty());
            assert_eq!(s.undo_depth(), depth);
            assert_eq!(s.constructions.len, 0);
        }
    }

    #[test]
    fn box_edit_rejects_unknown_attributes_and_exhausted_feature_ids() {
        let (mut s, id) = session();
        let b = spec();
        assert!(s.run("edit.addBox", &json!({"box":b,"attributes":{"missing":"lost"}})).is_err());
        assert!(s.map().layer(id).unwrap().dataset.features.is_empty());
        s.map_mut().layer_mut(id).unwrap().dataset.features.push(rustgis_data::Feature {
            id: u64::MAX,
            geometry: Some(b.geometry().unwrap()),
            attrs: vec![],
        });
        assert!(s.run("edit.addBox", &json!({"box":b})).is_err());
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
        assert!(s.box_spec(id, 0).is_none());
    }

    #[test]
    fn box_edit_replacement_sources_and_reopened_projects_do_not_inherit_construction() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let reopened = Session::new(s.project.clone());
        assert!(reopened.box_spec(id, row).is_none());
        let l = s.map_mut().layer_mut(id).unwrap();
        l.touch_replacement();
        assert!(s.box_spec(id, row).is_none());
    }
}
