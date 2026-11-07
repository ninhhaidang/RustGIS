//! A view is decoded and projected on the source worker. The UI installs Arc-backed caches.
use rustgis_data::query::Expr;
use rustgis_data::source::{Batch, FeatureSource, Request, memory};
use rustgis_data::{DataError, Dataset, Field, Geometry, Result};
use rustgis_geom::Extent;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Cache {
    pub projected: Arc<Vec<Option<Geometry>>>,
    pub extents: Arc<Vec<Extent>>,
    pub vertices: usize,
    pub key: String,
    pub mean_bytes: usize,
    pub feature_bytes: usize,
    pub projected_bytes: usize,
    /// The read kept the definition query's matches (its `matched` counts them).
    pub filtered: bool,
}

fn base_bytes(batch: &Batch, projected: &Vec<Option<Geometry>>) -> usize {
    memory::features_bytes(&batch.features)
        .saturating_add(crate::lod::cache_bytes(projected))
        .saturating_add(projected.len().saturating_mul(std::mem::size_of::<Extent>()))
}

fn retain_evenly<T>(items: &mut Vec<T>, kept: usize) {
    let total = items.len();
    let mut carry = total.saturating_sub(kept);
    items.retain(|_| {
        carry += kept;
        if carry >= total {
            carry -= total;
            true
        } else {
            false
        }
    });
    items.shrink_to_fit();
}

/// A layer's definition query, with the fields it names columns from (the layer's).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Query {
    pub text: String,
    pub fields: Vec<Field>,
}

/// The bytes of features held at once while a filtered read walks the source.
const FILTER_CHUNK_BYTES: usize = 8 << 20;

/// Reads a view for the worker: within `bytes` (an interactive view) or whole up to `req.limit`
/// (`full`, a render or export), with the layer's definition query applied before the limit.
pub(super) fn read_mode(
    source: &dyn FeatureSource,
    req: &Request,
    bytes: usize,
    projection: &crate::Projection,
    estimate: usize,
    full: bool,
    fields: Option<usize>,
    query: Option<&Query>,
) -> Result<(Batch, Cache)> {
    let read = |source: &dyn FeatureSource| -> Result<(Batch, Cache)> {
        if full { Ok(whole(source.read(req)?, projection)) } else { read_with_fields(source, req, bytes, projection, estimate, fields) }
    };
    // A query that doesn't parse draws every feature, so the view is read as without one.
    let Some((scratch, expr)) = query.and_then(|q| walked(source, q)) else { return read(source) };
    // A view read whole is filtered exactly by the layer. A sampled one would leave matches out
    // (#1456): the source is walked instead, keeping up to the view's limit of the matches.
    // Half the budget holds them; their projection and display levels take the rest.
    let first = read(source)?;
    if first.0.complete {
        return Ok(first);
    }
    drop(first);
    let batch = read_filtered(source, req, if full { usize::MAX } else { bytes / 2 }, scratch, &expr)?;
    let (batch, mut cache) = if full { whole(batch, projection) } else { fit(batch, bytes, projection, fields)? };
    cache.filtered = true;
    Ok((batch, cache))
}

/// What a sampled view of `source` is walked with, keeping the matches: the parsed `query` and a
/// dataset of its fields to hold each chunk. `None` when the source can't be read a chunk at a
/// time, the query doesn't parse, or it uses a row number (a row's place in the view, which a walk
/// of the source can't number the same way).
pub(super) fn walked(source: &dyn FeatureSource, query: &Query) -> Option<(Dataset, Expr)> {
    if !source.visits_chunks() {
        return None;
    }
    let info = source.info();
    let scratch = Dataset::new(info.name.clone(), info.geometry_type, query.fields.clone());
    let expr = rustgis_data::query::parse(&query.text, &scratch).ok()?;
    (!numbers_rows(&expr)).then_some((scratch, expr))
}

