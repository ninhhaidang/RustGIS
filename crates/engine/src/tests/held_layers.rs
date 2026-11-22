//! Commands on a layer held in its own system (#2032 C2): every one gives what it gives for the
//! same layer held in longitude/latitude. Each test runs a command on both and compares.

use rustgis_data::raster::Crs;
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as Cell};
use serde_json::{Value, json};

use crate::{Project, Session};

/// A directory of its own for a test's files, removed when dropped.
struct Dir(std::path::PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let path = std::env::temp_dir().join(format!("rustgis-held-layers-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// UTM zone 32N, the system the held layers are in.
const UTM: Crs = Crs::Epsg(32632);

/// How far apart, in degrees, a place may read through UTM and back (well under a millimetre).
const CLOSE: f64 = 1e-8;

/// Wells (points), Roads (a line) and Parcels (squares) near Ingolstadt, 2.5° east of UTM zone
/// 32N's central meridian (its grid turned some 1.9° from north there), with a name and a number.
fn layers() -> Vec<Dataset> {
    let fields = || vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Integer)];
    let mut wells = Dataset::new("Wells", GeometryType::Point, fields());
    let mut parcels = Dataset::new("Parcels", GeometryType::Polygon, fields());
    for i in 0..3 {
        for j in 0..3 {
            let (x, y) = (11.5 + 0.01 * f64::from(i), 48.7 + 0.01 * f64::from(j));
            let n = i64::from(i * 3 + j);
            wells.push(Some(Geometry::Point(Coord::new(x, y))), vec![Cell::Text(format!("w{n}")), Cell::Int(n)]);
            let ring = vec![
                Coord::new(x, y),
                Coord::new(x + 0.005, y),
                Coord::new(x + 0.005, y + 0.005),
                Coord::new(x, y + 0.005),
                Coord::new(x, y),
            ];
            parcels.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![Cell::Text(format!("p{n}")), Cell::Int(n)]);
        }
    }
    let mut roads = Dataset::new("Roads", GeometryType::Polyline, fields());
    let line = vec![Coord::new(11.49, 48.69), Coord::new(11.515, 48.705), Coord::new(11.53, 48.73)];
    roads.push(Some(Geometry::Line(vec![line])), vec![Cell::Text("r0".into()), Cell::Int(0)]);
    vec![wells, roads, parcels]
}

/// A session with [`layers`] held in `crs`.
fn session(crs: Crs) -> Session {
    let mut s = Session::new(Project::default());
    for mut ds in layers() {
        ds.hold_in(crs).unwrap();
        s.map_mut().add_dataset(ds);
    }
    s.map_mut().editing.settings.enabled = true;
    s
}

/// The sessions with the layers held in longitude/latitude and in UTM 32N.
fn both() -> [Session; 2] {
    [session(Crs::LonLat), session(UTM)]
}

/// A layer's geometry, read in longitude/latitude.
fn lonlat(s: &Session, layer: &str) -> Vec<Option<Geometry>> {
    let l = s.map().layer_by_name(layer).unwrap();
    l.dataset.in_lonlat().unwrap().features.iter().map(|f| f.geometry.clone()).collect()
}

/// A geometry's shape: its kind, then how many parts it has and how many places each part (or
/// ring) has.
fn shape(g: &Option<Geometry>) -> Vec<usize> {
    match g {
        None => vec![0],
        Some(Geometry::Point(_)) => vec![1],
        Some(Geometry::MultiPoint(v)) => vec![2, v.len()],
        Some(Geometry::Line(parts)) => [3, parts.len()].into_iter().chain(parts.iter().map(Vec::len)).collect(),
        Some(Geometry::Polygon(polys)) => [4, polys.len()]
            .into_iter()
            .chain(polys.iter().flat_map(|rings| std::iter::once(rings.len()).chain(rings.iter().map(Vec::len))))
            .collect(),
    }
}

/// Every coordinate of `a` within [`CLOSE`] of `b`'s, in the same shapes.
fn assert_close(a: &[Option<Geometry>], b: &[Option<Geometry>], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: feature count");
    for (i, (a, b)) in a.iter().zip(b).enumerate() {
        assert_eq!(shape(a), shape(b), "{what}: feature {i}'s shape");
        let (mut ca, mut cb) = (Vec::new(), Vec::new());
        if let Some(g) = a {
            g.for_each_coord(|c| ca.push(c));
        }
        if let Some(g) = b {
            g.for_each_coord(|c| cb.push(c));
        }
        assert_eq!(ca.len(), cb.len(), "{what}: feature {i}'s coordinates");
        for (p, q) in ca.iter().zip(&cb) {
            assert!((p.x - q.x).abs() < CLOSE && (p.y - q.y).abs() < CLOSE, "{what}: feature {i}: {p:?} vs {q:?}");
        }
    }
}

/// Runs `cmd` in each session and gives both results.
fn run_both(ss: &mut [Session; 2], cmd: &str, p: &Value) -> [Value; 2] {
    let a = ss[0].run(cmd, p).unwrap_or_else(|e| panic!("{cmd} (longitude/latitude): {e}"));
    let b = ss[1].run(cmd, p).unwrap_or_else(|e| panic!("{cmd} (UTM): {e}"));
    [a, b]
}

/// The rows of `layer` selected.
fn selected(s: &Session, layer: &str) -> Vec<usize> {
    let mut v = s.map().layer_by_name(layer).unwrap().selection.clone();
    v.sort_unstable();
    v
}

/// A held layer reads back in longitude/latitude where it was put, and its extent, a zoom to it
/// and the map's full extent are those of the same layer held in longitude/latitude.
#[test]
fn a_held_layer_has_the_same_extent_and_zoom() {
    let mut ss = both();
    assert_eq!(ss[1].map().layer_by_name("Wells").unwrap().dataset.coord_crs(), UTM);
    for layer in ["Wells", "Roads", "Parcels"] {
        assert_close(&lonlat(&ss[0], layer), &lonlat(&ss[1], layer), layer);
        let [a, b] = [&ss[0], &ss[1]].map(|s| s.map().layer_by_name(layer).unwrap().extent());
        // The held layer's is the longitude/latitude box of its UTM box: it holds the other, and
        // is a few percent larger, as UTM's grid is turned some 1.9° from north there.
        let slack = 0.1 * a.width().max(a.height());
        let ulp = 1e-9;
        assert!(
            b.min_x <= a.min_x + ulp && b.min_y <= a.min_y + ulp && b.max_x >= a.max_x - ulp && b.max_y >= a.max_y - ulp,
            "{layer}: {a:?} vs {b:?}"
        );
        assert!(a.min_x - b.min_x < slack && a.min_y - b.min_y < slack, "{layer}: {a:?} vs {b:?}");
        assert!(b.max_x - a.max_x < slack && b.max_y - a.max_y < slack, "{layer}: {a:?} vs {b:?}");
    }
    run_both(&mut ss, "layer.zoomTo", &json!({"layer": "Parcels"}));
    let [a, b] = [&ss[0], &ss[1]].map(|s| s.map().camera.center);
    // Web Mercator metres, on a layer some 2.8 km across: within the extents' difference.
    assert!((a.x - b.x).abs() < 5.0 && (a.y - b.y).abs() < 5.0, "{a:?} vs {b:?}");
}

/// Selecting by a box, a point, a radius and a polygon picks the same features.
#[test]
fn a_held_layer_is_selected_alike() {
    let mut ss = both();
    let picks = [
        ("select.byRect", json!({"minLon": 11.495, "minLat": 48.695, "maxLon": 11.512, "maxLat": 48.712})),
        ("select.atPoint", json!({"lon": 11.5125, "lat": 48.7125, "tolerancePx": 0})),
        ("select.byRadius", json!({"lon": 11.51, "lat": 48.71, "radius": 900})),
        ("select.byPolygon", json!({"points": [[11.495, 48.695], [11.525, 48.695], [11.5, 48.725], [11.495, 48.695]]})),
    ];
    for (cmd, p) in picks {
        for s in &mut ss {
            // Refused when nothing is selected.
            let _ = s.run("select.clear", &Value::Null);
        }
        let [a, b] = run_both(&mut ss, cmd, &p);
        assert_eq!(a, b, "{cmd}");
        for layer in ["Wells", "Roads", "Parcels"] {
            assert_eq!(selected(&ss[0], layer), selected(&ss[1], layer), "{cmd}: {layer}");
        }
        assert!(["Wells", "Parcels"].iter().any(|l| !selected(&ss[0], l).is_empty()), "{cmd} picked nothing");
    }
}

/// Exports in longitude/latitude and in another system, and a conversion to GeoJSON, write the
/// same places.
#[test]
fn a_held_layer_is_exported_alike() {
    let mut ss = both();
    let dir = Dir::new("exports");
    for (ext, crs) in [("geojson", None), ("gpkg", Some("EPSG:25832")), ("shp", Some("EPSG:32632")), ("csv", None)] {
        let mut read = Vec::new();
        for (k, s) in ss.iter_mut().enumerate() {
            let path = dir.path().join(format!("parcels{k}.{ext}"));
            let mut p = json!({"layer": "Parcels", "path": path.display().to_string()});
            if let Some(c) = crs {
                p["crs"] = json!(c);
            }
            s.run("share.exportLayer", &p).unwrap_or_else(|e| panic!("{ext} ({k}): {e}"));
            let ds = rustgis_data::io::read_path(&path).unwrap().0;
            read.push(ds.features.iter().map(|f| f.geometry.clone()).collect::<Vec<_>>());
        }
        assert_close(&read[0], &read[1], ext);
    }
    let p = json!({"layer": "Wells", "format": "geojson"});
    let [a, b] = run_both(&mut ss, "data.convert", &p);
    let read = [a, b].map(|v| {
        let text = v["text"].as_str().map(str::to_owned).unwrap_or_else(|| v.to_string());
        rustgis_data::io::read_bytes("w.geojson", text.as_bytes())
            .unwrap()
            .0
            .features
            .iter()
            .map(|f| f.geometry.clone())
            .collect::<Vec<_>>()
    });
    assert_close(&read[0], &read[1], "data.convert");
}

/// Geometry calculations give the same measures, and a buffer the same places to within UTM's
/// scale there.
#[test]
fn a_held_layer_is_analysed_alike() {
    let mut ss = both();
    run_both(&mut ss, "gp.run", &json!({"tool": "calculateGeometry", "params": {"input": "Parcels"}}));
    run_both(&mut ss, "gp.run", &json!({"tool": "calculateGeometry", "params": {"input": "Roads"}}));
    for layer in ["Parcels", "Roads"] {
        let [a, b] = [&ss[0], &ss[1]].map(|s| {
            let ds = &s.map().layer_by_name(layer).unwrap().dataset;
            (0..ds.features.len())
                .flat_map(|r| (2..ds.fields.len()).map(move |f| (r, f)))
                .map(|(r, f)| ds.value(r, f).as_f64().unwrap_or(f64::NAN))
                .collect::<Vec<_>>()
        });
        assert!(!a.is_empty(), "{layer}: no measures");
        assert_eq!(a.len(), b.len(), "{layer}: how many measures");
        for (p, q) in a.iter().zip(&b) {
            assert!((p - q).abs() <= 1e-6 * p.abs().max(1.0), "{layer}: {a:?} vs {b:?}");
        }
    }
    // A buffer of 100 km: on the ground in longitude/latitude, flat in UTM (#2032 C2e-3). UTM's
    // scale runs from 0.99969 at the buffer's west side (10.14°E) to 1.00059 at its east side
    // (12.86°E, `proj -S`), so the flat 100 km reach 100,017 m to 99,972 m on the ground.
    run_both(&mut ss, "gp.run", &json!({"tool": "buffer", "params": {"input": "Wells", "distance": 100}}));
    let well = lonlat(&ss[0], "Wells")[0].clone();
    let Some(Geometry::Point(well)) = well else { panic!("{well:?}") };
    let held_well = held_coords(&ss[1], "Wells")[0][0];
    for c in &held_coords(&ss[1], "Wells_Buffer")[0] {
        assert!(((c.x - held_well.x).hypot(c.y - held_well.y) - 100_000.0).abs() < 1e-6, "flat: {c:?}");
    }
    for (session, (low, high)) in [(&ss[0], (99_999.99, 100_000.01)), (&ss[1], (99_970.0, 100_020.0))] {
        let Some(Geometry::Polygon(rings)) = &lonlat(session, "Wells_Buffer")[0] else { panic!("no buffer") };
        let d: Vec<f64> = rings[0][0].iter().map(|c| rustgis_geom::wgs84_distance_m(well, *c)).collect();
        let (min, max) = (d.iter().copied().fold(f64::INFINITY, f64::min), d.iter().copied().fold(0.0, f64::max));
        assert!(min >= low && max <= high, "{min}..{max} m on the ground");
    }
}

/// A geometry set, a move in metres and an undo leave the same places, and a held layer keeps
/// its system through them.
#[test]
fn a_held_layer_is_edited_alike() {
    let mut ss = both();
    let g = json!({"type": "Point", "coordinates": [11.505, 48.705]});
    run_both(&mut ss, "edit.setGeometry", &json!({"layer": "Wells", "row": 0, "geometry": g}));
    assert_close(&lonlat(&ss[0], "Wells"), &lonlat(&ss[1], "Wells"), "setGeometry");
    run_both(&mut ss, "select.rows", &json!({"layer": "Parcels", "rows": [4]}));
    run_both(&mut ss, "edit.move", &json!({"dx": 250.0, "dy": -100.0}));
    let [a, b] = [lonlat(&ss[0], "Parcels"), lonlat(&ss[1], "Parcels")];
    // A move in metres on the map: the two agree to well within a centimetre.
    assert_eq!(a.len(), b.len(), "move: feature count");
    for (p, q) in a.iter().zip(&b) {
        assert_eq!(shape(p), shape(q), "move: shape");
        let (mut ca, mut cb) = (Vec::new(), Vec::new());
        p.as_ref().unwrap().for_each_coord(|c| ca.push(c));
        q.as_ref().unwrap().for_each_coord(|c| cb.push(c));
        for (p, q) in ca.iter().zip(&cb) {
            assert!((p.x - q.x).abs() < 1e-7 && (p.y - q.y).abs() < 1e-7, "move: {p:?} vs {q:?}");
        }
    }
    run_both(&mut ss, "edit.undo", &Value::Null);
    assert_close(&lonlat(&ss[0], "Parcels"), &lonlat(&ss[1], "Parcels"), "undo");
    assert_eq!(ss[1].map().layer_by_name("Parcels").unwrap().dataset.coord_crs(), UTM);
}

/// Features copied from a held layer and pasted into one held in longitude/latitude, and the
/// other way round, land where they were.
#[test]
fn features_are_copied_between_systems() {
    let mut s = session(UTM);
    let fields = vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Integer)];
    s.map_mut().add_dataset(Dataset::new("Plain", GeometryType::Point, fields));
    s.run("select.rows", &json!({"layer": "Wells", "rows": [1, 2]})).unwrap();
    s.run("edit.copy", &Value::Null).unwrap();
    s.run("edit.paste", &json!({"layer": "Plain"})).unwrap();
    assert_eq!(s.map().layer_by_name("Plain").unwrap().dataset.coord_crs(), Crs::LonLat);
    assert_close(&lonlat(&s, "Plain"), &lonlat(&s, "Wells")[1..3], "into longitude/latitude");
    s.run("select.clear", &Value::Null).unwrap();
    s.run("select.rows", &json!({"layer": "Plain", "rows": [0, 1]})).unwrap();
    s.run("edit.copy", &Value::Null).unwrap();
    s.run("edit.paste", &json!({"layer": "Wells"})).unwrap();
    let wells = lonlat(&s, "Wells");
    assert_close(&wells[9..], &wells[1..3], "into UTM");
}

