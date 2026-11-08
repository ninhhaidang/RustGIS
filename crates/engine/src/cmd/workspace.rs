//! View tab (Link Views, Thumbnail, Animation, Profile Viewing, Workflow Manager), Home and Data
//! (Infographics, Offline, Feature Cache) and Help (Diagnostic Monitor) commands.

use base64::Engine;
use rustgis_data::{Geometry, GeometryType};
use rustgis_geom::{Coord, Extent};
use rustgis_map::view::Keyframe;
use serde_json::{Value, json};

use super::{b, f, ps};
use crate::project::{Job, OfflineArea};
use crate::{CmdResult, CommandSpec as C, Session};

/// Removes every Shapefile index not in use and says what it freed, what stayed and why, and which
/// cache folder it couldn't empty.
fn empty_indexes(s: &mut Session, _p: &Value) -> CmdResult {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let e = rustgis_data::source::shp::empty_index_cache();
        if e.folders == 0 && !e.failed.is_empty() {
            return Err(format!("couldn't empty the Shapefile index cache: {}", e.failed.join("; ")));
        }
        for (text, error) in empty_notices(&e) {
            s.notify(text, error);
        }
        Ok(json!({
            "removed": e.removed, "freedBytes": e.removed_bytes, "inUse": e.in_use, "inUseBytes": e.in_use_bytes,
            "keptBytes": e.kept_bytes, "failed": e.failed,
        }))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        Err("the browser keeps no Shapefile index cache".to_string())
    }
}

/// What emptying the Shapefile index cache has to tell the user beyond its counts, each with
/// whether it is an error: nothing when all went.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn empty_notices(e: &rustgis_data::source::shp::Emptied) -> Vec<(String, bool)> {
    let size = |bytes: u64| {
        if bytes < 1024 * 1024 { format!("{:.1} KB", bytes as f64 / 1024.0) } else { format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)) }
    };
    let mut notices: Vec<(String, bool)> = e.failed.iter().map(|f| (format!("Shapefile index cache: couldn't empty {f}"), true)).collect();
    if e.in_use > 0 {
        let what = if e.in_use == 1 { "1 index".to_string() } else { format!("{} indexes", e.in_use) };
        notices.push((
            format!("Shapefile index cache: {what} in use by open layers or another RustGIS window kept ({})", size(e.in_use_bytes)),
            false,
        ));
    }
    if e.kept_bytes > 0 {
        notices.push((
            format!("Shapefile index cache: {} of files not removed (no readable lease, or refused by the system)", size(e.kept_bytes)),
            false,
        ));
    }
    notices
}

