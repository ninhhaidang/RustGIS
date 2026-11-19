//! A5 grids (opengeos/RustGIS#551): [A5](https://a5geo.org)'s pentagonal cells, which all have
//! the same area at a given resolution, as a polygon layer, and points counted by cell.
//!
//! A count per cell is then a density with no per-cell normalisation, unlike a grid whose cells
//! change size with latitude or projection. Cells are identified by A5's 64-bit ids, written in
//! hex (`a5`) so they survive CSV and JSON and join with data from other A5 tools.

use std::collections::{HashMap, HashSet};

use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
use rustgis_geom::{Coord, Extent};
use serde_json::Value as Json;

use crate::{Inputs, Output, ToolResult};

/// The most cells one run makes.
pub const MAX_CELLS: u64 = 200_000;
/// A5's finest resolution.
pub const MAX_RESOLUTION: i32 = 30;
/// How far over [`MAX_CELLS`] an area's [`estimate`] may go before it's refused without being
/// covered: the estimate is rough, and the exact count still applies after.
const ESTIMATE_MARGIN: f64 = 2.0;

fn num(p: &Json, k: &str) -> Option<f64> {
    p.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).filter(|f: &f64| f.is_finite())
}

fn text<'a>(p: &'a Json, k: &str) -> &'a str {
    p.get(k).and_then(Json::as_str).unwrap_or("")
}

/// The resolution asked for: a whole number from 0 to 30.
fn resolution(p: &Json, default: f64) -> Result<i32, String> {
    let r = num(p, "resolution").unwrap_or(default);
    if r.fract() != 0.0 || !(0.0..=f64::from(MAX_RESOLUTION)).contains(&r) {
        return Err(format!("Resolution is a whole number from 0 to {MAX_RESOLUTION}"));
    }
    Ok(r as i32)
}

fn output_name(p: &Json, default: &str) -> String {
    match text(p, "output").trim() {
        "" => default.to_string(),
        n => n.chars().take(200).collect(),
    }
}

/// A cell's area in square kilometres (every cell of a resolution has it).
fn area_km2(res: i32) -> f64 {
    a5::cell_area(res) / 1e6
}

/// A cell's outline as a longitude/latitude ring. Its longitudes run on from one vertex to the
/// next (past ±180° for a cell across the antimeridian), and a cell around a pole is closed along
/// the pole, so the ring is a polygon the map can draw.
pub fn ring(cell: u64) -> Option<Vec<Coord>> {
    let b = a5::cell_to_boundary(cell, None).ok()?;
    let mut out: Vec<Coord> = Vec::with_capacity(b.len() + 4);
    for ll in &b {
        let (mut lon, lat) = (ll.longitude(), ll.latitude());
        if let Some(prev) = out.last() {
            while lon - prev.x > 180.0 {
                lon -= 360.0;
            }
            while lon - prev.x < -180.0 {
                lon += 360.0;
            }
        }
        out.push(Coord::new(lon, lat));
    }
    let (first, last) = (*out.first()?, *out.last()?);
    // Around a pole the longitudes turn a whole circle: close the ring along the pole.
    let turn = last.x - first.x;
    if turn.abs() > 180.0 {
        let pole = if out.iter().map(|c| c.y).sum::<f64>() > 0.0 { 90.0 } else { -90.0 };
        out.push(Coord::new(last.x, pole));
        out.push(Coord::new(first.x, pole));
        out.push(first);
    }
    (out.len() >= 4).then_some(out)
}

/// The part of a closed ring west (`west`) or east of the meridian at `x`, closed; the points
/// where it crosses the meridian are put on it.
fn clip(ring: &[Coord], x: f64, west: bool) -> Vec<Coord> {
    let inside = |c: &Coord| if west { c.x <= x } else { c.x >= x };
    let mut out: Vec<Coord> = Vec::with_capacity(ring.len() + 4);
    for w in ring.windows(2) {
        let (a, b) = (w[0], w[1]);
        if inside(&a) {
            out.push(a);
        }
        if inside(&a) != inside(&b) {
            let t = (x - a.x) / (b.x - a.x);
            out.push(Coord::new(x, a.y + t * (b.y - a.y)));
        }
    }
    if let Some(first) = out.first().copied()
        && out.last() != Some(&first)
    {
        out.push(first);
    }
    out
}

