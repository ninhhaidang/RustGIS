//! A block diagram as a 3D model (#2319): `map.exportBlock` writes the ground of a map's block
//! (#1914) as the settled view lays it out (#2017, #2084), with the map drawn top down over the
//! block's box as its texture, the walls along the box's sides down to the base, and the base, as
//! binary glTF (`.glb`) or glTF with its buffer embedded (`.gltf`), for Blender, a web viewer or a
//! slicer.

use base64::Engine as _;
use rustgis_geom::Coord;
use rustgis_render::terrain::{self, Ground};
use serde_json::{Value, json};

use super::{f, ps};
use crate::Session;

/// The texture's pixels on the block's longer side when none is given, and the range allowed.
const TEXTURE: f64 = 4096.0;
const TEXTURE_RANGE: (f64, f64) = (256.0, rustgis_render::MAX_SIDE as f64);

/// How long elevation tiles are waited for before the model is written with what has come.
const TILE_WAIT: web_time::Duration = web_time::Duration::from_secs(60);

/// A mesh of the model: positions, normals, texture coordinates (the ground's), triangles.
#[derive(Default)]
struct Mesh {
    pos: Vec<[f32; 3]>,
    normal: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    tris: Vec<u32>,
}

impl Mesh {
    /// The triangle `a`, `b`, `c` (indices of positions already pushed), wound to face along `n`.
    fn tri_facing(&mut self, [a, b, c]: [u32; 3], n: [f32; 3]) {
        let p = |i: u32| self.pos[i as usize];
        let (pa, pb, pc) = (p(a), p(b), p(c));
        let (u, v) = (sub(pb, pa), sub(pc, pa));
        let cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let facing = cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2] >= 0.0;
        self.tris.extend(if facing { [a, b, c] } else { [a, c, b] });
    }

    /// A quad of four new positions, all with normal `n`, wound to face along it.
    fn quad(&mut self, corners: [[f32; 3]; 4], n: [f32; 3]) {
        let i = self.pos.len() as u32;
        self.pos.extend(corners);
        self.normal.extend([n; 4]);
        self.tri_facing([i, i + 1, i + 2], n);
        self.tri_facing([i, i + 2, i + 3], n);
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// `map.exportBlock`'s reading of the session (the path, the texture's size, a copy of the map
/// with its block), and the work that writes the model: on a thread of its own from the app, with
/// progress and Cancel, as an export is (`exports::IN_BACKGROUND`), since it waits for elevation
/// tiles and draws a texture of up to 8192 pixels a side.
pub(crate) fn export_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("exporting a block needs the desktop app".into());
    }
    let path = ps(p, "path").ok_or("path is required (a .glb, or a .gltf with everything embedded)")?.to_string();
    let lower = path.to_ascii_lowercase();
    let glb = lower.ends_with(".glb");
    if !glb && !lower.ends_with(".gltf") {
        return Err("path must end in .glb (binary glTF) or .gltf".into());
    }
    let texture = match p.get("texture") {
        None | Some(Value::Null) => TEXTURE,
        Some(_) => f(p, "texture")
            .filter(|t| (TEXTURE_RANGE.0..=TEXTURE_RANGE.1).contains(t))
            .ok_or(format!("texture is pixels on the block's longer side, {} to {}", TEXTURE_RANGE.0, TEXTURE_RANGE.1))?,
    };
    let mut map = s.map().detached();
    let Some(t) = map.view.terrain.as_ref() else {
        return Err("the map has no terrain: set one with map.terrain, with a \"block\"".into());
    };
    let Some(b) = t.block.as_ref() else {
        return Err("the map's terrain has no block diagram: set one with map.terrain's \"block\"".into());
    };
    let walls_layer = b.walls_layer.is_some();
    let round = !b.shape.is_box();
    let exaggeration = t.exaggeration;
    Ok(Box::new(move || {
        let mut notes = Vec::new();
        let written = (|| {
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            if round {
                as_box(&mut map)?;
            }
            let (ground, block, model) = read_ground(&map, texture)?;
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let terrain::Block { x0, y0, x1, y1, .. } = block;
            // The map drawn top down over the box without the terrain (its light, water glint and
            // walls are the 3D viewer's to draw) nor the effects that belong to a view (weather,
            // the sun's light, camera effects): its layers and labels, no scale bar or credits.
            let (bw, bh) = (x1 - x0, y1 - y0);
            let res = bw.max(bh) / texture.round();
            let (tw, th) = ((bw / res).round().max(1.0), (bh / res).round().max(1.0));
            map.view.terrain = None;
            let fx = &mut map.view.effects;
            (fx.weather, fx.sun_time, fx.vignette, fx.tilt_shift, fx.grain) = (rustgis_map::view::Weather::None, None, false, false, false);
            let camera = rustgis_map::Camera::new(Coord::new((x0 + x1) / 2.0, (y0 + y1) / 2.0), res);
            let view = rustgis_render::View { width: tw, height: th, pixels_per_point: 1.0, camera, sky: false };
            let placed = rustgis_render::render_placed(&mut map, &view)?;
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let png = placed.pixmap.encode_png().map_err(|e| e.to_string())?;
            let k = ground.per_unit(Coord::new((x0 + x1) / 2.0, (y0 + y1) / 2.0));
            let sw = rustgis_geom::merc_to_lonlat(Coord::new(x0, y0));
            let extras = json!({
                "origin": [sw.x, sw.y],
                "units": "metres east (x), up (y) and south (z) of the block's south-west corner (longitude, latitude); heights above sea level, exaggerated",
                "exaggeration": exaggeration,
                "metresPerMercatorUnit": k,
                "credits": placed.credits,
            });
            let bytes = gltf(&model.meshes, &png, block.walls, glb, extras)?;
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            rustgis_data::io::write_atomic(std::path::Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            let warn = rustgis_data::diagnostics::Severity::Warning;
            for gap in &placed.gaps {
                notes.push((warn, format!("{path}: the texture: {gap}")));
            }
            if model.missing > 0 {
                notes.push((
                    warn,
                    format!("{path}: {} point(s) of the ground have no height and stand at 0, as the map draws them", model.missing),
                ));
            }
            if model.pending {
                notes.push((warn, format!("{path}: elevation tiles were still on their way, so part of the ground is coarser or at 0: export again once the map has drawn them")));
            }
            let info = rustgis_data::diagnostics::Severity::Info;
            if walls_layer {
                notes.push((info, format!("{path}: the walls are their plain colour: walls coloured by a layer aren't in the model yet")));
            }
            if round {
                notes.push((
                    warn,
                    format!("{path}: the block is round on the map but the box round its circle in the model, based below all its ground: round blocks aren't in the model yet"),
                ));
            }
            notes.push((info, format!("{path}: the ground with the map as its texture, the walls and the base; the view's sky, fog, light and shadows are left to the 3D viewer")));
            let triangles: usize = model.meshes.iter().map(|m| m.tris.len() / 3).sum();
            let (nx, ny) = model.grid;
            Ok(json!({"path": path, "bytes": bytes.len(), "texture": [tw as u32, th as u32], "grid": [nx, ny], "triangles": triangles}))
        })();
        if written.is_err() {
            notes.clear();
        }
        super::exports::drawn(std::slice::from_ref(&map), notes, written, path)
    }))
}

