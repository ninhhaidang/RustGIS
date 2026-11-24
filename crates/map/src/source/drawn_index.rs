//! A streamed source's features indexed by their boxes as drawn on the map (#2788): in map units,
//! after projection, the boxes a frame culls by ([`crate::Layer::projected_extents`]). A frame
//! that reads only the features whose drawn boxes meet its cull extent draws what reading every
//! feature draws, in any projection and from any coordinate system, where the source's own boxes
//! can't say which those are. Built from a frame that read every feature, for one projection and
//! set of datum grids, and shared by render snapshots like the kept view ([`super::render_view`]).
use rustgis_data::source::FeatureSource;
use std::sync::{Arc, Weak};

/// How many boxes a node of the index holds.
const NODE: usize = 16;

/// The memory a feature takes in an index: its id and box (20 bytes), sorted in place where they
/// were gathered, and its share of the nodes above them (a box over 16 features, a box over 256,
/// …: a little over a byte), rounded up.
pub const PACKING_BYTES: usize = 22;

/// Boxes (f32, rounded outward) packed into a static R-tree: leaves of [`NODE`] boxes sorted by
/// the Sort-Tile-Recursive method (Leutenegger, Lopez and Edgington, "STR: a simple and efficient
/// algorithm for R-tree packing", 1997), each level above them the boxes of the nodes below.
pub struct DrawnBoxes {
    /// The features' ids and boxes, sorted into leaves of `NODE`.
    leaves: Vec<(u32, [f32; 4])>,
    /// From just above the leaves up: each level's boxes, a node's children `NODE` boxes of the
    /// level below.
    nodes: Vec<Vec<[f32; 4]>>,
}

/// `b` met by `q`, both boxes as `[min x, min y, max x, max y]`.
fn meets(b: &[f32; 4], q: &[f32; 4]) -> bool {
    b[0] <= q[2] && b[2] >= q[0] && b[1] <= q[3] && b[3] >= q[1]
}