/// The clipboard holds features in the system of the layer they were copied from. Its GeoJSON is
/// their places in longitude/latitude through WGS 84 whatever map they were copied on, also one
/// in another datum's system that draws them by a direct operation; pasted back into the layer
/// they keep its numbers exactly. GeoJSON text naming its system (a legacy `crs` member) is
/// pasted from that system: by the operation between the two into a layer held in another.
#[test]
fn the_clipboard_holds_features_in_their_own_system() {
    let gk = Crs::Epsg(31467);
    let mut s = session(gk);
    s.run("map.projection", &json!({"type": "EPSG:25832"})).unwrap();
    assert_eq!(s.map().flat().direct_system(), Some(Crs::Epsg(25832)));
    s.run("select.rows", &json!({"layer": "Wells", "rows": [1, 2]})).unwrap();
    let text = s.run("edit.copy", &Value::Null).unwrap()["text"].as_str().unwrap().to_string();
    let places = |text: &str| -> Vec<Coord> {
        let gj: Value = serde_json::from_str(text).unwrap();
        gj["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| Coord::new(f["geometry"]["coordinates"][0].as_f64().unwrap(), f["geometry"]["coordinates"][1].as_f64().unwrap()))
            .collect()
    };
    let exact: Vec<Coord> = lonlat(&s, "Wells")[1..3]
        .iter()
        .map(|g| match g {
            Some(Geometry::Point(c)) => *c,
            g => panic!("a point: {g:?}"),
        })
        .collect();
    // Within 0.1 mm of WGS 84's, where the map's way is some decimetres off.
    let copied = places(&text);
    assert_eq!(copied.len(), exact.len(), "every copied feature in the text");
    for (p, q) in copied.iter().zip(&exact) {
        assert!((p.x - q.x).abs() < 1e-9 && (p.y - q.y).abs() < 1e-9, "{p:?} vs {q:?}");
    }
    s.run("map.projection", &json!({"type": "mercator"})).unwrap();
    assert_eq!(s.run("edit.copy", &Value::Null).unwrap()["text"].as_str().unwrap(), text, "the same on Web Mercator");
    let native = |s: &Session, layer: &str| -> Vec<Option<Geometry>> {
        s.map().layer_by_name(layer).unwrap().dataset.features.iter().map(|f| f.geometry.clone()).collect()
    };
    s.run("edit.paste", &json!({"layer": "Wells"})).unwrap();
    let wells = native(&s, "Wells");
    assert_eq!(wells[9..], wells[1..3], "pasted where copied from, exactly");

    // UTM 32N text into a layer in longitude/latitude and into the DHDN one.
    let at = Coord::new(11.5, 48.7);
    let in_utm = Crs::LonLat.to(UTM).unwrap().point(at).unwrap();
    let pasted = json!({
        "type": "FeatureCollection",
        "crs": {"type": "name", "properties": {"name": "urn:ogc:def:crs:EPSG::32632"}},
        "features": [{"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [in_utm.x, in_utm.y]}}]
    })
    .to_string();
    s.map_mut().add_dataset(Dataset::new("Plain", GeometryType::Point, vec![]));
    s.run("edit.paste", &json!({"layer": "Plain", "text": pasted})).unwrap();
    assert_close(&lonlat(&s, "Plain"), &[Some(Geometry::Point(at))], "UTM text into longitude/latitude");
    s.run("edit.paste", &json!({"layer": "Wells", "text": pasted})).unwrap();
    let want = UTM.to(gk).unwrap().point(in_utm).unwrap();
    let Some(Geometry::Point(got)) = native(&s, "Wells")[11] else { panic!("a point") };
    assert!((got.x - want.x).abs() < 1e-6 && (got.y - want.y).abs() < 1e-6, "UTM text into DHDN: {got:?} vs {want:?}");
    // A layer pasted from a copy keeps what its import lost, so Define Projection still refuses it.
    s.map_mut().layers.iter_mut().find(|l| l.name == "Wells").unwrap().dataset.crs_incomplete = true;
    s.run("select.rows", &json!({"layer": "Wells", "rows": [1]})).unwrap();
    s.run("edit.copy", &Value::Null).unwrap();
    let made = s.run("edit.pasteSpecial", &json!({"name": "Copied"})).unwrap();
    let l = s.map().layer(made["layer"].as_u64().unwrap() as u32).unwrap();
    assert!(l.dataset.crs_incomplete);
    assert_eq!(l.dataset.coord_crs(), gk);
}

/// A route over a held network after Define Projection runs on the network as it now reads, not
/// on a graph kept from before: the relabel keeps the rows, so the graph is kept under its system
/// too. Here the UTM 32N numbers relabelled DHDN zone 3 lie about 2,800 km west, away from the
/// route's ends.
#[test]
fn a_route_follows_a_relabelled_network() {
    let mut s = session(UTM);
    let route = json!({"layer": "Roads", "from": [11.4951, 48.6925], "to": [11.5295, 48.7290], "snapDistance": 100});
    let first = s.run("analysis.route", &route).unwrap();
    assert_eq!(s.run("analysis.route", &route).unwrap()["lengthKm"], first["lengthKm"], "the kept graph");
    s.run("layer.defineProjection", &json!({"layer": "Roads", "crs": "EPSG:31467"})).unwrap();
    let after = s.run("analysis.route", &route);
    assert!(after.as_ref().is_err_and(|e| e.contains("from the network")), "a graph kept from before: {after:?}");
}

/// Define Projection on a layer made from a held layer's selection relabels it: its numbers stay
/// as they are, now in the system given. The layer made keeps the system it was read in.
#[test]
fn a_selection_layer_of_a_held_layer_is_relabelled_as_it_is() {
    let mut s = session(UTM);
    s.map_mut().layers.iter_mut().find(|l| l.name == "Wells").unwrap().dataset.crs = Some(UTM);
    s.run("select.rows", &json!({"layer": "Wells", "rows": [0, 4]})).unwrap();
    let made = s.run("select.layerFromSelection", &json!({"layer": "Wells"})).unwrap();
    let id = made["layer"].as_u64().unwrap() as u32;
    let l = s.map().layer(id).unwrap();
    assert_eq!((l.dataset.coord_crs(), l.dataset.crs), (UTM, Some(UTM)));
    let name = l.name.clone();
    let before: Vec<Option<Geometry>> = l.dataset.features.iter().map(|f| f.geometry.clone()).collect();
    s.run("layer.defineProjection", &json!({"layer": name, "crs": "EPSG:25832"})).unwrap();
    let l = s.map().layer(id).unwrap();
    assert_eq!(l.dataset.coord_crs(), Crs::Epsg(25832));
    let after: Vec<Option<Geometry>> = l.dataset.features.iter().map(|f| f.geometry.clone()).collect();
    assert_eq!(after, before, "relabelled, not converted");
    // One without a recorded system too.
    s.map_mut().layers.iter_mut().find(|l| l.name == "Wells").unwrap().dataset.crs = None;
    let made = s.run("select.layerFromSelection", &json!({"layer": "Wells"})).unwrap();
    let id = made["layer"].as_u64().unwrap() as u32;
    let name = s.map().layer(id).unwrap().name.clone();
    s.run("layer.defineProjection", &json!({"layer": name, "crs": "EPSG:25832"})).unwrap();
    let after: Vec<Option<Geometry>> = s.map().layer(id).unwrap().dataset.features.iter().map(|f| f.geometry.clone()).collect();
    assert_eq!(after, before, "relabelled, not converted, without a recorded system");
}

/// A project saves a held layer's features in its own system, exactly, with the system beside
/// them, and requires `native-coordinates`, so an older release refuses it rather than read them
/// as degrees (#2032 C2d). It opens back held as it was, bit for bit, as do a layer file and a map
/// file of it. A project of layers in longitude/latitude saves them as every release did, and
/// requires nothing.
#[test]
fn a_held_layer_is_saved_with_its_own_coordinates() {
    let [mut plain, mut held] = both();
    let dir = Dir::new("project");
    let path = dir.path().join("held.rgis");
    held.run("project.saveAs", &json!({"path": path.display().to_string()})).unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["requires"], json!(["native-coordinates"]));
    let wells = &saved["maps"][0]["layers"].as_array().unwrap().iter().find(|l| l["name"] == "Wells").unwrap()["dataset"];
    assert_eq!(wells["held_in"], json!({"Epsg": 32632}));
    let x = wells["features"][0]["geometry"]["Point"]["x"].as_f64().unwrap();
    assert!((600_000.0..800_000.0).contains(&x), "features saved in UTM 32N: {x}");
    let (back, said) = opened(&path);
    assert!(said.is_empty(), "nothing to say: {said:?}");
    for layer in ["Wells", "Roads", "Parcels"] {
        assert_eq!(back.map().layer_by_name(layer).unwrap().dataset.coord_crs(), UTM, "{layer}");
        assert_eq!(stored(&back, layer), stored(&held, layer), "{layer}: bit for bit");
    }
    // A layer file and a map file keep them alike, and require it too.
    let lyr = dir.path().join("wells.rgislyr");
    held.run("layer.save", &json!({"layer": "Wells", "path": lyr.display().to_string()})).unwrap();
    let rgismap = dir.path().join("held.rgismap");
    held.run("map.save", &json!({"path": rgismap.display().to_string()})).unwrap();
    for (command, file) in [("layer.addLayerFile", &lyr), ("map.import", &rgismap)] {
        let header: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(header["requires"], json!(["native-coordinates"]), "{command}");
        let mut s = Session::new(Project::default());
        s.run(command, &json!({"path": file.display().to_string()})).unwrap();
        let wells = s.project.maps.iter().flat_map(|m| &m.layers).filter(|l| l.name.starts_with("Wells")).last().unwrap();
        assert_eq!(wells.dataset.coord_crs(), UTM, "{command}");
        let got: Vec<Option<Geometry>> = wells.dataset.features.iter().map(|f| f.geometry.clone()).collect();
        assert_eq!(got, stored(&held, "Wells"), "{command}: bit for bit");
    }
    let lonlat_path = dir.path().join("plain.rgis");
    plain.run("project.saveAs", &json!({"path": lonlat_path.display().to_string()})).unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&lonlat_path).unwrap()).unwrap();
    assert!(saved.get("requires").is_none(), "{}", saved["requires"]);
    let wells = &saved["maps"][0]["layers"].as_array().unwrap().iter().find(|l| l["name"] == "Wells").unwrap()["dataset"];
    assert!(wells.get("held_in").is_none());
    // A held layer with no features (a template's) requires it too: an older reader would save
    // it again without its system.
    let mut empty = Session::new(Project::default());
    let mut ds = Dataset::new("Empty", GeometryType::Point, Vec::new());
    ds.hold_in(UTM).unwrap();
    empty.map_mut().add_dataset(ds);
    assert!(empty.project.required_capabilities().contains(&"native-coordinates"));
}

/// A table held in a system of its own is saved there with its system and requires
/// `native-coordinates`, and opens back held, bit for bit; one in longitude/latitude requires
/// nothing (#2032 C2d).
#[test]
fn a_held_table_is_saved_with_its_own_coordinates() {
    let dir = Dir::new("project-table");
    for (crs, required) in [(UTM, true), (Crs::LonLat, false)] {
        let mut s = Session::new(Project::default());
        let mut ds = layers().remove(0);
        ds.hold_in(crs).unwrap();
        let rows: Vec<Option<Geometry>> = ds.features.iter().map(|f| f.geometry.clone()).collect();
        s.project.add_table(ds);
        let path = dir.path().join(format!("{}.rgis", crs.label().replace(':', "-")));
        s.run("project.saveAs", &json!({"path": path.display().to_string()})).unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.get("requires").is_some_and(|r| r == &json!(["native-coordinates"])), required, "{}", crs.label());
        let (back, said) = opened(&path);
        assert!(said.is_empty(), "nothing to say: {said:?}");
        let t = &back.project.tables[0].dataset;
        assert_eq!(t.coord_crs(), crs);
        assert_eq!(t.features.iter().map(|f| f.geometry.clone()).collect::<Vec<_>>(), rows, "{}: bit for bit", crs.label());
    }
}

/// A session with the project at `path` opened, and what was said meanwhile besides that it
/// opened.
fn opened(path: &std::path::Path) -> (Session, Vec<String>) {
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    s.run("project.open", &json!({"path": path.display().to_string()})).unwrap();
    let said = s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| !t.starts_with("Opened ")).collect();
    (s, said)
}

/// A layer's geometry as it holds it.
fn stored(s: &Session, layer: &str) -> Vec<Option<Geometry>> {
    s.map().layer_by_name(layer).unwrap().dataset.features.iter().map(|f| f.geometry.clone()).collect()
}

/// The numbers of a text file (an OBJ's vertices), in order.
fn numbers(text: &str) -> Vec<f64> {
    text.split(|c: char| c.is_whitespace() || c == ',' || c == '[' || c == ']').filter_map(|w| w.parse::<f64>().ok()).collect()
}

/// `a` and `b` hold the same numbers, each within `tol`.
fn assert_numbers_close(a: &[f64], b: &[f64], tol: f64, what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: how many numbers");
    assert!(!a.is_empty(), "{what}: none");
    for (i, (p, q)) in a.iter().zip(b).enumerate() {
        assert!((p - q).abs() <= tol, "{what}: number {i}: {p} vs {q}");
    }
}

/// A 3D file, a web scene, a web layer and a mobile map package place the features alike.
#[test]
fn a_held_layer_is_shared_alike() {
    let mut ss = both();
    let dir = Dir::new("shares");
    let file = |k: usize, name: &str| dir.path().join(format!("{k}-{name}")).display().to_string();
    for (k, s) in ss.iter_mut().enumerate() {
        s.run("share.export3d", &json!({"layer": "Parcels", "path": file(k, "parcels.obj"), "heightField": "n"})).unwrap();
        s.run("layer.extrusion", &json!({"layer": "Parcels", "type": "height", "height": 50})).unwrap();
        s.run("share.webScene", &json!({"path": file(k, "scene.gltf")})).unwrap();
        s.run("share.webLayer", &json!({"layer": "Roads", "path": file(k, "roads.zip")})).unwrap();
        s.run("map.package", &json!({"path": file(k, "map.rgismpk"), "mobile": true, "tolerance": 0.0001})).unwrap();
    }
    let read = |name: &str| [0, 1].map(|k| std::fs::read(file(k, name)).unwrap());
    // Metres from the layer's corner, which is that of its longitude/latitude box, a little further
    // out for the held layer (see the extent above): the vertices from the first one, as the file
    // writes them (to a centimetre), shrunk alike at the box's centre's latitude.
    let [a, b] = read("parcels.obj").map(|t| {
        let text = String::from_utf8(t).unwrap();
        let v: Vec<[f64; 3]> = text
            .lines()
            .filter_map(|l| l.strip_prefix("v "))
            .map(|l| {
                let n = numbers(l);
                [n[0], n[1], n[2]]
            })
            .collect();
        let o = v[0];
        v.iter().flat_map(|p| [p[0] - o[0], p[1] - o[1], p[2] - o[2]]).collect::<Vec<_>>()
    });
    assert_numbers_close(&a, &b, 0.02, "3D file");
    // The scene's accessors' bounds, in metres from the map's centre.
    let [a, b] = read("scene.gltf").map(|t| {
        let g: Value = serde_json::from_slice(&t).unwrap();
        g["accessors"].as_array().unwrap().iter().flat_map(|a| numbers(&format!("{} {}", a["min"], a["max"]))).collect::<Vec<_>>()
    });
    assert_numbers_close(&a, &b, 1e-2, "web scene");
    let [a, b] = read("roads.zip").map(|z| {
        let z = rustgis_data::zip::read(&z).unwrap();
        let items = z.iter().find(|e| e.0.ends_with("items.json")).unwrap();
        let index = z.iter().find(|e| e.0 == "index.html").unwrap();
        let mut n = numbers(&String::from_utf8_lossy(&items.1));
        // The viewer's features, in the page's own data.
        n.extend(
            numbers(&String::from_utf8_lossy(&index.1)).into_iter().filter(|x| (11.4..=11.6).contains(x) || (48.6..=48.8).contains(x)),
        );
        n
    });
    assert_numbers_close(&a, &b, CLOSE, "web layer");
    for s in &mut ss {
        let k = usize::from(s.map().layer_by_name("Wells").unwrap().dataset.coord_crs() != Crs::LonLat);
        s.run("map.import", &json!({"path": file(k, "map.rgismpk")})).unwrap();
    }
    let [a, b] = [&ss[0], &ss[1]].map(|s| {
        let m = s.project.maps.last().unwrap();
        let l = m.layer_by_name("Parcels").unwrap();
        assert_eq!(l.dataset.coord_crs(), Crs::LonLat);
        l.dataset.features.iter().map(|f| f.geometry.clone()).collect::<Vec<_>>()
    });
    assert_close(&a, &b, "mobile map package");
}

/// A spatial join counts the same wells on each parcel (each has one, on its corner), whether
/// both layers are held in UTM or one of them in longitude/latitude.
#[test]
fn a_held_layer_is_joined_alike() {
    let mut ss = both();
    let fields = vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Integer)];
    let mut plain = Dataset::new("Plain", GeometryType::Point, fields);
    // Inside the parcels: one on a corner may read a hair either side of it through UTM.
    for g in lonlat(&ss[0], "Wells") {
        let Some(Geometry::Point(c)) = g else { continue };
        plain.push(Some(Geometry::Point(Coord::new(c.x + 0.001, c.y + 0.001))), vec![Cell::Text("w".into()), Cell::Int(0)]);
    }
    for s in &mut ss {
        s.map_mut().add_dataset(plain.clone());
    }
    let counts = |s: &Session, layer: &str| {
        let ds = &s.map().layer_by_name(layer).unwrap().dataset;
        let f = ds.field_index("join_count").unwrap();
        (0..ds.features.len()).map(|r| ds.value(r, f).as_f64().unwrap_or(-1.0)).collect::<Vec<_>>()
    };
    for (join, out) in [("Wells", "Held_Join"), ("Plain", "Mixed_Join")] {
        let p = json!({"tool": "spatialJoin", "params": {"target": "Parcels", "join": join, "match": "Intersects", "output": out}});
        run_both(&mut ss, "gp.run", &p);
        assert_eq!(counts(&ss[0], out), counts(&ss[1], out), "{join}");
        assert!(counts(&ss[0], out).iter().any(|&n| n > 0.0), "{join}: nothing joined");
        assert_close(&lonlat(&ss[0], out), &lonlat(&ss[1], out), out);
    }
}

/// The map is drawn alike, labels and all, on Web Mercator, another projection and the globe.
#[test]
fn a_held_layer_is_drawn_alike() {
    let mut ss = both();
    let dir = Dir::new("drawn");
    run_both(&mut ss, "labels.enable", &json!({"layer": "Wells", "on": true}));
    for (k, projection) in ["mercator", "epsg:3035", "globe"].into_iter().enumerate() {
        let camera = camera_over_parcels(projection);
        for s in &mut ss {
            s.run("map.projection", &json!({"type": projection})).unwrap();
            // The same view on each, as the two layers' boxes differ a little.
            s.map_mut().camera = camera;
        }
        let pngs = [0, 1].map(|i| {
            let path = dir.path().join(format!("{k}-{i}.png"));
            ss[i].run("share.exportMap", &json!({"path": path.display().to_string(), "width": 320, "height": 200})).unwrap();
            image::open(&path).unwrap().to_rgba8()
        });
        let differ = pngs[0].pixels().zip(pngs[1].pixels()).filter(|(a, b)| a != b).count();
        let drawn = pngs[0].pixels().filter(|p| p.0 != pngs[0].get_pixel(0, 0).0).count();
        assert!(drawn > 1000, "{projection:?}: little drawn ({drawn} pixels)");
        // Anti-aliased edges may shade a little otherwise.
        assert!(differ * 100 < drawn, "{projection:?}: {differ} of {drawn} drawn pixels differ");
    }
}

/// A camera over the parcels on a map of `projection`, set alike on both maps.
fn camera_over_parcels(projection: &str) -> rustgis_map::Camera {
    let mut s = session(Crs::LonLat);
    s.run("map.projection", &json!({"type": projection})).unwrap();
    s.run("layer.zoomTo", &json!({"layer": "Parcels"})).unwrap();
    s.map().camera
}

