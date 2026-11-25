//! Class-by-zone tables on the same cell-centre masks as Zonal Statistics.
use super::*;
use rustgis_data::loss::{LossCode, LossMessage};
use rustgis_data::{Measure, raster::Raster};

/// Counts or measures every class in each zone. One zone's mask is built as one step without
/// cancellation inside the scanline fill; the band scan and masked cells check between rows
/// and every few thousand cells. Independent zones use the shared, memory-bounded workers.
pub(super) fn run(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    run_on(inp, p, None)
}

/// The explicit thread count is for checking that each zone sums in the same order on any core.
pub(super) fn run_on(inp: &dyn Inputs, p: &Json, threads: Option<usize>) -> Result<ToolResult, String> {
    let r = inp.raster(s(p, "raster")).ok_or("choose an input raster")?.raster;
    r.require_coordinates()?;
    let band = zonal_bands(p.get("band").unwrap_or(&Json::Null), r.band_count())?;
    let [band] = band.as_slice() else { return Err("choose exactly one categorical band".into()) };
    let zones = input(inp, p, "zones")?;
    if zones.geometry_type != GeometryType::Polygon {
        return Err("Tabulate Area needs polygon zones".into());
    }
    let keys = key_fields(zones, p.get("zoneField"), "Zone Field")?;
    if keys.len() > 1 {
        return Err("choose one Zone Field, or leave it empty for each feature".into());
    }
    let count = match s(p, "measure") {
        "" | "Area" => false,
        "Count" => true,
        _ => return Err("Measure must be Area or Count".into()),
    };
    let unit = if s(p, "unit").is_empty() { "km2" } else { s(p, "unit") };
    // A count has no unit, so its unit isn't checked.
    let divisor = if count { 1.0 } else { Measure::Area.unit(unit).ok_or("choose an area unit: km2, m2, ha, ac, ft2 or mi2")? };
    let mut selected = rows(inp, p, "zones", zones);
    record_order(&mut selected);
    let groups = if keys.is_empty() {
        selected
            .iter()
            .filter_map(|&row| {
                let f = zones.features.get(row)?;
                Some(crate::stats::Group { key: vec![Value::Text(f.id.to_string())], rows: vec![row] })
            })
            .collect::<Vec<_>>()
    } else {
        crate::stats::group_rows(zones, &selected, &keys)
    };
    crate::control::begin(r.height as usize + groups.len());
    let classes = classes(r, *band)?;
    let width = classes.len() + 1;
    if groups.len().saturating_mul(width) > MAX_PIVOT_CELLS {
        return Err(format!("Tabulate Area would make more than {MAX_PIVOT_CELLS} table values; select fewer zones"));
    }
    let index: HashMap<u32, usize> = classes.iter().enumerate().map(|(i, &v)| (class_key(v), i)).collect();
    let areas = CellAreas::new(r, count);
    // The largest grouped geometry's copies (taken as four, in any system) and on the grid, the
    // largest raster mask and its trimmed copy, plus each zone's dense class totals.
    // Conservatively include the shared row cache in each worker's estimate so it stays within
    // the scratch allowance.
    let geometry_bytes = groups
        .iter()
        .map(|group| {
            group
                .rows
                .iter()
                .filter_map(|&row| zones.features.get(row)?.geometry.as_ref())
                .map(|g| coord_bytes(g).saturating_mul(4))
                .fold(0usize, usize::saturating_add)
        })
        .max()
        .unwrap_or(0);
    let scratch = (r.width as usize)
        .saturating_mul(r.height as usize)
        .saturating_mul(2)
        .saturating_add(classes.len().saturating_mul(size_of::<f64>()))
        .saturating_add(areas.cache_bytes)
        .saturating_add(geometry_bytes);
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_scratch(groups.len(), PARALLEL_FEATURES, scratch));
    // The zones are in the run's system: their own projected one, converted to the raster's
    // (none of it when that is the raster's too), else longitude/latitude.
    let own = (inp.work_crs() != Crs::LonLat).then(|| crate::tools::ToGrid::new(inp.work_crs(), r.crs)).transpose()?;
    let to = own.as_ref().map_or(crate::rasterops::ToWorld::LonLat(r.transformer()), crate::rasterops::ToWorld::Own);
    let totals =
        crate::parallel::try_map_on(&groups, threads, || (), |_, _, group| summarize(r, to, *band, zones, &group.rows, &index, &areas))?;
    let zone_field = keys.first().and_then(|&i| zones.fields.get(i)).cloned().unwrap_or_else(|| Field::new("zone_id", FieldType::Text));
    let mut out = Dataset::new(out_name(p, format!("{}_TabulateArea", zones.name)), GeometryType::None, vec![zone_field]);
    for &class in &classes {
        crate::control::check()?;
        let name = unique_field(&out, &format!("class_{class}"));
        out.fields.push(Field::new(name, if count { FieldType::Integer } else { FieldType::Double }));
    }
    let (mut nodata, mut invalid, mut unreadable, mut parts, mut empty) = (0u64, 0usize, 0usize, 0usize, 0usize);
    for (group, total) in groups.into_iter().zip(totals) {
        crate::control::check()?;
        nodata += total.nodata;
        invalid += total.invalid;
        unreadable += total.unreadable;
        parts += total.parts;
        empty += usize::from(total.values.iter().all(|v| *v == 0.0));
        let mut attrs = group.key;
        attrs.extend(total.values.into_iter().map(|v| if count { Value::Int(v as i64) } else { Value::Float(v / divisor) }));
        out.push_next(None, attrs);
    }
    let mut messages =
        vec![format!("Tabulated {} zone(s) by {} class(es), in {}", out.features.len(), classes.len(), if count { "cells" } else { unit })];
    if !count && !ellipsoidal_area(r) {
        messages.push(format!(
            "Area for {} zone(s) uses the raster projection ({}); it may differ from ellipsoidal ground area. Use an equal-area raster for ground-area summaries",
            out.features.len(), r.crs.label()
        ));
    }
    if nodata > 0 {
        messages.push(format!("{nodata} NoData or non-finite cell(s) inside zones were excluded (counted once per zone)"));
    }
    if invalid > 0 {
        messages.push(format!("{invalid} zone feature(s) had no polygon with an area and were excluded"));
    }
    let mut losses = Vec::new();
    if unreadable > 0 {
        losses.push(LossMessage {
            code: LossCode::DroppedGeometry,
            text: format!("{unreadable} zone feature(s) had invalid polygon geometry and were excluded"),
        });
    }
    if parts > 0 {
        losses.push(LossMessage {
            code: LossCode::DroppedGeometry,
            text: format!("{parts} polygon part(s) could not be projected onto the raster grid; valid parts were retained"),
        });
    }
    if empty > 0 {
        messages.push(format!("{empty} zone(s) contained no valid raster cell centres; their class values are zero"));
    }
    Ok(ToolResult { losses, output: Output::Table(out), messages })
}

