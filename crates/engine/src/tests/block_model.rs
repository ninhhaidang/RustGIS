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
