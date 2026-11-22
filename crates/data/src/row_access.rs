//! Reading and changing a layer's rows through [`Features`] without depending on how they are
//! stored (#2431). Today the rows are one shared `Vec<Feature>`, and any change copies all of them
//! for Undo. The plan is to keep attribute values in columns split into shared chunks, so that an
//! edit copies one chunk. Code that goes through these accessors keeps working when that happens;
//! code that reads a [`Feature`]'s fields or the row vector directly has to change with it, and
//! `cargo xtask rows` counts it outside this crate.
//!
//! Row positions stay dense: removing a row moves the rows after it up, as today.

use crate::{Feature, Features, Geometry, Value};
use std::sync::Arc;

/// One row of a layer, read where it is stored: its id, geometry and values. A view rather than
/// a `&Feature`, which a layer stored as columns won't have to lend.
#[derive(Clone, Copy, Debug)]
pub struct RowRef<'a> {
    row: &'a Feature,
}

impl<'a> RowRef<'a> {
    /// The row's id.
    pub fn id(self) -> u64 {
        self.row.id
    }

    /// The row's geometry, if it has one.
    pub fn geometry(self) -> Option<&'a Geometry> {
        self.row.geometry.as_ref()
    }

    /// The value of field `field`, or Null where the row has none.
    pub fn value(self, field: usize) -> &'a Value {
        self.row.attrs.get(field).unwrap_or(&Value::Null)
    }

    /// The row's values in field order.
    pub fn values(self) -> impl ExactSizeIterator<Item = &'a Value> + DoubleEndedIterator + Clone + 'a {
        self.row.attrs.iter()
    }

    /// A copy of the row, for code that keeps or hands on whole rows (a clipboard, a writer, an
    /// output layer).
    pub fn to_feature(self) -> Feature {
        self.row.clone()
    }
}