/// All finite valid band values, sorted numerically. The wide table has Pivot Table's column
/// limit, so a continuous band cannot allocate a column for every measurement.
fn classes(r: &Raster, band: usize) -> Result<Vec<f32>, String> {
    let mut found = HashSet::new();
    let mut pace = crate::control::Pace::default();
    for row in 0..r.height {
        crate::control::check()?;
        for col in 0..r.width {
            if pace.stop(1) {
                return Err(crate::control::Cancelled.into());
            }
            if let Some(v) = r.value(band, i64::from(col), i64::from(row)) {
                found.insert(class_key(v));
                if found.len() > MAX_PIVOT_COLUMNS {
                    return Err(format!("the categorical band has more than {MAX_PIVOT_COLUMNS} classes; reclassify it first"));
                }
            }
        }
        crate::control::advance(1);
    }
    let mut values: Vec<_> = found.into_iter().map(f32::from_bits).collect();
    values.sort_by(f32::total_cmp);
    Ok(values)
}

/// Canonicalises negative zero so equal numeric classes share one column.
fn class_key(v: f32) -> u32 {
    if v == 0.0 { 0 } else { v.to_bits() }
}

/// One zone's class totals and the counts of omitted inputs, kept separate from output messages
/// so workers produce identical messages in table order.
struct Totals {
    values: Vec<f64>,
    nodata: u64,
    invalid: usize,
    /// Zone features whose polygon couldn't be read.
    unreadable: usize,
    parts: usize,
}