/// The model's meshes (ground, walls, base), the ground's grid, and what it is short of.
struct Model {
    meshes: [Mesh; 3],
    grid: (usize, usize),
    missing: usize,
    pending: bool,
}

/// Turns `map`'s round block into the box round its circle, as the model has it (round blocks
/// aren't in it yet): its base then goes below the lowest ground of all of that box, which the
/// model's walls go down from, not of the circle's only.
fn as_box(map: &mut rustgis_map::Map) -> Result<(), String> {
    let ground = Ground::of(map, &map.camera)
        .ok_or("the map's terrain shows on a Web Mercator map with its DEM layer, so the block can't be read")?;
    let b = ground.block(map).ok_or("the block's box can't be read (over elevation tiles it needs an extent)")?;
    if let Some(block) = map.view.terrain.as_mut().and_then(|t| t.block.as_mut()) {
        block.shape = rustgis_map::view::BlockShape::Box;
        block.extent = Some([b.x0, b.y0, b.x1, b.y1]);
    }
    Ok(())
}

/// The map's ground and block, and the model built from them, elevation tiles waited for (as an
/// export waits for its tiles) until none is on its way, or for [`TILE_WAIT`].
fn read_ground(map: &rustgis_map::Map, texture: f64) -> Result<(Ground, terrain::Block, Model), String> {
    let start = web_time::Instant::now();
    loop {
        let drawn_at = rustgis_render::rustgis_tiles::cache::generation();
        let ground = Ground::of(map, &map.camera)
            .ok_or("the map's terrain shows on a Web Mercator map with its DEM layer, so the block can't be read")?;
        let block = ground.block(map).ok_or("the block's box can't be read (over elevation tiles it needs an extent)")?;
        let model = build(&ground, &block, texture);
        let left = TILE_WAIT.saturating_sub(start.elapsed());
        let cancelled = || rustgis_data::progress::check().is_err();
        if !ground.pending() || left.is_zero() || cfg!(target_arch = "wasm32") {
            let pending = ground.pending();
            return Ok((ground, block, Model { pending, ..model }));
        }
        // Built again only once a tile has come: news of none (the wait ran out) waits on.
        while rustgis_render::rustgis_tiles::cache::generation() == drawn_at && !TILE_WAIT.saturating_sub(start.elapsed()).is_zero() {
            let left = TILE_WAIT.saturating_sub(start.elapsed());
            rustgis_render::rustgis_tiles::cache::wait_for_news_unless(drawn_at, left.min(web_time::Duration::from_secs(30)), cancelled);
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
        }
    }
}

