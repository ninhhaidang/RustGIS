//! `map.exportBlock` (#2319): a block diagram written as a glTF model.

use base64::Engine as _;
use serde_json::{Value, json};

use super::{dem_b64, world};

/// The DEM's corners (10–10.04° E, 45–45.04° N) in Web Mercator, about x 1113195–1117648 and
/// y 5621521–5627821: a box inside them.
const INSIDE: [f64; 4] = [1_114_000.0, 5_622_000.0, 1_116_000.0, 5_625_000.0];

fn out(name: &str) -> String {
    let dir = std::env::temp_dir().join("rustgis-block-model");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name).to_string_lossy().into_owned()
}

/// A session with the test DEM and a block over `INSIDE`, exaggerated twice; and the DEM's name.
fn blocked() -> (crate::Session, String) {
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).map(|l| l.name.clone()).unwrap();
    s.run("map.terrain", &json!({"layer": dem, "exaggeration": 2, "block": {"extent": INSIDE, "depth": 50, "walls": "#806040"}})).unwrap();
    (s, dem)
}

/// A GLB's JSON and binary chunk, its header and chunk lengths checked.
fn read_glb(bytes: &[u8]) -> (Value, Vec<u8>) {
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
    assert_eq!(&bytes[0..4], b"glTF");
    assert_eq!(u32_at(4), 2);
    assert_eq!(u32_at(8), bytes.len(), "the header's length is the file's");
    let json_len = u32_at(12);
    assert_eq!(&bytes[16..20], b"JSON");
    assert!(json_len.is_multiple_of(4));
    let doc: Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
    let bin_at = 20 + json_len;
    let bin_len = u32_at(bin_at);
    assert_eq!(&bytes[bin_at + 4..bin_at + 8], b"BIN\0");
    assert_eq!(bin_at + 8 + bin_len, bytes.len());
    (doc, bytes[bin_at + 8..].to_vec())
}