/// Rasterizing a held layer onto a grid in longitude/latitude gives the grid of the same layer held
/// in longitude/latitude (cells off the parcels' edges, which may read a hair either side of a
/// cell's through UTM). Without a `crs`, a held layer is burned in its own system
/// (`a_held_layer_is_burned_in_its_own_system`).
#[test]
fn a_held_layer_is_rasterized_alike() {
    let mut ss = both();
    let [a, b] = run_both(
        &mut ss,
        "raster.rasterize",
        &json!({"layer": "Parcels", "field": "n", "cellSize": 0.0007, "nodata": -1, "crs": "EPSG:4326"}),
    );
    assert_eq!(a, b);
    // And the same cells: every value, the no-data ones too.
    let [ga, gb] = [(&ss[0], &a), (&ss[1], &b)]
        .map(|(s, v)| s.map().layer(v["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap());
    assert_eq!((ga.width, ga.height), (gb.width, gb.height));
    let cells = |g: &rustgis_data::raster::Raster| {
        (0..g.height)
            .flat_map(|y| (0..g.width).map(move |x| (x, y)))
            .map(|(x, y)| g.value(0, i64::from(x), i64::from(y)))
            .collect::<Vec<_>>()
    };
    let (ca, cb) = (cells(&ga), cells(&gb));
    assert!(ca.iter().any(Option::is_some) && ca.iter().any(Option::is_none), "burned and empty cells");
    assert_eq!(ca, cb);
}

/// Calculating a geometry property into a field, and `$area`/`$length` in a field calculation,
/// give the same values.
#[test]
fn a_held_layers_measures_are_calculated_alike() {
    let mut ss = both();
    let steps = [
        ("edit.addField", json!({"layer": "Parcels", "name": "a", "type": "double"})),
        ("edit.addField", json!({"layer": "Parcels", "name": "b", "type": "double"})),
        (
            "edit.calculateGeometry",
            json!({"layer": "Parcels", "field": "a", "property": "area", "unit": "m2", "scope": "all", "overwrite": true}),
        ),
        ("edit.calculateField", json!({"layer": "Parcels", "field": "b", "expression": "$area"})),
        ("edit.addField", json!({"layer": "Wells", "name": "x", "type": "double"})),
        (
            "edit.calculateGeometry",
            json!({"layer": "Wells", "field": "x", "property": "x", "unit": "deg", "scope": "all", "overwrite": true}),
        ),
        ("edit.addField", json!({"layer": "Wells", "name": "e", "type": "double"})),
        (
            "edit.calculateGeometry",
            json!({"layer": "Wells", "field": "e", "property": "x", "unit": "projected", "epsg": 25832, "scope": "all", "overwrite": true}),
        ),
        ("edit.addField", json!({"layer": "Roads", "name": "l", "type": "double"})),
        ("edit.calculateField", json!({"layer": "Roads", "field": "l", "expression": "$length"})),
    ];
    for (cmd, p) in steps {
        run_both(&mut ss, cmd, &p);
    }
    for (layer, field) in [("Parcels", "a"), ("Parcels", "b"), ("Wells", "x"), ("Wells", "e"), ("Roads", "l")] {
        let [a, b] = [&ss[0], &ss[1]].map(|s| {
            let ds = &s.map().layer_by_name(layer).unwrap().dataset;
            let f = ds.field_index(field).unwrap();
            (0..ds.features.len()).map(|r| ds.value(r, f).as_f64().unwrap_or(f64::NAN)).collect::<Vec<_>>()
        });
        assert_numbers_close(&a, &b, 1e-6 * a.iter().fold(1.0, |m: f64, v| m.max(v.abs())), &format!("{layer}.{field}"));
    }
}

/// A layer made from the selection, labels converted to annotation, an interpolated surface, a
/// route along the roads and points simulated in the parcels are alike.
#[test]
fn a_held_layer_feeds_other_layers_alike() {
    let mut ss = both();
    run_both(&mut ss, "select.rows", &json!({"layer": "Wells", "rows": [0, 4, 8]}));
    let [a, b] = run_both(&mut ss, "select.layerFromSelection", &json!({"layer": "Wells"}));
    let made = |s: &Session, v: &Value| s.map().layer(v["layer"].as_u64().unwrap() as u32).unwrap().name.clone();
    let (na, nb) = (made(&ss[0], &a), made(&ss[1], &b));
    assert_close(&lonlat(&ss[0], &na), &lonlat(&ss[1], &nb), "layer from selection");
    run_both(&mut ss, "labels.enable", &json!({"layer": "Parcels", "on": true}));
    run_both(&mut ss, "select.clear", &Value::Null);
    let [a, b] = run_both(&mut ss, "labels.convert", &json!({"layer": "Parcels"}));
    let (na, nb) = (made(&ss[0], &a), made(&ss[1], &b));
    let [pa, pb] = [lonlat(&ss[0], &na), lonlat(&ss[1], &nb)];
    assert!(!pa.is_empty(), "no annotation");
    assert_eq!(pa.len(), pb.len(), "annotation count");
    // A label's place is its polygon's label point, found in each system: within a centimetre.
    for (p, q) in pa.iter().zip(&pb) {
        let (Some(Geometry::Point(p)), Some(Geometry::Point(q))) = (p, q) else { panic!("points") };
        assert!((p.x - q.x).abs() < 1e-7 && (p.y - q.y).abs() < 1e-7, "annotation: {p:?} vs {q:?}");
    }
    // Cells off the wells' box's edges (the held layer's is a hair larger: see the extent above).
    let [a, b] = run_both(&mut ss, "analysis.interpolate", &json!({"layer": "Wells", "field": "n", "cellSize": 0.0015}));
    let grid =
        |s: &Session, v: &Value| s.map().layer(v["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    let (ga, gb) = (grid(&ss[0], &a), grid(&ss[1], &b));
    assert_eq!((ga.width, ga.height), (gb.width, gb.height));
    let [va, vb] = [&ga, &gb].map(|g| {
        (0..g.height)
            .flat_map(|y| (0..g.width).map(move |x| (x, y)))
            .map(|(x, y)| f64::from(g.value(0, i64::from(x), i64::from(y)).unwrap_or(f32::NAN)))
            .collect::<Vec<_>>()
    });
    assert_numbers_close(&va, &vb, 1e-3, "interpolation");
    // From and to off the road's vertices, which a held layer reads a hair away from them.
    let route = json!({"layer": "Roads", "from": [11.4951, 48.6925], "to": [11.5295, 48.7290], "snapDistance": 100});
    let [a, b] = run_both(&mut ss, "analysis.route", &route);
    assert_close(&lonlat(&ss[0], &made(&ss[0], &a)), &lonlat(&ss[1], &made(&ss[1], &b)), "route");
    let [a, b] = run_both(&mut ss, "analysis.simulate", &json!({"layer": "Parcels", "count": 5, "seed": 3}));
    assert_eq!(a["points"], b["points"]);
    assert_close(&lonlat(&ss[0], &made(&ss[0], &a)), &lonlat(&ss[1], &made(&ss[1], &b)), "simulation");
}

/// An offline area's edits sync back alike, also when they come back in longitude/latitude (as a
/// field package's do) to a source held in UTM: a moved well and a changed value reach the
/// source, and it keeps every place the edits didn't touch exactly as it held it.
#[test]
fn a_held_layers_offline_edits_sync_alike() {
    let mut ss = both();
    let before: Vec<Option<Geometry>> =
        ss[1].map().layer_by_name("Wells").unwrap().dataset.features.iter().map(|f| f.geometry.clone()).collect();
    run_both(&mut ss, "offline.download", &json!({"name": "Field", "extent": [11.4, 48.6, 11.6, 48.8]}));
    run_both(&mut ss, "offline.open", &json!({"name": "Field"}));
    let g = json!({"type": "Point", "coordinates": [11.505, 48.705]});
    run_both(&mut ss, "edit.setGeometry", &json!({"layer": "Wells", "row": 0, "geometry": g}));
    run_both(&mut ss, "edit.setValue", &json!({"layer": "Wells", "row": 1, "field": "n", "value": 99}));
    run_both(&mut ss, "map.activate", &json!({"name": "Map"}));
    ss[0].run("offline.sync", &json!({"name": "Field"})).unwrap();
    // The held session's edits come back as a field package brings them: in longitude/latitude,
    // beside a source and a last synced copy held in UTM.
    let mut returned = ss[1].project.maps.iter().find(|m| m.name == "Field (offline)").unwrap().clone();
    for l in &mut returned.layers {
        l.dataset = l.dataset.in_lonlat().unwrap().into_owned();
    }
    let ai = ss[1].project.offline_areas.iter().position(|a| a.name == "Field").unwrap();
    crate::cmd::workspace::sync_area(&mut ss[1], ai, &returned).unwrap();
    assert_close(&lonlat(&ss[0], "Wells"), &lonlat(&ss[1], "Wells"), "synced");
    let held = &ss[1].map().layer_by_name("Wells").unwrap().dataset;
    assert_eq!(held.coord_crs(), UTM);
    let n = held.field_index("n").unwrap();
    assert_eq!(held.value(1, n), &Cell::Int(99));
    // Feature 0 moved; every other place is as it was held, to the bit.
    let after: Vec<Option<Geometry>> = held.features.iter().map(|f| f.geometry.clone()).collect();
    assert_eq!(after.len(), before.len());
    assert_ne!(after[0], before[0]);
    assert_eq!(after[1..], before[1..]);
}

/// An offline map held in the source's own system, as the project keeps it, is compared with its
/// area's last synced copy as it is held: a feature not edited offline leaves a source edit made
/// since in place, and one edited offline replaces the source's.
#[test]
fn a_held_offline_map_keeps_the_sources_later_edits() {
    let mut s = session(UTM);
    s.run("offline.download", &json!({"name": "Field", "extent": [11.4, 48.6, 11.6, 48.8]})).unwrap();
    s.run("offline.open", &json!({"name": "Field"})).unwrap();
    s.run("edit.setValue", &json!({"layer": "Wells", "row": 1, "field": "n", "value": 99})).unwrap();
    s.run("map.activate", &json!({"name": "Map"})).unwrap();
    // The source's own edit, made after the download, of a feature the offline map left alone.
    s.run("edit.setValue", &json!({"layer": "Wells", "row": 0, "field": "n", "value": 7})).unwrap();
    let returned = s.project.maps.iter().find(|m| m.name == "Field (offline)").unwrap().clone();
    assert_eq!(returned.layers.iter().find(|l| l.name == "Wells").unwrap().dataset.coord_crs(), UTM);
    let ai = s.project.offline_areas.iter().position(|a| a.name == "Field").unwrap();
    crate::cmd::workspace::sync_area(&mut s, ai, &returned).unwrap();
    let wells = &s.map().layer_by_name("Wells").unwrap().dataset;
    let n = wells.field_index("n").unwrap();
    assert_eq!(wells.value(0, n), &Cell::Int(7), "an unedited offline copy replaced the source's edit");
    assert_eq!(wells.value(1, n), &Cell::Int(99));
    assert_eq!(wells.coord_crs(), UTM);
}

/// A web scene leaves out, and says, a feature of a held layer with a place that can't be given
/// in longitude/latitude, rather than failing the whole scene.
#[test]
fn a_web_scene_leaves_out_what_it_cannot_place() {
    let mut s = session(UTM);
    let dir = Dir::new("unplaced");
    let left_out = "left out of the scene: a place can't be given in longitude/latitude";
    // Every place converts: nothing is left out, nor said to be.
    let mark = s.notice_mark();
    let clean = dir.path().join("clean.gltf");
    s.run("share.webScene", &json!({"path": clean.display().to_string()})).unwrap();
    assert!(clean.exists());
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(!said.iter().any(|n| n.contains(left_out)), "{said:?}");
    // Far beyond any place UTM 32N holds.
    s.map_mut().layers.iter_mut().find(|l| l.name == "Wells").unwrap().dataset.push(Some(Geometry::Point(Coord::new(1e12, 1e12))), vec![]);
    let mark = s.notice_mark();
    let path = dir.path().join("scene.gltf");
    s.run("share.webScene", &json!({"path": path.display().to_string()})).unwrap();
    assert!(path.exists());
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(
        said.iter().any(|n| n.contains("1 feature(s) left out of the scene: a place can't be given in longitude/latitude")),
        "{said:?}"
    );
}

/// A held layer's feature with a place that can't be given in longitude/latitude is drawn without
/// it, and that is said: of the map, once it was drawn, in one notice for every such layer, once
/// for each version of a layer's geometry; and of an export that drew it. Layers whose places
/// all convert say nothing.
#[test]
fn a_held_layers_undrawable_features_are_said_once() {
    let mut s = session(UTM);
    let dir = Dir::new("undrawable");
    let said_of = |s: &Session, mark, what: &str| -> Vec<String> {
        s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains(what)).collect()
    };
    let of_map = "can't be given in longitude/latitude: the map";
    let of_export = "can't be given in longitude/latitude, drawn without it";
    // The screen's frame: drawn on the session's map, then looked at after a command.
    let frame = |s: &mut Session| {
        let view = crate::render::View { width: 160.0, height: 100.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
        crate::render::render(s.map_mut(), &view);
        let _ = s.run("select.clear", &Value::Null);
    };
    let export = |s: &mut Session| {
        let path = dir.path().join("map.png");
        s.run("share.exportMap", &json!({"path": path.display().to_string(), "width": 160, "height": 100})).unwrap();
    };
    let mark = s.notice_mark();
    frame(&mut s);
    export(&mut s);
    assert!(said_of(&s, mark, "longitude/latitude").is_empty(), "{:?}", said_of(&s, mark, "longitude/latitude"));
    // Far beyond any place UTM 32N holds.
    for name in ["Wells", "Roads"] {
        let l = s.map_mut().layers.iter_mut().find(|l| l.name == name).unwrap();
        let far = Coord::new(1e12, 1e12);
        let g = if name == "Wells" { Geometry::Point(far) } else { Geometry::Line(vec![vec![far, Coord::new(far.x + 1.0, far.y)]]) };
        l.dataset.push(Some(g), vec![]);
        l.touch_geometry();
    }
    // Not drawn yet: nothing found, nothing said.
    let mark = s.notice_mark();
    let _ = s.run("select.clear", &Value::Null);
    assert!(said_of(&s, mark, of_map).is_empty());
    // An export says what it drew without, of itself.
    export(&mut s);
    let said = said_of(&s, mark, of_export);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains(": 2 feature(s)") && said[0].ends_with("(map 'Map', layer 'Roads' (1); map 'Map', layer 'Wells' (1))"),
        "{said:?}"
    );
    // The map: said after the frame that drew it, once, however often it is drawn again.
    frame(&mut s);
    assert_eq!(
        said_of(&s, mark, of_map),
        [
            "2 feature(s) have a place that can't be given in longitude/latitude: the map and the globe draw them without it, and labels' lines, block diagrams and terrain water leave them out (map 'Map', layer 'Roads' (1); map 'Map', layer 'Wells' (1))"
        ]
    );
    frame(&mut s);
    assert_eq!(said_of(&s, mark, of_map).len(), 1);
    // A layer the export doesn't draw (hidden) isn't said of it, though the map's frame counted it and the copy keeps that.
    s.map_mut().layers.iter_mut().find(|l| l.name == "Roads").unwrap().visible = false;
    let hidden = s.notice_mark();
    export(&mut s);
    let said = said_of(&s, hidden, of_export);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains(": 1 feature(s)") && said[0].ends_with("(map 'Map', layer 'Wells' (1))"), "{said:?}");
    s.map_mut().layers.iter_mut().find(|l| l.name == "Roads").unwrap().visible = true;
    // An edit makes another version: said again once it is drawn, with its own count.
    let wells = s.map_mut().layers.iter_mut().find(|l| l.name == "Wells").unwrap();
    wells.dataset.push(Some(Geometry::Point(Coord::new(2e12, 2e12))), vec![]);
    wells.touch_geometry();
    let mark = s.notice_mark();
    frame(&mut s);
    let said = said_of(&s, mark, of_map);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("2 feature(s)") && said[0].ends_with("(map 'Map', layer 'Wells' (2))"), "{said:?}");
    // A version drawn first on a map in the layers' own system, which draws every place, says
    // nothing; the globe's copy, which leaves them out, says so at the same version; going back
    // to Web Mercator, which leaves out as many, says nothing more.
    s.run("map.projection", &json!({"type": "EPSG:32632"})).unwrap();
    s.map_mut().layers.iter_mut().find(|l| l.name == "Wells").unwrap().touch_geometry();
    let mark = s.notice_mark();
    frame(&mut s);
    assert!(said_of(&s, mark, of_map).is_empty(), "{:?}", said_of(&s, mark, of_map));
    s.map_mut().layers.iter_mut().find(|l| l.name == "Wells").unwrap().lonlat_in();
    let _ = s.run("select.clear", &Value::Null);
    let said = said_of(&s, mark, of_map);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].ends_with("(map 'Map', layer 'Wells' (2))"), "{said:?}");
    s.run("map.projection", &json!({"type": "mercator"})).unwrap();
    frame(&mut s);
    s.run("map.projection", &json!({"type": "EPSG:32632"})).unwrap();
    frame(&mut s);
    assert_eq!(said_of(&s, mark, of_map).len(), 1, "{:?}", said_of(&s, mark, of_map));
}

/// Every cell of a raster layer named in a command's answer.
fn raster_cells(s: &Session, answer: &Value) -> (u32, u32, Vec<Option<f32>>) {
    let id = answer["layer"].as_u64().unwrap_or_else(|| panic!("no layer in {answer}")) as u32;
    let g = s.map().layer(id).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    let cells =
        (0..g.height).flat_map(|y| (0..g.width).map(move |x| (x, y))).map(|(x, y)| g.value(0, i64::from(x), i64::from(y))).collect();
    (g.width, g.height, cells)
}

/// Rasters read through a held polygon layer read the same pixels: a spectral zone, a clip, and
/// training polygons for a classification.
#[test]
fn a_held_polygon_layer_reads_rasters_alike() {
    let mut ss = both();
    run_both(&mut ss, "raster.rasterize", &json!({"layer": "Parcels", "field": "n", "cellSize": 0.0007, "nodata": -1, "crs": "EPSG:4326"}));
    let raster = "Parcels raster";
    let [a, b] = run_both(&mut ss, "raster.spectral", &json!({"layer": raster, "zone": "Parcels"}));
    assert_eq!(a["pixels"], b["pixels"], "spectral zone");
    assert!(a["pixels"].as_u64().unwrap() > 0);
    assert_numbers_close(&numbers(&a["values"].to_string()), &numbers(&b["values"].to_string()), 1e-6, "spectral zone means");
    let [a, b] =
        run_both(&mut ss, "raster.process", &json!({"layer": raster, "operation": "clip", "polygon": "Parcels", "name": "clipped"}));
    let (ra, rb) = (raster_cells(&ss[0], &a), raster_cells(&ss[1], &b));
    assert!(ra.2.iter().any(Option::is_some), "the clip keeps cells");
    assert_eq!(ra, rb, "clip");
    let [a, b] = run_both(
        &mut ss,
        "raster.classifySupervised",
        &json!({"layer": raster, "training": "Parcels", "classField": "n", "name": "classes"}),
    );
    let (ra, rb) = (raster_cells(&ss[0], &a), raster_cells(&ss[1], &b));
    assert!(ra.2.iter().any(Option::is_some), "the classification fills cells");
    assert_eq!(ra, rb, "supervised classification");
}

/// A photo added to a photo layer held in another system is placed there: it reads back at the
/// photo's position, as in a photo layer held in longitude/latitude.
#[test]
fn a_photo_joins_a_held_photo_layer_at_its_place() {
    let mut ss = both();
    let photo = |lat: f64, lon: f64| rustgis_data::photo::example(true, (lat, "N"), (lon, "E"));
    for s in &mut ss {
        let first = photo(48.71, 11.51);
        crate::cmd::layer::add_photos_to(s, "Photos", Some(false), |r| r.add("a.jpg", &first)).unwrap();
    }
    // The held session's photo layer is held in UTM from here on, as a file's would be.
    let held = ss[1].map_mut().layers.iter_mut().find(|l| l.name == "Photos").unwrap();
    held.dataset.hold_in(UTM).unwrap();
    held.touch_geometry();
    for s in &mut ss {
        let second = photo(48.72, 11.52);
        crate::cmd::layer::add_photos_to(s, "Photos", Some(false), |r| r.add("b.jpg", &second)).unwrap();
    }
    assert_eq!(ss[1].map().layer_by_name("Photos").unwrap().dataset.coord_crs(), UTM);
    let (a, b) = (lonlat(&ss[0], "Photos"), lonlat(&ss[1], "Photos"));
    assert_eq!(a.len(), 2);
    assert_close(&a, &b, "photos");
}

