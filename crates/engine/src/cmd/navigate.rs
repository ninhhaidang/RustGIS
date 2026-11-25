//! Home › Navigate, Add (basemap) and Project (new map), bookmarks.

use rustgis_geom::{Coord, Extent};
use rustgis_map::layout::ElementKind;
use rustgis_map::presentation::PageKind;
use rustgis_map::{Basemap, Bookmark, Map, Projection};
use serde_json::{Value, json};

use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("map.zoomFull", "Full Extent", "Home › Navigate", zoom_full).roles(&[]).effects(crate::policy::Effects::NONE).view(),
        C::new("map.zoomIn", "Fixed Zoom In", "Home › Navigate", |s, _| zoom(s, 2.0)).roles(&[]).effects(crate::policy::Effects::NONE).view().key("Mod+="),
        C::new("map.zoomOut", "Fixed Zoom Out", "Home › Navigate", |s, _| zoom(s, 0.5)).roles(&[]).effects(crate::policy::Effects::NONE).view().key("Mod+-"),
        C::new("map.prevExtent", "Previous Extent", "Home › Navigate", |s, _| {
            if s.extent_back() { Ok(Value::Null) } else { Err("no previous extent".into()) }
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .when(|s| if s.can_extent_back() { None } else { Some("no previous extent") }),
        C::new("map.nextExtent", "Next Extent", "Home › Navigate", |s, _| {
            if s.extent_forward() { Ok(Value::Null) } else { Err("no next extent".into()) }
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .when(|s| if s.can_extent_forward() { None } else { Some("no next extent") }),
        C::new("map.zoomWorld", "Zoom To World", "Home › Navigate", |s, _| {
            s.zoom_to(&Extent::new(-170.0, -58.0, 180.0, 78.0));
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("map.extent", "View Extent", "Home › Navigate", |s, _| {
            let e = if s.map().projection.is_globe() { Extent::new(-180.0, -90.0, 180.0, 90.0) } else { super::workspace::view_extent(s) };
            if e.is_empty() {
                return Err("the view shows no part of the Earth".into());
            }
            Ok(json!({"extent": [e.min_x, e.min_y, e.max_x, e.max_y]}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("map.zoomToExtent", "Zoom To Extent", "Home › Navigate", zoom_to_extent).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"extent": [minLon, minLat, maxLon, maxLat]}"#),
        C::new("map.goToXY", "By Coordinates…", "Home › Navigate › Locate", go_to_xy).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"lon" | "x": degrees (-74.006, or text such as 74°0′22″W), or with crs the easting in that system, "lat" | "y": degrees (40.7128, or text such as 40°42′46″N), or with crs the northing, "crs"?: the system x and y are in: "map" (the map's own, as the status bar reads it), an EPSG or ESRI code or other system text (EPSG:2056), or a layer's name (the system it was read in); default degrees, "position"?: text in place of lon and lat, as the search box reads it, latitude first (40.7128, -74.006; 18north 583959 4507351; 18TWL8395907350; 2600000 mE 1200000 mN EPSG:2056, as the map's right-click menu copies it), "scale"?: number, "bearing"?: degrees clockwise from north, "pitch"?: degrees of tilt (0 to 85)}"#),
        C::new("map.convertPosition", "Convert Position", "Home › Navigate › Locate", convert_position).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"lon" | "x": degrees (-74.006, or text such as 74°0′22″W), or with crs the easting in that system, "lat" | "y": degrees (40.7128, or text such as 40°42′46″N), or with crs the northing, "crs"?: the system x and y are in: "map" (the map's own), an EPSG or ESRI code or other system text (EPSG:2056), or a layer's name (the system it was read in); default degrees, "position"?: text in place of lon and lat, as the search box reads it, latitude first (40.7128, -74.006; 18north 583959 4507351; 18TWL8395907350; Z AC 77146 62481; 2600000 mE 1200000 mN EPSG:2056), "digits"?: MGRS digits per axis, 0 (100 km) to 5 (1 m, the default)}"#),
        C::new("map.goToLink", "Go To Map Link", "Home › Navigate", go_to_link).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"url": a map link ending in #zoom/lat/lon[/bearing[/pitch]] (as MapLibre writes them; also #map=…), or just zoom/lat/lon[/bearing[/pitch]]}"#),
        C::new("map.link", "Map Link", "Home › Navigate", |s, _| {
            let hash = link_of(s.map()).ok_or("the view shows no part of the Earth")?;
            Ok(json!({"hash": hash}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("map.setBearing", "Rotate Map", "Home › Navigate", set_bearing).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"bearing": degrees clockwise from north (the direction the top of the view faces) | "by": degrees to turn it further}"#),
        C::new("map.setPitch", "Tilt Map", "Home › Navigate", set_pitch).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"pitch": degrees of tilt from looking straight down (0 to 85) | "by": degrees to tilt it further}"#),
        C::new("map.resetNorth", "Reset North", "Home › Navigate", |s, _| {
            let m = s.map_mut();
            m.camera.set_bearing(0.0);
            m.camera.set_pitch(0.0);
            s.record_extent();
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("map.setScale", "Set Scale", "Status Bar", |s, p| {
            let sc = f(p, "scale").ok_or("scale is required")?;
            s.map_mut().set_scale(sc);
            s.record_extent();
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"scale": number}"#),
        C::new("map.basemap", "Basemap", "Home › Add", basemap).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r##"{"name": "paper"|"midnight"|"ocean"|"atlas"|"blank", "keepTiles"?: bool (keeps the web tile basemaps over it), "color"?: "#RRGGBB" (blank: the paper's colour, drawn where a blank map draws white, the tilted view's sky included; null or #FFFFFF: white; default: as it was. Another basemap takes the paper off and keeps its own colours; a color given with it is said as a warning)}"##),
        C::new("map.graticule", "Graticule", "View › Display", graticule).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r##"{"on"?: bool (toggles when omitted, on when a type or crs is given, kept as it is when only onTop or color is given), "type"?: "degrees"|"measured"|"utm"|"mgrs", "crs"?: a projected system for the measured grid (EPSG:2056, WKT or a PROJ string; "" or null: the map's own when projected, else the UTM zone at the view's centre), "onTop"?: bool (the lines over the layers, under the labels; false or null: with the basemap, under the layers), "color"?: "#RRGGBB"|"#RRGGBBAA" ("" or null: the basemap's)}"##),
        C::new("map.reloadWebLayers", "Reload Web Layers", "View › Display", |_, _| {
            // The map view draws again when the tile cache's generation moves.
            crate::render::rustgis_tiles::cache::clear();
            Ok(json!({"generation": crate::render::rustgis_tiles::cache::generation()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("map.projection", "Globe", "View › View", |s, p| {
            // A preset's parameters, over those of `type` or of the map's own preset.
            let mut given: Vec<(&str, f64)> = Vec::new();
            for k in ["lon_0", "lat_1", "lat_2"] {
                if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
                    given.push((k, v.as_f64().ok_or_else(|| format!("{k} is a number of degrees"))?));
                }
            }
            let m = s.map_mut();
            let next = if given.is_empty() {
                match ps(p, "type") {
                    Some(t) => Projection::from_code(t)?,
                    None if m.projection.is_globe() => Projection::WebMercator,
                    None => Projection::Globe,
                }
            } else {
                with_preset_params(&ps(p, "type").map_or_else(|| m.projection.code(), str::to_string), given)?
            };
            m.set_projection(next);
            let out = json!({"projection": m.projection.code(), "label": m.projection.label()});
            if s.auto_cache {
                super::workspace::fill_cache(s);
            }
            Ok(out)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"type"?: "mercator"|"globe"|preset id (with parameters: "robinson +lon_0=150")|"EPSG:<code>"|"ESRI:<code>" (toggles the globe when omitted), "lon_0"?: degrees (a preset's central meridian), "lat_1"?, "lat_2"?: degrees (a conic preset's standard parallels; these apply to the map's own preset when type is omitted)}"#),
        C::new("map.projections", "Projections", "View › View", |_, _| {
            let mut v = vec![json!({"id": "mercator", "label": "Web Mercator"}), json!({"id": "globe", "label": "Globe"})];
            v.extend(rustgis_map::projection::PRESETS.iter().map(|(id, label, _)| json!({"id": id, "label": label})));
            Ok(Value::Array(v))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("bookmark.add", "New Bookmark", "Home › Navigate › Bookmarks", add_bookmark).inputs(map_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"name"?: string (a number is added when another bookmark has it), "description"?: string, "map"?: name (default: the active map; its current view)}"#,
        ),
        C::new("bookmark.go", "Go To Bookmark", "Home › Navigate › Bookmarks", go_bookmark).inputs(bookmark_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"name": string, "map"?: name (the bookmark's map), "pan"?: bool (keep the current scale)}"#),
        C::new("bookmark.remove", "Remove Bookmark", "Home › Navigate › Bookmarks", |s, p| {
            let (mi, bi) = bookmark_at(s, p)?;
            if let Some(m) = s.project.maps.get_mut(mi) {
                m.bookmarks.remove(bi);
            }
            Ok(Value::Null)
        }).inputs(bookmark_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string, "map"?: name (default: the active map)}"#),
        C::new("bookmark.rename", "Rename Bookmark", "Bookmarks", |s, p| {
            let new = ps(p, "newName").map(str::trim).filter(|n| !n.is_empty()).ok_or("newName is required")?;
            let new = super::within("newName", "a bookmark's name", new, MAX_BOOKMARK_NAME)?;
            let (mi, bi) = bookmark_at(s, p)?;
            let name = unique_bookmark_name(s, mi, Some(bi), &new);
            if let Some(b) = s.project.maps.get_mut(mi).and_then(|m| m.bookmarks.get_mut(bi)) {
                b.name = name.clone();
            }
            super::say_name_cut(s, "Rename Bookmark", "the name given", &new, &name);
            Ok(json!({"name": name}))
        }).inputs(bookmark_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string, "newName": string (a number is added when another bookmark has it), "map"?: name}"#),
        C::new("bookmark.update", "Update Bookmark", "Bookmarks", |s, p| {
            let (mi, bi) = bookmark_at(s, p)?;
            // The view it takes is the one on screen: another map's would be a view not seen.
            if mi != s.project.active_map {
                return Err("bring the bookmark's map into view to update it".into());
            }
            if let Some(m) = s.project.maps.get_mut(mi) {
                let camera = m.camera;
                if let Some(b) = m.bookmarks.get_mut(bi) {
                    b.camera = camera;
                }
            }
            Ok(Value::Null)
        }).inputs(bookmark_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string (it takes the current view; its map must be in view), "map"?: name}"#),
        C::new("bookmark.describe", "Bookmark Description", "Bookmarks", |s, p| {
            let text = super::within("description", "a bookmark's description", ps(p, "description").ok_or("description is required")?.trim(), MAX_DESCRIPTION)?;
            let (mi, bi) = bookmark_at(s, p)?;
            if let Some(b) = s.project.maps.get_mut(mi).and_then(|m| m.bookmarks.get_mut(bi)) {
                b.description = text;
            }
            Ok(Value::Null)
        }).inputs(bookmark_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string, "description": string (empty clears it), "map"?: name}"#),
        C::new("bookmark.move", "Move Bookmark", "Bookmarks", |s, p| {
            let to = super::uint(p, "to").ok_or("to is required (an index)")?;
            let (mi, bi) = bookmark_at(s, p)?;
            if let Some(m) = s.project.maps.get_mut(mi) {
                let b = m.bookmarks.remove(bi);
                let to = usize::try_from(to).unwrap_or(usize::MAX).min(m.bookmarks.len());
                m.bookmarks.insert(to, b);
            }
            Ok(Value::Null)
        }).inputs(bookmark_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string, "to": index (0: first; past the end: last), "map"?: name}"#),
        C::new("bookmark.save", "Export Bookmarks", "Home › Navigate › Bookmarks", |s, p| super::items::save_item(s, "bookmarks", p)).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"path": "*.rgisbookmarks"}"#),
        C::new("bookmark.import", "Import Bookmarks", "Home › Navigate › Bookmarks", import_bookmarks).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .params(r#"{"path"?: "*.rgisbookmarks", "text"?: bookmarks JSON, "data"?: base64 file}"#),
        C::new("map.new", "New Map", "Home › Project", new_map).transition(unmodelled_transition).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"name"?: string}"#),
        C::new("map.activate", "Activate Map", "Browser", |s, p| {
            let choice = activation(&super::InputSource::input_context(&*s), p)?;
            let choice = choice.ok_or("no activation choice")?;
            s.project.active_map = choice.map;
            s.active_layer = choice.layer;
            s.table_layer = None;
            Ok(Value::Null)
        }).inputs(required_map_inputs).transition(activation).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string}"#)
        .view(),
        C::new("map.rename", "Rename Map", "Layers", |s, p| {
            let (i, name) = rename_choice(&super::InputSource::input_context(&*s), p)?;
            let old = s.project.maps.get(i).map(|m| m.name.clone()).ok_or("no map")?;
            super::workspace::bind_offline_map_before_rename(&mut s.project, i)?;
            if let Some(m) = s.project.maps.get_mut(i) {
                m.name = name.clone();
            }
            follow_renamed_map(s, &old, &name);
            super::say_name_cut(s, "Rename Map", "the name given", s_name(p)?.trim(), &name);
            Ok(json!({"name": name}))
        }).transition(rename_transition).inputs(map_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string (the new name; a number is added when another map has it), "map"?: name (default: the active map)}"#),
        C::new("map.duplicate", "Duplicate Map", "Browser", duplicate_map).transition(unmodelled_transition).inputs(duplicate_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"map"?: name (default: the active map), "name"?: string (default: "<map> copy"; a number is added when another map has it)}"#),
        C::new("map.remove", "Delete Map", "Browser", |s, p| {
            if s.project.maps.len() < 2 {
                return Err("a project keeps at least one map".into());
            }
            let i = required_map_at(s,p)?;
            s.project.maps.remove(i);
            // Deleting another map keeps the one in view (a map tab's close button).
            let active = s.project.active_map;
            if i < active {
                s.project.active_map = active - 1;
            } else if i == active {
                s.project.active_map = 0;
                s.active_layer = s.map().layers.first().map(|l| l.id);
                s.table_layer = None;
            }
            Ok(Value::Null)
        }).transition(unmodelled_transition).inputs(required_map_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string}"#),
        // The order of the map tabs and the Browser's maps (#2802); the map in view stays in view.
        C::new("map.move", "Move Map", "Browser", |s, p| {
            let from = required_map_at(s, p)?;
            let to = super::move_item(s, |s| &mut s.project.maps, from, p)?;
            s.project.active_map = super::after_move(s.project.active_map, from, to);
            Ok(Value::Null)
        }).transition(unmodelled_transition).inputs(required_map_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": string, "to": index (0: first; past the end: last)}"#),
    ]
}

/// Projection `base`, a preset with or without parameters, with `given` over its own values.
fn with_preset_params(base: &str, given: Vec<(&str, f64)>) -> Result<Projection, String> {
    use rustgis_map::projection::{parse_preset, preset_code};
    let Some((id, mut params)) = parse_preset(base)? else {
        let name = Projection::from_code(base).map_or_else(|_| base.to_string(), |p| p.label());
        return Err(format!("only presets take lon_0, lat_1 and lat_2, not {name}"));
    };
    for (k, v) in given {
        params.retain(|(q, _)| *q != k);
        params.push((k, v));
    }
    Projection::from_code(&preset_code(id, &params))
}

fn s_name(p: &Value) -> Result<&str, String> {
    ps(p, "name").ok_or_else(|| "name is required".to_string())
}

/// The map `map` names (default: the active one). A `map` that isn't a name is an error, never
/// the active map.
pub(crate) fn map_at(input: &impl super::InputSource, p: &Value) -> Result<usize, String> {
    let context = input.input_context();
    let s = &context;
    match p.get("map") {
        None | Some(Value::Null) => Ok(s.active_map),
        Some(Value::String(m)) => s.project.maps.iter().position(|x| x.name.eq_ignore_ascii_case(m)).ok_or_else(|| format!("no map '{m}'")),
        Some(_) => Err("map must be a map's name".into()),
    }
}

/// A full copy of a map (#288), its data included, right after it and in view. Layer ids are per
/// map, so the copy keeps them; layouts aren't copied and keep showing the original.
fn duplicate_map(s: &mut Session, p: &Value) -> CmdResult {
    let i = map_at(s, p)?;
    // A name given past the limit is refused, as map.rename refuses it, not cut (#2639).
    let given = super::name_given(p, "a map's name", super::MAX_NAME)?;
    let mut m = s.project.maps.get(i).ok_or("no map")?.clone();
    let (name, cut) =
        super::copy_name("Duplicate Map", given.as_deref(), &m.name, |n| s.project.maps.iter().any(|x| x.name.eq_ignore_ascii_case(n)));
    m.name = name;
    m.offline_area_id = None;
    m.label_lock = None;
    m.label_lock_line_anchors.clear();
    m.renew_instance_id();
    for l in &mut m.layers {
        l.renew_instance_id();
        l.selection.clear();
    }
    let name = m.name.clone();
    let was_active = i == s.project.active_map;
    s.project.maps.insert(i + 1, m);
    s.project.active_map = i + 1;
    // The copy of the map in view keeps its active layer (same ids); another starts on its first.
    if !was_active {
        s.active_layer = s.map().layers.first().map(|l| l.id);
        s.table_layer = None;
    }
    if let Some(cut) = cut {
        s.notify(cut, false);
    }
    Ok(json!({"name": name}))
}

/// Points layout map frames, presentation map pages and offline areas at a renamed map (#287),
/// so they keep finding it.
pub(crate) fn follow_renamed_map(s: &mut Session, old: &str, new: &str) {
    let renamed = |m: &mut String| {
        if m.eq_ignore_ascii_case(old) {
            *m = new.to_string();
        }
    };
    for e in s.project.layouts.iter_mut().flat_map(|l| l.elements.iter_mut()) {
        if let ElementKind::MapFrame { map, .. } = &mut e.kind {
            renamed(map);
        }
    }
    for page in s.project.presentations.iter_mut().flat_map(|p| p.pages.iter_mut()) {
        if let PageKind::Map { map, .. } = &mut page.kind {
            renamed(map);
        }
    }
    for a in &mut s.project.offline_areas {
        renamed(&mut a.map);
    }
}

fn zoom_full(s: &mut Session, _: &Value) -> CmdResult {
    let ids = s.shown_layer_ids();
    if s.zoom_to_layers(&ids, rustgis_map::ExtentOf::Shown).is_none() {
        s.zoom_to(&Extent::new(-170.0, -58.0, 180.0, 78.0));
    }
    Ok(Value::Null)
}

fn zoom_to_extent(s: &mut Session, p: &Value) -> CmdResult {
    let e = lonlat_extent(p.get("extent")).ok_or("extent is [minLon, minLat, maxLon, maxLat], west before east and south before north")?;
    s.zoom_to(&e);
    Ok(Value::Null)
}

/// A longitude/latitude extent `[minLon, minLat, maxLon, maxLat]` within ±180/±90, not empty.
pub fn lonlat_extent(v: Option<&Value>) -> Option<Extent> {
    let n: Vec<f64> = v?.as_array()?.iter().map(Value::as_f64).collect::<Option<_>>()?;
    let [x0, y0, x1, y1] = n.as_slice() else { return None };
    let ok = |x: f64, lim: f64| x.is_finite() && (-lim..=lim).contains(&x);
    (ok(*x0, 180.0) && ok(*x1, 180.0) && ok(*y0, 90.0) && ok(*y1, 90.0) && x0 < x1 && y0 < y1).then(|| Extent::new(*x0, *y0, *x1, *y1))
}

fn zoom(s: &mut Session, factor: f64) -> CmdResult {
    let (w, h) = s.view_size;
    s.map_mut().camera.zoom_at(factor, w / 2.0, h / 2.0, w, h);
    s.record_extent();
    Ok(Value::Null)
}

/// A place typed as the search box reads one: latitude, then longitude, in decimal degrees,
/// degrees and minutes or degrees, minutes and seconds (`40.7128, -74.006`,
/// `40°42'46"N 74°0'22"W`), or on the Earth's map a UTM position or an MGRS reference
/// (`18north 583959 4507351`, `18TWL8395907350`). A reference that names no place says why, as
/// the search box does. What the reading assumed comes with the place, for a notice
/// ([`PositionRead`]).
pub(crate) fn read_position(map: &Map, text: &str) -> Result<(Coord, PositionRead), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the position is empty".into());
    }
    if text.chars().nth(super::search::MAX_CHARS).is_some() {
        return Err(format!("the position is longer than {} characters", super::search::MAX_CHARS));
    }
    if let Some(c) = super::search::parse_lat_lon(text) {
        // Without N, S, E or W the order is the reader's assumption, so it is said.
        let lettered = text.chars().any(|c| matches!(c.to_ascii_uppercase(), 'N' | 'S' | 'E' | 'W'));
        return Ok((c, if lettered { PositionRead::AsWritten } else { PositionRead::Ordered }));
    }
    // Two plain decimal numbers (`40.7, 200`) that aren't a latitude and a longitude: out of
    // range, not misspelled.
    let plain = |w: &&str| {
        let digits = w.strip_prefix(['-', '+']).unwrap_or(w);
        digits.chars().any(|c| c.is_ascii_digit())
            && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
            && digits.chars().filter(|&c| c == '.').count() <= 1
    };
    let words: Vec<&str> = text.split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()).collect();
    if let Some(read) = read_system_position(map, text) {
        return read.map(|c| (c, PositionRead::AsWritten));
    }
    if words.len() == 2 && words.iter().all(plain) {
        return Err("latitude must be within ±90 and longitude within ±180".into());
    }
    let degrees = "write a position as latitude, then longitude (40.7128, -74.006 or 40°42'46\"N 74°0'22\"W)";
    if !map.body().is_earth() {
        // A grid reference is told why; anything else how a place is written here.
        return Err(match super::search::read_grid_reference(text) {
            Err(rustgis_geom::GridError::Syntax) => degrees.into(),
            _ => "UTM and MGRS are the Earth's grids and name no place on this map; write a position as latitude, then longitude".into(),
        });
    }
    match super::search::read_grid_reference(text) {
        Ok(c) => Ok((
            c,
            rustgis_geom::parse_mgrs(text)
                .ok()
                .filter(|m| m.digits < 5)
                .map_or(PositionRead::AsWritten, |m| PositionRead::Square(m.precision_m())),
        )),
        Err(rustgis_geom::GridError::Syntax) => Err(format!("{degrees}, UTM (18north 583959 4507351) or MGRS (18TWL8395907350)")),
        Err(e) => Err(format!("no place at this grid reference: {e}")),
    }
}

/// A coordinate system a place's coordinates are given in, ready to give the place's longitude and
/// latitude.
pub(crate) enum System {
    /// A map's own (a flat projection, as the map draws it).
    Flat(rustgis_map::projection::Flat),
    /// A system as data names it.
    Data(rustgis_data::raster::Transformer),
}

impl System {
    /// The system data names `crs`: Web Mercator as the map draws it, whose unit is the metre
    /// (its conversion gives no parameters to read one from), else by its conversion.
    fn of_crs(crs: rustgis_data::raster::Crs) -> Result<System, String> {
        if crs == rustgis_data::raster::Crs::WebMercator {
            return Ok(System::Flat(rustgis_map::projection::Flat::Mercator));
        }
        if let Some(e) = axes_refused(crs) {
            return Err(e);
        }
        crs.transformer().map(System::Data)
    }

    /// The unit it counts in as the readout writes it: `m`, `ft` (international) or `ftUS`; `None`
    /// for another (degrees, a rarer foot).
    pub(crate) fn unit(&self) -> Option<&'static str> {
        let to_meter = match self {
            System::Flat(rustgis_map::projection::Flat::Mercator) => 1.0,
            System::Flat(rustgis_map::projection::Flat::Custom(p)) => p.to_meter(),
            System::Data(t) => {
                // Degrees aren't a length.
                let p = t.params()?;
                if p.method == "longlat" || p.method == "latlong" {
                    return None;
                }
                p.to_meter
            }
        };
        [("m", 1.0), ("ft", 0.3048), ("ftUS", 1200.0 / 3937.0)].into_iter().find(|(_, k)| (to_meter - k).abs() < 1e-9).map(|(u, _)| u)
    }

    /// Whether it counts in degrees (a geographic system): a map's own never does.
    fn is_geographic(&self) -> bool {
        matches!(self, System::Data(t) if t.is_geographic())
    }

    /// The longitude and latitude of `at`, a place in this system; `None` where it names none:
    /// where the inverse gives no place, or one that doesn't map back to `at` (to a centimetre, or
    /// a millionth of the coordinate's size, up to two of its units) because the system's formulas don't reach so far.
    pub(crate) fn to_lonlat(&self, at: Coord) -> Option<Coord> {
        if !at.is_finite() {
            return None;
        }
        let (c, back) = match self {
            System::Flat(f) => {
                let c = f.inverse(at)?;
                (c, f.forward_checked(c))
            }
            System::Data(t) => {
                let c = t.to_lonlat(at)?;
                (c, t.from_lonlat(c))
            }
        };
        let back = back?;
        // To a hundredth of the system's unit (a centimetre in metres), or a millionth of the
        // coordinate's size up to two units (2 m, 2 ft): an inverse that
        // iterates (Robinson's, to about 1.5e-5°, some 1.6 m) comes back that close; a place past
        // the formulas' reach doesn't.
        let close = |a: f64, b: f64| (a - b).abs() <= 0.01_f64.max((a.abs() * 1e-6).min(2.0));
        (c.is_finite() && (-180.0..=180.0).contains(&c.x) && (-90.0..=90.0).contains(&c.y) && close(back.x, at.x) && close(back.y, at.y))
            .then_some(c)
    }
}

/// Why a system X and Y can't be given in: they are an easting and a northing, and one counted
/// west or south, or northing first (Lo29's westing and southing), would take them mirrored or
/// swapped, so it is refused, as the readout refuses to write in one. `None` for one counted east,
/// then north, and for degrees (their own branch reads them).
fn axes_refused(crs: rustgis_data::raster::Crs) -> Option<String> {
    let axes = crs.axis_directions();
    (!crs.is_geographic() && axes != [(1.0, 0.0), (0.0, 1.0)]).then(|| {
        // The direction's larger component names it (`signum` of 0 is 1, so it can't).
        let name = |(x, y): (f64, f64)| match (x.abs() >= y.abs(), x > 0.0, y > 0.0) {
            (true, true, _) => "an easting",
            (true, false, _) => "a westing",
            (false, _, true) => "a northing",
            (false, _, false) => "a southing",
        };
        format!(
            "{} counts {}, then {}, not an easting, then a northing: give the place in degrees, or in a system counted east, then north",
            crs.label(),
            name(axes[0]),
            name(axes[1])
        )
    })
}

/// The system a layer's coordinates are given in, as Go To XY reads them: a raster's, its
/// pixels' (also one only assumed, as its pixels are placed in it); a layer of features', the one
/// it was read in. `None` for a layer of features that names none (longitude and latitude), and
/// for a raster with no pixels loaded, whose system is unknown ([`system_of`] refuses it, so check
/// that before taking `None` for degrees). Not
/// [`super::project::layer_system`], which leaves assumed systems out as the project's own.
pub fn layer_coordinates_system(l: &rustgis_map::Layer) -> Option<rustgis_data::raster::Crs> {
    match &l.raster {
        Some(r) => r.data.as_ref().map(|d| d.crs),
        None => l.dataset.crs,
    }
}

/// The system `name` gives: `map`, the map's own (the flat projection it draws in, not the globe
/// or another body's map); a layer of the map by that name, the system it was read in (longitude
/// and latitude for one that names none); else a code or other text a map's projection or data's
/// system takes (`EPSG:2056`, `Web Mercator`, `robinson +lon_0=150`, a PROJ string), which names
/// an Earth system, so none on another body's map (as [`read_system_position`] refuses it).
pub(crate) fn system_of(map: &Map, name: &str) -> Result<System, String> {
    let name = name.trim();
    if name.eq_ignore_ascii_case("map") {
        if !map.body().is_earth() || map.projection.is_globe() {
            return Err("the map has no flat system of its own here (the globe, or another body's map): give degrees".into());
        }
        return Ok(System::Flat(map.flat()));
    }
    // A layer's name first, so a layer named as a system (`Albers`, `2056`) means the layer.
    if let Some(l) = map.layer_by_name(name) {
        // A raster with no pixels (still being read, or its file couldn't be) names no system: its
        // system is unknown, not longitude and latitude.
        if l.raster.as_ref().is_some_and(|r| r.data.is_none()) {
            return Err(format!("layer '{name}' has no pixels loaded, so its system is unknown: wait until it has opened, or reload it"));
        }
        // A feature layer that names no system holds longitude and latitude.
        let crs = layer_coordinates_system(l).unwrap_or(rustgis_data::raster::Crs::LonLat);
        // A projection of the Earth names no place on another body's map, as its code doesn't;
        // degrees are that map's own, and another body's projection is its own.
        if !map.body().is_earth() && !crs.is_geographic() && !crs.is_planetary() {
            return Err(format!("layer '{name}' is in {}, a system of the Earth, and names no place on this map", crs.label()));
        }
        // Nor does another body's projection on the Earth's map: its inverse gives that body's
        // degrees, not the Earth's. (Its degrees are read as they are, as data reads them.)
        if map.body().is_earth() && !crs.is_geographic() && crs.is_planetary() {
            return Err(format!("layer '{name}' is in {}, a system of another body, and names no place on the Earth's map", crs.label()));
        }
        // Nor one body's projection on another's: Mars's inverse gives no place on the Moon.
        if let Some(body) = other_body(map, crs.planetary_semi_major()).filter(|_| !crs.is_geographic()) {
            return Err(format!("layer '{name}' is in {}, a system of {body}, and names no place on this map", crs.label()));
        }
        return System::of_crs(crs).map_err(|e| format!("layer '{name}': {e}"));
    }
    if let Some(named) = named_system(name) {
        let (system, semi_major) = named?;
        let planetary = semi_major.is_some();
        // An Earth system names no place on another body's map; that body's own does. Another
        // body's projection names none on the Earth's.
        if !map.body().is_earth() && !planetary {
            return Err(format!("{name} is a system of the Earth and names no place on this map"));
        }
        if map.body().is_earth() && planetary && !system.is_geographic() {
            return Err(format!("{name} is a system of another body and names no place on the Earth's map"));
        }
        if let Some(body) = other_body(map, semi_major).filter(|_| !system.is_geographic()) {
            return Err(format!("{name} is a system of {body} and names no place on this map"));
        }
        return Ok(system);
    }
    Err(format!("'{name}' is neither \"map\", a coordinate system (EPSG:2056), nor a layer's name"))
}

/// The body, said for a notice, that a system of another body than the Earth's
/// (`semi_major`, [`rustgis_data::raster::Crs::planetary_semi_major`]) is of, when the map is of
/// yet another body: their semi-major axes differ by more than a percent (the nearest pair of
/// bodies a map takes, Titan and Ganymede, differ by 2%). `None` on the Earth's map, for the
/// Earth's systems, and for the map's own body's.
fn other_body(map: &Map, semi_major: Option<f64>) -> Option<String> {
    let body = map.body();
    let a = semi_major.filter(|_| !body.is_earth())?;
    if (a - body.a).abs() <= body.a * 0.01 {
        return None;
    }
    let named = rustgis_geom::body::BODIES.iter().find(|b| !b.is_earth() && (a - b.a).abs() <= b.a * 0.01);
    Some(named.map_or_else(|| format!("another body (a radius of {:.0} km)", a / 1000.0), |b| b.name.to_string()))
}

/// A system named by a map projection's code (as the readout's copy names it) or by text data's
/// systems take, and whether it is another body's than the Earth's (a planetary PROJ string or
/// code); `None` for text that names neither, an error for one named but refused (counted
/// otherwise than east, then north). The globe names no flat system.
fn named_system(name: &str) -> Option<Result<(System, Option<f64>), String>> {
    // `map` is the map's own ([`system_of`]), not Web Mercator, which `Projection::from_code`
    // takes it for.
    if name.trim().eq_ignore_ascii_case("map") {
        return None;
    }
    // A code a map projection takes is read as the map draws it, but counted as data names it:
    // its axes and its body are the system's.
    let crs = rustgis_data::raster::Crs::parse(name).ok();
    match Projection::from_code(name) {
        Ok(Projection::Globe) => None,
        Ok(p) => Some(match crs.and_then(axes_refused) {
            Some(e) => Err(e),
            None => Ok((System::Flat(rustgis_map::projection::Flat::of(&p)), crs.and_then(|c| c.planetary_semi_major()))),
        }),
        Err(_) => {
            let c = crs?;
            Some(System::of_crs(c).map(|s| (s, c.planetary_semi_major())))
        }
    }
}

/// A place written in a named system, as the map's right-click menu copies one: an easting and a
/// northing, each with an optional unit and axis letter (`2600000 mE`, `1200000 mN`; `ftE`,
/// `ftUSN`, a bare `E`), then the system (`EPSG:2056`, `Web Mercator`). `None` for text not
/// written so; an error for a system that names no flat Earth system here, or a place it names
/// none at.
pub(crate) fn read_system_position(map: &Map, text: &str) -> Option<Result<Coord, String>> {
    let mut rest = text.trim();
    let mut numbers = [0.0; 2];
    let mut units: Vec<&str> = Vec::new();
    for (i, axis) in ['E', 'N'].into_iter().enumerate() {
        numbers[i] = next_word(&mut rest)?.parse::<f64>().ok().filter(|v| v.is_finite())?;
        // An optional unit and axis letter.
        let mut after = rest;
        let word = next_word(&mut after).unwrap_or("");
        if let Some(unit) =
            ["ftUS", "ft", "m", ""].into_iter().find(|u| word.strip_prefix(u).is_some_and(|l| l.len() == 1 && l.starts_with(axis)))
        {
            rest = after;
            if !unit.is_empty() {
                units.push(unit);
            }
        }
    }
    let system = rest.trim_start_matches(|c: char| c == ',' || c.is_whitespace()).trim();
    // Only a system's name: a letter, so a third number (a height) isn't taken for one; and no
    // WKT, which typed text would otherwise add to the table of systems files read.
    if !system.chars().next().is_some_and(char::is_alphabetic) || system.contains('[') {
        return None;
    }
    let (found, semi_major) = if system.eq_ignore_ascii_case("map") {
        match system_of(map, "map") {
            Ok(s) => (s, None),
            Err(e) => return Some(Err(e)),
        }
    } else {
        match named_system(system)? {
            Ok(found) => found,
            Err(e) => return Some(Err(e)),
        }
    };
    // An Earth system names no place on another body's map; that body's own does. Another
    // body's projection names none on the Earth's.
    let planetary = semi_major.is_some();
    if !map.body().is_earth() && !planetary {
        return Some(Err(format!("{system} is a system of the Earth and names no place on this map")));
    }
    if map.body().is_earth() && planetary && !found.is_geographic() {
        return Some(Err(format!("{system} is a system of another body and names no place on the Earth's map")));
    }
    if let Some(body) = other_body(map, semi_major).filter(|_| !found.is_geographic()) {
        return Some(Err(format!("{system} is a system of {body} and names no place on this map")));
    }
    // Degrees are written latitude first, as a position without a system reads them, while this
    // form takes an easting first: the order would be a guess, so a system in degrees is refused.
    if found.is_geographic() {
        return Some(Err(format!(
            "{system} counts in degrees: give the position without a system, latitude then longitude (or with N, S, E and W)"
        )));
    }
    // A unit written must be the system's: feet aren't read as metres.
    if let Some(unit) = units.iter().find(|u| Some(**u) != found.unit()) {
        let theirs = found.unit().unwrap_or("another unit");
        return Some(Err(format!("{system} counts in {theirs}, not {unit}")));
    }
    let at = Coord::new(numbers[0], numbers[1]);
    // Two numbers that read as a latitude and a longitude before a projected system (`map`, or
    // a code) are degrees typed with its name, not a place a few steps from its origin: said, not
    // taken for its units. Only numbers without a unit: `50 mE 30 mN map` says it is metres.
    let degrees = at.x.abs() <= 180.0 && at.y.abs() <= 180.0 && (at.x.abs() <= 90.0 || at.y.abs() <= 90.0);
    if degrees && units.is_empty() {
        // With a unit it counts in, how to say these are its units after all.
        let hint =
            found.unit().map(|u| format!(", or write its unit after each number ({} {u}E  {} {u}N)", at.x, at.y)).unwrap_or_default();
        return Some(Err(format!(
            "{} {} {system} reads as degrees, not as a place in {system}: give degrees without a system, latitude then longitude{hint}",
            at.x, at.y
        )));
    }
    Some(found.to_lonlat(at).ok_or_else(|| format!("{} {} names no place in {system}", at.x, at.y)))
}

/// The next word of `rest` (words are parted by spaces and commas), moving past it.
fn next_word<'a>(rest: &mut &'a str) -> Option<&'a str> {
    let t = rest.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
    let end = t.find(|c: char| c == ',' || c.is_whitespace()).unwrap_or(t.len());
    let (w, r) = t.split_at(end);
    *rest = r;
    (!w.is_empty()).then_some(w)
}

/// What reading a typed position assumed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PositionRead {
    /// Nothing: hemisphere letters, a UTM position or an MGRS reference to the meter.
    AsWritten,
    /// Two numbers without N, S, E or W, taken as latitude, then longitude (or the other way
    /// round when the first can only be a longitude): the order is said, so `2.35, 48.85` typed
    /// as x, y shows where it went.
    Ordered,
    /// An MGRS reference coarser than a meter, which names a square of this side in meters: the
    /// place is its center.
    Square(f64),
}

/// One axis typed on its own, as By Coordinates' Longitude (X) and Latitude (Y) boxes take it:
/// decimal degrees (`-74.006`), or degrees, minutes and seconds with or without marks and a
/// hemisphere letter before or after (`74°0′22″W`, `W 74 0 22`, `40 42 46.1 N`). A letter of the
/// other axis (N in the longitude) is refused rather than taken as the other axis.
pub(crate) fn read_axis(text: &str, latitude: bool) -> Result<f64, String> {
    let name = if latitude { "latitude" } else { "longitude" };
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("the {name} is empty"));
    }
    if text.chars().nth(64).is_some() {
        return Err(format!("the {name} is longer than 64 characters"));
    }
    let (own, other) = if latitude { (['N', 'S'], ['E', 'W']) } else { (['E', 'W'], ['N', 'S']) };
    // A letter at either end is a hemisphere only in text that is otherwise a number: a word
    // ending in N, S, E or W (`Paris`, `Lisbon`) gets the usage message, not a refusal as the
    // other axis.
    let hemispheres_only = text.chars().all(|c| !c.is_ascii_alphabetic() || matches!(c.to_ascii_uppercase(), 'N' | 'S' | 'E' | 'W'));
    let mut hemisphere = None;
    let mut rest = text;
    for at_end in [false, true] {
        let c = if at_end { rest.chars().next_back() } else { rest.chars().next() };
        if let Some(c) = c.map(|c| c.to_ascii_uppercase())
            && hemispheres_only
            && (own.contains(&c) || other.contains(&c))
        {
            if other.contains(&c) {
                return Err(format!("{c} belongs to the {}; write the {name} here", if latitude { "longitude" } else { "latitude" }));
            }
            if hemisphere.replace(c).is_some() {
                return Err(format!("the {name} has two hemisphere letters"));
            }
            // The letter is ASCII, one byte.
            rest = if at_end { rest.get(..rest.len().saturating_sub(1)) } else { rest.get(1..) }.unwrap_or("").trim();
        }
    }
    // One value a box: a comma is a decimal comma or a second value, neither of which is read here.
    if rest.contains(',') {
        return Err(format!("the {name} box takes one value, with a point for decimals (-74.006, not -74,006 or two numbers)"));
    }
    let usage = || format!("write the {name} in degrees (-74.006) or degrees, minutes and seconds (74°0′22″W)");
    // Each number goes where its mark says (° degrees, ′ minutes, ″ seconds), so `40°30″` is 40°
    // and 30 seconds, not 30 minutes; a number without a mark takes the place after the last.
    let mark = |c: char| match c {
        '°' | 'º' => Some(0),
        '\'' | '′' | '’' => Some(1),
        '"' | '″' | '”' => Some(2),
        _ => None,
    };
    let mut places: [Option<f64>; 3] = [None; 3];
    let mut next = 0usize;
    let mut number = String::new();
    let mut last: Option<usize> = None;
    // A number, once read, waits for a mark that may follow it; it is placed when the next number
    // starts or the text ends.
    let mut pending: Option<f64> = None;
    let mut place = |v: f64, at: Option<usize>, next: &mut usize, last: &mut Option<usize>| -> Result<(), String> {
        let at = at.unwrap_or(*next);
        let slot = places.get_mut(at).ok_or_else(usage)?;
        if slot.is_some() || last.is_some_and(|l| at <= l) {
            return Err(format!("the {name}'s degrees, minutes and seconds are out of order"));
        }
        *slot = Some(v);
        *last = Some(at);
        *next = at + 1;
        Ok(())
    };
    let finish = |number: &mut String| -> Result<Option<f64>, String> {
        if number.is_empty() {
            return Ok(None);
        }
        let v: f64 = number.replace('−', "-").parse().map_err(|_| usage())?;
        number.clear();
        if !v.is_finite() {
            return Err(usage());
        }
        Ok(Some(v))
    };
    for c in rest.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | '−') {
            number.push(c);
            continue;
        }
        if let Some(v) = finish(&mut number)? {
            if let Some(p) = pending.take() {
                place(p, None, &mut next, &mut last)?;
            }
            pending = Some(v);
        }
        if let Some(at) = mark(c) {
            let v = pending.take().ok_or_else(usage)?;
            place(v, Some(at), &mut next, &mut last)?;
        } else if !c.is_whitespace() {
            return Err(usage());
        }
    }
    if let Some(p) = pending.take() {
        place(p, None, &mut next, &mut last)?;
    }
    // Degrees come first; a place skipped before a later one is zero (`40°30″` is 40° 0′ 30″).
    let filled = last.ok_or_else(usage)? + 1;
    if places.first().is_none_or(Option::is_none) {
        return Err(usage());
    }
    let nums: Vec<f64> = places.iter().take(filled).map(|v| v.unwrap_or(0.0)).collect();
    // Minutes follow whole degrees and seconds whole minutes (`48.85 2.35` is two values, not
    // degrees and minutes).
    if nums.iter().rev().skip(1).any(|v| v.fract() != 0.0) {
        return Err(usage());
    }
    if hemisphere.is_some() && nums.first().is_some_and(|d| d.is_sign_negative()) {
        return Err(format!("the {name} has both a minus sign and a hemisphere letter"));
    }
    // The degrees, minutes and seconds as the search box sums them.
    let mut v = super::search::axis(&nums).ok_or_else(|| format!("the {name}'s minutes and seconds must be from 0 to under 60"))?;
    if matches!(hemisphere, Some('S' | 'W')) {
        v = -v;
    }
    let limit = if latitude { 90.0 } else { 180.0 };
    if v.abs() > limit {
        return Err(format!("the {name} must be within ±{limit}"));
    }
    Ok(v)
}

/// A number of degrees as typed back to the user: six decimals at most, no trailing zeros.
fn degrees_text(v: f64) -> String {
    let v = (v * 1e6).round() / 1e6;
    if v == 0.0 { "0".into() } else { format!("{v}") }
}

/// The place a command's parameters give, as `map.goToXY` and `map.convertPosition` take it: a
/// `position` written as the search box reads one, or `lon` and `lat` (or `x` and `y`), each a
/// number or text in degrees, minutes and seconds. With it, what the reading assumed, for a
/// notice: two bare numbers taken in an order, or an MGRS square taken at its center (`moving`
/// says whether the map goes there, which the notice's wording follows).
pub(crate) fn typed_place(map: &Map, p: &Value, moving: bool) -> Result<(Coord, Option<crate::notice_text::NoticeText>), String> {
    let position = match p.get("position") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => Some(t.as_str()),
        Some(_) => return Err("position must be text, such as \"40.7128, -74.006\" or \"18TWL8395907350\"".into()),
    };
    let given = |k: &str| !matches!(p.get(k), None | Some(Value::Null));
    let crs = match p.get("crs") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) if !t.trim().is_empty() => Some(t.trim()),
        Some(_) => return Err("crs must be \"map\", a coordinate system such as \"EPSG:2056\", or a layer's name".into()),
    };
    let (lon, lat, notice) = if let Some(text) = position {
        if ["lon", "lat", "x", "y"].iter().any(|k| given(k)) {
            return Err("give a position or lon and lat, not both".into());
        }
        if crs.is_some() {
            return Err("crs goes with x and y; a position names its own system (2600000 mE 1200000 mN EPSG:2056)".into());
        }
        let (c, read) = read_position(map, text)?;
        let reference = || text.trim().to_string().into();
        let said = match read {
            PositionRead::AsWritten => None,
            PositionRead::Ordered => Some(crate::notice_text::NoticeText::format(
                "Read {position} as latitude {lat}, longitude {lon}",
                [("position", reference()), ("lat", degrees_text(c.y).into()), ("lon", degrees_text(c.x).into())],
            )),
            PositionRead::Square(side) => {
                let side = if side >= 1000.0 { format!("{} km", side / 1000.0) } else { format!("{side} m") };
                // Both whole, as the translation catalogs take them.
                let template = if moving {
                    "{reference} names a {side} square, not a point: went to its center"
                } else {
                    "{reference} names a {side} square, not a point: gave its center"
                };
                Some(crate::notice_text::NoticeText::format(template, [("reference", reference()), ("side", side.into())]))
            }
        };
        (c.x, c.y, said)
    } else if let Some(crs) = crs {
        // Numbers in the named system: an easting and a northing, or whatever it counts.
        let axis = |keys: [&str; 2]| -> Result<f64, String> {
            let key = keys.iter().find(|k| given(k)).ok_or_else(|| format!("{} is required (or a position)", keys[1]))?;
            match p.get(*key) {
                Some(Value::String(t)) => t.trim().parse::<f64>().ok(),
                _ => f(p, key),
            }
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("{key} must be a number in {crs}"))
        };
        // Both names of one axis: one would be passed over.
        if ["lon", "x"].iter().all(|k| given(k)) || ["lat", "y"].iter().all(|k| given(k)) {
            return Err("give x and y, or lon and lat, not both".into());
        }
        let at = Coord::new(axis(["lon", "x"])?, axis(["lat", "y"])?);
        let c = system_of(map, crs)?.to_lonlat(at).ok_or_else(|| format!("{} {} names no place in {crs}", at.x, at.y))?;
        (c.x, c.y, None)
    } else {
        // A number, or text in degrees or degrees, minutes and seconds (`74°0′22″W`).
        let axis = |keys: [&str; 2], latitude: bool| -> Result<f64, String> {
            let key = keys.iter().find(|k| given(k)).ok_or_else(|| format!("{} is required (or a position)", keys[0]))?;
            match p.get(*key) {
                Some(Value::String(t)) => t.trim().parse::<f64>().ok().filter(|v| v.is_finite()).map_or_else(|| read_axis(t, latitude), Ok),
                _ => f(p, key).ok_or_else(|| format!("{key} must be a number of degrees, or text such as \"74°0′22″W\"")),
            }
        };
        (axis(["lon", "x"], false)?, axis(["lat", "y"], true)?, None)
    };
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return Err("longitude must be within ±180 and latitude within ±90".into());
    }
    Ok((Coord::new(lon, lat), notice))
}

/// A place in every form RustGIS writes one (#1042): longitude and latitude, in decimal degrees
/// and in degrees, minutes and seconds as the status bar shows them; UTM, or UPS from 84° N
/// and south of 80° S, with the easting and northing (`grid` says which: `utm` and `ups` each hold
/// only their own); and MGRS spaced as the readout writes it and in one word, to `digits` digits
/// (5 by default: 1 m). The place is read as `map.goToXY` reads it, and what the reading assumed
/// is said the same way. The map doesn't move. Every key is always there, `null` where it has no
/// value; on another body's map the Earth's grids are `null` and `note` says why.
fn convert_position(s: &mut Session, p: &Value) -> CmdResult {
    let digits = match p.get("digits") {
        None | Some(Value::Null) => 5,
        // A whole number sent as 2.0 (as Python or JavaScript may) is as good as 2.
        Some(v) => v
            .as_f64()
            .filter(|d| d.fract() == 0.0 && (0.0..=5.0).contains(d))
            .ok_or("digits must be a whole number from 0 (100 km) to 5 (1 m)")? as u8,
    };
    let (c, notice) = typed_place(s.map(), p, false)?;
    // Degrees to 1e-9 (under a millimeter), so text in degrees, minutes and seconds doesn't come
    // back as -74.00600000000001; adding 0.0 turns a -0.0 from a tiny negative into 0. Every form
    // below is made from this one place, so they all agree on its hemisphere.
    let nano = |v: f64| (v * 1e9).round() / 1e9 + 0.0;
    let c = Coord::new(nano(c.x), nano(c.y));
    // Grid meters truncated to the millimeter, as MGRS truncates: rounding could lift a southern
    // northing a hair short of the equator to 10,000,000 m, the equator itself.
    let mm = |v: f64| (v * 1000.0).floor() / 1000.0;
    let mut out = json!({
        "lon": c.x,
        "lat": c.y,
        "decimal": rustgis_geom::format_lonlat(c),
        "dms": rustgis_geom::format_dms(c),
        "grid": null,
        "utm": null,
        "ups": null,
        "zone": null,
        "band": null,
        "hemisphere": null,
        "easting": null,
        "northing": null,
        "mgrs": null,
        "mgrsCompact": null,
        "note": null,
    });
    if !s.map().body().is_earth() {
        out["note"] = json!("UTM, UPS and MGRS are the Earth's grids and name no place on this map");
    } else {
        if let Some(u) = rustgis_geom::to_utm(c) {
            out["grid"] = json!("utm");
            out["utm"] = json!(u.to_string());
            out["zone"] = json!(u.zone);
            out["band"] = json!(u.band.to_string());
            out["hemisphere"] = json!(u.hemisphere());
            out["easting"] = json!(mm(u.easting));
            out["northing"] = json!(mm(u.northing));
        } else if let Some(u) = rustgis_geom::to_ups(c) {
            out["grid"] = json!("ups");
            out["ups"] = json!(u.to_string());
            out["hemisphere"] = json!(u.hemisphere());
            out["easting"] = json!(mm(u.easting));
            out["northing"] = json!(mm(u.northing));
        }
        match rustgis_geom::to_mgrs(c, digits) {
            Some(m) => {
                out["mgrs"] = json!(m.to_string());
                out["mgrsCompact"] = json!(m.compact());
            }
            // Not reached by any place on the Earth today, but said rather than left a bare null.
            None => out["note"] = json!("no MGRS reference names this place"),
        }
    }
    if let Some(said) = notice {
        s.notify_text(said, false);
    }
    Ok(out)
}

pub(crate) fn go_to_xy(s: &mut Session, p: &Value) -> CmdResult {
    // What the reading assumed is said once the map has moved there, not before a refusal.
    let (c, notice) = typed_place(s.map(), p, true)?;
    let (lon, lat) = (c.x, c.y);
    // A scale that can't be used is refused, not dropped.
    let scale = match p.get("scale") {
        None | Some(Value::Null) => None,
        Some(_) => Some(f(p, "scale").filter(|s| *s > 0.0).ok_or("scale must be a positive number, such as 50000 for 1:50,000")?),
    };
    let bearing = match p.get("bearing") {
        None | Some(Value::Null) => None,
        Some(_) => Some(f(p, "bearing").ok_or("bearing must be a number of degrees")?),
    };
    let pitch = match p.get("pitch") {
        None | Some(Value::Null) => None,
        Some(_) => Some(f(p, "pitch").ok_or("pitch must be a number of degrees")?),
    };
    let m = s.map_mut();
    m.camera.center = m.to_map(Coord::new(lon, lat)).ok_or("that point is outside the map's projection")?;
    if let Some(sc) = scale {
        m.set_scale(sc);
    }
    if let Some(b) = bearing {
        m.camera.set_bearing(b);
    }
    if let Some(t) = pitch {
        m.camera.set_pitch(t);
    }
    m.camera.clamp();
    s.record_extent();
    if let Some(said) = notice {
        s.notify_text(said, false);
    }
    Ok(Value::Null)
}

/// The most characters of a map link read.
const MAX_LINK: usize = 4096;

/// A MapLibre map position: zoom (512-pixel tiles), latitude and longitude, bearing and pitch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MapLink {
    pub zoom: f64,
    pub lat: f64,
    pub lon: f64,
    pub bearing: f64,
    pub pitch: f64,
}

/// Reads a map position from a link's fragment, as MapLibre's `hash` option writes it:
/// `#zoom/lat/lon[/bearing[/pitch]]`, or `#name=zoom/lat/lon…` among other `&`-separated
/// parameters. Text without a `#` is read as the fragment itself.
pub(crate) fn parse_link(text: &str) -> Option<MapLink> {
    let text = text.trim();
    // Cut on a character boundary: a hostile link is long and need not be ASCII.
    let end = (0..=MAX_LINK.min(text.len())).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    let text = text.get(..end).unwrap_or("");
    // A web address's position is in its fragment; text with no `#` is a bare position only when
    // it isn't an address (`https://x/?a=1/2/3` is none).
    let fragment = match text.split_once('#') {
        Some((_, f)) => f,
        None if text.contains([':', '?', '=']) => return None,
        None => text,
    };
    // MapLibre's own parameter (`map=`, or the fragment alone) before any other.
    let parts: Vec<(Option<&str>, &str)> =
        fragment.split('&').map(|part| part.split_once('=').map_or((None, part), |(k, v)| (Some(k), v))).collect();
    let ours = parts.iter().filter(|(k, _)| matches!(k, None | Some("map")));
    let others = parts.iter().filter(|(k, _)| !matches!(k, None | Some("map")));
    ours.chain(others).find_map(|&(_, value)| {
        let nums: Vec<f64> = value.split('/').map(|n| n.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect::<Option<_>>()?;
        let (&zoom, &lat, &lon) = (nums.first()?, nums.get(1)?, nums.get(2)?);
        if nums.len() > 5 || !(0.0..=30.0).contains(&zoom) || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return None;
        }
        let (bearing, pitch) = (nums.get(3).copied().unwrap_or(0.0), nums.get(4).copied().unwrap_or(0.0));
        Some(MapLink { zoom, lat, lon, bearing, pitch })
    })
}

/// Whether `text` holds a map position [`parse_link`] reads (the web app asks before going to
/// its own address's fragment, which may be something else).
pub fn is_map_link(text: &str) -> bool {
    parse_link(text).is_some()
}

/// Ground metres per pixel at MapLibre zoom `zoom` and latitude `lat`.
pub(crate) fn ground_at_zoom(zoom: f64, lat: f64) -> f64 {
    let lat = lat.clamp(-rustgis_geom::MAX_LAT, rustgis_geom::MAX_LAT);
    std::f64::consts::TAU * rustgis_geom::MERCATOR_R / (512.0 * zoom.exp2()) * lat.to_radians().cos().abs()
}

/// The map's centre (lon/lat) and MapLibre zoom (512-pixel tiles), whatever its projection;
/// None when its centre is no place on Earth.
pub(crate) fn view_of(map: &Map) -> Option<(Coord, f64)> {
    let flat = map.flat();
    let c = flat.inverse(map.camera.center)?;
    let ground = map.camera.res * flat.ground_per_unit(map.camera.center);
    let merc = ground / c.y.clamp(-rustgis_geom::MAX_LAT, rustgis_geom::MAX_LAT).to_radians().cos().abs().max(1e-9);
    // Within what a link is read back with (a large window shows the world below zoom 0).
    let zoom = (std::f64::consts::TAU * rustgis_geom::MERCATOR_R / (512.0 * merc)).log2().clamp(0.0, 30.0);
    (zoom.is_finite() && c.x.is_finite() && c.y.is_finite()).then_some((c, zoom))
}

/// The map's view as a MapLibre link fragment (`#zoom/lat/lon/bearing/pitch`, the last two left
/// out while zero); None when its centre is no place on Earth.
pub(crate) fn link_of(map: &Map) -> Option<String> {
    let (c, zoom) = view_of(map)?;
    let (bearing, pitch) = (map.camera.bearing, map.camera.pitch);
    let round = |v: f64, places: i32| {
        let k = 10f64.powi(places);
        // + 0.0 turns a rounded -0 into 0.
        (v * k).round() / k + 0.0
    };
    let mut hash = format!("#{}/{}/{}", round(zoom, 2), round(c.y, 5), round(c.x, 5));
    if bearing.abs() >= 0.05 || pitch.abs() >= 0.05 {
        hash.push_str(&format!("/{}", round(bearing, 1)));
    }
    if pitch.abs() >= 0.05 {
        hash.push_str(&format!("/{}", round(pitch, 1)));
    }
    Some(hash)
}

/// Moves the map to a MapLibre-style view: a place, a zoom (512-pixel tiles), a bearing and a
/// pitch, each kept as it is when not given.
pub(crate) fn go_to_view(
    s: &mut Session,
    center: Option<Coord>,
    zoom: Option<f64>,
    bearing: Option<f64>,
    pitch: Option<f64>,
) -> Result<(), String> {
    let m = s.map();
    let flat = m.flat();
    let lonlat = match center {
        Some(c) => c,
        None => flat.inverse(m.camera.center).ok_or("the view shows no part of the Earth")?,
    };
    let ground = match zoom {
        Some(z) => ground_at_zoom(z, lonlat.y),
        None => m.camera.res * flat.ground_per_unit(m.camera.center),
    };
    let mut camera = place_at(m, lonlat, ground).ok_or("that place is outside the map's projection")?;
    if let Some(b) = bearing {
        camera.set_bearing(b);
    }
    if let Some(t) = pitch {
        camera.set_pitch(t);
    }
    s.map_mut().camera = camera;
    s.record_extent();
    Ok(())
}

/// Goes to the place a MapLibre map link shows.
fn go_to_link(s: &mut Session, p: &Value) -> CmdResult {
    let url = ps(p, "url").ok_or("url is required")?;
    let l = parse_link(url).ok_or("the link has no map position (#zoom/lat/lon[/bearing[/pitch]])")?;
    go_to_view(s, Some(Coord::new(l.lon, l.lat)), Some(l.zoom), Some(l.bearing), Some(l.pitch))?;
    Ok(json!({"lon": l.lon, "lat": l.lat, "zoom": l.zoom, "bearing": s.map().camera.bearing, "pitch": s.map().camera.pitch}))
}

/// Tilts the map or the globe.
fn set_pitch(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("pitch").is_some_and(|v| !v.is_null()) && f(p, "pitch").is_none() {
        return Err("pitch must be a number of degrees".into());
    }
    if p.get("by").is_some_and(|v| !v.is_null()) && f(p, "by").is_none() {
        return Err("by must be a number of degrees".into());
    }
    let m = s.map_mut();
    let pitch = match (f(p, "pitch"), f(p, "by")) {
        (Some(t), _) => t,
        (None, Some(by)) => m.camera.pitch + by,
        (None, None) => return Err("pitch (degrees of tilt, 0 to 85) or by (degrees to tilt further) is required".into()),
    };
    m.camera.set_pitch(pitch);
    let out = json!({"pitch": m.camera.pitch});
    s.record_extent();
    Ok(out)
}

/// Turns the map or the globe to a bearing.
fn set_bearing(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("bearing").is_some_and(|v| !v.is_null()) && f(p, "bearing").is_none() {
        return Err("bearing must be a number of degrees".into());
    }
    if p.get("by").is_some_and(|v| !v.is_null()) && f(p, "by").is_none() {
        return Err("by must be a number of degrees".into());
    }
    let m = s.map_mut();
    let bearing = match (f(p, "bearing"), f(p, "by")) {
        (Some(b), _) => b,
        (None, Some(by)) => m.camera.bearing + by,
        (None, None) => return Err("bearing (degrees clockwise from north) or by (degrees to turn) is required".into()),
    };
    if !bearing.is_finite() {
        return Err("bearing must be a number of degrees".into());
    }
    m.camera.set_bearing(bearing);
    let out = json!({"bearing": m.camera.bearing});
    s.record_extent();
    Ok(out)
}

/// Shows or hides the map's graticule, and sets what it draws: a type or a coordinate system
/// given shows it too. A coordinate system is for the measured grid, and refused (naming it) when
/// it can't be read or isn't projected. Whether its lines go over the layers, and their color,
/// are set without showing or hiding it.
fn graticule(s: &mut Session, p: &Value) -> CmdResult {
    use rustgis_map::graticule::GraticuleType;
    let on_top = match p.get("onTop") {
        None => None,
        // As for the color: null puts the default back (under the layers).
        Some(Value::Null) => Some(false),
        Some(v) => Some(super::bool_value(v).ok_or_else(|| format!("onTop must be true or false, not {v}"))?),
    };
    let color = match p.get("color") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(c)) if c.trim().is_empty() => Some(None),
        Some(v) => Some(Some(
            v.as_str()
                .and_then(rustgis_map::Rgba::from_hex)
                .ok_or_else(|| format!("color must be #RRGGBB or #RRGGBBAA (\"\" or null for the basemap's), not {v}"))?,
        )),
    };
    let kind = match p.get("type") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => Some(GraticuleType::from_name(t).ok_or_else(|| {
            let names: Vec<&str> = GraticuleType::ALL.iter().map(|t| t.code()).collect();
            format!("type must be one of {}, not \"{t}\"", names.join(", "))
        })?),
        Some(other) => {
            let names: Vec<String> = GraticuleType::ALL.iter().map(|t| format!("\"{}\"", t.code())).collect();
            return Err(format!("type must be one of {}, not {other}", names.join(", ")));
        }
    };
    let crs = match p.get("crs") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(c)) if c.trim().is_empty() => Some(None),
        Some(Value::String(c)) if c.trim().chars().count() > rustgis_map::layout::MAX_GRID_CRS => {
            return Err(format!("crs: at most {} characters", rustgis_map::layout::MAX_GRID_CRS));
        }
        Some(Value::String(c)) => {
            let read = rustgis_data::raster::Crs::parse(c.trim()).map_err(|e| format!("crs: {e}"))?;
            if read.is_geographic() {
                return Err(format!(
                    "crs: {} is geographic; a measured grid needs a projected system (\"degrees\" draws latitudes and longitudes)",
                    c.trim()
                ));
            }
            if read.is_geocentric() {
                return Err(format!("crs: {} is geocentric; a measured grid needs a projected system", c.trim()));
            }
            // (As the project checks it when it opens: one this version can't project to draws no grid.)
            if let Err(e) = read.transformer() {
                return Err(format!("crs: {} can't be used here: {e}", c.trim()));
            }
            Some(Some(c.trim().to_string()))
        }
        Some(other) => return Err(format!("crs must be a coordinate system (EPSG:2056, WKT or a PROJ string), or \"\", not {other}")),
    };
    let m = s.map_mut();
    let kind_after = kind.unwrap_or(m.graticule_type);
    if matches!(crs, Some(Some(_))) && kind_after != GraticuleType::Measured {
        return Err(format!("crs is the measured grid's; the graticule is \"{}\" (give \"type\": \"measured\")", kind_after.code()));
    }
    let set = kind.is_some() || crs.is_some();
    // Only the lines' place or color given: shown or hidden as it was.
    let styled = on_top.is_some() || color.is_some();
    m.graticule = super::b(p, "on").unwrap_or(if set {
        true
    } else if styled {
        m.graticule
    } else {
        !m.graticule
    });
    m.graticule_type = kind_after;
    if let Some(c) = crs {
        m.graticule_crs = c;
    }
    if let Some(t) = on_top {
        m.graticule_on_top = t;
    }
    if let Some(c) = color {
        m.graticule_color = c;
    }
    Ok(json!({
        "on": m.graticule,
        "type": m.graticule_type.code(),
        "crs": m.graticule_crs,
        "onTop": m.graticule_on_top,
        "color": m.graticule_color.map(rustgis_map::Rgba::hex),
    }))
}

fn basemap(s: &mut Session, p: &Value) -> CmdResult {
    let n = s_name(p)?;
    let basemap = Basemap::from_code(n).ok_or_else(|| format!("unknown basemap '{n}'"))?;
    // The blank basemap's paper (#2532): absent keeps it, null takes it off (white).
    let paper = super::opaque_color(p, "color")?;
    let m = s.map_mut();
    m.basemap = basemap;
    // A built-in basemap takes the place of the web basemaps (tile overlays stay).
    if !super::b(p, "keepTiles").unwrap_or(false) {
        m.tiles.retain(|t| t.overlay);
    }
    if basemap != Basemap::Blank {
        // Another basemap draws its own colours: the paper goes, not to be saved unseen.
        m.paper = None;
        if paper.is_some_and(|c| c.is_some()) {
            let warning = format!("color is for the blank basemap only: {} keeps its own colours", basemap.label());
            s.warn(&warning);
            return Ok(json!({"warning": warning}));
        }
    } else if let Some(paper) = paper {
        // White is the blank basemap's own paper: nothing to keep, or to require of a reader.
        m.paper = paper.filter(|c| *c != rustgis_map::Rgba::rgb(0xFF, 0xFF, 0xFF));
    }
    Ok(Value::Null)
}

fn add_bookmark(s: &mut Session, p: &Value) -> CmdResult {
    let mi = map_at(s, p)?;
    if s.project.maps.get(mi).is_some_and(|m| m.bookmarks.len() >= MAX_BOOKMARKS) {
        return Err(format!("a map keeps at most {MAX_BOOKMARKS} bookmarks"));
    }
    let description =
        super::within("description", "a bookmark's description", ps(p, "description").unwrap_or_default().trim(), MAX_DESCRIPTION)?;
    let given = ps(p, "name").map(str::trim).filter(|n| !n.is_empty());
    let base = match given {
        Some(n) => super::within("name", "a bookmark's name", n, MAX_BOOKMARK_NAME)?,
        None => s.project.maps.get(mi).map_or_else(|| "Bookmark 1".into(), new_bookmark_name),
    };
    // A taken name is numbered, never replacing the bookmark that has it (#351).
    let name = unique_bookmark_name(s, mi, None, &base);
    let m = s.project.maps.get_mut(mi).ok_or("no map")?;
    m.bookmarks.push(Bookmark { description, ..Bookmark::new(name.clone(), m.camera) });
    super::say_name_cut(s, "New Bookmark", "the name given", &base, &name);
    Ok(json!({"name": name}))
}

fn go_bookmark(s: &mut Session, p: &Value) -> CmdResult {
    let (mi, bi) = bookmark_at(s, p)?;
    let from = s.project.maps.get(mi).ok_or("no map")?;
    let mark = from.bookmarks.get(bi).map(|b| b.camera).ok_or("no bookmark")?;
    // Another map's bookmark moves the map in view to the same place, as desktop GIS do: through
    // longitude/latitude, so it works across projections.
    let to = if mi == s.project.active_map {
        mark
    } else {
        place_of(from, &mark, s.map()).ok_or("the map in view can't show this bookmark's place")?
    };
    let m = s.map_mut();
    if super::b(p, "pan").unwrap_or(false) {
        // The scale as shown (1:n at the centre), which in most projections isn't the same
        // resolution at another latitude.
        let scale = m.scale();
        m.camera.center = to.center;
        m.set_scale(scale);
    } else {
        m.camera = to;
    }
    s.record_extent();
    Ok(Value::Null)
}

/// Where camera `at` of map `from` is on map `to`: the same place and ground resolution; None
/// where `to` can't show it.
fn place_of(from: &Map, at: &rustgis_map::Camera, to: &Map) -> Option<rustgis_map::Camera> {
    let flat = from.flat();
    let lonlat = flat.inverse(at.center)?;
    let mut camera = place_at(to, lonlat, at.res * flat.ground_per_unit(at.center))?;
    camera.set_bearing(at.bearing);
    camera.set_pitch(at.pitch);
    Some(camera)
}

/// Map `map`'s camera moved to `lonlat` at `ground` metres per pixel; None where it can't show
/// that place (or the resolution is no number).
fn place_at(map: &Map, lonlat: Coord, ground: f64) -> Option<rustgis_map::Camera> {
    if !(ground.is_finite() && ground > 0.0) {
        return None;
    }
    let flat = map.flat();
    let center = flat.forward_checked(lonlat)?;
    let mut camera = map.camera;
    camera.center = center;
    camera.res = ground / flat.ground_per_unit(center).max(1e-9);
    camera.clamp();
    Some(camera)
}

/// The map (`map`, default the active one) and the index of its bookmark `name` (any case).
fn bookmark_at(input: &impl super::InputSource, p: &Value) -> Result<(usize, usize), String> {
    let context = input.input_context();
    let s = &context;
    let mi = map_at(input, p)?;
    let n = s_name(p)?;
    let bi = s.project.maps.get(mi).and_then(|m| m.bookmarks.iter().position(|b| b.name.eq_ignore_ascii_case(n)));
    Ok((mi, bi.ok_or_else(|| format!("no bookmark '{n}'"))?))
}

/// `base`, numbered when another of map `mi`'s bookmarks (not `own`) has it, ignoring case; the
/// number fits in the 120 characters.
fn unique_bookmark_name(s: &Session, mi: usize, own: Option<usize>, base: &str) -> String {
    let marks = s.project.maps.get(mi).map(|m| m.bookmarks.as_slice()).unwrap_or_default();
    let taken = |n: &str| marks.iter().enumerate().any(|(j, b)| Some(j) != own && b.name.eq_ignore_ascii_case(n));
    super::numbered_within(base, MAX_BOOKMARK_NAME, taken)
}

/// The name a new bookmark of `map` gets unless one is given: `Bookmark N`, N one past the
/// bookmarks it has, or the next N free (after a delete, or a bookmark named so by hand).
pub fn new_bookmark_name(map: &Map) -> String {
    let count = map.bookmarks.len();
    // The numbers taken, in one pass (names compare ignoring case, as `bookmark_at` does).
    let taken: std::collections::HashSet<usize> = map
        .bookmarks
        .iter()
        .filter_map(|b| b.name.get(..9).filter(|p| p.eq_ignore_ascii_case("bookmark ")).and_then(|_| b.name.get(9..)))
        .filter(|n| !n.starts_with('+') && !n.starts_with('0'))
        .filter_map(|n| n.parse().ok())
        .collect();
    // Of count + 1 numbers, the count bookmarks can't hold them all.
    let n = (count + 1..=2 * count + 1).find(|i| !taken.contains(i)).unwrap_or(count + 1);
    format!("Bookmark {n}")
}

/// The longest bookmark name and description kept.
pub(crate) const MAX_BOOKMARK_NAME: usize = 120;
const MAX_DESCRIPTION: usize = 2_000;

/// Most bookmarks a map keeps: as many as a QGIS project import reads.
pub(crate) use rustgis_map::qgis::MAX_BOOKMARKS;

/// The map's bookmarks as a bookmarks file. Places are lon/lat and metres per pixel, so they
/// open in the same place on a map in any projection.
pub(crate) fn bookmarks_file(map: &Map) -> Result<Vec<u8>, String> {
    if map.bookmarks.is_empty() {
        return Err("the map has no bookmarks; save one with New Bookmark".into());
    }
    let flat = map.flat();
    let marks: Vec<Value> = map
        .bookmarks
        .iter()
        .filter_map(|b| {
            let c = flat.inverse(b.camera.center)?;
            let ground = b.camera.res * flat.ground_per_unit(b.camera.center);
            let mut mark = json!({"name": b.name, "lon": c.x, "lat": c.y, "metresPerPixel": ground});
            if b.camera.bearing != 0.0 {
                mark["bearing"] = json!(b.camera.bearing);
            }
            if b.camera.pitch != 0.0 {
                mark["pitch"] = json!(b.camera.pitch);
            }
            if !b.description.is_empty() {
                mark["description"] = json!(b.description);
            }
            Some(mark)
        })
        .collect();
    if marks.is_empty() {
        return Err("none of the map's bookmarks has a place on the earth".into());
    }
    serde_json::to_vec_pretty(&json!({"type": "rustgisBookmarks", "bookmarks": marks})).map_err(|e| e.to_string())
}

fn import_bookmarks(s: &mut Session, p: &Value) -> CmdResult {
    let text = super::items::read_text(p)?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("not a RustGIS bookmarks file: {e}"))?;
    let list = v.get("bookmarks").and_then(Value::as_array).ok_or("not a RustGIS bookmarks file: no bookmarks")?;
    let m = s.map_mut();
    let mut names = Vec::new();
    // The map's bookmarks by name (the first of any that differ only in case, as found before),
    // and the names this file added or replaced: a file of many entries is read in one pass.
    let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, b) in m.bookmarks.iter().enumerate() {
        at.entry(b.name.to_ascii_lowercase()).or_insert(i);
    }
    let mut taken = std::collections::HashSet::new();
    // What the file has that isn't taken, counted to be said (#1896).
    let (mut no_name, mut no_place, mut past, mut cut_names, mut cut_descriptions) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for b in list {
        // A cut name is counted once the bookmark is kept, below.
        let (name, name_cut): (String, bool) = match super::ps(b, "name").map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => (n.chars().take(MAX_BOOKMARK_NAME).collect(), n.chars().nth(MAX_BOOKMARK_NAME).is_some()),
            None => {
                no_name += 1;
                continue;
            }
        };
        let (Some(lon), Some(lat), Some(ground)) = (f(b, "lon"), f(b, "lat"), f(b, "metresPerPixel")) else {
            no_place += 1;
            continue;
        };
        if !(lon.is_finite() && lat.is_finite() && (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat)) {
            no_place += 1;
            continue;
        }
        let Some(mut camera) = place_at(m, Coord::new(lon, lat), ground) else {
            no_place += 1;
            continue;
        };
        // A new bookmark past the map's limit is left out; one replacing a bookmark by its name
        // is taken, as it adds none.
        let key = name.to_ascii_lowercase();
        let existing = at.get(&key).copied();
        if existing.is_none() && m.bookmarks.len() >= MAX_BOOKMARKS {
            past += 1;
            continue;
        }
        cut_names += usize::from(name_cut);
        // A file from before bearings faces north.
        camera.set_bearing(f(b, "bearing").unwrap_or(0.0));
        camera.set_pitch(f(b, "pitch").unwrap_or(0.0));
        // Absent (a file from before descriptions, or written by hand) isn't empty: a bookmark
        // it replaces keeps its own.
        let description: Option<String> = super::ps(b, "description").map(|d| {
            cut_descriptions += usize::from(d.trim().chars().nth(MAX_DESCRIPTION).is_some());
            d.trim().chars().take(MAX_DESCRIPTION).collect()
        });
        // A bookmark by the same name is replaced where it is in the list (importing a file
        // again updates its bookmarks).
        match existing.and_then(|i| m.bookmarks.get_mut(i)) {
            Some(b) => {
                b.camera = camera;
                if let Some(d) = description {
                    b.description = d;
                }
            }
            None => {
                at.insert(key.clone(), m.bookmarks.len());
                m.bookmarks.push(Bookmark { description: description.unwrap_or_default(), ..Bookmark::new(name.clone(), camera) });
            }
        }
        if taken.insert(key) {
            names.push(name);
        }
    }
    let left: Vec<String> = [
        (no_name, "without a name left out".to_string()),
        (no_place, "without a place this map can show left out".to_string()),
        (past, format!("past the map's {MAX_BOOKMARKS} left out")),
        (cut_names, format!("with a name cut to {MAX_BOOKMARK_NAME} characters")),
        (cut_descriptions, format!("with a description cut to {MAX_DESCRIPTION} characters")),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} bookmark(s) {what}"))
    .collect();
    // Nothing taken is an error, which gives what was left out and why.
    if names.is_empty() {
        let none = "the file has no bookmarks this map can show";
        return Err(if left.is_empty() { none.to_string() } else { format!("{none}: {}", left.join(", ")) });
    }
    for what in left {
        s.notify(format!("Import Bookmarks: {what}"), false);
    }
    Ok(json!({"added": names}))
}