/// Accessor `i`'s floats (positions, normals, texture coordinates), its view checked to lie in
/// the buffer.
fn floats(doc: &Value, bin: &[u8], i: u64) -> Vec<f32> {
    let a = &doc["accessors"][i as usize];
    let v = &doc["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
    let (off, len) = (v["byteOffset"].as_u64().unwrap() as usize, v["byteLength"].as_u64().unwrap() as usize);
    assert!(off.is_multiple_of(4) && off + len <= bin.len());
    let width = match a["type"].as_str().unwrap() {
        "VEC3" => 3,
        "VEC2" => 2,
        _ => 1,
    };
    assert_eq!(len, a["count"].as_u64().unwrap() as usize * width * 4);
    bin[off..off + len].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}

/// Mesh `m`'s positions (x, y, z) and triangle count.
fn mesh(doc: &Value, bin: &[u8], m: usize) -> (Vec<[f32; 3]>, usize) {
    let prim = &doc["meshes"][m]["primitives"][0];
    let pos = floats(doc, bin, prim["attributes"]["POSITION"].as_u64().unwrap());
    let a = &doc["accessors"][prim["attributes"]["POSITION"].as_u64().unwrap() as usize];
    let pts: Vec<[f32; 3]> = pos.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
    // The bounds glTF requires of positions are the positions' own.
    for c in 0..3 {
        let lo = pts.iter().map(|p| p[c]).fold(f32::MAX, f32::min);
        let hi = pts.iter().map(|p| p[c]).fold(f32::MIN, f32::max);
        assert_eq!((a["min"][c].as_f64().unwrap() as f32, a["max"][c].as_f64().unwrap() as f32), (lo, hi));
    }
    let idx = &doc["accessors"][prim["indices"].as_u64().unwrap() as usize];
    let count = idx["count"].as_u64().unwrap() as usize;
    assert_eq!(count % 3, 0);
    (pts, count / 3)
}

#[test]
fn a_block_is_exported_as_a_glb_with_the_map_as_its_texture_and_walls_meeting_the_ground() {
    let (mut s, dem) = blocked();
    let path = out("block.glb");
    let v = s.run("map.exportBlock", &json!({"path": path, "texture": 512})).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(v["bytes"].as_u64(), Some(bytes.len() as u64));
    let (doc, bin) = read_glb(&bytes);
    assert_eq!(doc["asset"]["version"], "2.0");
    let names: Vec<&str> = doc["meshes"].as_array().unwrap().iter().filter_map(|m| m["name"].as_str()).collect();
    assert_eq!(names, ["Ground", "Walls", "Base"]);
    // The texture: a PNG as wide as asked on the box's longer side (3000 m north–south, 2000 m
    // east–west in Web Mercator units).
    let image = &doc["bufferViews"][doc["images"][0]["bufferView"].as_u64().unwrap() as usize];
    let off = image["byteOffset"].as_u64().unwrap() as usize;
    assert_eq!(&bin[off..off + 8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(v["texture"], json!([341, 512]));
    assert_eq!(doc["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"], 0);
    // The ground's corners stand at the DEM's height there, exaggerated.
    let raster = s.map().layer_by_name(&dem).and_then(|l| l.raster.as_ref()).and_then(|r| r.data.clone()).unwrap();
    let (ground, _) = mesh(&doc, &bin, 0);
    let (nx, ny) = (v["grid"][0].as_u64().unwrap() as usize, v["grid"][1].as_u64().unwrap() as usize);
    assert_eq!(ground.len(), (nx + 1) * (ny + 1));
    let [x0, y0, x1, y1] = INSIDE;
    for (i, j, m) in [(0, 0, (x0, y1)), (nx, 0, (x1, y1)), (0, ny, (x0, y0)), (nx, ny, (x1, y0))] {
        let want = rustgis_render::terrain::dem_height(&raster, rustgis_geom::Coord::new(m.0, m.1)).unwrap() * 2.0;
        let got = ground[j * (nx + 1) + i][1];
        assert!((f64::from(got) - want).abs() < 0.01, "corner {i}, {j}: {got} vs {want}");
    }
    // South-west corner at the origin, north toward -z, metres scaled as at the centre.
    let (sw, ne) = (ground[ny * (nx + 1)], ground[nx]);
    assert_eq!((sw[0], sw[2]), (0.0, 0.0));
    assert!(ne[0] > 1000.0 && ne[2] < -1500.0 && ne[0] < 2000.0 && ne[2] > -3000.0, "{ne:?}");
    // Every wall vertex above the base is one of the ground's: no gap between them.
    let (walls, wall_tris) = mesh(&doc, &bin, 1);
    let (base, base_tris) = mesh(&doc, &bin, 2);
    let floor = base[0][1];
    assert!(base.iter().all(|p| p[1] == floor) && base_tris == 2);
    let lowest = ground.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
    assert!(((lowest - floor) - 100.0).abs() < 0.01, "the base is the depth below the lowest ground, exaggerated");
    let on_ground: std::collections::HashSet<[u32; 3]> = ground.iter().map(|p| p.map(f32::to_bits)).collect();
    let tops: Vec<&[f32; 3]> = walls.iter().filter(|p| p[1] != floor).collect();
    assert!(!tops.is_empty() && tops.iter().all(|p| on_ground.contains(&p.map(f32::to_bits))));
    assert_eq!(wall_tris, 2 * 2 * (nx + ny), "two triangles a cell along each of the four sides");
    // The walls' colour, linear as glTF takes it (#806040 in sRGB).
    let c = &doc["materials"][1]["pbrMetallicRoughness"]["baseColorFactor"];
    assert!((c[0].as_f64().unwrap() - 0.2158).abs() < 1e-3 && c[3] == 1.0, "{c}");
    // Triangles index positions that exist.
    let idx = &doc["accessors"][doc["meshes"][0]["primitives"][0]["indices"].as_u64().unwrap() as usize];
    let view = &doc["bufferViews"][idx["bufferView"].as_u64().unwrap() as usize];
    let off = view["byteOffset"].as_u64().unwrap() as usize;
    let len = view["byteLength"].as_u64().unwrap() as usize;
    assert!(bin[off..off + len].chunks(4).all(|c| (u32::from_le_bytes(c.try_into().unwrap()) as usize) < ground.len()));
}

#[test]
fn a_block_is_exported_as_gltf_with_its_buffer_embedded() {
    let (mut s, _) = blocked();
    let path = out("block.gltf");
    s.run("map.exportBlock", &json!({"path": path, "texture": 256})).unwrap();
    let doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let uri = doc["buffers"][0]["uri"].as_str().unwrap();
    let data =
        base64::engine::general_purpose::STANDARD.decode(uri.strip_prefix("data:application/octet-stream;base64,").unwrap()).unwrap();
    assert_eq!(doc["buffers"][0]["byteLength"].as_u64(), Some(data.len() as u64));
    let (ground, tris) = mesh(&doc, &data, 0);
    assert!(tris > 0 && !ground.is_empty());
}

#[test]
fn exporting_a_block_is_refused_without_one_and_with_wrong_settings() {
    let mut s = world();
    let e = s.run("map.exportBlock", &json!({"path": out("none.glb")})).unwrap_err();
    assert!(e.contains("no terrain"), "{e}");
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).map(|l| l.name.clone()).unwrap();
    s.run("map.terrain", &json!({"layer": dem})).unwrap();
    let e = s.run("map.exportBlock", &json!({"path": out("none.glb")})).unwrap_err();
    assert!(e.contains("no block diagram"), "{e}");
    s.run("map.terrain", &json!({"block": {}})).unwrap();
    assert!(s.run("map.exportBlock", &json!({})).unwrap_err().contains("path is required"));
    assert!(s.run("map.exportBlock", &json!({"path": out("block.obj")})).unwrap_err().contains(".glb"));
    for bad in [json!(100), json!(9000), json!("big")] {
        let e = s.run("map.exportBlock", &json!({"path": out("bad.glb"), "texture": bad})).unwrap_err();
        assert!(e.contains("texture"), "{bad}: {e}");
    }
    assert!(!std::path::Path::new(&out("none.glb")).exists() && !std::path::Path::new(&out("bad.glb")).exists());
}

#[test]
fn the_texture_leaves_out_the_views_weather_and_camera_effects() {
    let (mut s, _) = blocked();
    let (plain, fogged) = (out("plain.glb"), out("fogged.glb"));
    s.run("map.exportBlock", &json!({"path": plain, "texture": 256})).unwrap();
    let fx = &mut s.map_mut().view.effects;
    (fx.weather, fx.vignette, fx.grain) = (rustgis_map::view::Weather::Fog, true, true);
    s.run("map.exportBlock", &json!({"path": fogged, "texture": 256})).unwrap();
    assert_eq!(std::fs::read(&plain).unwrap(), std::fs::read(&fogged).unwrap(), "fog, vignette and grain stay out of the model");
    assert_eq!(s.map().view.effects.weather, rustgis_map::view::Weather::Fog, "the map keeps its weather");
}

#[test]
fn a_round_block_is_exported_as_its_box_with_a_notice_and_a_box_with_none() {
    let (mut s, _) = blocked();
    let round = |s: &crate::Session, mark| s.notices_since(mark).iter().filter(|n| n.text.contains("is round")).count();
    let mark = s.notice_mark();
    s.run("map.exportBlock", &json!({"path": out("box.glb"), "texture": 256})).unwrap();
    assert_eq!(round(&s, mark), 0, "{:?}", s.notices_since(mark));
    s.run("map.terrain", &json!({"block": {"extent": INSIDE, "shape": "circle"}})).unwrap();
    let mark = s.notice_mark();
    s.run("map.exportBlock", &json!({"path": out("round.glb"), "texture": 256})).unwrap();
    assert_eq!(round(&s, mark), 1, "{:?}", s.notices_since(mark));
}

#[test]
fn a_round_blocks_model_is_the_box_round_its_circle_based_below_all_of_its_ground() {
    let (mut s, _) = blocked();
    let meshes = |s: &mut crate::Session, block: Value, name: &str| {
        s.run("map.terrain", &json!({"block": block})).unwrap();
        s.run("map.exportBlock", &json!({"path": out(name), "texture": 256})).unwrap();
        let (doc, bin) = read_glb(&std::fs::read(out(name)).unwrap());
        (0..3).map(|m| mesh(&doc, &bin, m).0).collect::<Vec<_>>()
    };
    let round = meshes(&mut s, json!({"extent": INSIDE, "shape": "circle", "depth": 50}), "round_box.glb");
    // The circle's box: as wide as INSIDE, as tall, centred.
    let [x0, y0, x1, y1] = INSIDE;
    let (cy, r) = ((y0 + y1) / 2.0, (x1 - x0) / 2.0);
    let square = meshes(&mut s, json!({"extent": [x0, cy - r, x1, cy + r], "shape": "box", "depth": 50}), "square.glb");
    // The ground outside the circle is in the model, so its base is the box's, below all of it:
    // no wall runs up from the ground to a base taken from the circle's ground only.
    assert_eq!(round, square);
    let floor = round[2][0][1];
    let lowest = round[0].iter().map(|p| p[1]).fold(f32::MAX, f32::min);
    assert!(((lowest - floor) - 100.0).abs() < 0.01 && round[1].iter().all(|p| p[1] >= floor), "{lowest} {floor}");
}

/// Where unit A (west) meets unit B (east) on the walls layer of [`by_units`], in degrees east.
const CONTACT_LON: f64 = 10.016;

/// [`blocked`] with its walls coloured by a polygon layer: unit A solid red west of
/// [`CONTACT_LON`], unit B solid blue with level black hatch lines east of it; with a lake (east
/// of 10.02 E, north of 45.01 N) as the terrain's water, shown in the walls' section, when `lake`.
fn by_units(lake: bool) -> crate::Session {
    let (mut s, dem) = blocked();
    let add = |s: &mut crate::Session, file: &str, gj: Value| {
        let data = base64::engine::general_purpose::STANDARD.encode(gj.to_string().as_bytes());
        s.run("layer.addDataBytes", &json!({"fileName": file, "data": data})).unwrap();
        let stem = file.trim_end_matches(".geojson");
        s.map().layers.iter().find(|l| l.name.starts_with(stem)).map(|l| l.name.clone()).unwrap()
    };
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]]);
    let feature =
        |props: Value, ring: Value| json!({"type": "Feature", "properties": props, "geometry": {"type": "Polygon", "coordinates": ring}});
    let units = json!({"type": "FeatureCollection", "features": [
        feature(json!({"u": "A"}), rect(9.9, 44.9, CONTACT_LON, 45.1)),
        feature(json!({"u": "B"}), rect(CONTACT_LON, 44.9, 10.1, 45.1)),
    ]});
    let geo = add(&mut s, "units.geojson", units);
    s.run("layer.symbology", &json!({"layer": geo, "type": "unique", "field": "u"})).unwrap();
    s.run("layer.fillLayers", &json!({"layer": geo, "value": "A", "fills": [{"color": "#FF0000", "pattern": "solid"}]})).unwrap();
    let b_fills =
        json!([{"color": "#0000FF", "pattern": "solid"}, {"color": "#000000", "pattern": "hatch", "angle": 0, "spacing": 4, "width": 1}]);
    s.run("layer.fillLayers", &json!({"layer": geo, "value": "B", "fills": b_fills})).unwrap();
    let mut terrain = json!({"layer": dem, "block": {"walls": {"color": "#806040", "layer": geo}}});
    if lake {
        let lake = add(
            &mut s,
            "lake.geojson",
            json!({"type": "FeatureCollection", "features": [feature(json!({}), rect(10.02, 45.01, 10.1, 45.1))]}),
        );
        terrain["water"] = json!({"layer": lake});
        terrain["block"]["water"] = json!({"depth": 10});
    }
    s.run("map.terrain", &terrain).unwrap();
    s
}