/// The ground over the block's box on the settled view's grid (#2017, #2084), metres east, up
/// and south of the box's south-west corner (scaled as at its centre), texture coordinates from
/// its north-west corner, its cells as a picture `texture` pixels on the longer side lays them
/// out; the walls along its four sides from the ground's edge down to the base;
/// the base.
fn build(ground: &Ground, b: &terrain::Block, texture: f64) -> Model {
    let (bw, bh) = (b.x1 - b.x0, b.y1 - b.y0);
    let k = ground.per_unit(Coord::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0));
    let long = texture.round();
    let short = |a: f64, b: f64| (long * a / b).round().max(1.0) as u32;
    let picture = if bw >= bh { (long as u32, short(bh, bw)) } else { (short(bw, bh), long as u32) };
    let corners = [Coord::new(b.x0, b.y1), Coord::new(b.x1, b.y1), Coord::new(b.x0, b.y0), Coord::new(b.x1, b.y0)];
    let cells = ground.cells_along(&corners);
    let (nx, ny) = terrain::settled_grid(picture, (cells.map(|c| c.0), cells.map(|c| c.1)));
    let (cols, rows) = (nx + 1, ny + 1);
    let mut g = Mesh::default();
    let mut missing = 0;
    for j in 0..rows {
        for i in 0..cols {
            let (u, v) = (i as f64 / nx as f64, j as f64 / ny as f64);
            let m = Coord::new(b.x0 + bw * u, b.y1 - bh * v);
            let h = ground.height(m).unwrap_or_else(|| {
                missing += 1;
                0.0
            });
            g.pos.push([((m.x - b.x0) * k) as f32, h as f32, (-(m.y - b.y0) * k) as f32]);
            g.uv.push([u as f32, v as f32]);
        }
    }
    let at = |i: usize, j: usize| g.pos[j * cols + i];
    for j in 0..rows {
        for i in 0..cols {
            // Central differences (one-sided at the edges): along x and along z (south).
            let tx = sub(at((i + 1).min(nx), j), at(i.saturating_sub(1), j));
            let tz = sub(at(i, (j + 1).min(ny)), at(i, j.saturating_sub(1)));
            let n = [tz[1] * tx[2] - tz[2] * tx[1], tz[2] * tx[0] - tz[0] * tx[2], tz[0] * tx[1] - tz[1] * tx[0]];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            g.normal.push(if len > 0.0 && len.is_finite() { n.map(|c| c / len) } else { [0.0, 1.0, 0.0] });
        }
    }
    for j in 0..ny {
        for i in 0..nx {
            let a = (j * cols + i) as u32;
            let (bb, c) = (a + 1, a + cols as u32);
            g.tris.extend([a, c, bb, bb, c, c + 1]);
        }
    }
    // The walls: each side's ground edge, west to east and north to south, down to the base.
    let base = b.base as f32;
    let mut w = Mesh::default();
    let sides: [(Vec<usize>, [f32; 3]); 4] = [
        ((0..cols).collect(), [0.0, 0.0, -1.0]),
        ((0..cols).map(|i| ny * cols + i).collect(), [0.0, 0.0, 1.0]),
        ((0..rows).map(|j| j * cols).collect(), [-1.0, 0.0, 0.0]),
        ((0..rows).map(|j| j * cols + nx).collect(), [1.0, 0.0, 0.0]),
    ];
    for (edge, n) in &sides {
        for pair in edge.windows(2) {
            let (t0, t1) = (g.pos[pair[0]], g.pos[pair[1]]);
            w.quad([t0, t1, [t1[0], base, t1[2]], [t0[0], base, t0[2]]], *n);
        }
    }
    let (wx, wz) = ((bw * k) as f32, (-bh * k) as f32);
    let mut floor = Mesh::default();
    floor.quad([[0.0, base, 0.0], [wx, base, 0.0], [wx, base, wz], [0.0, base, wz]], [0.0, -1.0, 0.0]);
    Model { meshes: [g, w, floor], grid: (nx, ny), missing, pending: false }
}

