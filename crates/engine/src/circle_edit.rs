//! Constrained circles on ordinary polygon layers. Parameters live only in the session cache.
//! Each geometry is rebuilt from the frozen grid, centre and radius; no accumulated vertex edits.
use crate::box_edit::Frame;
use crate::construction::{Cache, Construction, Record};
use crate::{CmdResult, Session};
use rustgis_data::Geometry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn default_tolerance() -> f64 {
    0.25
}
/// A circle in planar grid units (image pixels or default Web Mercator grid metres).
/// `tolerance` bounds the sagitta before projection. The transformed straight-edge polygon
/// cannot promise this bound in other coordinate systems. Sampling is bounded to 32..=2048 edges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CircleSpec {
    #[serde(default)]
    pub frame: Frame,
    pub center: [f64; 2],
    pub radius: f64,
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
}
impl CircleSpec {
    pub fn point(&self, angle: f64) -> [f64; 2] {
        [self.center[0] + self.radius * angle.cos(), self.center[1] + self.radius * angle.sin()]
    }
    /// Bounded tessellation; refuse accuracy that cannot be met, rather than silently cap it.
    pub fn segments(&self) -> Result<usize, String> {
        if !self.center.iter().chain([&self.radius, &self.tolerance]).all(|v| v.is_finite() && v.abs() <= 1e12)
            || self.radius < 1e-6
            || self.tolerance <= 0.
        {
            return Err("a circle needs finite coordinates, a positive radius and positive tolerance".into());
        }
        // 2*sin²(theta/2) avoids cancellation for small tolerance/radius ratios.
        for n in [32_usize, 64, 128, 256, 512, 1024, 2048] {
            let error = 2. * self.radius * (std::f64::consts::PI / (2. * n as f64)).sin().powi(2);
            if error <= self.tolerance {
                return Ok(n);
            }
        }
        Err("circle tolerance requires more than 2048 edges; increase tolerance or reduce radius".into())
    }
    pub fn geometry(&self) -> Result<Geometry, String> {
        self.built().map(|b| b.lonlat)
    }
    /// The circle in longitude/latitude, and its vertices in its frame's system.
    pub(crate) fn built(&self) -> Result<crate::construction::Built, String> {
        let n = self.segments()?;
        let (mut ring, mut world) = (Vec::with_capacity(n + 1), Vec::with_capacity(n + 1));
        for i in 0..n {
            let p = self.point(i as f64 * std::f64::consts::TAU / n as f64);
            world.push(self.frame.to_world(p).ok_or("circle vertex is outside its construction grid")?);
            let c = self
                .frame
                .to_lonlat(p)
                .filter(|c| c.is_finite() && c.x.abs() <= 180. && c.y.abs() < 85.05)
                .ok_or("circle crosses an unsupported projection boundary or the polar editing limit")?;
            ring.push(c);
        }
        let span = ring.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max) - ring.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        if span > 180. {
            return Err("circles crossing the antimeridian are not supported".into());
        }
        let mut area = 0.;
        let origin = ring.first().copied().ok_or("missing circle vertex")?;
        for (a, b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
            if a == b {
                return Err("circle collapses after transformation; use a larger circle".into());
            }
            area += (a.x - origin.x) * (b.y - origin.y) - (b.x - origin.x) * (a.y - origin.y);
        }
        if !area.is_finite() || area == 0. {
            return Err("circle collapses after transformation".into());
        }
        Ok(crate::construction::Built::new(&self.frame, ring, world, area < 0.))
    }
}
impl Session {
    /// Imported or independently edited polygons never acquire circle constraints automatically.
    pub fn circle_spec(&self, layer: u32, row: usize) -> Option<CircleSpec> {
        let g = self.map().layer(layer)?.dataset.features.get(row)?.geometry.as_ref()?;
        self.constructions.get(Cache::key(self, layer, row)?, g)?.spec.circle_spec().cloned()
    }
}
// Shared feature intake also supports standalone tables; a circle always targets a layer.
fn reject_table(p: &Value) -> Result<(), String> {
    if p.get("table").is_some_and(|t| !t.is_null()) {
        return Err("circles require a polygon layer; the 'table' parameter is unsupported".into());
    }
    Ok(())
}
pub(crate) fn new_circle_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    reject_table(p)?;
    crate::cmd::design::new_feature_inputs(input, p)
}
pub fn check_tool(s: &Session, id: u32) -> Result<(), String> {
    check_layer(s, id)?;
    if s.map().projection.is_globe() || s.map().camera.pitch != 0. {
        return Err("the Circle tool requires a flat Earth map with zero pitch".into());
    }
    Ok(())
}
fn check_layer(s: &Session, id: u32) -> Result<(), String> {
    if s.map().body != "earth" {
        return Err("circles currently require an Earth map".into());
    }
    crate::box_edit::check_layer(s, id)
}
fn parsed(p: &Value, previous_frame: Option<&Frame>) -> Result<CircleSpec, String> {
    let value = p.get("circle").ok_or("circle is required")?;
    let mut b: CircleSpec = CircleSpec::deserialize(value).map_err(|e| e.to_string())?;
    if value.get("frame").is_none()
        && let Some(frame) = previous_frame
    {
        b.frame = frame.clone();
    }
    b.geometry()?;
    Ok(b)
}
fn row(p: &Value) -> Result<usize, String> {
    p.get("row").and_then(Value::as_u64).and_then(|r| usize::try_from(r).ok()).ok_or_else(|| "row is required".into())
}
/// Synchronous: a single feature, with sampling capped at 2048 edges.
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
    let result = b.built()?.add(s, id, p)?;
    let row = result.get("row").and_then(Value::as_u64).and_then(|r| usize::try_from(r).ok()).ok_or("circle was not added")?;
    remember(s, id, row, b.clone())?;
    Ok(
        json!({"row":row,"circle":b,"polygonEdges":b.segments()?,"approximation":"straight edges; tolerance is in construction-grid units before projection"}),
    )
}
pub(crate) fn remember(s: &mut Session, id: u32, row: usize, spec: CircleSpec) -> Result<(), String> {
    let key = Cache::key(s, id, row).ok_or("circle source changed")?;
    if key.3 == u64::MAX {
        return Err("feature IDs have reached their limit; create a fresh layer for circles".into());
    }
    let g = s.map().layer(id).and_then(|l| l.dataset.features.get(row)?.geometry.clone()).ok_or("circle geometry is missing")?;
    if !spec.built()?.as_stored(s, id, &g)? {
        return Err("an edit rule changed the circle geometry".into());
    }
    s.constructions.remember(key, Record { spec: Construction::Circle(spec), geometry: g })
}
/// Synchronous: a single feature, with sampling capped at 2048 edges.
pub(crate) fn update(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    if p.get("attributes").is_some() {
        return Err("edit.updateCircle changes the shape only; set attributes with edit.setValue".into());
    }
    let row = row(p)?;
    let key = Cache::key(s, id, row).ok_or("no such feature")?;
    let g =
        s.map().layer(id).and_then(|l| l.dataset.features.get(row)).and_then(|f| f.geometry.as_ref()).ok_or("missing circle geometry")?;
    let before = s
        .constructions
        .get(key, g)
        .and_then(|r| r.spec.circle_spec().cloned())
        .ok_or("this feature has no unchanged circle construction; use Edit Vertices")?;
    if p.get("featureId").is_some_and(|v| v.as_u64() != Some(key.3)) {
        return Err("the feature changed during this edit".into());
    }
    let b = parsed(p, Some(&before.frame))?;
    if b.frame != before.frame {
        return Err("a circle's construction grid is frozen; start another circle to change grids".into());
    }
    b.built()?.replace(s, id, row)?;
    remember(s, id, row, b.clone())?;
    Ok(
        json!({"row":row,"circle":b,"polygonEdges":b.segments()?,"approximation":"straight edges; tolerance is in construction-grid units before projection"}),
    )
}
pub(crate) fn delete_circle(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let row = row(p)?;
    let key = Cache::key(s, id, row).ok_or("no such feature")?;
    if s.circle_spec(id, row).is_none() {
        return Err("this feature has no unchanged circle construction".into());
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
    let b = s.circle_spec(id, row).ok_or("this feature has no unchanged circle construction")?;
    Ok(
        json!({"row":row,"circle":b,"polygonEdges":b.segments()?,"approximation":"straight edges; tolerance is in construction-grid units before projection"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::box_edit::Frame;
    use rustgis_data::raster::{Crs, ImageGrid};
    use rustgis_data::{Dataset, Field, FieldType, GeometryType};
    use rustgis_geom::Coord;
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
    fn spec() -> CircleSpec {
        CircleSpec { frame: Frame::default(), center: [1000., 1000.], radius: 100., tolerance: 0.25 }
    }
    fn add(s: &mut Session, b: &CircleSpec) -> usize {
        s.run("edit.addCircle", &json!({"circle":b,"attributes":{"class":"vehicle","_rustgis_box":"user-owned"}})).unwrap()["row"]
            .as_u64()
            .unwrap() as usize
    }
    #[test]
    fn circle_edit_preserves_attributes_identity_and_construction_through_history() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let original = s.map().layer(id).unwrap().dataset.features[row].clone();
        let fields = s.map().layer(id).unwrap().dataset.fields.clone();
        let mut next = b.clone();
        next.radius = 112.;
        next.tolerance = 0.1;
        s.run("edit.updateCircle", &json!({"row":row,"featureId":original.id,"circle":next})).unwrap();
        let after = &s.map().layer(id).unwrap().dataset.features[row];
        assert_eq!(after.id, original.id);
        assert_eq!(after.attrs, original.attrs);
        assert_eq!(s.map().layer(id).unwrap().dataset.fields, fields);
        assert_eq!(s.circle_spec(id, row), Some(next.clone()));
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.circle_spec(id, row), Some(b.clone()));
        assert_eq!(s.map().layer(id).unwrap().dataset.features[row].geometry, original.geometry);
        s.run("edit.redo", &json!({})).unwrap();
        assert_eq!(s.circle_spec(id, row), Some(next));
        s.run("edit.delete", &json!({})).unwrap();
        assert!(s.circle_spec(id, row).is_none());
        s.run("edit.undo", &json!({})).unwrap();
        assert!(s.circle_spec(id, row).is_some());
    }
    #[test]
    fn circle_edit_foreign_geometry_changes_disable_handles_and_undo_restores_them() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let changed = Geometry::Polygon(vec![vec![vec![Coord::new(0., 0.), Coord::new(1., 0.), Coord::new(0., 1.), Coord::new(0., 0.)]]]);
        s.run("edit.setGeometry", &json!({"row":row,"geometry":rustgis_data::io::geometry_to_geojson(&changed)})).unwrap();
        assert!(s.circle_spec(id, row).is_none());
        assert!(s.run("edit.updateCircle", &json!({"row":row,"circle":b})).is_err());
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.circle_spec(id, row), Some(b));
    }
    #[test]
    fn circle_edit_delete_targets_one_object_and_row_id_reuse_never_adopts_a_polygon() {
        let (mut s, id) = session();
        let b = spec();
        let first = add(&mut s, &b);
        let second = add(&mut s, &b);
        s.map_mut().layer_mut(id).unwrap().selection = vec![first, second];
        let depth = s.undo_depth();
        s.run("edit.deleteCircle", &json!({"row":first})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
        assert!(s.circle_spec(id, 0).is_some());
        assert_eq!(s.undo_depth(), depth + 1);
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 2);
        assert!(s.circle_spec(id, first).is_some() && s.circle_spec(id, second).is_some());
        s.run("edit.delete", &json!({})).unwrap();
        s.run("edit.addFeature", &json!({"geometry":rustgis_data::io::geometry_to_geojson(&b.geometry().unwrap())})).unwrap();
        assert!(s.circle_spec(id, 0).is_none());
    }

    #[test]
    fn circle_edit_cut_paste_reused_ids_stay_ordinary_and_history_restores_only_originals() {
        let (mut s, id) = session();
        let b = spec();
        let kept = add(&mut s, &b);
        let cut = add(&mut s, &b);
        let original = s.map().layer(id).unwrap().dataset.features[cut].clone();
        s.run("edit.cut", &json!({})).unwrap();
        assert_eq!(s.circle_spec(id, kept), Some(b.clone()));
        s.run("edit.paste", &json!({"layer":id})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features[cut], original);
        assert!(s.circle_spec(id, cut).is_none());
        assert_eq!(s.circle_spec(id, kept), Some(b.clone()));
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.circle_spec(id, cut), Some(b.clone()));
        s.run("edit.redo", &json!({})).unwrap();
        s.run("edit.redo", &json!({})).unwrap();
        assert!(s.circle_spec(id, cut).is_none());
        assert_eq!(s.circle_spec(id, kept), Some(b));
    }

    #[test]
    fn circle_edit_update_inherits_omitted_image_grid_before_geometry_validation() {
        let (mut s, id) = session();
        let grid = ImageGrid::new(400, 300, Crs::from_epsg(32633).unwrap(), [500_000., 0.8, 0.2, 1000., 0.1, -0.8]).unwrap();
        let b = CircleSpec { frame: Frame::from_grid(&grid).unwrap(), center: [200., 150.], radius: 30., tolerance: 0.25 };
        let row = add(&mut s, &b);
        let mut next = b.clone();
        next.radius = 80.;
        next.tolerance = 0.1;
        let mut value = serde_json::to_value(&next).unwrap();
        value.as_object_mut().unwrap().remove("frame");
        s.run("edit.updateCircle", &json!({"row":row,"circle":value})).unwrap();
        assert_eq!(s.circle_spec(id, row), Some(next.clone()));
        assert_eq!(s.map().layer(id).unwrap().dataset.features[row].geometry, Some(next.geometry().unwrap()));
        let depth = s.undo_depth();
        for bad in [json!(Frame::default()), Value::Null, json!({"epsg":0})] {
            value["frame"] = bad;
            assert!(s.run("edit.updateCircle", &json!({"row":row,"circle":value})).is_err());
            assert_eq!(s.circle_spec(id, row), Some(next.clone()));
            assert_eq!(s.undo_depth(), depth);
        }
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.circle_spec(id, row), Some(b));
    }

    #[test]
    fn circle_edit_interactive_activation_requires_a_flat_view_but_geometry_does_not() {
        let (mut s, id) = session();
        let flat = s.map().projection.clone();
        let b = spec();
        for (projection, pitch) in [(rustgis_map::Projection::Globe, 0.), (flat.clone(), 30.)] {
            s.map_mut().projection = projection;
            s.map_mut().camera.pitch = pitch;
            let error = s.run("tool.set", &json!({"tool":"circle"})).unwrap_err();
            assert!(error.contains("zero pitch"));
            assert_eq!(s.tool, crate::Tool::Explore);
            let row = add(&mut s, &b);
            assert_eq!(s.map().layer(id).unwrap().dataset.features[row].geometry, Some(b.geometry().unwrap()));
            let mut changed = b.clone();
            changed.radius = 70.;
            s.run("edit.updateCircle", &json!({"row":row,"circle":changed})).unwrap();
            assert_eq!(s.circle_spec(id, row), Some(changed));
            s.run("edit.deleteCircle", &json!({"row":row})).unwrap();
            assert!(s.map().layer(id).unwrap().dataset.features.is_empty());
        }
        s.map_mut().projection = flat;
        s.map_mut().camera.pitch = 0.;
        s.run("tool.set", &json!({"tool":"circle"})).unwrap();
        assert_eq!(s.tool, crate::Tool::Circle);
    }

    #[test]
    fn circle_edit_update_refuses_attributes_without_mutation_and_points_to_set_value() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let before = s.map().layer(id).unwrap().dataset.features[row].clone();
        let depth = s.undo_depth();
        let mut changed = b.clone();
        changed.radius = 70.;
        for attributes in [json!({"class":"other"}), json!({}), Value::Null] {
            let error = s.run("edit.updateCircle", &json!({"row":row,"circle":changed,"attributes":attributes})).unwrap_err();
            assert!(error.contains("edit.setValue"));
            assert_eq!(s.map().layer(id).unwrap().dataset.features[row], before);
            assert_eq!(s.circle_spec(id, row), Some(b.clone()));
            assert_eq!(s.undo_depth(), depth);
        }
        s.run("edit.setValue", &json!({"row":row,"field":"class","value":"other"})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.value(row, 0), &rustgis_data::Value::Text("other".into()));
        assert_eq!(s.circle_spec(id, row), Some(b));
    }

    #[test]
    fn circle_edit_add_refuses_standalone_table_targets_before_intake_or_mutation() {
        let (mut s, id) = session();
        let depth = s.undo_depth();
        for mut p in [json!({"table":"Rows"}), json!({"layer":id,"table":"Rows"})] {
            p["circle"] = json!(spec());
            let error = s.run("edit.addCircle", &p).unwrap_err();
            assert!(error.contains("polygon layer"), "{error}");
            assert!(s.map().layer(id).unwrap().dataset.features.is_empty());
            assert_eq!(s.undo_depth(), depth);
        }
    }

    #[test]
    fn circle_edit_rejects_unknown_attributes_and_exhausted_feature_ids() {
        let (mut s, id) = session();
        let b = spec();
        assert!(s.run("edit.addCircle", &json!({"circle":b,"attributes":{"missing":"lost"}})).is_err());
        assert!(s.map().layer(id).unwrap().dataset.features.is_empty());
        s.map_mut().layer_mut(id).unwrap().dataset.features.push(rustgis_data::Feature {
            id: u64::MAX,
            geometry: Some(b.geometry().unwrap()),
            attrs: vec![],
        });
        assert!(s.run("edit.addCircle", &json!({"circle":b})).is_err());
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
        assert!(s.circle_spec(id, 0).is_none());
    }

    #[test]
    fn circle_edit_replacement_sources_and_reopened_projects_do_not_inherit_construction() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let reopened = Session::new(s.project.clone());
        assert!(reopened.circle_spec(id, row).is_none());
        let l = s.map_mut().layer_mut(id).unwrap();
        l.touch_replacement();
        assert!(s.circle_spec(id, row).is_none());
    }
    #[test]
    fn circle_edit_affine_radius_tolerance_and_winding() {
        let grids = [
            Frame::default(),
            Frame::from_grid(&ImageGrid::new(400, 300, Crs::from_epsg(32633).unwrap(), [500000., 0.8, 0.2, 1000., 0.1, -0.8]).unwrap())
                .unwrap(),
        ];
        for frame in grids {
            for radius in [0.01, 30., 1000., 100000.] {
                let b = CircleSpec { frame: frame.clone(), center: [200., 150.], radius, tolerance: 0.25 };
                let Geometry::Polygon(polys) = b.geometry().unwrap() else { panic!("polygon") };
                let ring = &polys[0][0];
                let n = b.segments().unwrap();
                assert_eq!(ring.len(), n + 1);
                assert_eq!(ring.first(), ring.last());
                let mut area = 0.;
                for pair in ring.windows(2) {
                    area += pair[0].x * pair[1].y - pair[1].x * pair[0].y;
                    let a = frame.to_local(pair[0]).unwrap();
                    let c = frame.to_local(pair[1]).unwrap();
                    assert!(((a[0] - 200.).hypot(a[1] - 150.) - radius).abs() < 1e-5);
                    let midpoint = [(a[0] + c[0]) / 2., (a[1] + c[1]) / 2.];
                    let sagitta = radius - (midpoint[0] - 200.).hypot(midpoint[1] - 150.);
                    assert!(sagitta <= b.tolerance + 1e-5);
                }
                assert!(area > 0.);
            }
        }
        let mut b = spec();
        b.tolerance = 1e-20;
        assert!(b.geometry().unwrap_err().contains("2048"));
        b.radius = f64::NAN;
        assert!(b.geometry().is_err());
        b.radius = f64::INFINITY;
        assert!(b.geometry().is_err());
    }

    #[test]
    fn circle_edit_bad_parameters_and_stale_ids_are_atomic() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        let before = s.map().layer(id).unwrap().dataset.features[row].clone();
        let depth = s.undo_depth();
        for patch in [
            json!({"radius":0}),
            json!({"radius":-1}),
            json!({"radius":"NaN"}),
            json!({"tolerance":0}),
            json!({"tolerance":1e-30}),
            json!({"center":[1e99,0]}),
            json!({"frame":{"affine":[0,0,0,0,0,0]}}),
            json!({"frame":{"epsg":0}}),
            json!({"unexpected":true}),
        ] {
            let mut bad = json!(b);
            for (k, v) in patch.as_object().unwrap() {
                bad[k] = v.clone();
            }
            assert!(s.run("edit.updateCircle", &json!({"row":row,"circle":bad})).is_err());
            assert!(s.run("edit.addCircle", &json!({"circle":bad})).is_err());
            assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
            assert_eq!(s.map().layer(id).unwrap().dataset.features[row], before);
            assert_eq!(s.circle_spec(id, row), Some(b.clone()));
            assert_eq!(s.undo_depth(), depth);
        }
        for cmd in ["edit.updateCircle", "edit.deleteCircle"] {
            assert!(s.run(cmd, &json!({"row":row,"featureId":before.id+1,"circle":b})).is_err());
            assert_eq!(s.undo_depth(), depth);
        }
        let mut polar = spec();
        polar.center = [0., -30_000_000.];
        assert!(polar.geometry().is_err());
        let mut crossing = spec();
        crossing.center = [20_037_500., 0.];
        crossing.radius = 1000.;
        assert!(crossing.geometry().is_err());
    }

    #[test]
    fn circle_edit_geojson_is_plain_and_export_counts_each_shape_kind() {
        let (mut s, id) = session();
        let b = spec();
        add(&mut s, &b);
        add(&mut s, &b);
        s.run("edit.addBox", &json!({"box":{"center":[0,0],"width":100,"height":200,"angle":0}})).unwrap();
        assert!(s.circle_spec(id, 2).is_none());
        assert!(s.box_spec(id, 0).is_none());
        let cache = s.constructions.clone();
        let layer = s.map().layer(id).unwrap().clone();
        let map = s.map().instance_id();
        let mut changed = b.clone();
        changed.radius = 42.;
        s.run("edit.updateCircle", &json!({"row":0,"circle":changed})).unwrap();
        assert!(cache.export_notes(map, &layer).unwrap().iter().any(|n| n.starts_with("2 circle")));
        for ext in ["geojson", "csv", "gpx"] {
            let path = std::env::temp_dir().join(format!("rustgis-circle-{}.{ext}", std::process::id()));
            s.notices.clear();
            s.run("share.exportLayer", &json!({"layer":id,"path":path})).unwrap();
            assert_eq!(s.notices.iter().filter(|n| n.text.starts_with("2 circle")).count(), 1);
            assert_eq!(s.notices.iter().filter(|n| n.text.starts_with("1 rotated box")).count(), 1);
            if ext == "geojson" {
                let text = std::fs::read_to_string(&path).unwrap();
                let j: Value = serde_json::from_str(&text).unwrap();
                assert!(j.get("rustgis_annotation").is_none());
                let props = j["features"][0]["properties"].as_object().unwrap();
                assert_eq!(props.len(), 2);
                assert_eq!(props["class"], "vehicle");
                let (ds, report) = rustgis_data::io::read_geojson(&text, "reopened").unwrap();
                assert_eq!(report.skipped, 0);
                assert_eq!(ds.features.len(), 3);
                let mut reopened = Session::new(crate::Project::default());
                let other = reopened.map_mut().add_dataset(ds);
                assert!(reopened.circle_spec(other, 0).is_none());
                assert!(
                    reopened
                        .constructions
                        .export_notes(reopened.map().instance_id(), reopened.map().layer(other).unwrap())
                        .unwrap()
                        .is_empty()
                );
            } else {
                assert!(s.notices.iter().any(|n| if ext == "gpx" {
                    n.text.contains("GPX has no areas")
                } else {
                    n.text.contains("point")
                }));
            }
            let _ = std::fs::remove_file(path);
        }
    }
    #[test]
    fn circle_edit_non_earth_errors_name_the_circle_tool_without_mutation() {
        let (mut s, id) = session();
        let b = spec();
        let row = add(&mut s, &b);
        s.map_mut().body = "moon".into();
        let depth = s.undo_depth();
        for (cmd, params) in [
            ("edit.addCircle", json!({"circle":b})),
            ("edit.updateCircle", json!({"row":row,"circle":b})),
            ("edit.deleteCircle", json!({"row":row})),
            ("tool.set", json!({"tool":"circle"})),
        ] {
            let error = s.run(cmd, &params).unwrap_err();
            assert_eq!(error, "circles currently require an Earth map");
            assert_eq!(s.circle_spec(id, row), Some(b.clone()));
            assert_eq!(s.undo_depth(), depth);
        }
    }

    fn construction_notices(s: &Session) -> Vec<&str> {
        s.notices
            .iter()
            .map(|n| n.text.as_str())
            .filter(|n| n.contains("session-only") && n.contains("construction is not included"))
            .collect()
    }

    #[test]
    fn circle_edit_source_id_reassignment_preserves_both_constructions_and_history() {
        use rustgis_map::source::{PostgisEdit, PostgisOrigin, RowVersion, row_hash};
        for circle in [true, false] {
            let (mut s, id) = session();
            let b = spec();
            let bx = crate::box_edit::BoxSpec { frame: Frame::default(), center: [0., 0.], width: 100., height: 80., angle: 25. };
            for _ in 0..2 {
                s.run("edit.addFeature", &json!({"geometry":rustgis_data::io::geometry_to_geojson(&b.geometry().unwrap())})).unwrap();
            }
            let layer = s.map_mut().layer_mut(id).unwrap();
            let highest = layer.dataset.features[1].id;
            layer.postgis = Some(PostgisEdit {
                origin: PostgisOrigin {
                    database: "postgres:host=127.0.0.1 port=1 dbname=gis".into(),
                    table: "polygons".into(),
                    family: None,
                    crs: None,
                },
                key: "fid".into(),
                srid: 4326,
                writable: true,
                condition: None,
                limit: 0,
                versions: layer.dataset.features.iter().map(|f| (f.id, RowVersion { version: 1, hash: row_hash(f) })).collect(),
            });
            layer.selection = vec![1];
            s.run("edit.delete", &json!({})).unwrap();
            let depth = s.undo_depth();
            let (command, params) = if circle { ("edit.addCircle", json!({"circle":b})) } else { ("edit.addBox", json!({"box":bx})) };
            s.run(command, &params).unwrap();
            let final_id = s.map().layer(id).unwrap().dataset.features[1].id;
            assert!(final_id > highest);
            assert_eq!(s.constructions.len, 1);
            assert_eq!(s.undo_depth(), depth + 1);
            assert_eq!(s.circle_spec(id, 1), circle.then(|| b.clone()));
            assert_eq!(s.box_spec(id, 1), (!circle).then(|| bx.clone()));
            s.run("edit.undo", &json!({})).unwrap();
            assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
            assert_eq!(s.constructions.len, 0);
            s.run("edit.redo", &json!({})).unwrap();
            assert_eq!(s.map().layer(id).unwrap().dataset.features[1].id, final_id);
            if circle {
                s.run("edit.updateCircle", &json!({"row":1,"featureId":final_id,"circle":{"center":[1000,1000],"radius":120}})).unwrap();
                assert_eq!(s.circle_spec(id, 1).unwrap().radius, 120.);
            } else {
                assert_eq!(s.box_spec(id, 1), Some(bx));
            }
        }
    }

    #[test]
    fn circle_edit_mesh_losses_count_only_contributing_features() {
        let (mut s, id) = session();
        let b = spec();
        s.run("edit.addCircle", &json!({"circle":b,"attributes":{"class":"hidden"}})).unwrap();
        s.run("edit.addCircle", &json!({"circle":b,"attributes":{"class":"flat"}})).unwrap();
        s.run("edit.addBox", &json!({"box":{"center":[0,0],"width":100,"height":80,"angle":0},"attributes":{"class":"solid"}})).unwrap();
        s.run("edit.addField", &json!({"name":"h","type":"Float"})).unwrap();
        for (row, height) in [(0, 10), (1, 0), (2, 20)] {
            s.run("edit.setValue", &json!({"row":row,"field":"h","value":height})).unwrap();
        }
        s.map_mut().layer_mut(id).unwrap().definition_query = Some("class <> 'hidden'".into());
        for (kind, circles) in [("obj", 1), ("stl", 0)] {
            let p = json!({"layer":id,"heightField":"h"});
            s.notices.clear();
            let (_, bytes) = crate::cmd::items::item_bytes(&mut s, kind, &p).unwrap();
            assert!(!bytes.is_empty());
            let notes = construction_notices(&s);
            assert_eq!(notes.len(), 1 + circles, "{kind}: {notes:?}");
            assert!(notes.iter().any(|n| n.starts_with("1 rotated box")));
            assert_eq!(notes.iter().filter(|n| n.starts_with("1 circle")).count(), circles);
            let path = std::env::temp_dir().join(format!("rustgis-circle-mesh-{}.{kind}", std::process::id()));
            s.notices.clear();
            let mut p = p;
            p["path"] = json!(path);
            s.run("share.export3d", &p).unwrap();
            assert_eq!(construction_notices(&s).len(), 1 + circles);
            let _ = std::fs::remove_file(path);
        }
        // With only an ordinary solid retained, a successful mesh has no construction losses.
        s.run(
            "edit.addFeature",
            &json!({"geometry":rustgis_data::io::geometry_to_geojson(&b.geometry().unwrap()),"attributes":{"class":"ordinary","h":20}}),
        )
        .unwrap();
        s.map_mut().layer_mut(id).unwrap().definition_query = Some("class = 'ordinary'".into());
        for kind in ["obj", "stl"] {
            s.notices.clear();
            crate::cmd::items::item_bytes(&mut s, kind, &json!({"heightField":"h"})).unwrap();
            assert!(construction_notices(&s).is_empty());
        }
    }

    #[test]
    fn circle_edit_explicit_project_save_reports_live_constructions_across_maps() {
        let (mut s, first) = session();
        let b = spec();
        add(&mut s, &b);
        s.run("edit.addBox", &json!({"box":{"center":[0,0],"width":100,"height":200,"angle":0}})).unwrap();
        let mut other = s.map().clone();
        other.name = "Other map".into();
        other.renew_instance_id();
        s.project.maps.push(other);
        s.project.active_map = 1;
        // The cloned polygons are ordinary: their new map identity must not adopt construction.
        assert!(s.circle_spec(first, 0).is_none());
        add(&mut s, &b);
        let path = std::env::temp_dir().join(format!("rustgis-circle-save-{}.rgis", std::process::id()));
        for cmd in ["project.saveAs", "project.save"] {
            s.notices.clear();
            s.run(cmd, &json!({"path":path})).unwrap();
            let notes = construction_notices(&s);
            assert_eq!(notes.len(), 2);
            assert!(notes.iter().any(|n| n.starts_with("2 circle")));
            assert!(notes.iter().any(|n| n.starts_with("1 rotated box")));
            assert_eq!(s.circle_spec(first, 2), Some(b.clone()));
        }
        let mut reopened = Session::new(crate::Project::default());
        reopened.run("project.open", &json!({"path":path})).unwrap();
        assert!(reopened.circle_spec(first, 2).is_none());
        assert!(reopened.construction_output_notes().unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn circle_edit_item_outputs_count_only_their_source_and_ordinary_polygons_stay_quiet() {
        let (mut s, id) = session();
        let b = spec();
        add(&mut s, &b);
        let second = s.map_mut().add_dataset(Dataset::new("Ordinary", GeometryType::Polygon, vec![]));
        s.run("edit.addFeature", &json!({"layer":second,"geometry":rustgis_data::io::geometry_to_geojson(&b.geometry().unwrap())}))
            .unwrap();
        for kind in ["layer", "map", "projectPackage", "mapPackage", "mobilePackage", "template", "webLayer", "webMap", "geodataService"] {
            s.active_layer = Some(id);
            s.notices.clear();
            crate::cmd::items::item_bytes(&mut s, kind, &json!({"layer":id})).unwrap();
            let notes = construction_notices(&s);
            assert_eq!(notes.len(), 1, "{kind}: {notes:?}");
            assert!(notes[0].starts_with("1 circle"), "{kind}");
            assert_eq!(s.circle_spec(id, 0), Some(b.clone()));
        }
        for kind in ["layer", "webLayer"] {
            s.notices.clear();
            crate::cmd::items::item_bytes(&mut s, kind, &json!({"layer":second})).unwrap();
            assert!(construction_notices(&s).is_empty(), "{kind}");
        }
        s.notices.clear();
        assert!(crate::cmd::items::item_bytes(&mut s, "map", &json!({"map":"missing"})).is_err());
        assert!(construction_notices(&s).is_empty());
        let mut ordinary = Session::new(s.project.clone());
        let path = std::env::temp_dir().join(format!("rustgis-circle-clean-save-{}.rgis", std::process::id()));
        ordinary.run("project.saveAs", &json!({"path":path})).unwrap();
        assert!(construction_notices(&ordinary).is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn circle_edit_conversion_notices_cover_text_and_background_file_output() {
        let (mut s, id) = session();
        let b = spec();
        add(&mut s, &b);
        s.notices.clear();
        let result = s.run("data.convert", &json!({"layer":id,"format":"geojson"})).unwrap();
        assert!(result["text"].as_str().unwrap().contains("Polygon"));
        assert_eq!(construction_notices(&s).len(), 1);
        let path = std::env::temp_dir().join(format!("rustgis-circle-convert-{}.geojson", std::process::id()));
        let work = crate::cmd::workflows::convert_work(&s, &json!({"layer":id,"format":"geojson","path":path})).unwrap();
        // A later live edit cannot change the worker's frozen source/cache snapshot.
        s.run("edit.deleteCircle", &json!({"row":0})).unwrap();
        let wrote = work().unwrap();
        assert_eq!(wrote.notes.iter().filter(|n| n.starts_with("1 circle")).count(), 1);
        assert!(s.construction_output_notes().unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn circle_edit_mobile_package_counts_only_retained_layers_after_group_visibility() {
        let (mut s, hidden) = session();
        let b = spec();
        add(&mut s, &b);
        let visible = s.map_mut().add_dataset(Dataset::new("Visible circles", GeometryType::Polygon, vec![]));
        s.run("edit.addCircle", &json!({"layer":visible,"circle":b})).unwrap();
        s.run("edit.addBox", &json!({"layer":visible,"box":{"center":[0,0],"width":100,"height":80,"angle":0}})).unwrap();
        let group = s.run("group.add", &json!({"name":"Hidden group","layers":[hidden]})).unwrap()["group"].clone();
        s.run("group.set", &json!({"group":group,"visible":false})).unwrap();
        assert!(s.map().layer(hidden).unwrap().visible, "visibility is inherited from the group");
        for (kind, circles) in [("mobilePackage", 1), ("mapPackage", 2)] {
            s.notices.clear();
            let (_, bytes) = crate::cmd::items::item_bytes(&mut s, kind, &json!({"tolerance":0})).unwrap();
            let notes = construction_notices(&s);
            assert_eq!(notes.len(), 2, "{kind}");
            assert!(notes.iter().any(|n| n.starts_with(&format!("{circles} circle"))), "{kind}: {notes:?}");
            assert!(notes.iter().any(|n| n.starts_with("1 rotated box")));
            if kind == "mobilePackage" {
                let entries = rustgis_data::zip::read_matching(&bytes, |name| name.ends_with(".rgismap")).unwrap();
                let map: rustgis_map::Map = serde_json::from_slice(&entries[0].1).unwrap();
                assert!(map.layer(hidden).is_none());
                assert!(map.layer(visible).is_some());
            }
        }
        s.map_mut().layer_mut(visible).unwrap().visible = false;
        s.notices.clear();
        crate::cmd::items::item_bytes(&mut s, "mobilePackage", &json!({"tolerance":0})).unwrap();
        assert!(construction_notices(&s).is_empty());
    }

    #[test]
    fn circle_edit_committed_construction_notices_survive_later_workflow_rollback() {
        let (mut s, id) = session();
        let b = spec();
        add(&mut s, &b);
        for (command, extension) in
            [("project.saveAs", "rgis"), ("layer.save", "rgislayer"), ("share.export3d", "obj"), ("share.export3d", "stl")]
        {
            let path = std::env::temp_dir().join(format!("rustgis-circle-written-{}.{extension}", std::process::id()));
            s.notices.clear();
            let script = format!("{command} {}\nlayer.remove {{\"layer\":\"missing\"}}", json!({"layer":id,"path":path}));
            assert!(s.run("script.run", &json!({"script":script})).is_err());
            assert!(path.exists(), "{command}");
            let notes = construction_notices(&s);
            assert_eq!(notes.len(), 1, "{command}: {notes:?}");
            assert!(notes[0].starts_with("1 circle"));
            assert_eq!(s.circle_spec(id, 0), Some(b.clone()));
            let _ = std::fs::remove_file(path);
        }
    }

    /// A circle built in its layer's own grid is written as built: every vertex the grid's place
    /// in the layer's coordinates, exactly, at the radius from the centre.
    #[test]
    fn a_circle_in_the_layers_own_grid_is_written_as_built() {
        let mut ds = Dataset::new("Lots", GeometryType::Polygon, vec![Field::new("class", FieldType::Text)]);
        ds.hold_in(Crs::Epsg(32632)).unwrap();
        let mut s = Session::new(crate::Project::default());
        let id = s.map_mut().add_dataset(ds);
        s.active_layer = Some(id);
        s.run("edit.enable", &json!({"on":true})).unwrap();
        let c = CircleSpec { frame: Frame::for_crs(Crs::Epsg(32632)), center: [684_000., -5_397_000.], radius: 50., tolerance: 0.05 };
        let row = s.run("edit.addCircle", &json!({"circle":c})).unwrap()["row"].as_u64().unwrap() as usize;
        let mut got = Vec::new();
        s.map().layer(id).unwrap().dataset.features[row].geometry.as_ref().unwrap().for_each_coord(|p| got.push(p));
        assert_eq!(got.len(), c.segments().unwrap() + 1);
        for p in &got {
            assert!(((p.x - 684_000.).hypot(p.y - 5_397_000.) - 50.).abs() < 1e-9, "{p:?}");
        }
        assert_eq!(s.circle_spec(id, row), Some(c));
    }
}
