//! View tab: view effects (illumination, weather, visual and camera effects, post-processing),
//! colour vision simulation, scene drawing mode and depth priority, view clipping, display type,
//! global and local views, navigation modes, the navigator and the camera.

use rustgis_map::view::{ColorVision, Weather};
use serde_json::{Value, json};

use super::layer::with;
use super::{b, f, need_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("map.effects", "View Effects", "View › Effects", effects).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"sunTime"?: seconds since 1970|"now"|null (illumination), "weather"?: "none"|"haze"|"fog"|"rain"|"snow", "vignette"?, "edges"?, "stars"?: bool (visual effects; stars around the globe), "tiltShift"?, "grain"?: bool (camera effects), "brightness"?, "contrast"?, "saturation"?: -100..100, "sepia"?: 0..100 (post-processing), "reset"?: bool}"#,
        ),
        C::new("map.colorVision", "Color Vision Simulator", "View › Accessibility", |s, p| {
            let m = ps(p, "mode").ok_or("mode is required")?;
            let cv = ColorVision::from_code(m).ok_or_else(|| format!("unknown colour vision '{m}'"))?;
            s.map_mut().view.color_vision = cv;
            Ok(json!({"mode": cv.code()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"mode": "normal"|"protanopia"|"deuteranopia"|"tritanopia"|"achromatopsia"}"#),
        C::new("layer.priority", "Depth Priority", "View › Scene", |s, p| {
            let v = p.get("value").and_then(Value::as_i64).ok_or("value is required (higher draws on top)")?.clamp(-1000, 1000) as i32;
            with(s, p, |l| l.priority = v)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_layer)
        .params(r#"{"layer"?, "value": integer (higher draws on top)}"#),
        C::new("map.drawingMode", "Drawing Mode", "View › Scene", |s, p| {
            let w = match ps(p, "mode").ok_or("mode is required")? {
                "standard" => false,
                "wireframe" => true,
                m => return Err(format!("unknown drawing mode '{m}'")),
            };
            s.map_mut().view.wireframe = w;
            Ok(json!({"wireframe": w}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"mode": "standard"|"wireframe"}"#),
        C::new("map.clipping", "View Clipping", "View › View Clipping", |s, p| {
            let d = match ps(p, "mode").unwrap_or("distance") {
                "none" => None,
                "distance" => Some(f(p, "distance").filter(|d| *d > 0.0).ok_or("distance is required (metres)")?.min(4e7)),
                m => return Err(format!("unknown clipping mode '{m}'")),
            };
            s.map_mut().view.clip_distance = d;
            Ok(json!({"distance": d}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"mode": "none"|"distance", "distance"?: metres from the view's centre}"#),
        C::new("map.terrain", "Terrain", "View › Scene", terrain).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r##"{"layer"?: a DEM raster layer (heights in metres; without layer or tiles, and no terrain yet, the active layer when it is a raster) | "tiles"?: elevation tile URL with {z}/{x}/{y}, "encoding"?: "terrarium"|"mapbox" (tiles; default terrarium), "exaggeration"?: 0 to 20 (default 1, or the terrain's), "block"?: a block diagram {"extent"?: "layer" (the DEM's extent, default) | [xmin, ymin, xmax, ymax] in map units (needed over tiles), "depth"?: metres from the lowest ground to the base (default half the relief, at least 100), "walls"?: "#RRGGBB" (default #C8B89A) | {"color"?: "#RRGGBB" (default #C8B89A), "layer"?: a polygon layer that holds all its features (not one read a view at a time; it may be hidden): its symbology colours the walls where its polygons meet the block's edge, and the walls are color off its polygons}, "shape"?: "box" (default) | "circle" (the largest circle in the box, its walls round it) | {"layer": a polygon layer, "where"?: sql (picks its polygons, as a definition query)} (the ground inside the polygons' outer rings, holes not cut, the box round them: no extent), "radius"?: a circle's radius in map units (default: the largest in the box), "water"?: the terrain's water shown in the walls' section {"depth"?: metres from the level to the floor, "field"?: a numeric field of the water layer holding each one's depth} (one of them needed), or false}, or false to take it off (default: as it was), "water"?: water bodies drawn flat at their level {"layer": a polygon layer, "level"?: a numeric field of levels in metres (default: each polygon's lowest height along its shore), "glint"?: 0 to 1, how brightly a tilted view's water shines where it reflects the sun toward the eye (default 0.3; 0: none), "shore"?: 0 to 1, how dark the band along the water's shore is at the outline, fading a few pixels inside it (default 0.35; 0: none)}, or false to take it off (default: as it was), "off"?: true}"##,
        ),
        C::new("map.exportOrbit", "Export Orbit", "View › Scene", |s, p| super::exports::write_here(s, p, "map.exportOrbit")).inputs(orbit_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"path": "*.gif" | "*.zip" (PNG frames), "frames"?: 1 to 2000 (default 60), "turn"?: degrees the bearing turns by over the frames (default 360, once round; negative turns the other way), "width"?, "height"?: points, 64 to 4096 (default 640 × 400), "dpi"?: 48 to 600 (default 96), "speed"?: milliseconds a GIF frame shows for (default 50; a GIF keeps it to the hundredth of a second)}"#),
        C::new("map.exportBlock", "Export Block", "View › Scene", |s, p| super::exports::write_here(s, p, "map.exportBlock")).inputs(orbit_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"path": "*.glb" | "*.gltf" (a 3D model of the map's block diagram: the ground with the map as its texture, the walls and the base), "texture"?: pixels on the block's longer side, 256 to 8192 (default 4096)}"#),
        C::new("map.displayType", "Display Type", "View › Options", |s, p| {
            let fast = match ps(p, "mode").ok_or("mode is required")? {
                "quality" => false,
                "speed" => true,
                m => return Err(format!("unknown display type '{m}'")),
            };
            s.map_mut().view.fast = fast;
            Ok(json!({"mode": if fast { "speed" } else { "quality" }}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"mode": "quality"|"speed"}"#),
        C::new("map.navigator", "Navigator", "View › Navigation", |s, p| {
            let v = &mut s.map_mut().view;
            v.navigator = b(p, "on").unwrap_or(!v.navigator);
            Ok(json!({"navigator": v.navigator}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"on"?: bool}"#),
        C::new("map.camera", "Camera", "View › Navigation", camera).roles(&[]).effects(crate::policy::Effects::NONE).view().params(r#"{"lon"?, "lat"?, "scale"? (the camera's position when omitted)}"#),
        C::new("map.navigation", "Navigation Mode", "Home › Navigate", |s, p| {
            let fp = match ps(p, "mode").ok_or("mode is required")? {
                "planar" => false,
                "firstPerson" => true,
                m => return Err(format!("unknown navigation mode '{m}'")),
            };
            let m = s.map_mut();
            m.view.first_person = fp;
            // Planar navigation is on a flat map.
            if !fp && m.projection.is_globe() {
                m.set_projection(rustgis_map::Projection::WebMercator);
            }
            Ok(json!({"mode": if fp { "firstPerson" } else { "planar" }}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"mode": "planar"|"firstPerson"}"#),
        C::new("map.viewMode", "View", "View › View", |s, p| {
            let next = match ps(p, "mode").ok_or("mode is required")? {
                "global" => rustgis_map::Projection::Globe,
                "local" => {
                    let c = s.map().center_lonlat();
                    rustgis_map::Projection::from_code(&format!("local:{:.4},{:.4}", c.x, c.y))?
                }
                m => return Err(format!("unknown view '{m}'")),
            };
            let m = s.map_mut();
            m.set_projection(next);
            Ok(json!({"projection": m.projection.code(), "label": m.projection.label()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"mode": "global" (a globe)|"local" (a flat view centred here)}"#),
        C::new("map.walk", "Walk", "Home › Navigate", walk).roles(&[]).effects(crate::policy::Effects::NONE).view().params(r#"{"forward"?: steps, "right"?: steps, "up"?: steps (first-person navigation)}"#),
    ]
}

fn effects(s: &mut Session, p: &Value) -> CmdResult {
    // Built on a copy and applied only when every parameter is valid.
    let mut fx = s.map().view.effects.clone();
    if b(p, "reset").unwrap_or(false) {
        fx = Default::default();
    }
    if let Some(v) = p.get("sunTime") {
        fx.sun_time = match v {
            Value::Null => None,
            Value::String(t) if t == "now" => {
                Some(web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
            }
            _ => Some(v.as_f64().filter(|t| t.is_finite()).ok_or("sunTime is seconds since 1970, \"now\" or null")?),
        };
    }
    if let Some(w) = ps(p, "weather") {
        fx.weather = match w {
            "none" => Weather::None,
            "haze" => Weather::Haze,
            "fog" => Weather::Fog,
            "rain" => Weather::Rain,
            "snow" => Weather::Snow,
            o => return Err(format!("unknown weather '{o}'")),
        };
    }
    for (k, slot) in [
        ("vignette", &mut fx.vignette),
        ("edges", &mut fx.edges),
        ("stars", &mut fx.stars),
        ("tiltShift", &mut fx.tilt_shift),
        ("grain", &mut fx.grain),
    ] {
        if let Some(v) = b(p, k) {
            *slot = v;
        }
    }
    for (k, slot, lo) in [
        ("brightness", &mut fx.brightness, -100.0),
        ("contrast", &mut fx.contrast, -100.0),
        ("saturation", &mut fx.saturation, -100.0),
        ("sepia", &mut fx.sepia, 0.0),
    ] {
        if let Some(v) = f(p, k) {
            *slot = v.clamp(lo, 100.0);
        }
    }
    let out = serde_json::to_value(&fx).map_err(|e| e.to_string())?;
    s.map_mut().view.effects = fx;
    Ok(out)
}

fn camera(s: &mut Session, p: &Value) -> CmdResult {
    let (lon, lat, scale) = (f(p, "lon"), f(p, "lat"), f(p, "scale"));
    if lon.is_some() || lat.is_some() || scale.is_some() {
        let c = s.map().center_lonlat();
        let mut q = json!({"lon": lon.unwrap_or(c.x), "lat": lat.unwrap_or(c.y)});
        if let Some(sc) = scale {
            q["scale"] = json!(sc);
        }
        super::navigate::go_to_xy(s, &q)?;
    }
    let c = s.map().center_lonlat();
    Ok(json!({"lon": c.x, "lat": c.y, "scale": s.scale(), "projection": s.map().projection.label()}))
}

/// The most degrees an orbit export turns by: ten times round.
const MAX_TURN: f64 = 3600.0;

/// The bearing of frame `i` of the `n` of an orbit that turns by `turn` degrees from `start`.
/// Whole turns stop one step short of where they began, so the animation loops with no frame
/// shown twice; a part of a turn ends on its last bearing.
fn orbit_bearing(start: f64, turn: f64, i: usize, n: usize) -> f64 {
    let steps = if turn.rem_euclid(360.0) == 0.0 { n } else { n.saturating_sub(1).max(1) };
    start + turn * i as f64 / steps as f64
}

/// An orbit draws the map's own sources at every bearing: the renderer's bindings.
fn orbit_inputs(input: &super::InputContext<'_>, _: &Value) -> Result<super::Inputs, String> {
    Ok(super::tiles::render_inputs(input.map()))
}

/// What an orbit export draws, its parameters checked: [`check_orbit_params`].
struct Orbit {
    frames: f64,
    turn: f64,
    w: f64,
    h: f64,
    dpi: f64,
    speed: u32,
}

/// Checks an orbit export's parameters other than its path (each number in its range, and the
/// pixels within the bound), with nothing drawn or written: the app calls it before its file
/// picker opens, and [`orbit_work`] before it draws.
pub fn check_orbit_params(p: &Value) -> Result<(), String> {
    orbit_params(p).map(|_| ())
}

fn orbit_params(p: &Value) -> Result<Orbit, String> {
    use super::time::{MAX_FRAMES, MAX_PIXELS};
    // A number outside its range is refused, never drawn at the nearest one unsaid.
    let within = |k: &str, default: f64, (least, most): (f64, f64)| match p.get(k) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().filter(|x| (least..=most).contains(x)).ok_or_else(|| format!("{k} must be a number from {least} to {most}")),
    };
    let frames = within("frames", 60.0, (1.0, MAX_FRAMES as f64))?;
    if frames.fract() != 0.0 {
        return Err("frames must be a whole number".into());
    }
    let turn = within("turn", 360.0, (-MAX_TURN, MAX_TURN))?;
    if turn == 0.0 {
        return Err("turn must not be 0: it is the degrees the view turns by".into());
    }
    let (w, h) = (within("width", 640.0, (64.0, 4096.0))?, within("height", 400.0, (64.0, 4096.0))?);
    let dpi = within("dpi", 96.0, (48.0, 600.0))?;
    let speed = within("speed", 50.0, (16.0, 60_000.0))? as u32;
    // Rendering blocks while it runs, and every frame is held until the file is written: a
    // bound on the work, as time.export's.
    let pixels = frames * w * h * (dpi / 96.0).powi(2);
    if pixels > MAX_PIXELS {
        return Err(format!(
            "{frames} frames at {w} × {h} points and {dpi} dpi is {:.0} million pixels, more than the {:.0} million an orbit draws: lower the frames, the size or the dpi",
            pixels / 1e6,
            MAX_PIXELS / 1e6
        ));
    }
    Ok(Orbit { frames, turn, w, h, dpi, speed })
}

/// Turns the view about its centre, keeping its scale and pitch, and draws a frame a step as an
/// animated GIF or a zip of PNGs (the writer `time.export` uses). The frames are drawn from a
/// copy of the map, so the view never moves: the app draws them on a thread of its own, with a
/// frame a step of progress and Cancel between frames (`exports::IN_BACKGROUND`), and a script
/// where it is called (`exports::write_here`). As with Export Map's copy, what a raster read on
/// the copy left out is said by the session's own landing check (`notice_landed_rasters`, after
/// each command and on each frame of the app), not frame by frame here.
pub(crate) fn orbit_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    // Files are written by the desktop app; the browser's go through its services.
    if cfg!(target_arch = "wasm32") {
        return Err("exporting an orbit needs the desktop app".into());
    }
    let path = ps(p, "path").ok_or("path is required (a .gif, or a .zip of PNG frames)")?.to_string();
    let lower = path.to_ascii_lowercase();
    let gif = lower.ends_with(".gif");
    if !gif && !lower.ends_with(".zip") {
        return Err("path must end in .gif (an animation) or .zip (PNG frames)".into());
    }
    let Orbit { frames, turn, w, h, dpi, speed } = orbit_params(p)?;
    let n = frames as usize;
    let (mut map, view_size) = (s.map().detached(), s.view_size);
    let start = map.camera.bearing;
    Ok(Box::new(move || {
        let mut shadows = Vec::new();
        let written = (|| {
            let (made, said) = rustgis_render::shadows_said(|| {
                super::time::animate(n, gif.then_some(speed), |i| {
                    map.camera.set_bearing(orbit_bearing(start, turn, i, n));
                    super::share::png_of(&mut map, view_size, Some(w), Some(h), Some(dpi))
                })
            });
            let bytes = made?;
            // What the frames said of their cast shadows, once for the export.
            shadows = super::share::shadow_warnings("The orbit", &said);
            rustgis_data::io::write_atomic(std::path::Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "frames": n, "turn": turn, "bytes": bytes.len()}))
        })();
        let warn = rustgis_data::diagnostics::Severity::Warning;
        let notes = if written.is_ok() { shadows.into_iter().map(|e| (warn, e)).collect() } else { Vec::new() };
        super::exports::drawn(std::slice::from_ref(&map), notes, written, path)
    }))
}

/// One first-person step: a tenth of the view, forward (north), right (east) or up (zooming out).
fn walk(s: &mut Session, p: &Value) -> CmdResult {
    let (fw, rt, up) = (
        f(p, "forward").unwrap_or(0.0).clamp(-100.0, 100.0),
        f(p, "right").unwrap_or(0.0).clamp(-100.0, 100.0),
        f(p, "up").unwrap_or(0.0).clamp(-20.0, 20.0),
    );
    let (w, h) = s.view_size;
    let m = s.map_mut();
    let step = w.min(h) * 0.1;
    if m.projection.is_globe() {
        rustgis_map::Globe::drag(&mut m.camera, -rt * step, fw * step, w, h);
    } else {
        m.camera.pan_px(-rt * step, fw * step);
    }
    if up != 0.0 {
        m.camera.zoom_at(0.8f64.powf(up), w / 2.0, h / 2.0, w, h);
    }
    let c = m.center_lonlat();
    Ok(json!({"lon": c.x, "lat": c.y}))
}

/// A block diagram's settings (#1914) from `map.terrain`'s `"block"` object: the shape read here,
/// the values checked by [`rustgis_map::view::TerrainBlock::check`] (as a saved project's are),
/// and the walls' layer as the water's is ([`polygon_layer`]).
fn terrain_block(s: &Session, b: &serde_json::Map<String, Value>) -> Result<rustgis_map::view::TerrainBlock, String> {
    if let Some(k) = b.keys().find(|k| !matches!(k.as_str(), "extent" | "depth" | "walls" | "shape" | "radius" | "water")) {
        return Err(format!("block has no '{k}' (extent, depth, walls, shape, radius, water)"));
    }
    let extent = match b.get("extent") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.eq_ignore_ascii_case("layer") => None,
        Some(Value::Array(a)) => {
            let e: Vec<f64> = a.iter().filter_map(Value::as_f64).filter(|v| v.is_finite()).collect();
            match e[..] {
                [x0, y0, x1, y1] if a.len() == 4 => Some([x0, y0, x1, y1]),
                _ => return Err("block's extent is [xmin, ymin, xmax, ymax], numbers with xmin < xmax and ymin < ymax".into()),
            }
        }
        Some(_) => return Err("block's extent is \"layer\" or [xmin, ymin, xmax, ymax]".into()),
    };
    let depth = match b.get("depth") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_f64().ok_or("block's depth is metres, 0 to 20000")?),
    };
    let color = |c: Option<&Value>| match c {
        None | Some(Value::Null) => Ok(rustgis_map::view::BLOCK_WALLS),
        Some(Value::String(c)) => rustgis_map::Rgba::from_hex(c).ok_or("block's walls is an opaque colour, #RRGGBB"),
        Some(_) => Err("block's walls is a colour, #RRGGBB, or {\"color\"?, \"layer\"?}"),
    };
    // A colour, or the colour and a polygon layer that colours the walls where it has a polygon.
    let (walls, walls_layer) = match b.get("walls") {
        Some(Value::Object(w)) => {
            if let Some(k) = w.keys().find(|k| !matches!(k.as_str(), "color" | "layer")) {
                return Err(format!("block's walls has no '{k}' (color, layer)"));
            }
            let layer = match w.get("layer") {
                None | Some(Value::Null) => None,
                Some(Value::String(n)) => Some(polygon_layer(s, n, "block's walls", "the units the walls show")?),
                Some(_) => return Err("block's walls' layer is a polygon layer's name".into()),
            };
            (color(w.get("color"))?, layer)
        }
        other => (color(other)?, None),
    };
    // The water in the walls' section (#2317): its depth, or the water layer's field holding it
    // (checked against the terrain's water once that is known, in `terrain`).
    let water = match b.get("water") {
        None | Some(Value::Null | Value::Bool(false)) => None,
        Some(Value::Object(w)) => {
            if let Some(k) = w.keys().find(|k| !matches!(k.as_str(), "depth" | "field")) {
                return Err(format!("block's water has no '{k}' (depth, field)"));
            }
            let depth = match w.get("depth") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_f64().ok_or("block's water depth is metres, 0 to 20000")?),
            };
            let field = match w.get("field") {
                None | Some(Value::Null) => None,
                Some(Value::String(f)) if !f.trim().is_empty() => Some(f.trim().to_string()),
                Some(_) => return Err("block's water field is a numeric field of the water layer".into()),
            };
            Some(rustgis_map::view::BlockWater { depth, field })
        }
        Some(_) => return Err("block's water is {\"depth\"?, \"field\"?}, or false".into()),
    };
    // The outline inside the box (#2318): the box itself, or the circle in it.
    let radius = match b.get("radius") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_f64().ok_or("block's radius is metres (map units)")?),
    };
    let shape = match b.get("shape") {
        None | Some(Value::Null) => rustgis_map::view::BlockShape::Box,
        Some(Value::String(s)) if s.eq_ignore_ascii_case("box") => rustgis_map::view::BlockShape::Box,
        Some(Value::String(s)) if s.eq_ignore_ascii_case("circle") => rustgis_map::view::BlockShape::Circle { radius },
        Some(Value::Object(o)) => block_polygons(s, o)?,
        Some(_) => return Err("block's shape is \"box\", \"circle\" or {\"layer\", \"where\"?}".into()),
    };
    // "extent": "layer" names the DEM's box, which a block cut to polygons doesn't take.
    if matches!(shape, rustgis_map::view::BlockShape::Polygon { .. }) && b.get("extent").is_some_and(|e| !e.is_null()) {
        return Err("a block cut to polygons takes their box: give no extent".into());
    }
    if radius.is_some() && !matches!(shape, rustgis_map::view::BlockShape::Circle { .. }) {
        return Err("block's radius is a circle's: give \"shape\": \"circle\" too".into());
    }
    let block = rustgis_map::view::TerrainBlock { extent, depth, walls, walls_layer, shape, water };
    block.check()?;
    Ok(block)
}

/// A block cut to polygons (#2318) from `"shape": {"layer", "where"?}`: a polygon layer as the
/// water's ([`polygon_layer`]) and a `where` that reads and picks a polygon among the rows its
/// definition query keeps.
fn block_polygons(s: &Session, o: &serde_json::Map<String, Value>) -> Result<rustgis_map::view::BlockShape, String> {
    if let Some(k) = o.keys().find(|k| !matches!(k.as_str(), "layer" | "where")) {
        return Err(format!("block's shape has no '{k}' (layer, where)"));
    }
    let name = o.get("layer").and_then(Value::as_str).ok_or("block's shape needs a layer: a polygon layer of the outline")?;
    let name = polygon_layer(s, name, "block's shape", "the outline the block is cut to")?;
    let filter = match o.get("where") {
        None | Some(Value::Null) => None,
        Some(Value::String(w)) if w.trim().is_empty() => None,
        Some(Value::String(w)) => Some(w.trim().to_string()),
        Some(_) => return Err("block's shape's where is an expression, as a definition query".into()),
    };
    let l = s.map().layer_by_name(&name).ok_or_else(|| format!("no layer '{name}'"))?;
    rustgis_data::rows::Query::compile(filter.as_deref(), &l.dataset).map_err(|e| format!("block's shape's where: {e}"))?;
    // The outline read here (one pass over the layer's rows and the picked rings' corners) is
    // kept for the frames that follow.
    if rustgis_render::terrain::Outline::of(s.map(), &name, filter.as_deref()).is_none() {
        return Err(match &filter {
            Some(w) => format!("block's shape: no polygon of '{name}' with an area matches {w}"),
            None => format!("block's shape: '{name}' has no polygon with an area to cut the block to"),
        });
    }
    Ok(rustgis_map::view::BlockShape::Polygon { layer: name, filter })
}

/// What simplifying a polygon block's outline lost: rings narrower than the spacing, and corners
/// past a third of the finest mesh cell or its smallest rings, to stay under 65 535 sides (said
/// when it is set and when a project opens).
pub(crate) fn outline_simplified(layer: &str, o: &rustgis_render::terrain::Outline) -> Option<String> {
    let (spacing, coarsened) = o.spacing();
    // Rings narrower than the spacing, with no area left once thinned (#2318).
    let narrow =
        (o.narrow() > 0).then(|| format!("{} ring(s) of '{layer}' narrower than {spacing:.0} m were left out of the block", o.narrow()));
    let left = match o.dropped() {
        0 if !coarsened => return narrow,
        0 => String::new(),
        n => format!(", and its {n} smallest ring(s) were left out"),
    };
    let said = format!(
        "the outline of '{layer}' the block is cut to has too many corners: corners up to {spacing:.0} m apart were merged (less in smaller rings){left}, to stay under 65535 sides"
    );
    Some(match narrow {
        Some(n) => format!("{said}; {n}"),
        None => said,
    })
}

/// The holes of a polygon block's polygons, which aren't cut: the ground inside them is drawn
/// (said when it is set and when a project opens).
pub(crate) fn outline_holes(layer: &str, o: &rustgis_render::terrain::Outline) -> Option<String> {
    let n = o.holes();
    (n > 0).then(|| format!("{n} hole(s) in the polygons of '{layer}' the block is cut to aren't cut: the ground inside them is drawn"))
}

/// The name of the polygon layer `name` for `what` (water, a block's walls), checked: a polygon
/// layer (`holds` says of what) holding all its polygons, of a name of its own.
fn polygon_layer(s: &Session, name: &str, what: &str, holds: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("{what} needs a layer: a polygon layer of {holds}"));
    }
    let l = s.map().layer_by_name(name).ok_or_else(|| format!("no layer '{name}'"))?;
    if l.geometry_type() != rustgis_data::GeometryType::Polygon {
        return Err(format!("'{name}' isn't a polygon layer ({holds})"));
    }
    // A layer read in chunks holds only the view's: its polygons would come and go with the view.
    if l.is_streamed() {
        return Err(format!("'{name}' is read a view at a time: {what} needs a layer that holds all its polygons"));
    }
    if s.map().layers.iter().filter(|o| o.name.eq_ignore_ascii_case(name)).count() > 1 {
        return Err(format!("more than one layer is called '{name}': give the {what} layer a name of its own, then try again"));
    }
    Ok(l.name.clone())
}

/// A terrain's water (#1915) from `map.terrain`'s `"water"` object, checked: a polygon layer of a
/// name of its own, a numeric field of it, and a glint and a shore band from 0 to 1.
fn terrain_water(s: &Session, w: &serde_json::Map<String, Value>) -> Result<rustgis_map::view::TerrainWater, String> {
    if let Some(k) = w.keys().find(|k| !matches!(k.as_str(), "layer" | "level" | "glint" | "shore")) {
        return Err(format!("water has no '{k}' (layer, level, glint, shore)"));
    }
    let name = w.get("layer").and_then(Value::as_str).ok_or("water needs a layer: a polygon layer of water bodies")?;
    let name = polygon_layer(s, name, "water", "water bodies")?;
    let l = s.map().layer_by_name(&name).ok_or_else(|| format!("no layer '{name}'"))?;
    let level = match w.get("level") {
        None | Some(Value::Null) => None,
        Some(Value::String(f)) => {
            let i = l.dataset.field_index(f).ok_or_else(|| format!("'{name}' has no field '{f}'"))?;
            let field = &l.dataset.fields[i];
            if !field.ty.is_numeric() {
                return Err(format!("water's level field '{}' isn't numeric (levels in metres)", field.name));
            }
            Some(field.name.clone())
        }
        Some(_) => return Err("water's level is a field name".into()),
    };
    let glint = match w.get("glint") {
        None | Some(Value::Null) => rustgis_map::view::WATER_GLINT,
        Some(v) => v.as_f64().filter(|g| (0.0..=1.0).contains(g)).ok_or("water's glint is a number from 0 to 1")?,
    };
    let shore = match w.get("shore") {
        None | Some(Value::Null) => rustgis_map::view::WATER_SHORE,
        Some(v) => v.as_f64().filter(|d| (0.0..=1.0).contains(d)).ok_or("water's shore is a number from 0 to 1")?,
    };
    Ok(rustgis_map::view::TerrainWater { layer: l.name.clone(), level, glint, shore })
}

/// Sets the map's terrain: the relief a tilted view drapes the map over, from a DEM raster layer
/// or elevation tiles, or turns it off. Parameters are checked before anything changes.
///
/// Runs on the UI thread: it changes the map's view settings, which the frames read. With water
/// (#1915) it reads the water's shores over a DEM layer for its notices, bounded by the render
/// crate's `MAX_SHORE_SAMPLES` heights, with no progress or Cancel; the read is kept, so the
/// frames that follow draw from it instead of reading the shores again. Over elevation tiles it
/// reads no height (the coarse count comes from the shapes). With a block cut to polygons
/// (#2318) it reads them once, as the water's shores: one pass over the layer's rows and each
/// picked ring's corners once, kept (`Outline::of`) for the frames that follow, which read it
/// again only after the layer is edited.
fn terrain(s: &mut Session, p: &Value) -> CmdResult {
    use rustgis_map::view::{MAX_EXAGGERATION, Terrain, TerrainEncoding, TerrainSource};
    if p.get("off").and_then(Value::as_bool) == Some(true) {
        s.map_mut().view.terrain = None;
        return Ok(json!({"terrain": null}));
    }
    // Left out, the block stays as it was; false or null takes it off.
    let block = match p.get("block") {
        None => None,
        Some(Value::Null | Value::Bool(false)) => Some(None),
        Some(Value::Bool(true)) => Some(Some(terrain_block(s, &serde_json::Map::new())?)),
        Some(Value::Object(b)) => Some(Some(terrain_block(s, b)?)),
        Some(_) => return Err("block is an object (extent, depth, walls, shape, radius, water), or false to take it off".into()),
    };
    // Left out (or null, as for the exaggeration), the water stays as it was; false takes it off
    // (#1915).
    let water = match p.get("water") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(false)) => Some(None),
        Some(Value::Object(w)) => Some(Some(terrain_water(s, w)?)),
        Some(_) => return Err("water is an object (layer, level, glint, shore), or false to take it off".into()),
    };
    let exaggeration = match p.get("exaggeration").filter(|v| !v.is_null()) {
        Some(_) => {
            Some(f(p, "exaggeration").filter(|v| v.is_finite()).ok_or("exaggeration must be a number")?.clamp(0.0, MAX_EXAGGERATION))
        }
        None => None,
    };
    let source = match (ps(p, "layer"), ps(p, "tiles")) {
        (Some(_), Some(_)) => return Err("give a layer or tiles, not both".into()),
        (Some(name), None) => {
            let l = s.map().layer_by_name(name).ok_or_else(|| format!("no layer '{name}'"))?;
            if l.raster.is_none() {
                return Err(format!("'{name}' isn't a raster layer (a DEM of heights in metres)"));
            }
            Some(TerrainSource::Layer(l.name.clone()))
        }
        (None, Some(url)) => {
            let url = url.trim();
            let lower = url.to_ascii_lowercase();
            if !(lower.starts_with("https://") || lower.starts_with("http://")) {
                return Err("tiles is a web address (http or https)".into());
            }
            if !(url.contains("{z}") && url.contains("{x}") && (url.contains("{y}") || url.contains("{-y}"))) {
                return Err("tiles is an XYZ URL with {z}, {x} and {y}".into());
            }
            let encoding = match ps(p, "encoding") {
                Some(e) => TerrainEncoding::from_code(e).ok_or_else(|| format!("unknown encoding '{e}' (terrarium or mapbox)"))?,
                None => TerrainEncoding::Terrarium,
            };
            Some(TerrainSource::Tiles { url: url.to_string(), encoding })
        }
        (None, None) => None,
    };
    // Nothing named and no terrain yet (Terrain from the ribbon or command search): the active
    // layer, when it is a raster, is the DEM.
    let was_off = s.map().view.terrain.is_none();
    let source = source.or_else(|| {
        if !was_off {
            return None;
        }
        let l = s.active_layer.and_then(|id| s.map().layer(id)).filter(|l| l.raster.is_some())?;
        Some(TerrainSource::Layer(l.name.clone()))
    });
    // Relief shows on a tilted Web Mercator map only (the globe and other projections stay flat).
    let mercator = matches!(s.map().projection, rustgis_map::Projection::WebMercator);
    let flat = s.map().camera.pitch.abs() < 0.5;
    let m = s.map_mut();
    let old_terrain = m.view.terrain.clone();
    // Whether the old block is cut off below: said once the terrain is set, not when a refusal keeps it.
    let mut block_cut = false;
    let t = match (source, m.view.terrain.take()) {
        (Some(source), old) => {
            let (old_exaggeration, old_block, old_water) = old.map_or((None, None, None), |t| (Some(t.exaggeration), t.block, t.water));
            // A block cut to the old DEM's extent doesn't carry over to elevation tiles, which
            // have none (the result shows it gone); one with its own box does.
            block_cut =
                block.is_none() && old_block.as_ref().is_some_and(|b| !b.own_box()) && matches!(source, TerrainSource::Tiles { .. });
            let old_block = old_block.filter(|b| b.own_box() || matches!(source, TerrainSource::Layer(_)));
            Terrain {
                source,
                exaggeration: exaggeration.or(old_exaggeration).unwrap_or(1.0),
                block: block.clone().unwrap_or(old_block),
                water: water.clone().unwrap_or(old_water),
            }
        }
        // Only a new exaggeration, block or water: the terrain keeps its source.
        (None, Some(old)) => Terrain {
            exaggeration: exaggeration.unwrap_or(old.exaggeration),
            block: block.clone().unwrap_or_else(|| old.block.clone()),
            water: water.clone().unwrap_or_else(|| old.water.clone()),
            ..old
        },
        (None, None) => {
            return Err(
                "make a DEM raster layer the active layer first (a script can instead name a layer, or give elevation tiles)".into()
            );
        }
    };
    let mut out = json!({
        "source": match &t.source {
            TerrainSource::Layer(n) => json!({"layer": n}),
            TerrainSource::Tiles { url, encoding } => json!({"tiles": crate::sources::redact_output(url), "encoding": encoding.code()}),
        },
        "exaggeration": t.exaggeration,
        "block": null,
        "water": t.water.as_ref().map(|w| json!({"layer": w.layer, "level": w.level, "glint": w.glint, "shore": w.shore})),
    });
    // Over elevation tiles there is no layer's extent to cut the block to.
    if matches!(t.source, TerrainSource::Tiles { .. }) && t.block.as_ref().is_some_and(|b| !b.own_box()) {
        if let Some(old) = old_terrain {
            m.view.terrain = Some(old);
        }
        return Err("a block over elevation tiles needs an extent box [xmin, ymin, xmax, ymax] (Terrain stays as it was)".into());
    }
    // The DEM's own box, where a block on its extent is cut, and its footprint (the outer bounds
    // of its corners), which a box given by the user is refused only outside of.
    let dem_box = match (&t.source, &t.block) {
        (TerrainSource::Layer(n), Some(_)) => m
            .layer_by_name(n)
            .and_then(|l| l.raster.as_ref())
            .and_then(|r| r.data.clone())
            .map(|r| (n.clone(), rustgis_render::terrain::raster_box(&r), rustgis_render::terrain::raster_footprint(&r))),
        _ => None,
    };
    // A block this call leaves out is the one the terrain had: the refusal says so, and how to take it off.
    let kept = if block.is_none() { "; the block is the one kept from before, and \"block\": false takes it off" } else { "" };
    // A block cut to polygons (#2318) is checked on the box round them, refused when they can't
    // be read (a block kept from before whose layer changed).
    let own = t.block.as_ref().and_then(|b| super::terrain_blocks::own_box(m, b));
    let outline = match t.block.as_ref().map(|b| &b.shape) {
        Some(rustgis_map::view::BlockShape::Polygon { layer, filter }) => {
            let Some(o) = rustgis_render::terrain::Outline::of(m, layer, filter.as_deref()) else {
                let e =
                    format!("the block's polygons in '{layer}' can't be read: none with an area matches{kept} (Terrain stays as it was)");
                if let Some(old) = old_terrain {
                    m.view.terrain = Some(old);
                }
                return Err(e);
            };
            Some((layer.clone(), o))
        }
        _ => None,
    };
    let refused = match (&dem_box, own) {
        // A DEM whose corners give no box (one off the map's world, say) would draw no block.
        (Some((n, None, _)), None) => Some(format!(
            "no box fits inside '{n}'s extent on the map: give the block an extent [xmin, ymin, xmax, ymax]{kept} (Terrain stays as it was)"
        )),
        // A box off the DEM would draw nothing. A box inside its footprint but off its inner box
        // still draws the ground it reaches (a DEM turned by its projection has corners beyond it).
        (Some((n, _, Some(d))), Some(e)) if e[2] <= d[0] || e[0] >= d[2] || e[3] <= d[1] || e[1] >= d[3] => Some(format!(
            "block's extent doesn't overlap '{n}' (its corners are [{:.0}, {:.0}, {:.0}, {:.0}] in Web Mercator metres{kept}; Terrain stays as it was)",
            d[0], d[1], d[2], d[3]
        )),
        _ => None,
    };
    if let Some(e) = refused {
        if let Some(old) = old_terrain {
            m.view.terrain = Some(old);
        }
        return Err(e);
    }
    // Data may arrive after this command. Loaded DEMs can be checked now; deferred ones are
    // checked by notice_landed_rasters before their next frame, or after a successful command.
    if let (TerrainSource::Layer(name), Some(block)) = (&t.source, &t.block)
        && let Some(raster) = m.layer_by_name(name).and_then(|layer| layer.raster.as_ref()).and_then(|raster| raster.data.as_ref())
        && let Err(reason) = super::terrain_blocks::check(&crate::project::short_name(name), raster, block.extent.or(own))
    {
        if let Some(old) = old_terrain {
            m.view.terrain = Some(old);
        }
        return Err(format!("{reason}{kept} (Terrain stays as it was)"));
    }
    // The box the block is cut to, as numbers: the DEM's (an approximation for one turned by its
    // projection) when none was given.
    out["block"] = t.block.as_ref().map_or(Value::Null, |b| {
        let (extent, from) = match (b.extent.or(own), dem_box.as_ref().and_then(|d| d.1)) {
            (Some(e), _) if b.extent.is_some() => (json!(e), "box"),
            (Some(e), _) => (json!(e), "shape"),
            (None, Some(d)) => (json!(d), "layer"),
            (None, None) => (json!("layer"), "layer"),
        };
        let walls = match &b.walls_layer {
            Some(l) => json!({"color": b.walls.hex(), "layer": l}),
            None => json!(b.walls.hex()),
        };
        // A circle's radius as drawn: the largest in the box when none is given or a larger one is
        // (`radius_capped`), when the box is known.
        let largest = b.extent.or(dem_box.as_ref().and_then(|d| d.1)).map(|[x0, y0, x1, y1]| (x1 - x0).min(y1 - y0) / 2.0);
        let (shape, radius, capped) = match (&b.shape, largest) {
            (rustgis_map::view::BlockShape::Box, _) => (json!("box"), None, None),
            (rustgis_map::view::BlockShape::Circle { radius: Some(r) }, Some(l)) => (json!("circle"), Some(r.min(l)), Some(*r > l)),
            (rustgis_map::view::BlockShape::Circle { radius }, l) => (json!("circle"), radius.or(l), Some(false)),
            (rustgis_map::view::BlockShape::Polygon { layer, filter }, _) => (json!({"layer": layer, "where": filter}), None, None),
        };
        json!({"extent": extent, "extent_from": from, "depth": b.depth, "walls": walls, "shape": shape, "radius": radius, "radius_capped": capped, "water": b.water})
    });
    // A polygon block's outline as drawn: its sides and how near its corners were let lie.
    if let Some((_, o)) = &outline {
        let (spacing, coarsened) = o.spacing();
        out["block"]["outline"] = json!({"sides": o.side_count(), "spacing": spacing, "coarsened": coarsened, "dropped": o.dropped(), "holes": o.holes(), "narrow": o.narrow()});
    }
    if matches!(t.source, TerrainSource::Layer(_)) && t.block.is_some() && dem_box.is_none() {
        out["block"]["pendingDem"] = json!(true);
    }
    // The terrain keeps its DEM by name: with two layers of that name (a rename can make them),
    // it would draw the first one's heights. Checked for a terrain kept on too.
    if let TerrainSource::Layer(n) = &t.source
        && m.layers.iter().filter(|l| l.name.eq_ignore_ascii_case(n)).count() > 1
    {
        let e = format!(
            "more than one layer is called '{n}': give the DEM a name of its own, then try again naming it (Terrain stays as it was)"
        );
        // A terrain that was on stays as it was.
        if let Some(old) = old_terrain {
            m.view.terrain = Some(old);
        }
        return Err(e);
    }
    // So does its water (kept through other changes, a rename can give its name to another).
    if let Some(w) = &t.water
        && m.layers.iter().filter(|l| l.name.eq_ignore_ascii_case(&w.layer)).count() > 1
    {
        let e = format!(
            "more than one layer is called '{}': give the water a name of its own, then try again (Terrain stays as it was)",
            w.layer
        );
        if let Some(old) = old_terrain {
            m.view.terrain = Some(old);
        }
        return Err(e);
    }
    // And the walls' layer (#1914), kept by name as the water's is.
    if let Some(l) = t.block.as_ref().and_then(|b| b.walls_layer.as_ref())
        && m.layers.iter().filter(|o| o.name.eq_ignore_ascii_case(l)).count() > 1
    {
        let e = format!(
            "more than one layer is called '{l}': give the block's walls layer a name of its own, then try again (Terrain stays as it was)"
        );
        if let Some(old) = old_terrain {
            m.view.terrain = Some(old);
        }
        return Err(e);
    }
    // The water a block's walls cut (#2317), when this call sets the block or the terrain's water
    // (a block kept from before is checked against new water too): the terrain's water, and the
    // depth field a numeric field of its layer. How many of the polygons its definition query
    // keeps have no depth is said.
    let mut no_depth = None;
    let block_water = t.block.as_ref().and_then(|b| b.water.as_ref()).filter(|_| block.is_some() || water.is_some());
    if let Some(bw) = block_water {
        let checked = (|| {
            let w = t.water.as_ref().ok_or(
                "block's water shows the terrain's water in the walls: give the terrain a water layer, or take the block's water off by giving the block again without it (a \"block\" object sets the whole block: its extent, depth and walls too)",
            )?;
            let l = m.layer_by_name(&w.layer).ok_or_else(|| format!("no layer '{}'", w.layer))?;
            let Some(f) = &bw.field else { return Ok(None) };
            let i = l.dataset.field_index(f).ok_or_else(|| format!("'{}' has no field '{f}' (the block's water depths)", l.name))?;
            if !l.dataset.fields.get(i).is_some_and(|field| field.ty.is_numeric()) {
                return Err(format!("block's water field '{f}' isn't numeric (depths in metres)"));
            }
            let keep = l.query_rows(&l.dataset);
            let missing = |(row, v): &(usize, &rustgis_data::Value)| {
                keep.as_ref().is_none_or(|k| k.get(*row).copied().unwrap_or(false)) && v.as_f64().is_none_or(|d| !d.is_finite() || d <= 0.0)
            };
            let n = if bw.depth.is_some() { 0 } else { l.dataset.features.column(i).enumerate().filter(missing).count() };
            Ok((n > 0).then(|| (n, l.name.clone(), f.clone())))
        })();
        match checked {
            Ok(n) => no_depth = n,
            Err(e) => {
                if let Some(old) = old_terrain {
                    m.view.terrain = Some(old);
                }
                return Err(format!("{e} (Terrain stays as it was)"));
            }
        }
    }
    let heights_from = match &t.source {
        TerrainSource::Layer(n) => Some(n.clone()),
        TerrainSource::Tiles { .. } => None,
    };
    let water_layer = t.water.as_ref().filter(|_| water.is_some()).map(|w| w.layer.clone());
    m.view.terrain = Some(t);
    // A polygon block's outline simplified past a third of the finest mesh cell, or short of its
    // smallest rings, to stay under 65 535 sides: said, with how far.
    for said in outline.iter().flat_map(|(layer, o)| [outline_simplified(layer, o), outline_holes(layer, o)]).flatten() {
        s.notify(format!("Terrain: {said}"), false);
    }
    if block_cut {
        s.notify(
            "Terrain's block was cut to the DEM's extent and is off with the switch to elevation tiles: give it an extent to keep it"
                .to_string(),
            false,
        );
    }
    if let Some((n, layer, field)) = no_depth {
        let (polygons, have) = if n == 1 { ("1 polygon".to_string(), "has") } else { (format!("{n} polygons"), "have") };
        s.notify(format!("Block: {polygons} of '{layer}' {have} no depth in '{field}', so a wall crossing them shows no water"), false);
    }
    // When the water is set, what its levels left out is said, with how many.
    if let Some(name) = water_layer {
        use rustgis_render::terrain::Ground;
        // Water polygons with no height along their shore (off the DEM) and no level stay on the
        // ground. Over elevation tiles a shore whose tiles haven't come yet isn't off the
        // ground: not said there, nor are tiles asked for here.
        let over_dem = || Ground::of(s.map(), &s.map().camera).map_or(0, |g| g.water_unlevelled());
        let unlevelled = match if heights_from.is_some() { over_dem() } else { 0 } {
            0 => None,
            1 => Some(format!(
                "Water: 1 polygon of '{name}' has no height along its shore (off the DEM) and no level, so stays on the ground"
            )),
            n => Some(format!(
                "Water: {n} polygons of '{name}' have no height along their shore (off the DEM) and no level, so stay on the ground"
            )),
        };
        // When the layer's shores together are too long to read every half cell, all of them are
        // read at one wider step: a lowest point may be missed, leaving the water a little high.
        // Said over a DEM and over tiles alike (the step follows the shores' length, not the
        // heights read).
        let coarse = match Ground::water_coarse_unread(s.map(), &s.map().camera) {
            0 => None,
            1 => Some(format!(
                "Water: the shore of 1 polygon of '{name}' was read at wider steps than half a cell of the heights (shores too long to read whole), so its level may lie a little above its lowest shore"
            )),
            n => Some(format!(
                "Water: the shores of {n} polygons of '{name}' were read at wider steps than half a cell of the heights (shores too long to read whole), so their levels may lie a little above their lowest shore"
            )),
        };
        for said in [unlevelled, coarse].into_iter().flatten() {
            s.notify(said, false);
        }
    }
    // Turned on: which layer gives the heights (any raster can be the active layer, an RGB image
    // too), and what to do to see the relief.
    if was_off {
        let mut said = Vec::new();
        if let Some(n) = heights_from {
            said.push(format!("heights from '{n}' (its first band, in metres)"));
        }
        if !mercator {
            said.push("relief shows on a tilted Web Mercator map, not on this projection".to_string());
        } else if flat {
            said.push("tilt the map (drag with the right button, or Shift+Up) to see the relief".to_string());
        }
        if !said.is_empty() {
            s.notify(format!("Terrain is on: {}", said.join("; ")), false);
        }
    }
    Ok(out)
}
