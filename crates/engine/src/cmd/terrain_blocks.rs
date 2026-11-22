//! Validate retained block diagrams when a DEM's deferred pixels become available. Checks are
//! bounded to a fixed grid, once per DEM allocation and block extent, never per raster cell.

use std::collections::HashSet;
use std::sync::{Arc, Weak};

use rustgis_data::raster::Raster;
use rustgis_geom::Coord;
use rustgis_map::view::TerrainSource;

use crate::Session;
use crate::notice_text::NoticeText;

const CHECK_CELLS: usize = 64;

/// The box `b` is cut to when it has one of its own: its extent, or the box round the polygons it
/// is cut to (#2318) when they can be read. None: the DEM's.
pub(crate) fn own_box(map: &rustgis_map::Map, b: &rustgis_map::view::TerrainBlock) -> Option<[f64; 4]> {
    b.extent.or_else(|| match &b.shape {
        rustgis_map::view::BlockShape::Polygon { layer, filter } => {
            rustgis_render::terrain::Outline::of(map, layer, filter.as_deref()).map(|o| o.extent())
        }
        _ => None,
    })
}

pub(crate) struct Checked {
    layer: u64,
    pixels: Weak<Raster>,
    extent: Option<[f64; 4]>,
}

#[derive(Debug)]
pub(crate) struct Coverage {
    extent: [f64; 4],
    missing: usize,
    total: usize,
}

/// Uses the same admission rule as a loaded DEM, plus a fixed sampling grid that says when
/// heights are missing inside or beyond its footprint. No file or tile request is started.
pub(super) fn check(name: &str, raster: &Raster, extent: Option<[f64; 4]>) -> Result<Coverage, String> {
    let boxes = (name.to_string(), rustgis_render::terrain::raster_box(raster), rustgis_render::terrain::raster_footprint(raster));
    if let Some(reason) = super::project::off_dem(extent, Some(&boxes)) {
        return Err(reason);
    }
    let extent = extent.or(boxes.1).ok_or("no block extent fits the DEM")?;
    if raster.width == 0 || raster.height == 0 {
        return Err(format!("'{name}' has no height cells for the block"));
    }
    let mut missing = 0;
    for row in 0..=CHECK_CELLS {
        for col in 0..=CHECK_CELLS {
            let x = extent[0] + (extent[2] - extent[0]) * col as f64 / CHECK_CELLS as f64;
            let y = extent[1] + (extent[3] - extent[1]) * row as f64 / CHECK_CELLS as f64;
            missing += usize::from(rustgis_render::terrain::dem_height(raster, Coord::new(x, y)).is_none());
        }
    }
    let total = (CHECK_CELLS + 1) * (CHECK_CELLS + 1);
    if missing == total {
        // A coarse grid can step over a small valid DEM. Only refuse when a bounded scan
        // proves the entire first band has no height; otherwise retain the block and report
        // the sampled missing locations. Never scan an unbounded raster on a frame.
        let empty = raster.width.checked_mul(raster.height).is_some_and(|cells| cells <= (CHECK_CELLS * CHECK_CELLS) as u32)
            && (0..raster.height).all(|row| {
                (0..raster.width).all(|col| raster.value(0, i64::from(col), i64::from(row)).is_none_or(|height| !height.is_finite()))
            });
        if empty {
            return Err(format!("the block has no height from '{name}': its bounded first band has no usable cells"));
        }
    }
    Ok(Coverage { extent, missing, total })
}

/// Blocks cut to polygons that can't be read, said as the project opened: remembered as checked,
/// so the landing check doesn't say them again, now or when the DEM's pixels land (an edit that
/// changes their box checks again).
pub(crate) fn told_uncut(session: &mut Session) {
    for map in &session.project.maps {
        let Some(terrain) = &map.view.terrain else { continue };
        let (TerrainSource::Layer(name), Some(block)) = (&terrain.source, &terrain.block) else { continue };
        if !matches!(block.shape, rustgis_map::view::BlockShape::Polygon { .. }) || own_box(map, block).is_some() {
            continue;
        }
        let Some(layer) = map.layer_by_name(name) else { continue };
        let pixels = layer.raster.as_ref().and_then(|raster| raster.data.as_ref()).map(Arc::downgrade).unwrap_or_default();
        let checked = Checked { layer: layer.instance_id(), pixels, extent: None };
        session.terrain_blocks_checked.insert(map.instance_id(), checked);
    }
}