/// The texture coordinates of primitive `p` of the walls (mesh 1), and its picture.
fn wall_side(doc: &Value, bin: &[u8], p: usize) -> (Vec<[f32; 2]>, rustgis_render::Pixmap) {
    let prim = &doc["meshes"][1]["primitives"][p];
    let uv = floats(doc, bin, prim["attributes"]["TEXCOORD_0"].as_u64().unwrap());
    let material = &doc["materials"][prim["material"].as_u64().unwrap() as usize];
    let texture = &doc["textures"][material["pbrMetallicRoughness"]["baseColorTexture"]["index"].as_u64().unwrap() as usize];
    let image = &doc["images"][texture["source"].as_u64().unwrap() as usize];
    assert_eq!(image["mimeType"], "image/png");
    let v = &doc["bufferViews"][image["bufferView"].as_u64().unwrap() as usize];
    let (off, len) = (v["byteOffset"].as_u64().unwrap() as usize, v["byteLength"].as_u64().unwrap() as usize);
    let picture = rustgis_render::Pixmap::decode_png(&bin[off..off + len]).unwrap();
    (uv.chunks(2).map(|c| [c[0], c[1]]).collect(), picture)
}

#[test]
fn walls_coloured_by_a_polygon_layer_get_a_picture_a_side_meeting_the_ground() {
    let mut s = by_units(false);
    let path = out("units.glb");
    let mark = s.notice_mark();
    let v = s.run("map.exportBlock", &json!({"path": path, "texture": 512})).unwrap();
    let notices: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(!notices.iter().any(|n| n.contains("plain colour") || n.contains("water") || n.contains("squeezed")), "{notices:?}");
    let (doc, bin) = read_glb(&std::fs::read(&path).unwrap());
    assert_eq!(doc["meshes"][1]["primitives"].as_array().map(Vec::len), Some(4), "a primitive a side");
    assert_eq!(doc["images"].as_array().map(Vec::len), Some(5), "the map and the four sides");
    let (ground, _) = mesh(&doc, &bin, 0);
    let on_ground: std::collections::HashSet<[u32; 3]> = ground.iter().map(|p| p.map(f32::to_bits)).collect();
    let floor = mesh(&doc, &bin, 2).0[0][1];
    let (nx, ny) = (v["grid"][0].as_u64().unwrap() as usize, v["grid"][1].as_u64().unwrap() as usize);
    let wide = ground[nx][0];
    let [x0, _, x1, _] = INSIDE;
    let contact = (rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(CONTACT_LON, 45.0)).x - x0) / (x1 - x0);
    let (red, blue) = (|c: (u8, u8, u8)| c == (255, 0, 0), |c: (u8, u8, u8)| c == (0, 0, 255));
    let mut cells = 0;
    for (p, name) in ["South", "East", "North", "West"].iter().enumerate() {
        let prim = &doc["meshes"][1]["primitives"][p];
        assert_eq!(doc["materials"][prim["material"].as_u64().unwrap() as usize]["name"], format!("Walls {name}"));
        // Every vertex above the base is one of the ground's, bit for bit: no gap.
        let pos = floats(&doc, &bin, prim["attributes"]["POSITION"].as_u64().unwrap());
        let pts: Vec<[f32; 3]> = pos.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
        assert!(pts.iter().filter(|q| q[1] != floor).all(|q| on_ground.contains(&q.map(f32::to_bits))), "{name}");
        let idx = &doc["accessors"][prim["indices"].as_u64().unwrap() as usize];
        cells += idx["count"].as_u64().unwrap() as usize / 6;
        let (uv, picture) = wall_side(&doc, &bin, p);
        assert_eq!(uv.len(), pts.len());
        assert!(uv.iter().flatten().all(|c| (0.0..=1.0).contains(c)), "{name}: {uv:?}");
        // The feet along the last row, the side's highest ground along the first.
        assert!(uv.iter().zip(&pts).all(|(t, q)| (q[1] == floor) == (t[1] == 1.0)), "{name}");
        assert!(uv.iter().any(|t| t[1] == 0.0), "{name}");
        // Seen from outside, left to right: the south side west to east, the north east to west.
        for (t, q) in uv.iter().zip(&pts) {
            let x = q[0] / wide;
            match p {
                0 => assert!((t[0] - x).abs() < 1e-5, "{name}: {t:?} {q:?}"),
                2 => assert!((t[0] - (1.0 - x)).abs() < 1e-5, "{name}: {t:?} {q:?}"),
                _ => {}
            }
        }
        // The colours at known places: a column of unit A all red; one of B blue with black lines.
        let column = |u: f64| {
            let x = (u * f64::from(picture.width())) as u32;
            (0..picture.height()).filter_map(|y| picture.pixel(x, y)).map(|c| (c.red(), c.green(), c.blue())).collect::<Vec<_>>()
        };
        let in_a = |u: f64| column(u).iter().all(|c| red(*c));
        let in_b = |u: f64| {
            let col = column(u);
            !col.iter().any(|c| red(*c)) && col.iter().any(|c| blue(*c)) && col.iter().any(|c| c.2 < 64)
        };
        match p {
            // The south and north sides cross the contact; the west lies in A, the east in B.
            0 => assert!(in_a(contact / 2.0) && in_b((1.0 + contact) / 2.0), "{name}"),
            2 => assert!(in_b((1.0 - contact) / 2.0) && in_a(1.0 - contact / 2.0), "{name}"),
            1 => assert!(in_b(0.5), "{name}"),
            _ => assert!(in_a(0.5), "{name}"),
        }
    }
    assert_eq!(cells, 2 * (nx + ny), "a cell along each side, as plain walls have");
}