/// Pixels of two PNGs that differ, and those the first draws (unlike its corner).
fn png_difference(a: &std::path::Path, b: &std::path::Path) -> (usize, usize) {
    let (a, b) = (image::open(a).unwrap().to_rgba8(), image::open(b).unwrap().to_rgba8());
    assert_eq!(a.dimensions(), b.dimensions());
    let differ = a.pixels().zip(b.pixels()).filter(|(x, y)| x != y).count();
    let drawn = a.pixels().filter(|p| p.0 != a.get_pixel(0, 0).0).count();
    (differ, drawn)
}

/// A layer masked by a held polygon layer is drawn alike, on Web Mercator and on the globe.
#[test]
fn a_held_mask_is_drawn_alike() {
    let mut ss = both();
    let dir = Dir::new("mask");
    run_both(&mut ss, "layer.mask", &json!({"layer": "Roads", "mask": "Parcels"}));
    for (k, projection) in ["mercator", "globe"].into_iter().enumerate() {
        let camera = camera_over_parcels(projection);
        for s in &mut ss {
            s.run("map.projection", &json!({"type": projection})).unwrap();
            s.map_mut().camera = camera;
        }
        let paths = [0, 1].map(|i| dir.path().join(format!("{k}-{i}.png")));
        for (s, path) in ss.iter_mut().zip(&paths) {
            s.run("share.exportMap", &json!({"path": path.display().to_string(), "width": 320, "height": 200})).unwrap();
        }
        let (differ, drawn) = png_difference(&paths[0], &paths[1]);
        assert!(drawn > 1000, "{projection}: little drawn ({drawn} pixels)");
        assert!(differ * 100 < drawn, "{projection}: {differ} of {drawn} drawn pixels differ");
    }
}

/// A publication layout's globe locator sketches held polygon layers alike.
#[test]
fn a_held_layers_layout_locator_is_drawn_alike() {
    let mut ss = both();
    let dir = Dir::new("locator");
    let camera = camera_over_parcels("mercator");
    for s in &mut ss {
        s.map_mut().camera = camera;
    }
    run_both(&mut ss, "layout.publication", &json!({"profile": "a5", "locator": "globe", "title": "Held"}));
    let paths = [0, 1].map(|i| dir.path().join(format!("page-{i}.png")));
    for (s, path) in ss.iter_mut().zip(&paths) {
        s.run("layout.export", &json!({"layout": 0, "path": path.display().to_string(), "dpi": 36})).unwrap();
    }
    let (differ, drawn) = png_difference(&paths[0], &paths[1]);
    assert!(drawn > 1000, "little drawn ({drawn} pixels)");
    assert!(differ * 100 < drawn, "{differ} of {drawn} drawn pixels differ");
    // The locator's map keeps a sketch of the polygons, in longitude/latitude: too small on the
    // globe to tell apart in the picture, so compared as places.
    let sketch = |s: &Session| -> Vec<Option<Geometry>> {
        let m = s.project.maps.iter().find(|m| m.name == "Map Locator").expect("the locator's map");
        let l = m.layers.iter().find(|l| l.name == "Parcels").expect("the parcels' sketch");
        assert_eq!(l.dataset.coord_crs(), Crs::LonLat);
        l.dataset.features.iter().map(|f| f.geometry.clone()).collect()
    };
    let (a, b) = (sketch(&ss[0]), sketch(&ss[1]));
    assert!(!a.is_empty());
    assert_close(&a, &b, "locator sketch");
}

/// A layout's export says the undrawable places of the maps it shows only: a map it doesn't show
/// keeps its count from the screen's frame, which the export doesn't say.
#[test]
fn a_layout_says_only_the_undrawable_places_of_its_maps() {
    let mut s = session(UTM);
    let dir = Dir::new("layout");
    let wells = s.map_mut().layers.iter_mut().find(|l| l.name == "Wells").unwrap();
    wells.dataset.push(Some(Geometry::Point(Coord::new(1e12, 1e12))), vec![]);
    wells.touch_geometry();
    // The screen's frame counts the first map's place.
    let view = crate::render::View { width: 160.0, height: 100.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
    crate::render::render(s.map_mut(), &view);
    let said = |s: &Session, mark| -> Vec<String> {
        s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("drawn without it")).collect()
    };
    let export = |s: &mut Session, name: &str| {
        let path = dir.path().join(name);
        s.run("layout.export", &json!({"path": path.display().to_string(), "dpi": 36})).unwrap();
    };
    // A layout of another map: nothing said.
    s.run("map.new", &json!({"name": "Second"})).unwrap();
    s.run("map.activate", &json!({"name": "Second"})).unwrap();
    s.run("layout.new", &json!({"page": "a5", "template": "map"})).unwrap();
    let mark = s.notice_mark();
    export(&mut s, "second.png");
    assert!(said(&s, mark).is_empty(), "{:?}", said(&s, mark));
    // A layout of the first map: said.
    s.run("map.activate", &json!({"name": "Map"})).unwrap();
    s.run("layout.new", &json!({"page": "a5", "template": "map"})).unwrap();
    let mark = s.notice_mark();
    export(&mut s, "first.png");
    let first = said(&s, mark);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].ends_with("(map 'Map', layer 'Wells' (1))"), "{first:?}");
}

/// A held layer a frame reads without drawing it (here the terrain's water, hidden, read by a
/// tilted view) has its undrawable places counted and said, as a drawn layer's are: by an export
/// that read it, and of the map after the screen's frame. A frame that doesn't read it (on a
/// globe, or flat and north up with no shaded relief) says nothing of it.
#[test]
fn a_hidden_held_water_layers_undrawable_places_are_said() {
    let mut s = session(UTM);
    let dir = Dir::new("water");
    let parcels = s.map_mut().layers.iter_mut().find(|l| l.name == "Parcels").unwrap();
    let far = |x: f64| Coord::new(1e12 + x, 1e12);
    parcels.dataset.push(Some(Geometry::Polygon(vec![vec![vec![far(0.0), far(1.0), far(2.0), far(0.0)]]])), vec![]);
    parcels.touch_geometry();
    parcels.visible = false;
    // Heights in Web Mercator round the parcels, without which terrain water isn't read.
    let gt = rustgis_data::raster::GeoTransform { origin_x: 1.2e6, origin_y: 6.2e6, pixel_width: 1e4, pixel_height: -1e4 };
    let dem = rustgis_data::raster::Raster::new(
        "dem",
        4,
        4,
        vec![vec![10.0; 16]],
        gt,
        Crs::WebMercator,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    s.map_mut().add_raster("dem", rustgis_map::raster::RasterLayer::from_path(dem, "dem.tif"));
    s.map_mut().view.terrain = Some(rustgis_map::view::Terrain {
        source: rustgis_map::view::TerrainSource::Layer("dem".into()),
        exaggeration: 1.0,
        block: None,
        water: Some(rustgis_map::view::TerrainWater { layer: "Parcels".into(), level: None, glint: 0.0, shore: 0.0 }),
    });
    let said = |s: &Session, mark, what: &str| -> Vec<String> {
        s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains(what)).collect()
    };
    let export = |s: &mut Session, name: &str| {
        let path = dir.path().join(name);
        s.run("share.exportMap", &json!({"path": path.display().to_string(), "width": 160, "height": 100})).unwrap();
    };
    let frame = |s: &mut Session| {
        let view = crate::render::View { width: 160.0, height: 100.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
        crate::render::render(s.map_mut(), &view);
        let _ = s.run("select.clear", &Value::Null);
    };
    // Flat and north up, then on a globe: no terrain water is read, so nothing is said of it.
    let mark = s.notice_mark();
    export(&mut s, "flat.png");
    frame(&mut s);
    s.map_mut().projection = rustgis_map::Projection::Globe;
    export(&mut s, "globe.png");
    frame(&mut s);
    assert!(said(&s, mark, "Parcels").is_empty(), "{:?}", said(&s, mark, "Parcels"));
    s.map_mut().projection = rustgis_map::Projection::WebMercator;
    // Tilted, the ground reads the water: the export says what it left out.
    s.map_mut().camera.pitch = 40.0;
    let mark = s.notice_mark();
    export(&mut s, "tilted.png");
    let drawn = said(&s, mark, "drawn without it");
    assert_eq!(drawn.len(), 1, "{drawn:?}");
    assert!(drawn[0].ends_with("(map 'Map', layer 'Parcels' (1))"), "{drawn:?}");
    // The screen's frame, then a command: the map's notice names the water layer.
    let mark = s.notice_mark();
    frame(&mut s);
    let of_map = said(&s, mark, "can't be given in longitude/latitude: the map");
    assert_eq!(of_map.len(), 1, "{of_map:?}");
    assert!(of_map[0].ends_with("(map 'Map', layer 'Parcels' (1))"), "{of_map:?}");
}

/// On a map drawn in another system than the layer's, on another datum (DHDN 3-degree Gauss-Krüger
/// zone 3 on ETRS89 UTM 32N, whose direct operation is some decimetres from the way through WGS 84:
/// 0.37 m at Stuttgart in PROJ 9.9.0, see the map's
/// `a_held_layer_is_drawn_straight_into_a_map_in_a_coordinate_system`), the places edits give and
/// take are those the map draws: a vertex's handle is where the map draws the vertex, and a vertex
/// moved to where the map draws it stays where it was stored. Calculate Geometry's coordinates are
/// the layer's own through WGS 84, whatever map.
#[test]
fn edits_on_a_map_in_another_datums_system_land_on_what_it_draws() {
    let gk = Crs::Epsg(31467);
    let mut s = session(gk);
    s.run("map.projection", &json!({"type": "EPSG:25832"})).unwrap();
    let flat = s.map().flat();
    assert_eq!(flat.direct_system(), Some(Crs::Epsg(25832)));
    let id = s.map().layer_by_name("Roads").unwrap().id;
    let stored = s.map().layer(id).unwrap().dataset.features[0].geometry.clone().unwrap();
    let drawn = s.map_mut().layer_mut(id).unwrap().projected_in(&flat, "test")[0].clone().unwrap();
    let (Geometry::Line(stored_parts), Geometry::Line(drawn_parts)) = (&stored, &drawn) else { panic!("a line") };
    let l = s.map().layer(id).unwrap();
    assert_eq!(l.dataset.shown_in, Some(Crs::Epsg(25832)), "the map told its layer its system");
    let Geometry::Line(handles) = &*l.dataset.geometry_to_lonlat(&stored).unwrap() else { panic!("a line") };
    let exact = l.dataset.lonlat_conversion().unwrap();
    for ((h, d), c) in handles[0].iter().zip(&drawn_parts[0]).zip(&stored_parts[0]) {
        let at = flat.forward_checked(*h).unwrap();
        assert!((at.x - d.x).hypot(at.y - d.y) < 1e-6, "handle at {at:?}, vertex drawn at {d:?}");
        // Through WGS 84 the handle would be off the line.
        let via = flat.forward_checked(exact.point(*c).unwrap()).unwrap();
        assert!((via.x - d.x).hypot(via.y - d.y) > 0.1, "{via:?} vs {d:?}");
    }
    // A vertex moved to where the map draws it, as a drag there gives it, keeps its place.
    let at = flat.inverse(drawn_parts[0][1]).unwrap();
    s.run("edit.vertex", &json!({"layer": "Roads", "row": 0, "action": "move", "index": 1, "lon": at.x, "lat": at.y})).unwrap();
    let Some(Geometry::Line(after)) = &s.map().layer(id).unwrap().dataset.features[0].geometry else { panic!("a line") };
    let (a, b) = (after[0][1], stored_parts[0][1]);
    assert!((a.x - b.x).hypot(a.y - b.y) < 1e-6, "moved to {a:?} from {b:?}");
    // Analysis reads the layer through WGS 84, as on any map.
    let g = rustgis_data::Dataset::geometry_to_lonlat_by(&exact, &stored).unwrap();
    s.run("map.projection", &json!({"type": "mercator"})).unwrap();
    let l = s.map().layer(id).unwrap();
    assert_eq!(l.dataset.shown_in, None);
    assert_eq!(*l.dataset.geometry_to_lonlat(&stored).unwrap(), *g, "on Web Mercator, edits go through WGS 84");
}

/// A label moved on a map in another datum's system becomes an annotation where it was drawn:
/// at the layer's coordinates the map's direct operation puts there, not those WGS 84 gives
/// (some decimetres off between DHDN and ETRS89).
#[test]
fn a_moved_label_becomes_an_annotation_where_it_was_drawn() {
    let gk = Crs::Epsg(31467);
    let mut s = session(gk);
    s.run("map.projection", &json!({"type": "EPSG:25832"})).unwrap();
    let flat = s.map().flat();
    assert_eq!(flat.direct_system(), Some(Crs::Epsg(25832)));
    let id = s.map().layer_by_name("Wells").unwrap().id;
    let drawn = s.map_mut().layer_mut(id).unwrap().projected_in(&flat, "test")[2].clone().unwrap();
    let Geometry::Point(drawn) = drawn else { panic!("a point") };
    let l = s.map().layer(id).unwrap();
    let (moved, target) = (l.dataset.features[1].id, l.dataset.features[2].geometry.clone().unwrap());
    let Geometry::Point(target) = target else { panic!("a point") };
    // Put on the map where well 2 is drawn, as a drag there gives it.
    let put = flat.inverse(drawn).unwrap();
    s.run("labels.enable", &json!({"layer": "Wells", "on": true})).unwrap();
    s.run("labels.move", &json!({"layer": "Wells", "feature": moved, "to": [put.x, put.y]})).unwrap();
    let made = s.run("labels.convert", &json!({"layer": "Wells"})).unwrap();
    let a = s.map().layer(made["layer"].as_u64().unwrap() as u32).unwrap();
    assert_eq!(a.dataset.coord_crs(), gk, "held as the labelled layer is");
    let Some(Geometry::Point(at)) = a.dataset.features[1].geometry else { panic!("a point") };
    // Within the millimetre two inverses of the datum shift take: the direct one, then through
    // WGS 84 as `hold_as` reads the annotation back. PROJ's own round trip moves it 0.85 mm:
    // `echo 5401067.822206589 3684021.1657746523 | cs2cs -d 10 EPSG:31467 EPSG:25832` gives
    // 683871.9767392263 5399346.3693805207, which `cs2cs -d 10 EPSG:25832 EPSG:31467` turns into
    // 5401067.8229720965 3684021.1661309246 (PROJ 9.9.0, PROJ_NETWORK=OFF).
    assert!((at.x - target.x).hypot(at.y - target.y) < 5e-3, "annotation at {at:?}, label drawn at {target:?}");
    // Through WGS 84 it would be off the drawn label.
    let plain = Crs::LonLat.to(gk).unwrap().point(put).unwrap();
    assert!((plain.x - target.x).hypot(plain.y - target.y) > 0.1, "{plain:?} vs {target:?}");
}

/// A project saved by an older release opens as it did (#2032 C2): a layer it kept in the
/// project holds its features in longitude/latitude, as they were saved, and a layer read in
/// place from a UTM 32N Shapefile is read again in the file's own system, at the same places.
/// Both export the file's own numbers to UTM 32N (the kept one within a micrometre, its round
/// trip through longitude/latitude),
/// draw alike on Web Mercator and on a UTM map, and saving the project again writes the kept
/// layer's features as they were saved.
#[test]
fn a_project_saved_by_an_older_release_opens_as_it_did() {
    let dir = Dir::new("older-project");
    let shp = dir.path().join("parcels.shp");
    let mut file = layers().remove(2);
    file.hold_in(UTM).unwrap();
    rustgis_data::io::write_path_in(&file, &shp, UTM).unwrap();
    let numbers = |ds: &Dataset| ds.features.iter().map(|f| f.geometry.clone()).collect::<Vec<_>>();
    // As the file holds them (its rings in the Shapefile's order).
    let read = rustgis_data::io::read_path(&shp).unwrap().0;
    assert_eq!(read.coord_crs(), UTM, "read in the file's own system");
    let in_file = numbers(&read);

    // As an older release saved it: the kept layer's features in longitude/latitude (every release
    // held them there), and the layer read in place by its file's path.
    let mut older = Session::new(Project::default());
    let kept = older.run("layer.addData", &json!({"path": shp.display().to_string(), "name": "Kept"})).unwrap();
    let kept = kept["layer"].as_u64().unwrap() as u32;
    let lonlat = older.map().layer(kept).unwrap().dataset.in_lonlat().unwrap().into_owned();
    older.map_mut().layer_mut(kept).unwrap().dataset = lonlat.clone();
    older.run("layer.addData", &json!({"path": shp.display().to_string(), "name": "Streamed", "stream": true})).unwrap();
    let project = dir.path().join("older.rgis");
    older.run("project.saveAs", &json!({"path": project.display().to_string()})).unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&project).unwrap()).unwrap();
    let saved_kept = saved["maps"][0]["layers"].as_array().unwrap().iter().find(|l| l["name"] == "Kept").unwrap();
    let ring = &saved_kept["dataset"]["features"][0]["geometry"]["Polygon"][0][0][0];
    let x = ring["x"].as_f64().unwrap();
    assert!((11.0..12.0).contains(&x), "saved in longitude/latitude: {ring}");

    let mut s = Session::new(Project::default());
    s.run("project.open", &json!({"path": project.display().to_string()})).unwrap();
    s.wait_in_place_reads();
    let kept = s.map().layer_by_name("Kept").unwrap().id;
    let streamed = s.map().layer_by_name("Streamed").unwrap().id;
    // Kept: as saved, in longitude/latitude, its system remembered for exports.
    let k = &s.map().layer(kept).unwrap().dataset;
    assert_eq!(k.coord_crs(), Crs::LonLat);
    assert_eq!(k.crs, Some(UTM));
    assert_eq!(numbers(k), numbers(&lonlat));
    // Read in place: the file's own numbers, at the same places.
    let whole = s.map().layer(streamed).unwrap().load_all().unwrap();
    assert_eq!(whole.coord_crs(), UTM);
    assert_eq!(numbers(&whole), in_file);
    assert_close(&numbers(&whole.in_lonlat().unwrap()), &numbers(&lonlat), "read in place");
    let (ek, es) = (k.lonlat_extent(), whole.lonlat_extent());
    for (a, b) in [(ek.min_x, es.min_x), (ek.min_y, es.min_y), (ek.max_x, es.max_x), (ek.max_y, es.max_y)] {
        assert!((a - b).abs() < CLOSE, "extents {ek:?} {es:?}");
    }

    // Both export the file's numbers.
    for name in ["Kept", "Streamed"] {
        let out = dir.path().join(format!("{name}.shp"));
        s.run("share.exportLayer", &json!({"layer": name, "path": out.display().to_string(), "crs": "EPSG:32632"})).unwrap();
        let back = rustgis_data::io::read_path(&out).unwrap().0;
        assert_eq!(back.coord_crs(), UTM, "{name}");
        let (mut a, mut b) = (Vec::new(), Vec::new());
        numbers(&back).iter().flatten().for_each(|g| g.for_each_coord(|c| a.push(c)));
        in_file.iter().flatten().for_each(|g| g.for_each_coord(|c| b.push(c)));
        assert_eq!(a.len(), b.len(), "{name}");
        for (p, q) in a.iter().zip(&b) {
            assert!((p.x - q.x).hypot(p.y - q.y) < 1e-6, "{name}: {p:?} {q:?}");
        }
    }

    // Both draw alike, on Web Mercator and on a map in the file's system.
    for (k, projection) in ["mercator", "EPSG:32632"].into_iter().enumerate() {
        s.run("map.projection", &json!({"type": projection})).unwrap();
        s.run("layer.zoomTo", &json!({"layer": "Kept"})).unwrap();
        let paths = [0, 1].map(|i| dir.path().join(format!("{k}-{i}.png")));
        for (shown, path) in [(kept, &paths[0]), (streamed, &paths[1])] {
            let hidden = if shown == kept { streamed } else { kept };
            s.run("layer.visibility", &json!({"layer": hidden, "visible": false})).unwrap();
            s.run("layer.visibility", &json!({"layer": shown, "visible": true})).unwrap();
            s.run("share.exportMap", &json!({"path": path.display().to_string(), "width": 320, "height": 200})).unwrap();
        }
        // Where each draws (the two layers have colours of their own).
        let [a, b] = paths.map(|p| image::open(p).unwrap().to_rgba8());
        let (bg_a, bg_b) = (*a.get_pixel(0, 0), *b.get_pixel(0, 0));
        let drawn = a.pixels().filter(|p| **p != bg_a).count();
        let differ = a.pixels().zip(b.pixels()).filter(|(p, q)| (**p != bg_a) != (**q != bg_b)).count();
        assert!(drawn > 1000, "{projection}: little drawn ({drawn} pixels)");
        assert!(differ * 100 < drawn, "{projection}: {differ} of {drawn} drawn pixels differ");
    }

    // Saved again, the kept layer's features are written as they were saved.
    let again = dir.path().join("again.rgis");
    s.run("project.saveAs", &json!({"path": again.display().to_string()})).unwrap();
    let saved_again: Value = serde_json::from_str(&std::fs::read_to_string(&again).unwrap()).unwrap();
    let again_kept = saved_again["maps"][0]["layers"].as_array().unwrap().iter().find(|l| l["name"] == "Kept").unwrap();
    assert_eq!(again_kept["dataset"]["features"], saved_kept["dataset"]["features"]);
}