impl Features {
    /// How many rows there are.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row `row`, if there is one.
    pub fn row(&self, row: usize) -> Option<RowRef<'_>> {
        self.rows.get(row).map(|row| RowRef { row })
    }

    /// Every row, in order.
    pub fn rows(&self) -> impl ExactSizeIterator<Item = RowRef<'_>> + DoubleEndedIterator + Clone + '_ {
        self.rows.iter().map(|row| RowRef { row })
    }

    /// The value of field `field` in row `row`, or Null where there is none: a cell read without
    /// going through the row, which stays one lookup when values are kept by column.
    pub fn value(&self, row: usize, field: usize) -> &Value {
        self.row(row).map_or(&Value::Null, |r| r.value(field))
    }

    /// The geometry of row `row`, if the row exists and has one.
    pub fn geometry(&self, row: usize) -> Option<&Geometry> {
        self.row(row).and_then(RowRef::geometry)
    }

    /// Field `field`'s value in every row, in order (Null where a row has none): what a whole-column
    /// read (a statistic, a renderer's classes) will get without visiting other fields.
    pub fn column(&self, field: usize) -> impl ExactSizeIterator<Item = &Value> + DoubleEndedIterator + Clone + '_ {
        self.rows().map(move |r| r.value(field))
    }

    /// Sets field `field` of row `row` to `value`, returning the value it replaced; `None`, and
    /// nothing changes, when the row or the field doesn't exist.
    pub fn set_value(&mut self, row: usize, field: usize, value: Value) -> Option<Value> {
        self.rows.get(row)?.attrs.get(field)?;
        let cell = self.rows_mut().get_mut(row)?.attrs.get_mut(field)?;
        Some(std::mem::replace(cell, value))
    }

    /// Sets row `row`'s geometry, returning the one it replaced; `None`, and nothing changes, when
    /// there is no row `row`.
    pub fn set_geometry(&mut self, row: usize, geometry: Option<Geometry>) -> Option<Option<Geometry>> {
        self.rows.get(row)?;
        let old = &mut self.rows_mut().get_mut(row)?.geometry;
        Some(std::mem::replace(old, geometry))
    }

    /// Adds a column after the last, holding `value` in every row (Add Field): one new column
    /// when values are kept by column, not a change to every row.
    pub fn add_column(&mut self, value: Value) {
        for f in self.rows_mut() {
            // Exactly one more slot: a doubled capacity in every row would cost more than the column.
            f.attrs.reserve_exact(1);
            f.attrs.push(value.clone());
        }
    }

    /// Removes column `field` from every row that has it (Delete Field).
    pub fn remove_column(&mut self, field: usize) {
        if self.rows.iter().all(|f| f.attrs.len() <= field) {
            return;
        }
        for f in self.rows_mut() {
            if field < f.attrs.len() {
                f.attrs.remove(field);
            }
        }
    }

    /// Replaces column `field` with `values`, one per row in order, returning the old column
    /// (Calculate Field): the change a column-shaped edit records. Gives `values` back, and
    /// nothing changes, when there isn't one per row or a row has no field `field`.
    pub fn replace_column(&mut self, field: usize, values: Vec<Value>) -> Result<Vec<Value>, Vec<Value>> {
        if values.len() != self.rows.len() || self.rows.iter().any(|f| f.attrs.len() <= field) {
            return Err(values);
        }
        let rows = self.rows_mut();
        let old = rows.iter_mut().zip(values).filter_map(|(f, v)| f.attrs.get_mut(field).map(|c| std::mem::replace(c, v))).collect();
        Ok(old)
    }

    /// From now on, new rows ([`crate::Dataset::push`]) are given ids above `id`, even once the rows
    /// holding the highest ids are removed: for a layer whose ids up to `id` belong to rows given to
    /// it from elsewhere (a field package's), which a new row must never be taken for.
    pub fn keep_ids_above(&mut self, id: u64) {
        self.id_floor = self.id_floor.max(id);
    }

    /// The id new rows are kept above ([`Features::keep_ids_above`]), 0 for none.
    pub fn id_floor(&self) -> u64 {
        self.id_floor
    }

    /// The highest id a row has, or that new rows are kept above ([`Features::keep_ids_above`]):
    /// the next new row's id is one more.
    pub fn highest_id(&self) -> u64 {
        self.rows.iter().map(|f| f.id).max().unwrap_or(0).max(self.id_floor)
    }

    /// The id the first of `n` new rows takes, one above [`Features::highest_id`], when all `n`
    /// have one free above it below `i64::MAX`, the largest id a GeoPackage or PostGIS feature can
    /// have (so the last still leaves room for the next); `None` when they haven't, for a caller to
    /// refuse rather than give an id a table can't hold, or two rows one id. No rows always fit.
    pub fn free_ids(&self, n: usize) -> Option<u64> {
        if n == 0 {
            return Some(self.highest_id().saturating_add(1));
        }
        let first = self.highest_id().checked_add(1)?;
        let last = first.checked_add(u64::try_from(n).ok()?.saturating_sub(1))?;
        (last < i64::MAX.unsigned_abs()).then_some(first)
    }

    /// Appends `feature` as the last row, keeping its id.
    pub fn push_row(&mut self, feature: Feature) {
        let id = feature.id;
        self.rows_mut().push(feature);
        self.max_id = self.max_id.map(|m| m.max(id));
    }

    /// Appends a row of id `id`, `geometry` and the values `attrs`, in field order: a row made from
    /// its parts, such as one copied in from another layer's fields.
    pub fn push_parts(&mut self, id: u64, geometry: Option<Geometry>, attrs: Vec<Value>) {
        self.push_row(Feature { id, geometry, attrs });
    }

    /// The rows for a change, copied first when another owner (an Undo snapshot) shares them.
    /// Every change goes through it, so the hook [`crate::before_rows_copy`] sets runs before a
    /// copy, as it does through `DerefMut`.
    fn rows_mut(&mut self) -> &mut Vec<Feature> {
        self.before_copy();
        Arc::make_mut(&mut self.rows)
    }

    /// Removes row `row`, moving the rows after it up one, and returns it; `None`, and nothing
    /// changes, when there is no row `row`.
    pub fn remove_row(&mut self, row: usize) -> Option<Feature> {
        if row >= self.rows.len() {
            return None;
        }
        self.max_id = None;
        Some(self.rows_mut().remove(row))
    }

    /// Keeps the rows `keep` says yes to, in order, asking it once per row. When it keeps them
    /// all, nothing changes and the rows stay shared.
    pub fn retain_rows(&mut self, mut keep: impl FnMut(RowRef<'_>) -> bool) {
        let kept: Vec<bool> = self.rows.iter().map(|row| keep(RowRef { row })).collect();
        if kept.iter().all(|k| *k) {
            return;
        }
        self.max_id = None;
        // `Vec::retain` visits each row once, in order.
        let mut kept = kept.into_iter();
        self.rows_mut().retain(|_| kept.next().unwrap_or(true));
    }

    /// Keeps the first `len` rows.
    pub fn truncate_rows(&mut self, len: usize) {
        if len < self.rows.len() {
            self.max_id = None;
            self.rows_mut().truncate(len);
        }
    }

    /// A copy of every row as a [`Feature`]: for tests and for code that needs the rows as a list,
    /// at the cost of building it.
    pub fn to_rows(&self) -> Vec<Feature> {
        self.rows.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use crate::{Dataset, Feature, Features, Field, FieldType, Geometry, GeometryType, Value};

    fn layer() -> Dataset {
        let fields = vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Integer)];
        let mut ds = Dataset::new("t", GeometryType::Point, fields);
        ds.push(Some(Geometry::Point(rustgis_geom::Coord::new(1.0, 2.0))), vec![Value::Text("a".into()), Value::Int(1)]);
        ds.push(None, vec![Value::Text("b".into()), Value::Int(2)]);
        ds.push(Some(Geometry::Point(rustgis_geom::Coord::new(3.0, 4.0))), vec![Value::Text("c".into())]);
        ds
    }

    #[test]
    fn no_new_rows_always_fit_below_the_largest_id() {
        let mut ds = layer();
        ds.features.keep_ids_above(i64::MAX.unsigned_abs() - 1);
        assert_eq!(ds.features.free_ids(1), None);
        assert!(ds.features.free_ids(0).is_some());
        ds.features.keep_ids_above(u64::MAX);
        assert!(ds.features.free_ids(0).is_some());
    }

    #[test]
    fn reads_match_the_rows() {
        let ds = layer();
        let f = &ds.features;
        assert_eq!((f.len(), f.is_empty()), (3, false));
        for (r, row) in f.rows().enumerate() {
            assert_eq!(row.to_feature(), ds.features[r]);
            assert_eq!(row.id(), ds.features[r].id);
            assert_eq!(row.geometry(), f.geometry(r));
            assert_eq!(row.values().cloned().collect::<Vec<_>>(), ds.features[r].attrs);
        }
        assert_eq!(f.value(1, 1), &Value::Int(2));
        // Padded by `push`, so the short row reads Null; past the end is Null too.
        assert_eq!(f.value(2, 1), &Value::Null);
        assert_eq!((f.value(9, 0), f.value(0, 9)), (&Value::Null, &Value::Null));
        assert!(f.row(3).is_none() && f.geometry(1).is_none());
        assert_eq!(f.column(0).map(Value::to_string).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(f.rows().rev().map(|r| r.id()).collect::<Vec<_>>(), [3, 2, 1]);
        assert_eq!(f.to_rows(), ds.features.to_vec());
    }

    #[test]
    fn a_change_copies_the_rows_only_when_it_changes_something() {
        let ds = layer();
        let mut copy = ds.features.clone();
        assert_eq!(copy.set_value(0, 5, Value::Null), None);
        assert_eq!(copy.set_value(7, 0, Value::Null), None);
        assert_eq!(copy.set_geometry(7, None), None);
        assert_eq!(copy.replace_column(0, vec![Value::Null]), Err(vec![Value::Null]));
        copy.remove_column(5);
        copy.truncate_rows(3);
        assert_eq!(copy.remove_row(3), None);
        copy.retain_rows(|_| true);
        assert!(copy.shares(&ds.features));

        assert_eq!(copy.set_value(0, 1, Value::Int(10)), Some(Value::Int(1)));
        assert!(!copy.shares(&ds.features));
        assert_eq!((ds.features.value(0, 1), copy.value(0, 1)), (&Value::Int(1), &Value::Int(10)));
        assert_eq!(copy.set_geometry(1, ds.features.geometry(0).cloned()), Some(None));
        assert_eq!(copy.geometry(1), ds.features.geometry(0));
    }

    #[test]
    fn a_change_that_copies_shared_rows_calls_the_copy_hook_first() {
        let ds = layer();
        let mut copy = ds.features.clone();
        let fired = |copy: &mut Features, change: &dyn Fn(&mut Features)| crate::before_rows_copy(Box::new(|| {}), || change(copy)).1;
        assert!(!fired(&mut copy, &|f| {
            f.set_value(9, 0, Value::Null);
        }));
        assert!(fired(&mut copy, &|f| f.add_column(Value::Null)));
        // The rows are its own now: nothing more to copy.
        assert!(!fired(&mut copy, &|f| {
            f.set_value(0, 0, Value::Null);
        }));
        let mut other = ds.features.clone();
        assert!(fired(&mut other, &|f| f.truncate_rows(1)));
    }

    #[test]
    fn columns_change_in_every_row() {
        let mut ds = layer();
        assert_eq!(ds.ensure_field("flag", FieldType::Boolean), 2);
        assert_eq!(ds.features.column(2).cloned().collect::<Vec<_>>(), [Value::Null, Value::Null, Value::Null]);
        let old = ds.features.replace_column(1, vec![Value::Int(7), Value::Int(8), Value::Int(9)]);
        assert_eq!(old, Ok(vec![Value::Int(1), Value::Int(2), Value::Null]));
        ds.features.remove_column(0);
        assert_eq!(ds.features.row(2).map(|r| r.values().cloned().collect::<Vec<_>>()), Some(vec![Value::Int(9), Value::Null]));
    }

    #[test]
    fn removing_rows_keeps_them_dense_and_ids_fresh() {
        let mut ds = layer();
        assert_eq!(ds.features.remove_row(1).map(|f| f.id), Some(2));
        assert_eq!(ds.features.rows().map(|r| r.id()).collect::<Vec<_>>(), [1, 3]);
        // As through the vector: the largest id is searched for again, so it can be reused.
        ds.features.retain_rows(|r| r.id() != 3);
        ds.push(None, vec![]);
        assert_eq!(ds.features.rows().map(|r| r.id()).collect::<Vec<_>>(), [1, 2]);
        ds.features.truncate_rows(1);
        ds.features.push_row(Feature { id: 40, geometry: None, attrs: vec![Value::Null, Value::Null] });
        ds.push(None, vec![]);
        assert_eq!(ds.features.rows().map(|r| r.id()).collect::<Vec<_>>(), [1, 40, 41]);
    }
}