/// Whether an expression reads `@row_number` anywhere.
fn numbers_rows(e: &Expr) -> bool {
    use rustgis_data::query::Var;
    let any = |es: &[Expr]| es.iter().any(numbers_rows);
    match e {
        Expr::Var(v) => *v == Var::RowNumber,
        Expr::And(es) | Expr::Or(es) | Expr::Concat(es) | Expr::Call(_, es) => any(es),
        Expr::Not(e) | Expr::IsNullExpr { e, .. } => numbers_rows(e),
        Expr::Compare(_, a, b) | Expr::LikeExpr { e: a, pattern: b, .. } => numbers_rows(a) || numbers_rows(b),
        Expr::InExpr { e, list, .. } => numbers_rows(e) || any(list),
        Expr::Between { e, lo, hi, .. } => numbers_rows(e) || numbers_rows(lo) || numbers_rows(hi),
        Expr::Chain(first, rest) => numbers_rows(first) || rest.iter().any(|(_, e)| numbers_rows(e)),
        Expr::Case { subject, arms, otherwise } => {
            subject.as_deref().is_some_and(numbers_rows)
                || arms.iter().any(|(w, t)| numbers_rows(w) || numbers_rows(t))
                || otherwise.as_deref().is_some_and(numbers_rows)
        }
        Expr::Cmp { .. } | Expr::Like { .. } | Expr::IsNull { .. } | Expr::In { .. } | Expr::Lit(_) | Expr::Field(_) => false,
    }
}

/// The features of `req` that `expr` keeps, walking every feature of its extent: an evenly
/// spread sample of the matches when more than `req.limit` of them, or more than `bytes`, match.
/// `scratch` holds each chunk, with the fields `expr` was parsed against.
fn read_filtered(source: &dyn FeatureSource, req: &Request, bytes: usize, mut scratch: Dataset, expr: &Expr) -> Result<Batch> {
    let limit = req.limit.max(1);
    let (mut kept, mut kept_bytes, mut step, mut seen) = (Vec::new(), 0usize, 1u64, 0u64);
    let summary = source.visit_chunks(req.extent, FILTER_CHUNK_BYTES, &mut |chunk| {
        scratch.features = chunk.into();
        let keep: Vec<bool> = (0..scratch.features.len()).map(|r| rustgis_data::query::eval(expr, &scratch, r)).collect();
        for (feature, _) in std::mem::take(&mut scratch.features).into_iter().zip(keep).filter(|(_, k)| *k) {
            seen += 1;
            if !(seen - 1).is_multiple_of(step) {
                continue;
            }
            kept_bytes = kept_bytes.saturating_add(memory::feature_bytes(&feature));
            kept.push(feature);
            // Every other match kept, at twice the step, as a source's own sample does.
            while kept.len() > 1 && (kept.len() > limit || kept_bytes > bytes) {
                let mut i = 0;
                kept.retain(|_| {
                    i += 1;
                    i % 2 == 1
                });
                kept_bytes = memory::features_bytes(&kept);
                step = step.saturating_mul(2);
            }
        }
        Ok(())
    })?;
    Ok(Batch { features: kept, matched: seen, complete: step == 1, skipped: summary.skipped })
}

/// A whole read's cache: every feature projected, without a byte budget.
fn whole(batch: Batch, projection: &crate::Projection) -> (Batch, Cache) {
    let flat = crate::projection::Flat::of(projection);
    let projected: Vec<_> = batch.features.iter().map(|f| f.geometry.as_ref().map(|g| flat.project(g))).collect();
    let cache = Cache {
        extents: Arc::new(projected.iter().map(|g| g.as_ref().map_or(Extent::EMPTY, Geometry::extent)).collect()),
        vertices: projected.iter().flatten().map(crate::lod::thinnable).sum(),
        mean_bytes: 0,
        feature_bytes: memory::features_bytes(&batch.features),
        projected_bytes: crate::lod::cache_bytes(&projected),
        projected: Arc::new(projected),
        key: projection.code(),
        filtered: false,
    };
    (batch, cache)
}

#[cfg(test)]
fn read(
    source: &dyn FeatureSource,
    req: &Request,
    bytes: usize,
    projection: &crate::Projection,
    estimate: usize,
) -> Result<(Batch, Cache)> {
    read_with_fields(source, req, bytes, projection, estimate, None)
}