fn new_map(s: &mut Session, p: &Value) -> CmdResult {
    let base = super::ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Map").to_string();
    let mut name = base.clone();
    let mut i = 1;
    while s.project.maps.iter().any(|m| m.name.eq_ignore_ascii_case(&name)) {
        i += 1;
        name = format!("{base}{i}");
    }
    let mut m = Map::new(name.clone());
    (m.basemap, m.paper) = (s.map().basemap, s.map().paper);
    // Drawn in the project's coordinate system, where the active map is looking; one a map can't
    // be drawn in is said, and the map starts in Web Mercator as without one.
    if let Some(crs) = s.project.coordinate_system {
        match map_projection_for(crs) {
            Ok(projection) => {
                (m.projection, m.camera) = (s.map().projection.clone(), s.map().camera);
                m.camera.bearing = 0.0;
                m.camera.pitch = 0.0;
                m.set_projection(projection);
            }
            Err(why) => s.notify(format!("New map '{name}' is drawn in Web Mercator: {why}"), false),
        }
    }
    s.project.maps.push(m);
    s.project.active_map = s.project.maps.len() - 1;
    s.active_layer = None;
    s.table_layer = None;
    Ok(json!({"name": name}))
}

/// The map projection for a coordinate system, or why a map can't be drawn in it: a map is
/// drawn in Web Mercator, a world projection or a projected system with an EPSG or ESRI code.
pub fn map_projection_for(crs: rustgis_data::raster::Crs) -> Result<Projection, String> {
    use rustgis_data::raster::Crs;
    match crs {
        Crs::WebMercator => Ok(Projection::WebMercator),
        c if c.is_geographic() => {
            Err(format!("the project's coordinate system, {}, is in degrees, and a map is drawn in a projected one", c.label()))
        }
        // An ESRI system is a custom one named by its code.
        c => {
            let code = match c {
                Crs::Epsg(_) => c.code(),
                _ => Some(c.label()).filter(|l| l.starts_with("ESRI:")).ok_or_else(|| {
                    format!("a map is drawn in a system with an EPSG or ESRI code, and the project's, {}, has none", c.label())
                })?,
            };
            Projection::from_code(&code).map_err(|e| format!("a map can't be drawn in the project's coordinate system, {}: {e}", c.label()))
        }
    }
}