/// Blocks cut to polygons of layer `cut`, which was just removed: said by that removal
/// (`terrain_follows_removal`), so remembered as said here. Otherwise the landing check would say
/// them again as blocks whose polygons can't be read (#2318).
pub(crate) fn said_removed(session: &mut Session, cut: &str) {
    for map in &session.project.maps {
        let Some(terrain) = &map.view.terrain else { continue };
        let (TerrainSource::Layer(name), Some(block)) = (&terrain.source, &terrain.block) else { continue };
        let rustgis_map::view::BlockShape::Polygon { layer: shape, .. } = &block.shape else { continue };
        if !shape.eq_ignore_ascii_case(cut) || map.layer_by_name(cut).is_some() {
            continue;
        }
        let Some(dem) = map.layer_by_name(name) else { continue };
        let pixels = dem.raster.as_ref().and_then(|raster| raster.data.as_ref()).map(Arc::downgrade).unwrap_or_default();
        let checked = Checked { layer: dem.instance_id(), pixels, extent: None };
        session.terrain_blocks_checked.insert(map.instance_id(), checked);
    }
}

/// Called after a command commits and when the frame reports newly landed raster pixels.
/// A Weak retains allocation identity without retaining the DEM's pixel buffers.
pub(crate) fn landed(session: &mut Session) {
    landed_impl(session, false);
}

/// Undo/redo restores the recorded view exactly; report unusable restored bounds without
/// immediately deleting them again. Remembering the checked allocation also keeps later frames.
pub(crate) fn after_command(session: &mut Session, id: &str) {
    landed_impl(session, matches!(id, "edit.undo" | "edit.redo"));
}