/// With the `count-row-uses` feature, which only `cargo xtask rows` turns on, the row vector's
/// methods are reached through these deprecated copies (and [`Feature`]'s fields are deprecated),
/// so the compiler names every place that uses them. Each behaves exactly as the vector's own,
/// reached through `Deref` or `DerefMut` as before.
#[cfg(feature = "count-row-uses")]
mod counted {
    use crate::{Feature, Features};
    use std::ops::{Deref, DerefMut};
    use std::slice::SliceIndex;

    impl Features {
        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn get<I: SliceIndex<[Feature]>>(&self, i: I) -> Option<&I::Output> {
            self.deref().get(i)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn get_mut<I: SliceIndex<[Feature]>>(&mut self, i: I) -> Option<&mut I::Output> {
            self.deref_mut().get_mut(i)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn iter(&self) -> std::slice::Iter<'_, Feature> {
            self.deref().iter()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Feature> {
            self.deref_mut().iter_mut()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn first(&self) -> Option<&Feature> {
            self.deref().first()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn first_mut(&mut self) -> Option<&mut Feature> {
            self.deref_mut().first_mut()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn last(&self) -> Option<&Feature> {
            self.deref().last()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn push(&mut self, f: Feature) {
            self.deref_mut().push(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn pop(&mut self) -> Option<Feature> {
            self.deref_mut().pop()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn remove(&mut self, i: usize) -> Feature {
            self.deref_mut().remove(i)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn retain<F: FnMut(&Feature) -> bool>(&mut self, f: F) {
            self.deref_mut().retain(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn truncate(&mut self, n: usize) {
            self.deref_mut().truncate(n)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn clear(&mut self) {
            self.deref_mut().clear()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn extend<I: IntoIterator<Item = Feature>>(&mut self, it: I) {
            self.deref_mut().extend(it)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn reverse(&mut self) {
            self.deref_mut().reverse()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn sort_by_key<K: Ord, F: FnMut(&Feature) -> K>(&mut self, f: F) {
            self.deref_mut().sort_by_key(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn to_vec(&self) -> Vec<Feature> {
            self.deref().to_vec()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn as_slice(&self) -> &[Feature] {
            self.deref().as_slice()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn chunks(&self, n: usize) -> std::slice::Chunks<'_, Feature> {
            self.deref().chunks(n)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn windows(&self, n: usize) -> std::slice::Windows<'_, Feature> {
            self.deref().windows(n)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn contains(&self, f: &Feature) -> bool {
            self.deref().contains(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn capacity(&self) -> usize {
            self.deref().capacity()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn reserve(&mut self, n: usize) {
            self.deref_mut().reserve(n)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn try_reserve(&mut self, n: usize) -> Result<(), std::collections::TryReserveError> {
            self.deref_mut().try_reserve(n)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn try_reserve_exact(&mut self, n: usize) -> Result<(), std::collections::TryReserveError> {
            self.deref_mut().try_reserve_exact(n)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn shrink_to_fit(&mut self) {
            self.deref_mut().shrink_to_fit()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn insert(&mut self, i: usize, f: Feature) {
            self.deref_mut().insert(i, f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn drain<R: std::ops::RangeBounds<usize>>(&mut self, r: R) -> std::vec::Drain<'_, Feature> {
            self.deref_mut().drain(r)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn append(&mut self, other: &mut Vec<Feature>) {
            self.deref_mut().append(other)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn split_off(&mut self, at: usize) -> Vec<Feature> {
            self.deref_mut().split_off(at)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn swap(&mut self, a: usize, b: usize) {
            self.deref_mut().swap(a, b)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn swap_remove(&mut self, i: usize) -> Feature {
            self.deref_mut().swap_remove(i)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn resize(&mut self, n: usize, f: Feature) {
            self.deref_mut().resize(n, f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn resize_with<F: FnMut() -> Feature>(&mut self, n: usize, f: F) {
            self.deref_mut().resize_with(n, f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn retain_mut<F: FnMut(&mut Feature) -> bool>(&mut self, f: F) {
            self.deref_mut().retain_mut(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn dedup_by_key<K: PartialEq, F: FnMut(&mut Feature) -> K>(&mut self, f: F) {
            self.deref_mut().dedup_by_key(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn sort_by<F: FnMut(&Feature, &Feature) -> std::cmp::Ordering>(&mut self, f: F) {
            self.deref_mut().sort_by(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn sort_unstable_by<F: FnMut(&Feature, &Feature) -> std::cmp::Ordering>(&mut self, f: F) {
            self.deref_mut().sort_unstable_by(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn sort_unstable_by_key<K: Ord, F: FnMut(&Feature) -> K>(&mut self, f: F) {
            self.deref_mut().sort_unstable_by_key(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn binary_search_by<F: FnMut(&Feature) -> std::cmp::Ordering>(&self, f: F) -> Result<usize, usize> {
            self.deref().binary_search_by(f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn binary_search_by_key<B: Ord, F: FnMut(&Feature) -> B>(&self, b: &B, f: F) -> Result<usize, usize> {
            self.deref().binary_search_by_key(b, f)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn last_mut(&mut self) -> Option<&mut Feature> {
            self.deref_mut().last_mut()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn split_at(&self, mid: usize) -> (&[Feature], &[Feature]) {
            self.deref().split_at(mid)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn split_at_mut(&mut self, mid: usize) -> (&mut [Feature], &mut [Feature]) {
            self.deref_mut().split_at_mut(mid)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn as_mut_slice(&mut self) -> &mut [Feature] {
            self.deref_mut().as_mut_slice()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn chunks_mut(&mut self, n: usize) -> std::slice::ChunksMut<'_, Feature> {
            self.deref_mut().chunks_mut(n)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn chunks_exact(&self, n: usize) -> std::slice::ChunksExact<'_, Feature> {
            self.deref().chunks_exact(n)
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn split_first(&self) -> Option<(&Feature, &[Feature])> {
            self.deref().split_first()
        }

        #[deprecated(note = "use the row accessors (`Features::row`, `rows`, `set_value`, ...); see cargo xtask rows")]
        pub fn split_last(&self) -> Option<(&Feature, &[Feature])> {
            self.deref().split_last()
        }
    }
}
