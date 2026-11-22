//! Point and polygon labels curved along a guide line (#2262): a range's name along its ridge, a
//! valley's along its valley line. Everything here is on the screen, in points, y down.

use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

use crate::line_labels::{self, Line};

/// The most guide lines a label is tried along: the longest, or nearest, first.
const MOST_TRIED: usize = 8;

/// The side of a cell of the grid the guide stretches are found by (points).
const CELL: f32 = 64.0;

/// How near a guide stretch's points are kept to the last one kept (points): finer detail than
/// this doesn't turn a letter, and the stretches in view then hold about as many points as the
/// screen length they run, however dense the layer.
const MIN_STEP: f32 = 0.5;

/// A guide line's stretch in view, and its box ([min x, min y, max x, max y]).
struct Guide {
    line: Line,
    bbox: [f32; 4],
}

/// A guide layer's lines in view (each stretch on its own), and those of each value of its field,
/// by the value's text trimmed and in lower case. A stretch holds about 24 bytes a point it keeps
/// (its path both ways, [`Line`]) and its box, and is listed in each [`CELL`] its box covers.
#[derive(Default)]
pub(crate) struct Guides {
    lines: Vec<Guide>,
    /// The box of all of them: the grid cells looked in are those in it.
    extent: Option<[f32; 4]>,
    by_value: HashMap<String, Vec<usize>>,
    cells: HashMap<(i32, i32), Vec<usize>>,
}

/// The grid cells a box covers (a stretch's, so in view, or one cut to [`Guides`]' extent).
fn cells_of(b: [f32; 4]) -> impl Iterator<Item = (i32, i32)> {
    let at = |v: f32| (v / CELL).floor().clamp(-2.0, 1024.0) as i32;
    (at(b[0])..=at(b[2])).flat_map(move |x| (at(b[1])..=at(b[3])).map(move |y| (x, y)))
}

impl Guides {
    /// Adds a stretch of a guide line with `value` in its field (none: no field, or no value),
    /// its points thinned to [`MIN_STEP`] apart.
    pub(crate) fn push(&mut self, stretch: &[(f32, f32)], value: Option<&str>) {
        let mut kept: Vec<(f32, f32)> = Vec::with_capacity(stretch.len());
        for (k, p) in stretch.iter().enumerate() {
            let far = kept.last().is_none_or(|q| (p.0 - q.0).hypot(p.1 - q.1) >= MIN_STEP);
            if far || k + 1 == stretch.len() {
                kept.push(*p);
            }
        }
        let Some(line) = Line::new(&kept) else { return };
        let bbox =
            kept.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)]);
        let id = self.lines.len();
        if let Some(v) = value {
            self.by_value.entry(v.trim().to_lowercase()).or_default().push(id);
        }
        self.extent = Some(self.extent.map_or(bbox, |e| [e[0].min(bbox[0]), e[1].min(bbox[1]), e[2].max(bbox[2]), e[3].max(bbox[3])]));
        for cell in cells_of(bbox) {
            self.cells.entry(cell).or_default().push(id);
        }
        self.lines.push(Guide { line, bbox });
    }

    /// The stretches whose boxes meet box `b`, each once: looked for in the cells of `b` cut to
    /// their extent (a label's box may run far out of view, a polygon's zoomed into).
    fn meeting(&self, b: [f32; 4]) -> Vec<usize> {
        let Some(e) = self.extent else { return Vec::new() };
        let cut = [b[0].max(e[0]), b[1].max(e[1]), b[2].min(e[2]), b[3].min(e[3])];
        if cut[0] > cut[2] || cut[1] > cut[3] {
            return Vec::new();
        }
        let mut ids: Vec<usize> = cells_of(cut).filter_map(|c| self.cells.get(&c)).flatten().copied().collect();
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|i| {
            self.lines.get(*i).is_some_and(|g| g.bbox[0] <= b[2] && b[0] <= g.bbox[2] && g.bbox[1] <= b[3] && b[1] <= g.bbox[3])
        });
        ids
    }
}

