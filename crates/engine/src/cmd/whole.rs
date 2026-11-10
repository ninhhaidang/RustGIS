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
    let names: Vec<&str> = match command {
        "dl.train" | "dl.classify" | "raster.classifySupervised" => super::ps(params, "training").into_iter().collect(),
        _ => Vec::new(),
    };
    names.into_iter().filter_map(|n| s.map().layer_by_name(n)).filter(|l| l.is_streamed()).collect()
}
