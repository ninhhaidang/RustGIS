//! Importing layout files.

use super::*;

pub(crate) fn import(s: &mut Session, text: &str) -> CmdResult {
    if text.len() as u64 > MAX_LAYOUT {
        return Err("the layout file is too large".into());
    }
    // A newer format, or a capability this release doesn't know, is refused by name rather than
    // read without what it adds (#2277). A file without a header is a legacy layout.
    crate::project::check_format(text, "layout")?;
    let mut l: Layout = serde_json::from_str(text).map_err(|e| format!("not a RustGIS layout: {e}"))?;
    // Refused before anything is said of it.
    if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
        return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
    }
    let repairs = l.clamp();
    if !repairs.is_empty() {
        for notice in crate::project::layout_notices(&l.name, "while importing the layout file", 0, &[(l.name.clone(), repairs)]) {
            s.notify(notice, false);
        }
    }
    let (name, cut) = unique_name(s, "Import Layout", "the layout file's name", &l.name);
    l.name = name;
    if let Some(cut) = cut {
        s.notify(cut, false);
    }
    // Frames showing maps this project lacks show the active map instead.
    let fallback = s.map().name.clone();
    let names: Vec<String> = s.project.maps.iter().map(|m| m.name.to_ascii_lowercase()).collect();
    let mut swapped = 0usize;
    for e in &mut l.elements {
        if let ElementKind::MapFrame { map, .. } = &mut e.kind
            && !names.contains(&map.to_ascii_lowercase())
        {
            *map = fallback.clone();
            swapped = swapped.saturating_add(1);
        }
    }
    if swapped > 0 {
        s.notify(
            format!(
                "{}: {swapped} map frame(s) showing a map this project lacks show \"{}\" instead",
                crate::project::short_name(&l.name),
                crate::project::short_name(&fallback)
            ),
            false,
        );
    }
    // A frame centred where its map can't place the point (a pole on Web Mercator) is said (#2684).
    // One notice for them all, counted.
    let unplaced: Vec<(String, Option<f64>)> = l.elements.iter().filter_map(|e| unplaced_center(s, &l, e.id)).collect();
    if let Some(said) = unplaced_notice(&l.name, &unplaced) {
        s.notify(said, false);
    }
    let name = l.name.clone();
    s.project.layouts.push(l);
    let i = s.project.layouts.len() - 1;
    s.active_layout = Some(i);
    say_layout_text_statistics(s, i);
    Ok(json!({"layout": i, "name": name}))
}

/// Map frame `id` in `l` when its centre lies where its map's projection can't place it (a pole
/// on Web Mercator, whose poles are at infinity): its name and the latitude it's drawn centred
/// at instead, as [`rustgis_map::layout::frame_camera`] draws it (85° for a Mercator pole;
/// `None` when that can't be read back) (#2684). `None` when it's drawn where it's centred, or
/// `id` isn't a frame of a map here.
pub(in crate::cmd::layout) fn unplaced_center(s: &Session, l: &Layout, id: u32) -> Option<(String, Option<f64>)> {
    let e = l.element(id)?;
    let ElementKind::MapFrame { map, center, scale, .. } = &e.kind else { return None };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?;
    if rustgis_map::layout::frame_center_placed(m, *center) {
        return None;
    }
    let cam = rustgis_map::layout::frame_camera(m, *center, *scale, 0.0, 0.0);
    let drawn = m.to_lonlat(cam.center).map(|c| (c.y * 100.0).round() / 100.0).filter(|y| y.is_finite());
    Some((e.name.clone(), drawn))
}

/// One notice for the map frames of layout `layout` centred where their maps' projections can't
/// show them, with where each is drawn instead ([`unplaced_center`]): counted, the first five
/// named. `None` for none (#2684).
pub(in crate::cmd::layout) fn unplaced_notice(layout: &str, frames: &[(String, Option<f64>)]) -> Option<String> {
    if frames.is_empty() {
        return None;
    }
    let at = |lat: &Option<f64>| match lat {
        Some(y) => format!("latitude {}", rustgis_map::symbology::fmt_num(*y)),
        None => "the map's view centre".to_string(),
    };
    let mut named = frames
        .iter()
        .take(5)
        .map(|(name, lat)| format!("'{}' at {}", crate::project::short_name(name), at(lat)))
        .collect::<Vec<_>>()
        .join(", ");
    if frames.len() > 5 {
        named.push_str(&format!(" and {} more", frames.len() - 5));
    }
    Some(format!(
        "{}: {} map frame(s) are centred where their map's projection can't show the centre, and are drawn centred elsewhere: {named}",
        crate::project::short_name(layout),
        frames.len()
    ))
}