/// A cell as a polygon for the map: its [`ring`], or, for a cell across the antimeridian (and one
/// round a pole), its two sides of ±180°, each within −180° to 180°, as the parts of one polygon,
/// the way Natural Earth splits a country there.
pub fn parts(cell: u64) -> Option<Vec<Vec<Vec<Coord>>>> {
    let r = ring(cell)?;
    let (lo, hi) = r.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.x), hi.max(c.x)));
    if lo >= -180.0 && hi <= 180.0 {
        return Some(vec![vec![r]]);
    }
    // The side past the meridian is moved back by a turn.
    let (edge, turn) = if hi > 180.0 { (180.0, -360.0) } else { (-180.0, 360.0) };
    let near = clip(&r, edge, edge > 0.0);
    let far: Vec<Coord> = clip(&r, edge, edge < 0.0).into_iter().map(|c| Coord::new(c.x + turn, c.y)).collect();
    // A side that only touches the meridian (a corner on it, so fewer than four points) has no
    // area and is left out.
    let out: Vec<Vec<Vec<Coord>>> = [near, far].into_iter().filter(|p| p.len() >= 4).map(|p| vec![p]).collect();
    (!out.is_empty()).then_some(out)
}

/// Whether a polygon's ring can be covered: three corners or more, each a longitude and latitude
/// (within ±90°, and ±360° so a ring that runs on past the antimeridian is kept).
fn usable(ring: &[Coord]) -> bool {
    let closed = ring.len() > 1 && ring.first() == ring.last();
    ring.len() - usize::from(closed) >= 3 && ring.iter().all(|c| c.is_finite() && c.y.abs() <= 90.0 && c.x.abs() <= 360.0)
}

/// The fields every A5 layer starts with: the cell's id (hex), resolution and area.
fn cell_fields() -> Vec<Field> {
    vec![Field::new("a5", FieldType::Text), Field::new("resolution", FieldType::Integer), Field::new("area_km2", FieldType::Double)]
}

fn cell_attrs(cell: u64, res: i32) -> Vec<Value> {
    vec![Value::Text(a5::u64_to_hex(cell)), Value::Int(i64::from(res)), Value::Float(area_km2(res))]
}

/// An extent as a ring, its edges followed every degree, so a parallel isn't taken for a great
/// circle.
fn extent_ring(e: Extent) -> Vec<a5::LonLat> {
    let steps = |a: f64, b: f64| ((b - a).abs().ceil() as usize).clamp(1, 720);
    let mut ring = Vec::new();
    let n = steps(e.min_x, e.max_x);
    ring.extend((0..n).map(|i| a5::LonLat::new(e.min_x + (e.max_x - e.min_x) * i as f64 / n as f64, e.min_y)));
    let m = steps(e.min_y, e.max_y);
    ring.extend((0..m).map(|i| a5::LonLat::new(e.max_x, e.min_y + (e.max_y - e.min_y) * i as f64 / m as f64)));
    ring.extend((0..n).map(|i| a5::LonLat::new(e.max_x - (e.max_x - e.min_x) * i as f64 / n as f64, e.max_y)));
    ring.extend((0..m).map(|i| a5::LonLat::new(e.min_x, e.max_y - (e.max_y - e.min_y) * i as f64 / m as f64)));
    ring
}

/// An extent within the globe's longitudes and latitudes, for a view panned past ±180° or zoomed
/// out past the poles; `None` when nothing of it is on the globe. A side with no width (one point,
/// or a line along a parallel or meridian) is widened by a hair, so the cells holding it are found.
fn on_the_globe(e: Extent) -> Option<Extent> {
    const HAIR: f64 = 1e-7;
    let side = |lo: f64, hi: f64, limit: f64| {
        let (lo, hi) = (lo.max(-limit), hi.min(limit));
        if lo == hi { ((lo - HAIR).max(-limit), (hi + HAIR).min(limit)) } else { (lo, hi) }
    };
    let ((x0, x1), (y0, y1)) = (side(e.min_x, e.max_x, 180.0), side(e.min_y, e.max_y, 90.0));
    (x0 < x1 && y0 < y1).then(|| Extent::new(x0, y0, x1, y1))
}

fn too_many(n: impl std::fmt::Display, res: i32) -> String {
    format!("{n} cells at resolution {res} is too many (at most {MAX_CELLS}): use a coarser resolution or a smaller area")
}

