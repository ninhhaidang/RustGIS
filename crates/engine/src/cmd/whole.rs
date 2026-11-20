//! Every feature of a layer, for a command whose result stands for the whole layer (a model
//! trained from it, a classification): a layer read in place holds only the features of the
//! last view, so its source is read whole, as Load All Features reads it, and the rows that
//! couldn't be read are said. As for an in-memory layer, every row is read, whatever the layer's
//! definition query.

use rustgis_data::Dataset;
use rustgis_map::{Layer, Unreadable};

use crate::Session;

/// A layer's rows to read whole, taken when the command starts and read where its work runs
/// (on another thread for one the app runs in the background).
pub(crate) struct Whole {
    /// The layer's name, for what is said of the read.
    name: String,
    rows: Rows,
}

enum Rows {
    /// A layer in memory: its features, which are all of them (shared, not copied).
    Held(Dataset),
    /// A layer read in place: a copy of it, its own read of the source to make.
    Source(Box<Layer>),
}

/// The rows of a layer's source that a whole read couldn't read and left out, and the values in
/// its date fields that weren't dates.
pub(crate) struct Unread {
    layer: String,
    rows: usize,
    dates: Option<String>,
    rounded: Option<String>,
    messages: Vec<String>,
}

impl Whole {
    /// `l`'s rows, to read with [`Whole::read`].
    pub(crate) fn of(l: &Layer) -> Self {
        let rows = if let Some(source) = &l.source {
            // What a whole read needs of the layer (its source, and the name, fields, type and
            // marks the read keeps), not the view's features and caches.
            let ds = &l.dataset;
            let mut header = Dataset::new(ds.name.clone(), ds.geometry_type, ds.fields.clone());
            header.zm_dropped = ds.zm_dropped;
            header.source = ds.source.clone();
            let mut copy = Layer::new(l.id, header);
            let mut source = source.clone();
            // Its own read: the live layer keeps the one under way for its view.
            source.detach_read();
            copy.source = Some(source);
            Rows::Source(Box::new(copy))
        } else {
            Rows::Held(l.dataset.clone())
        };
        Whole { name: l.name.clone(), rows }
    }

    /// Every feature in longitude/latitude, whatever system the layer holds its coordinates in
    /// (its work reads places there), with the fields the layer lists (so a field's index in the
    /// layer is its index here), and the rows of its source that couldn't be read. A source read
    /// a chunk at a time looks at Cancel between chunks; one read at once (a GeoJSON file, a
    /// database table) is a single step that doesn't, as for Load All Features.
    pub(crate) fn read(self) -> Result<(Dataset, Unread), String> {
        let Whole { name, rows } = self;
        let (mut ds, unread) = match rows {
            Rows::Held(ds) => (ds, Unreadable::default()),
            Rows::Source(l) => l.load_all_counted().map_err(|e| format!("{name}: {e}"))?,
        };
        if ds.coord_crs() != rustgis_data::raster::Crs::LonLat {
            ds = ds.in_lonlat()?.into_owned();
        }
        Ok((ds, Unread::of(name, unread)))
    }
}

impl Unread {
    /// What a whole read of `layer` couldn't read, to say.
    pub(crate) fn of(layer: String, unread: Unreadable) -> Self {
        Unread { layer, rows: unread.rows, dates: unread.dates, rounded: unread.rounded, messages: unread.messages }
    }

    /// Says the rows left out, as Load All Features says them, the dates read as null and the
    /// date-times rounded to the microsecond (nothing when there were none).
    pub(crate) fn say(&self, s: &mut Session) {
        if self.rows > 0 {
            s.warn(unread_notice(&self.layer, self.rows));
        }
        if let Some(dates) = &self.dates {
            s.notify(format!("{}: {dates}", self.layer), false);
        }
        if let Some(rounded) = &self.rounded {
            s.notify(format!("{}: {rounded}", self.layer), false);
        }
        for message in &self.messages {
            s.notify(message.clone(), false);
        }
    }

    /// What [`Unread::say`] says, as lines: for a tool's messages.
    pub(crate) fn lines(&self) -> Vec<String> {
        let rows = (self.rows > 0).then(|| unread_notice(&self.layer, self.rows));
        let dates = [&self.dates, &self.rounded].into_iter().flatten().map(|n| format!("{}: {n}", self.layer));
        rows.into_iter().chain(dates).chain(self.messages.iter().cloned()).collect()
    }

    /// `error`, with the rows left out when there were any: they may be why it failed.
    pub(crate) fn explain(&self, error: String) -> String {
        if self.rows == 0 { error } else { format!("{error} ({})", unread_notice(&self.layer, self.rows)) }
    }
}