/// A project fixture with a layer kept in UTM 32N, its features saved there (#2032 C2d), opens
/// held there, at the places PROJ gives for its points' longitude/latitude: `printf "48.7 11.5\n48.71
/// 11.5\n" | cs2cs -d 6 EPSG:4326 EPSG:32632` gives 683944.533251 5397122.834867 and 683908.073487
/// 5398234.294584 (PROJ 9.9.0, PROJ_NETWORK=OFF).
#[test]
fn the_held_layer_fixture_opens_held() {
    let p = Project::from_json(include_str!("../../../../schema/fixtures/held_native.rgis")).unwrap();
    let wells = &p.maps[0].layers[0].dataset;
    assert_eq!(wells.coord_crs(), UTM);
    let at: Vec<Coord> = wells
        .features
        .iter()
        .map(|f| match f.geometry {
            Some(Geometry::Point(c)) => c,
            ref g => panic!("a point: {g:?}"),
        })
        .collect();
    for (c, (x, y)) in at.iter().zip([(683_944.533_251, 5_397_122.834_867), (683_908.073_487, 5_398_234.294_584)]) {
        assert!((c.x - x).abs() < 1e-6 && (c.y - y).abs() < 1e-6, "{c:?}");
    }
}

/// Every coordinate of a layer as it holds it ([`Dataset::coord_crs`]), feature by feature.
fn held_coords(s: &Session, layer: &str) -> Vec<Vec<Coord>> {
    let l = s.map().layer_by_name(layer).unwrap_or_else(|| panic!("no layer {layer}"));
    l.dataset
        .features
        .iter()
        .map(|f| {
            let mut v = Vec::new();
            if let Some(g) = &f.geometry {
                g.for_each_coord(|c| v.push(c));
            }
            v
        })
        .collect()
}

/// The coordinates as bits, for comparing them exactly.
fn bits(v: &[Vec<Coord>]) -> Vec<(u64, u64)> {
    v.iter().flatten().map(|c| (c.x.to_bits(), c.y.to_bits())).collect()
}

/// A tool that works in its first input's own system (#2032 C2e) is given a layer held in UTM in
/// UTM, not a longitude/latitude copy, and its new layer is held in UTM too: what it copies keeps
/// its stored coordinates bit for bit, and what it makes from them (an envelope) is made along
/// UTM's axes.
#[test]
fn a_tool_works_in_its_held_inputs_own_system() {
    let mut s = session(UTM);
    s.map_mut().layers.iter_mut().for_each(|l| l.dataset.crs = Some(UTM));
    let run = |s: &mut Session, tool: &str, input: &str, out: &str| {
        s.run("gp.run", &json!({"tool": tool, "params": {"input": input, "output": out}})).unwrap_or_else(|e| panic!("{tool}: {e}"));
        let l = s.map().layer_by_name(out).unwrap_or_else(|| panic!("{tool}: no output"));
        assert_eq!(l.dataset.coord_crs(), UTM, "{tool}: held in");
    };
    for (tool, input, out) in [
        ("exportFeatures", "Wells", "Exported"),
        ("polygonToLine", "Parcels", "Outlines"),
        ("repairGeometry", "Parcels", "Repaired"),
        ("featureVerticesToPoints", "Roads", "Vertices"),
        ("splitLineAtVertices", "Roads", "Segments"),
        ("unsplitLine", "Roads", "Unsplit"),
    ] {
        run(&mut s, tool, input, out);
        let (copied, from) = (bits(&held_coords(&s, out)), bits(&held_coords(&s, input)));
        let kept = |c: &(u64, u64)| from.contains(c);
        assert!(!copied.is_empty() && copied.iter().all(kept), "{tool}: its coordinates are the input's, as held");
    }
    // Export Features keeps the input's system as the one exports use (#2032 C2e).
    assert_eq!(s.map().layer_by_name("Exported").unwrap().dataset.crs, Some(UTM));
    // Each envelope is the box of the parcel's stored UTM coordinates, which in longitude/latitude
    // is a turned rectangle, not the box of the parcel's longitudes and latitudes.
    run(&mut s, "featureEnvelopeToPolygon", "Parcels", "Envelopes");
    let parcels = held_coords(&s, "Parcels");
    for (i, envelope) in held_coords(&s, "Envelopes").iter().enumerate() {
        let parcel = parcels.get(i).unwrap();
        let (min_x, max_x) = parcel.iter().fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.x), b.max(c.x)));
        let (min_y, max_y) = parcel.iter().fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.y), b.max(c.y)));
        for c in envelope {
            assert!(c.x == min_x || c.x == max_x, "envelope {i}: x {} of {min_x}..{max_x}", c.x);
            assert!(c.y == min_y || c.y == max_y, "envelope {i}: y {} of {min_y}..{max_y}", c.y);
        }
    }
    // Check Geometry finds nothing wrong with valid squares, in any system.
    s.run("gp.run", &json!({"tool": "checkGeometry", "params": {"input": "Parcels", "output": "Problems"}})).unwrap();
    assert!(held_coords(&s, "Problems").is_empty());
    // A tool working on attributes alone answers as it does for the layer in longitude/latitude.
    let mut ss = both();
    let p = json!({"tool": "selectByAttributes", "params": {"input": "Wells", "where": "n >= 4"}});
    run_both(&mut ss, "gp.run", &p);
    assert_eq!(selected(&ss[0], "Wells"), selected(&ss[1], "Wells"));
    assert_eq!(selected(&ss[1], "Wells"), vec![4, 5, 6, 7, 8]);
    // Summary Statistics and Pivot Table make the same table as for the layer in longitude/latitude.
    let table = |s: &Session| {
        let t = &s.project.tables.last().expect("a table").dataset;
        let cells = (0..t.features.len()).flat_map(|r| (0..t.fields.len()).map(move |f| (r, f))).map(|(r, f)| t.value(r, f).to_string());
        (t.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>(), cells.collect::<Vec<_>>())
    };
    for p in [
        json!({"tool": "summaryStatistics", "params": {"input": "Wells", "field": "n"}}),
        json!({"tool": "pivotTable", "params": {"input": "Wells", "fields": "n", "pivot": "name", "value": "n"}}),
    ] {
        run_both(&mut ss, "gp.run", &p);
        let (a, b) = (table(&ss[0]), table(&ss[1]));
        assert!(!a.1.is_empty(), "{p}: an empty table");
        assert_eq!(a, b, "{p}");
    }
}

/// Merge's other inputs are converted into its first input's own system, where its result is
/// held: the first's coordinates kept bit for bit, the others' where PROJ puts them; with a
/// first input in longitude/latitude, everything stays there as before.
#[test]
fn merge_converts_its_other_inputs_into_the_firsts_system() {
    let mut s = session(UTM);
    let mut plain = Dataset::new("Plain", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    plain.push(Some(Geometry::Point(Coord::new(11.6, 48.75))), vec![Cell::Text("p".into())]);
    s.map_mut().add_dataset(plain);
    let merge = |s: &mut Session, inputs: [&str; 2], out: &str| {
        let p = json!({"tool": "merge", "params": {"inputs": inputs, "attributes": true, "output": out}});
        s.run("gp.run", &p).unwrap_or_else(|e| panic!("merge: {e}"))
    };
    merge(&mut s, ["Wells", "Plain"], "Merged");
    let merged = s.map().layer_by_name("Merged").unwrap();
    assert_eq!(merged.dataset.coord_crs(), UTM);
    let (out, wells) = (held_coords(&s, "Merged"), held_coords(&s, "Wells"));
    assert_eq!(bits(out.get(..9).unwrap()), bits(&wells), "the wells as held");
    // echo 48.75 11.6 | cs2cs -f %.6f EPSG:4326 EPSG:32632 (PROJ 9.9.0): 691112.022414 5402926.247736
    let c = out.get(9).and_then(|v| v.first()).copied().unwrap();
    assert!((c.x - 691_112.022_414).abs() < 1e-3 && (c.y - 5_402_926.247_736).abs() < 1e-3, "{c:?}");
    // The other way round, the run works in longitude/latitude, as every run did before.
    merge(&mut s, ["Plain", "Wells"], "Merged_LonLat");
    let lonlat_merge = s.map().layer_by_name("Merged_LonLat").unwrap();
    assert_eq!(lonlat_merge.dataset.coord_crs(), Crs::LonLat);
    let first = held_coords(&s, "Merged_LonLat").first().and_then(|v| v.first()).copied().unwrap();
    assert_eq!((first.x, first.y), (11.6, 48.75));
}

/// An input with a place the first input's system can't hold (a point a quarter of the way round
/// the Earth from UTM zone 32N's meridian, which PROJ can't convert either) has the run work in
/// longitude/latitude, as every run did before, and its message says why.
#[test]
fn an_input_the_first_ones_system_cant_hold_is_worked_on_in_lonlat() {
    let mut s = session(UTM);
    let mut far = Dataset::new("Far", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    // echo 0 99 | cs2cs EPSG:4326 EPSG:32632 (PROJ 9.9.0): * * inf
    far.push(Some(Geometry::Point(Coord::new(99.0, 0.0))), vec![Cell::Text("far".into())]);
    s.map_mut().add_dataset(far);
    let p = json!({"tool": "merge", "params": {"inputs": ["Wells", "Far"], "attributes": true, "output": "Merged"}});
    let mark = s.notice_mark();
    s.run("gp.run", &p).unwrap();
    let merged = s.map().layer_by_name("Merged").unwrap();
    assert_eq!(merged.dataset.coord_crs(), Crs::LonLat);
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.starts_with("Merge worked in longitude/latitude, not EPSG:32632: Far: feature")), "{said:?}");
    let last = held_coords(&s, "Merged").last().and_then(|v| v.first()).copied().unwrap();
    assert_eq!((last.x, last.y), (99.0, 0.0));
    // A layer the run only names its output after isn't read, so it doesn't decide the system.
    let p = json!({"tool": "exportFeatures", "params": {"input": "Wells", "output": "Far"}});
    let answer = s.run("gp.run", &p).unwrap();
    let id = answer.get("layer").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset.coord_crs(), UTM);
}

/// A standalone table without geometry (a spreadsheet's rows) joins onto a layer held in UTM as it
/// does onto one in longitude/latitude, and the joined layer keeps the parcels' places as held.
#[test]
fn a_table_joins_onto_a_held_layer() {
    let mut owners =
        Dataset::new("Owners", GeometryType::None, vec![Field::new("n", FieldType::Integer), Field::new("owner", FieldType::Text)]);
    for n in 0..9 {
        owners.push(None, vec![Cell::Int(n), Cell::Text(format!("owner {n}"))]);
    }
    let mut ss = both();
    for s in &mut ss {
        s.project.add_table(owners.clone());
    }
    let p = json!({"tool": "joinField", "params": {"target": "Parcels", "targetField": "n", "joinTable": "Owners", "joinField": "n", "output": "Owned"}});
    run_both(&mut ss, "gp.run", &p);
    let owner = |s: &Session| {
        let ds = &s.map().layer_by_name("Owned").unwrap().dataset;
        let f = ds.field_index("owner").unwrap();
        (0..ds.features.len()).map(|r| ds.value(r, f).to_string()).collect::<Vec<_>>()
    };
    assert_eq!(owner(&ss[0]), owner(&ss[1]));
    assert_eq!(owner(&ss[1]).first().map(String::as_str), Some("owner 0"));
    assert_eq!(ss[1].map().layer_by_name("Owned").unwrap().dataset.coord_crs(), UTM);
    assert_eq!(bits(&held_coords(&ss[1], "Owned")), bits(&held_coords(&ss[1], "Parcels")));
    // Named under a name Join Field also accepts for its target, the layer decides the system too.
    let p = json!({"tool": "joinField", "params": {"input": "Parcels", "targetField": "n", "join": "Owners", "joinField": "n", "output": "Owned_2"}});
    ss[1].run("gp.run", &p).unwrap();
    assert_eq!(ss[1].map().layer_by_name("Owned_2").unwrap().dataset.coord_crs(), UTM);
    assert_eq!(bits(&held_coords(&ss[1], "Owned_2")), bits(&held_coords(&ss[1], "Parcels")));
}

/// Join Field reads only the attributes of its join table, so a place there that UTM can't hold
/// (99°E on the equator, see [`an_input_the_first_ones_system_cant_hold_is_worked_on_in_lonlat`])
/// doesn't send the run to longitude/latitude: the parcels keep their stored coordinates.
#[test]
fn a_join_tables_places_dont_decide_the_system() {
    let mut s = session(UTM);
    let mut far = Dataset::new("Far", GeometryType::Point, vec![Field::new("n", FieldType::Integer), Field::new("owner", FieldType::Text)]);
    far.push(Some(Geometry::Point(Coord::new(99.0, 0.0))), vec![Cell::Int(0), Cell::Text("far owner".into())]);
    s.map_mut().add_dataset(far);
    let mark = s.notice_mark();
    let p = json!({"tool": "joinField", "params": {"target": "Parcels", "targetField": "n", "joinTable": "Far", "joinField": "n", "output": "Owned"}});
    s.run("gp.run", &p).unwrap();
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(!said.iter().any(|t| t.contains("worked in longitude/latitude")), "{said:?}");
    assert_eq!(s.map().layer_by_name("Owned").unwrap().dataset.coord_crs(), UTM);
    assert_eq!(bits(&held_coords(&s, "Owned")), bits(&held_coords(&s, "Parcels")));
    let ds = &s.map().layer_by_name("Owned").unwrap().dataset;
    assert_eq!(ds.value(0, ds.field_index("owner").unwrap()).to_string(), "far owner");
    // The same as a standalone table given by its number: read as it is too.
    let mut table = s.map().layer_by_name("Far").unwrap().dataset.clone();
    table.name = "Far table".into();
    let id = s.project.add_table(table);
    let mark = s.notice_mark();
    let p = json!({"tool": "joinField", "params": {"target": "Parcels", "targetField": "n", "joinTable": id, "joinField": "n", "output": "Owned_2"}});
    s.run("gp.run", &p).unwrap();
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(!said.iter().any(|t| t.contains("worked in longitude/latitude")), "{said:?}");
    let ds = &s.map().layer_by_name("Owned_2").unwrap().dataset;
    assert_eq!(ds.coord_crs(), UTM);
    assert_eq!(ds.value(0, ds.field_index("owner").unwrap()).to_string(), "far owner");
}

/// A project giving Web Mercator and longitude/latitude as their EPSG codes (`{"Epsg": 3857}`,
/// `{"Epsg": 4326}`), as one written by hand or by another program may, opens with them as
/// RustGIS's own Web Mercator and longitude/latitude: the layer held in Web Mercator stays off the
/// projected path tools take, and the other is in longitude/latitude.
#[test]
fn the_epsg_variants_fixture_opens_as_rustgis_own_systems() {
    let p = Project::from_json(include_str!("../../../../schema/fixtures/held_epsg_variants.rgis")).unwrap();
    let (mercator, plain) = (&p.maps[0].layers[0].dataset, &p.maps[0].layers[1].dataset);
    assert_eq!((mercator.coord_crs(), mercator.crs), (Crs::WebMercator, Some(Crs::LonLat)));
    assert_eq!((plain.coord_crs(), plain.crs), (Crs::LonLat, Some(Crs::WebMercator)));
    // echo 48.7 11.5 | cs2cs -f %.6f EPSG:4326 EPSG:3857 (PROJ 9.9.0): 1280174.144123 6224110.131660
    let Some(Geometry::Point(c)) = mercator.features.first().and_then(|f| f.geometry.clone()) else { panic!("a point") };
    assert!((c.x - 1_280_174.144_123).abs() < 1e-6 && (c.y - 6_224_110.131_660).abs() < 1e-6, "{c:?}");
    let back = mercator.in_lonlat().unwrap();
    let Some(Geometry::Point(l)) = back.features.first().and_then(|f| f.geometry.clone()) else { panic!("a point") };
    assert!((l.x - 11.5).abs() < 1e-9 && (l.y - 48.7).abs() < 1e-9, "{l:?}");
}

/// How far apart, in degrees, a place an overlay makes may be between a run in UTM and one in
/// longitude/latitude: an edge straight in UTM and the same edge straight in the analysis plane
/// (Web Mercator) part by up to about L²·tan φ / 8R at its middle, some 20 cm for the road's 3 km
/// edge here (21 cm, `Roads` clipped by `Area`, measured), so where it crosses another edge moves
/// by that much. Half a metre. Straight in UTM is the nearer of the two to the shortest way on the
/// ground.
const OVERLAID: f64 = 5e-6;