/// Uses the strict map selector for operations that accept map names or null.
fn map_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let map = s.project.maps.get(map_at(input, p)?).ok_or("no map")?;
    let mut inputs = super::Inputs::layers(std::iter::empty());
    inputs.maps.push(map.instance_id());
    Ok(inputs)
}

/// Activation and removal require a string name, unlike optional map selectors.
fn required_map_at(input: &impl super::InputSource, p: &Value) -> Result<usize, String> {
    let context = input.input_context();
    let s = &context;
    let n = s_name(p)?;
    s.project.maps.iter().position(|m| m.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no map '{n}'"))
}
/// Binds the required map and its existing layers before activation/removal.
fn required_map_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let map = s.project.maps.get(required_map_at(input, p)?).ok_or("no map")?;
    let mut inputs = super::Inputs::layers(map.layers.iter().map(|l| (map, l)));
    inputs.maps.push(map.instance_id());
    Ok(inputs)
}
/// Binds the map and layer incarnations that duplication will clone.
fn duplicate_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let map = s.project.maps.get(map_at(input, p)?).ok_or("no map")?;
    let mut inputs = super::Inputs::layers(map.layers.iter().map(|l| (map, l)));
    inputs.maps.push(map.instance_id());
    Ok(inputs)
}
/// Binds the bookmark's source map and the active destination map without freezing camera data.
fn bookmark_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let (mi, bi) = bookmark_at(input, p)?;
    let map = s.project.maps.get(mi).ok_or("no map")?;
    let mut inputs = super::Inputs::layers(std::iter::empty());
    inputs.maps.extend([map.instance_id(), s.map().instance_id()]);
    inputs.state = json!({"bookmark":bi,"name":map.bookmarks.get(bi).map(|b|&b.name)});
    Ok(inputs)
}