/// Reads a view within `bytes`: a small probe sizes the rows (unless `estimate` already has),
/// then as many as fit, padded to the layer's `fields`.
fn read_with_fields(
    source: &dyn FeatureSource,
    req: &Request,
    bytes: usize,
    projection: &crate::Projection,
    estimate: usize,
    fields: Option<usize>,
) -> Result<(Batch, Cache)> {
    let flat = crate::projection::Flat::of(projection);
    let base_budget = bytes.saturating_mul(3) / 4;
    let count_for = |cost: usize| req.limit.min(base_budget.saturating_mul(9) / 10 / cost.max(1)).max(usize::from(req.limit > 0));
    let mut request = Request { limit: if estimate == 0 { req.limit.min(64) } else { count_for(estimate) }, ..*req };
    let mut batch = if estimate == 0 || req.limit == 0 { source.read_budgeted(&request, bytes)? } else { source.read(&request)? };
    // Trusted row sizes avoid a second probe, but never exempt a newly oversized row.
    refuse_oversized(&batch, bytes)?;
    pad(&mut batch, bytes, fields)?;
    let project = |batch: &Batch| batch.features.iter().map(|f| f.geometry.as_ref().map(|g| flat.project(g))).collect::<Vec<_>>();
    let mut projected = project(&batch);
    // The first small uniform sample measures real geometry and trimmed attributes rather
    // than padded DBF widths. Later windows reuse the measured bytes per row, avoiding a
    // second probe. This fills the available budget instead of leaving most of it unused.
    if estimate == 0 && !batch.complete {
        // An initial sample may hit only deleted/unreadable rows. It is not proof that
        // the matching extent is empty; let the bounded provider try a wider sample.
        let mean = if batch.features.is_empty() {
            std::mem::size_of::<rustgis_data::Feature>().saturating_mul(4)
        } else {
            base_bytes(&batch, &projected).div_ceil(batch.features.len())
        };
        let target = count_for(mean);
        if target > request.limit {
            drop(projected);
            drop(batch);
            request.limit = target;
            batch = source.read_budgeted(&request, bytes)?;
            pad(&mut batch, bytes, fields)?;
            projected = project(&batch);
        }
    }
    trim(batch, projected, bytes, projection)
}

/// A view read some other way (a filtered walk) fitted to the view's `bytes` as a read is.
fn fit(mut batch: Batch, bytes: usize, projection: &crate::Projection, fields: Option<usize>) -> Result<(Batch, Cache)> {
    refuse_oversized(&batch, bytes)?;
    pad(&mut batch, bytes, fields)?;
    let flat = crate::projection::Flat::of(projection);
    let projected = batch.features.iter().map(|f| f.geometry.as_ref().map(|g| flat.project(g))).collect();
    trim(batch, projected, bytes, projection)
}

/// Refuses a read holding a feature larger than the whole view budget.
fn refuse_oversized(batch: &Batch, bytes: usize) -> Result<()> {
    if batch.features.iter().any(|f| std::mem::size_of::<rustgis_data::Feature>().saturating_add(memory::feature_bytes(f)) > bytes) {
        return Err(DataError::Format("one feature exceeds the view memory budget; load it explicitly for full processing".into()));
    }
    Ok(())
}

/// Gives every feature the layer's `fields` attributes, first keeping as many rows as their
/// padded attributes fit in `bytes`.
fn pad(batch: &mut Batch, bytes: usize, fields: Option<usize>) -> Result<()> {
    if let Some(fields) = fields {
        let row =
            fields.saturating_mul(std::mem::size_of::<rustgis_data::Value>()).saturating_add(std::mem::size_of::<rustgis_data::Feature>());
        let kept = bytes / row;
        if !batch.features.is_empty() && kept == 0 {
            return Err(DataError::Format(
                "one attribute row exceeds the view memory budget; load it explicitly for full processing".into(),
            ));
        }
        if batch.features.len() > kept {
            retain_evenly(&mut batch.features, kept);
            batch.complete = false;
        }
        for feature in &mut batch.features {
            feature.attrs.resize(fields, rustgis_data::Value::Null);
        }
    }
    Ok(())
}

