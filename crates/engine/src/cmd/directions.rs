//! Directions: a route between places, through a Valhalla routing server (Home › Navigate ›
//! Directions in the app, which plans and replays routes in its Directions pane).

use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Attr};
use rustgis_geom::{Coord, Extent};
use rustgis_map::{Rgba, symbology::Renderer};
use serde_json::{Value, json};

use super::{b, ps};
use crate::render::rustgis_tiles::routing::{self, Avoid, Mode, Route, Router};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![C::new("map.route", "Directions", "Home › Navigate", route).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
        r#"{"from": [lon, lat] | "a place, an address or 'lat, lon'", "to": the same, "via"?: [places] (stops on the way), "mode"?: "drive" (default) | "bike" | "walk", "avoid"?: ["tolls", "highways", "ferries"] (driving), "imperial"?: bool (miles in the instructions), "language"?: "en-US" | "de" | …, "url"?: "a Valhalla server of your own", "addLayer"?: bool (adds the route and its steps as layers, default true), "zoom"?: bool (default true), "answer"?: "a routing answer already fetched (the app's; nothing is fetched)", "route"?: index of the route in answer}"#,
    )]
}

/// The most places one call looks up by name.
const MAX_NAMES: usize = 5;

/// A place a command names: `[lon, lat]`, `{lon, lat}`, `"lat, lon"`, or a name to look up;
/// `looked` counts the names looked up so far, spaced as the public geocoder asks.
fn place(s: &Session, v: Option<&Value>, what: &str, looked: &mut usize) -> Result<Coord, String> {
    let bad = || format!("{what} is [lon, lat], 'lat, lon', or a place's name");
    let c = match v {
        Some(Value::Array(a)) => {
            Coord::new(a.first().and_then(Value::as_f64).unwrap_or(f64::NAN), a.get(1).and_then(Value::as_f64).unwrap_or(f64::NAN))
        }
        Some(Value::Object(o)) => {
            Coord::new(o.get("lon").and_then(Value::as_f64).unwrap_or(f64::NAN), o.get("lat").and_then(Value::as_f64).unwrap_or(f64::NAN))
        }
        Some(Value::String(t)) if !t.trim().is_empty() => match routing::parse_latlon(t) {
            Some(c) => c,
            None => {
                *looked += 1;
                return look_up(s, t, *looked > 1);
            }
        },
        _ => return Err(bad()),
    };
    if c.is_finite() && c.x.abs() <= 180.0 && c.y.abs() <= 90.0 { Ok(c) } else { Err(bad()) }
}

/// The first place a name finds; `wait`s first when a name was just looked up (the public
/// geocoder allows a request a second).
fn look_up(_s: &Session, name: &str, wait: bool) -> Result<Coord, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = wait;
        Err(format!("in the web app, give '{name}' as [lon, lat], or plan the route in the Directions pane"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let g = crate::render::rustgis_tiles::geocode::Geocoder::new(None, None)?;
        if wait {
            std::thread::sleep(std::time::Duration::from_millis(g.spacing_ms()));
        }
        let found = crate::render::rustgis_tiles::geocode::search_blocking(&g, name, 1)?;
        found.places.first().map(|p| Coord::new(p.lon, p.lat)).ok_or_else(|| format!("no place found for '{name}'"))
    }
}

/// The roads to avoid a command names.
fn avoid(p: &Value) -> Result<Avoid, String> {
    let mut a = Avoid::default();
    let names: Vec<&str> = match p.get("avoid") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => s.split(',').map(str::trim).filter(|s| !s.is_empty()).collect(),
        Some(Value::Array(v)) => v.iter().filter_map(Value::as_str).collect(),
        _ => return Err("avoid is a list: tolls, highways, ferries".into()),
    };
    for n in names {
        match n.to_ascii_lowercase().as_str() {
            "tolls" | "toll" => a.tolls = true,
            "highways" | "highway" | "motorways" => a.highways = true,
            "ferries" | "ferry" => a.ferries = true,
            other => return Err(format!("can't avoid '{other}' (tolls, highways or ferries)")),
        }
    }
    Ok(a)
}