/// A whole read made before its command runs ([`super::reads`]): the source it read, the fields
/// the layer listed then, and what it read with the number of rows that couldn't be read.
type ReadAhead = (rustgis_map::source::SourceLayer, Vec<rustgis_data::Field>, Result<(Dataset, Unreadable), String>);

/// The key the whole read of `l` made ahead is taken by.
fn ahead_key(l: &Layer) -> String {
    format!("whole:{}", l.instance_id())
}

/// The whole read of the layer read in place that `p` names, made before `name` (the command that
/// takes it with [`take_or_read`]) runs, on a copy of the layer: `None` for a layer in memory.
pub(crate) fn ahead(s: &Session, p: &serde_json::Value, name: &'static str) -> Option<super::reads::ReadWork> {
    let l = s.map().layer(s.layer_id(p).ok()?)?;
    let mut copy = l.clone();
    let source = copy.source.as_mut()?;
    // Its own read: the live layer keeps the one under way.
    source.detach_read();
    let (copied, fields) = (source.clone(), l.dataset.fields.clone());
    Some(super::reads::ahead(name, ahead_key(l), move || (copied, fields, copy.load_all_counted())))
}

/// Every feature of layer `id` read in place, with the rows of its source that couldn't be read:
/// the read made ahead ([`ahead`]) when there is one from the source the layer still reads,
/// listing the fields it lists now; read here otherwise (one opened afresh since, or with other
/// fields, is read again).
pub(crate) fn take_or_read(s: &mut Session, id: u32) -> Result<(Dataset, Unread), String> {
    let key = ahead_key(s.map().layer(id).ok_or("no such layer")?);
    let ahead = super::reads::take::<ReadAhead>(s, &key);
    let l = s.map().layer(id).ok_or("no such layer")?;
    let (ds, unread) = match ahead {
        Some((copied, fields, read)) if l.source.as_ref().is_some_and(|src| src.same_opening(&copied)) && fields == l.dataset.fields => {
            read?
        }
        _ => l.load_all_counted()?,
    };
    Ok((ds, Unread::of(l.name.clone(), unread)))
}

/// Loads a layer read in place whole, in place of its view, as an export's copy of the project
/// needs it: its features are then all of its source's, its selection is kept (by feature id),
/// and it no longer reads its source. A layer in memory is left as it is. The read looks at
/// Cancel as [`Whole::read`] does.
pub(crate) fn load_into(l: &mut Layer) -> Result<Unread, String> {
    let name = l.name.clone();
    if !l.is_streamed() {
        return Ok(Unread::of(name, Unreadable::default()));
    }
    let selected = l.selected_ids();
    let (ds, unread) = l.load_all_counted().map_err(|e| format!("{name}: {e}"))?;
    l.dataset = ds;
    l.dataset.windowed = false;
    l.source = None;
    l.selection = l.dataset.features.iter().enumerate().filter(|(_, f)| selected.contains(&f.id)).map(|(i, _)| i).collect();
    l.touch_geometry();
    Ok(Unread::of(name, unread))
}

/// The notice for `unread` rows of `layer`'s source that couldn't be read.
pub(crate) fn unread_notice(layer: &str, unread: usize) -> String {
    match unread {
        1 => format!("{layer}: 1 row couldn't be read and was left out"),
        n => format!("{layer}: {n} rows couldn't be read and were left out"),
    }
}

