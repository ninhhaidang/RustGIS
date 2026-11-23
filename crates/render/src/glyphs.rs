//! Character markers ([`Marker::Char`](rustgis_map::Marker::Char)): a character of an installed
//! family as a marker's outline, placed as QGIS places a font marker: the em is the marker's size,
//! the character's advance is centred on the point and its baseline is half the font's ascent
//! below it. A family the machine lacks, or one without the character, draws it in the label font.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use ab_glyph::{Font, OutlineCurve};
use rustgis_map::Glyph;
use tiny_skia::{Path, PathBuilder};

/// The most characters kept, and the most curves read from one (a crafted font's glyph).
const MAX_KEPT: usize = 4096;
const MAX_CURVES: usize = 20_000;

/// A character's outline one em across, the point at the origin (y down), and the furthest it
/// reaches from the point, in ems.
#[derive(Debug)]
pub(crate) struct CharShape {
    pub path: Path,
    pub reach: f32,
}

/// `g`'s outline (see the module), read once and kept; `None` for a character with no outline
/// (a space) in its family and in the label font.
pub(crate) fn shape(g: Glyph) -> Option<Arc<CharShape>> {
    type Shapes = HashMap<Glyph, Option<Arc<CharShape>>>;
    static KEPT: OnceLock<Mutex<(u32, Shapes)>> = OnceLock::new();
    let kept = KEPT.get_or_init(Default::default);
    let generation = crate::fonts::font_generation();
    {
        let mut cache = kept.lock().ok()?;
        if cache.0 != generation {
            cache.0 = generation;
            cache.1.clear();
        }
        if let Some(s) = cache.1.get(&g) {
            return s.clone();
        }
    }
    let (s, settled) = read(g);
    // Not kept while the installed families are still being indexed: drawn in the label font for
    // now, in its own family once they are.
    if settled
        && let Ok(mut kept) = kept.lock()
        && kept.0 == generation
        && kept.1.len() < MAX_KEPT
    {
        kept.1.insert(g, s.clone());
    }
    s
}

/// `g`'s outline, and whether it is final (its family's face, or the label font's because the
/// family isn't installed rather than not indexed yet).
fn read(g: Glyph) -> (Option<Arc<CharShape>>, bool) {
    let (named, settled) = crate::text::marker_font(g.font.as_str());
    let from = |f: &ab_glyph::FontRef<'static>| {
        let id = f.glyph_id(g.ch);
        (id.0 != 0).then(|| outline(f, id)).flatten()
    };
    let s = named.and_then(from).or_else(|| crate::text::marker_fallback(g.ch).and_then(|(f, id)| outline(f, id)));
    (s.map(Arc::new), settled)
}

fn outline(f: &ab_glyph::FontRef<'static>, id: ab_glyph::GlyphId) -> Option<CharShape> {
    let upm = f.units_per_em().filter(|u| u.is_finite() && *u > 0.0)?;
    let advance = f.h_advance_unscaled(id);
    let ascent = f.ascent_unscaled();
    let (dx, dy) = (if advance.is_finite() { advance / 2.0 } else { 0.0 }, if ascent.is_finite() { ascent / 2.0 } else { 0.0 });
    // Font units (y up, from the origin of the advance and the baseline) to ems about the point.
    let at = |p: ab_glyph::Point| ((p.x - dx) / upm, (dy - p.y) / upm);
    let o = f.outline(id)?;
    let mut pb = PathBuilder::new();
    let mut last: Option<ab_glyph::Point> = None;
    for c in o.curves.iter().take(MAX_CURVES) {
        let (start, end) = match c {
            OutlineCurve::Line(a, b) | OutlineCurve::Quad(a, _, b) | OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
        };
        if last != Some(start) {
            if last.is_some() {
                pb.close();
            }
            let (x, y) = at(start);
            pb.move_to(x, y);
        }
        match c {
            OutlineCurve::Line(_, b) => {
                let (x, y) = at(*b);
                pb.line_to(x, y);
            }
            OutlineCurve::Quad(_, k, b) => {
                let ((kx, ky), (x, y)) = (at(*k), at(*b));
                pb.quad_to(kx, ky, x, y);
            }
            OutlineCurve::Cubic(_, k1, k2, b) => {
                let ((ax, ay), (bx, by), (x, y)) = (at(*k1), at(*k2), at(*b));
                pb.cubic_to(ax, ay, bx, by, x, y);
            }
        }
        last = Some(end);
    }
    if last.is_some() {
        pb.close();
    }
    let path = pb.finish()?;
    let b = path.bounds();
    let reach = b.left().abs().max(b.right().abs()).max(b.top().abs()).max(b.bottom().abs());
    reach.is_finite().then_some(CharShape { path, reach })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_measurement_does_not_report_output_loss() {
        let _ = crate::approximations::take();
        for c in ['\u{10fffd}', ' ', '\u{200b}'] {
            for _ in 0..2 {
                let _ = crate::marker_reach(rustgis_map::Marker::Char(Glyph::new(c, "")));
            }
        }
        assert!(crate::approximations::take().is_empty());
    }

    #[test]
    fn a_character_is_centred_on_its_advance_with_its_baseline_below_the_point() {
        // The label font's "H": as wide either side of the point, its foot on the baseline half
        // the ascent below it, standing a cap height tall.
        let f = crate::text::label_font().unwrap();
        let s = shape(Glyph::new('H', "")).unwrap();
        let b = s.path.bounds();
        let upm = f.units_per_em().unwrap();
        let id = f.glyph_id('H');
        let ink = f.outline(id).unwrap().bounds;
        let advance = f.h_advance_unscaled(id);
        assert!((b.left() - (ink.min.x - advance / 2.0) / upm).abs() < 1e-3, "{b:?}");
        assert!((b.bottom() - f.ascent_unscaled() / 2.0 / upm).abs() < 0.02, "{b:?}");
        assert!(b.height() > 0.5 && b.height() < 0.8, "a cap height: {b:?}");
        assert!(s.reach >= b.bottom() && s.reach < 1.0);
    }

    #[test]
    fn a_missing_family_or_character_draws_in_the_label_font_and_a_space_draws_nothing() {
        let label = shape(Glyph::new('H', "")).unwrap();
        let missing = shape(Glyph::new('H', "No Such Family Anywhere")).unwrap();
        assert_eq!(label.path.bounds(), missing.path.bounds());
        assert!(shape(Glyph::new(' ', "")).is_none());
        assert!(shape(Glyph::new('\u{10FFFD}', "")).is_none(), "a character no font has");
    }
}
