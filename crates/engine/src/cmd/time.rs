//! The time slider: binding layers to a date field, the slider's span, steps and window, moving
//! through it, and exporting its steps as frames.

use rustgis_data::time::{self, Unit};
use rustgis_map::time::{TimeBinding, TimeSlider, field_span, time_fields};
use serde_json::{Value, json};

use super::{b, f, need_feature_layer, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

/// The window presets: steps before and after the current one, and whether it is cumulative.
pub const SHOWS: [(&str, u32, u32, bool); 4] =
    [("step", 0, 1, false), ("wide", 1, 1, false), ("wider", 3, 3, false), ("cumulative", 0, 1, true)];

/// The most frames an export makes.
const MAX_FRAMES: usize = 2000;
/// The most pixels an export renders over all its frames (it blocks while it runs): 300 frames
/// at 1280 × 720, or 2000 at about 500 × 300.
const MAX_PIXELS: f64 = 3.0e8;

/// The map has a time slider.
fn need_slider(s: &Session) -> Option<&'static str> {
    s.map().time.is_none().then_some("turn the time slider on first (Time Slider, or time.bind)")
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("time.bind", "Bind to Time Slider", "View › Time", bind).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "field"?: date field (default: the one most of whose values read as dates), "start"?, "end"?: dates (default: the field's span, when the slider is new), "unit"?: "hour"|"day"|"month"|"year", "interval"?: number, "show"?: "step"|"wide"|"wider"|"cumulative"}"#),
        C::new("time.unbind", "Unbind from Time Slider", "View › Time", |s, p| {
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            let was = l.time.take().map(|t| t.field);
            super::zarr::follow_time(s);
            Ok(json!({"unbound": was}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r#"{"layer"?}"#),
        C::new("time.fields", "Time Fields", "View › Time", |s, p| {
            let id = s.layer_id(p)?;
            let l = s.map().layer(id).ok_or("no such layer")?;
            let fields: Vec<Value> = time_fields(&l.dataset).into_iter().map(|(n, share)| json!({"field": n, "share": share})).collect();
            Ok(json!({"fields": fields, "bound": l.time.as_ref().map(|t| t.field.clone())}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .when(need_feature_layer)
        .params(r#"{"layer"?}"#),
        C::new("time.set", "Time Slider Settings", "View › Time", set).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"start"?, "end"?: dates (text, a year or milliseconds since 1970), "date"?: the current step, "unit"?: "hour"|"day"|"month"|"year", "interval"?: steps of that many units, "dates"?: [dates] (irregular steps; [] for regular ones), "show"?: "step"|"wide"|"wider"|"cumulative", "before"?: steps before the current one the window takes in, "after"?: steps from the current one (at least 1), "cumulative"?: bool, "speed"?: milliseconds a step shows for, "loop"?: bool, "format"?: date format (YYYY, MM, MMM, DD, HH, mm), "off"?: bool (turns the slider off, every feature showing)}"#,
        ),
        C::new("time.goTo", "Go to Date", "View › Time", |s, p| {
            let m = s.map_mut();
            let sl = m.time.as_mut().ok_or("the time slider is off")?;
            // A step by its number (0 = the first; past the end, the last), or a date.
            let t = match uint(p, "step") {
                Some(i) => {
                    let steps = sl.steps();
                    steps.get(usize::try_from(i).unwrap_or(usize::MAX).min(steps.len().saturating_sub(1))).copied().unwrap_or(sl.start)
                }
                None => p.get("date").and_then(time::parse_input).ok_or("date is required (2015-06-01, a year, or milliseconds since 1970), or step")?,
            };
            sl.go_to(t);
            m.apply_time();
            super::zarr::follow_time(s);
            Ok(info(s))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .when(need_slider)
        .params(r#"{"date": date | "step": number (0 = the first)}"#),
        C::new("time.step", "Step Time", "View › Time", |s, p| {
            let by = f(p, "by").unwrap_or(1.0).clamp(-1e6, 1e6) as i64;
            let wrap = b(p, "wrap").unwrap_or(false);
            let m = s.map_mut();
            m.time.as_mut().ok_or("the time slider is off")?.step(by, wrap);
            m.apply_time();
            super::zarr::follow_time(s);
            Ok(info(s))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .when(need_slider)
        .params(r#"{"by"?: steps (default 1; negative goes back), "wrap"?: bool (past the end starts over)}"#),
        C::new("time.info", "Time Slider", "View › Time", |s, _| Ok(info(s))).roles(&[]).effects(crate::policy::Effects::NONE).view(),
        C::new("time.export", "Export Time Animation", "View › Time", export).inputs(export_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .when(need_slider)
            .params(r#"{"path": "*.gif" | "*.zip" (PNG frames), "width"?, "height"?: px, "speed"?: milliseconds a frame shows for (default: the slider's), "from"?, "to"?: dates (default: the whole span)}"#),
    ]
}

/// The slider's state: its span, steps, where it is and what it shows.
pub fn info(s: &Session) -> Value {
    let m = s.map();
    let bound: Vec<Value> = m
        .layers
        .iter()
        .filter_map(|l| {
            let t = l.time.as_ref()?;
            // The features in the window, from the rows worked out when it was applied (cheap for
            // every step while playing).
            let n = l.dataset.features.len();
            let in_window = t.keeps(&l.dataset).map_or(n, |k| k.iter().filter(|k| **k).count());
            Some(json!({"layer": l.name, "field": t.field, "shown": in_window, "features": n}))
        })
        .collect();
    let tiles: Vec<Value> = m.tiles.iter().filter(|t| t.time_url.is_some()).map(|t| json!({"name": t.name, "url": t.url})).collect();
    let Some(t) = &m.time else { return json!({"on": false, "layers": bound, "tiles": tiles}) };
    let steps = t.steps();
    let (from, to) = t.window();
    json!({
        "on": true,
        "start": t.label(t.start), "end": t.label(t.end), "date": t.label(t.current),
        "current": t.current, "index": t.index_in(&steps), "steps": steps.len(),
        "unit": t.unit.code(), "interval": t.interval, "before": t.before, "after": t.after, "cumulative": t.cumulative,
        "window": [if from.is_finite() { json!(t.label(from)) } else { Value::Null }, t.label(to)],
        "speed": t.speed_ms, "loop": t.looping, "format": t.date_format(),
        "layers": bound, "tiles": tiles,
    })
}

fn date(p: &Value, k: &str) -> Result<Option<f64>, String> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            time::parse_input(v).map(Some).ok_or_else(|| format!("{k} is not a date (2015-06-01, a year, or milliseconds since 1970)"))
        }
    }
}

fn unit(p: &Value) -> Result<Option<Unit>, String> {
    match ps(p, "unit") {
        None => Ok(None),
        Some(u) => Unit::from_code(u).map(Some).ok_or_else(|| format!("unknown unit '{u}' (hour, day, month or year)")),
    }
}

fn bind(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let field = match ps(p, "field").map(str::trim).filter(|x| !x.is_empty()) {
        Some(fname) => {
            l.dataset.field_index(fname).ok_or_else(|| format!("no field '{fname}'"))?;
            fname.to_string()
        }
        None => time_fields(&l.dataset).into_iter().next().map(|(n, _)| n).ok_or("no field of this layer holds dates")?,
    };
    let span = field_span(&l.dataset, &field).ok_or_else(|| format!("no value of '{field}' reads as a date"))?;
    let name = l.name.clone();
    let fresh = s.map().time.is_none();
    if fresh {
        s.map_mut().time = Some(TimeSlider::spanning(span.0, span.1));
    }
    if let Some(l) = s.map_mut().layer_mut(id) {
        l.time = Some(TimeBinding::new(field.clone()));
    }
    // A slider made for this layer takes the given settings; one already on keeps its own unless
    // given others.
    let own: serde_json::Map<String, Value> =
        ["start", "end", "unit", "interval", "show"].iter().filter_map(|k| p.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
    set(s, &Value::Object(own))?;
    s.active_layer = Some(id);
    let mut r = info(s);
    r["field"] = json!(field);
    r["bound"] = json!(name);
    Ok(r)
}

/// Sets the time slider up, turning it on when it is off (`time.set`).
pub(crate) fn set(s: &mut Session, p: &Value) -> CmdResult {
    if b(p, "off") == Some(true) {
        s.map_mut().time = None;
        s.map_mut().apply_time();
        super::zarr::follow_time(s);
        return Ok(info(s));
    }
    let (start, end, at) = (date(p, "start")?, date(p, "end")?, date(p, "date")?);
    let unit = unit(p)?;
    let show = match ps(p, "show") {
        None => None,
        Some(x) => Some(
            SHOWS
                .iter()
                .find(|(n, ..)| n.eq_ignore_ascii_case(x.trim()))
                .ok_or_else(|| format!("unknown show '{x}' (step, wide, wider or cumulative)"))?,
        ),
    };
    let dates = match p.get("dates") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .take(rustgis_map::time::MAX_STEPS)
                .map(|v| time::parse_input(v).ok_or("dates are dates (text, a year or milliseconds)"))
                .collect::<Result<Vec<f64>, _>>()?,
        ),
        Some(_) => return Err("dates is a list of dates".into()),
    };
    let m = s.map_mut();
    let fresh = m.time.is_none();
    if fresh && start.is_none() && dates.is_none() {
        return Err("the time slider is off: give its start (and end), or bind a layer (time.bind)".into());
    }
    let t = m.time.get_or_insert_with(|| {
        let a = start.or(dates.as_ref().and_then(|d| d.first().copied())).unwrap_or(0.0);
        let z = end.or(dates.as_ref().and_then(|d| d.last().copied())).unwrap_or(a);
        TimeSlider::spanning(a, z)
    });
    if let Some(x) = start {
        t.start = x;
    }
    if let Some(x) = end {
        t.end = x;
    }
    if let Some(u) = unit {
        t.unit = u;
    }
    if let Some(n) = uint(p, "interval") {
        t.interval = n.clamp(1, 10_000) as u32;
    }
    if let Some(d) = dates {
        if let (Some(a), Some(z)) = (d.iter().copied().reduce(f64::min), d.iter().copied().reduce(f64::max)) {
            t.start = a;
            t.end = z;
        }
        t.dates = d;
    }
    if let Some((_, before, after, cumulative)) = show {
        (t.before, t.after, t.cumulative) = (*before, *after, *cumulative);
    }
    if let Some(n) = uint(p, "before") {
        t.before = n.min(10_000) as u32;
    }
    if let Some(n) = uint(p, "after") {
        t.after = n.min(10_000) as u32;
    }
    if let Some(c) = b(p, "cumulative") {
        t.cumulative = c;
    }
    if let Some(ms) = f(p, "speed") {
        t.speed_ms = ms.clamp(16.0, 60_000.0) as u32;
    }
    if let Some(l) = b(p, "loop") {
        t.looping = l;
    }
    if let Some(fmt) = ps(p, "format") {
        t.format = fmt.chars().take(64).collect();
    }
    // A new span starts at its start; otherwise the slider stays on (or near) its step.
    let target = at.unwrap_or(if start.is_some() || fresh { t.start } else { t.current });
    t.normalize();
    t.go_to(target);
    m.apply_time();
    super::zarr::follow_time(s);
    Ok(info(s))
}

/// The executor's bounded frame selection, shared with its pure source selector.
struct ExportFrames {
    saved: TimeSlider,
    steps: Vec<f64>,
    width: f64,
    height: f64,
}

/// Selects date-filtered steps and dimensions within the executor's frame and pixel budgets.
fn export_frames(map: &rustgis_map::Map, p: &Value) -> Result<ExportFrames, String> {
    let (w, h) = (f(p, "width").unwrap_or(640.0).clamp(64.0, 1920.0), f(p, "height").unwrap_or(400.0).clamp(64.0, 1080.0));
    let (from, to) = (date(p, "from")?, date(p, "to")?);
    let saved = map.time.clone().ok_or("the time slider is off")?;
    let steps: Vec<f64> = saved.steps().into_iter().filter(|t| from.is_none_or(|a| *t >= a) && to.is_none_or(|z| *t <= z)).collect();
    if steps.is_empty() {
        return Err("no step falls between from and to".into());
    }
    // Rendering blocks while it runs: a bound on the work.
    if steps.len() > MAX_FRAMES || steps.len() as f64 * w * h > MAX_PIXELS {
        return Err(format!("{} frames at {w:.0} × {h:.0} is too many; narrow from and to, or lower the size", steps.len()));
    }
    Ok(ExportFrames { saved, steps, width: w, height: h })
}

/// Resolves only bounded tile/slider copies with the same date owner that each real frame uses.
fn frame_tiles(map: &rustgis_map::Map, frames: &ExportFrames, mut visit: impl FnMut(&rustgis_map::TileLayer)) {
    for source in &map.tiles {
        let mut tile = source.clone();
        let mut slider = frames.saved.clone();
        for step in &frames.steps {
            slider.current = *step;
            slider.normalize();
            tile.apply_time(Some(&slider));
            if tile.draws() && tile.opacity > 0.0 {
                visit(&tile);
            }
        }
    }
}

/// Binds raw dated definitions and selected steps, showing only resolved redacted destinations.
fn export_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let frames = export_frames(input.map(), p)?;
    let mut inputs = super::tiles::render_inputs(input.map());
    // Raw definitions plus the exact slider/steps bind every derived route without expanding
    // private state into a second copy for every frame. Only redacted resolved URLs are shown.
    inputs.state = json!({"render":inputs.state,"time":frames.saved,"steps":frames.steps});
    let mut places = std::collections::BTreeSet::new();
    frame_tiles(input.map(), &frames, |tile| {
        places.insert(crate::sources::redact(&tile.url));
        places.extend(tile.legends.iter().map(|legend| crate::sources::redact(&legend.url)));
    });
    inputs.review_places = places.into_iter().collect();
    Ok(inputs)
}

/// A later selected frame can reveal another source even when the current frame draws none.
pub(crate) fn export_reach(input: &super::InputContext<'_>, p: &Value) -> crate::policy::Effects {
    let Ok(frames) = export_frames(input.map(), p) else { return crate::policy::Effects::NONE };
    let mut reach = crate::policy::Effects::NONE;
    frame_tiles(input.map(), &frames, |tile| {
        reach = reach.union(super::tiles::render_tile_reach(tile));
    });
    reach
}

/// Renders every step (or those from `from` to `to`) and writes them as a GIF or a zip of PNGs.
fn export(s: &mut Session, p: &Value) -> CmdResult {
    // Files are written by the desktop app; the browser's go through its services.
    if cfg!(target_arch = "wasm32") {
        return Err("exporting time animations needs the desktop app".into());
    }
    let path = ps(p, "path").ok_or("path is required (a .gif, or a .zip of PNG frames)")?.to_string();
    let lower = path.to_ascii_lowercase();
    let gif = lower.ends_with(".gif");
    if !gif && !lower.ends_with(".zip") {
        return Err("path must end in .gif (an animation) or .zip (PNG frames)".into());
    }
    let ExportFrames { saved, steps, width: w, height: h } = export_frames(s.map(), p)?;
    let speed = f(p, "speed").map_or(saved.speed_ms, |x| x.clamp(16.0, 60_000.0) as u32);
    // Each frame shows the view's extent, as Export Map does. The slider comes back however the
    // frames end (an error, or a panic the session catches).
    let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frames(s, &steps, w, h, gif.then_some(speed))));
    s.map_mut().time = Some(saved);
    s.map_mut().apply_time();
    // The step it is back at, read here: a command after the export may read the layer.
    super::zarr::follow_time_now(s);
    let bytes = made.map_err(|_| "exporting the time steps failed unexpectedly".to_string())??;
    rustgis_data::io::write_atomic(std::path::Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
    Ok(json!({"path": path, "frames": steps.len(), "bytes": bytes.len()}))
}

/// The browser build doesn't export (files go through the app's services there), so it leaves
/// the GIF encoder out.
#[cfg(target_arch = "wasm32")]
fn frames(_: &mut Session, _: &[f64], _: f64, _: f64, _: Option<u32>) -> Result<Vec<u8>, String> {
    Err("exporting time animations needs the desktop app".into())
}

/// Renders the map at each step: an animated GIF that loops, each frame shown for `gif`
/// milliseconds, or (`None`) a zip of PNGs.
#[cfg(not(target_arch = "wasm32"))]
fn frames(s: &mut Session, steps: &[f64], w: f64, h: f64, gif: Option<u32>) -> Result<Vec<u8>, String> {
    use image::codecs::gif::{GifEncoder, Repeat};
    let mut out = Vec::new();
    let mut pngs = Vec::new();
    {
        let mut enc = match gif {
            Some(_) => {
                let mut e = GifEncoder::new_with_speed(&mut out, 10);
                e.set_repeat(Repeat::Infinite).map_err(|e| e.to_string())?;
                Some(e)
            }
            None => None,
        };
        for (i, t) in steps.iter().enumerate() {
            if let Some(sl) = s.map_mut().time.as_mut() {
                sl.current = *t;
            }
            s.map_mut().apply_time();
            // Zarr layers that follow the slider read this step's slice, here: each frame draws
            // its own step.
            super::zarr::follow_time_now(s);
            // Dated tile layers have this step's address now: natively, map_png (through
            // rustgis_render::render_complete) fetches the tiles it lacks and waits for them
            // before it draws, so each frame shows its own date's imagery.
            let png = super::share::map_png(s, Some(w), Some(h), Some(96.0))?;
            match (&mut enc, gif) {
                (Some(e), Some(ms)) => {
                    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
                    let delay = image::Delay::from_numer_denom_ms(ms, 1);
                    e.encode_frame(image::Frame::from_parts(img, 0, 0, delay)).map_err(|e| e.to_string())?;
                }
                _ => pngs.push((format!("frame_{:04}.png", i + 1), png)),
            }
        }
    }
    if gif.is_some() { Ok(out) } else { rustgis_data::zip::write(&pngs) }
}

/// `follow_time` visits these source-backed layers in every map, including deferred reads.
/// Capturing their reach is pure: it neither advances time nor opens a Zarr store.
pub(crate) fn following_sources(s: &Session) -> Vec<(u64, u32, &str)> {
    s.project
        .maps
        .iter()
        .flat_map(|m| {
            m.layers.iter().filter_map(move |l| l.raster.as_ref()?.time_zarr.as_ref().map(|z| (m.instance_id(), l.id, z.store.as_str())))
        })
        .collect()
}
/// Commands that actually call the shared `follow_time` executor, including nested settings.
pub(crate) fn follows_sources(id: &str) -> bool {
    matches!(id, "time.bind" | "time.unbind" | "time.set" | "time.goTo" | "time.step" | "time.export")
}