/// The layers read in place that `command` with `params` reads whole from their sources (for its
/// permission effects, which then reach those sources as Load All Features does).
pub(crate) fn layers_read_whole<'a>(s: &super::InputContext<'a>, command: &str, params: &serde_json::Value) -> Vec<&'a Layer> {
    let layers: Vec<&'a Layer> = match command {
        "dl.train" | "dl.classify" | "raster.classifySupervised" => {
            super::ps(params, "training").and_then(|n| s.map().layer_by_name(n)).into_iter().collect()
        }
        "labels.convert" => s.layer_id(params).ok().and_then(|id| s.map().layer(id)).into_iter().collect(),
        "report.export" => super::report::index(s, params)
            .map(|i| super::report::reported_layers(s, i))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(m, id)| s.project.maps.get(m)?.layer(id))
            .collect(),
        _ => Vec::new(),
    };
    layers.into_iter().filter(|l| l.is_streamed()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A detached read can be the first opening: its source metadata never reaches the live
    /// layer, so the command and export must carry the diagnostics with the rows they read.
    #[test]
    fn detached_whole_reads_relay_new_metadata_and_leave_clean_reads_quiet() {
        for (tag, coordinates, lossy) in [("3d", "[1,2,3]", true), ("xy", "[1,2]", false)] {
            let path = std::env::temp_dir().join(format!("rustgis-whole-notices-{}-{tag}.geojson", std::process::id()));
            let coordinates: serde_json::Value = serde_json::from_str(coordinates).unwrap();
            let text = serde_json::json!({"type":"FeatureCollection","features":[{
                "type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":coordinates}
            }]})
            .to_string();
            std::fs::write(&path, text).unwrap();
            let mut layer = Layer::new(1, Dataset::new("sites", rustgis_data::GeometryType::Point, Vec::new()));
            let mut source = rustgis_map::source::SourceLayer::default();
            source.path = Some(path.display().to_string());
            layer.source = Some(source);
            let (dataset, unread) = Whole::of(&layer).read().unwrap();
            assert_eq!(dataset.features.len(), 1);
            assert_eq!(dataset.zm_dropped, lossy);
            assert!(layer.source.as_ref().unwrap().info().is_none(), "the live source remains unopened");
            assert_eq!(unread.lines().iter().filter(|n| n.contains("Z or M")).count(), usize::from(lossy));
            let mut session = Session::new(crate::Project::new("notices"));
            unread.say(&mut session);
            assert_eq!(session.notices.len(), usize::from(lossy));
            let (_, lost) = layer.load_all_counted().unwrap();
            let export = super::super::share::read_notes("out.kml", &layer.name, &lost);
            assert_eq!(export.len(), usize::from(lossy));
            assert!(export.iter().all(|n| n.starts_with("out.kml: ")));
            std::fs::remove_file(path).unwrap();
        }
    }

    /// Commands and tools keep non-date diagnostics alongside the existing specialized dates.
    #[test]
    fn whole_read_messages_reach_both_session_and_tool_consumers_once() {
        let unread = Unread::of(
            "sites".into(),
            Unreadable {
                dates: Some("1 value isn't a date and was read as empty, in 'day'".into()),
                messages: vec!["source: conversion left out 2 source features".into()],
                ..Default::default()
            },
        );
        let mut session = Session::new(crate::Project::new("notices"));
        unread.say(&mut session);
        assert_eq!(session.notices.iter().map(|n| &n.text).collect::<Vec<_>>(), unread.lines().iter().collect::<Vec<_>>());
        assert_eq!(session.notices.len(), 2);
        assert_eq!(unread.lines().iter().filter(|n| n.contains("conversion left out 2")).count(), 1);
        assert!(Unread::of("clean".into(), Unreadable::default()).lines().is_empty());
    }

    /// A fresh counted Z/M report replaces the export's generic loss notice, while a resident
    /// layer that lost its heights earlier still needs that generic export warning.
    #[test]
    fn whole_exports_report_dimension_loss_once_and_keep_resident_warnings() {
        let root = std::env::temp_dir().join(format!("rustgis-whole-export-notices-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for (tag, coordinates, old_loss, expected) in [
            ("3d", Some(serde_json::json!([1, 2, 3])), false, 1),
            ("xy", Some(serde_json::json!([1, 2])), false, 0),
            ("resident", None, true, 1),
        ] {
            let mut ds = Dataset::new("sites", rustgis_data::GeometryType::Point, Vec::new());
            ds.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(1.0, 2.0))), Vec::new());
            ds.zm_dropped = old_loss;
            let mut layer = Layer::new(1, ds);
            if let Some(coordinates) = coordinates {
                let input = root.join(format!("{tag}.geojson"));
                let text = serde_json::json!({"type":"FeatureCollection","features":[{
                    "type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":coordinates}
                }]})
                .to_string();
                std::fs::write(&input, text).unwrap();
                let mut source = rustgis_map::source::SourceLayer::default();
                source.path = Some(input.display().to_string());
                layer.source = Some(source);
            }
            let mut session = Session::new(crate::Project::new("notices"));
            let id = session.map_mut().add_layer(layer);
            let output = root.join(format!("{tag}.kml"));
            session.run("share.exportLayer", &serde_json::json!({"layer":id,"path":output})).unwrap();
            let notes: Vec<_> = session.notices.iter().filter(|n| n.text.contains("Z or M")).collect();
            assert_eq!(notes.len(), expected, "{tag}: {notes:?}");
            if tag == "3d" {
                assert!(notes[0].text.contains("1 feature had Z or M"), "the counted reader report survives");
            } else if old_loss {
                assert!(notes[0].text.contains("its geometry has no Z or M"), "the older loss is still reported");
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