/// An sRGB channel (0–255) as the linear value glTF's colour factors take.
fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The model's glTF 2.0: meshes Ground (textured with `png`), Walls and Base (both `walls`), one
/// node each; binary glTF when `glb`, otherwise the JSON with its buffer as a data URI.
fn gltf(meshes: &[Mesh; 3], png: &[u8], walls: rustgis_map::Rgba, glb: bool, extras: Value) -> Result<Vec<u8>, String> {
    let mut buf: Vec<u8> = Vec::new();
    let (mut views, mut accessors) = (Vec::<Value>::new(), Vec::<Value>::new());
    let mut view = |buf: &mut Vec<u8>, bytes: &[u8], target: Option<u32>| {
        while !buf.len().is_multiple_of(4) {
            buf.push(0);
        }
        let mut v = json!({"buffer": 0, "byteOffset": buf.len(), "byteLength": bytes.len()});
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        buf.extend_from_slice(bytes);
        views.push(v);
        views.len() - 1
    };
    let floats = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let mut out = Vec::new();
    for (i, m) in meshes.iter().enumerate() {
        let flat: Vec<f32> = m.pos.iter().flatten().copied().collect();
        let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &m.pos {
            for c in 0..3 {
                (mn[c], mx[c]) = (mn[c].min(p[c]), mx[c].max(p[c]));
            }
        }
        let pv = view(&mut buf, &floats(&flat), Some(34962));
        accessors.push(json!({"bufferView": pv, "componentType": 5126, "count": m.pos.len(), "type": "VEC3", "min": mn, "max": mx}));
        let mut attributes = json!({"POSITION": accessors.len() - 1});
        let normals: Vec<f32> = m.normal.iter().flatten().copied().collect();
        let nv = view(&mut buf, &floats(&normals), Some(34962));
        accessors.push(json!({"bufferView": nv, "componentType": 5126, "count": m.normal.len(), "type": "VEC3"}));
        attributes["NORMAL"] = json!(accessors.len() - 1);
        if !m.uv.is_empty() {
            let uv: Vec<f32> = m.uv.iter().flatten().copied().collect();
            let uvv = view(&mut buf, &floats(&uv), Some(34962));
            accessors.push(json!({"bufferView": uvv, "componentType": 5126, "count": m.uv.len(), "type": "VEC2"}));
            attributes["TEXCOORD_0"] = json!(accessors.len() - 1);
        }
        let idx: Vec<u8> = m.tris.iter().flat_map(|x| x.to_le_bytes()).collect();
        let iv = view(&mut buf, &idx, Some(34963));
        accessors.push(json!({"bufferView": iv, "componentType": 5125, "count": m.tris.len(), "type": "SCALAR"}));
        out.push(
            json!({"primitives": [{"attributes": attributes, "indices": accessors.len() - 1, "material": usize::from(i > 0), "mode": 4}]}),
        );
    }
    let image = view(&mut buf, png, None);
    for (m, name) in out.iter_mut().zip(["Ground", "Walls", "Base"]) {
        m["name"] = json!(name);
    }
    let colour = [linear(walls.r()), linear(walls.g()), linear(walls.b()), 1.0];
    let mut buffer = json!({"byteLength": buf.len()});
    if !glb {
        buffer["uri"] = json!(format!("data:application/octet-stream;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf)));
    }
    let doc = json!({
        "asset": {"version": "2.0", "generator": "RustGIS map.exportBlock"},
        "scene": 0,
        "scenes": [{"name": "Block", "nodes": [0, 1, 2]}],
        "nodes": [{"name": "Ground", "mesh": 0}, {"name": "Walls", "mesh": 1}, {"name": "Base", "mesh": 2}],
        "meshes": out,
        "materials": [
            {"name": "Map", "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}, "metallicFactor": 0.0, "roughnessFactor": 1.0}},
            {"name": "Walls", "pbrMetallicRoughness": {"baseColorFactor": colour, "metallicFactor": 0.0, "roughnessFactor": 1.0}},
        ],
        "textures": [{"sampler": 0, "source": 0}],
        "samplers": [{"magFilter": 9729, "minFilter": 9729, "wrapS": 33071, "wrapT": 33071}],
        "images": [{"bufferView": image, "mimeType": "image/png"}],
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [buffer],
        "extras": extras,
    });
    let mut text = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    if !glb {
        return Ok(text);
    }
    // Binary glTF: a 12-byte header, the JSON chunk padded with spaces, the binary chunk with zeros.
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
    let total = 12 + 8 + text.len() + 8 + buf.len();
    let total = u32::try_from(total).map_err(|_| "the model would pass glTF's 4 GB: give a smaller texture".to_string())?;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&text);
    out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&buf);
    Ok(out)
}
