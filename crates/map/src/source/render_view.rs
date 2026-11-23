//! The features a streamed layer's last view read and the geometry its drawing made of them
//! (projected, and thinned for display), kept so that a frame of a view inside that one draws
//! without reading, parsing or projecting them again (#1065). Shared by render snapshots, like
//! the thinning decisions ([`super::render_lod`]), and bounded by [`KEPT_VIEW_BYTES`] and the
//! room the source's share of the map's view memory leaves.
use rustgis_data::source::FeatureSource;
use rustgis_geom::Extent;
use std::sync::{Arc, Weak};

use crate::Layer;

/// The most a kept view holds: its rows and the geometry drawn from them
/// ([`Layer::view_cache_bytes`]). A view with more, or with more than the source's share of the
/// map's view memory leaves beside its interaction rows, is drawn without being kept.
pub const KEPT_VIEW_BYTES: usize = crate::lod::CACHE_BYTES;

#[derive(Default)]
pub(super) struct KeptView {
    source: Option<Weak<dyn FeatureSource>>,
    /// What its geometry was drawn for: the projection ([`crate::Projection::code`]) and the
    /// datum grids there were then, as the renderer names them.
    key: String,
    /// The longitude/latitude box it was read for: every feature whose box meets it.
    read: Extent,
    /// The chunks drawn from it in turn, in the source's order.
    chunks: Arc<Vec<Layer>>,
    /// What the chunks hold ([`Layer::view_cache_bytes`]).
    bytes: usize,
    /// The coarsest resolution (map units a pixel) a view's read was found too large to keep at,
    /// for this key: views as coarse or coarser read only what they draw, and keep nothing.
    too_large_at: Option<f64>,
}

impl KeptView {
    /// Forgets everything kept for another source or key.
    fn follow(&mut self, source: &Arc<dyn FeatureSource>, key: &str) {
        let same = self.source.as_ref().and_then(Weak::upgrade).is_some_and(|old| Arc::ptr_eq(&old, source));
        if !same || self.key != key {
            *self = KeptView { source: Some(Arc::downgrade(source)), key: key.into(), ..KeptView::default() };
        }
    }

    /// Forgets the chunks, keeping what was learnt of sizes.
    fn drop_chunks(&mut self) {
        self.read = Extent::EMPTY;
        self.chunks = Arc::default();
        self.bytes = 0;
    }

    /// The kept chunks, when they were read for a box holding `near` and hold at most `room`
    /// bytes; ones holding more (the source's share shrank) are let go.
    pub(super) fn get(&mut self, source: &Arc<dyn FeatureSource>, key: &str, near: &Extent, room: usize) -> Option<Arc<Vec<Layer>>> {
        self.follow(source, key);
        if self.bytes > room {
            self.drop_chunks();
        }
        let r = &self.read;
        // A view read with nothing in it is kept too (no chunks): its frames draw nothing. With
        // nothing kept, `read` is empty and holds no box.
        let holds = r.min_x <= near.min_x && r.min_y <= near.min_y && r.max_x >= near.max_x && r.max_y >= near.max_y;
        holds.then(|| Arc::clone(&self.chunks))
    }

    /// Lets the chunks go when they hold more than `room` bytes.
    pub(super) fn trim(&mut self, room: usize) {
        if self.bytes > room {
            self.drop_chunks();
        }
    }

    /// Whether a view at resolution `res` is worth reading wider than it draws to keep: not
    /// when one as coarse or coarser was already too large to keep.
    pub(super) fn worth_keeping(&mut self, source: &Arc<dyn FeatureSource>, key: &str, res: f64) -> bool {
        self.follow(source, key);
        self.too_large_at.is_none_or(|at| res < at)
    }

    /// Keeps `chunks` of `bytes`, read for `read`, in place of what was kept; `None` when they
    /// came to more than may be kept at resolution `res`, which is remembered.
    pub(super) fn put(&mut self, source: &Arc<dyn FeatureSource>, key: &str, read: Extent, res: f64, chunks: Option<(Vec<Layer>, usize)>) {
        self.follow(source, key);
        match chunks {
            Some((chunks, bytes)) => {
                self.read = read;
                self.chunks = Arc::new(chunks);
                self.bytes = bytes;
            }
            None => {
                self.drop_chunks();
                self.too_large_at = Some(self.too_large_at.map_or(res, |at| at.min(res)));
            }
        }
    }

    /// Puts `chunks` of `bytes` (the kept ones drawn again, with the geometry that drawing made,
    /// a finer level of detail say) in place of `drawn`, when those are still what is kept;
    /// another source or key kept since is left as it is.
    pub(super) fn refresh(
        &mut self,
        source: &Arc<dyn FeatureSource>,
        key: &str,
        drawn: &Arc<Vec<Layer>>,
        chunks: Vec<Layer>,
        bytes: usize,
    ) {
        let same = self.source.as_ref().and_then(Weak::upgrade).is_some_and(|old| Arc::ptr_eq(&old, source));
        if same && self.key == key && Arc::ptr_eq(&self.chunks, drawn) {
            self.chunks = Arc::new(chunks);
            self.bytes = bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::source::{Batch, Request, SourceInfo};

    struct Empty(SourceInfo);
    impl FeatureSource for Empty {
        fn info(&self) -> &SourceInfo {
            &self.0
        }
        fn read(&self, _: &Request) -> rustgis_data::Result<Batch> {
            Ok(Batch::default())
        }
    }

    /// A view too large to keep stops wider reads at its resolution and coarser ones, not at
    /// finer ones; another projection or source starts afresh.
    #[test]
    fn a_view_too_large_to_keep_stops_wide_reads_as_coarse_or_coarser() {
        let info = SourceInfo {
            name: "s".into(),
            format: "test",
            geometry_type: rustgis_data::GeometryType::Point,
            fields: Vec::new(),
            extent: Extent::EMPTY,
            count: Some(0),
            crs: "EPSG:4326".into(),
            indexed: true,
            notes: Default::default(),
            zm_dropped: false,
        };
        let source: Arc<dyn FeatureSource> = Arc::new(Empty(info.clone()));
        let mut kept = KeptView::default();
        assert!(kept.worth_keeping(&source, "3857", 100.0));
        kept.put(&source, "3857", Extent::new(0.0, 0.0, 1.0, 1.0), 100.0, None);
        assert!(!kept.worth_keeping(&source, "3857", 100.0));
        assert!(!kept.worth_keeping(&source, "3857", 200.0));
        assert!(kept.worth_keeping(&source, "3857", 50.0));
        assert!(kept.get(&source, "3857", &Extent::new(0.2, 0.2, 0.4, 0.4), usize::MAX).is_none(), "nothing kept");
        assert!(kept.worth_keeping(&source, "robinson", 100.0), "another projection");
        let other: Arc<dyn FeatureSource> = Arc::new(Empty(info));
        assert!(kept.worth_keeping(&other, "3857", 100.0), "another source");
    }
}