/// Every place of `a` within `tol` degrees of one of `b`'s and the other way round, feature by
/// feature, in the same shapes.
fn assert_near(a: &[Option<Geometry>], b: &[Option<Geometry>], tol: f64, what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: feature count");
    for (i, (a, b)) in a.iter().zip(b).enumerate() {
        assert_eq!(shape(a), shape(b), "{what}: feature {i}'s shape");
        let (mut ca, mut cb) = (Vec::new(), Vec::new());
        a.iter().for_each(|g| g.for_each_coord(|c| ca.push(c)));
        b.iter().for_each(|g| g.for_each_coord(|c| cb.push(c)));
        // As places, not in order: the overlay may start a ring at another of its vertices.
        let near = |p: &Coord, of: &[Coord]| of.iter().any(|q| (p.x - q.x).abs() < tol && (p.y - q.y).abs() < tol);
        for p in &ca {
            assert!(near(p, &cb), "{what}: feature {i}: {p:?} not in {cb:?}");
        }
        for q in &cb {
            assert!(near(q, &ca), "{what}: feature {i}: {q:?} not in {ca:?}");
        }
    }
}

/// The overlays and spatial relations (#2032 C2e-2) work in UTM on layers held there, with no
/// pole or antimeridian check reading UTM's metres as degrees: each gives the features, attributes
/// and selections it gives for the same layers in longitude/latitude, its new layers are held in
/// UTM, and where the two planes' straight edges differ, the places they make are within
/// [`OVERLAID`] of each other.
#[test]
fn overlays_work_in_their_held_inputs_own_system() {
    let mut ss = both();
    // A box across the parcels' grid, cutting some, covering some and missing others.
    let fields = vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Integer)];
    let mut area = Dataset::new("Area", GeometryType::Polygon, fields);
    let ring = [(11.502, 48.702), (11.523, 48.702), (11.523, 48.723), (11.502, 48.723), (11.502, 48.702)];
    area.push(
        Some(Geometry::Polygon(vec![vec![ring.iter().map(|&(x, y)| Coord::new(x, y)).collect()]])),
        vec![Cell::Text("a".into()), Cell::Int(7)],
    );
    for (s, crs) in ss.iter_mut().zip([Crs::LonLat, UTM]) {
        let mut a = area.clone();
        a.hold_in(crs).unwrap();
        s.map_mut().add_dataset(a);
        // The same box left in longitude/latitude in both sessions: converted into UTM for a run
        // there, as another layer is.
        let mut plain = area.clone();
        plain.name = "Plain_Area".into();
        s.map_mut().add_dataset(plain);
    }
    let ids = |s: &Session| s.map().layers.iter().map(|l| l.id).collect::<Vec<_>>();
    let attrs = |ds: &Dataset| {
        (0..ds.features.len()).map(|r| (0..ds.fields.len()).map(|f| ds.value(r, f).to_string()).collect::<Vec<_>>()).collect::<Vec<_>>()
    };
    for p in [
        json!({"tool": "clip", "params": {"input": "Parcels", "clip": "Area"}}),
        json!({"tool": "clip", "params": {"input": "Roads", "clip": "Area"}}),
        json!({"tool": "clip", "params": {"input": "Wells", "clip": "Area"}}),
        json!({"tool": "erase", "params": {"input": "Parcels", "erase": "Area"}}),
        json!({"tool": "intersect", "params": {"input": "Parcels", "overlay": "Area"}}),
        json!({"tool": "identity", "params": {"input": "Parcels", "overlay": "Area"}}),
        json!({"tool": "union", "params": {"input": "Parcels", "overlay": "Area"}}),
        json!({"tool": "symmetricalDifference", "params": {"input": "Parcels", "overlay": "Area"}}),
        json!({"tool": "update", "params": {"input": "Parcels", "update": "Area"}}),
        json!({"tool": "split", "params": {"input": "Parcels", "split": "Area"}}),
        json!({"tool": "dissolve", "params": {"input": "Parcels", "field": "name"}}),
        json!({"tool": "featureToPoint", "params": {"input": "Parcels"}}),
        json!({"tool": "countPoints", "params": {"polygons": "Area", "points": "Wells"}}),
        json!({"tool": "spatialJoin", "params": {"target": "Parcels", "join": "Area", "match": "Intersects"}}),
        json!({"tool": "spatialJoin", "params": {"target": "Parcels", "join": "Area", "match": "Largest overlap", "output": "Largest"}}),
        json!({"tool": "spatialJoin", "params": {"target": "Wells", "join": "Area", "match": "Have their center in", "output": "Centres"}}),
        json!({"tool": "clip", "params": {"input": "Parcels", "clip": "Plain_Area", "output": "Plain_Clip"}}),
        json!({"tool": "union", "params": {"input": "Parcels", "overlay": "Plain_Area", "output": "Plain_Union"}}),
    ] {
        let before = ids(&ss[0]);
        let marks = [ss[0].notice_mark(), ss[1].notice_mark()];
        run_both(&mut ss, "gp.run", &p);
        // Nothing said in one that isn't in the other: no pole or degree notice for UTM's metres.
        let [said_a, said_b] = [0, 1].map(|i| ss[i].notices_since(marks[i]).iter().map(|n| n.text.clone()).collect::<Vec<_>>());
        assert_eq!(said_a, said_b, "{p}");
        let made: Vec<u32> = ids(&ss[0]).into_iter().filter(|id| !before.contains(id)).collect();
        assert!(!made.is_empty(), "{p}: no layer");
        for id in made {
            let (a, b) = (&ss[0].map().layer(id).unwrap().dataset, &ss[1].map().layer(id).unwrap().dataset);
            assert_eq!(b.coord_crs(), UTM, "{p}: {} held in", b.name);
            assert!(!a.features.is_empty(), "{p}: {} is empty", a.name);
            assert_eq!(attrs(a), attrs(b), "{p}: {}", a.name);
            let geoms = |d: &Dataset| d.in_lonlat().unwrap().features.iter().map(|f| f.geometry.clone()).collect::<Vec<_>>();
            assert_near(&geoms(a), &geoms(b), OVERLAID, &format!("{p}: {}", a.name));
        }
    }
    // The wells' hull is made of wells, exactly as held. (Which of them are its corners depends on
    // the plane: the grid's middle well on two of its edges lies on the edge straight in Web
    // Mercator, and just outside it in UTM, which turns and shears the grid a little.)
    run_both(&mut ss, "gp.run", &json!({"tool": "convexHull", "params": {"input": "Wells", "output": "Hull"}}));
    assert_eq!(ss[1].map().layer_by_name("Hull").unwrap().dataset.coord_crs(), UTM);
    let wells = bits(&held_coords(&ss[1], "Wells"));
    let hull = bits(&held_coords(&ss[1], "Hull"));
    assert!(hull.len() >= 5 && hull.iter().all(|c| wells.contains(c)), "{hull:?}");
    // Select Layer By Location without a distance, and with one (geodesic, in longitude/latitude).
    for p in [
        json!({"tool": "selectByLocation", "params": {"input": "Wells", "select": "Area"}}),
        json!({"tool": "selectByLocation", "params": {"input": "Wells", "select": "Area", "distance": 0.5, "units": "Kilometers"}}),
    ] {
        run_both(&mut ss, "gp.run", &p);
        assert_eq!(selected(&ss[0], "Wells"), selected(&ss[1], "Wells"), "{p}");
        assert!(!selected(&ss[1], "Wells").is_empty(), "{p}: nothing selected");
    }
}

/// Spatial Join's Largest overlap picks the zone covering most of each parcel in UTM as in
/// longitude/latitude: the middle column of parcels (11.510° to 11.515°) lies 0.0035° in the
/// western zone and 0.0015° in the eastern one, so it joins the western one, and the outer columns
/// their own side's. Measured in UTM's plane, not by reading its metres as degrees.
#[test]
fn largest_overlap_ranks_overlaps_in_the_held_system() {
    let mut ss = both();
    let fields = vec![Field::new("zone", FieldType::Text)];
    let mut zones = Dataset::new("Zones", GeometryType::Polygon, fields);
    for (name, west, east) in [("west", 11.49, 11.5135), ("east", 11.5135, 11.53)] {
        let ring = [(west, 48.69), (east, 48.69), (east, 48.74), (west, 48.74), (west, 48.69)];
        zones.push(
            Some(Geometry::Polygon(vec![vec![ring.iter().map(|&(x, y)| Coord::new(x, y)).collect()]])),
            vec![Cell::Text(name.into())],
        );
    }
    for (s, crs) in ss.iter_mut().zip([Crs::LonLat, UTM]) {
        let mut z = zones.clone();
        z.hold_in(crs).unwrap();
        s.map_mut().add_dataset(z);
    }
    let p = json!({"tool": "spatialJoin", "params": {"target": "Parcels", "join": "Zones", "match": "Largest overlap", "output": "Zoned"}});
    run_both(&mut ss, "gp.run", &p);
    let zone = |s: &Session| {
        let ds = &s.map().layer_by_name("Zoned").unwrap().dataset;
        let f = ds.field_index("join_zone").unwrap();
        (0..ds.features.len()).map(|r| ds.value(r, f).to_string()).collect::<Vec<_>>()
    };
    let want: Vec<String> = ["west", "west", "west", "west", "west", "west", "east", "east", "east"].map(String::from).to_vec();
    assert_eq!(zone(&ss[0]), want);
    assert_eq!(zone(&ss[1]), want);
}