fn now() -> f64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Separates in-memory cache controls from commands that read or remove files on disk.
pub fn commands() -> Vec<C> {
    vec![
        C::new("view.linkViews", "Link Views", "View › Link", |s, p| {
            s.project.link_views = b(p, "on").unwrap_or(!s.project.link_views);
            if s.project.link_views {
                s.sync_linked_views();
            }
            Ok(json!({"linked": s.project.link_views}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"on"?: bool}"#),
        C::new("project.thumbnail", "Create Thumbnail", "Publish › Thumbnail", |s, p| {
            let t = thumbnail(s, p)?;
            s.project.thumbnail = t;
            Ok(json!({"thumbnail": s.project.thumbnail.is_some()}))
        }).inputs(thumbnail_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .params(r#"{"create"?: bool (from the map view), "image"?: base64 PNG/JPEG, "path"?: image file, "clear"?: bool}"#),
        C::new("map.thumbnail", "Map Thumbnail", "Publish › Thumbnail", |s, p| {
            let t = thumbnail(s, p)?;
            s.map_mut().thumbnail = t;
            Ok(json!({"thumbnail": s.map().thumbnail.is_some()}))
        }).inputs(thumbnail_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .params(r#"{"create"?: bool, "image"?: base64 PNG/JPEG, "path"?: image file, "clear"?: bool}"#),
        // Workflow Manager.
        C::new("job.new", "Workflow Manager", "View › Windows", |s, p| {
            let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?.chars().take(200).collect();
            if s.project.jobs.len() >= 1000 {
                return Err("a project keeps at most 1000 jobs; remove finished ones first".into());
            }
            let id = s.project.jobs.iter().map(|j| j.id).max().unwrap_or(0).saturating_add(1);
            let extent = if b(p, "viewExtent").unwrap_or(false) { Some(view_extent(s)) } else { None };
            let t = now();
            s.project.jobs.push(Job {
                id,
                name,
                description: ps(p, "description").unwrap_or("").chars().take(4000).collect(),
                status: "new".into(),
                assigned: ps(p, "assigned").unwrap_or("").chars().take(200).collect(),
                extent,
                created: t,
                updated: t,
            });
            Ok(json!({"job": id}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string, "description"?, "assigned"?, "viewExtent"?: bool (the job covers the map view)}"#),
        C::new("job.update", "Update Job", "View › Windows", |s, p| {
            let id = p.get("job").and_then(Value::as_u64).ok_or("job is required")?;
            let status = ps(p, "status").map(str::to_string);
            if status.as_deref().is_some_and(|st| !matches!(st, "new" | "in progress" | "on hold" | "done")) {
                return Err("status is new, in progress, on hold or done".into());
            }
            let j = s.project.jobs.iter_mut().find(|j| u64::from(j.id) == id).ok_or_else(|| format!("no job {id}"))?;
            if let Some(st) = status {
                j.status = st;
            }
            if let Some(a) = ps(p, "assigned") {
                j.assigned = a.chars().take(200).collect();
            }
            if let Some(d) = ps(p, "description") {
                j.description = d.chars().take(4000).collect();
            }
            j.updated = now();
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"job": id, "status"?: "new"|"in progress"|"on hold"|"done", "assigned"?, "description"?}"#),
        C::new("job.remove", "Delete Job", "View › Windows", |s, p| {
            let id = p.get("job").and_then(Value::as_u64).ok_or("job is required")?;
            let before = s.project.jobs.len();
            s.project.jobs.retain(|j| u64::from(j.id) != id);
            if s.project.jobs.len() == before { Err(format!("no job {id}")) } else { Ok(Value::Null) }
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"job": id}"#),
        // Animation.
        C::new("animation.add", "Add Keyframe", "View › Animation", |s, p| {
            let c = s.map().center_lonlat();
            let k = Keyframe { center: c, scale: s.scale(), seconds: f(p, "seconds").unwrap_or(2.0).clamp(0.1, 600.0) };
            let m = s.map_mut();
            if m.animation.len() >= 500 {
                return Err("an animation has at most 500 keyframes".into());
            }
            m.animation.push(k);
            Ok(json!({"keyframes": m.animation.len()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"seconds"?: time from the previous keyframe}"#),
        C::new("animation.remove", "Remove Keyframe", "View › Animation", |s, p| {
            let m = s.map_mut();
            match p.get("index").and_then(Value::as_u64) {
                Some(i) if (i as usize) < m.animation.len() => {
                    m.animation.remove(i as usize);
                }
                Some(i) => return Err(format!("no keyframe {i}")),
                None if b(p, "all").unwrap_or(false) => m.animation.clear(),
                None => {
                    m.animation.pop().ok_or("the map has no keyframes")?;
                }
            }
            Ok(json!({"keyframes": m.animation.len()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"index"?: number (the last one when omitted), "all"?: bool}"#),
        C::new("animation.export", "Export Animation", "View › Animation", |s, p| {
            let path = ps(p, "path").ok_or("path is required (a .zip of PNG frames)")?.to_string();
            let bytes = animation_zip(s, p)?;
            rustgis_data::io::write_atomic(std::path::Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        }).inputs(map_render_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .view()
        .params(r#"{"path": "*.zip", "fps"?: 1..30, "width"?, "height"?: px}"#),
        // Profiles.
        C::new("profile.create", "Create Profile", "View › Profile Viewing", |s, p| {
            let pts = points(p)?;
            profile(s, p, &pts)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"layer": elevation raster, "points": [[lon, lat], …], "samples"?: number}"#),
        C::new("profile.autoComplete", "Auto-complete Profile", "View › Profile Viewing", |s, p| {
            // Continue the line from its first point through its second to the raster's edge.
            let pts = points(p)?;
            let (a, b2) = (pts.first().copied().ok_or("give two points")?, pts.get(1).copied().ok_or("give two points")?);
            let r = dem(s, p)?;
            let e = r.extent();
            let (dx, dy) = (b2.x - a.x, b2.y - a.y);
            if dx == 0.0 && dy == 0.0 {
                return Err("the two points are the same".into());
            }
            let mut t = 1.0;
            while t < 1e4 && e.contains(Coord::new(a.x + dx * t * 1.1, a.y + dy * t * 1.1)) {
                t *= 1.1;
            }
            profile(s, p, &[a, Coord::new(a.x + dx * t, a.y + dy * t)])
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"layer": elevation raster, "samples"?: number (default 200), "points": [[lon, lat], [lon, lat]] (a start and a direction)}"#),
        C::new("profile.crossSection", "Cross Section", "View › Profile Viewing", |s, p| {
            // Across a line at a point: perpendicular, `width` metres long.
            let pts = points(p)?;
            let (a, b2) =
                (pts.first().copied().ok_or("give the line's two points")?, pts.get(1).copied().ok_or("give the line's two points")?);
            let t = f(p, "at").unwrap_or(0.5).clamp(0.0, 1.0);
            let mid = Coord::new(a.x + (b2.x - a.x) * t, a.y + (b2.y - a.y) * t);
            let k = mid.y.to_radians().cos().max(0.01);
            let (dx, dy) = ((b2.x - a.x) * k, b2.y - a.y);
            let len = (dx * dx + dy * dy).sqrt().max(1e-12);
            let half_deg = f(p, "width").unwrap_or(2000.0).clamp(1.0, 1e6) / 2.0 / 111_320.0;
            let (nx, ny) = (-dy / len * half_deg / k, dx / len * half_deg);
            profile(s, p, &[Coord::new(mid.x - nx, mid.y - ny), Coord::new(mid.x + nx, mid.y + ny)])
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(
            r#"{"layer": elevation raster, "samples"?: number (default 200), "points": [[lon, lat], [lon, lat]] (the line crossed), "at"?: 0..1 along it, "width"?: metres}"#,
        ),
        C::new("profile.alongLine", "Profile Along a Line", "View › Profile Viewing", |s, p| {
            let lines = selected_lines(s, p, false)?;
            let first = lines.into_iter().next().ok_or("no line")?;
            profile(s, p, &first)
        }).inputs(profile_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"layer": elevation raster, "samples"?: number (default 200), "lines": line layer, "row"?: number (the first selected when omitted)}"#),
        C::new("profile.alongSelected", "Profile Along Selected Lines", "View › Profile Viewing", |s, p| {
            let lines = selected_lines(s, p, true)?;
            let out: Vec<Value> = lines.iter().take(50).map(|l| profile(s, p, l)).collect::<Result<_, _>>()?;
            Ok(Value::Array(out))
        }).inputs(profile_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"layer": elevation raster, "samples"?: number (default 200), "lines": line layer}"#),
        // Infographics and diagnostics.
        C::new("info.infographic", "Infographics", "Home › Inspect", infographic).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"layer"? (every visible feature layer when omitted)}"#),
        C::new("info.diagnostics", "Diagnostic Monitor", "Help › Performance", |s, _| Ok(diagnostics(s))).roles(&[]).effects(crate::policy::Effects::NONE).view(),
        // Offline areas.
        C::new("offline.download", "Download Map", "Data › Offline", offline_download).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"name"?: string, "extent"?: [minLon, minLat, maxLon, maxLat] (the map view when omitted)}"#),
        C::new("offline.sync", "Sync", "Data › Offline", offline_sync).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"name": offline area}"#),
        C::new("offline.remove", "Remove", "Data › Offline", |s, p| {
            let n = ps(p, "name").ok_or("name is required")?;
            let before = s.project.offline_areas.len();
            s.project.offline_areas.retain(|a| !a.name.eq_ignore_ascii_case(n));
            if s.project.offline_areas.len() == before {
                return Err(format!("no offline area '{n}'"));
            }
            let map_name = format!("{n} (offline)");
            if s.project.maps.len() > 1
                && let Some(i) = s.project.maps.iter().position(|m| m.name.eq_ignore_ascii_case(&map_name))
            {
                s.project.maps.remove(i);
                // The active map stays the same map; if it was the removed one, a neighbour.
                let a = s.project.active_map;
                s.project.active_map = if a > i { a - 1 } else { a.min(s.project.maps.len().saturating_sub(1)) };
                if a == i {
                    s.active_layer = s.map().layers.first().map(|l| l.id);
                }
            }
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": offline area}"#),
        C::new("offline.open", "Open Offline Map", "Data › Offline", |s, p| {
            let n = ps(p, "name").ok_or("name is required")?;
            let a =
                s.project.offline_areas.iter().find(|a| a.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no offline area '{n}'"))?;
            crate::project::ensure_map_imports_complete(&a.data)?;
            let map_name = format!("{} (offline)", a.name);
            if let Some(i) = s.project.maps.iter().position(|m| m.name.eq_ignore_ascii_case(&map_name)) {
                s.project.active_map = i;
            } else {
                let mut m = a.data.clone();
                m.name = map_name;
                m.renew_instance_id();
                // Reading a project checks only maps' layers, not offline snapshots', so their
                // selections are put right here.
                for l in &mut m.layers {
                    l.renew_instance_id();
                    l.normalize_selection();
                }
                // As a project's maps say when opened (#1596).
                for (_, text) in crate::project::label_losses::hidden(m.layers.iter()) {
                    s.notify(text, false);
                }
                s.project.maps.push(m);
                s.project.active_map = s.project.maps.len() - 1;
            }
            s.active_layer = s.map().layers.first().map(|l| l.id);
            let e = s.project.offline_areas.iter().find(|a| a.name.eq_ignore_ascii_case(n)).map(|a| a.extent).unwrap_or(Extent::EMPTY);
            s.zoom_to(&e);
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": offline area}"#),
        // Feature cache.
        C::new("cache.fill", "Fill Cache", "Data › Feature Cache", |s, _| {
            let n = fill_cache(s);
            Ok(json!({"cached": n}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("cache.empty", "Empty Cache", "Data › Feature Cache", |s, _| {
            let mut n = 0;
            for m in &mut s.project.maps {
                for l in &mut m.layers {
                    l.clear_cache();
                    n += 1;
                }
            }
            Ok(json!({"emptied": n}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        // The Shapefile index cache on disk, shared by every project (#1259). Command search,
        // scripts and the CLI run it; it has no ribbon button.
        C::new("cache.emptyIndexes", "Empty Index Cache", "Data › Shapefile Index Cache", empty_indexes).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).view(),
        // A fill runs to the end in one go (it's quick); cancelling stops the automatic fills
        // that follow projection changes.
        C::new("cache.cancel", "Cancel Caching", "Data › Feature Cache", |s, _| {
            s.auto_cache = false;
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("cache.auto", "Auto Cache", "Data › Feature Cache", |s, p| {
            s.auto_cache = b(p, "on").unwrap_or(!s.auto_cache);
            if s.auto_cache {
                fill_cache(s);
            }
            Ok(json!({"autoCache": s.auto_cache}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"on"?: bool}"#),
    ]
}

/// Fills every layer's projected geometry cache.
pub(crate) fn fill_cache(s: &mut Session) -> usize {
    let mut n = 0;
    for m in &mut s.project.maps {
        let (flat, key) = (m.flat(), m.projection.code());
        for l in &mut m.layers {
            // Counts the layers this fill actually projected.
            if !l.is_raster() && !l.is_cached(&key) {
                l.projected_in(&flat, &key);
                n += 1;
            }
        }
    }
    n
}

/// The longitude/latitude extent the flat map's view shows.
pub fn view_extent(s: &Session) -> Extent {
    view_extent_of(s.map(), s.view_size)
}

/// The longitude/latitude extent a `size` view of map `m` shows.
pub fn view_extent_of(m: &rustgis_map::Map, (w, h): (f64, f64)) -> Extent {
    let e = m.camera.extent(w, h);
    let mut ll = Extent::EMPTY;
    for i in 0..=8 {
        for j in 0..=8 {
            let c = Coord::new(e.min_x + e.width() * i as f64 / 8.0, e.min_y + e.height() * j as f64 / 8.0);
            if let Some(x) = m.to_lonlat(c) {
                ll.expand(x);
            }
        }
    }
    ll
}

/// Connects active-map rendering to the common layer and raw tile route bindings.
fn map_render_inputs(input: &super::InputContext<'_>, _: &Value) -> Result<super::Inputs, String> {
    Ok(super::tiles::render_inputs(input.map()))
}

/// Clearing wins; otherwise only an explicit create request asks the map renderer.
fn creates_thumbnail(p: &Value) -> bool {
    !b(p, "clear").unwrap_or(false) && b(p, "create").unwrap_or(false)
}

/// Adds renderer bindings only on the branch that creates a thumbnail from the map.
fn thumbnail_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    if creates_thumbnail(p) { map_render_inputs(input, p) } else { Ok(super::Inputs::layers(std::iter::empty())) }
}

/// Clears, renders, or accepts a supplied picture in the same precedence as its selector.
fn thumbnail(s: &mut Session, p: &Value) -> Result<Option<String>, String> {
    if b(p, "clear").unwrap_or(false) {
        return Ok(None);
    }
    if creates_thumbnail(p) {
        let png = super::share::map_png(s, Some(240.0), Some(160.0), Some(96.0))?;
        return Ok(Some(base64::engine::general_purpose::STANDARD.encode(png)));
    }
    let b64 = super::graphics::picture(p)?;
    Ok(Some(b64))
}

fn points(p: &Value) -> Result<Vec<Coord>, String> {
    let pts: Vec<Coord> = p
        .get("points")
        .and_then(Value::as_array)
        .ok_or("points is required")?
        .iter()
        .take(10_000)
        .filter_map(|v| Some(Coord::new(v.get(0)?.as_f64()?, v.get(1)?.as_f64()?)))
        .filter(|c| c.is_finite())
        .collect();
    if pts.len() < 2 { Err("a profile needs two or more points".into()) } else { Ok(pts) }
}

fn dem(s: &Session, p: &Value) -> Result<std::sync::Arc<rustgis_data::raster::Raster>, String> {
    let n = ps(p, "layer").ok_or("layer (an elevation raster) is required")?;
    s.map().layer_by_name(n).and_then(|l| l.raster.as_ref()).and_then(|r| r.data.clone()).ok_or_else(|| format!("no raster layer '{n}'"))
}

/// An elevation profile with its summary.
fn profile(s: &Session, p: &Value, path: &[Coord]) -> CmdResult {
    let r = dem(s, p)?;
    let samples = p.get("samples").and_then(Value::as_u64).unwrap_or(200) as usize;
    let prof = rustgis_analysis::rasterops::profile(&r, 0, path, samples);
    let zs: Vec<f64> = prof.iter().filter_map(|x| x.2).collect();
    if zs.is_empty() {
        return Err("the line doesn't cross the raster".into());
    }
    let (lo, hi) = zs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), z| (a.min(*z), b.max(*z)));
    let (mut gain, mut loss) = (0.0, 0.0);
    for w in zs.windows(2) {
        if let [a, b2] = w {
            if b2 > a { gain += b2 - a } else { loss += a - b2 }
        }
    }
    let rows: Vec<Value> = prof.iter().map(|(d, c, z)| json!({"distance": d, "lon": c.x, "lat": c.y, "elevation": z})).collect();
    Ok(json!({"length": prof.last().map_or(0.0, |x| x.0), "min": lo, "max": hi, "gain": gain, "loss": loss, "samples": rows}))
}

fn selected_lines(s: &Session, p: &Value, all: bool) -> Result<Vec<Vec<Coord>>, String> {
    let n = profile_lines(p)?;
    let l =
        s.map().layer_by_name(n).filter(|l| l.geometry_type() == GeometryType::Polyline).ok_or_else(|| format!("no line layer '{n}'"))?;
    let rows: Vec<usize> = match p.get("row").and_then(Value::as_u64) {
        Some(r) if !all => vec![r as usize],
        _ if l.selection.is_empty() => return Err(format!("select lines in {n} first")),
        _ if all => l.selection.clone(),
        _ => l.selection.iter().take(1).copied().collect(),
    };
    let out: Vec<Vec<Coord>> = rows
        .iter()
        .filter_map(|r| match l.dataset.features.get(*r)?.geometry.as_ref()? {
            Geometry::Line(parts) => Some(parts.iter().flatten().copied().collect()),
            _ => None,
        })
        .filter(|v: &Vec<Coord>| v.len() >= 2)
        .collect();
    if out.is_empty() { Err("no usable lines".into()) } else { Ok(out) }
}

/// Every keyframe-to-keyframe move rendered as PNG frames in a zip.
fn animation_zip(s: &mut Session, p: &Value) -> Result<Vec<u8>, String> {
    let keys = s.map().animation.clone();
    if keys.len() < 2 {
        return Err("add at least two keyframes".into());
    }
    let fps = f(p, "fps").unwrap_or(10.0).clamp(1.0, 30.0);
    let (w, h) = (f(p, "width").unwrap_or(640.0).clamp(64.0, 1920.0), f(p, "height").unwrap_or(400.0).clamp(64.0, 1080.0));
    // The frames as they will be made (each move rounded, plus the last), within a count and a
    // pixel budget (every PNG is held until the zip is written).
    let count: usize = keys.windows(2).map(|w| w.get(1).map_or(1, |k| (k.seconds * fps).round().max(1.0) as usize)).sum::<usize>() + 1;
    if count > 3000 || count as f64 * w * h > 1.5e9 {
        return Err(format!("the animation would have {count} frames at {w:.0} × {h:.0}; lower the fps, the durations or the size"));
    }
    let saved = s.map().camera;
    let saved_size = s.view_size;
    s.view_size = (w, h);
    let mut frames = Vec::new();
    let mut n = 0;
    let mut render = |s: &mut Session, c: Coord, scale: f64| -> Result<(), String> {
        let m = s.map_mut();
        if let Some(mc) = m.to_map(c) {
            m.camera.center = mc;
        }
        m.set_scale(scale);
        let png = super::share::map_png(s, Some(w), Some(h), Some(96.0))?;
        n += 1;
        frames.push((format!("frame_{n:04}.png"), png));
        Ok(())
    };
    let mut result = Ok(());
    'frames: for pair in keys.windows(2) {
        let [a, b2] = pair else { continue };
        let steps = (b2.seconds * fps).round().max(1.0) as usize;
        for i in 0..steps {
            let t = i as f64 / steps as f64;
            // Smooth in and out; scale changes geometrically.
            let e = t * t * (3.0 - 2.0 * t);
            let c = Coord::new(a.center.x + (b2.center.x - a.center.x) * e, a.center.y + (b2.center.y - a.center.y) * e);
            let sc = rustgis_map::camera::scale_between(a.scale, b2.scale, e);
            if let Err(e) = render(s, c, sc) {
                result = Err(e);
                break 'frames;
            }
        }
    }
    if result.is_ok()
        && let Some(last) = keys.last()
    {
        result = render(s, last.center, last.scale);
    }
    s.map_mut().camera = saved;
    s.view_size = saved_size;
    result?;
    rustgis_data::zip::write(&frames)
}

/// The project's size and the session's state (Diagnostic Monitor). Read-only, so the monitor can
/// show it without running a command.
pub fn diagnostics(s: &Session) -> Value {
    let mut features = 0usize;
    let mut vertices = 0usize;
    let mut pixels = 0usize;
    let mut cached = 0usize;
    let mut layers = 0usize;
    for m in &s.project.maps {
        let key = m.projection.code();
        for l in &m.layers {
            layers += 1;
            features += l.dataset.features.len();
            vertices = vertices
                .saturating_add(l.dataset.features.iter().filter_map(|f| f.geometry.as_ref()).map(|g| g.coord_count()).sum::<usize>());
            pixels = pixels.saturating_add(
                l.raster
                    .as_ref()
                    .and_then(|r| r.data.as_ref())
                    .map_or(0, |d| (d.width as usize).saturating_mul(d.height as usize).saturating_mul(d.band_count())),
            );
            cached += usize::from(l.is_cached(&key));
        }
    }
    json!({
        "maps": s.project.maps.len(), "layers": layers, "features": features, "vertices": vertices, "rasterPixels": pixels,
        "cachedLayers": cached, "estimatedMemoryMB": ((vertices as f64 * 16.0 + features as f64 * 64.0 + pixels as f64 * 4.0) / 1_048_576.0 * 10.0).round() / 10.0,
        "commands": s.registry.commands.len(), "revision": s.revision, "undo": s.undo_depth(), "notices": s.notices.len(),
        "layouts": s.project.layouts.len(), "geoprocessingRuns": s.project.gp_history.len(),
    })
}

fn infographic(s: &mut Session, p: &Value) -> CmdResult {
    Ok(infographic_of(s, ps(p, "layer")))
}

/// Cards summarising the features in view, for every visible feature layer or just `only`.
pub fn infographic_of(s: &Session, only: Option<&str>) -> Value {
    let e = view_extent(s);
    let only = only.map(str::to_ascii_lowercase);
    let mut cards = Vec::new();
    for l in s.map().layers.iter().filter(|l| s.map().shows(l) && !l.is_raster() && !l.graphics) {
        if only.as_ref().is_some_and(|o| *o != l.name.to_ascii_lowercase()) {
            continue;
        }
        let rows: Vec<usize> = l
            .visible_rows()
            .iter()
            .enumerate()
            .filter(|(r, v)| **v && l.dataset.features.get(*r).and_then(|f| f.geometry.as_ref()).is_some_and(|g| g.extent().intersects(&e)))
            .map(|(r, _)| r)
            .collect();
        let numbers: Vec<Value> = l
            .dataset
            .fields
            .iter()
            .enumerate()
            .filter(|(_, fd)| fd.ty.is_numeric())
            .take(4)
            .map(|(fi, fd)| {
                let v: Vec<f64> = rows.iter().filter_map(|r| l.dataset.value(*r, fi).as_f64()).filter(|x| x.is_finite()).collect();
                let sum: f64 = v.iter().sum();
                json!({"field": fd.name, "sum": sum, "mean": if v.is_empty() { None } else { Some(sum / v.len() as f64) }, "max": v.iter().copied().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))})
            })
            .collect();
        let category = l.dataset.fields.iter().enumerate().find(|(_, fd)| !fd.ty.is_numeric() && !fd.name.eq_ignore_ascii_case("name"));
        let top: Vec<Value> = category
            .map(|(fi, _)| {
                let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                for r in &rows {
                    *counts.entry(l.dataset.value(*r, fi).to_string()).or_insert(0) += 1;
                }
                let mut v: Vec<(String, usize)> = counts.into_iter().collect();
                v.sort_by(|a, b2| b2.1.cmp(&a.1).then_with(|| a.0.cmp(&b2.0)));
                v.into_iter().take(5).map(|(k, n)| json!({"value": k, "count": n})).collect()
            })
            .unwrap_or_default();
        cards.push(json!({"layer": l.name, "count": rows.len(), "numbers": numbers, "category": category.map(|(_, f)| f.name.clone()), "top": top}));
    }
    json!({"extent": [e.min_x, e.min_y, e.max_x, e.max_y], "layers": cards})
}

/// The map's feature layers clipped to an extent (rasters stay behind: they're files already).
fn snapshot(m: &mut rustgis_map::Map, e: &Extent) -> rustgis_map::Map {
    // The layers (the bulk) are set aside while the rest of the map is copied, then each
    // feature layer is copied without its features, and only the features in the extent added.
    let layers = std::mem::take(&mut m.layers);
    let mut out = m.clone();
    m.layers = layers;
    for l in m.layers.iter_mut().filter(|l| !l.is_raster()) {
        let features = std::mem::take(&mut l.dataset.features);
        let mut c = l.clone();
        l.dataset.features = features;
        c.dataset.features =
            l.dataset.features.iter().filter(|f| f.geometry.as_ref().is_some_and(|g| g.extent().intersects(e))).cloned().collect();
        c.selection.clear();
        c.touch_geometry();
        out.layers.push(c);
    }
    out
}

/// Most offline areas a project keeps (each holds a copy of its features).
const MAX_AREAS: usize = 20;

fn offline_download(s: &mut Session, p: &Value) -> CmdResult {
    crate::project::ensure_map_imports_complete(s.map())?;
    let e = match p.get("extent").and_then(Value::as_array) {
        Some(a) => {
            let n: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
            match n.as_slice() {
                [x0, y0, x1, y1] if x0 < x1 && y0 < y1 => Extent::new(*x0, *y0, *x1, *y1),
                _ => return Err("extent is [minLon, minLat, maxLon, maxLat]".into()),
            }
        }
        None => view_extent(s),
    };
    if e.is_empty() {
        return Err("the extent is empty".into());
    }
    let base = ps(p, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(200).collect())
        .unwrap_or_else(|| format!("{} offline", s.map().name));
    let mut name = base.clone();
    let mut i = 1;
    // Unique among areas, and its offline map's name ("<area> (offline)") unused by other maps.
    let taken = |s: &Session, n: &str| {
        s.project.offline_areas.iter().any(|a| a.name.eq_ignore_ascii_case(n))
            || s.project.maps.iter().any(|m| m.name.eq_ignore_ascii_case(&format!("{n} (offline)")))
    };
    while taken(s, &name) {
        i += 1;
        name = format!("{base} {i}");
    }
    if s.project.offline_areas.len() >= MAX_AREAS {
        return Err(format!("a project keeps at most {MAX_AREAS} offline areas; remove one first"));
    }
    let data = snapshot(s.map_mut(), &e);
    let features: usize = data.layers.iter().map(|l| l.dataset.features.len()).sum();
    let t = now();
    s.project.offline_areas.push(OfflineArea { name: name.clone(), map: s.map().name.clone(), extent: e, created: t, synced: t, data });
    Ok(json!({"name": name, "features": features}))
}

fn offline_fields_match(a: &[rustgis_data::Field], b: &[rustgis_data::Field]) -> bool {
    // Attribute values are positional; aliases are display metadata and field lookup ignores case.
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.name.eq_ignore_ascii_case(&y.name) && x.ty == y.ty)
}

/// Copies edits made to an offline area (in its offline map, if open) back to its source map by
/// feature id: changed and new features update the source, deleted ones are removed from it.
/// A feature changed in the source too since the last sync takes the area's version (or its
/// deletion), and is reported with a warning, as is an offline edit to a feature since deleted
/// from the source.
/// Then the area is refreshed from the source.
fn offline_sync(s: &mut Session, p: &Value) -> CmdResult {
    let n = ps(p, "name").ok_or("name is required")?;
    let ai = s.project.offline_areas.iter().position(|a| a.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no offline area '{n}'"))?;
    let area = s.project.offline_areas.get(ai).cloned().ok_or("no such area")?;
    let offline_map = format!("{} (offline)", area.name);
    let edited = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&offline_map)).cloned().unwrap_or_else(|| area.data.clone());
    let si = s
        .project
        .maps
        .iter()
        .position(|m| m.name.eq_ignore_ascii_case(&area.map))
        .ok_or_else(|| format!("the source map '{}' is gone", area.map))?;
    crate::project::ensure_map_imports_complete(s.project.maps.get(si).ok_or("the source map is gone")?)?;
    let (mut updated, mut added, mut deleted, mut skipped) = (0, 0, 0, 0);
    // Features changed in the source since the last sync whose changes the area's version (or its
    // deletion) replaced, and features changed offline whose edits a deletion in the source
    // dropped, as "layer #id".
    let (mut conflicts, mut dropped) = (Vec::new(), Vec::new());
    let tag = |layer: &str, id: u64| format!("{layer} #{id}");
    if let Some(src) = s.project.maps.get_mut(si) {
        for ol in &edited.layers {
            // Layers are matched by id (the snapshot keeps them), so same-named layers don't mix.
            // Layers removed from either side since the download are left alone.
            let Some(sl) = src.layers.iter_mut().find(|l| l.id == ol.id && !l.is_raster()) else { continue };
            let baseline = area.data.layers.iter().find(|l| l.id == ol.id);
            if !offline_fields_match(&sl.dataset.fields, &ol.dataset.fields)
                && baseline.is_none_or(|l| {
                    !offline_fields_match(&l.dataset.fields, &ol.dataset.fields) || l.dataset.features != ol.dataset.features
                })
            {
                return Err(format!("cannot sync layer '{}': source and offline fields differ; align the fields before syncing", ol.name));
            }
            // The ids the area held when it was last synced.
            let had: std::collections::HashSet<u64> =
                baseline.map(|l| l.dataset.features.iter().map(|f| f.id).collect()).unwrap_or_default();
            let now_ids: std::collections::HashSet<u64> = ol.dataset.features.iter().map(|f| f.id).collect();
            // The area's copies as last synced: only features changed offline since then go back,
            // so edits made to the source meanwhile aren't overwritten by stale copies.
            let base: std::collections::HashMap<u64, &rustgis_data::Feature> =
                baseline.map(|l| l.dataset.features.iter().map(|f| (f.id, f)).collect()).unwrap_or_default();
            // Deletions first, so a fresh id given to a new feature below can't be swept away.
            let before = sl.dataset.features.len();
            sl.dataset.features.retain(|x| {
                let keep = !had.contains(&x.id) || now_ids.contains(&x.id);
                // Deleted offline but changed in the source meanwhile.
                if !keep && base.get(&x.id).is_some_and(|b| *b != x) {
                    conflicts.push(tag(&sl.name, x.id));
                }
                keep
            });
            deleted += before - sl.dataset.features.len();
            for f in &ol.dataset.features {
                if had.contains(&f.id) {
                    let changed_offline = base.get(&f.id).is_none_or(|b| *b != f);
                    match sl.dataset.features.iter_mut().find(|x| x.id == f.id) {
                        Some(x) if changed_offline && x != f => {
                            if base.get(&f.id).is_some_and(|b| *b != x) {
                                conflicts.push(tag(&sl.name, f.id));
                            }
                            *x = f.clone();
                            updated += 1;
                        }
                        // Changed offline but deleted from the source meanwhile: left deleted, and
                        // reported.
                        None if changed_offline => {
                            skipped += 1;
                            dropped.push(tag(&sl.name, f.id));
                        }
                        _ => {}
                    }
                } else {
                    // Added offline (its id may clash with one outside the area): a fresh id, above
                    // any the source or the area has used.
                    let id = sl.dataset.features.iter().map(|x| x.id).chain(had.iter().copied()).max().unwrap_or(0).saturating_add(1);
                    sl.dataset.features.push(rustgis_data::Feature { id, ..f.clone() });
                    added += 1;
                }
            }
            sl.selection.clear();
            sl.touch_geometry();
        }
    }
    let fresh = s.project.maps.get_mut(si).map(|m| snapshot(m, &area.extent)).ok_or("the source map is gone")?;
    if let Some(m) = s.project.maps.iter_mut().find(|m| m.name.eq_ignore_ascii_case(&offline_map)) {
        let name = m.name.clone();
        *m = fresh.clone();
        m.name = name;
        // A copy of the source map: a map, and layers, of its own.
        m.renew_instance_id();
        for l in &mut m.layers {
            l.renew_instance_id();
        }
    }
    if let Some(a) = s.project.offline_areas.get_mut(ai) {
        a.data = fresh;
        a.synced = now();
    }
    // Never silent: say which features lost an edit, so they can be checked (Undo puts the
    // source's versions back).
    let listed = |ids: &[String]| {
        let mut said: Vec<String> = ids.iter().take(20).cloned().collect();
        if ids.len() > 20 {
            said.push(format!("{} more", ids.len() - 20));
        }
        said.join(", ")
    };
    if !conflicts.is_empty() {
        s.notify(
            format!(
                "Offline area '{}': {} feature(s) were changed in the map '{}' since the last sync and also edited or deleted in the area, so the area's version replaced the map's changes: {}. Undo restores the map's versions",
                area.name,
                conflicts.len(),
                area.map,
                listed(&conflicts)
            ),
            false,
        );
    }
    if !dropped.is_empty() {
        s.notify(
            format!(
                "Offline area '{}': {} feature(s) edited in the area were deleted from the map '{}' meanwhile, so those edits were dropped: {}",
                area.name,
                dropped.len(),
                area.map,
                listed(&dropped)
            ),
            false,
        );
    }
    Ok(json!({"updated": updated, "added": added, "deleted": deleted, "skipped": skipped, "conflicts": conflicts.len()}))
}

/// Binds the elevation raster and literal selected-line source.
fn profile_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    super::Inputs::named(s.map(), [super::imagery::raster_name(p)?, profile_lines(p)?])
}

/// Parses the same required line-layer name used by profile extraction.
fn profile_lines(p: &Value) -> Result<&str, String> {
    ps(p, "lines").ok_or("lines (a line layer) is required".into())
}
