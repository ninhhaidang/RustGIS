//! Which rows of a layer have a shape whose box, in the layer's own coordinates, may meet a given
//! box: a grid of the rows' boxes, kept without retaining the rows until the rows change (any
//! edit makes them other rows, as [`Features::allocation_key`] says). The places an edit keeps
//! ([`crate::Layer::rows_touching`]) are looked for through it, so that search no longer reads
//! every feature of every layer in the edited layer's system (#2032 C2f-6).

use std::sync::{Mutex, Weak};

use rustgis_data::{Feature, Features};
use rustgis_geom::Extent;

/// The most cells a row's box is listed in; a box across more is listed as wide, and looked at by
/// every search (a long road among many short ones).
const MAX_CELLS_PER_ROW: usize = 16;

#[derive(Debug, Default)]
struct Grid {
    /// The rows the grid was made from, without keeping them.
    rows: Weak<Vec<Feature>>,
    /// The box all the listed boxes are in.
    total: Extent,
    cols: usize,
    cell_w: f64,
    cell_h: f64,
    /// Each cell's rows, row by row then column by column.
    cells: Vec<Vec<u32>>,
    /// Rows whose box spans too many cells.
    wide: Vec<u32>,
    /// Whether there are rows past `u32::MAX`, which aren't listed.
    beyond: bool,
}

#[derive(Debug, Default)]
pub(super) struct Cache(Mutex<Grid>);

/// A copy of a layer starts without a grid, as its rows may soon be other rows than these.
impl Clone for Cache {
    fn clone(&self) -> Self {
        Cache::default()
    }
}

impl Grid {
    /// The grid of `rows`' boxes, made in two walks over every row's vertices: one for the box
    /// they are all in, one to list each row's box, so no box is kept per row (a list of them
    /// would take 32 bytes a row while the grid is made). Capped at 256 by 256 cells, so the
    /// cells' headers (24 bytes each) take about 1.5 MB at most; each row is listed at 4 bytes a
    /// cell it is in, in at most [`MAX_CELLS_PER_ROW`] cells (usually one to four), or once in
    /// the wide list.
    fn new(rows: &Features) -> Grid {
        // A row's box, empty for a row without a shape or a place (`Geometry::extent` leaves out a
        // coordinate that isn't a number, so every other box is finite).
        let box_of = |i: usize| rows.geometry(i).map_or(Extent::EMPTY, |g| g.extent());
        let (mut total, mut shaped) = (Extent::EMPTY, 0usize);
        for i in 0..rows.len() {
            let e = box_of(i);
            if !e.is_empty() {
                total = total.union(&e);
                shaped += 1;
            }
        }
        // About one row a cell.
        let cols = ((shaped as f64).sqrt().ceil() as usize).clamp(1, 256);
        let mut grid = Grid {
            rows: rows.allocation_key(),
            total,
            cols,
            cell_w: total.width() / cols as f64,
            cell_h: total.height() / cols as f64,
            cells: vec![Vec::new(); cols * cols],
            wide: Vec::new(),
            beyond: false,
        };
        for i in 0..rows.len() {
            let e = &box_of(i);
            let Ok(row) = u32::try_from(i) else {
                // Past 4 billion rows a row can't be listed: every search looks at them all.
                grid.beyond = true;
                break;
            };
            match grid.span(e) {
                Some((x0, y0, x1, y1)) if (x1 - x0 + 1) * (y1 - y0 + 1) <= MAX_CELLS_PER_ROW => {
                    for y in y0..=y1 {
                        for x in x0..=x1 {
                            grid.cells[y * cols + x].push(row);
                        }
                    }
                }
                Some(_) => grid.wide.push(row),
                // No shape, or no place: never meets a box.
                None => {}
            }
        }
        grid
    }

    /// The column and row of the cell `v` is in along one axis, clamped to the grid.
    fn cell(&self, v: f64, min: f64, size: f64) -> usize {
        if size > 0.0 { (((v - min) / size).floor().max(0.0) as usize).min(self.cols - 1) } else { 0 }
    }

    /// The cells box `e` meets (first column, first row, last column, last row), `None` when it
    /// meets none.
    fn span(&self, e: &Extent) -> Option<(usize, usize, usize, usize)> {
        if !e.intersects(&self.total) {
            return None;
        }
        let t = &self.total;
        Some((
            self.cell(e.min_x, t.min_x, self.cell_w),
            self.cell(e.min_y, t.min_y, self.cell_h),
            self.cell(e.max_x, t.min_x, self.cell_w),
            self.cell(e.max_y, t.min_y, self.cell_h),
        ))
    }

