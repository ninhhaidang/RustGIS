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

/// A project saves a held layer in longitude/latitude, as older releases read it, and opens it
/// back held there, at the same places.
#[test]
fn a_held_layer_is_saved_in_lonlat() {
    let [plain, mut held] = both();
    let dir = Dir::new("project");
    let path = dir.path().join("held.rgis");
    held.run("project.saveAs", &json!({"path": path.display().to_string()})).unwrap();
    let mut back = Session::new(Project::default());
    back.run("project.open", &json!({"path": path.display().to_string()})).unwrap();
    for layer in ["Wells", "Roads", "Parcels"] {
        assert_eq!(back.map().layer_by_name(layer).unwrap().dataset.coord_crs(), Crs::LonLat);
        assert_close(&lonlat(&back, layer), &lonlat(&plain, layer), layer);
    }
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
