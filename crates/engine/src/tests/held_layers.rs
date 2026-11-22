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

/// Geometry calculations and a buffer give the same measures and places.
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
    run_both(&mut ss, "gp.run", &json!({"tool": "buffer", "params": {"input": "Wells", "distance": 100}}));
    assert_close(&lonlat(&ss[0], "Wells_Buffer"), &lonlat(&ss[1], "Wells_Buffer"), "buffer");
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

/// Rasterizing a held layer gives the grid of the same layer held in longitude/latitude (cells
/// off the parcels' edges, which may read a hair either side of a cell's through UTM).
#[test]
fn a_held_layer_is_rasterized_alike() {
    let mut ss = both();
    let [a, b] = run_both(&mut ss, "raster.rasterize", &json!({"layer": "Parcels", "field": "n", "cellSize": 0.0007, "nodata": -1}));
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
    run_both(&mut ss, "raster.rasterize", &json!({"layer": "Parcels", "field": "n", "cellSize": 0.0007, "nodata": -1}));
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
