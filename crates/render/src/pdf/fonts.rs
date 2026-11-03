//! The document's fonts: for each face text is set in, Type 3 fonts whose glyphs are the face's
//! outlines, in font units, 256 glyphs to a font (a Type 3 font's codes are single bytes), each
//! with the map from its codes back to the characters they stand for.

use std::collections::HashMap;
use std::fmt::Write as _;

use ab_glyph::{Font, FontRef, GlyphId, OutlineCurve};

use super::Doc;
use super::content::num;

#[derive(Default)]
pub(super) struct Fonts {
    faces: Vec<Face>,
    /// Faces by the address of their `FontRef` (fonts are parsed once and kept).
    by_font: HashMap<usize, usize>,
}

struct Face {
    font: &'static FontRef<'static>,
    upm: f32,
    chunks: Vec<Chunk>,
    /// Each glyph's chunk and code.
    codes: HashMap<u16, (usize, u8)>,
}

struct Chunk {
    /// The font's object number.
    id: usize,
    glyphs: Vec<(GlyphId, char)>,
}

/// Where a glyph is: its font's object number, its code there and its advance in ems.
pub(super) struct Placed {
    pub font: usize,
    pub code: u8,
    pub advance: f32,
}

impl Fonts {
    /// `id` of `font` (standing for `ch`), given a code in one of its Type 3 fonts the first time;
    /// `None` only past what a font can hold.
    pub fn place(&mut self, doc: &mut Doc, font: &'static FontRef<'static>, id: GlyphId, ch: char) -> Option<Placed> {
        let key = std::ptr::from_ref(font) as usize;
        let fi = match self.by_font.get(&key) {
            Some(&i) => i,
            None => {
                // A face with no usable units per em is read as the usual 1000, as the pixels read
                // it at one unit a pixel's em: its text is kept, not left out.
                let upm = font.units_per_em().filter(|u| u.is_finite() && *u > 0.0).unwrap_or(1000.0);
                self.faces.push(Face { font, upm, chunks: Vec::new(), codes: HashMap::new() });
                self.by_font.insert(key, self.faces.len() - 1);
                self.faces.len() - 1
            }
        };
        let face = self.faces.get_mut(fi)?;
        let advance = font.h_advance_unscaled(id) / face.upm;
        let advance = if advance.is_finite() { advance } else { 0.0 };
        let (ci, code) = match face.codes.get(&id.0) {
            Some(&c) => c,
            None => {
                if face.chunks.last().is_none_or(|c| c.glyphs.len() >= 256) {
                    face.chunks.push(Chunk { id: doc.reserve(), glyphs: Vec::new() });
                }
                let ci = face.chunks.len() - 1;
                let chunk = face.chunks.get_mut(ci)?;
                let code = u8::try_from(chunk.glyphs.len()).ok()?;
                chunk.glyphs.push((id, ch));
                face.codes.insert(id.0, (ci, code));
                (ci, code)
            }
        };
        Some(Placed { font: face.chunks.get(ci)?.id, code, advance })
    }

    /// Writes every font given a glyph.
    pub fn write(self, doc: &mut Doc) {
        for face in &self.faces {
            for chunk in &face.chunks {
                write_chunk(doc, face, chunk);
            }
        }
    }
}

