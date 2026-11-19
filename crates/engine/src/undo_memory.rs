//! Budget for feature allocations retained by undo and redo snapshots.
use crate::{Project, Step, notice_text::NoticeText};
use rustgis_data::{Feature, Features, source::memory::feature_bytes};
use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::Weak,
};

type Key = Weak<Vec<Feature>>;

/// The allocation identities in a snapshot. The project keeps the corresponding rows alive.
pub(crate) type Allocations = Vec<Key>;

pub(crate) struct Budget {
    pub(crate) limit: usize,
    // Command dispatch also runs post-command maintenance, which can record a second step.
    pub(crate) deferred: bool,
    // Dropping steps is said once a project: past the budget every further edit drops one, and
    // a message each time would only repeat the first. Another project says it again.
    said: bool,
    // Keeping the weak key prevents address reuse while its estimate is cached, without keeping
    // the rows themselves alive. Copy-on-write gives changed rows a different key.
    estimates: HashMap<usize, (Key, usize)>,
}

impl Default for Budget {
    fn default() -> Self {
        Self { limit: rustgis_data::memory::undo_history(), deferred: false, said: false, estimates: HashMap::new() }
    }
}

impl Budget {
    pub(crate) fn clear(&mut self) {
        self.estimates.clear();
        self.said = false;
    }

    /// Keep a contiguous history around the current state. Count a shared allocation only at its
    /// highest-priority owner, so the estimate falls only once its last snapshot is evicted.
    pub(crate) fn trim(&mut self, live: &Project, undo: &mut Vec<Step>, redo: &mut Vec<Step>) -> Option<NoticeText> {
        let mut seen: HashSet<_> = rows(live).map(|features| features.allocation_key().as_ptr() as usize).collect();
        let mut used = 0usize;
        let undo_protected = usize::from(!undo.is_empty());
        let mut account = |step: &Step| {
            used = used.saturating_add(self.private_bytes(&step.8, &mut seen));
            used <= self.limit
        };

        // Work in reverse eviction order. Keep the newest undo even if it alone exceeds the
        // limit, then the nearest redos, then as many older undos as will fit.
        if let Some(newest) = undo.last() {
            account(newest);
        }
        let redo_keep = redo.iter().rev().take_while(|step| account(step)).count();
        let undo_keep = if redo_keep == redo.len() {
            undo_protected + undo.iter().rev().skip(undo_protected).take_while(|step| account(step)).count()
        } else {
            undo_protected
        };

        let undo_drop = undo.len() - undo_keep;
        let redo_drop = redo.len() - redo_keep;
        undo.drain(..undo_drop);
        redo.drain(..redo_drop);
        self.estimates.retain(|id, (key, _)| seen.contains(id) && key.strong_count() > 0);
        if undo_drop + redo_drop == 0 || std::mem::replace(&mut self.said, true) {
            return None;
        }
        let template = "Undo history is limited by memory: older steps are dropped as you edit, keeping Undo and Redo to about {limit}. The newest step is always kept.";
        let limit = if self.limit >= 1 << 30 {
            format!("{:.1} GiB", self.limit as f64 / (1_u64 << 30) as f64)
        } else if self.limit >= 1 << 20 {
            format!("{:.1} MiB", self.limit as f64 / (1_u64 << 20) as f64)
        } else {
            format!("{} B", self.limit)
        };
        Some(NoticeText::format(template, [("limit", limit.into())]))
    }

    /// Measure only history-owned allocations with no cached estimate. The other work visits
    /// cached allocation identities, independent of the number of rows.
    fn private_bytes(&mut self, allocations: &Allocations, seen: &mut HashSet<usize>) -> usize {
        allocations.iter().fold(0, |sum, key| {
            let id = key.as_ptr() as usize;
            if !seen.insert(id) {
                return sum;
            }
            let (_, bytes) = self.estimates.entry(id).or_insert_with(|| {
                let bytes = key.upgrade().as_deref().map_or(0, estimate);
                (key.clone(), bytes)
            });
            sum.saturating_add(*bytes)
        })
    }
}

/// Records identities once when the snapshot enters history or has a reservation removed.
pub(crate) fn allocations(project: &Project) -> Allocations {
    rows(project).map(Features::allocation_key).collect()
}