/// A run in UTM says when another input's long edges, converted into UTM vertex by vertex, run
/// far from where a run in longitude/latitude takes them: a degree-wide box's east-west edges
/// (74 km at 48.7°N) follow their parallels in longitude/latitude, straight in Web Mercator, and
/// are chords in UTM, which part from them by about L² tan φ / 8R, some 120 m at the middle. Its
/// north-south edges are meridians, straight in Web Mercator; 2° and 3° east of UTM zone 32N's
/// central meridian they curve in UTM, by some 8 m over the degree, so they count too. The clip
/// itself isn't changed by it here: the parcels lie far inside the box.
#[test]
fn a_converted_inputs_long_edges_are_said() {
    let mut s = session(UTM);
    let mut boxed = Dataset::new("Degree", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
    let ring = [(11.0, 48.2), (12.0, 48.2), (12.0, 49.2), (11.0, 49.2), (11.0, 48.2)];
    boxed.push(Some(Geometry::Polygon(vec![vec![ring.iter().map(|&(x, y)| Coord::new(x, y)).collect()]])), vec![Cell::Text("d".into())]);
    s.map_mut().add_dataset(boxed);
    let mark = s.notice_mark();
    s.run("gp.run", &json!({"tool": "clip", "params": {"input": "Parcels", "clip": "Degree", "output": "Clipped"}})).unwrap();
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    let note = said.iter().find(|t| t.contains("edge(s) of 'Degree' (4), converted into EPSG:32632")).unwrap_or_else(|| panic!("{said:?}"));
    assert!(note.starts_with("Clip: 4 edge(s)"), "{note}");
    let far: f64 = note.split("up to ").nth(1).and_then(|t| t.split(' ').next()).and_then(|t| t.parse().ok()).unwrap();
    // 74.3 km along 48.2°N: 74_300² × tan 48.2° / (8 × 6_371_000) = 121 m.
    assert!((110.0..130.0).contains(&far), "{note}");
    assert_eq!(held_coords(&s, "Clipped").len(), 9);
    // Merge copies the box's vertices and crosses no edges: nothing to say.
    let mark = s.notice_mark();
    s.run("gp.run", &json!({"tool": "merge", "params": {"inputs": ["Parcels", "Degree"], "attributes": false, "output": "Both"}})).unwrap();
    assert!(!s.notices_since(mark).iter().any(|n| n.text.contains("taken straight there")));
}

/// The environment's extent admits the features of a layer held in UTM that it admits of the same
/// layer in longitude/latitude: the extent's box in UTM, turned some 1.9° with UTM's grid, reaches
/// past the middle of each of its edges by some 25 m, where a point 5 m out isn't admitted.
#[test]
fn the_environments_extent_admits_alike_in_the_held_system() {
    let mut ss = both();
    let mut near = Dataset::new("Near", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    let (west, south, east, north) = (11.505, 48.705, 11.525, 48.725);
    // 5 m out: 0.000_045° of latitude, 0.000_068° of longitude at 48.7°.
    let (mid_x, mid_y, out_y, out_x) = (11.515, 48.715, 0.000_045, 0.000_068);
    let outside = [(mid_x, north + out_y), (mid_x, south - out_y), (east + out_x, mid_y), (west - out_x, mid_y)];
    for (x, y) in outside.into_iter().chain([(mid_x, mid_y)]) {
        near.push(Some(Geometry::Point(Coord::new(x, y))), vec![Cell::Text(format!("{x},{y}"))]);
    }
    for (s, crs) in ss.iter_mut().zip([Crs::LonLat, UTM]) {
        let mut d = near.clone();
        d.hold_in(crs).unwrap();
        s.map_mut().add_dataset(d);
        s.project.gp_env.extent = Some(rustgis_geom::Extent { min_x: west, min_y: south, max_x: east, max_y: north });
    }
    run_both(&mut ss, "gp.run", &json!({"tool": "featureToPoint", "params": {"input": "Near", "output": "Inside"}}));
    let names = |s: &Session| {
        let ds = &s.map().layer_by_name("Inside").unwrap().dataset;
        (0..ds.features.len()).map(|r| ds.value(r, 0).to_string()).collect::<Vec<_>>()
    };
    assert_eq!(names(&ss[0]), vec!["11.515,48.715".to_string()]);
    assert_eq!(names(&ss[1]), names(&ss[0]));
}

/// How far a converted edge runs from where a run in longitude/latitude takes it is looked for
/// beyond its middle, and on a ring's closing edge when the ring doesn't repeat its first vertex.
#[test]
fn bent_edges_are_found_beyond_their_middle_and_on_closing_edges() {
    let bent = |g: Geometry| {
        let mut ds = Dataset::new("Edges", g.kind(), Vec::new());
        ds.push(Some(g), Vec::new());
        let c = ds.in_crs(UTM).unwrap().into_owned();
        crate::cmd::gp::bent_edges(&ds, &c, UTM).unwrap()
    };
    // Across UTM zone 32N's central meridian (9°E) at the equator, point-symmetric about where they
    // cross: its middle, (9°E, 0°), is on the chord and on the edge in Web Mercator alike, and it
    // bends away to either side, by metres at its quarters.
    let (n, far) = bent(Geometry::Line(vec![vec![Coord::new(8.0, -1.0), Coord::new(10.0, 1.0)]]));
    assert_eq!(n, 1);
    assert!(far > 1.0, "{far}");
    // The degree-wide box of `a_converted_inputs_long_edges_are_said`, closed and open: its four edges
    // either way.
    let ring = [(11.0, 48.2), (12.0, 48.2), (12.0, 49.2), (11.0, 49.2)].map(|(x, y)| Coord::new(x, y)).to_vec();
    let mut closed = ring.clone();
    closed.push(ring[0]);
    let open = bent(Geometry::Polygon(vec![vec![ring]]));
    assert_eq!(open.0, 4);
    assert_eq!(bent(Geometry::Polygon(vec![vec![closed]])).0, 4);
}

/// A line held in UTM along a northing is tilted in longitude/latitude: an environment's extent
/// near a corner of its box there, away from the line, admits it, in a run in longitude/latitude
/// and in the held system alike.
#[test]
fn the_environments_extent_admits_a_tilted_line_by_its_box_in_longitude_latitude() {
    let mut ss = both();
    let back = UTM.to(Crs::LonLat).unwrap();
    // From the central meridian 200 km east at northing 5,400 km (48.7°N): its east end some
    // 3.6 km south of its west end's latitude.
    let (w, e) = (back.point(Coord::new(500_000.0, 5_400_000.0)).unwrap(), back.point(Coord::new(700_000.0, 5_400_000.0)).unwrap());
    assert!((w.y - e.y).abs() > 0.02, "{w:?} {e:?}");
    let mut line = Dataset::new("Tilted", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    line.push(Some(Geometry::Line(vec![vec![w, e]])), vec![Cell::Text("tilted".into())]);
    // A small box at the corner of the line's box at its west end's longitude and east end's
    // latitude, inside it: kilometres from the line.
    let (x, y) = (w.x + 0.002, e.y + 0.002 * (w.y - e.y).signum());
    for (s, crs) in ss.iter_mut().zip([Crs::LonLat, UTM]) {
        let mut d = line.clone();
        d.hold_in(crs).unwrap();
        s.map_mut().add_dataset(d);
        s.project.gp_env.extent = Some(rustgis_geom::Extent { min_x: x - 0.001, min_y: y - 0.001, max_x: x + 0.001, max_y: y + 0.001 });
    }
    run_both(&mut ss, "gp.run", &json!({"tool": "featureToPoint", "params": {"input": "Tilted", "output": "Admitted"}}));
    for s in &ss {
        assert_eq!(s.map().layer_by_name("Admitted").unwrap().dataset.features.len(), 1);
    }
}

/// A standalone table with geometry given by its number is converted into the system the run
/// works in, as one given by name is.
#[test]
fn a_numbered_table_is_converted_into_the_held_system() {
    let mut ss = both();
    let mut wells = Dataset::new("Wells", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for (x, y) in [(11.51, 48.71), (11.6, 48.8)] {
        wells.push(Some(Geometry::Point(Coord::new(x, y))), vec![Cell::Text(format!("{x},{y}"))]);
    }
    let mut area = Dataset::new("Area", GeometryType::Polygon, Vec::new());
    let ring = [(11.5, 48.7), (11.52, 48.7), (11.52, 48.72), (11.5, 48.72), (11.5, 48.7)];
    area.push(Some(Geometry::Polygon(vec![vec![ring.iter().map(|&(x, y)| Coord::new(x, y)).collect()]])), Vec::new());
    let mut ids = Vec::new();
    for (s, crs) in ss.iter_mut().zip([Crs::LonLat, UTM]) {
        let mut d = wells.clone();
        d.hold_in(crs).unwrap();
        s.map_mut().add_dataset(d);
        // In longitude/latitude in both sessions.
        ids.push(s.project.add_table(area.clone()));
    }
    assert_eq!(ids[0], ids[1]);
    run_both(&mut ss, "gp.run", &json!({"tool": "clip", "params": {"input": "Wells", "clip": ids[0], "output": "Inside"}}));
    for s in &ss {
        let ds = &s.map().layer_by_name("Inside").unwrap().dataset;
        assert_eq!(ds.coord_crs(), if std::ptr::eq(s, &ss[0]) { Crs::LonLat } else { UTM });
        assert_eq!((0..ds.features.len()).map(|r| ds.value(r, 0).to_string()).collect::<Vec<_>>(), vec!["11.51,48.71"]);
    }
}

/// A place given by its UTM zone 32N coordinates, in longitude/latitude.
fn utm_place(x: f64, y: f64) -> Coord {
    UTM.to(Crs::LonLat).unwrap().point(Coord::new(x, y)).unwrap()
}

/// Adds a layer of `geoms` (in longitude/latitude) to `s`, held in `crs`, its features named
/// `<name>0`, `<name>1`… in their order.
fn add_held(s: &mut Session, name: &str, kind: GeometryType, geoms: Vec<Geometry>, crs: Crs) {
    let mut d = Dataset::new(name, kind, vec![Field::new("name", FieldType::Text)]);
    for (i, g) in geoms.into_iter().enumerate() {
        d.push(Some(g), vec![Cell::Text(format!("{name}{i}"))]);
    }
    d.hold_in(crs).unwrap();
    s.map_mut().add_dataset(d);
}

/// A layer's numbers in field `field`, in row order (empty values as NaN).
fn field_numbers(s: &Session, layer: &str, field: &str) -> Vec<f64> {
    let ds = &s.map().layer_by_name(layer).unwrap_or_else(|| panic!("no layer {layer}")).dataset;
    let f = ds.field_index(field).unwrap_or_else(|| panic!("{layer}: no field {field}"));
    (0..ds.features.len()).map(|r| ds.value(r, f).as_f64().unwrap_or(f64::NAN)).collect()
}

/// In a run in UTM, distances in metres are measured flat there, as in a projected system tools
/// measure them: a buffer is a circle of that radius in UTM, Select Layer By Location and Spatial
/// Join take what lies within it there, Near measures to the nearest point there and gives it in
/// UTM, Points Along Lines steps along the line there from grid north, and Simplify's tolerance is
/// a length there. At the origin (12.86°E, 48.70°N, 3.86° east of the central meridian), UTM's
/// scale is 1.00059 (`proj -S +proj=utm +zone=32 +datum=WGS84`), a UTM metre 0.99941 m on the
/// ground: a point 1000.3 UTM metres away is 999.71 m away on the ground, within a kilometre for a
/// run in longitude/latitude and not for one in UTM. The places and lengths checked are UTM's own.
#[test]
fn distances_are_measured_flat_in_the_held_system() {
    let (x0, y0) = (784_000.0, 5_401_300.0);
    let pt = |x: f64, y: f64| Geometry::Point(utm_place(x, y));
    let mut s = session(UTM);
    add_held(&mut s, "Origin", GeometryType::Point, vec![pt(x0, y0)], UTM);
    add_held(&mut s, "Around", GeometryType::Point, vec![pt(x0 + 999.7, y0), pt(x0 + 1000.3, y0), pt(x0, y0 + 500.0)], UTM);
    let held = held_coords(&s, "Origin")[0][0];
    assert!((held.x - x0).abs() < 1e-6 && (held.y - y0).abs() < 1e-6, "{held:?}");

    // A buffer of a kilometre: a circle of 1000 UTM metres.
    s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Origin", "distance": 1, "units": "Kilometers", "output": "Disc"}}))
        .unwrap();
    assert_eq!(s.map().layer_by_name("Disc").unwrap().dataset.coord_crs(), UTM);
    let disc = &held_coords(&s, "Disc")[0];
    assert!(disc.len() > 100, "{}", disc.len());
    for c in disc {
        assert!(((c.x - x0).hypot(c.y - y0) - 1000.0).abs() < 1e-6, "{c:?}");
    }

    // Select Layer By Location within a kilometre: the points 999.7 m and 500 m away, not the one
    // 1000.3 m away (999.71 m on the ground, which a run in longitude/latitude selects).
    s.run(
        "gp.run",
        &json!({"tool": "selectByLocation", "params": {"input": "Around", "select": "Origin", "distance": 1000, "units": "Meters"}}),
    )
    .unwrap();
    assert_eq!(selected(&s, "Around"), vec![0, 2]);

    // Near: the point 500 m north, at 0.5 km, where it is in UTM.
    s.run("gp.run", &json!({"tool": "near", "params": {"input": "Origin", "near": "Around", "output": "Nearest"}})).unwrap();
    assert_eq!(s.map().layer_by_name("Nearest").unwrap().dataset.coord_crs(), UTM);
    assert!((field_numbers(&s, "Nearest", "near_dist")[0] - 0.5).abs() < 1e-9);
    assert!((field_numbers(&s, "Nearest", "near_x")[0] - x0).abs() < 1e-6);
    assert!((field_numbers(&s, "Nearest", "near_y")[0] - (y0 + 500.0)).abs() < 1e-6);

    // Spatial Join: the closest, and those within a kilometre, nearest first.
    let p = json!({"tool": "spatialJoin", "params": {
        "target": "Origin", "join": "Around", "match": "Closest", "units": "Meters", "distanceField": "dist", "output": "Closest"
    }});
    s.run("gp.run", &p).unwrap();
    assert!((field_numbers(&s, "Closest", "dist")[0] - 500.0).abs() < 1e-6);
    let p = json!({"tool": "spatialJoin", "params": {
        "target": "Origin", "join": "Around", "match": "Within a distance", "distance": 1000, "units": "Meters",
        "distanceField": "dist", "operation": "Join one to many", "output": "Within"
    }});
    s.run("gp.run", &p).unwrap();
    let within = field_numbers(&s, "Within", "dist");
    assert_eq!(within.len(), 2, "{within:?}");
    assert!((within[0] - 500.0).abs() < 1e-6 && (within[1] - 999.7).abs() < 1e-6, "{within:?}");

    // Points Along Lines: every kilometre along a 3 km line due east in UTM, heading 90° from grid
    // north.
    add_held(&mut s, "East", GeometryType::Polyline, vec![Geometry::Line(vec![vec![utm_place(x0, y0), utm_place(x0 + 3000.0, y0)]])], UTM);
    s.run(
        "gp.run",
        &json!({"tool": "pointsAlongLines", "params": {"input": "East", "distance": 1, "units": "Kilometers", "output": "Steps"}}),
    )
    .unwrap();
    assert_eq!(s.map().layer_by_name("Steps").unwrap().dataset.coord_crs(), UTM);
    let steps = held_coords(&s, "Steps");
    assert_eq!(steps.len(), 4, "{steps:?}");
    for (k, c) in steps.iter().map(|p| p[0]).enumerate() {
        assert!((c.x - (x0 + 1000.0 * k as f64)).abs() < 1e-6 && (c.y - y0).abs() < 1e-6, "{k}: {c:?}");
    }
    let along = field_numbers(&s, "Steps", "distance");
    assert!(along.iter().zip([0.0, 1.0, 2.0, 3.0]).all(|(d, want)| (d - want).abs() < 1e-9), "{along:?}");
    assert!(field_numbers(&s, "Steps", "angle").iter().all(|a| (a - 90.0).abs() < 1e-9));

    // Simplify: a bend of 0.4 UTM metres goes at a tolerance of half a metre, and stays at 0.3 m.
    let bend = vec![utm_place(x0, y0), utm_place(x0 + 500.0, y0 + 0.4), utm_place(x0 + 1000.0, y0)];
    add_held(&mut s, "Bend", GeometryType::Polyline, vec![Geometry::Line(vec![bend])], UTM);
    for (km, left, out) in [(0.0005, 2, "Plain"), (0.0003, 3, "Bent")] {
        s.run("gp.run", &json!({"tool": "simplify", "params": {"input": "Bend", "tolerance": km, "output": out}})).unwrap();
        assert_eq!(s.map().layer_by_name(out).unwrap().dataset.coord_crs(), UTM);
        assert_eq!(held_coords(&s, out)[0].len(), left, "{km} km");
    }
}

/// In a system in US survey feet, a distance in metres is that many metres' worth of its feet:
/// a buffer of 1000 m in NAD83 / California zone 3 (ftUS, EPSG:2227) is a circle of
/// 1000 / 0.304800609601219 = 3280.833 ft.
#[test]
fn a_distance_is_in_the_systems_own_unit() {
    const FEET: Crs = Crs::Epsg(2227);
    let mut s = Session::new(Project::default());
    let place = FEET.to(Crs::LonLat).unwrap().point(Coord::new(6_000_000.0, 2_100_000.0)).unwrap();
    add_held(&mut s, "Spot", GeometryType::Point, vec![Geometry::Point(place)], FEET);
    s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Spot", "distance": 1000, "units": "Meters", "output": "Disc"}}))
        .unwrap();
    let centre = held_coords(&s, "Spot")[0][0];
    let radius = 1000.0 / 0.304_800_609_601_219;
    for c in &held_coords(&s, "Disc")[0] {
        assert!(((c.x - centre.x).hypot(c.y - centre.y) - radius).abs() < 1e-6, "{c:?}");
    }
}

/// Points Along Lines' angle is from grid north whichever way a system's axes point: due east is
/// 90° in Hartebeesthoek94 / Lo27 (EPSG:22275, `+axis=wsu`), whose x grows westward and y
/// southward.
#[test]
fn a_direction_is_from_grid_north_whatever_the_axes() {
    const LO27: Crs = Crs::Epsg(22275);
    let mut s = Session::new(Project::default());
    let back = LO27.to(Crs::LonLat).unwrap();
    // 1 km due east in the grid: x (westing) down by 1000.
    let (a, b) = (back.point(Coord::new(10_000.0, 3_000_000.0)).unwrap(), back.point(Coord::new(9_000.0, 3_000_000.0)).unwrap());
    add_held(&mut s, "East", GeometryType::Polyline, vec![Geometry::Line(vec![vec![a, b]])], LO27);
    s.run(
        "gp.run",
        &json!({"tool": "pointsAlongLines", "params": {"input": "East", "distance": 500, "units": "Meters", "output": "Steps"}}),
    )
    .unwrap();
    assert_eq!(s.map().layer_by_name("Steps").unwrap().dataset.coord_crs(), LO27);
    let angles = field_numbers(&s, "Steps", "angle");
    assert_eq!(angles.len(), 3, "{angles:?}");
    assert!(angles.iter().all(|a| (a - 90.0).abs() < 1e-9), "{angles:?}");
}

/// The tools that measure flat in a held layer's own system take junk distances and places without
/// a panic: they refuse, or make what they can.
#[test]
fn distance_tools_take_junk_in_the_held_system() {
    let mut s = session(UTM);
    let far = UTM.to(Crs::LonLat).unwrap().point(Coord::new(500_000.0, 5_400_000.0)).unwrap();
    add_held(&mut s, "Junk", GeometryType::Polyline, vec![Geometry::Line(vec![vec![far, far, Coord::new(f64::NAN, 1.0)]])], UTM);
    for d in [json!(0), json!(-1), json!(1e308), json!(-1e308), json!("x"), json!(null), json!(5e-324)] {
        for (tool, params) in [
            ("buffer", json!({"input": "Junk", "distance": d})),
            ("buffer", json!({"input": "Parcels", "distance": d, "dissolve": "All"})),
            ("simplify", json!({"input": "Roads", "tolerance": d})),
            ("pointsAlongLines", json!({"input": "Junk", "distance": d})),
            ("pointsAlongLines", json!({"input": "Roads", "distance": d, "startOffset": d, "endOffset": d, "includeEnd": "Yes"})),
            ("near", json!({"input": "Wells", "near": "Junk", "distance": d})),
            ("selectByLocation", json!({"input": "Wells", "select": "Junk", "distance": d})),
            ("spatialJoin", json!({"target": "Wells", "join": "Roads", "match": "Closest", "distance": d})),
            ("spatialJoin", json!({"target": "Wells", "join": "Junk", "match": "Within a distance", "distance": d})),
        ] {
            // Refused or done: not a panic, which a tool run gives back as "<Tool> failed
            // unexpectedly".
            if let Err(e) = s.run("gp.run", &json!({"tool": tool, "params": params})) {
                assert!(!e.contains("failed unexpectedly"), "{tool} {params}: {e}");
            }
        }
    }
}

/// Adds a layer of `geoms`, in `crs`'s own coordinates as they are, held and read in it, with a
/// number `v` per feature (1, 2, 3…).
fn add_in(s: &mut Session, name: &str, kind: GeometryType, geoms: Vec<Geometry>, crs: Crs) {
    let mut d = Dataset::new(name, kind, vec![Field::new("v", FieldType::Double)]);
    for (i, g) in geoms.into_iter().enumerate() {
        d.push(Some(g), vec![Cell::Float(i as f64 + 1.0)]);
    }
    d.held_in = crs;
    d.crs = Some(crs);
    s.map_mut().add_dataset(d);
}

/// A raster layer's grid.
fn grid_of(s: &Session, layer: &str) -> std::sync::Arc<rustgis_data::raster::Raster> {
    let l = s.map().layer_by_name(layer).unwrap_or_else(|| panic!("no layer {layer}"));
    l.raster.as_ref().and_then(|r| r.data.clone()).unwrap_or_else(|| panic!("{layer} is no raster"))
}

/// IDW, Kriging and Kernel Density make their grid in the system a layer is held in from its own
/// coordinates, as they are: the grid's corner is the points' own westmost and northmost
/// coordinates, exactly, where a run in longitude/latitude converted them there and back. The
/// points lie 1500 km east of the zone's central meridian, where that round trip doesn't come back
/// to the same bits.
#[test]
fn a_grid_is_made_from_the_held_systems_own_coordinates() {
    let (x0, y0) = (2_000_000.25, 5_401_300.75);
    let at = [(0.0, 0.0), (1234.5, 310.25), (410.125, 2045.5), (2500.75, 1800.0)];
    let mut s = session(UTM);
    add_in(&mut s, "Samples", GeometryType::Point, at.iter().map(|(x, y)| Geometry::Point(Coord::new(x0 + x, y0 + y))).collect(), UTM);
    let (west, north) = (x0, y0 + 2045.5);
    for (tool, out) in [("idwInterpolation", "Idw"), ("kriging", "Krig")] {
        s.run("gp.run", &json!({"tool": tool, "params": {"input": "Samples", "field": "v", "pixelSize": 100, "output": out}})).unwrap();
        let g = grid_of(&s, out);
        assert_eq!(g.crs, UTM, "{tool}");
        assert_eq!(
            (g.transform.origin_x.to_bits(), g.transform.origin_y.to_bits()),
            (west.to_bits(), north.to_bits()),
            "{tool}: {:?}",
            g.transform
        );
    }
    s.run("gp.run", &json!({"tool": "kernelDensity", "params": {"input": "Samples", "radius": 500, "pixelSize": 50, "output": "Kde"}}))
        .unwrap();
    let g = grid_of(&s, "Kde");
    assert_eq!(g.crs, UTM);
    // Its grid reaches the radius past the points, on whole cells from the westmost point.
    let (dx, dy) = (west - g.transform.origin_x, g.transform.origin_y - north);
    assert!(dx > 0.0 && dy > 0.0 && (dx / 50.0).fract() == 0.0 && (dy / 50.0).fract() == 0.0, "{:?}", g.transform);
}

/// Gauss-Krüger zone 3 on DHDN (EPSG:31467) to UTM 32N on ETRS89 (EPSG:25832).
const GK3: Crs = Crs::Epsg(31467);
const ETRS_UTM: Crs = Crs::Epsg(25832);

/// A grid in another system than the points' own takes the direct operation between the two, as
/// [`Crs::to`] converts: between DHDN and ETRS89, 0.44 m from the way through WGS 84 a run in
/// longitude/latitude took (`a_conversion_between_two_datums_takes_their_transformation`).
#[test]
fn a_grid_in_another_system_takes_the_direct_operation() {
    let mut s = Session::new(Project::default());
    let at = [Coord::new(3_500_000.0, 5_400_000.0), Coord::new(3_501_200.0, 5_400_900.0), Coord::new(3_500_700.0, 5_401_500.0)];
    add_in(&mut s, "Samples", GeometryType::Point, at.iter().map(|c| Geometry::Point(*c)).collect(), GK3);
    s.run(
        "gp.run",
        &json!({"tool": "idwInterpolation", "params": {"input": "Samples", "field": "v", "pixelSize": 10, "crs": "EPSG:25832", "output": "Idw"}}),
    )
    .unwrap();
    let to = GK3.to(ETRS_UTM).unwrap();
    let placed: Vec<Coord> = at.iter().map(|c| to.point(*c).unwrap()).collect();
    let west = placed.iter().map(|c| c.x).fold(f64::INFINITY, f64::min);
    let north = placed.iter().map(|c| c.y).fold(f64::NEG_INFINITY, f64::max);
    let g = grid_of(&s, "Idw");
    assert_eq!(g.crs, ETRS_UTM);
    assert_eq!((g.transform.origin_x, g.transform.origin_y), (west, north));
}

/// A session with rasters `grid` (10 m by 10 m of 10 cm cells in UTM 32N on ETRS89, each holding
/// its column and row, col + 1000 × row) and `blocks` (the same cells in 100 classes of a metre
/// square), and two layers of one 6 m square: `Zone`, held in Gauss-Krüger zone 3 on DHDN, and
/// `Converted`, the same square converted into UTM by [`Crs::to`] and held there. Through WGS 84
/// the square lands 0.44 m from where the direct operation puts it.
fn datum_pair() -> Session {
    use rustgis_data::raster::{ColorHint, GeoTransform, Raster};
    let mut s = Session::new(Project::default());
    // 10 m by 10 m of 10 cm cells, each holding its column and row (col + 1000 × row).
    let (w, h) = (100u32, 100u32);
    let values: Vec<f32> = (0..w * h).map(|i| (i % w + 1000 * (i / w)) as f32).collect();
    let gt = GeoTransform { origin_x: 499_920.0, origin_y: 5_398_285.0, pixel_width: 0.1, pixel_height: -0.1 };
    let r = Raster::new("grid", w, h, vec![values], gt, ETRS_UTM, None, ColorHint::Gray, "Float32").unwrap();
    s.map_mut().add_raster("grid", rustgis_map::raster::RasterLayer::from_path(r, "grid.tif"));
    // The same cells in 100 classes of a metre square, for Tabulate Area.
    let classes: Vec<f32> = (0..w * h).map(|i| (i % w / 10 + 10 * (i / w / 10)) as f32).collect();
    let r = Raster::new("blocks", w, h, vec![classes], gt, ETRS_UTM, None, ColorHint::Gray, "Float32").unwrap();
    s.map_mut().add_raster("blocks", rustgis_map::raster::RasterLayer::from_path(r, "blocks.tif"));
    // A 6 m square round (3,500,000, 5,400,000) in GK3, 499,924.54, 5,398,279.54 in UTM.
    let (cx, cy) = (3_500_000.0, 5_400_000.0);
    let ring: Vec<Coord> =
        [(-3.0, -3.0), (3.0, -3.0), (3.0, 3.0), (-3.0, 3.0), (-3.0, -3.0)].iter().map(|(dx, dy)| Coord::new(cx + dx, cy + dy)).collect();
    let to = GK3.to(ETRS_UTM).unwrap();
    let there: Vec<Coord> = ring.iter().map(|c| to.point(*c).unwrap()).collect();
    add_in(&mut s, "Zone", GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![ring]])], GK3);
    add_in(&mut s, "Converted", GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![there]])], ETRS_UTM);
    s
}