/// One Type 3 font: its glyph procedures, widths, encoding and map back to Unicode.
fn write_chunk(doc: &mut Doc, face: &Face, chunk: &Chunk) {
    let mut procs = String::new();
    let mut widths = String::new();
    let mut names = String::new();
    let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (code, (id, _)) in chunk.glyphs.iter().enumerate() {
        let advance = face.font.h_advance_unscaled(*id);
        let advance = if advance.is_finite() { advance } else { 0.0 };
        let (ops, b) = glyph_procedure(face.font, *id, advance);
        if let Some(b) = b {
            bbox = [bbox[0].min(b[0]), bbox[1].min(b[1]), bbox[2].max(b[2]), bbox[3].max(b[3])];
        }
        let proc_id = doc.reserve();
        doc.stream(proc_id, "", &ops, false);
        let _ = write!(procs, "/g{code} {proc_id} 0 R ");
        let _ = write!(names, "/g{code}");
        let mut w = Vec::new();
        num(&mut w, advance, 2);
        widths.push_str(&String::from_utf8_lossy(&w));
        widths.push(' ');
    }
    if bbox[0] > bbox[2] {
        bbox = [0.0; 4];
    }
    let to_unicode = doc.reserve();
    doc.stream(to_unicode, "", to_unicode_cmap(&chunk.glyphs).as_bytes(), false);
    let mut matrix = Vec::new();
    for v in [1.0 / face.upm, 0.0, 0.0, 1.0 / face.upm, 0.0, 0.0] {
        num(&mut matrix, v, 9);
        matrix.push(b' ');
    }
    let mut b = Vec::new();
    for v in bbox {
        num(&mut b, v, 0);
        b.push(b' ');
    }
    doc.put(
        chunk.id,
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [{}] /FontMatrix [{}] /CharProcs << {procs}>> /Encoding << /Type /Encoding /Differences [0 {names}] >> /FirstChar 0 /LastChar {} /Widths [{widths}] /Resources << >> /ToUnicode {to_unicode} 0 R >>",
            String::from_utf8_lossy(&b).trim_end(),
            String::from_utf8_lossy(&matrix).trim_end(),
            chunk.glyphs.len().saturating_sub(1),
        )
        .as_bytes(),
    );
}

/// A glyph's procedure: its advance and box (`d1`, so it takes the text's colour) and its
/// outline, filled; and its box in font units, `None` for a glyph with no outline (a space).
fn glyph_procedure(font: &FontRef<'static>, id: GlyphId, advance: f32) -> (Vec<u8>, Option<[f32; 4]>) {
    let mut path = Vec::new();
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut grow = |p: ab_glyph::Point| {
        if p.x.is_finite() && p.y.is_finite() {
            b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
        }
    };
    let pt = |out: &mut Vec<u8>, p: ab_glyph::Point| {
        num(out, p.x, 1);
        out.push(b' ');
        num(out, p.y, 1);
        out.push(b' ');
    };
    if let Some(o) = font.outline(id) {
        let mut last: Option<ab_glyph::Point> = None;
        // Every curve: the outline is already read, and a glyph cut short would draw wrong.
        for c in &o.curves {
            let (start, end) = match c {
                OutlineCurve::Line(a, b) | OutlineCurve::Quad(a, _, b) | OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
            };
            if last != Some(start) {
                if last.is_some() {
                    path.extend_from_slice(b"h\n");
                }
                grow(start);
                pt(&mut path, start);
                path.extend_from_slice(b"m\n");
            }
            match c {
                OutlineCurve::Line(_, e) => {
                    grow(*e);
                    pt(&mut path, *e);
                    path.extend_from_slice(b"l\n");
                }
                OutlineCurve::Quad(s, k, e) => {
                    // A quadratic as the cubic with the same curve.
                    let lerp = |a: ab_glyph::Point, t: f32| ab_glyph::point(a.x + (k.x - a.x) * t, a.y + (k.y - a.y) * t);
                    for p in [lerp(*s, 2.0 / 3.0), lerp(*e, 2.0 / 3.0), *e] {
                        grow(p);
                        pt(&mut path, p);
                    }
                    path.extend_from_slice(b"c\n");
                }
                OutlineCurve::Cubic(_, k1, k2, e) => {
                    for p in [*k1, *k2, *e] {
                        grow(p);
                        pt(&mut path, p);
                    }
                    path.extend_from_slice(b"c\n");
                }
            }
            last = Some(end);
        }
        if last.is_some() {
            path.extend_from_slice(b"h\nf\n");
        }
    }
    let bbox = (b[0] <= b[2]).then_some(b);
    let mut ops = Vec::with_capacity(path.len() + 48);
    num(&mut ops, advance, 2);
    ops.extend_from_slice(b" 0 ");
    let [x0, y0, x1, y1] = bbox.unwrap_or([0.0; 4]);
    for v in [x0.floor(), y0.floor(), x1.ceil(), y1.ceil()] {
        num(&mut ops, v, 0);
        ops.push(b' ');
    }
    ops.extend_from_slice(b"d1\n");
    ops.extend_from_slice(&path);
    (ops, bbox)
}