fn rows(project: &Project) -> impl Iterator<Item = &Features> {
    project
        .maps
        .iter()
        .chain(project.offline_areas.iter().map(|area| &area.data))
        .flat_map(|map| map.layers.iter().map(|layer| &layer.dataset.features))
        .chain(project.tables.iter().map(|table| &table.dataset.features))
        .filter(|features| features.capacity() > 0)
}

/// Count the row vector's capacity and extrapolate the heap storage of up to 64 evenly spaced
/// rows, using the same attribute, string and geometry accounting as streamed view windows.
fn estimate(features: &Vec<Feature>) -> usize {
    let stride = features.len().div_ceil(64).max(1);
    let (sum, count) =
        features.iter().step_by(stride).fold((0usize, 0usize), |(sum, count), row| (sum.saturating_add(feature_bytes(row)), count + 1));
    features.capacity().saturating_mul(size_of::<Feature>()).saturating_add(sum.div_ceil(count.max(1)).saturating_mul(features.len()))
}

#[cfg(test)]
mod tests {
    use crate::Session;
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
    use serde_json::json;

    fn session(text_len: usize) -> Session {
        let mut session = Session::new(crate::Project::default());
        let mut data = Dataset::new("rows", GeometryType::Point, vec![Field::new("text", FieldType::Text)]);
        data.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![Value::Text("a".repeat(text_len))]);
        session.map_mut().add_layer(rustgis_map::Layer::new(0, data));
        session.undo_memory.limit = 2 << 10;
        session
    }

    fn edit(session: &mut Session, text: &str) {
        session.run("edit.setValue", &json!({"layer": "rows", "row": 0, "field": "text", "value": text})).unwrap();
    }

    fn value(session: &Session) -> &Value {
        &session.map().layer_by_name("rows").unwrap().dataset.features[0].attrs[0]
    }

    #[test]
    fn shared_snapshots_are_counted_once_and_dropping_is_said_once() {
        let mut session = session(1024);
        let mark = session.notice_mark();
        for visible in [false, true] {
            session.run("layer.visibility", &json!({"layer": "rows", "visible": visible})).unwrap();
        }
        let b = "b".repeat(1024);
        let c = "c".repeat(1024);
        edit(&mut session, &b);
        assert_eq!(session.undo_depth(), 3);
        assert!(session.notices_since(mark).is_empty());

        // Both visibility steps and the first edit own the same initial rows. All three have
        // to go before the second edit fits, while the remaining step spans b to c.
        edit(&mut session, &c);
        let notices = session.notices_since(mark);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].text.contains("older steps are dropped"), "{notices:?}");
        assert_eq!(session.undo_depth(), 1);

        let mark = session.notice_mark();
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(b.clone()));
        session.run("edit.redo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(c.clone()));
        assert!(session.notices_since(mark).is_empty());

        // Past the budget each edit drops a step again, without saying it again.
        let mark = session.notice_mark();
        edit(&mut session, &b);
        assert_eq!(session.undo_depth(), 1);
        assert!(session.notices_since(mark).is_empty());

        // Another project's first drop is said again.
        session.clear_history();
        edit(&mut session, &c);
        let mark = session.notice_mark();
        edit(&mut session, &b);
        assert_eq!(session.notices_since(mark).len(), 1);
    }

    #[test]
    fn keeps_the_newest_undo_even_when_its_rows_exceed_the_budget() {
        let mut session = session(4096);
        edit(&mut session, "small");
        assert_eq!(session.undo_depth(), 1);
        assert!(session.notices.is_empty());
        edit(&mut session, "smaller");
        assert_eq!(session.undo_depth(), 1);
        assert_eq!(session.notices.len(), 1);
        assert!(session.notices[0].text.contains("older steps are dropped"));
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text("small".into()));
    }

    #[test]
    fn cuts_the_farthest_redo_when_undo_moves_large_live_rows_into_history() {
        let mut session = session(1);
        let b = "b".repeat(1200);
        let c = "c".repeat(1200);
        edit(&mut session, &b);
        edit(&mut session, &c);
        let mark = session.notice_mark();
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(b.clone()));
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text("a".into()));
        let notices = session.notices_since(mark);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].text.contains("older steps are dropped"));
        session.run("edit.redo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(b));
        assert!(!session.can_redo(), "only the nearer redo fits");
    }
}