/// The actual activation choice, shared with ordered approval; raw selection bookkeeping survives.
fn activation(s: &super::InputContext<'_>, p: &Value) -> Result<Option<super::Selection>, String> {
    let map = required_map_at(s, p)?;
    let layer = s.project.maps.get(map).and_then(|m| m.layers.first()).map(|l| l.id);
    Ok(Some(super::Selection { map, layer, names_changed: false }))
}
/// Validates the actual rename selector and uniqueness rule without changing project data.
fn rename_choice(s: &super::InputContext<'_>, p: &Value) -> Result<(usize, String), String> {
    let n = s_name(p)?.trim();
    if n.is_empty() {
        return Err("name is required".into());
    }
    let i = map_at(s, p)?;
    let n = super::within("name", "a map's name", n, super::MAX_NAME)?;
    let name = super::numbered(&n, |n| s.project.maps.iter().enumerate().any(|(j, m)| j != i && m.name.eq_ignore_ascii_case(n)));
    Ok((i, name))
}
/// Renaming preserves defaults on the same incarnation; it does not model a later map namespace.
fn rename_transition(s: &super::InputContext<'_>, p: &Value) -> Result<Option<super::Selection>, String> {
    let _ = rename_choice(s, p)?;
    let mut selection = s.selection();
    selection.names_changed = true;
    Ok(Some(selection))
}
/// These namespace changes cannot be predicted by a scalar borrowed view, so exact Ask refuses.
fn unmodelled_transition(_: &super::InputContext<'_>, _: &Value) -> Result<Option<super::Selection>, String> {
    Ok(None)
}