fn landed_impl(session: &mut Session, restoring: bool) {
    // Like in-place overview maintenance, unrelated retained-view edits belong only to the
    // native owner/frame. An authorized Undo/Redo only remembers its restored allocation and
    // warns; it never drops bounds, so the next native frame cannot undo that explicit Undo.
    if !session.policy_ingress_idle() && !restoring {
        return;
    }
    let mut checked = std::mem::take(&mut session.terrain_blocks_checked);
    let mut maps = HashSet::new();
    let mut dropped = Vec::new();
    let mut restored = Vec::new();
    let mut partial = Vec::new();
    // Blocks cut to polygons (#2318) that can't be read, and those off their DEM: said, and kept,
    // so an edit of the polygons brings them back.
    let (mut uncut, mut off) = (Vec::new(), Vec::new());
    for map in &session.project.maps {
        let map_id = map.instance_id();
        maps.insert(map_id);
        let Some(terrain) = &map.view.terrain else {
            checked.remove(&map_id);
            continue;
        };
        let (TerrainSource::Layer(name), Some(block)) = (&terrain.source, &terrain.block) else {
            checked.remove(&map_id);
            continue;
        };
        let Some(layer) = map.layer_by_name(name) else {
            checked.remove(&map_id);
            continue;
        };
        let polygons = matches!(block.shape, rustgis_map::view::BlockShape::Polygon { .. });
        let Some(raster) = layer.raster.as_ref().and_then(|raster| raster.data.as_ref()) else {
            // A block cut to polygons that can't be read keeps its mark while its pixels are on
            // their way: it was said as the project opened (`told_uncut`).
            if !(polygons && checked.get(&map_id).is_some_and(|old| old.extent.is_none())) {
                checked.remove(&map_id);
            }
            continue;
        };
        let pixels = Arc::downgrade(raster);
        let extent = own_box(map, block);
        // One that still can't be read was said already, whatever pixels have landed.
        if checked.get(&map_id).is_some_and(|old: &Checked| {
            old.layer == layer.instance_id() && old.extent == extent && (old.pixels.ptr_eq(&pixels) || (polygons && extent.is_none()))
        }) {
            continue;
        }
        checked.insert(map_id, Checked { layer: layer.instance_id(), pixels, extent });
        if polygons && extent.is_none() {
            uncut.push(crate::project::short_name(&map.name));
            continue;
        }
        let name = crate::project::short_name(name);
        match check(&name, raster, extent) {
            Err(reason) => {
                let detail = format!("{} ({reason})", crate::project::short_name(&map.name));
                if polygons {
                    off.push(detail);
                } else if restoring {
                    restored.push(detail);
                } else {
                    dropped.push((map_id, detail));
                    checked.remove(&map_id);
                }
            }
            Ok(coverage) if coverage.missing > 0 => partial.push((crate::project::short_name(&map.name), name, coverage)),
            Ok(_) => {}
        }
    }
    checked.retain(|map, _| maps.contains(map));
    session.terrain_blocks_checked = checked;
    let details = |first: Vec<String>, total: usize| {
        let mut text = first.join("; ");
        if total > 3 {
            text.push_str(&format!("; and {} more", total - 3));
        }
        text
    };
    if !dropped.is_empty() {
        let ids: HashSet<_> = dropped.iter().map(|(id, _)| *id).collect();
        // Pixel loading is not itself an edit transaction. Record its durable view adjustment
        // explicitly, including when this is called from a frame rather than a command.
        let changed = session.guarded("Update loaded terrain blocks", true, |session| {
            for map in &mut session.project.maps {
                if ids.contains(&map.instance_id())
                    && let Some(terrain) = &mut map.view.terrain
                {
                    terrain.block = None;
                }
            }
            Ok(serde_json::Value::Null)
        });
        if let Err(error) = changed {
            session.notify(error, true);
        } else {
            session.notify_text(
                NoticeText::format(
                    "Terrain: {count} block diagram(s) dropped after their DEM loaded: {details}; the terrain draws the whole map",
                    [
                        ("count", dropped.len().to_string().into()),
                        ("details", details(dropped.iter().take(3).map(|(_, detail)| detail.clone()).collect(), dropped.len()).into()),
                    ],
                ),
                false,
            );
        }
    }
    if !uncut.is_empty() {
        session.notify(
            format!(
                "Terrain: the polygons a block diagram is cut to can't be read on {}: no block is drawn until they can",
                uncut.join(", ")
            ),
            false,
        );
    }
    if !off.is_empty() {
        session.notify(
            format!(
                "Terrain: {} block diagram(s) cut to polygons lie off their DEM, kept for when the polygons change: {}",
                off.len(),
                details(off.iter().take(3).cloned().collect(), off.len())
            ),
            false,
        );
    }
    if !restored.is_empty() {
        session.notify_text(NoticeText::format(
            "Terrain: {count} restored block diagram(s) kept without usable DEM heights: {details}; change their bounds or remove the blocks to use the DEM",
            [("count", restored.len().to_string().into()), ("details", details(restored.iter().take(3).cloned().collect(), restored.len()).into())],
        ), false);
    }
    if !partial.is_empty() {
        let missing = partial.iter().map(|(_, _, coverage)| coverage.missing).sum::<usize>();
        let total = partial.iter().map(|(_, _, coverage)| coverage.total).sum::<usize>();
        let first = partial
            .iter()
            .take(3)
            .map(|(map, name, coverage)| {
                let [x0, y0, x1, y1] = coverage.extent;
                format!("{map}, DEM '{name}', [{x0:.0}, {y0:.0}, {x1:.0}, {y1:.0}]: {}/{}", coverage.missing, coverage.total)
            })
            .collect();
        session.notify_text(NoticeText::format(
            "Terrain: {missing} of {total} check samples for {count} block diagram(s) have no DEM height (ground fallback 0 m before water and exaggeration); checked on a {side} by {side} grid in Web Mercator boxes: {details}",
            [("missing", missing.to_string().into()), ("total", total.to_string().into()), ("count", partial.len().to_string().into()),
             ("side", (CHECK_CELLS + 1).to_string().into()), ("details", details(first, partial.len()).into())],
        ), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform};
    use rustgis_map::{
        raster::RasterLayer,
        view::{BLOCK_WALLS, Terrain, TerrainBlock},
    };

    fn pixels(nodata: bool) -> Raster {
        Raster::new(
            "DEM",
            4,
            4,
            vec![vec![if nodata { -9999.0 } else { 12.0 }; 16]],
            GeoTransform { origin_x: 1000.0, origin_y: 1400.0, pixel_width: 100.0, pixel_height: -100.0 },
            Crs::WebMercator,
            Some(-9999.0),
            ColorHint::Gray,
            "Float32",
        )
        .unwrap()
    }
    fn pending(extent: [f64; 4]) -> Session {
        let mut s = Session::new(crate::Project::default());
        s.map_mut().add_raster("DEM", RasterLayer::unread_in_place("https://invalid.example/dem.tif"));
        s.map_mut().view.terrain = Some(Terrain {
            source: TerrainSource::Layer("DEM".into()),
            exaggeration: 1.0,
            block: Some(TerrainBlock {
                extent: Some(extent),
                depth: None,
                walls: BLOCK_WALLS,
                walls_layer: None,
                shape: Default::default(),
                water: None,
            }),
            water: None,
        });
        s.notices.clear();
        s
    }
    fn load(s: &mut Session, r: Raster) {
        s.map_mut().layers.iter_mut().find(|l| l.name == "DEM").unwrap().raster.as_mut().unwrap().data = Some(Arc::new(r));
    }
    #[test]
    fn a_small_valid_dem_between_grid_samples_keeps_its_block() {
        let extent = [900.0, 900.0, 64900.0, 64900.0];
        let raster = pixels(false);
        assert!(rustgis_render::terrain::dem_height(&raster, Coord::new(1200.0, 1200.0)).is_some());
        let coverage = check("DEM", &raster, Some(extent)).unwrap();
        assert_eq!(coverage.missing, coverage.total);
        let mut session = pending(extent);
        load(&mut session, raster);
        session.notice_landed_rasters();
        assert!(session.map().view.terrain.as_ref().unwrap().block.is_some());
        assert!(session.notices.iter().any(|notice| notice.text.contains("check samples")));
    }

    #[test]
    fn a_named_read_only_command_does_not_drop_unrelated_terrain_blocks() {
        use crate::policy::{AgentAccess, Call, Effects, Revocation, Scope};
        struct Permit(bool);
        impl Revocation for Permit {
            fn allows(&self, effects: Effects) -> bool {
                self.0 || effects == Effects::NONE
            }
        }
        let mut session = pending([2000.0, 2000.0, 2200.0, 2200.0]);
        load(&mut session, pixels(false));
        session.clear_history();
        let before = serde_json::to_value(&session.project).unwrap();
        let state = session.state();
        let scope = Scope::agent(AgentAccess::Allow, false, "terrain-read", None, Arc::new(Permit(false)));
        session.run_scoped(scope, &Call::command("info.diagnostics", serde_json::Value::Null)).unwrap();
        assert_eq!(serde_json::to_value(&session.project).unwrap(), before);
        assert_eq!(session.state(), state);
        assert_eq!(session.undo_depth(), 0);
        assert!(!session.dirty());
        // The actual native frame can perform that maintenance with an undoable edit.
        session.notice_landed_rasters();
        assert!(session.map().view.terrain.as_ref().unwrap().block.is_none());
        assert!(session.dirty());
        assert_eq!(session.undo_depth(), 1);
        // Full access may explicitly Undo the native edit. Its cache bookkeeping is harmless
        // and necessary: the following frame must keep the recorded bounds restored by Undo.
        let owner = Scope::agent(AgentAccess::Full, true, "terrain-undo", None, Arc::new(Permit(true)));
        session.run_scoped(owner, &Call::command("edit.undo", serde_json::Value::Null)).unwrap();
        assert!(session.map().view.terrain.as_ref().unwrap().block.is_some());
        session.notice_landed_rasters();
        assert!(session.map().view.terrain.as_ref().unwrap().block.is_some());
        assert!(!session.dirty());
        assert_eq!(session.undo_depth(), 0);
    }

    #[test]
    fn deferred_dem_revalidates_disjoint_block_once_without_removing_terrain() {
        let mut s = pending([2000.0, 2000.0, 2200.0, 2200.0]);
        s.notice_landed_rasters();
        assert!(s.map().view.terrain.as_ref().unwrap().block.is_some());
        assert!(s.notices.is_empty());
        load(&mut s, pixels(false));
        s.notice_landed_rasters();
        assert!(s.map().view.terrain.as_ref().unwrap().block.is_none());
        let count = s.notices.iter().filter(|n| n.text.contains("1 block diagram(s) dropped")).count();
        assert_eq!(count, 1);
        s.notice_landed_rasters();
        assert_eq!(s.notices.iter().filter(|n| n.text.contains("block diagram(s) dropped")).count(), 1);
        assert!(s.dirty());
        assert_eq!(s.undo_depth(), 1);
        s.run("edit.undo", &serde_json::Value::Null).unwrap();
        assert!(s.map().view.terrain.as_ref().unwrap().block.is_some());
        assert!(!s.dirty());
        s.notice_landed_rasters();
        assert!(s.map().view.terrain.as_ref().unwrap().block.is_some(), "a frame must not undo the user's Undo");
        assert_eq!(s.undo_depth(), 0);
        s.run("edit.redo", &serde_json::Value::Null).unwrap();
        assert!(s.map().view.terrain.as_ref().unwrap().block.is_none());
        assert!(s.dirty());
    }
    #[test]
    fn loaded_dem_without_heights_is_refused_before_terrain_changes() {
        let mut s = pending([1100.0, 1100.0, 1300.0, 1300.0]);
        load(&mut s, pixels(true));
        let before = s.map().view.terrain.clone();
        let error = s
            .run("map.terrain", &serde_json::json!({"layer": "DEM", "exaggeration": 2, "block": {"extent": [1100, 1100, 1300, 1300]}}))
            .unwrap_err();
        assert!(error.contains("no height"), "{error}");
        assert_eq!(s.map().view.terrain, before);
        assert!(!s.notices.iter().any(|notice| notice.text.contains("block diagram(s) dropped")));
    }

    #[test]
    fn partial_dem_says_sample_count_and_box_once_per_allocation_clean_dem_says_none() {
        let mut s = pending([900.0, 1000.0, 1300.0, 1400.0]);
        load(&mut s, pixels(false));
        s.notice_landed_rasters();
        let notices: Vec<_> = s.notices.iter().filter(|n| n.text.contains("check samples")).collect();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].text.contains("4225") && notices[0].text.contains("[900, 1000, 1300, 1400]"));
        assert!(s.map().view.terrain.as_ref().unwrap().block.is_some());
        s.notice_landed_rasters();
        assert_eq!(s.notices.iter().filter(|n| n.text.contains("check samples")).count(), 1);
        load(&mut s, pixels(false));
        s.notice_landed_rasters();
        assert_eq!(s.notices.iter().filter(|n| n.text.contains("check samples")).count(), 2);
        let mut clean = pending([1100.0, 1100.0, 1300.0, 1300.0]);
        load(&mut clean, pixels(false));
        clean.notice_landed_rasters();
        assert!(clean.notices.is_empty());
        assert!(check("DEM", &pixels(true), Some([1100.0, 1100.0, 1300.0, 1300.0])).unwrap_err().contains("no height"));
    }
}