    /// The rows of the `len` whose box may meet `within`, each once, in order.
    fn touching(&self, within: &Extent, len: usize) -> Vec<usize> {
        if self.beyond {
            return (0..len).collect();
        }
        let mut found: Vec<u32> = Vec::new();
        if let Some((x0, y0, x1, y1)) = self.span(within) {
            found.extend_from_slice(&self.wide);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    found.extend_from_slice(&self.cells[y * self.cols + x]);
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found.into_iter().map(|r| r as usize).collect()
    }
}

impl Cache {
    /// The rows of `rows` whose shape's box may meet `within` (a superset of those that do; one
    /// without a shape only past `u32::MAX` rows, when every row is given), each once, in order. The grid is made again only once the rows
    /// change.
    pub(super) fn touching(&self, rows: &Features, within: &Extent) -> Vec<usize> {
        // The grid is disposable; a poisoned lock does not invalidate the features themselves.
        let mut grid = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !grid.rows.ptr_eq(&rows.allocation_key()) {
            *grid = Grid::new(rows);
        }
        grid.touching(within, rows.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Dataset, Geometry, GeometryType};
    use rustgis_geom::Coord;

    fn line(a: (f64, f64), b: (f64, f64)) -> Option<Geometry> {
        Some(Geometry::Line(vec![vec![Coord::new(a.0, a.1), Coord::new(b.0, b.1)]]))
    }

    /// The rows meeting `within`, read one by one as the search did before the grid.
    fn scanned(ds: &Dataset, within: &Extent) -> Vec<usize> {
        (0..ds.features.len()).filter(|&i| ds.features.geometry(i).is_some_and(|g| g.extent().intersects(within))).collect()
    }

    #[test]
    fn the_grid_finds_every_row_a_scan_finds() {
        let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
        // A thousand short lines on a grid, a long one across them all, one without a shape and
        // one with a place that isn't a number.
        for i in 0..1000 {
            let (x, y) = (f64::from(i % 40) * 1000.0, f64::from(i / 40) * 1000.0);
            ds.push(line((x, y), (x + 400.0, y + 300.0)), vec![]);
        }
        ds.push(line((-5000.0, -5000.0), (50_000.0, 30_000.0)), vec![]);
        ds.push(None, vec![]);
        ds.push(line((f64::NAN, 3.0), (100.0, 200.0)), vec![]);
        let cache = Cache::default();
        for within in [
            Extent::new(0.0, 0.0, 1.0, 1.0),
            Extent::new(2500.0, 2500.0, 3500.0, 3200.0),
            Extent::new(-1e9, -1e9, 1e9, 1e9),
            Extent::new(39_000.0, 24_000.0, 39_500.0, 24_400.0),
            Extent::new(1e7, 1e7, 1e7 + 1.0, 1e7 + 1.0),
            Extent::new(100.0, 200.0, 100.0, 200.0),
        ] {
            let found = cache.touching(&ds.features, &within);
            let want = scanned(&ds, &within);
            assert!(want.iter().all(|r| found.contains(r)), "{within:?}: {want:?} not all in {found:?}");
            assert!(found.windows(2).all(|w| w[0] < w[1]));
            assert!(found.iter().all(|&r| ds.features.geometry(r).is_some()));
        }
        // Few rows looked at for a small box, not every one.
        assert!(cache.touching(&ds.features, &Extent::new(2500.0, 2500.0, 2600.0, 2600.0)).len() < 20);
    }

    #[test]
    fn the_grid_follows_the_rows_edits() {
        let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
        for i in 0..100 {
            let x = f64::from(i) * 10.0;
            ds.push(line((x, 0.0), (x + 1.0, 1.0)), vec![]);
        }
        let cache = Cache::default();
        let there = Extent::new(5000.0, 5000.0, 5001.0, 5001.0);
        assert!(cache.touching(&ds.features, &there).is_empty());
        let before = ds.features.clone();
        ds.features.set_geometry(7, line((5000.5, 5000.5), (5000.6, 5000.6)));
        assert_eq!(cache.touching(&ds.features, &there), [7]);
        assert!(cache.touching(&before, &there).is_empty());
        // Changed in place, with no other copy of the rows.
        drop(before);
        ds.features.set_geometry(7, line((0.0, 0.0), (1.0, 1.0)));
        assert!(cache.touching(&ds.features, &there).is_empty());
    }

    #[test]
    fn a_grid_with_no_width_or_rows_on_cell_edges_finds_them() {
        // Every line on x = 100: the boxes' total has no width.
        let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
        for i in 0..200 {
            ds.push(line((100.0, f64::from(i)), (100.0, f64::from(i) + 1.0)), vec![]);
        }
        let cache = Cache::default();
        for within in [Extent::new(100.0, 50.5, 100.0, 50.6), Extent::new(99.0, 199.0, 101.0, 300.0), Extent::new(0.0, 0.0, 99.9, 500.0)] {
            let found = cache.touching(&ds.features, &within);
            assert!(scanned(&ds, &within).iter().all(|r| found.contains(r)), "{within:?}: {found:?}");
        }
        // Points exactly on the cells' edges (a 15 by 15 grid of 225 rows over 0..=14).
        let mut ds = Dataset::new("points", GeometryType::Point, vec![]);
        for i in 0..225 {
            ds.push(Some(Geometry::Point(Coord::new(f64::from(i % 15), f64::from(i / 15)))), vec![]);
        }
        let cache = Cache::default();
        for i in 0..225 {
            let c = Coord::new(f64::from(i % 15), f64::from(i / 15));
            let within = Extent::new(c.x, c.y, c.x, c.y);
            assert!(cache.touching(&ds.features, &within).contains(&(i as usize)), "{c:?}");
        }
    }

    /// The rows `layer` gives for `within` whose box does meet it.
    fn meeting(layer: &crate::Layer, within: &Extent) -> Vec<usize> {
        let found = layer.rows_touching(within);
        found.into_iter().filter(|&r| layer.dataset.features.geometry(r).is_some_and(|g| g.extent().intersects(within))).collect()
    }

    #[test]
    fn a_layer_looks_up_its_rows_and_a_copy_makes_its_own_grid() {
        let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
        for i in 0..50 {
            let x = f64::from(i) * 10.0;
            ds.push(line((x, 0.0), (x + 1.0, 1.0)), vec![]);
        }
        let mut layer = crate::Layer::new(1, ds);
        let near = Extent::new(200.5, 0.5, 200.6, 0.6);
        assert_eq!(meeting(&layer, &near), [20]);
        let copy = layer.clone();
        layer.dataset.features.set_geometry(20, line((900.0, 900.0), (901.0, 901.0)));
        layer.touch_geometry();
        assert!(meeting(&layer, &near).is_empty());
        assert_eq!(meeting(&layer, &Extent::new(900.0, 900.0, 900.5, 900.5)), [20]);
        assert_eq!(meeting(&copy, &near), [20]);
    }
}