/// About how many cells cover a polygon (its outer ring, then its holes) at a resolution, worked
/// out before A5 covers it: its area over a cell's, and a cell per cell's width along its rings,
/// where a covering adds the cells its edges cross. Edges are taken as great circles.
fn estimate(rings: &[Vec<a5::LonLat>], res: i32) -> f64 {
    // A5's authalic sphere.
    let radius = (a5::cell_area(-1) / (4.0 * std::f64::consts::PI)).sqrt();
    let (mut area, mut length) = (0.0, 0.0);
    for (i, ring) in rings.iter().enumerate() {
        let (mut twice, mut arc) = (0.0, 0.0);
        for (k, a) in ring.iter().enumerate() {
            let b = &ring[(k + 1) % ring.len()];
            let (l1, p1, l2, p2) =
                (a.longitude().to_radians(), a.latitude().to_radians(), b.longitude().to_radians(), b.latitude().to_radians());
            let mut dl = l2 - l1;
            if dl > std::f64::consts::PI {
                dl -= std::f64::consts::TAU;
            } else if dl < -std::f64::consts::PI {
                dl += std::f64::consts::TAU;
            }
            twice += dl * (2.0 + p1.sin() + p2.sin());
            let h = ((p2 - p1) / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
            arc += 2.0 * h.clamp(0.0, 1.0).sqrt().asin();
        }
        let a = (twice / 2.0).abs() * radius * radius;
        area += if i == 0 { a } else { -a };
        length += arc * radius;
    }
    let cell = a5::cell_area(res);
    if !area.is_finite() || !length.is_finite() {
        return f64::INFINITY;
    }
    area.max(0.0) / cell + length / cell.sqrt()
}

/// The cells at a resolution of coverings (each compacted, as A5 gives them), each once and in id
/// order; more than [`MAX_CELLS`] is an error. A covering's cells don't overlap, so it's counted
/// exactly before any is made. Coverings can overlap one another (neighbouring parcels, buffers),
/// so they're merged into a set as they come and a cell they share counts once.
fn cells_of(coverings: impl IntoIterator<Item = Result<Vec<u64>, String>>, res: i32) -> Result<Vec<u64>, String> {
    // A coarser cell holds the ratio of the two resolutions' cell counts.
    let total = a5::get_num_cells(res);
    let mut cells: HashSet<u64> = HashSet::new();
    // Covered one at a time, so a run over the cap stops at the covering that takes it there.
    for covering in coverings {
        let covering = covering?;
        let parts = || covering.iter().copied().filter(|c| !a5::is_compaction_marker(*c));
        let n = parts().fold(0u64, |n, c| n.saturating_add(total / a5::get_num_cells(a5::get_resolution(c)).max(1)));
        if n > MAX_CELLS {
            return Err(too_many(n, res));
        }
        for c in parts() {
            if a5::get_resolution(c) == res {
                cells.insert(c);
            } else {
                cells.extend(a5::cell_to_children(c, Some(res))?);
            }
        }
        if cells.len() as u64 > MAX_CELLS {
            return Err(too_many(format!("At least {}", cells.len()), res));
        }
    }
    let mut cells: Vec<u64> = cells.into_iter().collect();
    cells.sort_unstable();
    Ok(cells)
}

/// "1 polygon part" or "2 polygon parts" (and points) without a usable outline (or position).
fn left_out(n: usize, what: &str, lacking: &str) -> String {
    match n {
        1 => format!("1 {what} without a usable {lacking} was left out"),
        n => format!("{n} {what}s without a usable {lacking} were left out"),
    }
}

/// The A5 Grid tool: the cells at a resolution over the map's extent, a layer's polygons (or its
/// extent, for points and lines), or the whole world.
pub fn grid(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let res = resolution(p, 5.0)?;
    let options = a5::PolygonToCellsOptions {
        containment: if text(p, "containment") == "Center" { a5::Containment::Center } else { a5::Containment::Overlapping },
    };
    let area = match text(p, "area") {
        "" | "Map Extent" => "Map Extent",
        a @ ("Input Layer" | "World") => a,
        a => return Err(format!("Area is Map Extent, Input Layer or World, not {}", a.chars().take(40).collect::<String>())),
    };
    // Polygons left out for want of a usable outline.
    let mut skipped = 0usize;
    let off = "is outside the globe's longitudes and latitudes (−180° to 180°, −90° to 90°)";
    let cells: Vec<u64> = if area == "World" {
        cells_of([a5::get_res0_cells()], res)?
    } else {
        // The polygons to cover, each its rings.
        let shapes: Vec<Vec<Vec<a5::LonLat>>> = if area == "Input Layer" {
            let name = text(p, "input");
            let ds = inp.dataset(name).ok_or_else(|| format!("No layer named {name}"))?;
            let sel = inp.selection(name);
            let rows: Vec<usize> = if sel.is_empty() { (0..ds.features.len()).collect() } else { sel.to_vec() };
            let mut shapes = Vec::new();
            let mut extent = Extent::EMPTY;
            for r in rows {
                match ds.features.get(r).and_then(|f| f.geometry.as_ref()) {
                    Some(Geometry::Polygon(polys)) => {
                        // A polygon whose outline isn't a ring is left out; so is a hole that isn't one.
                        for poly in polys {
                            if !poly.first().is_some_and(|r| usable(r)) {
                                skipped += 1;
                                continue;
                            }
                            shapes.push(
                                poly.iter().filter(|r| usable(r)).map(|r| r.iter().map(|c| a5::LonLat::new(c.x, c.y)).collect()).collect(),
                            );
                        }
                    }
                    Some(g) => extent = extent.union(&g.extent()),
                    None => {}
                }
            }
            if !extent.is_empty() {
                shapes.push(vec![extent_ring(on_the_globe(extent).ok_or_else(|| format!("The extent of {name} {off}"))?)]);
            }
            if shapes.is_empty() {
                return Err(match skipped {
                    0 => format!("{name} has no features to cover"),
                    n => format!("{name} has nothing to cover: {}", left_out(n, "polygon part", "outline")),
                });
            }
            shapes
        } else {
            let e = inp.view_extent().ok_or("There's no map view to fill; choose World or Input Layer")?;
            vec![vec![extent_ring(on_the_globe(e).ok_or_else(|| format!("The map view {off}"))?)]]
        };
        // Far too many is refused before A5 works on it; the count after is exact.
        let guess: f64 = shapes.iter().map(|s| estimate(s, res)).sum();
        if guess > ESTIMATE_MARGIN * MAX_CELLS as f64 {
            return Err(too_many(if guess.is_finite() { format!("About {guess:.0}") } else { "Too many".into() }, res));
        }
        let cells = cells_of(shapes.iter().map(|s| a5::polygon_to_cells(s, res, Some(options))), res)?;
        if cells.is_empty() {
            return Err(format!("No cell at resolution {res} has its centre in the area: choose Cells: Overlapping or a finer resolution"));
        }
        cells
    };
    let mut out = Dataset::new(output_name(p, &format!("A5 r{res}")), GeometryType::Polygon, cell_fields());
    for c in &cells {
        if let Some(polygon) = parts(*c) {
            out.push(Some(Geometry::Polygon(polygon)), cell_attrs(*c, res));
        }
    }
    let mut msg = format!("A5 grid: {} cells at resolution {res}, each {:.6} km²", out.features.len(), area_km2(res));
    if skipped > 0 {
        msg.push_str(&format!("; {}", left_out(skipped, "polygon part", "outline")));
    }
    Ok(ToolResult { losses: Vec::new(), output: Output::Layer(out), messages: vec![msg] })
}

/// The Aggregate Points to A5 tool: a layer's points counted by the cell holding them (and a
/// numeric field summed and averaged), as the cells that hold any.
pub fn aggregate(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let res = resolution(p, 9.0)?;
    let name = text(p, "input");
    let ds = inp.dataset(name).ok_or_else(|| format!("No layer named {name}"))?;
    if ds.geometry_type != GeometryType::Point {
        return Err(format!("{name} isn't a point layer"));
    }
    let field = match text(p, "field").trim() {
        "" => None,
        f => Some(ds.field_index(f).ok_or_else(|| format!("{name} has no field {f}"))?),
    };
    let sel = inp.selection(name);
    let rows: Vec<usize> = if sel.is_empty() { (0..ds.features.len()).collect() } else { sel.to_vec() };
    // Per cell: its points, and the field's sum and how many values it had (a feature's value
    // once in each cell, however many of a multipoint's points fall in it).
    let mut by: HashMap<u64, (u64, f64, u64)> = HashMap::new();
    let mut outside = 0usize;
    for r in rows {
        let Some(f) = ds.features.get(r) else { continue };
        let pts: Vec<Coord> = match &f.geometry {
            Some(Geometry::Point(c)) => vec![*c],
            Some(Geometry::MultiPoint(cs)) => cs.clone(),
            _ => continue,
        };
        let v = field.and_then(|i| ds.value(r, i).as_f64()).filter(|v| v.is_finite());
        let mut credited: HashSet<u64> = HashSet::new();
        for c in pts {
            if !c.is_finite() || c.y.abs() > 90.0 || c.x.abs() > 360.0 {
                outside += 1;
                continue;
            }
            let Ok(cell) = a5::lonlat_to_cell(a5::LonLat::new(c.x, c.y), res) else {
                outside += 1;
                continue;
            };
            let e = by.entry(cell).or_insert((0, 0.0, 0));
            e.0 += 1;
            if let Some(v) = v
                && credited.insert(cell)
            {
                e.1 += v;
                e.2 += 1;
            }
            if by.len() as u64 > MAX_CELLS {
                return Err(format!("The points fall in more than {MAX_CELLS} cells at resolution {res}: use a coarser resolution"));
            }
        }
    }
    if by.is_empty() {
        return Err(format!("{name} has no points to count"));
    }
    let mut fields = cell_fields();
    fields.extend([Field::new("count", FieldType::Integer), Field::new("density_per_km2", FieldType::Double)]);
    let fname = field.and_then(|i| ds.fields.get(i)).map(|f| f.name.clone());
    if let Some(f) = &fname {
        fields.extend([Field::new(format!("{f}_sum"), FieldType::Double), Field::new(format!("{f}_mean"), FieldType::Double)]);
    }
    let mut out = Dataset::new(output_name(p, &format!("{name} A5 r{res}")), GeometryType::Polygon, fields);
    let mut cells: Vec<(&u64, &(u64, f64, u64))> = by.iter().collect();
    cells.sort_unstable_by_key(|(c, _)| **c);
    let area = area_km2(res);
    for (c, (n, sum, nv)) in cells {
        let Some(polygon) = parts(*c) else { continue };
        let mut attrs = cell_attrs(*c, res);
        attrs.extend([Value::Int(i64::try_from(*n).unwrap_or(i64::MAX)), Value::Float(*n as f64 / area)]);
        if fname.is_some() {
            attrs.extend([
                if *nv > 0 { Value::Float(*sum) } else { Value::Null },
                if *nv > 0 { Value::Float(sum / *nv as f64) } else { Value::Null },
            ]);
        }
        out.push(Some(Geometry::Polygon(polygon)), attrs);
    }
    let mut msg = format!("{} cells at resolution {res} ({area:.6} km² each) hold the points", out.features.len());
    if outside > 0 {
        msg.push_str(&format!("; {}", left_out(outside, "point", "position")));
    }
    Ok(ToolResult { losses: Vec::new(), output: Output::Layer(out), messages: vec![msg] })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inputs with one layer and a view.
    struct One(Dataset, Option<Extent>);
    impl Inputs for One {
        fn dataset(&self, n: &str) -> Option<&Dataset> {
            (n == self.0.name).then_some(&self.0)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
        fn view_extent(&self) -> Option<Extent> {
            self.1
        }
    }

    fn points(cs: &[(f64, f64, f64)]) -> Dataset {
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        for (x, y, v) in cs {
            d.push(Some(Geometry::Point(Coord::new(*x, *y))), vec![Value::Float(*v)]);
        }
        d
    }

    fn layer(r: ToolResult) -> Dataset {
        match r.output {
            Output::Layer(d) => d,
            _ => panic!("a layer"),
        }
    }

    /// Equal areas at a resolution, against A5's own count of the Earth's cells; every cell's
    /// ring closes and holds its centre.
    #[test]
    fn a_grid_of_equal_cells() {
        let inp = One(points(&[]), Some(Extent::new(-56.5, -13.5, -55.0, -12.0)));
        let d = layer(grid(&inp, &serde_json::json!({"resolution": 7})).unwrap());
        assert!(d.features.len() > 4, "{}", d.features.len());
        let areas: Vec<f64> = (0..d.features.len()).map(|r| d.value(r, 2).as_f64().unwrap()).collect();
        assert!(areas.windows(2).all(|w| w[0] == w[1]));
        let earth = 4.0 * std::f64::consts::PI * 6_371_007.180_918_475f64.powi(2) / 1e6;
        assert!((areas[0] * a5::get_num_cells(7) as f64 / earth - 1.0).abs() < 1e-6, "{}", areas[0]);
        for f in d.features.iter() {
            let Some(Geometry::Polygon(p)) = &f.geometry else { panic!("a polygon") };
            assert_eq!(p[0][0].first(), p[0][0].last());
        }
        // The whole world at resolution 1, and the cap.
        let w = layer(grid(&inp, &serde_json::json!({"resolution": 1, "area": "World"})).unwrap());
        assert_eq!(w.features.len() as u64, a5::get_num_cells(1));
        assert!(grid(&inp, &serde_json::json!({"resolution": 12, "area": "World"})).unwrap_err().contains("too many"));
        // The finest resolution's cell count (15·4³⁰, about 1.7e19) fits in a u64: the world there is
        // counted, not overflowed, and refused.
        let e = grid(&inp, &serde_json::json!({"resolution": 30, "area": "World"})).unwrap_err();
        assert!(e.starts_with(&a5::get_num_cells(30).to_string()) && e.contains("too many"), "{e}");
        // A view far too big for the resolution is refused by the estimate, before it's covered
        // (a covering whose edges alone cross millions of cells); one just over by the exact count.
        let wide = One(points(&[]), Some(Extent::new(-60.0, -30.0, -40.0, -10.0)));
        let e = grid(&wide, &serde_json::json!({"resolution": 30})).unwrap_err();
        assert!(e.starts_with("About ") && e.contains("too many"), "{e}");
        let e = grid(&One(points(&[]), Some(Extent::new(-60.0, -30.0, -46.0, -16.0))), &serde_json::json!({"resolution": 11})).unwrap_err();
        assert!(!e.starts_with("About ") && e.contains("too many"), "{e}");
        // A view panned past the antimeridian or the poles is cut to the globe; one off it is refused.
        let panned = layer(
            grid(&One(points(&[]), Some(Extent::new(-200.0, -100.0, -170.0, -80.0))), &serde_json::json!({"resolution": 3})).unwrap(),
        );
        assert!(!panned.features.is_empty());
        assert!(
            grid(&One(points(&[]), Some(Extent::new(190.0, 0.0, 200.0, 10.0))), &serde_json::json!({})).unwrap_err().contains("outside")
        );
        // A layer of one point, or of points along a parallel, has an extent with no width or
        // height: it's covered by the cells holding it, or none has its centre there.
        let one = One(points(&[(-55.72, -12.55, 1.0)]), None);
        let d = layer(grid(&one, &serde_json::json!({"resolution": 9, "area": "Input Layer", "input": "pts"})).unwrap());
        let here = a5::u64_to_hex(a5::lonlat_to_cell(a5::LonLat::new(-55.72, -12.55), 9).unwrap());
        assert!((0..d.features.len()).any(|r| d.value(r, 0) == &Value::Text(here.clone())), "{here}");
        let row = One(points(&[(-56.0, -12.55, 1.0), (-55.0, -12.55, 1.0)]), None);
        assert!(
            !layer(grid(&row, &serde_json::json!({"resolution": 9, "area": "Input Layer", "input": "pts"})).unwrap()).features.is_empty()
        );
        let e =
            grid(&one, &serde_json::json!({"resolution": 9, "area": "Input Layer", "input": "pts", "containment": "Center"})).unwrap_err();
        assert!(e.contains("Overlapping"), "{e}");
        // A polygon without a usable outline is left out, and said to be; an unknown area is refused.
        let mut shapes = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let square = vec![
            Coord::new(-56.0, -13.0),
            Coord::new(-55.0, -13.0),
            Coord::new(-55.0, -12.0),
            Coord::new(-56.0, -12.0),
            Coord::new(-56.0, -13.0),
        ];
        shapes.push(Some(Geometry::Polygon(vec![vec![square], vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]]])), vec![]);
        shapes.push(
            Some(Geometry::Polygon(vec![vec![vec![
                Coord::new(f64::NAN, 0.0),
                Coord::new(1.0, 0.0),
                Coord::new(1.0, 1.0),
                Coord::new(0.0, 0.0),
            ]]])),
            vec![],
        );
        let r = grid(&One(shapes, None), &serde_json::json!({"resolution": 7, "area": "Input Layer", "input": "pts"})).unwrap();
        assert!(r.messages[0].contains("2 polygon parts without a usable outline"), "{:?}", r.messages);
        assert!(!layer(r).features.is_empty());
        // Corners past the poles aren't a longitude and latitude: with nothing else, the error says
        // why. A ring that runs on past the antimeridian is covered.
        let mut off = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let tall =
            vec![Coord::new(0.0, 80.0), Coord::new(10.0, 80.0), Coord::new(10.0, 95.0), Coord::new(0.0, 95.0), Coord::new(0.0, 80.0)];
        off.push(Some(Geometry::Polygon(vec![vec![tall]])), vec![]);
        let e = grid(&One(off, None), &serde_json::json!({"resolution": 3, "area": "Input Layer", "input": "pts"})).unwrap_err();
        assert!(e.contains("nothing to cover: 1 polygon part without a usable outline was left out"), "{e}");
        // Overlapping polygons count the cells they share once: three copies of one, together
        // over the cap but not once merged, make that one's cells.
        let block = vec![
            Coord::new(-60.0, -30.0),
            Coord::new(-46.0, -30.0),
            Coord::new(-46.0, -16.0),
            Coord::new(-60.0, -16.0),
            Coord::new(-60.0, -30.0),
        ];
        let mut copies = Dataset::new("pts", GeometryType::Polygon, vec![]);
        for _ in 0..3 {
            copies.push(Some(Geometry::Polygon(vec![vec![block.clone()]])), vec![]);
        }
        let p10 = serde_json::json!({"resolution": 10, "area": "Input Layer", "input": "pts"});
        let three = layer(grid(&One(copies, None), &p10).unwrap()).features.len();
        let mut single = Dataset::new("pts", GeometryType::Polygon, vec![]);
        single.push(Some(Geometry::Polygon(vec![vec![block]])), vec![]);
        let alone = layer(grid(&One(single, None), &p10).unwrap()).features.len();
        assert!(3 * alone as u64 > MAX_CELLS && three == alone, "{alone} cells, {three} from three copies");
        let mut across = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let wide =
            vec![Coord::new(170.0, 0.0), Coord::new(190.0, 0.0), Coord::new(190.0, 10.0), Coord::new(170.0, 10.0), Coord::new(170.0, 0.0)];
        across.push(Some(Geometry::Polygon(vec![vec![wide]])), vec![]);
        let d = layer(grid(&One(across, None), &serde_json::json!({"resolution": 3, "area": "Input Layer", "input": "pts"})).unwrap());
        let at = |lon: f64| a5::u64_to_hex(a5::lonlat_to_cell(a5::LonLat::new(lon, 5.0), 3).unwrap());
        for lon in [175.0, -175.0] {
            assert!((0..d.features.len()).any(|r| d.value(r, 0) == &Value::Text(at(lon))), "{lon}");
        }
        assert!(grid(&inp, &serde_json::json!({"area": "Mars"})).unwrap_err().contains("Map Extent, Input Layer or World"));
        for bad in [serde_json::json!({"resolution": 31}), serde_json::json!({"resolution": 2.5}), serde_json::json!({"resolution": -1})] {
            assert!(grid(&inp, &bad).is_err(), "{bad}");
        }
        assert!(grid(&One(points(&[]), None), &serde_json::json!({})).is_err(), "no view");
    }

    /// The estimate before covering against the exact count after: it's of the same order, so
    /// a margin of [`ESTIMATE_MARGIN`] refuses only what's well over the cap.
    #[test]
    fn the_estimate_follows_the_count() {
        for (e, res) in [
            (Extent::new(-56.5, -13.5, -55.0, -12.0), 9),
            (Extent::new(-75.0, -35.0, -35.0, 5.0), 6),
            (Extent::new(-180.0, -90.0, 180.0, 90.0), 3),
        ] {
            let shape = vec![extent_ring(e)];
            let options = a5::PolygonToCellsOptions { containment: a5::Containment::Overlapping };
            let n = cells_of([a5::polygon_to_cells(&shape, res, Some(options))], res).unwrap().len() as f64;
            let guess = estimate(&shape, res);
            assert!(guess > n / ESTIMATE_MARGIN && guess < n * ESTIMATE_MARGIN, "{e:?} r{res}: {guess} for {n}");
        }
        // The whole globe's area is the authalic sphere's: every cell of a resolution.
        let world = estimate(&[extent_ring(Extent::new(-180.0, -90.0, 180.0, 90.0))], 5);
        assert!(world > a5::get_num_cells(5) as f64);
    }

    /// Rings across the antimeridian run on past ±180°; those round a pole close along it.
    #[test]
    fn rings_across_the_antimeridian_and_round_the_poles() {
        for (lon, lat) in [(179.99, 10.0), (-179.99, -40.0), (0.0, 89.99), (120.0, -89.99)] {
            let c = a5::lonlat_to_cell(a5::LonLat::new(lon, lat), 3).unwrap();
            let r = ring(c).unwrap();
            assert!(r.windows(2).all(|w| (w[1].x - w[0].x).abs() <= 180.0), "{lon},{lat}: {r:?}");
            assert_eq!(r.first(), r.last());
        }
        // For the map, a cell across the antimeridian is split there: its parts lie within ±180°
        // and cover what its ring did, as a pole's do. One elsewhere stays whole.
        let shoelace = |r: &[Coord]| r.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum::<f64>().abs() / 2.0;
        for (lon, lat) in [(179.99, 10.0), (-179.99, -40.0), (0.0, 89.99), (120.0, -89.99)] {
            let c = a5::lonlat_to_cell(a5::LonLat::new(lon, lat), 3).unwrap();
            let p = parts(c).unwrap();
            // Across the antimeridian, two parts; round a pole, two when its ring turns past ±180°.
            assert!(p.len() == 2 || lon.abs() < 179.0, "{lon},{lat}: {p:?}");
            assert!(p.iter().flatten().flatten().all(|c| (-180.0..=180.0).contains(&c.x)), "{lon},{lat}: {p:?}");
            assert!(p.iter().all(|poly| poly[0].first() == poly[0].last()));
            let whole = shoelace(&ring(c).unwrap());
            let split: f64 = p.iter().map(|poly| shoelace(&poly[0])).sum();
            assert!((split / whole - 1.0).abs() < 1e-9, "{lon},{lat}: {split} for {whole}");
        }
        let sorriso = a5::lonlat_to_cell(a5::LonLat::new(-55.72, -12.55), 3).unwrap();
        assert_eq!(parts(sorriso).unwrap(), vec![vec![ring(sorriso).unwrap()]]);
    }

    /// Points counted by cell: the count, the density by the cell's area, and a field's sum and mean.
    #[test]
    fn points_counted_by_cell() {
        let near = [
            (-55.72, -12.55, 1.0),
            (-55.7201, -12.5501, 3.0),
            (-55.7202, -12.5499, f64::NAN),
            (10.75, 59.91, 5.0),
            // A longitude no wrap makes sense of is left out.
            (1e12, 0.0, 1.0),
        ];
        let inp = One(points(&near), None);
        let d = layer(aggregate(&inp, &serde_json::json!({"input": "pts", "resolution": 12, "field": "v"})).unwrap());
        assert_eq!(d.features.len(), 2);
        let left = aggregate(&inp, &serde_json::json!({"input": "pts", "resolution": 12})).unwrap();
        assert!(left.messages[0].contains("1 point without a usable position was left out"), "{:?}", left.messages);
        let row = (0..2).find(|r| d.value(*r, 3).as_f64() == Some(3.0)).expect("three points in Sorriso's cell");
        let area = d.value(row, 2).as_f64().unwrap();
        assert!((d.value(row, 4).as_f64().unwrap() - 3.0 / area).abs() < 1e-9);
        assert_eq!((d.value(row, 5).as_f64(), d.value(row, 6).as_f64()), (Some(4.0), Some(2.0)));
        let Value::Text(id) = d.value(row, 0) else { panic!("a hex id") };
        assert_eq!(a5::hex_to_u64(id).ok(), a5::lonlat_to_cell(a5::LonLat::new(-55.72, -12.55), 12).ok());
        // A multipoint's value counts once in its cell, however many of its points are there.
        let mut multi = points(&[(-55.72, -12.55, 1.0)]);
        multi.push(Some(Geometry::MultiPoint(vec![Coord::new(-55.72, -12.55), Coord::new(-55.7201, -12.5501)])), vec![Value::Float(5.0)]);
        let d = layer(aggregate(&One(multi, None), &serde_json::json!({"input": "pts", "resolution": 12, "field": "v"})).unwrap());
        assert_eq!((d.value(0, 3).as_f64(), d.value(0, 5).as_f64(), d.value(0, 6).as_f64()), (Some(3.0), Some(6.0), Some(3.0)));
        for bad in [
            serde_json::json!({"input": "nope"}),
            serde_json::json!({"input": "pts", "field": "nope"}),
            serde_json::json!({"input": "pts", "resolution": 40}),
        ] {
            assert!(aggregate(&inp, &bad).is_err(), "{bad}");
        }
    }
}