/// Masks a whole group together: a cell in overlapping features or parts of the same zone
/// counts once. The zones' places reach the raster's system by `to`.
fn summarize(
    r: &Raster,
    to: crate::rasterops::ToWorld<'_>,
    band: usize,
    zones: &Dataset,
    rows: &[usize],
    classes: &HashMap<u32, usize>,
    areas: &CellAreas<'_>,
) -> Result<Totals, String> {
    let mut total = Totals { values: vec![0.0; classes.len()], nodata: 0, invalid: 0, unreadable: 0, parts: 0 };
    let mut polygons = Vec::new();
    for &row in rows {
        crate::control::check()?;
        let shape = zones.features.get(row).and_then(|f| f.geometry.as_ref());
        let valid = match shape {
            Some(Geometry::Polygon(p)) => crate::rasterops::validated_polygon_parts(p).ok(),
            _ => None,
        };
        // A polygon that can't be read is lost to its zone; a feature with no polygon, or none
        // with an area, has no area to lose.
        match (valid, shape) {
            (Some(p), _) => polygons.extend(p),
            (None, Some(Geometry::Polygon(p))) if crate::rasterops::has_a_ring(p) => total.unreadable += 1,
            (None, _) => total.invalid += 1,
        }
    }
    let mut failures = Vec::new();
    let mask = crate::rasterops::polygon_mask_on(to, &r.transform, (r.width, r.height), &polygons, &mut failures)?;
    total.parts = failures.len();
    crate::control::check()?;
    let Some((cols, rows, mask)) = mask else { return Ok(total) };
    let width = (cols.end - cols.start) as usize;
    let mut pace = crate::control::Pace::default();
    for (ri, row) in rows.enumerate() {
        crate::control::check()?;
        let mut row_area = areas.constant;
        let line = mask.get(ri * width..(ri + 1) * width).ok_or("invalid zone mask")?;
        for (ci, &inside) in line.iter().enumerate() {
            if pace.stop(1) {
                return Err(crate::control::Cancelled.into());
            }
            if !inside {
                continue;
            }
            let col = cols.start + ci as u32;
            let Some(v) = r.value(band, i64::from(col), i64::from(row)) else {
                total.nodata += 1;
                continue;
            };
            let area = if let Some(a) = row_area {
                a
            } else {
                let a = areas.at(col, row)?;
                if areas.by_row {
                    row_area = Some(a);
                }
                a
            };
            if !area.is_finite() || area <= 0.0 {
                return Err("raster cell area is not finite and positive".into());
            }
            let value = classes.get(&class_key(v)).and_then(|&i| total.values.get_mut(i)).ok_or("class missing from band catalog")?;
            *value += area;
            if !value.is_finite() {
                return Err("class area exceeds the numeric range".into());
            }
        }
    }
    Ok(total)
}

/// Geographic and Web Mercator grids use ground area; other projections retain their grid area.
fn ellipsoidal_area(r: &Raster) -> bool {
    r.transformer().is_geographic() || r.crs == Crs::WebMercator
}

/// Ground area cached by row for WGS 84 lon/lat and Web Mercator. A fixed first-column cell
/// makes rounding independent of zone order and worker scheduling. Other geographic datums or
/// axis orders can vary across a row after transformation, so keep their per-cell calculation.
struct CellAreas<'a> {
    raster: &'a Raster,
    constant: Option<f64>,
    by_row: bool,
    rows: std::sync::Mutex<Vec<Option<f64>>>,
    cache_bytes: usize,
}

impl<'a> CellAreas<'a> {
    /// Bounds the shared cache by the scratch budget. The worker estimate includes this shared
    /// allocation conservatively; refused allocations and rows past its limit cost speed only.
    fn new(r: &'a Raster, count: bool) -> Self {
        let constant = if count {
            Some(1.0)
        } else if ellipsoidal_area(r) {
            None
        } else {
            let metres = r.transformer().params().map_or(1.0, |p| p.to_meter);
            Some((r.transform.pixel_width * r.transform.pixel_height).abs() * metres * metres)
        };
        let by_row = matches!(r.crs, Crs::LonLat | Crs::WebMercator);
        let mut rows = Vec::new();
        if constant.is_none() && by_row {
            let n = (r.height as usize).min(crate::parallel::scratch_budget() / size_of::<Option<f64>>());
            if rows.try_reserve_exact(n).is_ok() {
                rows.resize(n, None);
            }
        }
        let cache_bytes = rows.capacity().saturating_mul(size_of::<Option<f64>>());
        Self { raster: r, constant, by_row, rows: std::sync::Mutex::new(rows), cache_bytes }
    }

    /// Measures only rows that contain a valid masked cell. Workers share each cached value;
    /// no-data-only or off-raster zones neither fill the cache nor cause corner-transform errors.
    fn at(&self, col: u32, row: u32) -> Result<f64, String> {
        if !self.by_row {
            return geographic_cell_area(self.raster, col, row);
        }
        let mut cache = self.rows.lock().map_err(|_| "raster row area cache could not be accessed")?;
        if let Some(Some(a)) = cache.get(row as usize) {
            return Ok(*a);
        }
        let a = geographic_cell_area(self.raster, 0, row)?;
        if let Some(cached) = cache.get_mut(row as usize) {
            *cached = Some(a);
        }
        Ok(a)
    }
}

/// Measures a cell's four corners on WGS 84 using the shared ellipsoidal area calculation.
/// A corner that cannot transform is an error.
fn geographic_cell_area(r: &Raster, col: u32, row: u32) -> Result<f64, String> {
    let (x, y) = (f64::from(col), f64::from(row));
    let corners = [(x, y), (x + 1.0, y), (x + 1.0, y + 1.0), (x, y + 1.0)];
    let ring = corners
        .into_iter()
        .map(|(c, row)| {
            r.transformer().to_lonlat(r.transform.to_world(c, row)).ok_or("a raster cell corner could not be transformed to WGS 84")
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rustgis_geom::wgs84_ring_perimeter_area_m2(&ring).1)
}