/// Thins the rows and their projection evenly until they fit three quarters of `bytes`, and
/// builds the window's cache.
fn trim(mut batch: Batch, mut projected: Vec<Option<Geometry>>, bytes: usize, projection: &crate::Projection) -> Result<(Batch, Cache)> {
    let base_budget = bytes.saturating_mul(3) / 4;
    // Keep room for display levels. Actual allocation accounting also handles projections
    // that split at a world seam and create more parts than the original geometry.
    loop {
        let used = base_bytes(&batch, &projected);
        // The display-cache reservation is optional: one indivisible feature may
        // use the entire assigned budget, leaving no room for display copies.
        if used <= base_budget || (batch.features.len() <= 1 && used <= bytes) {
            break;
        }
        if batch.features.len() <= 1 {
            return Err(DataError::Format(
                "one projected feature exceeds the view memory budget; load it explicitly for full processing".into(),
            ));
        }
        let kept = batch.features.len().saturating_mul(base_budget) / used.max(1);
        let kept = kept.max(1).min(batch.features.len() - 1);
        retain_evenly(&mut batch.features, kept);
        retain_evenly(&mut projected, kept);
        batch.complete = false;
    }
    let extents = Arc::new(projected.iter().map(|g| g.as_ref().map_or(Extent::EMPTY, Geometry::extent)).collect());
    let vertices = projected.iter().flatten().map(crate::lod::thinnable).sum();
    let mean_bytes = base_bytes(&batch, &projected).div_ceil(batch.features.len().max(1));
    let feature_bytes = memory::features_bytes(&batch.features);
    let projected_bytes = crate::lod::cache_bytes(&projected);
    Ok((
        batch,
        Cache {
            projected: Arc::new(projected),
            extents,
            vertices,
            key: projection.code(),
            mean_bytes,
            feature_bytes,
            projected_bytes,
            filtered: false,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
    use rustgis_geom::Coord;

    fn source() -> Box<dyn FeatureSource> {
        let mut ds = Dataset::new("complex", GeometryType::Polyline, vec![Field::new("text", FieldType::Text)]);
        for i in 0..600 {
            ds.push(
                Some(Geometry::Line(vec![(0..80).map(|j| Coord::new(i as f64 * 0.01, j as f64 * 0.001)).collect()])),
                vec![Value::Text("x".repeat(300))],
            );
        }
        let (bytes, _) = rustgis_data::io::write_bytes(&ds, "fgb").unwrap();
        rustgis_data::source::open_bytes("complex.fgb", bytes, None).unwrap()
    }

    #[test]
    fn byte_budget_prepares_aligned_projection_and_preserves_full_processing() {
        let source = source();
        let budget = 128 * 1024;
        let projection = crate::Projection::Custom("robinson +lon_0=150".into());
        let (batch, cache) = read(source.as_ref(), &Request::all(), budget, &projection, 0).unwrap();
        assert!(!batch.complete);
        assert!(batch.features.len() < 600 && !batch.features.is_empty());
        assert_eq!(batch.features.len(), cache.projected.len());
        let bytes = memory::features_bytes(&batch.features)
            + crate::lod::cache_bytes(&cache.projected)
            + cache.extents.capacity() * std::mem::size_of::<Extent>();
        assert!(bytes <= budget * 3 / 4, "{bytes}");
        assert!(bytes >= budget / 2, "available memory was wasted by an overly thin sample: {bytes}");
        let flat = crate::projection::Flat::of(&projection);
        for (row, projected) in batch.features.iter().zip(cache.projected.iter()) {
            assert_eq!(*projected, row.geometry.as_ref().map(|g| flat.project(g)));
        }
        assert_eq!(source.read(&Request::all()).unwrap().features.len(), 600);
    }

    #[test]
    fn installed_windows_share_the_map_budget_and_reuse_prepared_geometry() {
        let mut map = crate::Map::new("budget");
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut s = super::super::SourceLayer::from_path("memory.fgb", None, source());
            s.limit = 100;
            ids.push(map.add_streamed("complex", s));
        }
        let extent = Extent::new(-10.0, -10.0, 10.0, 10.0);
        settle(&mut map, &extent);
        let flat = map.flat();
        let key = map.projection.code();
        for id in ids {
            let layer = map.layer_mut(id).unwrap();
            assert_eq!(layer.source.as_ref().unwrap().budget(), super::super::VIEW_BYTES / 2);
            assert!(layer.is_cached(&key), "installing a window left projection on the UI thread");
            let cache = layer.projected.clone();
            for tol in [100.0, 10.0, 1.0, 0.1] {
                layer.thinned_in(&flat, &key, tol);
            }
            assert!(Arc::ptr_eq(&cache, &layer.projected), "first zoom reprojected the window");
            assert!(layer.view_cache_bytes() <= layer.source.as_ref().unwrap().budget());
            let mut older = layer.clone();
            older.source.as_mut().unwrap().byte_budget *= 2;
            assert!(!layer.adopt_cache(&older), "a worker using an older, larger budget must not restore its cache");
        }
        map.set_projection(crate::Projection::Custom("robinson".into()));
        settle(&mut map, &extent);
        assert!(map.layers.iter().all(|l| l.is_cached(&map.projection.code())));
    }
    fn settle(map: &mut crate::Map, extent: &Extent) {
        map.sync_sources(extent, 1000.0, false);
        for layer in map.layers.iter_mut().filter(|l| l.visible) {
            assert!(!layer.sync_source(extent, true));
        }
    }

    #[test]
    fn zero_limits_and_oversized_features_preserve_read_contracts() {
        let source = source();
        let projection = crate::Projection::WebMercator;
        for estimate in [0, 100] {
            let request = Request { extent: None, limit: 0 };
            let (batch, cache) = read(source.as_ref(), &request, 4096, &projection, estimate).unwrap();
            assert!(batch.features.is_empty());
            assert!(cache.projected.is_empty());
            assert_eq!(batch.matched, 600);
        }
        assert!(read(source.as_ref(), &Request::all(), 1, &projection, 0).is_err());
        let (full, _) = read_mode(source.as_ref(), &Request::all(), 1, &projection, 0, true, None, None).unwrap();
        assert!(full.complete);
        assert_eq!(full.features.len(), 600);
    }

    #[test]
    fn padded_project_fields_are_included_in_the_budget() {
        let source = source();
        let bytes = 64 * 1024;
        let (batch, cache) =
            read_with_fields(source.as_ref(), &Request::all(), bytes, &crate::Projection::WebMercator, 0, Some(200)).unwrap();
        assert!(batch.features.iter().all(|f| f.attrs.len() == 200));
        assert!(base_bytes(&batch, &cache.projected) <= bytes * 3 / 4);
        assert!(!batch.complete);
        assert!(read_with_fields(source.as_ref(), &Request::all(), bytes, &crate::Projection::WebMercator, 0, Some(usize::MAX)).is_err());
    }

    #[test]
    fn export_mode_replaces_an_interaction_sample_and_preserves_selection() {
        let mut src = super::super::SourceLayer::from_path("memory.fgb", None, source());
        src.byte_budget = 64 * 1024;
        src.projection = Some(crate::Projection::WebMercator);
        let mut layer = crate::Layer::new_streamed(1, src);
        let extent = Extent::new(-10.0, -10.0, 10.0, 10.0);
        assert!(!layer.sync_source(&extent, true));
        assert!(layer.dataset.features.len() < 600);
        let id = layer.dataset.features.first().unwrap().id;
        layer.selection.push(0);
        assert_eq!(layer.load_all().unwrap().features.len(), 600);
        let mut map = crate::Map::new("export");
        map.layers.push(layer);
        assert!(!map.sync_sources(&extent, 1000.0, true));
        let layer = &map.layers[0];
        assert_eq!(layer.dataset.features.len(), 600);
        assert_eq!(layer.dataset.features[layer.selection[0]].id, id);
        assert!(layer.source.as_ref().unwrap().window().unwrap().complete);
        let shared = map.clone();
        assert!(Arc::ptr_eq(&layer.projected, &shared.layers[0].projected));
    }

    #[test]
    fn hidden_resident_windows_share_budget_and_release_excess_capacity() {
        let mut map = crate::Map::new("resident");
        let first = map.add_streamed("first", super::super::SourceLayer::from_path("memory.fgb", None, source()));
        let extent = Extent::new(-10.0, -10.0, 10.0, 10.0);
        settle(&mut map, &extent);
        let layer = map.layer_mut(first).unwrap();
        let selected = layer.dataset.features.first().unwrap().id;
        layer.selection.push(0);
        layer.visible = false;
        layer.projected = Arc::new(Vec::with_capacity(super::super::VIEW_BYTES / std::mem::size_of::<Option<Geometry>>() * 3 / 4));
        map.add_streamed("second", super::super::SourceLayer::from_path("memory.fgb", None, source()));
        settle(&mut map, &extent);
        let layer = map.layer_mut(first).unwrap();
        assert!(layer.dataset.features.is_empty());
        assert!(layer.view_cache_bytes() <= super::super::VIEW_BYTES / 2);
        assert!(!layer.source.as_ref().unwrap().is_pending());
        // A hidden window can be reset repeatedly before it is visible again. Its IDs
        // belong to this same source and must survive those display-only changes.
        map.set_projection(crate::Projection::Custom("robinson".into()));
        settle(&mut map, &extent);
        map.set_projection(crate::Projection::WebMercator);
        settle(&mut map, &extent);
        let layer = map.layer_mut(first).unwrap();
        assert_eq!(layer.source.as_ref().unwrap().saved_selection, vec![selected]);
        layer.visible = true;
        settle(&mut map, &extent);
        let layer = map.layer(first).unwrap();
        assert_eq!(layer.dataset.features[layer.selection[0]].id, selected);
        assert!(map.layers.iter().map(crate::Layer::view_cache_bytes).sum::<usize>() <= super::super::VIEW_BYTES);
        // Capacity can also grow without a budget/projection change. Eviction must
        // invalidate the old complete-window marker, or no replacement read starts.
        map.layer_mut(first).unwrap().projected =
            Arc::new(Vec::with_capacity(super::super::VIEW_BYTES / std::mem::size_of::<Option<Geometry>>() * 3 / 4));
        settle(&mut map, &extent);
        assert!(!map.layer(first).unwrap().dataset.features.is_empty());
    }
    #[test]
    fn failed_reads_cannot_publish_an_old_projection_cache() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Failing {
            inner: Box<dyn FeatureSource>,
            fail: Arc<AtomicBool>,
        }
        impl FeatureSource for Failing {
            fn info(&self) -> &rustgis_data::source::SourceInfo {
                self.inner.info()
            }
            fn read(&self, req: &Request) -> Result<Batch> {
                if self.fail.load(Ordering::Relaxed) {
                    return Err(DataError::Format("injected failure".into()));
                }
                self.inner.read(req)
            }
        }
        let fail = Arc::new(AtomicBool::new(false));
        let mut src = super::super::SourceLayer::from_path("memory.fgb", None, Box::new(Failing { inner: source(), fail: fail.clone() }));
        let extent = Extent::new(-10.0, -10.0, 10.0, 10.0);
        src.start(extent, true);
        assert!(matches!(src.poll(true), super::super::Polled::Done(..)));
        assert!(src.ready_cache.is_some());
        src.saved_selection = vec![42];
        fail.store(true, Ordering::Relaxed);
        src.start(extent, true);
        assert!(matches!(src.poll(true), super::super::Polled::Failed(..)));
        assert!(src.ready_cache.is_none());
        assert!(src.saved_selection.is_empty());
        fail.store(false, Ordering::Relaxed);
        let mut layer = crate::Layer::new_streamed(1, src);
        layer.source.as_mut().unwrap().reset();
        assert!(!layer.sync_source(&extent, true));
        assert!(!layer.dataset.features.is_empty());
        assert!(layer.selection.is_empty(), "an unrelated successful read restored failed selection IDs");
        layer.source.as_mut().unwrap().saved_selection = vec![42];
        layer.source.as_mut().unwrap().reset();
        assert!(layer.source.as_ref().unwrap().saved_selection.is_empty(), "explicit reset retained parked selection");
    }
    #[test]
    fn an_empty_incomplete_probe_does_not_hide_the_source() {
        struct Sparse(Box<dyn FeatureSource>);
        impl FeatureSource for Sparse {
            fn info(&self) -> &rustgis_data::source::SourceInfo {
                self.0.info()
            }
            fn read(&self, req: &Request) -> Result<Batch> {
                if req.limit <= 128 {
                    return Ok(Batch { features: Vec::with_capacity(128), matched: 600, complete: false, ..Batch::default() });
                }
                self.0.read(req)
            }
        }
        let source = Sparse(source());
        // Exercise the default adapter directly: the map's wider retry must not hide a
        // provider probe whose empty Vec still has allocated capacity.
        let probe = source.read_budgeted(&Request::all(), 128 * 1024).unwrap();
        assert!(!probe.features.is_empty());
        let (batch, cache) = read(&source, &Request::all(), 128 * 1024, &crate::Projection::WebMercator, 0).unwrap();
        assert!(!batch.features.is_empty());
        assert_eq!(batch.features.len(), cache.projected.len());
        assert!(base_bytes(&batch, &cache.projected) <= 96 * 1024);
        assert_eq!(source.read(&Request::all()).unwrap().features.len(), 600);
    }
    #[test]
    fn one_feature_can_use_the_optional_display_reservation() {
        let source = source();
        let req = Request { extent: None, limit: 1 };
        let projection = crate::Projection::WebMercator;
        let (mut full, cache) = read_mode(source.as_ref(), &req, 1, &projection, 0, true, None, None).unwrap();
        full.features.shrink_to_fit();
        let budget = base_bytes(&full, &cache.projected);
        let (batch, cache) = read(source.as_ref(), &req, budget, &projection, 0).unwrap();
        assert_eq!(batch.features, full.features);
        assert!(base_bytes(&batch, &cache.projected) <= budget);
    }
    #[test]
    fn measured_rows_read_once_and_still_reject_newly_oversized_features() {
        use std::sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        };
        struct Counted {
            inner: Box<dyn FeatureSource>,
            limits: Mutex<Vec<usize>>,
            large: AtomicBool,
        }
        impl FeatureSource for Counted {
            fn info(&self) -> &rustgis_data::source::SourceInfo {
                self.inner.info()
            }
            fn read(&self, req: &Request) -> Result<Batch> {
                self.limits.lock().unwrap().push(req.limit);
                let mut batch = self.inner.read(req)?;
                if self.large.load(Ordering::Relaxed)
                    && let Some(f) = batch.features.first_mut()
                {
                    f.attrs.push(Value::Text("x".repeat(256 * 1024)));
                }
                Ok(batch)
            }
        }
        let mut ds = Dataset::new("points", GeometryType::Point, vec![]);
        for i in 0..1000 {
            ds.push(Some(Geometry::Point(Coord::new(i as f64 * 0.01, 0.0))), vec![]);
        }
        let (bytes, _) = rustgis_data::io::write_bytes(&ds, "fgb").unwrap();
        let source = Counted {
            inner: rustgis_data::source::open_bytes("points.fgb", bytes, None).unwrap(),
            limits: Mutex::new(vec![]),
            large: AtomicBool::new(false),
        };
        let budget = 128 * 1024;
        let projection = crate::Projection::WebMercator;
        let (batch, cache) = read(&source, &Request::all(), budget, &projection, 0).unwrap();
        assert!(!batch.features.is_empty());
        source.limits.lock().unwrap().clear();
        let (batch, _) = read(&source, &Request::all(), budget, &projection, cache.mean_bytes).unwrap();
        let limits = source.limits.lock().unwrap();
        assert_eq!(limits.len(), 1, "a trusted estimate still triggered provider probes");
        assert!(limits[0] > 128, "fixture must cover the old adapter's redundant-probe path");
        drop(limits);
        assert!(!batch.features.is_empty());
        source.large.store(true, Ordering::Relaxed);
        source.limits.lock().unwrap().clear();
        let result = read(&source, &Request::all(), budget, &projection, cache.mean_bytes);
        assert!(matches!(result, Err(DataError::Format(s)) if s.contains("one feature exceeds")));
        assert_eq!(source.limits.lock().unwrap().len(), 1);
    }
}
