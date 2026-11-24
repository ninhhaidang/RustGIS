//! Explicit fitting of one ordinary polygon. Previews run on the desktop read queue; Apply
//! replaces one bounded feature through normal edit rules. No construction is saved in files.
use crate::box_edit::{BoxSpec, Frame};
use crate::circle_edit::CircleSpec;
use crate::{CmdResult, Session};
use rustgis_data::{Dataset, Geometry, progress};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_VERTICES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "dimensions", rename_all = "camelCase", deny_unknown_fields)]
pub enum Shape {
    Circle(CircleSpec),
    RotatedBox(BoxSpec),
}
impl Shape {
    pub fn geometry(&self) -> Result<Geometry, String> {
        match self {
            Self::Circle(s) => s.geometry(),
            Self::RotatedBox(s) => s.geometry(),
        }
    }
}
/// An ephemeral preview, bound to this source instance, revision and exact geometry. Not a file
/// format or a recovered construction: callers explicitly apply the proposed replacement.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proposal {
    #[serde(default)]
    pub request: u64,
    pub layer: u32,
    pub row: usize,
    pub feature_id: u64,
    pub map_instance: u64,
    pub layer_instance: u64,
    pub replacement: u64,
    pub revision: u64,
    source_geometry: Geometry,
    pub shape: Shape,
}
impl Proposal {
    pub fn current(&self, s: &Session) -> bool {
        // Undo/redo can reuse a geometry revision. Comparing the bounded source snapshot also
        // prevents an old preview from overwriting different geometry at the same revision.
        polygon(&self.source_geometry).is_ok()
            && s.map().instance_id() == self.map_instance
            && s.map().layer(self.layer).is_some_and(|l| {
                l.instance_id() == self.layer_instance
                    && l.replacement_revision() == self.replacement
                    && l.geometry_rev == self.revision
                    && l.dataset
                        .features
                        .get(self.row)
                        .is_some_and(|f| f.id == self.feature_id && f.geometry.as_ref() == Some(&self.source_geometry))
            })
    }
}
fn polygon(g: &Geometry) -> Result<&[rustgis_geom::Coord], String> {
    let Geometry::Polygon(polys) = g else { return Err("conversion requires a polygon".into()) };
    if polys.len() != 1 || polys.first().is_none_or(|p| p.len() != 1) {
        return Err("conversion requires one polygon without holes; multipart features and holes are not discarded".into());
    }
    let ring = polys.first().and_then(|p| p.first()).ok_or("missing polygon ring")?;
    if ring.len() < 4 || ring.first() != ring.last() || ring.len() > MAX_VERTICES + 1 {
        return Err("conversion needs a closed polygon with 3 to 4096 vertices; simplify larger polygons explicitly first".into());
    }
    Ok(ring.get(..ring.len() - 1).unwrap_or_default())
}
fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}
/// Andrew's monotone-chain hull (1979), followed by an exhaustive hull-edge bounding rectangle
/// search. Bounded to 4096 input vertices; each edge checks Cancel and counts its progress.
fn hull(mut points: Vec<[f64; 2]>) -> Result<Vec<[f64; 2]>, String> {
    progress::check().map_err(|e| e.to_string())?;
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup();
    let mut out = Vec::new();
    for chain in [points.clone(), points.into_iter().rev().collect()] {
        let start = out.len();
        for p in chain {
            while out.len() >= start + 2 && cross(out[out.len() - 2], out[out.len() - 1], p) <= 0. {
                out.pop();
            }
            out.push(p);
        }
        out.pop();
    }
    if out.len() < 3 {
        return Err("polygon is degenerate in the chosen construction grid".into());
    }
    Ok(out)
}
pub fn fit(g: &Geometry, frame: Frame, kind: &str) -> Result<Shape, String> {
    let ring = polygon(g)?;
    progress::begin(ring.len());
    let mut points = Vec::with_capacity(ring.len());
    let (mut west, mut east) = (f64::INFINITY, f64::NEG_INFINITY);
    for c in ring {
        progress::step(1).map_err(|e| e.to_string())?;
        if !c.is_finite() || c.x.abs() > 180. || c.y.abs() >= 85.05 {
            return Err("polygon crosses the supported Earth editing limits".into());
        }
        west = west.min(c.x);
        east = east.max(c.x);
        let p = frame
            .to_local(*c)
            .filter(|p| p.iter().all(|v| v.is_finite() && v.abs() <= 1e12))
            .ok_or("polygon cannot be transformed into this construction grid")?;
        points.push(p);
    }
    if east - west > 180. {
        return Err("conversion across the antimeridian is unsupported".into());
    }
    let hull = hull(points.clone())?;
    let shape = match kind {
        "circle" => {
            let bounds = bounds(&points, 0.);
            let center = [(bounds[0] + bounds[2]) / 2., (bounds[1] + bounds[3]) / 2.];
            let radius = points.iter().map(|p| (p[0] - center[0]).hypot(p[1] - center[1])).fold(0., f64::max);
            Shape::Circle(CircleSpec { frame, center, radius, tolerance: 0.25_f64.max(radius / 10000.) })
        }
        "rotatedBox" => {
            progress::begin(hull.len());
            let mut best: Option<BoxSpec> = None;
            for (a, b) in hull.iter().zip(hull.iter().cycle().skip(1)).take(hull.len()) {
                progress::step(1).map_err(|e| e.to_string())?;
                let mut angle = (b[1] - a[1]).atan2(b[0] - a[0]);
                let [x0, y0, x1, y1] = bounds(&points, angle);
                let (mut width, mut height) = (x1 - x0, y1 - y0);
                let (u, v) = ((x0 + x1) / 2., (y0 + y1) / 2.);
                let (sin, cos) = angle.sin_cos();
                let center = [u * cos - v * sin, u * sin + v * cos];
                if height > width {
                    std::mem::swap(&mut width, &mut height);
                    angle += std::f64::consts::FRAC_PI_2;
                }
                let candidate = BoxSpec { frame: frame.clone(), center, width, height, angle: angle.to_degrees().rem_euclid(180.) };
                if best.as_ref().is_none_or(|b| {
                    width * height < b.width * b.height * (1. - 1e-12)
                        || ((width * height - b.width * b.height).abs() <= b.width * b.height * 1e-12 && candidate.angle < b.angle)
                }) {
                    best = Some(candidate);
                }
            }
            Shape::RotatedBox(best.ok_or("polygon has no bounding rectangle")?)
        }
        _ => return Err("kind must be circle or rotatedBox".into()),
    };
    shape.geometry()?;
    Ok(shape)
}
fn bounds(points: &[[f64; 2]], angle: f64) -> [f64; 4] {
    let (s, c) = angle.sin_cos();
    points.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |mut b, p| {
        let (x, y) = (p[0] * c + p[1] * s, -p[0] * s + p[1] * c);
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
        b
    })
}
pub(crate) fn preview_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let mut inputs = crate::cmd::Inputs::primary(input, p)?;
    if let Some(image) = p.get("image") {
        let id = input.layer_id(&json!({"layer":image}))?;
        inputs.layers.push((input.map().instance_id(), id));
    }
    Ok(inputs)
}
pub(crate) fn apply_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let layer = p.pointer("/preview/layer").ok_or("preview.layer is required")?;
    crate::cmd::Inputs::primary(input, &json!({"layer":layer}))
}
pub(crate) fn need_editing(s: &Session) -> Option<&'static str> {
    (!s.map().editing.settings.enabled).then_some("editing is turned off (Edit › Edit)")
}
fn check_layer(s: &Session, layer: u32) -> Result<(), String> {
    if s.map().body != "earth" {
        return Err("shape conversion currently requires an Earth map".into());
    }
    crate::box_edit::check_layer(s, layer)
}
fn target(s: &Session, p: &Value) -> Result<Proposal, String> {
    if p.get("table").is_some_and(|v| !v.is_null()) {
        return Err("conversion requires a polygon layer, not a table".into());
    }
    let layer = s.layer_id(p)?;
    check_layer(s, layer)?;
    let row = p.get("row").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).ok_or("row is required")?;
    let l = s.map().layer(layer).ok_or("no layer")?;
    let feature = l.dataset.features.get(row).ok_or("no such feature")?;
    if p.get("featureId").is_some_and(|v| v.as_u64() != Some(feature.id)) {
        return Err("feature changed; preview again".into());
    }
    let geometry = feature.geometry.as_ref().ok_or("missing polygon geometry")?;
    polygon(geometry)?;
    Ok(Proposal {
        request: p.get("request").and_then(Value::as_u64).unwrap_or(0),
        layer,
        row,
        feature_id: feature.id,
        map_instance: s.map().instance_id(),
        layer_instance: l.instance_id(),
        replacement: l.replacement_revision(),
        revision: l.geometry_rev,
        source_geometry: geometry.clone(),
        shape: Shape::Circle(CircleSpec { frame: Frame::default(), center: [0., 0.], radius: 1., tolerance: 0.25 }),
    })
}
fn frame(raster: Option<&rustgis_map::raster::RasterLayer>, p: &Value) -> Result<Frame, String> {
    if let Some(r) = raster {
        crate::image_grid::read(r)
    } else if let Some(f) = p.get("frame") {
        serde_json::from_value(f.clone()).map_err(|e| e.to_string())
    } else {
        Ok(Frame::default())
    }
}
struct Prepared {
    dataset: Dataset,
    target: Proposal,
    result: Result<Proposal, String>,
}
fn gather(dataset: &Dataset, mut t: Proposal, raster: Option<&rustgis_map::raster::RasterLayer>, p: &Value) -> Result<Proposal, String> {
    if raster.is_some() && p.get("frame").is_some() {
        return Err("choose image or frame, not both".into());
    }
    let f = frame(raster, p)?;
    // Fitted in longitude/latitude, from the layer's own coordinates, as its shape's geometry is
    // given (`replace_geometry` writes it back in them).
    let feature = dataset.features.row(t.row).ok_or("missing polygon")?;
    let polygon = feature.geometry().ok_or("missing polygon")?;
    let polygon = dataset.feature_to_lonlat(feature.id(), polygon)?;
    t.shape = fit(&polygon, f, p.get("kind").and_then(Value::as_str).ok_or("kind is required")?)?;
    Ok(t)
}
fn source(s: &Session, p: &Value) -> Result<Option<rustgis_map::raster::RasterLayer>, String> {
    let Some(image) = p.get("image") else { return Ok(None) };
    let id = s.layer_id(&json!({"layer":image}))?;
    Ok(Some(s.map().layer(id).and_then(|l| l.raster.clone()).ok_or("image must name an open GeoTIFF layer")?))
}
pub(crate) fn ahead(s: &Session, p: &Value) -> Option<crate::cmd::reads::ReadWork> {
    let t = target(s, p).ok()?;
    let dataset = s.map().layer(t.layer)?.dataset.clone();
    let raster = source(s, p);
    let p = p.clone();
    Some(crate::cmd::reads::ahead("Preview Shape Conversion", "shapeConversion".into(), move || {
        let result = raster.and_then(|r| gather(&dataset, t.clone(), r.as_ref(), &p));
        Prepared { dataset, target: t, result }
    }))
}
pub(crate) fn preview(s: &mut Session, p: &Value) -> CmdResult {
    let t = target(s, p)?;
    let ready = crate::cmd::reads::take::<Prepared>(s, "shapeConversion");
    #[cfg(not(target_arch = "wasm32"))]
    if ready.is_none() && s.statistics_window_thread == Some(std::thread::current().id()) {
        return Err("shape fitting needs the desktop background preview queue; use Convert Polygon to Shape… (its Preview Conversion), or ask for edit.previewShapeConversion through MCP".into());
    }
    let done = if let Some(a) = ready {
        let l = s.map().layer(t.layer).ok_or("no layer")?;
        if !a.target.current(s) || !a.dataset.features.shares(&l.dataset.features) || a.target.row != t.row {
            return Err("source changed during fitting; preview again".into());
        }
        a.result?
    } else {
        gather(&s.map().layer(t.layer).ok_or("no layer")?.dataset, t, source(s, p)?.as_ref(), p)?
    };
    serde_json::to_value(done).map_err(|e| e.to_string())
}
/// Synchronous: replaces one polygon with at most 2048 generated edges. Fitting and header
/// reading happen in the cancellable preview read; Apply never scans a layer or reads a file.
pub(crate) fn apply(s: &mut Session, p: &Value) -> CmdResult {
    let proposal: Proposal = serde_json::from_value(p.get("preview").ok_or("preview is required")?.clone()).map_err(|e| e.to_string())?;
    if p.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err("confirm:true is required to replace the polygon with the previewed shape".into());
    }
    check_layer(s, proposal.layer)?;
    if !proposal.current(s) {
        return Err("source changed; preview the conversion again".into());
    }
    let l = s.map().layer(proposal.layer).ok_or("no layer")?;
    polygon(l.dataset.features.get(proposal.row).and_then(|f| f.geometry.as_ref()).ok_or("missing polygon")?)?;
    s.constructions.room()?;
    let attributes = l.dataset.features.get(proposal.row).ok_or("no feature")?.attrs.clone();
    let g = proposal.shape.geometry()?;
    crate::cmd::design::replace_geometry(s, proposal.layer, proposal.row, g)?;
    if s.map()
        .layer(proposal.layer)
        .and_then(|l| l.dataset.features.get(proposal.row))
        .is_none_or(|f| f.attrs != attributes || f.id != proposal.feature_id)
    {
        return Err("an edit rule changed the feature identity or attributes; conversion was refused".into());
    }
    match proposal.shape {
        Shape::Circle(c) => crate::circle_edit::remember(s, proposal.layer, proposal.row, c)?,
        Shape::RotatedBox(b) => crate::box_edit::remember(s, proposal.layer, proposal.row, b)?,
    }
    s.notify("1 polygon replaced by an explicitly fitted shape in the chosen construction grid; the boundary may change, and construction remains session-only",false);
    Ok(json!({"converted":1,"layer":proposal.layer,"row":proposal.row,"featureId":proposal.feature_id}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Dataset, Field, FieldType, GeometryType};
    fn ordinary(g: Geometry) -> (Session, u32) {
        let mut s = Session::new(crate::Project::default());
        let id =
            s.map_mut().add_dataset(Dataset::new("Shared annotations", GeometryType::Polygon, vec![Field::new("class", FieldType::Text)]));
        s.active_layer = Some(id);
        s.run("edit.enable", &json!({"on":true})).unwrap();
        s.run("edit.addFeature", &json!({"geometry":rustgis_data::io::geometry_to_geojson(&g),"attributes":{"class":"tree"}})).unwrap();
        (s, id)
    }
    fn rectangle(frame: Frame) -> BoxSpec {
        BoxSpec { frame, center: [1000., 1000.], width: 160., height: 70., angle: 27. }
    }
    #[test]
    fn shape_conversion_roundtrip_fits_image_and_default_grids_without_metadata() {
        let frames = [Frame::default(), Frame { epsg: 32630, width: 512, height: 512, affine: [500000., 0.8, 0.2, 5700000., 0.1, -0.8] }];
        for frame in frames {
            let b = rectangle(frame.clone());
            let c = CircleSpec { frame: frame.clone(), center: [1000., 1000.], radius: 45., tolerance: 0.1 };
            for (kind, g) in [("circle", c.geometry().unwrap()), ("rotatedBox", b.geometry().unwrap())] {
                let (mut s, id) = ordinary(g);
                assert!(s.box_spec(id, 0).is_none() && s.circle_spec(id, 0).is_none());
                let before = s.map().layer(id).unwrap().dataset.features[0].clone();
                let depth = s.undo_depth();
                let p = s.run("edit.previewShapeConversion", &json!({"layer":id,"row":0,"kind":kind,"frame":frame})).unwrap();
                assert_eq!(s.undo_depth(), depth);
                assert_eq!(s.map().layer(id).unwrap().dataset.features[0], before);
                s.run("edit.applyShapeConversion", &json!({"preview":p,"confirm":true})).unwrap();
                assert_eq!(s.undo_depth(), depth + 1);
                let after = &s.map().layer(id).unwrap().dataset.features[0];
                assert_eq!(after.id, before.id);
                assert_eq!(after.attrs, before.attrs);
                if kind == "circle" {
                    let fit = s.circle_spec(id, 0).unwrap();
                    assert!((fit.radius - c.radius).abs() < 1e-5);
                    assert!((fit.center[0] - c.center[0]).abs() < 1e-5);
                } else {
                    let fit = s.box_spec(id, 0).unwrap();
                    assert!((fit.angle - b.angle).abs() < 1e-5);
                    assert!((fit.width - b.width).abs() < 1e-5);
                    assert!((fit.height - b.height).abs() < 1e-5);
                }
                assert_eq!(s.construction_output_notes().unwrap().len(), 1);
                let text = rustgis_data::io::geometry_to_geojson(after.geometry.as_ref().unwrap());
                assert_eq!(text["type"], "Polygon");
                s.run("edit.undo", &json!({})).unwrap();
                assert_eq!(s.map().layer(id).unwrap().dataset.features[0], before);
                assert!(s.circle_spec(id, 0).is_none() && s.box_spec(id, 0).is_none());
                s.run("edit.redo", &json!({})).unwrap();
                assert!(s.circle_spec(id, 0).is_some() || s.box_spec(id, 0).is_some());
            }
        }
    }
    #[test]
    fn shape_conversion_rejects_unsupported_geometry_and_stale_or_unconfirmed_apply() {
        let g = rectangle(Frame::default()).geometry().unwrap();
        let (mut s, id) = ordinary(g.clone());
        let p = s.run("edit.previewShapeConversion", &json!({"row":0,"kind":"circle"})).unwrap();
        let depth = s.undo_depth();
        assert!(s.run("edit.applyShapeConversion", &json!({"preview":p})).is_err());
        assert_eq!(s.undo_depth(), depth);
        s.run("edit.setGeometry", &json!({"row":0,"geometry":rustgis_data::io::geometry_to_geojson(&g)})).unwrap();
        assert!(s.run("edit.applyShapeConversion", &json!({"preview":p,"confirm":true})).is_err());
        let Geometry::Polygon(polys) = g.clone() else { panic!() };
        let ring = polys[0][0].clone();
        for bad in [
            Geometry::Polygon(vec![vec![ring.clone(), ring.clone()]]),
            Geometry::Polygon(vec![vec![ring.clone()], vec![ring.clone()]]),
            Geometry::Point(ring[0]),
        ] {
            assert!(fit(&bad, Frame::default(), "circle").is_err());
            assert!(fit(&bad, Frame::default(), "rotatedBox").is_err());
        }
        assert!(s.circle_spec(id, 0).is_none());
        assert!(fit(&g, Frame { affine: [0.; 6], ..Frame::default() }, "circle").is_err());
    }
    #[test]
    fn shape_conversion_preview_rejects_undo_revision_aliases() {
        for kind in ["circle", "rotatedBox"] {
            let (mut s, id) = ordinary(rectangle(Frame::default()).geometry().unwrap());
            let mut changed = rectangle(Frame::default());
            changed.width *= 2.;
            s.run("edit.setGeometry", &json!({"row":0,"geometry":rustgis_data::io::geometry_to_geojson(&changed.geometry().unwrap())}))
                .unwrap();
            let preview = s.run("edit.previewShapeConversion", &json!({"row":0,"kind":kind})).unwrap();
            // Use the wire representation too: source coordinates must survive JSON exactly.
            let preview: Value = serde_json::from_str(&serde_json::to_string(&preview).unwrap()).unwrap();
            s.run("edit.undo", &json!({})).unwrap();
            assert_eq!(s.map().layer(id).unwrap().geometry_rev, preview["revision"].as_u64().unwrap());
            let restored = s.map().layer(id).unwrap().dataset.features[0].clone();
            let depth = s.undo_depth();
            let error = s.run("edit.applyShapeConversion", &json!({"preview":preview,"confirm":true})).unwrap_err();
            assert!(error.contains("source changed"), "{error}");
            assert_eq!(s.map().layer(id).unwrap().dataset.features[0], restored);
            assert_eq!(s.undo_depth(), depth);
            assert!(s.box_spec(id, 0).is_none() && s.circle_spec(id, 0).is_none());
            // Refusal must preserve Redo as well as the restored source and construction state.
            s.run("edit.redo", &json!({})).unwrap();
            assert_eq!(s.map().layer(id).unwrap().dataset.features[0].geometry, Some(changed.geometry().unwrap()));
            let fresh = s.run("edit.previewShapeConversion", &json!({"row":0,"kind":kind})).unwrap();
            let fresh: Value = serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
            s.run("edit.applyShapeConversion", &json!({"preview":fresh,"confirm":true})).unwrap();
        }
    }

    #[test]
    fn shape_conversion_apply_checks_its_preview_layer_instead_of_the_active_stream() {
        for kind in ["circle", "rotatedBox"] {
            let (mut s, id) = ordinary(rectangle(Frame::default()).geometry().unwrap());
            let preview = s.run("edit.previewShapeConversion", &json!({"layer":id,"row":0,"kind":kind})).unwrap();
            let streamed = s.map_mut().add_streamed("Unrelated stream", rustgis_map::source::SourceLayer::default());
            s.active_layer = Some(streamed);
            s.run("edit.applyShapeConversion", &json!({"preview":preview,"confirm":true})).unwrap();
            assert!(s.box_spec(id, 0).is_some() || s.circle_spec(id, 0).is_some());
            let preview = s.run("edit.previewShapeConversion", &json!({"layer":id,"row":0,"kind":kind})).unwrap();
            s.map_mut().layer_mut(id).unwrap().source = Some(rustgis_map::source::SourceLayer::default());
            s.active_layer = None;
            let before = s.map().layer(id).unwrap().dataset.features[0].clone();
            let depth = s.undo_depth();
            let error = s.run("edit.applyShapeConversion", &json!({"preview":preview,"confirm":true})).unwrap_err();
            assert!(error.contains("read in place"), "{error}");
            assert_eq!(s.map().layer(id).unwrap().dataset.features[0], before);
            assert_eq!(s.undo_depth(), depth);
        }
    }

    #[test]
    fn shape_conversion_rules_cannot_silently_change_attributes_and_history_restores_constructions() {
        let (mut s, id) = ordinary(rectangle(Frame::default()).geometry().unwrap());
        let before = s.map().layer(id).unwrap().dataset.features[0].clone();
        s.run("data.attributeRule", &json!({"name":"Reclassify","kind":"calculation","field":"class","expression":"'changed'"})).unwrap();
        let proposal = s.run("edit.previewShapeConversion", &json!({"row":0,"kind":"circle"})).unwrap();
        let depth = s.undo_depth();
        let error = s.run("edit.applyShapeConversion", &json!({"preview":proposal,"confirm":true})).unwrap_err();
        assert!(error.contains("attributes"));
        assert_eq!(s.map().layer(id).unwrap().dataset.features[0], before);
        assert_eq!(s.undo_depth(), depth);
        assert!(s.circle_spec(id, 0).is_none());
        s.run("edit.undo", &json!({})).unwrap(); // Remove the rule.
        for kind in ["circle", "rotatedBox"] {
            let preview = s.run("edit.previewShapeConversion", &json!({"row":0,"kind":kind})).unwrap();
            s.run("edit.applyShapeConversion", &json!({"preview":preview,"confirm":true})).unwrap();
        }
        assert!(s.box_spec(id, 0).is_some());
        assert!(s.circle_spec(id, 0).is_none());
        s.run("edit.undo", &json!({})).unwrap();
        assert!(s.circle_spec(id, 0).is_some());
        assert!(s.box_spec(id, 0).is_none());
        s.run("edit.redo", &json!({})).unwrap();
        assert!(s.box_spec(id, 0).is_some());
        assert_eq!(s.map().layer(id).unwrap().dataset.features[0].attrs, before.attrs);
    }

    #[test]
    fn shape_conversion_plugin_handled_rejection_is_atomic_and_has_no_inner_undo() {
        for (index, kind) in ["circle", "rotatedBox"].into_iter().enumerate() {
            let (mut s, id) = ordinary(rectangle(Frame::default()).geometry().unwrap());
            let initial = s.run("edit.previewShapeConversion", &json!({"row":0,"kind":"rotatedBox"})).unwrap();
            s.run("edit.applyShapeConversion", &json!({"preview":initial,"confirm":true})).unwrap();
            s.run("data.archive", &json!({"on":true})).unwrap();
            s.run("data.attributeRule", &json!({"name":"Reclassify","kind":"calculation","field":"class","expression":"'changed'"}))
                .unwrap();
            let plugin = format!("shape-conversion-savepoint-{index}");
            for reject in [true, false] {
                if !reject {
                    s.run("data.attributeRule", &json!({"name":"Reclassify","remove":true})).unwrap();
                }
                let preview = s.run("edit.previewShapeConversion", &json!({"row":0,"kind":kind})).unwrap();
                let request = json!({"op":"edit.applyShapeConversion","params":{"preview":preview,"confirm":true}});
                let wasm = rustgis_plugins::testing::wasm_with_handled_host_error(&request);
                assert!(!wasm.is_empty());
                let manifest = rustgis_plugins::testing::manifest(&plugin, &["edit"]);
                let bytes = rustgis_plugins::testing::zip_of(&[("plugin.json", manifest.as_bytes()), ("plugin.wasm", &wasm)]);
                rustgis_plugins::load(rustgis_plugins::Package::from_zip(&bytes).unwrap(), "test").unwrap();
                let before = s.map().layer(id).unwrap().dataset.features[0].clone();
                let archive = s.map().layer(id).unwrap().design.archive.clone();
                let counts = s.run("edit.status", &json!({})).unwrap()["layers"].clone();
                let depth = s.undo_depth();
                let result = s.run(&format!("plugin:{plugin}/f"), &json!({})).unwrap();
                assert_eq!(s.undo_depth(), depth + 1, "only the outer plugin records Undo");
                if reject {
                    assert!(result["handled"]["error"].as_str().unwrap().contains("attributes"), "{result}");
                    assert_eq!(s.map().layer(id).unwrap().dataset.features[0], before);
                    assert_eq!(s.map().layer(id).unwrap().design.archive, archive);
                    assert_eq!(s.run("edit.status", &json!({})).unwrap()["layers"], counts);
                    assert!(s.box_spec(id, 0).is_some() && s.circle_spec(id, 0).is_none());
                } else {
                    assert_eq!(result["handled"]["ok"]["converted"], 1, "{result}");
                    assert_eq!(s.circle_spec(id, 0).is_some(), kind == "circle");
                    assert_eq!(s.box_spec(id, 0).is_some(), kind == "rotatedBox");
                    s.run("edit.undo", &json!({})).unwrap();
                    assert_eq!(s.map().layer(id).unwrap().dataset.features[0], before);
                }
                rustgis_plugins::unload(&plugin);
            }
        }
    }

    #[test]
    fn shape_conversion_scoped_preview_keeps_approved_named_and_implicit_selectors() {
        use crate::policy::{Call, Effects, Outcome, Revocation, Scope};
        use std::sync::Arc;
        struct Live;
        impl Revocation for Live {
            fn allows(&self, _: Effects) -> bool {
                true
            }
        }
        for selector in [Value::Null, json!("Shared annotations")] {
            for change_target in [false, true] {
                let (mut s, id) = ordinary(rectangle(Frame::default()).geometry().unwrap());
                let mut params = json!({"row":0,"kind":"circle"});
                if !selector.is_null() {
                    params["layer"] = selector.clone();
                }
                let call = Call::command("edit.previewShapeConversion", params);
                let scope = Scope::assistant(2274, Arc::new(Live));
                let Outcome::NeedsApproval(pending) = s.review(&scope, &call) else { panic!("preview needs Files approval") };
                let read = s.start_scoped_archive_read(scope, &call, Some(pending.allow())).unwrap();
                let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
                if change_target {
                    s.run("layer.rename", &json!({"layer":id,"name":"Renamed source"})).unwrap();
                    let other = s.map_mut().add_dataset(Dataset::new("Shared annotations", GeometryType::Polygon, vec![]));
                    s.active_layer = Some(other);
                }
                let result = crate::cmd::reads::read_land(&mut s, done);
                if change_target {
                    assert!(result.is_err());
                } else {
                    let proposal: Proposal = serde_json::from_value(result.unwrap()).unwrap();
                    assert_eq!(proposal.layer, id);
                    assert!(proposal.current(&s));
                }
                assert!(s.circle_spec(id, 0).is_none());
            }
        }
    }

    #[test]
    fn shape_conversion_background_preview_is_cancellable_and_rejects_changed_rows() {
        let (mut s, id) = ordinary(rectangle(Frame::default()).geometry().unwrap());
        let params = json!({"layer":id,"row":0,"kind":"rotatedBox"});
        let read = crate::cmd::reads::read_ahead(&mut s, "edit.previewShapeConversion", &params).unwrap();
        let control = rustgis_analysis::control::Control::new();
        control.cancel();
        assert!(read.run(&control).is_err());
        let read = crate::cmd::reads::read_ahead(&mut s, "edit.previewShapeConversion", &params).unwrap();
        let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
        s.run("edit.setValue", &json!({"row":0,"field":"class","value":"changed"})).unwrap();
        assert!(crate::cmd::reads::read_land(&mut s, done).is_err());
        assert!(s.box_spec(id, 0).is_none());
    }
    #[test]
    fn shape_conversion_reads_original_embedded_geotiff_header_instead_of_display_grid() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
        let original = Raster::new(
            "original",
            8,
            6,
            vec![vec![1.; 48]],
            GeoTransform { origin_x: 500000., origin_y: 5700000., pixel_width: 2., pixel_height: -3. },
            Crs::from_epsg(32630).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let bytes = geotiff::write(&original, 0).unwrap();
        let grid = geotiff::read_image_grid(std::io::Cursor::new(&bytes)).unwrap();
        let frame = Frame::from_grid(&grid).unwrap();
        let c = CircleSpec { frame: frame.clone(), center: [4., 3.], radius: 1., tolerance: 0.01 };
        let (mut s, id) = ordinary(c.geometry().unwrap());
        let display = Raster::new(
            "overview",
            1,
            1,
            vec![vec![1.]],
            GeoTransform { origin_x: 500000., origin_y: 5700000., pixel_width: 16., pixel_height: -18. },
            Crs::from_epsg(32630).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let _ =
            crate::cmd::raster::add_raster(&mut s, "Image", rustgis_map::RasterLayer::from_bytes(display, "original.tif", bytes)).unwrap();
        let image = s.map().layer_by_name("Image").unwrap().id;
        let p = s.run("edit.previewShapeConversion", &json!({"layer":id,"row":0,"kind":"circle","image":image})).unwrap();
        s.run("edit.applyShapeConversion", &json!({"preview":p,"confirm":true})).unwrap();
        let proposal: Proposal = serde_json::from_value(p).unwrap();
        let Shape::Circle(fit) = proposal.shape else { panic!() };
        assert_eq!(fit.frame, frame);
        assert!((fit.radius - 1.).abs() < 1e-5);
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod linked_images {
        use super::*;
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};

        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        fn pixels() -> Raster {
            Raster::new(
                "image",
                64,
                64,
                vec![vec![1.; 4096]],
                GeoTransform { origin_x: 100., origin_y: 2000., pixel_width: 2., pixel_height: -3. },
                Crs::from_epsg(3857).unwrap(),
                None,
                ColorHint::Gray,
                "UInt8",
            )
            .unwrap()
        }
        fn setup(case: &str, kind: &str, queued: bool) -> (Dir, Session, u32, u32, Frame) {
            let dir = Dir(std::env::temp_dir().join(format!("rustgis-conversion-linked-{}-{case}-{kind}-{queued}", std::process::id())));
            std::fs::create_dir_all(&dir.0).unwrap();
            let path = dir.0.join("image.tif");
            let bytes = geotiff::write(&pixels(), 0).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let frame = Frame::from_grid(&geotiff::read_image_grid(std::io::Cursor::new(&bytes)).unwrap()).unwrap();
            let geometry = if kind == "circle" {
                CircleSpec { frame: frame.clone(), center: [32., 32.], radius: 5., tolerance: 0.01 }.geometry().unwrap()
            } else {
                BoxSpec { frame: frame.clone(), center: [32., 32.], width: 12., height: 8., angle: 27. }.geometry().unwrap()
            };
            let (mut s, layer) = ordinary(geometry);
            let image = s.map_mut().add_raster(
                "Image",
                rustgis_map::RasterLayer::from_path(rustgis_data::raster::read_path(&path).unwrap(), path.display().to_string()),
            );
            (dir, s, layer, image, frame)
        }
        fn preview(s: &mut Session, params: &Value, queued: bool) -> Result<Value, String> {
            if queued {
                let read = crate::cmd::reads::read_ahead(s, "edit.previewShapeConversion", params)?;
                let done = read.run(&rustgis_analysis::control::Control::new())?;
                crate::cmd::reads::read_land(s, done)
            } else {
                s.run("edit.previewShapeConversion", params)
            }
        }
        fn apply(s: &mut Session, layer: u32, params: &Value, queued: bool, expected: &Frame) {
            let before = s.map().layer(layer).unwrap().dataset.features[0].clone();
            let depth = s.undo_depth();
            let value = preview(s, params, queued).unwrap();
            let proposal: Proposal = serde_json::from_value(value.clone()).unwrap();
            let frame = match proposal.shape {
                Shape::Circle(c) => c.frame,
                Shape::RotatedBox(b) => b.frame,
            };
            assert_eq!(&frame, expected);
            assert_eq!(s.undo_depth(), depth);
            assert_eq!(s.map().layer(layer).unwrap().dataset.features[0], before);
            s.run("edit.applyShapeConversion", &json!({"preview":value,"confirm":true})).unwrap();
            assert_eq!(s.undo_depth(), depth + 1);
            let after = &s.map().layer(layer).unwrap().dataset.features[0];
            assert_eq!(after.id, before.id);
            assert_eq!(after.attrs, before.attrs);
            assert!(s.circle_spec(layer, 0).is_some() || s.box_spec(layer, 0).is_some());
        }

        #[test]
        fn shape_conversion_verified_local_images_preview_and_apply_in_both_read_paths() {
            for queued in [false, true] {
                for kind in ["circle", "rotatedBox"] {
                    let (_dir, mut s, layer, image, frame) = setup("verified", kind, queued);
                    apply(&mut s, layer, &json!({"layer":layer,"row":0,"kind":kind,"image":image}), queued, &frame);
                }
            }
        }

        #[test]
        fn shape_conversion_local_image_failures_preserve_features_and_recover_after_reload() {
            for queued in [false, true] {
                for kind in ["circle", "rotatedBox"] {
                    for case in ["missing", "unverified", "changed"] {
                        let (dir, mut s, layer, image, frame) = setup(case, kind, queued);
                        let raster = s.map_mut().layer_mut(image).unwrap().raster.as_mut().unwrap();
                        let expected = match case {
                            "missing" => {
                                raster.data = None;
                                "pixels are not loaded"
                            }
                            "unverified" => {
                                raster.data = Some(std::sync::Arc::new(pixels()));
                                "no verified original file revision"
                            }
                            _ => {
                                let path = dir.0.join("image.tif");
                                let mut bytes = std::fs::read(&path).unwrap();
                                bytes.push(0); // Valid TIFF, deterministically different length even on coarse-timestamp filesystems.
                                std::fs::write(path, bytes).unwrap();
                                "file changed"
                            }
                        };
                        let before = s.map().layer(layer).unwrap().dataset.features[0].clone();
                        let depth = s.undo_depth();
                        let params = json!({"layer":layer,"row":0,"kind":kind,"image":image});
                        let error = preview(&mut s, &params, queued).unwrap_err();
                        assert!(error.contains(expected), "{error}");
                        assert!(error.contains("Reload Data") && error.contains("retry"), "{error}");
                        assert!(!error.contains("Reload Grid"), "conversion has no Reload Grid action: {error}");
                        assert_eq!(s.undo_depth(), depth);
                        assert_eq!(s.map().layer(layer).unwrap().dataset.features[0], before);
                        assert!(s.box_spec(layer, 0).is_none() && s.circle_spec(layer, 0).is_none());
                        s.run("layer.reloadData", &json!({"layer":image})).unwrap();
                        apply(&mut s, layer, &params, queued, &frame);
                    }
                }
            }
        }
    }

    #[test]
    fn shape_conversion_enclosing_fits_are_stable_for_concave_polygons() {
        let frame = Frame::default();
        let points = [[0., 0.], [100., 0.], [30., 30.], [100., 100.], [0., 100.], [0., 0.]];
        let g = Geometry::Polygon(vec![vec![points.into_iter().map(|p| frame.to_lonlat(p).unwrap()).collect()]]);
        let Shape::RotatedBox(b) = fit(&g, frame.clone(), "rotatedBox").unwrap() else { panic!() };
        assert!((b.width * b.height - 10000.).abs() < 1e-5);
        let Shape::Circle(c) = fit(&g, frame, "circle").unwrap() else { panic!() };
        assert!((c.radius - 50_f64.hypot(50.)).abs() < 1e-5);
    }
}