fn route(s: &mut Session, p: &Value) -> CmdResult {
    let mode = Mode::parse(ps(p, "mode").unwrap_or("drive"))?;
    let router = Router::new(ps(p, "url"))?;
    let imperial = b(p, "imperial").unwrap_or(false);
    let routes = match ps(p, "answer") {
        Some(answer) => router.parse(answer.as_bytes())?,
        None => {
            // Each name waits its turn at the public geocoder, so a call looks up a few.
            let named = ["from", "to"]
                .iter()
                .filter_map(|k| p.get(*k))
                .chain(p.get("via").and_then(Value::as_array).into_iter().flatten())
                .filter(|v| v.as_str().is_some_and(|t| !t.trim().is_empty() && routing::parse_latlon(t).is_none()))
                .count();
            if named > MAX_NAMES {
                return Err(format!("give at most {MAX_NAMES} places by name; give the others as [lon, lat]"));
            }
            let mut looked = 0;
            let mut points = vec![place(s, p.get("from"), "from", &mut looked)?];
            if let Some(via) = p.get("via").filter(|v| !v.is_null()) {
                let via = via.as_array().ok_or("via is a list of places")?;
                if via.len() + 2 > routing::MAX_POINTS {
                    return Err(format!("a route passes through at most {} places", routing::MAX_POINTS));
                }
                for v in via {
                    points.push(place(s, Some(v), "each stop", &mut looked)?);
                }
            }
            points.push(place(s, p.get("to"), "to", &mut looked)?);
            let req = routing::Request {
                points,
                mode,
                avoid: avoid(p)?,
                alternates: false,
                imperial,
                language: ps(p, "language").unwrap_or("en-US").to_string(),
            };
            fetch(&router, &req)?
        }
    };
    let i = match p.get("route") {
        None | Some(Value::Null) => 0,
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or("route is a route's index in the answer")?,
    };
    let r = routes.get(i).ok_or_else(|| format!("the answer has {} route(s)", routes.len()))?;
    let mut out = json!({
        "distanceKm": r.distance_m / 1000.0,
        "durationMin": r.duration_s / 60.0,
        "distance": routing::format_distance(r.distance_m, imperial),
        "duration": routing::format_duration(r.duration_s),
        "summary": r.summary,
        "steps": r.steps.iter().take(200).map(|st| json!({
            "instruction": st.instruction, "type": st.kind, "modifier": st.modifier, "road": st.road,
            "distanceM": st.distance_m, "durationS": st.duration_s, "lon": st.location.x, "lat": st.location.y,
        })).collect::<Vec<_>>(),
    });
    if b(p, "addLayer").unwrap_or(true) {
        let (line, steps) = add_layers(s, r, mode, imperial);
        out["layer"] = json!(line);
        out["stepsLayer"] = json!(steps);
    }
    if b(p, "zoom").unwrap_or(true) {
        let [w, south, e, n] = r.extent();
        // A margin around the route.
        let (mx, my) = (((e - w) * 0.08).max(0.002), ((n - south) * 0.08).max(0.002));
        s.zoom_to(&Extent::new(w - mx, south - my, e + mx, n + my));
    }
    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch(router: &Router, req: &routing::Request) -> Result<Vec<Route>, String> {
    routing::route_blocking(router, req)
}

#[cfg(target_arch = "wasm32")]
fn fetch(_: &Router, _: &routing::Request) -> Result<Vec<Route>, String> {
    Err("in the web app, plan routes in the Directions pane (Home › Navigate › Directions)".into())
}

/// Adds a route as two layers: its line, and a point for each maneuver. Returns their ids.
pub fn add_layers(s: &mut Session, r: &Route, mode: Mode, imperial: bool) -> (u32, u32) {
    let mut line = Dataset::new(
        format!("Route ({})", routing::format_distance(r.distance_m, imperial)),
        GeometryType::Polyline,
        vec![
            Field::new("mode", FieldType::Text),
            Field::new("distance_km", FieldType::Double),
            Field::new("duration_min", FieldType::Double),
            Field::new("summary", FieldType::Text),
        ],
    );
    line.push(
        Some(Geometry::Line(vec![r.line.clone()])),
        vec![
            Attr::Text(mode.id().into()),
            Attr::Float(r.distance_m / 1000.0),
            Attr::Float(r.duration_s / 60.0),
            Attr::Text(r.summary.clone()),
        ],
    );
    let mut steps = Dataset::new(
        "Route steps",
        GeometryType::Point,
        vec![
            Field::new("step", FieldType::Integer),
            Field::new("type", FieldType::Text),
            Field::new("modifier", FieldType::Text),
            Field::new("instruction", FieldType::Text),
            Field::new("road", FieldType::Text),
            Field::new("distance_m", FieldType::Double),
            Field::new("duration_s", FieldType::Double),
        ],
    );
    for (i, st) in r.steps.iter().enumerate() {
        steps.push(
            Some(Geometry::Point(st.location)),
            vec![
                Attr::Int(i as i64 + 1),
                Attr::Text(st.kind.clone()),
                Attr::Text(st.modifier.clone()),
                Attr::Text(st.instruction.clone()),
                Attr::Text(st.road.clone()),
                Attr::Float(st.distance_m),
                Attr::Float(st.duration_s),
            ],
        );
    }
    let blue = Rgba::rgb(0x25, 0x63, 0xEB);
    let lid = super::layer::add_dataset(s, line);
    if let Some(l) = s.map_mut().layer_mut(lid)
        && let Renderer::Single(sym) = &mut l.renderer
    {
        sym.stroke = blue;
        sym.fill = blue;
        sym.stroke_width = 4.0;
    }
    let sid = super::layer::add_dataset(s, steps);
    if let Some(l) = s.map_mut().layer_mut(sid)
        && let Renderer::Single(sym) = &mut l.renderer
    {
        sym.fill = Rgba::rgb(255, 255, 255);
        sym.stroke = Rgba::rgb(0x1E, 0x3A, 0x8A);
        sym.stroke_width = 2.0;
        sym.size = 7.0;
    }
    (lid, sid)
}
