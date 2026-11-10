//! Every feature of a layer, for a command whose result stands for the whole layer (a model
//! trained from it, a classification): a layer read in place holds only the features of the
//! last view, so its source is read whole, as Load All Features reads it, and the rows that
//! couldn't be read are said. As for an in-memory layer, every row is read, whatever the layer's
//! definition query.

use rustgis_data::Dataset;
use rustgis_map::Layer;

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

/// The rows of a layer's source that a whole read couldn't read and left out.
pub(crate) struct Unread {
    layer: String,
    rows: usize,
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

    /// Every feature, with the fields the layer lists (so a field's index in the layer is its
    /// index here), and the rows of its source that couldn't be read. A source read a chunk at a
    /// time looks at Cancel between chunks; one read at once (a GeoJSON file, a database table)
    /// is a single step that doesn't, as for Load All Features.
    pub(crate) fn read(self) -> Result<(Dataset, Unread), String> {
        let Whole { name, rows } = self;
        let (ds, rows) = match rows {
            Rows::Held(ds) => (ds, 0),
            Rows::Source(l) => l.load_all_counted().map_err(|e| format!("{name}: {e}"))?,
        };
        Ok((ds, Unread { layer: name, rows }))
    }
}

impl Unread {
    /// Says the rows left out, as Load All Features says them (nothing when none were).
    pub(crate) fn say(&self, s: &mut Session) {
        if self.rows > 0 {
            s.warn(unread_notice(&self.layer, self.rows));
        }
    }

    /// `error`, with the rows left out when there were any: they may be why it failed.
    pub(crate) fn explain(&self, error: String) -> String {
        if self.rows == 0 { error } else { format!("{error} ({})", unread_notice(&self.layer, self.rows)) }
    }
}

/// A whole read made before its command runs ([`super::reads`]): the source it read, the fields
/// the layer listed then, and what it read with the number of rows that couldn't be read.
type ReadAhead = (rustgis_map::source::SourceLayer, Vec<rustgis_data::Field>, Result<(Dataset, usize), String>);

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
    let (ds, rows) = match ahead {
        Some((copied, fields, read)) if l.source.as_ref().is_some_and(|src| src.same_opening(&copied)) && fields == l.dataset.fields => {
            read?
        }
        _ => l.load_all_counted()?,
    };
    Ok((ds, Unread { layer: l.name.clone(), rows }))
}

/// Loads a layer read in place whole, in place of its view, as an export's copy of the project
/// needs it: its features are then all of its source's, its selection is kept (by feature id),
/// and it no longer reads its source. A layer in memory is left as it is. The read looks at
/// Cancel as [`Whole::read`] does.
pub(crate) fn load_into(l: &mut Layer) -> Result<Unread, String> {
    let name = l.name.clone();
    if !l.is_streamed() {
        return Ok(Unread { layer: name, rows: 0 });
    }
    let selected = l.selected_ids();
    let (ds, rows) = l.load_all_counted().map_err(|e| format!("{name}: {e}"))?;
    l.dataset = ds;
    l.dataset.windowed = false;
    l.source = None;
    l.selection = l.dataset.features.iter().enumerate().filter(|(_, f)| selected.contains(&f.id)).map(|(i, _)| i).collect();
    l.touch_geometry();
    Ok(Unread { layer: name, rows })
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