/// The box around `boxes`.
fn around<'a>(boxes: impl Iterator<Item = &'a [f32; 4]>) -> [f32; 4] {
    boxes.fold([f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY], |a, b| {
        [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
    })
}

impl DrawnBoxes {
    /// The index of these features' drawn boxes (feature id, box), sorted where they lie: the
    /// only memory it adds is the nodes above them. `None` when those can't be held, or `stop`
    /// says to give up (it is asked between the steps). Each of its two sorts is one step that
    /// doesn't look at `stop`: about a second for millions of features, on the worker that packs
    /// it, never the frame.
    pub fn new(mut boxes: Vec<(u32, [f32; 4])>, stop: impl Fn() -> bool) -> Option<DrawnBoxes> {
        let centre = |b: &[f32; 4], axis: usize| f64::from(b[axis]) + f64::from(b[axis + 2]);
        // Sort-Tile-Recursive: in vertical slices of whole leaves by x, each slice by y.
        boxes.sort_unstable_by(|a, b| centre(&a.1, 0).total_cmp(&centre(&b.1, 0)));
        if stop() {
            return None;
        }
        let leaves = boxes.len().div_ceil(NODE);
        let slice = (leaves as f64).sqrt().ceil().max(1.0) as usize * NODE;
        for s in boxes.chunks_mut(slice) {
            s.sort_unstable_by(|a, b| centre(&a.1, 1).total_cmp(&centre(&b.1, 1)));
        }
        if stop() {
            return None;
        }
        boxes.shrink_to_fit();
        let mut nodes: Vec<Vec<[f32; 4]>> = Vec::new();
        let mut up = Vec::new();
        up.try_reserve_exact(leaves).ok()?;
        up.extend(boxes.chunks(NODE).map(|c| around(c.iter().map(|(_, b)| b))));
        while up.len() > 1 {
            let mut next = Vec::new();
            next.try_reserve_exact(up.len().div_ceil(NODE)).ok()?;
            next.extend(up.chunks(NODE).map(|c| around(c.iter())));
            nodes.push(std::mem::replace(&mut up, next));
        }
        nodes.push(up);
        Some(DrawnBoxes { leaves: boxes, nodes })
    }

    /// How many features it holds.
    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// The memory it holds.
    pub fn bytes(&self) -> usize {
        self.leaves.capacity() * std::mem::size_of::<(u32, [f32; 4])>()
            + self.nodes.iter().map(|l| l.capacity() * std::mem::size_of::<[f32; 4]>()).sum::<usize>()
    }

    /// The ids of the features whose boxes meet `q`, in order; `None` when more than `most` do,
    /// or `stop` says to give up (it is asked as the search goes).
    pub fn query(&self, q: [f32; 4], most: usize, stop: impl Fn() -> bool) -> Option<Vec<u64>> {
        if stop() {
            return None;
        }
        let mut out = Vec::new();
        // The levels from the top down, the leaves (level 0) last.
        let top = self.nodes.len();
        let mut stack = vec![(top, 0usize)];
        let mut looked = 0usize;
        while let Some((level, i)) = stack.pop() {
            looked += 1;
            if looked.is_multiple_of(4096) && stop() {
                return None;
            }
            if level == 0 {
                if let Some((id, b)) = self.leaves.get(i)
                    && meets(b, &q)
                {
                    out.push(u64::from(*id));
                    if out.len() > most {
                        return None;
                    }
                }
                continue;
            }
            let Some(b) = self.nodes.get(level - 1).and_then(|l| l.get(i)) else { continue };
            if !meets(b, &q) {
                continue;
            }
            let below = if level == 1 { self.leaves.len() } else { self.nodes.get(level - 2).map_or(0, Vec::len) };
            stack.extend((i * NODE..((i + 1) * NODE).min(below)).map(|c| (level - 1, c)));
        }
        out.sort_unstable();
        Some(out)
    }
}

/// The index kept for a source: for one projection and set of datum grids (its key), or none,
/// with whether one was found too large to keep for them.
#[derive(Default)]
pub(super) struct DrawnIndex {
    source: Option<Weak<dyn FeatureSource>>,
    key: String,
    boxes: Option<Arc<DrawnBoxes>>,
    /// The memory an index for this key was found to need when it didn't fit: one is gathered
    /// again once that much is free.
    too_large: Option<usize>,
    /// The latest room the map gave the index ([`DrawnIndex::trim`]): one packed while the room
    /// shrank isn't kept past it.
    room: Option<usize>,
    /// The ticket of the index being packed for this key, off the frame that gathered its boxes:
    /// only that packing may keep its index.
    building: Option<u64>,
    /// The last ticket given.
    tickets: u64,
}

impl DrawnIndex {
    /// Forgets everything kept for another source or key.
    fn follow(&mut self, source: &Arc<dyn FeatureSource>, key: &str) {
        let same = self.source.as_ref().and_then(Weak::upgrade).is_some_and(|old| Arc::ptr_eq(&old, source));
        if !same || self.key != key {
            *self = DrawnIndex {
                source: Some(Arc::downgrade(source)),
                key: key.into(),
                tickets: self.tickets,
                room: self.room,
                ..DrawnIndex::default()
            };
        }
    }

    /// The index for this key, if one is kept.
    pub(super) fn get(&mut self, source: &Arc<dyn FeatureSource>, key: &str) -> Option<Arc<DrawnBoxes>> {
        self.follow(source, key);
        self.boxes.clone()
    }

    /// Whether a frame reading every feature should gather their boxes to make one: not when one
    /// is kept or was too large for this key.
    pub(super) fn wanted(&mut self, source: &Arc<dyn FeatureSource>, key: &str, share: usize) -> bool {
        self.follow(source, key);
        self.boxes.is_none() && self.too_large.is_none_or(|need| need <= share) && self.building.is_none()
    }

    /// Marks an index as being packed for this key, and gives the ticket it is kept with; `None`
    /// when one is kept, being packed, or was too large.
    pub(super) fn begin(&mut self, source: &Arc<dyn FeatureSource>, key: &str, share: usize) -> Option<u64> {
        if !self.wanted(source, key, share) {
            return None;
        }
        self.tickets += 1;
        self.building = Some(self.tickets);
        self.building
    }

    /// The ticket of the index being packed, if one is.
    pub(super) fn building_ticket(&self) -> Option<u64> {
        self.building
    }

    /// Whether one is being packed for this key.
    pub(super) fn building(&mut self, source: &Arc<dyn FeatureSource>, key: &str) -> bool {
        self.follow(source, key);
        self.building.is_some()
    }

    /// Keeps the index packed with `ticket`, when that packing is still the one this slot waits
    /// for (not one for a projection, datum grids or source since replaced) and it fits `share`
    /// and the latest room: the room left beside it when it was kept, `None` when it wasn't.
    /// `None` boxes (its packing couldn't start, or stopped) keep none, to be gathered again.
    pub(super) fn finish(&mut self, ticket: u64, boxes: Option<DrawnBoxes>, share: usize) -> Option<usize> {
        if self.building != Some(ticket) {
            return None;
        }
        self.building = None;
        let room = self.room.map_or(share, |r| r.min(share));
        match boxes {
            Some(b) if b.bytes() <= room => {
                let left = room - b.bytes();
                self.boxes = Some(Arc::new(b));
                Some(left)
            }
            Some(b) => {
                self.too_large = Some(b.bytes());
                None
            }
            None => None,
        }
    }

    /// Keeps no index for this key: its boxes, `need` bytes, were too large to gather.
    pub(super) fn refuse(&mut self, source: &Arc<dyn FeatureSource>, key: &str, need: usize) {
        self.follow(source, key);
        self.too_large = Some(need);
        self.boxes = None;
    }

    /// The memory the kept index holds.
    pub(super) fn bytes(&self) -> usize {
        self.boxes.as_ref().map_or(0, |b| b.bytes())
    }

    /// Lets the index go when it holds more than `room` bytes.
    pub(super) fn trim(&mut self, room: usize) {
        self.room = Some(room);
        let bytes = self.bytes();
        if bytes > room {
            self.boxes = None;
            self.too_large = Some(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packing keeps its index only while the slot still waits for it: not after the slot
    /// moved to another projection (key) and back, nor after another packing began.
    #[test]
    fn a_packing_keeps_its_index_only_while_the_slot_waits_for_it() {
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
        let info = SourceInfo {
            name: "s".into(),
            format: "test",
            geometry_type: rustgis_data::GeometryType::Polygon,
            fields: Vec::new(),
            extent: rustgis_geom::Extent::EMPTY,
            count: Some(1),
            crs: "EPSG:4326".into(),
            indexed: true,
            notes: Default::default(),
            zm_dropped: false,
        };
        let source: Arc<dyn FeatureSource> = Arc::new(Empty(info));
        let one = || DrawnBoxes::new(vec![(1, [0.0, 0.0, 1.0, 1.0])], || false);
        let mut slot = DrawnIndex::default();
        let a = slot.begin(&source, "a", 1000).unwrap();
        assert!(slot.begin(&source, "a", 1000).is_none(), "one packing at a time");
        // The view moved to another projection, and back: the first packing is stale.
        assert!(slot.begin(&source, "b", 1000).is_some());
        assert!(slot.get(&source, "a").is_none());
        let again = slot.begin(&source, "a", 1000).unwrap();
        assert!(slot.finish(a, one(), 1000).is_none(), "a stale packing keeps nothing");
        assert!(slot.finish(again, one(), 1000).is_some());
        assert!(slot.get(&source, "a").is_some_and(|i| i.len() == 1));
        // The room shrank while one was packed: it isn't kept, and is again once there is room.
        slot.trim(0);
        assert!(slot.get(&source, "a").is_none());
        assert!(slot.begin(&source, "a", 1).is_none(), "too large for the room now");
        let t = slot.begin(&source, "a", 1000).unwrap();
        assert!(slot.finish(t, one(), 1000).is_none(), "the latest room is 0");
        slot.trim(600);
        let t = slot.begin(&source, "a", 1000).unwrap();
        let left = slot.finish(t, one(), 1000);
        assert!(left.is_some_and(|l| l < 600), "what the latest room leaves beside it: {left:?}");
    }

    /// The index finds what a scan of every box finds, in id order, over boxes of every size,
    /// none, one, and a query of nothing.
    #[test]
    fn the_index_finds_what_a_scan_finds() {
        let mut boxes = Vec::new();
        let mut seed = 7u64;
        let mut next = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as f32 / (1u64 << 31) as f32
        };
        for id in 0..5_000u32 {
            let (x, y, w, h) = (next() * 1000.0, next() * 1000.0, next() * next() * 200.0, next() * 3.0);
            boxes.push((id * 3 + 1, [x, y, x + w, y + h]));
        }
        let index = DrawnBoxes::new(boxes.clone(), || false).unwrap();
        assert_eq!(index.len(), 5_000);
        for q in [[0.0f32, 0.0, 1000.0, 1000.0], [400.0, 400.0, 420.0, 401.0], [-10.0, -10.0, -1.0, -1.0], [500.0, 0.0, 500.0, 1000.0]] {
            let mut scan: Vec<u64> = boxes.iter().filter(|(_, b)| meets(b, &q)).map(|(id, _)| u64::from(*id)).collect();
            scan.sort_unstable();
            assert_eq!(index.query(q, usize::MAX, || false), Some(scan.clone()), "{q:?}");
            // Past `most`, none: the caller reads everything.
            assert_eq!(index.query(q, scan.len().saturating_sub(1), || false).is_some(), scan.is_empty(), "{q:?}");
        }
        assert_eq!(DrawnBoxes::new(Vec::new(), || false).unwrap().query([0.0, 0.0, 1.0, 1.0], 9, || false), Some(vec![]));
        let one = DrawnBoxes::new(vec![(9, [1.0, 1.0, 2.0, 2.0])], || false).unwrap();
        assert_eq!(
            (one.query([0.0, 0.0, 1.0, 1.0], 9, || false), one.query([3.0, 3.0, 4.0, 4.0], 9, || false)),
            (Some(vec![9]), Some(vec![]))
        );
        assert!(DrawnBoxes::new(boxes.clone(), || true).is_none(), "a stopped packing gives none");
        assert_eq!(index.query([0.0, 0.0, 1000.0, 1000.0], usize::MAX, || true), None, "a stopped search gives none");
    }
}
