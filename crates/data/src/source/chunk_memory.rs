//! Allocation accounting for the chunks of a complete traversal (drawing, exports).
use crate::{DataError, Extent, Feature, Geometry, Value};
use std::mem::size_of;

/// Allocated vector storage owned by a geometry, excluding the enum in its containing array.
pub fn geometry_bytes(g: &Geometry) -> usize {
    let ring = |r: &Vec<crate::Coord>| r.capacity().saturating_mul(size_of::<crate::Coord>());
    let parts = |p: &Vec<Vec<crate::Coord>>| {
        p.iter().fold(p.capacity().saturating_mul(size_of::<Vec<crate::Coord>>()), |n, r| n.saturating_add(ring(r)))
    };
    match g {
        Geometry::Point(_) => 0,
        Geometry::MultiPoint(p) => ring(p),
        Geometry::Line(p) => parts(p),
        Geometry::Polygon(p) => {
            p.iter().fold(p.capacity().saturating_mul(size_of::<Vec<Vec<crate::Coord>>>()), |n, r| n.saturating_add(parts(r)))
        }
    }
}

/// Heap storage owned by a row, including unused capacity and attribute strings.
pub fn feature_bytes(f: &Feature) -> usize {
    let attrs = f.attrs.iter().fold(f.attrs.capacity().saturating_mul(size_of::<Value>()), |n, v| {
        n.saturating_add(match v {
            Value::Text(s) => s.capacity(),
            _ => 0,
        })
    });
    attrs.saturating_add(f.geometry.as_ref().map_or(0, geometry_bytes))
}

/// Actual allocated chunk storage, including spare capacity.
pub fn features_bytes(features: &Vec<Feature>) -> usize {
    features.iter().fold(features.capacity().saturating_mul(size_of::<Feature>()), |n, f| n.saturating_add(feature_bytes(f)))
}

/// Gathers features into chunks of at most `bytes` of owned storage for
/// [`FeatureSource::visit_chunks`](super::FeatureSource::visit_chunks), handing each to the
/// visitor as it fills. A feature larger than the target on its own is handed over alone.
pub(crate) struct Chunker<'a> {
    bytes: usize,
    used: usize,
    features: Vec<Feature>,
    visit: &'a mut dyn FnMut(Vec<Feature>) -> crate::Result<()>,
}

impl<'a> Chunker<'a> {
    /// Refuses a zero target, which no feature fits.
    pub(crate) fn new(bytes: usize, visit: &'a mut dyn FnMut(Vec<Feature>) -> crate::Result<()>) -> crate::Result<Self> {
        if bytes == 0 {
            return Err(crate::DataError::Format("the chunk memory budget is zero".into()));
        }
        Ok(Chunker { bytes, used: 0, features: Vec::new(), visit })
    }

    /// Adds a feature, first handing over the chunk it would overflow.
    pub(crate) fn push(&mut self, feature: Feature) -> crate::Result<()> {
        let size = feature_bytes(&feature).saturating_add(size_of::<Feature>());
        if !self.features.is_empty() && self.used.saturating_add(size) > self.bytes {
            self.flush()?;
        }
        // A geometry is indivisible: a row bigger than the target is kept whole rather than
        // dropped or failing the whole traversal.
        if size > self.bytes {
            return (self.visit)(vec![feature]);
        }
        self.used = self.used.saturating_add(size);
        self.features.push(feature);
        Ok(())
    }

    /// Hands over the features gathered so far, if any.
    pub(crate) fn flush(&mut self) -> crate::Result<()> {
        if self.features.is_empty() {
            return Ok(());
        }
        let mut chunk = std::mem::take(&mut self.features);
        chunk.shrink_to_fit();
        self.used = 0;
        (self.visit)(chunk)
    }
}

/// A streamed row kept for a copy clipped to `extent`: `None` when it has no shape or its shape's
/// extent misses `extent`, else the row with exactly `width` values (cut, or padded with nulls)
/// and the heap bytes it holds. Fails, rather than aborting, when the values can't be allocated.
pub fn clip_row(mut row: Feature, extent: &Extent, width: usize) -> Result<Option<(Feature, usize)>, DataError> {
    if row.geometry.as_ref().is_none_or(|g| !g.extent().intersects(extent)) {
        return Ok(None);
    }
    row.attrs.truncate(width);
    row.attrs
        .try_reserve(width.saturating_sub(row.attrs.len()))
        .map_err(|_| DataError::Format("not enough memory for the copied attributes".into()))?;
    row.attrs.resize(width, Value::Null);
    let bytes = feature_bytes(&row);
    Ok(Some((row, bytes)))
}