/// What the label's feature is, for the guide lines that don't match it by value.
pub(crate) enum Near<'a> {
    /// A polygon: its box on the screen where it is had cheaply (from its projected extent; none
    /// on the globe), and its outline there ([`crate::area_labels::screen_rings`]), worked out
    /// only when that box meets a guide stretch: the text goes on a guide line where it is inside it.
    Polygon(Option<[f32; 4]>, Box<dyn FnOnce() -> Option<Vec<Vec<(f32, f32)>>> + 'a>),
    /// A point: the text goes on the nearest guide line within the text's width, as near it as fits.
    Point((f32, f32)),
}

/// One line of `text`, each character `measure` wide and `gap` apart, curved along a guide line:
/// the label's centre and its characters. A polygon's tries the lines whose field holds `value`
/// (the feature's, as text, ignoring case) first, longest first, wherever they run; with none
/// such that fits, those inside it. A point's takes the nearest line within the text's width, one holding
/// its value before the others there: a line of its value farther off isn't its own.
/// Along each line its middle is tried, then places either side ([`line_labels::positions`]).
/// `None` when the text follows none of them: the label is placed as without a guide.
///
/// Per frame, for each label with a guide and its label point in view; it looks at no Cancel: it
/// reads the stretches listed in the grid cells round its point or polygon, and tries at most
/// [`MOST_TRIED`] lines, a polygon's inside tested against its outline thinned as for its centre
/// line (#2295): at most 3 points a position tried, each against every edge of the outline.
pub(super) fn glyphs(
    guides: &Guides,
    value: Option<&str>,
    near: Near<'_>,
    text: &str,
    measure: impl Fn(&str) -> f32,
    gap: f32,
    size: f32,
) -> Option<((f32, f32), Vec<super::LabelGlyph>)> {
    let chars: Vec<String> = text.graphemes(true).map(String::from).collect();
    let advances: Vec<f32> = chars.iter().map(|c| measure(c)).collect();
    let (centre, turns) = curved(guides, value, near, &advances, gap, size)?;
    let glyphs = chars.into_iter().zip(turns).map(|(text, (dx, dy, angle))| super::LabelGlyph { text, dx, dy, angle }).collect();
    Some((centre, glyphs))
}

type Curved = ((f32, f32), Vec<(f32, f32, f32)>);

/// [`glyphs`]' characters `advances` wide, as [`Line::curved`] takes them.
fn curved(guides: &Guides, value: Option<&str>, near: Near<'_>, advances: &[f32], gap: f32, size: f32) -> Option<Curved> {
    let len = line_labels::curved_len(advances, gap);
    let line = |i: &usize| guides.lines.get(*i).map(|g| &g.line);
    let longest_first = |mut ids: Vec<usize>| {
        ids.sort_by(|a, b| line(b).map_or(0.0, Line::length).total_cmp(&line(a).map_or(0.0, Line::length)));
        ids.truncate(MOST_TRIED);
        ids
    };
    let along = |line: &Line, fits: &dyn Fn(&Curved) -> bool| {
        line_labels::positions(line.length(), len, 0.0)
            .into_iter()
            .filter_map(|at| line.curved(advances, gap, size, 0.0, Some(at)))
            .find(|c| fits(c))
    };
    let matched = value.and_then(|v| guides.by_value.get(&v.trim().to_lowercase()));
    match near {
        Near::Point(p) => {
            // Nearest first, each within the text's width, those of its value before the others:
            // centred at its nearest place, or as near it as the text fits on the line.
            let mut nearest: Vec<(bool, f32, usize, f32)> = guides
                .meeting([p.0 - len, p.1 - len, p.0 + len, p.1 + len])
                .into_iter()
                .filter_map(|i| {
                    let (d2, at) = line(&i)?.nearest(p, 0.0).filter(|(d2, _)| *d2 <= len * len)?;
                    Some((!matched.is_some_and(|m| m.contains(&i)), d2, i, at))
                })
                .collect();
            nearest.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
            nearest.into_iter().take(MOST_TRIED).find_map(|(_, _, i, at)| {
                let l = line(&i).filter(|l| l.length() >= len)?;
                l.curved(advances, gap, size, 0.0, Some(at.clamp(len / 2.0, l.length() - len / 2.0)))
            })
        }
        Near::Polygon(within, rings) => {
            if let Some(ids) = matched
                && let Some(c) = longest_first(ids.clone()).iter().filter_map(line).find_map(|l| along(l, &|_| true))
            {
                return Some(c);
            }
            if within.is_some_and(|b| guides.meeting(b).is_empty()) {
                return None;
            }
            let rings = rings()?;
            let b = rings
                .iter()
                .flatten()
                .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)]);
            // Its middle and both ends inside the polygon.
            let inside = |(c, turns): &Curved| {
                [turns.first(), turns.last()].into_iter().flatten().map(|t| (c.0 + t.0, c.1 + t.1)).chain([*c]).all(|p| inside(&rings, p))
            };
            longest_first(guides.meeting(b)).iter().filter_map(line).find_map(|l| along(l, &inside))
        }
    }
}

