//! Bounded, shared session-only constructions. Snapshots accompany undo and background exports.
//! Records are guarded by feature identity, source generation and exact geometry equality.
use crate::Session;
use crate::box_edit::BoxSpec;
use rustgis_data::Geometry;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Session-only shape parameters shared by the bounded construction cache.
#[derive(Clone)]
pub(crate) enum Construction {
    Box(BoxSpec),
    Circle(crate::circle_edit::CircleSpec),
}
impl Construction {
    pub(crate) fn box_spec(&self) -> Option<&BoxSpec> {
        if let Self::Box(b) = self { Some(b) } else { None }
    }
    pub(crate) fn circle_spec(&self) -> Option<&crate::circle_edit::CircleSpec> {
        if let Self::Circle(c) = self { Some(c) } else { None }
    }
}
type Key = (u64, u64, u64, u64);
#[derive(Clone)]
pub(crate) struct Record {
    pub(crate) spec: Construction,
    pub(crate) geometry: Geometry,
}
type Shard = Arc<BTreeMap<Key, Arc<Record>>>;

#[derive(Default)]
pub(crate) struct Counts {
    boxes: usize,
    circles: usize,
}
impl Counts {
    pub(crate) fn notes(self) -> Vec<String> {
        let boxes = (self.boxes > 0).then(|| format!("{} rotated box(es): session-only construction is not included in this output and will not survive reimport; the source polygons use four transformed corners, with curved grid edges approximated by straight edges", self.boxes));
        let circles = (self.circles > 0).then(|| format!("{} circle(s): session-only centre/radius construction is not included in this output and will not survive reimport; source polygons approximate circles with straight edges (tolerance in each construction grid, before projection)", self.circles));
        boxes.into_iter().chain(circles).collect()
    }
}

/// Immutable snapshots accompany undo and background exports. An edit copies at most 512
/// record references and the 256-entry directory; construction geometries remain shared.
#[derive(Clone, Default)]
pub(crate) struct Cache {
    shards: Arc<Vec<Shard>>,
    pub(crate) len: usize,
}
impl Cache {
    pub(crate) fn key(s: &Session, layer: u32, row: usize) -> Option<Key> {
        let l = s.map().layer(layer)?;
        Some((s.map().instance_id(), l.instance_id(), l.replacement_revision(), l.dataset.features.get(row)?.id))
    }
    fn shard(key: Key) -> usize {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hash);
        (hash.finish() % 256) as usize
    }
    pub(crate) fn get(&self, key: Key, geometry: &Geometry) -> Option<&Record> {
        self.shards.get(Self::shard(key))?.get(&key).map(|r| r.as_ref()).filter(|r| r.geometry == *geometry)
    }
    pub(crate) fn room(&self) -> Result<(), String> {
        if self.len >= 100_000 {
            Err("this session already holds 100000 shape constructions; start a new session for more".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn remember(&mut self, key: Key, record: Record) -> Result<(), String> {
        let at = Self::shard(key);
        if self.shards.is_empty() {
            let empty = Arc::new(BTreeMap::new());
            self.shards = Arc::new(vec![empty; 256]);
        }
        let shards = Arc::make_mut(&mut self.shards);
        let shard = shards.get_mut(at).ok_or("shape cache is unavailable")?;
        let exists = shard.contains_key(&key);
        if !exists && shard.len() >= 512 {
            return Err("shape construction cache capacity reached; start a new session for more".into());
        }
        Arc::make_mut(shard).insert(key, Arc::new(record));
        if !exists {
            self.len += 1;
        }
        Ok(())
    }
    fn remove(&mut self, key: Key) {
        let at = Self::shard(key);
        if self.shards.get(at).is_none_or(|s| !s.contains_key(&key)) {
            return;
        }
        if let Some(shard) = Arc::make_mut(&mut self.shards).get_mut(at)
            && Arc::make_mut(shard).remove(&key).is_some()
        {
            self.len = self.len.saturating_sub(1);
        }
    }
    /// Follow source-edit ID reassignment without reviving stale constructions.
    pub(crate) fn rekey(&mut self, key: Key, id: u64, geometry: Option<&Geometry>) -> Result<(), String> {
        if let Some(record) = geometry.and_then(|g| self.get(key, g)).cloned() {
            self.remove(key);
            self.remember((key.0, key.1, key.2, id), record)?;
        }
        Ok(())
    }
    pub(crate) fn count_feature(&self, map: u64, layer: &rustgis_map::Layer, feature: &rustgis_data::Feature, counts: &mut Counts) {
        let key = (map, layer.instance_id(), layer.replacement_revision(), feature.id);
        if let Some(record) = feature.geometry.as_ref().and_then(|g| self.get(key, g)) {
            match record.spec {
                Construction::Box(_) => counts.boxes += 1,
                Construction::Circle(_) => counts.circles += 1,
            }
        }
    }
    pub(crate) fn export_notes(&self, map: u64, layer: &rustgis_map::Layer) -> Result<Vec<String>, String> {
        self.output_notes(std::iter::once((map, layer)))
    }
    /// Count only unchanged, live constructions from the output's source layers. Exports call
    /// this on their worker. Document writers call it in their existing synchronous durable
    /// transaction, before output, with the same cancellation context as serialization.
    pub(crate) fn output_notes<'a>(&self, layers: impl IntoIterator<Item = (u64, &'a rustgis_map::Layer)>) -> Result<Vec<String>, String> {
        if self.len == 0 {
            return Ok(Vec::new());
        }
        let mut counts = Counts::default();
        for (map, layer) in layers {
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            for (row, feature) in layer.dataset.features.iter().enumerate() {
                if row.is_multiple_of(256) {
                    rustgis_data::progress::check().map_err(|e| e.to_string())?;
                }
                self.count_feature(map, layer, feature, &mut counts);
            }
        }
        Ok(counts.notes())
    }
}
/// Forget the live record for insertion/removal paths that call this hook (`design::apply`).
/// This prevents id reuse in those paths from adopting construction; undo keeps its own cache.
/// Sync and restore paths may retain records, guarded by the shape accessors' geometry-equality checks.
pub(crate) fn forget_feature(s: &mut Session, layer: u32, feature: u64) {
    if let Some(l) = s.map().layer(layer) {
        let key = (s.map().instance_id(), l.instance_id(), l.replacement_revision(), feature);
        s.constructions.remove(key);
    }
}

impl Session {
    /// Counted losses for a layer output, including browser downloads. Call before encoding
    /// and present the notes only after the output has been accepted or written.
    pub fn layer_construction_output_notes(&self, layer: u32) -> Result<Vec<String>, String> {
        let l = self.map().layer(layer).ok_or("no such layer")?;
        self.constructions.export_notes(self.map().instance_id(), l)
    }
    /// Counted runtime-construction losses for a whole-project output. Embedders saving the
    /// public `Project` directly should present these notes too: `Project` owns no session cache.
    /// Explicit save/download paths call this inside their existing durable output transaction;
    /// unlike the live session, the serialized document contains only polygon geometry.
    pub fn construction_output_notes(&self) -> Result<Vec<String>, String> {
        self.constructions.output_notes(self.project.maps.iter().flat_map(|m| m.layers.iter().map(move |l| (m.instance_id(), l))))
    }
}