/// The CMap taking each code of a font to the character its glyph stands for.
fn to_unicode_cmap(glyphs: &[(GlyphId, char)]) -> String {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    // At most 100 entries to a block.
    for (b, block) in glyphs.chunks(100).enumerate() {
        let _ = writeln!(s, "{} beginbfchar", block.len());
        for (i, (_, ch)) in block.iter().enumerate() {
            let hex: String = ch.encode_utf16(&mut [0; 2]).iter().map(|u| format!("{u:04X}")).collect();
            let _ = writeln!(s, "<{:02X}> <{hex}>", b * 100 + i);
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_share_a_font_until_it_holds_256() {
        let font = crate::text::label_font().unwrap();
        let mut doc = Doc::new();
        let mut fonts = Fonts::default();
        let a = fonts.place(&mut doc, font, font.glyph_id('A'), 'A').unwrap();
        let again = fonts.place(&mut doc, font, font.glyph_id('A'), 'A').unwrap();
        assert_eq!((a.font, a.code), (again.font, again.code));
        assert!(a.advance > 0.3 && a.advance < 1.0, "an em fraction: {}", a.advance);
        // Glyph ids past what one font holds go to a second.
        let mut last = a;
        for g in 1..300u16 {
            last = fonts.place(&mut doc, font, GlyphId(g), 'x').unwrap();
        }
        assert_ne!(last.font, again.font);
        // 'A' itself may be one of those ids, placed once.
        let distinct = 299 + usize::from(!(1..300).contains(&font.glyph_id('A').0));
        assert_eq!(fonts.faces[0].chunks.iter().map(|c| c.glyphs.len()).collect::<Vec<_>>(), vec![256, distinct - 256]);
    }

    #[test]
    fn a_glyph_procedure_is_its_outline_in_font_units() {
        let font = crate::text::label_font().unwrap();
        let id = font.glyph_id('H');
        let (ops, bbox) = glyph_procedure(font, id, font.h_advance_unscaled(id));
        let ops = String::from_utf8(ops).unwrap();
        assert!(ops.contains(" d1\n") && ops.contains(" m\n") && ops.contains(" l\n") && ops.trim_end().ends_with("h\nf"), "{ops}");
        let [x0, y0, x1, y1] = bbox.unwrap();
        // An H stands on the baseline, a cap height tall.
        let upm = font.units_per_em().unwrap();
        assert!(y0.abs() < upm * 0.02 && (y1 - y0) > upm * 0.5 && x1 > x0, "{bbox:?}");
        let (space, none) = glyph_procedure(font, font.glyph_id(' '), 200.0);
        assert!(none.is_none());
        assert_eq!(String::from_utf8(space).unwrap(), "200 0 0 0 0 0 d1\n");
    }

    #[test]
    fn the_unicode_map_spans_blocks_and_astral_characters() {
        let glyphs: Vec<(GlyphId, char)> = (0..150).map(|i| (GlyphId(i), if i == 120 { '🙂' } else { 'é' })).collect();
        let cmap = to_unicode_cmap(&glyphs);
        assert_eq!(cmap.matches("beginbfchar").count(), 2);
        assert!(cmap.contains("100 beginbfchar") && cmap.contains("50 beginbfchar"));
        assert!(cmap.contains("<00> <00E9>") && cmap.contains("<78> <D83DDE42>"));
    }
}