/// Whether `p` is inside the outlines `rings` (holes among them, even–odd).
fn inside(rings: &[Vec<(f32, f32)>], p: (f32, f32)) -> bool {
    let mut odd = false;
    for r in rings {
        for (k, a) in r.iter().enumerate() {
            let Some(b) = r.get(k + 1).or_else(|| r.first()) else { continue };
            if (a.1 <= p.1) != (b.1 <= p.1) && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
                odd = !odd;
            }
        }
    }
    odd
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quarter of a circle of radius 300 about (0, 400), from (0, 100) to (300, 400).
    fn arc() -> Vec<(f32, f32)> {
        (0..=60).map(|i| i as f32 / 60.0 * std::f32::consts::FRAC_PI_2).map(|t| (300.0 * t.sin(), 400.0 - 300.0 * t.cos())).collect()
    }

    fn advances() -> Vec<f32> {
        vec![8.0; 12]
    }

    #[test]
    fn a_label_takes_its_own_guide_line_else_the_one_inside_its_polygon() {
        let mut guides = Guides::default();
        let straight: Vec<(f32, f32)> = (0..=10).map(|i| (500.0 + 40.0 * i as f32, 600.0)).collect();
        guides.push(&straight, Some("Serra Reta"));
        guides.push(&arc(), Some("Serra Curva"));
        // A polygon's by value, ignoring case and spaces round it, wherever its line runs: along the
        // arc, every character on it (its outline isn't needed).
        let unread = || Near::Polygon(None, Box::new(|| None));
        let (_, turns) = curved(&guides, Some(" serra curva"), unread(), &advances(), 1.0, 10.0).expect("on its line");
        let (c, _) = curved(&guides, Some("SERRA CURVA"), unread(), &advances(), 1.0, 10.0).expect("on its line");
        assert!(turns.iter().all(|t| ((c.0 + t.0).hypot(c.1 + t.1 - 400.0) - 300.0).abs() < 6.0), "{c:?} {turns:?}");
        // No value: the line inside the polygon, here a box round the straight line.
        let rings = vec![vec![(480.0, 560.0), (920.0, 560.0), (920.0, 640.0), (480.0, 640.0)]];
        let (c, _) = curved(&guides, None, Near::Polygon(None, Box::new(|| Some(rings))), &advances(), 1.0, 10.0).expect("inside");
        assert!((c.1 - 600.0).abs() < 1.0 && (600.0..800.0).contains(&c.0), "{c:?}");
        // A polygon that neither line runs inside: none.
        let away = vec![vec![(0.0, 900.0), (100.0, 900.0), (100.0, 990.0), (0.0, 990.0)]];
        assert!(curved(&guides, None, Near::Polygon(None, Box::new(|| Some(away))), &advances(), 1.0, 10.0).is_none());
        // A polygon whose box meets no guide line: its outline isn't worked out.
        let asked = std::cell::Cell::new(false);
        let far = Near::Polygon(
            Some([5000.0, 5000.0, 5100.0, 5100.0]),
            Box::new(|| {
                asked.set(true);
                None
            }),
        );
        assert!(curved(&guides, None, far, &advances(), 1.0, 10.0).is_none());
        assert!(!asked.get(), "its outline isn't worked out");
    }

    /// A polygon's value's line too short for its text falls to the line inside it, not to none.
    #[test]
    fn a_polygon_whose_named_line_is_too_short_takes_one_inside_it() {
        let mut guides = Guides::default();
        guides.push(&[(500.0, 600.0), (540.0, 600.0)], Some("Serra Grande"));
        let straight: Vec<(f32, f32)> = (0..=10).map(|i| (480.0 + 44.0 * i as f32, 600.0)).collect();
        guides.push(&straight, None);
        let rings = vec![vec![(480.0, 560.0), (920.0, 560.0), (920.0, 640.0), (480.0, 640.0)]];
        let named = Near::Polygon(None, Box::new(|| Some(rings)));
        let (c, turns) =
            curved(&guides, Some("serra grande"), named, &advances(), 1.0, 10.0).expect("inside, its named line being too short");
        assert!((c.1 - 600.0).abs() < 1.0 && !turns.is_empty(), "{c:?}");
    }

    /// A dense stretch keeps a point every half a point at least, and its ends; the grid finds a
    /// stretch by any cell its box covers, not those away from it.
    #[test]
    fn stretches_are_thinned_and_found_by_their_cells() {
        let mut guides = Guides::default();
        let dense: Vec<(f32, f32)> = (0..=10_000).map(|i| (i as f32 * 0.03, 10.0)).collect();
        guides.push(&dense, None);
        let line = &guides.lines[0].line;
        assert!((line.length() - 300.0).abs() < 0.01, "{}", line.length());
        assert_eq!(guides.meeting([290.0, 0.0, 400.0, 20.0]), vec![0]);
        assert!(guides.meeting([0.0, 100.0, 300.0, 200.0]).is_empty());
        assert!(guides.cells.values().map(Vec::len).sum::<usize>() <= 6);
    }

    #[test]
    fn a_point_takes_the_nearest_line_within_its_texts_width() {
        let mut guides = Guides::default();
        guides.push(&arc(), None);
        let text = line_labels::curved_len(&advances(), 1.0);
        // 60 points off the arc's middle, nearer than the text is long: on the arc, near the point.
        let p = (300.0f32 * 0.707 + 42.0, 400.0 - 300.0 * 0.707 - 42.0);
        let (c, _) = curved(&guides, None, Near::Point(p), &advances(), 1.0, 10.0).expect("nearest");
        assert!((c.0.hypot(c.1 - 400.0) - 300.0).abs() < 6.0 && (c.0 - p.0).hypot(c.1 - p.1) < text, "{c:?}");
        // Farther than the text's width: none.
        assert!(curved(&guides, None, Near::Point((p.0 + text, p.1 - text)), &advances(), 1.0, 10.0).is_none());
        // A line shorter than the text: none.
        let mut short = Guides::default();
        short.push(&[(0.0, 0.0), (20.0, 0.0)], None);
        assert!(curved(&short, None, Near::Point((10.0, 5.0)), &advances(), 1.0, 10.0).is_none());
        // A line of its value far off isn't its own: the nearest within the text's width is, and
        // one of its value there before a nearer one.
        let mut named = Guides::default();
        named.push(&arc(), None);
        let far: Vec<(f32, f32)> = (0..=10).map(|i| (2000.0 + 40.0 * i as f32, 2000.0)).collect();
        named.push(&far, Some("Pico"));
        let (c, _) = curved(&named, Some("Pico"), Near::Point(p), &advances(), 1.0, 10.0).expect("the arc beside it");
        assert!((c.0 - p.0).hypot(c.1 - p.1) < text, "{c:?}");
        let beside: Vec<(f32, f32)> = (0..=10).map(|i| (p.0 - 200.0 + 40.0 * i as f32, p.1 + 90.0)).collect();
        named.push(&beside, Some("pico"));
        let (c, _) = curved(&named, Some("Pico"), Near::Point(p), &advances(), 1.0, 10.0).expect("its own beside it");
        assert!((c.1 - (p.1 + 90.0)).abs() < 1.0, "{c:?}");
    }

    /// Drawn: a range's name follows the ridge line with its name, a peak's the ridge beside it,
    /// every character on the ridge; a range with no ridge keeps its level label.
    #[test]
    fn a_ranges_name_follows_its_ridge() {
        use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
        use rustgis_geom::{Coord, lonlat_to_merc};
        use rustgis_map::{Camera, LabelGuide, Map};
        // A ridge bending round (0, -3), 3.5° out, from 40° west of north to 40° east.
        let (centre, r) = (Coord::new(0.0, -3.0), 3.5);
        let ridge: Vec<Coord> = (0..=40)
            .map(|i| (f64::from(i) / 40.0 * 80.0 - 40.0).to_radians())
            .map(|t| Coord::new(r * t.sin(), centre.y + r * t.cos()))
            .collect();
        let square = |x: f64, y: f64, s: f64| {
            vec![vec![vec![Coord::new(x, y), Coord::new(x + s, y), Coord::new(x + s, y + s), Coord::new(x, y + s), Coord::new(x, y)]]]
        };
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("Cumeadas", GeometryType::Polyline, vec![Field::new("nome", FieldType::Text)]);
        d.push(Some(Geometry::Line(vec![ridge])), vec![Value::Text("Serra Grande".into())]);
        m.add_dataset(d);
        let mut d = Dataset::new("Serras", GeometryType::Polygon, vec![Field::new("nome", FieldType::Text)]);
        d.push(Some(Geometry::Polygon(square(-2.0, -1.0, 2.0))), vec![Value::Text("Serra Grande".into())]);
        d.push(Some(Geometry::Polygon(square(-2.5, -2.5, 0.6))), vec![Value::Text("Serra Baixa".into())]);
        let serras = m.add_dataset(d);
        let mut d = Dataset::new("Picos", GeometryType::Point, vec![Field::new("nome", FieldType::Text)]);
        // Near the ridge's east end, far from the range's name at its middle.
        d.push(Some(Geometry::Point(Coord::new(2.0, -0.05))), vec![Value::Text("Pico".into())]);
        let picos = m.add_dataset(d);
        for (id, field) in [(serras, Some("nome")), (picos, None)] {
            let l = m.layer_mut(id).unwrap();
            l.labels.enabled = true;
            l.labels.default.field = "nome".into();
            l.labels.default.size = 12.0;
            // Every label drawn, whatever the platform's font makes them crowd.
            l.labels.default.show_all = true;
            l.labels.default.guide = Some(LabelGuide { layer: "cumeadas".into(), field: field.map(String::from) });
        }
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, -0.5)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = crate::View { width: 400.0, height: 400.0, pixels_per_point: 1.0, camera, sky: false };
        let labels = crate::render(&mut m, &view).unwrap().labels;
        let (cx, cy) = view.to_screen(lonlat_to_merc(centre));
        let per_degree =
            (view.to_screen(lonlat_to_merc(Coord::new(1.0, 0.0))).0 - view.to_screen(lonlat_to_merc(Coord::new(0.0, 0.0))).0) as f32;
        for name in ["Serra Grande", "Pico"] {
            let l = labels.iter().find(|l| l.text == name).unwrap_or_else(|| panic!("{name} not placed"));
            assert_eq!(l.glyphs.iter().map(|g| g.text.as_str()).collect::<String>(), name);
            for g in &l.glyphs {
                let off = ((l.x + g.dx) - cx as f32).hypot((l.y + g.dy) - cy as f32) / per_degree - r as f32;
                assert!(off.abs() < 0.1, "{name}: {} off the ridge by {off}°", g.text);
            }
        }
        let low = labels.iter().find(|l| l.text == "Serra Baixa").expect("placed");
        assert!(low.glyphs.is_empty(), "no ridge: level");
        // Hidden and held in another system (UTM zone 31 south), so read point by point, the
        // lines out of view passed over: the same.
        let ridges = m.layers.iter_mut().find(|l| l.name == "Cumeadas").unwrap();
        ridges.visible = false;
        ridges.dataset.hold_in(rustgis_data::raster::Crs::Epsg(32731)).unwrap();
        ridges.touch_geometry();
        let again = crate::render(&mut m, &view).unwrap().labels;
        let serra = again.iter().find(|l| l.text == "Serra Grande").expect("placed");
        assert_eq!(serra.glyphs.len(), "Serra Grande".chars().count(), "hidden and held, still along its ridge");
        // Read in place from its file (a project written by hand): passed over, and no copy of it
        // made for the guide.
        let ridges = m.layers.iter_mut().find(|l| l.name == "Cumeadas").unwrap();
        ridges.source = Some(Default::default());
        ridges.clear_cache();
        let streamed = crate::render(&mut m, &view).unwrap().labels;
        let ridges = m.layers.iter().find(|l| l.name == "Cumeadas").unwrap();
        assert!(ridges.projected_cache().is_empty() && ridges.display_cache().is_none(), "no copy made");
        let serra = streamed.iter().find(|l| l.text == "Serra Grande").expect("placed");
        assert!(serra.glyphs.is_empty(), "placed as without its guide");
    }
}