#[test]
fn walls_cutting_through_water_are_said_to_leave_its_section_out() {
    let mut s = by_units(true);
    let mark = s.notice_mark();
    s.run("map.exportBlock", &json!({"path": out("lake.glb"), "texture": 256})).unwrap();
    let said = s.notices_since(mark).iter().filter(|n| n.text.contains("run through water")).count();
    assert_eq!(said, 1, "{:?}", s.notices_since(mark));
}

#[test]
fn walls_too_tall_for_their_pictures_are_said_to_be_squeezed() {
    let mut s = by_units(false);
    // Unit B's black dots, 12 points wide and 48 apart (fewer than the walls' most marks down
    // the tall wall), on its blue.
    let dots = json!([{"color": "#0000FF", "pattern": "solid"}, {"color": "#000000", "pattern": "dots", "marker": "circle", "size": 12, "spacing": 48}]);
    let geo = s.map().layers.iter().find(|l| l.name.starts_with("units")).map(|l| l.name.clone()).unwrap();
    s.run("layer.fillLayers", &json!({"layer": geo, "value": "B", "fills": dots})).unwrap();
    s.run("map.terrain", &json!({"exaggeration": 20, "block": {"depth": 20000}})).unwrap();
    let path = out("tall.glb");
    let mark = s.notice_mark();
    s.run("map.exportBlock", &json!({"path": path, "texture": 256})).unwrap();
    let said = s.notices_since(mark).iter().filter(|n| n.text.contains("squeezed")).count();
    assert_eq!(said, 1, "{:?}", s.notices_since(mark));
    let (doc, bin) = read_glb(&std::fs::read(&path).unwrap());
    assert_eq!(wall_side(&doc, &bin, 0).1.height(), rustgis_render::MAX_SIDE, "no picture taller than a picture can be");
    // The dots are squeezed as their places are, so they stay round on the wall the picture is
    // stretched back on: in the east side's picture (all unit B) wide and flat, not round and
    // run together down the squeezed rows.
    let (_, east) = wall_side(&doc, &bin, 1);
    let dark = |x: u32, y: u32| east.pixel(x, y).is_some_and(|c| c.blue() < 128);
    let longest = |n: u32, at: &dyn Fn(u32) -> bool| {
        (0..n).fold((0, 0), |(run, best), i| if at(i) { (run + 1, best.max(run + 1)) } else { (0, best) }).1
    };
    let across = (0..east.height()).map(|y| longest(east.width(), &|x| dark(x, y))).max().unwrap_or(0);
    let down = (0..east.width()).map(|x| longest(east.height(), &|y| dark(x, y))).max().unwrap_or(0);
    assert!(across >= 8 && across >= 3 * down, "dots {across} px across, {down} down");
}