/// Zonal Statistics and Tabulate Area place zones held in another system than the raster's by the
/// direct operation between the two: a square held in Gauss-Krüger zone 3 on DHDN covers the cells
/// of a raster in UTM 32N on ETRS89 that the same square converted there by [`Crs::to`] covers,
/// on cells of 10 cm (and in their metre-square classes), where the way through WGS 84 is 0.44 m
/// off.
#[test]
fn zones_reach_a_raster_by_the_direct_operation() {
    let mut s = datum_pair();
    let mut stats = Vec::new();
    let mut tables = Vec::new();
    for zone in ["Zone", "Converted"] {
        let out = format!("{zone}Stats");
        s.run("gp.run", &json!({"tool": "zonalStats", "params": {"raster": "grid", "polygons": zone, "output": out}})).unwrap();
        stats.push((field_numbers(&s, &out, "grid_count"), field_numbers(&s, &out, "grid_mean")));
        let table = format!("{zone}Classes");
        s.run(
            "gp.run",
            &json!({"tool": "tabulateArea", "params": {"raster": "blocks", "zones": zone, "measure": "Count", "output": table}}),
        )
        .unwrap();
        let t = s.project.tables.iter().find(|t| t.name == table).unwrap_or_else(|| panic!("no table {table}"));
        let row: Vec<f64> = (1..t.fields.len()).filter_map(|f| t.value(0, f).as_f64()).collect();
        tables.push((t.fields.iter().skip(1).map(|f| f.name.clone()).collect::<Vec<_>>(), row));
    }
    // About 60 × 60 cells.
    assert!(stats[0].0[0] > 3000.0, "{stats:?}");
    assert_eq!(stats[0], stats[1]);
    assert_eq!(tables[0], tables[1]);
}

/// NSIDC Sea Ice Polar Stereographic North, a projected system that holds the pole.
const POLAR: Crs = Crs::Epsg(3413);

/// The places a layer's points or zones are taken to in another system are said when that system
/// can't hold them: IDW from points held in a polar system, on a grid in Web Mercator, leaves out
/// the one at the pole and counts it. A zone held there that reaches past Web Mercator's 85.05°
/// keeps its part below the limit on a Web Mercator raster, as a zone in longitude/latitude
/// always did (its vertices beyond are taken as on the limit): Zonal Statistics counts its cells
/// and says no part was left out. A part that reaches a place the raster's system can't hold at
/// all is left out and said, by Zonal Statistics and Tabulate Area, its other parts counted.
#[test]
fn a_held_layers_places_another_system_cant_hold_are_said() {
    use rustgis_data::raster::{ColorHint, GeoTransform, Raster};
    let to = Crs::LonLat.to(POLAR).unwrap();
    let mut s = Session::new(Project::default());
    let pts = [Coord::new(0.0, 90.0), Coord::new(5.0, 70.0), Coord::new(8.0, 72.0)];
    add_in(&mut s, "Ice", GeometryType::Point, pts.iter().map(|c| Geometry::Point(to.point(*c).unwrap())).collect(), POLAR);
    let mark = s.notice_mark();
    let answer = s
        .run(
            "gp.run",
            &json!({"tool": "idwInterpolation", "params": {"input": "Ice", "field": "v", "pixelSize": 10000, "crs": "EPSG:3857", "output": "Idw"}}),
        )
        .unwrap();
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).chain([answer.to_string()]).collect();
    assert!(said.iter().any(|t| t.contains("1 point(s) beyond what") && t.contains("85.05")), "{said:?}");
    assert!(said.iter().any(|t| t.contains("from 2 points")), "{said:?}");

    // Web Mercator from 0° to 10°E, 80°N to its limit, in cells of 10 km, all 1.
    let (top, bottom): (f64, f64) = (20_037_508.342_789_244, 15_538_711.096_309_224);
    let (w, h) = (112u32, ((top - bottom) / 10_000.0).ceil() as u32);
    let gt = GeoTransform { origin_x: 0.0, origin_y: top, pixel_width: 10_000.0, pixel_height: -10_000.0 };
    let r = Raster::new("merc", w, h, vec![vec![1.0; (w * h) as usize]], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
    s.map_mut().add_raster("merc", rustgis_map::raster::RasterLayer::from_path(r, "merc.tif"));
    // From 2°E to 8°E, 83°N to 87°N, its edges followed in steps of a degree.
    let mut ring: Vec<Coord> = (2..=8).map(|x| Coord::new(f64::from(x), 83.0)).collect();
    ring.extend((84..=87).map(|y| Coord::new(8.0, f64::from(y))));
    ring.extend((2..8).rev().map(|x| Coord::new(f64::from(x), 87.0)));
    ring.extend((83..87).rev().map(|y| Coord::new(2.0, f64::from(y))));
    let ring: Vec<Coord> = ring.iter().map(|c| to.point(*c).unwrap()).collect();
    add_in(&mut s, "Cap", GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![ring]])], POLAR);
    let mark = s.notice_mark();
    let answer =
        s.run("gp.run", &json!({"tool": "zonalStats", "params": {"raster": "merc", "polygons": "Cap", "output": "CapStats"}})).unwrap();
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).chain([answer.to_string()]).collect();
    assert!(!said.iter().any(|t| t.contains("could not be projected") || t.contains("invalid geometry")), "{said:?}");
    assert!(field_numbers(&s, "CapStats", "merc_count")[0] > 100.0, "{:?}", field_numbers(&s, "CapStats", "merc_count"));

    // A zone of two parts on a raster in Antarctic Polar Stereographic (EPSG:3031), which can't
    // hold the North Pole: the part reaching it is left out and said, the other one counted.
    let south = Crs::Epsg(3031);
    let into = Crs::LonLat.to(south).unwrap();
    let near: Vec<Coord> =
        [(10.0, 0.0), (11.0, 0.0), (11.0, 1.0), (10.0, 1.0), (10.0, 0.0)].iter().map(|(x, y)| Coord::new(*x, *y)).collect();
    let mut bounds = rustgis_geom::Extent::EMPTY;
    near.iter().for_each(|c| bounds.expand(into.point(*c).unwrap()));
    let (w, h) = (((bounds.max_x - bounds.min_x) / 1000.0).ceil() as u32 + 2, ((bounds.max_y - bounds.min_y) / 1000.0).ceil() as u32 + 2);
    let gt = GeoTransform { origin_x: bounds.min_x - 1000.0, origin_y: bounds.max_y + 1000.0, pixel_width: 1000.0, pixel_height: -1000.0 };
    let r = Raster::new("south", w, h, vec![vec![1.0; (w * h) as usize]], gt, south, None, ColorHint::Gray, "Float32").unwrap();
    s.map_mut().add_raster("south", rustgis_map::raster::RasterLayer::from_path(r, "south.tif"));
    let pole: Vec<Coord> = [(0.0, 89.0), (90.0, 89.0), (0.0, 90.0), (0.0, 89.0)].iter().map(|(x, y)| Coord::new(*x, *y)).collect();
    let parts = [near, pole].map(|ring| vec![ring.iter().map(|c| to.point(*c).unwrap()).collect::<Vec<_>>()]);
    add_in(&mut s, "Split", GeometryType::Polygon, vec![Geometry::Polygon(parts.to_vec())], POLAR);
    for (tool, params) in [
        ("zonalStats", json!({"raster": "south", "polygons": "Split", "output": "SplitStats"})),
        ("tabulateArea", json!({"raster": "south", "zones": "Split", "measure": "Count", "output": "SplitClasses"})),
    ] {
        let mark = s.notice_mark();
        let answer = s.run("gp.run", &json!({"tool": tool, "params": params})).unwrap();
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).chain([answer.to_string()]).collect();
        assert!(said.iter().any(|t| t.contains("1 polygon part(s) could not be projected onto the raster grid")), "{tool}: {said:?}");
    }
    assert!(field_numbers(&s, "SplitStats", "south_count")[0] > 100.0, "{:?}", field_numbers(&s, "SplitStats", "south_count"));
}

/// Rasterize, raster clip and a spectral zone burn, cut and read a polygon held in its own system
/// from its coordinates there: a square held in Gauss-Krüger zone 3 on DHDN burned onto, cut out
/// of and read on a raster in UTM 32N on ETRS89 gives the cells the same square converted there
/// by [`Crs::to`] gives, on cells of 10 cm, where the way through WGS 84 is 0.44 m off.
#[test]
fn a_raster_takes_a_held_polygon_by_the_direct_operation() {
    let mut s = datum_pair();
    let mut got = Vec::new();
    for zone in ["Zone", "Converted"] {
        let burned = s
            .run("raster.rasterize", &json!({"layer": zone, "value": 5, "grid": "grid", "nodata": 0, "name": format!("{zone} burned")}))
            .unwrap();
        let clipped = s
            .run("raster.process", &json!({"layer": "grid", "operation": "clip", "polygon": zone, "name": format!("{zone} clip")}))
            .unwrap();
        let read = s.run("raster.spectral", &json!({"layer": "grid", "zone": zone})).unwrap();
        got.push((raster_cells(&s, &burned), raster_cells(&s, &clipped), read["pixels"].clone(), read["values"].clone()));
    }
    // About 60 × 60 cells.
    assert!(got[0].2.as_u64().unwrap() > 3000, "{:?}", got[0].2);
    assert!(got[0].0.2.iter().filter(|v| v.is_some()).count() > 3000);
    assert_eq!(got[0], got[1]);
}

/// A held layer is burned in its own system by default, from its coordinates as they are: a
/// 100 m square whose edges run through cell centres (a cell's west and south edge taken in, its
/// east and north left out) covers exactly 10 × 10 cells of 10 m, in UTM, where a run through
/// longitude/latitude moved its edges a hair off those centres. The square lies 1500 km east of
/// the zone's central meridian, where that round trip doesn't come back to the same bits.
#[test]
fn a_held_layer_is_burned_in_its_own_system() {
    let (x0, y0) = (2_000_005.0, 5_401_305.0);
    let ring: Vec<Coord> =
        [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0), (0.0, 0.0)].iter().map(|(x, y)| Coord::new(x0 + x, y0 + y)).collect();
    let mut s = Session::new(Project::default());
    add_in(&mut s, "Square", GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![ring]])], UTM);
    let a = s.run("raster.rasterize", &json!({"layer": "Square", "cellSize": 10, "name": "Burned"})).unwrap();
    let g = grid_of(&s, "Burned");
    assert_eq!(g.crs, UTM);
    assert_eq!((g.transform.origin_x, g.transform.origin_y), (2_000_000.0, 5_401_410.0), "{:?}", g.transform);
    assert_eq!(a["cells"], 100, "{a}");
}

/// A spectral zone of two parts reads both: two 100 m squares of one feature, held in UTM, each
/// holding 10 × 10 centres of 10 m cells (none on an edge), burned and then read as the zone, give
/// 200 pixels; a part over another's pixels doesn't count them twice.
#[test]
fn a_spectral_zone_reads_every_part() {
    let square = |x0: f64, y0: f64| -> Vec<Coord> {
        [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0), (0.0, 0.0)].iter().map(|(x, y)| Coord::new(x0 + x, y0 + y)).collect()
    };
    let parts = vec![vec![square(2_000_002.0, 5_401_302.0)], vec![square(2_000_502.0, 5_401_302.0)]];
    let mut s = Session::new(Project::default());
    add_in(&mut s, "Two", GeometryType::Polygon, vec![Geometry::Polygon(parts)], UTM);
    let burned = s.run("raster.rasterize", &json!({"layer": "Two", "value": 3, "cellSize": 10, "name": "Burned"})).unwrap();
    assert_eq!(burned["cells"], 200, "{burned}");
    let read = s.run("raster.spectral", &json!({"layer": "Burned", "zone": "Two"})).unwrap();
    assert_eq!((read["pixels"].clone(), read["values"].clone()), (json!(200), json!([3.0])), "{read}");
    // A third part over half of the first: its 50 shared pixels count once.
    let parts =
        vec![vec![square(2_000_002.0, 5_401_302.0)], vec![square(2_000_502.0, 5_401_302.0)], vec![square(2_000_052.0, 5_401_302.0)]];
    add_in(&mut s, "Three", GeometryType::Polygon, vec![Geometry::Polygon(parts)], UTM);
    let read = s.run("raster.spectral", &json!({"layer": "Burned", "zone": "Three"})).unwrap();
    assert_eq!(read["pixels"], 200, "{read}");
}

/// Rasterize's default grid in another system takes in the polygons as they are burned, by the
/// direct operation: a 5 cm square held in Gauss-Krüger zone 3 (DHDN), burned onto 1 cm cells in
/// ETRS89 / UTM 32N, lands 0.44 m from where the route through WGS 84 puts it, so a grid sized
/// by that route alone holds none of it.
#[test]
fn rasterize_sizes_its_grid_by_the_operation_that_burns() {
    let (x0, y0) = (3_456_000.0, 5_430_000.0);
    let ring: Vec<Coord> =
        [(0.0, 0.0), (0.05, 0.0), (0.05, 0.05), (0.0, 0.05), (0.0, 0.0)].iter().map(|(x, y)| Coord::new(x0 + x, y0 + y)).collect();
    let mut s = Session::new(Project::default());
    add_in(&mut s, "Tiny", GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![ring]])], GK3);
    let a = s.run("raster.rasterize", &json!({"layer": "Tiny", "crs": "EPSG:25832", "cellSize": 0.01, "name": "Burned"})).unwrap();
    assert!((16..=36).contains(&a["cells"].as_u64().unwrap()), "{a}");
}

/// Rasterize sizes its default grid in another system by the polygons' box in longitude/latitude,
/// and says how many vertices it couldn't place there, left out of that box: a polygon held in UTM
/// with a vertex far beyond any place UTM holds, burned onto a grid in degrees. With an extent
/// given, the box isn't used, and nothing is said of it.
#[test]
fn rasterize_says_the_vertices_left_out_of_its_extent() {
    let (x0, y0) = (784_000.0, 5_401_300.0);
    let square: Vec<Coord> =
        [(0.0, 0.0), (1000.0, 0.0), (1000.0, 1000.0), (0.0, 1000.0), (0.0, 0.0)].iter().map(|(x, y)| Coord::new(x0 + x, y0 + y)).collect();
    let far = vec![Coord::new(x0, y0), Coord::new(1e12, 1e12), Coord::new(x0, y0 + 500.0), Coord::new(x0, y0)];
    let mut s = Session::new(Project::default());
    add_in(&mut s, "Plots", GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![square]]), Geometry::Polygon(vec![vec![far]])], UTM);
    let left_out = "vertex that can't be placed in longitude/latitude was left out of the grid's extent: check its coordinates and the layer's coordinate system";
    let said = |s: &Session, mark| s.notices_since(mark).iter().map(|n| n.text.clone()).collect::<Vec<_>>();
    let mark = s.notice_mark();
    s.run("raster.rasterize", &json!({"layer": "Plots", "crs": "EPSG:4326", "cellSize": 0.001, "name": "A"})).unwrap();
    let a = said(&s, mark);
    assert!(a.iter().any(|t| t.contains(&format!("Plots: 1 {left_out}"))), "{a:?}");
    let mark = s.notice_mark();
    s.run(
        "raster.rasterize",
        &json!({"layer": "Plots", "crs": "EPSG:4326", "cellSize": 0.001, "extent": [12.8, 48.6, 12.9, 48.8], "name": "B"}),
    )
    .unwrap();
    let b = said(&s, mark);
    assert!(!b.iter().any(|t| t.contains(left_out)), "{b:?}");
}

/// Clean input says nothing of places left out: points and a polygon held in UTM, all of which the
/// systems on each side hold, interpolated and smoothed onto Web Mercator and burned onto a grid
/// in degrees with no extent given.
#[test]
fn clean_held_input_says_nothing_left_out() {
    let mut s = Session::new(Project::default());
    let at = [(784_000.0, 5_401_300.0), (785_000.0, 5_402_300.0), (786_000.0, 5_401_800.0), (784_500.0, 5_402_900.0)];
    add_in(&mut s, "Samples", GeometryType::Point, at.iter().map(|(x, y)| Geometry::Point(Coord::new(*x, *y))).collect(), UTM);
    let square: Vec<Coord> = [(0.0, 0.0), (500.0, 0.0), (500.0, 500.0), (0.0, 500.0), (0.0, 0.0)]
        .iter()
        .map(|(x, y)| Coord::new(784_000.0 + x, 5_401_300.0 + y))
        .collect();
    add_in(&mut s, "Plot", GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![square]])], UTM);
    let mark = s.notice_mark();
    let mut answers = Vec::new();
    for (tool, out) in [("idwInterpolation", "Idw"), ("kriging", "Krig")] {
        let p = json!({"tool": tool, "params": {"input": "Samples", "field": "v", "pixelSize": 100, "crs": "EPSG:3857", "output": out}});
        answers.push(s.run("gp.run", &p).unwrap().to_string());
    }
    let p = json!({"tool": "kernelDensity", "params": {"input": "Samples", "radius": 500, "pixelSize": 50, "crs": "EPSG:3857", "output": "Kde"}});
    answers.push(s.run("gp.run", &p).unwrap().to_string());
    s.run("raster.rasterize", &json!({"layer": "Plot", "crs": "EPSG:4326", "cellSize": 0.001, "name": "Burned"})).unwrap();
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).chain(answers).collect();
    for left_out in ["can't give in longitude/latitude", "beyond what", "left out of the grid's extent"] {
        assert!(!said.iter().any(|t| t.contains(left_out)), "{left_out}: {said:?}");
    }
}

/// A point the layer's own system can't give in longitude/latitude is counted apart from one the
/// grid's system can't hold: IDW from points held in UTM, one far beyond any place UTM holds,
/// says that UTM can't give it, not that the grid can't hold it.
#[test]
fn a_point_its_own_system_cant_give_is_said_as_such() {
    let mut s = Session::new(Project::default());
    let at = [Coord::new(784_000.0, 5_401_300.0), Coord::new(785_000.0, 5_402_300.0), Coord::new(1e12, 1e12)];
    add_in(&mut s, "Samples", GeometryType::Point, at.iter().map(|c| Geometry::Point(*c)).collect(), UTM);
    let mark = s.notice_mark();
    let answer = s
        .run(
            "gp.run",
            &json!({"tool": "idwInterpolation", "params": {"input": "Samples", "field": "v", "pixelSize": 100, "crs": "EPSG:3857", "output": "Idw"}}),
        )
        .unwrap();
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).chain([answer.to_string()]).collect();
    let own = format!("1 point(s) that {} can't give in longitude/latitude were left out", UTM.label());
    assert!(said.iter().any(|t| t.contains(&own)), "{said:?}");
    assert!(!said.iter().any(|t| t.contains("beyond what")), "{said:?}");
}
