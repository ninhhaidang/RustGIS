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
        C::new("map.effects", "View Effects", "View › Effects", effects).params(
            r#"{"sunTime"?: seconds since 1970|"now"|null (illumination), "weather"?: "none"|"haze"|"fog"|"rain"|"snow", "vignette"?, "edges"?, "stars"?: bool (visual effects; stars around the globe), "tiltShift"?, "grain"?: bool (camera effects), "brightness"?, "contrast"?, "saturation"?: -100..100, "sepia"?: 0..100 (post-processing), "reset"?: bool}"#,
        ),
        C::new("map.colorVision", "Color Vision Simulator", "View › Accessibility", |s, p| {
            let m = ps(p, "mode").ok_or("mode is required")?;
            let cv = ColorVision::from_code(m).ok_or_else(|| format!("unknown colour vision '{m}'"))?;
            s.map_mut().view.color_vision = cv;
            Ok(json!({"mode": cv.code()}))
        })
        .view()
        .params(r#"{"mode": "normal"|"protanopia"|"deuteranopia"|"tritanopia"|"achromatopsia"}"#),
        C::new("layer.priority", "Depth Priority", "View › Scene", |s, p| {
            let v = p.get("value").and_then(Value::as_i64).ok_or("value is required (higher draws on top)")?.clamp(-1000, 1000) as i32;
            with(s, p, |l| l.priority = v)
        })
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
        })
        .params(r#"{"mode": "standard"|"wireframe"}"#),
        C::new("map.clipping", "View Clipping", "View › View Clipping", |s, p| {
            let d = match ps(p, "mode").unwrap_or("distance") {
                "none" => None,
                "distance" => Some(f(p, "distance").filter(|d| *d > 0.0).ok_or("distance is required (metres)")?.min(4e7)),
                m => return Err(format!("unknown clipping mode '{m}'")),
            };
            s.map_mut().view.clip_distance = d;
            Ok(json!({"distance": d}))
        })
        .params(r#"{"mode": "none"|"distance", "distance"?: metres from the view's centre}"#),
        C::new("map.terrain", "Terrain", "View › Scene", terrain).params(
            r#"{"layer"?: a DEM raster layer (heights in metres; without layer or tiles, and no terrain yet, the active layer when it is a raster) | "tiles"?: elevation tile URL with {z}/{x}/{y}, "encoding"?: "terrarium"|"mapbox" (tiles; default terrarium), "exaggeration"?: 0 to 20 (default 1, or the terrain's), "off"?: true}"#,
        ),
        C::new("map.displayType", "Display Type", "View › Options", |s, p| {
            let fast = match ps(p, "mode").ok_or("mode is required")? {
                "quality" => false,
                "speed" => true,
                m => return Err(format!("unknown display type '{m}'")),
            };
            s.map_mut().view.fast = fast;
            Ok(json!({"mode": if fast { "speed" } else { "quality" }}))
        })
        .view()
        .params(r#"{"mode": "quality"|"speed"}"#),
        C::new("map.navigator", "Navigator", "View › Navigation", |s, p| {
            let v = &mut s.map_mut().view;
            v.navigator = b(p, "on").unwrap_or(!v.navigator);
            Ok(json!({"navigator": v.navigator}))
        })
        .view()
        .params(r#"{"on"?: bool}"#),
        C::new("map.camera", "Camera", "View › Navigation", camera).view().params(r#"{"lon"?, "lat"?, "scale"? (the camera's position when omitted)}"#),
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
        })
        .params(r#"{"mode": "planar"|"firstPerson"}"#),
        C::new("map.viewMode", "View", "View › View", |s, p| {
            let next = match ps(p, "mode").ok_or("mode is required")? {
                "global" => rustgis_map::Projection::Globe,
                "local" => {
                    if !rustgis_map::projection::local_views_supported() {
                        return Err("local views aren't available in the web app".to_string());
                    }
                    let c = s.map().center_lonlat();
                    rustgis_map::Projection::from_code(&format!("local:{:.4},{:.4}", c.x, c.y))?
                }
                m => return Err(format!("unknown view '{m}'")),
            };
            let m = s.map_mut();
            m.set_projection(next);
            Ok(json!({"projection": m.projection.code(), "label": m.projection.label()}))
        })
        .params(r#"{"mode": "global" (a globe)|"local" (a flat view centred here; not in the web app)}"#),
        C::new("map.walk", "Walk", "Home › Navigate", walk).view().params(r#"{"forward"?: steps, "right"?: steps, "up"?: steps (first-person navigation)}"#),
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

/// Sets the map's terrain: the relief a tilted view drapes the map over, from a DEM raster layer
/// or elevation tiles, or turns it off. Parameters are checked before anything changes.
fn terrain(s: &mut Session, p: &Value) -> CmdResult {
    use rustgis_map::view::{MAX_EXAGGERATION, Terrain, TerrainEncoding, TerrainSource};
    if p.get("off").and_then(Value::as_bool) == Some(true) {
        s.map_mut().view.terrain = None;
        return Ok(json!({"terrain": null}));
    }
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
    let t = match (source, m.view.terrain.take()) {
        (Some(source), old) => Terrain { source, exaggeration: exaggeration.or(old.map(|t| t.exaggeration)).unwrap_or(1.0) },
        // Only a new exaggeration: the terrain keeps its source.
        (None, Some(old)) => Terrain { exaggeration: exaggeration.unwrap_or(old.exaggeration), ..old },
        (None, None) => {
            return Err(
                "make a DEM raster layer the active layer first (a script can instead name a layer, or give elevation tiles)".into()
            );
        }
    };
    let out = json!({
        "source": match &t.source {
            TerrainSource::Layer(n) => json!({"layer": n}),
            TerrainSource::Tiles { url, encoding } => json!({"tiles": url, "encoding": encoding.code()}),
        },
        "exaggeration": t.exaggeration,
    });
    // The terrain keeps its DEM by name: with two layers of that name (a rename can make them),
    // it would draw the first one's heights. Checked for a terrain kept on too.
    if let TerrainSource::Layer(n) = &t.source
        && m.layers.iter().filter(|l| l.name.eq_ignore_ascii_case(n)).count() > 1
    {
        let e = format!("two layers are called '{n}': rename one, then turn Terrain on");
        // A terrain that was on stays as it was.
        if let Some(old) = old_terrain {
            m.view.terrain = Some(old);
        }
        return Err(e);
    }
    let heights_from = match &t.source {
        TerrainSource::Layer(n) => Some(n.clone()),
        TerrainSource::Tiles { .. } => None,
    };
    m.view.terrain = Some(t);
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
