//! A face embedded in a PDF as itself (#1815): a subset of its file holding the glyphs the
//! document sets, written as a composite font (Type 0 over a CID font, Identity-H), so that a
//! vector editor reads its text as text. A face is embedded only when its file draws exactly what
//! the export draws (the face at its file's default instance) and its licence allows it; anything
//! else is set in Type 3 outlines, with the reason.

use ab_glyph::{Font, FontRef, GlyphId, OutlineCurve};

/// The largest subset of one face embedded, and of all of a document's faces together: past
/// them a face's text is set in outlines.
const MAX_SUBSET: usize = 8 << 20;
pub(super) const MAX_EMBEDDED: usize = 32 << 20;
/// The most faces of a collection (`.ttc`) looked through for the one drawn.
const MAX_COLLECTION: u32 = 64;

/// A face's subset, ready to be written.
pub(super) struct Embedded {
    /// The subset font file, and whether its outlines are CFF (else TrueType).
    pub file: Vec<u8>,
    pub cff: bool,
    /// Its name, with the subset's tag (`ABCDEF+Name`).
    pub name: String,
    /// The subset's glyph (the CID the content sets) of each glyph of the face.
    pub remapper: subsetter::GlyphRemapper,
    /// Its descriptor's metrics, in thousandths of the em: box, ascent, descent, cap height,
    /// italic angle in degrees.
    pub bbox: [f32; 4],
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    pub italic_angle: f32,
}

/// Why a face's text is set in outlines rather than in the face itself.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Refused {
    /// Its licence forbids embedding (`fsType` 2), subsetting (0x100), or allows bitmaps only (0x200).
    Licence(&'static str),
    /// It is drawn at a variable font's instance other than its file's default.
    Instance,
    /// Its file doesn't hold the face as it is drawn.
    NotInFile,
    /// Its file can't be read or subset.
    Unreadable(String),
    /// Its subset would pass [`MAX_SUBSET`].
    SubsetTooLarge,
    /// With it, the document's embedded faces would pass [`MAX_EMBEDDED`].
    DocumentTooLarge,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Licence(what) => write!(f, "its licence {what}"),
            Refused::Instance => f.write_str("it is drawn at another weight or width than its file's default"),
            Refused::NotInFile => f.write_str("its file doesn't hold it as drawn"),
            Refused::Unreadable(e) => write!(f, "its file could not be subset: {e}"),
            Refused::SubsetTooLarge => write!(f, "its subset passes {} MB", MAX_SUBSET >> 20),
            Refused::DocumentTooLarge => write!(f, "the document's embedded faces would pass {} MB", MAX_EMBEDDED >> 20),
        }
    }
}

/// A name for the face in a notice: its full name, else its family, else its PostScript name.
/// In a collection, the face that draws `glyphs` as `font` does ([`drawn_as`]); where none
/// does (another instance of a variable font), the first with as many glyphs and units per em.
pub(super) fn face_name(font: &FontRef<'_>, glyphs: &[GlyphId]) -> String {
    let data = font.font_data();
    let index = drawn_as(font, glyphs)
        .or_else(|| (0..collection_len(data)).find(|&i| ttf_parser::Face::parse(data, i).is_ok_and(|f| same_face(font, &f))));
    let face = index.and_then(|i| ttf_parser::Face::parse(data, i).ok());
    face.and_then(|f| {
        [ttf_parser::name_id::FULL_NAME, ttf_parser::name_id::FAMILY, ttf_parser::name_id::POST_SCRIPT_NAME]
            .into_iter()
            .find_map(|id| f.names().into_iter().filter(|n| n.name_id == id).find_map(|n| n.to_string()))
    })
    .map(|n| n.chars().filter(|c| !c.is_control()).take(80).collect())
    .unwrap_or_else(|| "an unnamed face".into())
}

/// How many faces `data` holds: one for a single font file.
fn collection_len(data: &[u8]) -> u32 {
    ttf_parser::fonts_in_collection(data).unwrap_or(1).clamp(1, MAX_COLLECTION)
}

/// Whether `face` parsed from the file is the one `font` was read as: the same number of glyphs
/// and units per em (a quick check before comparing outlines).
fn same_face(font: &FontRef<'_>, face: &ttf_parser::Face) -> bool {
    usize::from(face.number_of_glyphs()) == font.glyph_count() && font.units_per_em() == Some(f32::from(face.units_per_em()))
}

/// The face of `font`'s file that draws `glyphs` as `font` does, at the file's own default
/// instance: its index (a collection's faces can share glyphs; any that draws them alike will do).
fn drawn_as(font: &FontRef<'_>, glyphs: &[GlyphId]) -> Option<u32> {
    let data = font.font_data();
    (0..collection_len(data)).find(|&i| {
        FontRef::try_from_slice_and_index(data, i).is_ok_and(|fresh| {
            fresh.glyph_count() == font.glyph_count()
                && fresh.units_per_em() == font.units_per_em()
                && glyphs.iter().all(|&g| draws_alike(font, &fresh, g))
        })
    })
}

/// The face `font` as a subset holding `glyphs`, or why it can't be. A subsetter that fails
/// leaves the face to outlines; natively, one that panics on a hostile file does too (its panic
/// caught). In the browser a panic can't be caught (wasm32 aborts), but no file of the user's
/// reaches it there: fonts are read from disk (the interface's, installed families, CJK and script
/// faces), so the web has only the label face bundled with the app. `room` is what the document
/// can still embed.
pub(super) fn embed(font: &FontRef<'_>, glyphs: &[GlyphId], room: usize) -> Result<Embedded, Refused> {
    let data = font.font_data();
    let Some(index) = drawn_as(font, glyphs) else {
        return Err(if ab_glyph::VariableFont::variations(font).is_empty() { Refused::NotInFile } else { Refused::Instance });
    };
    let face = ttf_parser::Face::parse(data, index).map_err(|e| Refused::Unreadable(e.to_string()))?;
    if let Some(os2) = face.tables().os2 {
        if os2.permissions() == Some(ttf_parser::Permissions::Restricted) {
            return Err(Refused::Licence("forbids embedding"));
        }
        if !os2.is_subsetting_allowed() {
            return Err(Refused::Licence("forbids subsetting"));
        }
        if !os2.is_outline_embedding_allowed() {
            return Err(Refused::Licence("allows embedding bitmaps only"));
        }
    }
    let mut remapper = subsetter::GlyphRemapper::new();
    for g in glyphs {
        remapper.remap(g.0);
    }
    let subset = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| subsetter::subset(data, index, &remapper)));
    let file = match subset {
        Ok(Ok(file)) => file,
        Ok(Err(e)) => return Err(Refused::Unreadable(e.to_string())),
        Err(_) => return Err(Refused::Unreadable("the subsetter failed".into())),
    };
    if file.len() > MAX_SUBSET {
        return Err(Refused::SubsetTooLarge);
    }
    if file.len() > room {
        return Err(Refused::DocumentTooLarge);
    }
    let k = 1000.0 / f32::from(face.units_per_em().max(1));
    let b = face.global_bounding_box();
    let name = face
        .names()
        .into_iter()
        .filter(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
        .find_map(|n| n.to_string())
        .unwrap_or_default();
    let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).take(56).collect();
    let name = if name.is_empty() { "Face".to_string() } else { name };
    Ok(Embedded {
        cff: face.tables().cff.is_some(),
        name: format!("{}+{name}", tag(&name, glyphs, data)),
        remapper,
        bbox: [f32::from(b.x_min) * k, f32::from(b.y_min) * k, f32::from(b.x_max) * k, f32::from(b.y_max) * k],
        ascent: f32::from(face.ascender()) * k,
        descent: f32::from(face.descender()) * k,
        cap_height: f32::from(face.capital_height().unwrap_or(face.ascender())) * k,
        italic_angle: face.italic_angle(),
        file,
    })
}

/// Whether glyph `g` has the same advance and outline in both faces.
fn draws_alike(a: &impl Font, b: &impl Font, g: GlyphId) -> bool {
    if a.h_advance_unscaled(g).to_bits() != b.h_advance_unscaled(g).to_bits() {
        return false;
    }
    match (a.outline(g), b.outline(g)) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            x.curves.len() == y.curves.len() && x.curves.iter().zip(&y.curves).all(|(p, q)| curve_points(p) == curve_points(q))
        }
        _ => false,
    }
}

/// A curve's kind and points, to compare.
fn curve_points(c: &OutlineCurve) -> (u8, [(f32, f32); 4]) {
    let p = |q: &ab_glyph::Point| (q.x, q.y);
    match c {
        OutlineCurve::Line(a, b) => (0, [p(a), p(b), (0.0, 0.0), (0.0, 0.0)]),
        OutlineCurve::Quad(a, b, c) => (1, [p(a), p(b), p(c), (0.0, 0.0)]),
        OutlineCurve::Cubic(a, b, c, d) => (2, [p(a), p(b), p(c), p(d)]),
    }
}

/// A subset's six-letter tag, from its face's name, glyphs and file (its size and bytes): the same
/// subset, the same tag, and two files with the same name and glyphs, two tags.
fn tag(name: &str, glyphs: &[GlyphId], file: &[u8]) -> String {
    // FNV-1a, a byte at a time: a multiplication carries a byte's bits only upwards, so a wider
    // value mixed at once would leave its high bytes out of the low bits the letters are read from.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let glyph_bytes = glyphs.iter().flat_map(|g| g.0.to_le_bytes());
    let len_bytes = (file.len() as u64).to_le_bytes();
    for b in name.bytes().chain(glyph_bytes).chain(len_bytes).chain(file.iter().copied()) {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    (0..6).map(|i| char::from(b'A' + ((h >> (i * 5)) % 26) as u8)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::tests::objects;
    use crate::pdf::{Options, Page, Text, write, write_with};
    use crate::vector::{Drawing, Glyph, GlyphRun};
    use tiny_skia::{Color, Transform};

    /// A face of the test's own (egui's bundled Ubuntu Light, which allows embedding), its bytes
    /// changed by `patch` first, parsed and kept.
    fn face(patch: impl FnOnce(&mut Vec<u8>)) -> &'static FontRef<'static> {
        let mut bytes = epaint_default_fonts::UBUNTU_LIGHT.to_vec();
        patch(&mut bytes);
        let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        Box::leak(Box::new(FontRef::try_from_slice(bytes).unwrap()))
    }

    /// Where the table `tag` starts in a single font file.
    fn table(bytes: &[u8], tag: &[u8; 4]) -> usize {
        let n = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
        let rec = (0..n).map(|i| 12 + 16 * i).find(|&r| &bytes[r..r + 4] == tag).unwrap();
        u32::from_be_bytes(bytes[rec + 8..rec + 12].try_into().unwrap()) as usize
    }

    /// A page with `text` set in `font` twice: on the page (a layout text) and in a group (a
    /// map's label), the second slanted as a synthetic italic is.
    fn page(font: &'static FontRef<'static>, text: &str) -> Page {
        let run = |kx: f32| GlyphRun {
            font,
            size: 20.0,
            color: Color::BLACK,
            transform: Transform::from_row(1.0, 0.0, kx, 1.0, 10.0, 40.0),
            glyphs: text.chars().zip(0..).map(|(ch, i)| Glyph { id: font.glyph_id(ch), x: 11.0 * i as f32, ch }).collect(),
            halo: None,
            decorative: false,
        };
        let mut d = Drawing::new(200.0, 100.0);
        d.glyphs(run(0.0));
        let mut label = Drawing::new(200.0, 100.0);
        label.glyphs(run(-0.2));
        d.place(label, Transform::from_translate(0.0, 40.0), 1.0, rustgis_map::effects::Blend::Normal, None);
        Page::drawing(d, 100.0, 50.0)
    }

    /// Text in fonts is its face embedded as a subset (#1815), a Type 0 font over a TrueType CID
    /// font, the label and the slanted copy set in it too, and its text still read back.
    #[test]
    fn text_in_fonts_is_its_face_embedded() {
        let font = face(|_| ());
        let (pdf, outlined) = write_with(&[page(font, "Ação")], "Fonts", Options { text: Text::Fonts, ..Options::default() });
        assert!(outlined.is_empty(), "{outlined:?}");
        let objs = objects(&pdf);
        let dicts: Vec<&str> = objs.iter().map(|o| o.0.as_str()).collect();
        assert!(dicts.iter().any(|d| d.contains("/Subtype /Type0") && d.contains("/Encoding /Identity-H")), "{dicts:?}");
        assert!(dicts.iter().any(|d| d.contains("/Subtype /CIDFontType2") && d.contains("/CIDToGIDMap /Identity")));
        assert!(!dicts.iter().any(|d| d.contains("/Type3")), "no glyph is set in outlines");
        let file = objs.iter().find(|o| o.0.contains("/Length1")).unwrap();
        assert!(file.1.len() < epaint_default_fonts::UBUNTU_LIGHT.len() / 4, "a subset: {} bytes", file.1.len());
        assert!(FontRef::try_from_slice(&file.1).is_ok(), "the subset is a font");
        // Each character back from its CID, and the content setting two-byte codes.
        let cmap = objs.iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|s| s.contains("<0000> <FFFF>")).unwrap();
        for ch in ['A', 'ç', 'ã', 'o'] {
            assert!(cmap.contains(&format!("> <{:04X}>", ch as u32)), "{ch} in {cmap}");
        }
        let content = objs.iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|s| s.contains(" TJ")).unwrap();
        assert!(content.contains("[<0001>") && content.contains("<0004>] TJ"), "codes are CIDs: {content}");
        // Each CID is its character's glyph (CIDToGIDMap /Identity), with its width in /W.
        assert_eq!(cids_are_their_glyphs(font, &objs, "/Subtype /CIDFontType2"), 4, "A, ç, ã, o");
        // Outlines, as before, whatever the face allows.
        let (outlines, none) = write_with(&[page(font, "Ação")], "Fonts", Options { text: Text::Outlines, ..Options::default() });
        assert!(none.is_empty() && outlines == write(&[page(font, "Ação")], "Fonts"));
        let text = String::from_utf8_lossy(&outlines);
        assert!(text.contains("/Subtype /Type3") && !text.contains("/Type0"));
    }

    /// Checks, for every CID of the document's two-byte ToUnicode map, that the subset embedded
    /// for the CID font whose dictionary holds `subtype` draws its character as `font` does at
    /// that CID (the same advance and outline) and that /W has the face's width for it; how many
    /// CIDs it checked.
    fn cids_are_their_glyphs(font: &FontRef<'static>, objs: &[(String, Vec<u8>)], subtype: &str) -> usize {
        let file = objs.iter().find(|o| o.0.contains("/Length1") || o.0.contains("/Subtype /OpenType")).unwrap();
        let subset = FontRef::try_from_slice(&file.1).unwrap();
        let cid_font = objs.iter().find(|o| o.0.contains(subtype)).map(|o| o.0.as_str()).unwrap();
        let w: Vec<f32> = cid_font
            .split("/W [")
            .nth(1)
            .unwrap()
            .split(" ]")
            .next()
            .unwrap()
            .replace(['[', ']'], " ")
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        let cmap = objs.iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|s| s.contains("<0000> <FFFF>")).unwrap();
        let upm = font.units_per_em().unwrap();
        let mut checked = 0;
        for line in cmap.lines().filter(|l| l.starts_with('<') && l.len() == 13 && *l != "<0000> <FFFF>") {
            let cid = u16::from_str_radix(&line[1..5], 16).unwrap();
            let ch = char::from_u32(u32::from_str_radix(&line[8..12], 16).unwrap()).unwrap();
            let (own, sub) = (font.glyph_id(ch), GlyphId(cid));
            assert_eq!(font.h_advance_unscaled(own), subset.h_advance_unscaled(sub), "{ch}: CID {cid}");
            let points = |f: &FontRef, g| f.outline(g).map(|o| o.curves.iter().map(curve_points).collect::<Vec<_>>());
            assert_eq!(points(font, own), points(&subset, sub), "{ch}: CID {cid}'s outline");
            let width = w.chunks(2).find(|p| p[0] == f32::from(cid)).map(|p| p[1]).unwrap_or_else(|| panic!("CID {cid} in {w:?}"));
            assert!((width - font.h_advance_unscaled(own) / upm * 1000.0).abs() < 0.01, "{ch}: /W {width}");
            checked += 1;
        }
        checked
    }

    /// A subset's tag follows its face's file, not only its name and glyphs: two files with the
    /// same name and glyphs get two tags (#1815), whichever of their bytes differs, the high bytes
    /// of an eight-byte word too.
    #[test]
    fn a_subset_tag_follows_its_file() {
        let glyphs = [GlyphId(3), GlyphId(7)];
        let a = tag("Face", &glyphs, b"ABCDEFGHIJKLMNOP");
        assert_eq!(a, tag("Face", &glyphs, b"ABCDEFGHIJKLMNOP"), "the same subset, the same tag");
        assert!(a.len() == 6 && a.chars().all(|c| c.is_ascii_uppercase()), "{a}");
        // The same length, one byte apart: each of a word's eight bytes, in turn.
        for at in 0..8 {
            let mut other = *b"ABCDEFGHIJKLMNOP";
            other[at] ^= 0x5A;
            assert_ne!(a, tag("Face", &glyphs, &other), "byte {at}");
        }
    }

    /// A CFF face (an OpenType .otf) is embedded as a CIDFontType0 over its subset as an
    /// OpenType file (FontFile3, PDF 1.6), each CID its character's glyph. The face is the
    /// test's own (testdata/README.md): a square, a triangle and two bars, each its own width.
    #[test]
    fn a_cff_face_is_a_cid_font_type0() {
        let bytes: &'static [u8] = include_bytes!("../../testdata/test-cff.otf");
        let font: &'static FontRef<'static> = Box::leak(Box::new(FontRef::try_from_slice(bytes).unwrap()));
        let (pdf, outlined) = write_with(&[page(font, "CAB")], "CFF", Options { text: Text::Fonts, ..Options::default() });
        assert!(outlined.is_empty(), "{outlined:?}");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.6") && !text.contains("/Type3"), "OpenType font files are PDF 1.6");
        let objs = objects(&pdf);
        let cid_font = objs.iter().find(|o| o.0.contains("/Subtype /CIDFontType0")).map(|o| o.0.as_str()).unwrap();
        assert!(!cid_font.contains("/CIDToGIDMap"), "{cid_font}");
        assert!(objs.iter().any(|o| o.0.contains("/FontFile3")) && objs.iter().any(|o| o.0.contains("/Subtype /OpenType")));
        assert_eq!(cids_are_their_glyphs(font, &objs, "/Subtype /CIDFontType0"), 3, "C, A, B");
    }

    /// A halo (#1815) is its text again under it, filled and stroked: in an embedded face, the
    /// same font in rendering mode 2, its stroke rounded and as wide as the run's scale makes it,
    /// marked as an artifact; in outlines, its glyphs' outlines filled and stroked, as before.
    #[test]
    fn a_halo_is_its_text_filled_and_stroked() {
        let font = face(|_| ());
        let run = |halo: Option<f32>, color: Color| GlyphRun {
            font,
            size: 20.0,
            color,
            transform: Transform::from_row(2.0, 0.0, 0.0, 2.0, 10.0, 40.0),
            glyphs: "Rio".chars().zip(0..).map(|(ch, i)| Glyph { id: font.glyph_id(ch), x: 11.0 * i as f32, ch }).collect(),
            halo,
            decorative: false,
        };
        let page = || {
            let mut d = Drawing::new(200.0, 100.0);
            d.begin_group(0.9, rustgis_map::effects::Blend::Normal, None);
            d.glyphs(run(Some(2.4), Color::WHITE));
            d.end_group();
            d.glyphs(run(None, Color::BLACK));
            Page::drawing(d, 100.0, 50.0)
        };
        let streams =
            |pdf: &[u8]| objects(pdf).into_iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).collect::<Vec<_>>().join("\n");
        let (pdf, outlined) = write_with(&[page()], "Halo", Options { text: Text::Fonts, ..Options::default() });
        assert!(outlined.is_empty());
        let text = String::from_utf8_lossy(&pdf);
        assert_eq!(text.matches("/Subtype /Type0").count(), 1, "the halo in the text's own font");
        let content = streams(&pdf);
        let halo = content.split("/Artifact BMC\n").nth(1).and_then(|h| h.split("EMC\n").next()).unwrap_or_else(|| panic!("{content}"));
        assert!(halo.starts_with("BT\n2 Tr\n") && halo.ends_with("] TJ\n0 Tr\nET\n"), "{halo}");
        // Stroked in white, rounded, the run's 2.4 pixels doubled by its scale.
        assert!(
            content.contains("1 1 1 RG") && content.contains("4.8 w") && content.contains("1 J") && content.contains("1 j"),
            "{content}"
        );
        assert_eq!(content.matches(" Tr\n").count(), 2, "the text itself is filled only");
        // In outlines: the halo as its glyphs' outlines, filled and stroked.
        let outlines = streams(&write_with(&[page()], "Halo", Options { text: Text::Outlines, ..Options::default() }).0);
        assert!(!outlines.contains(" Tr\n") && !outlines.contains("/Artifact") && outlines.contains("\nB\n"), "{outlines}");
        // A synthetic-bold copy is decoration too, without a halo's stroked rendering mode.
        let mut drawing = Drawing::new(200.0, 100.0);
        drawing.glyphs(run(None, Color::BLACK));
        let mut strike = run(None, Color::BLACK);
        strike.transform.tx += 1.0;
        strike.decorative = true;
        drawing.glyphs(strike);
        let pdf = write_with(&[Page::drawing(drawing, 100.0, 50.0)], "Bold", Options { text: Text::Fonts, ..Options::default() }).0;
        let content = streams(&pdf);
        let (before, artifact) = content.split_once("/Artifact BMC\n").unwrap();
        assert_eq!(before.matches("BT\n").count(), 1, "the actual text is kept once");
        let artifact = artifact.split("EMC\n").next().unwrap();
        assert!(artifact.starts_with("BT\n") && !artifact.contains(" Tr\n"), "{artifact}");
    }

    /// A face whose licence forbids embedding (fsType 2) is set in outlines, and said.
    #[test]
    fn a_face_that_forbids_embedding_is_set_in_outlines() {
        let font = face(|b| {
            let os2 = table(b, b"OS/2");
            b[os2 + 8..os2 + 10].copy_from_slice(&2u16.to_be_bytes());
        });
        let (pdf, outlined) = write_with(&[page(font, "Rio")], "Restricted", Options { text: Text::Fonts, ..Options::default() });
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Subtype /Type3") && !text.contains("/Type0"));
        assert_eq!(outlined.len(), 1, "{outlined:?}");
        assert_eq!(outlined[0].face, "Ubuntu Light");
        assert_eq!(outlined[0].why, "its licence forbids embedding");
    }

    /// A face is embedded only from the file that draws it as the export does. A static face
    /// (Ubuntu Light) is its file's own, so it is embedded; another file's glyphs draw otherwise,
    /// so a face is never matched to one. (`Refused::Instance` needs a variable font drawn at
    /// another weight, which no bundled face is; `Refused::NotInFile` is all but unreachable,
    /// since `font_data` is the very file the face was read from.)
    #[test]
    fn only_the_face_as_drawn_is_embedded() {
        let font = face(|_| ());
        let glyphs: Vec<GlyphId> = "Rio".chars().map(|c| font.glyph_id(c)).collect();
        assert!(embed(font, &glyphs, MAX_EMBEDDED).is_ok());
        // Another file's glyphs draw otherwise, so a face is never matched to one.
        let hack = FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR).unwrap();
        assert!(!draws_alike(font, &hack, glyphs[0]));
        let full = embed(font, &glyphs, 10).err();
        assert_eq!(full, Some(Refused::DocumentTooLarge), "past the room left");
        assert_eq!(full.unwrap().to_string(), "the document's embedded faces would pass 32 MB");
    }

    /// A font file cut short or scrambled (in its glyphs, its table directory or its metrics) that
    /// the font parser still reads never panics embedding or naming its face: the face is embedded,
    /// or refused with the reason.
    #[test]
    fn embedding_a_scrambled_font_file_never_panics() {
        let clean = epaint_default_fonts::UBUNTU_LIGHT;
        let glyf = table(clean, b"glyf");
        let mut tried = 0;
        for step in 0..64usize {
            let mut bytes = clean.to_vec();
            // Scramble a stretch of the glyph outlines, the table directory or the metrics.
            let at = [glyf + step * 97, 12 + step * 3, table(clean, b"hmtx") + step * 13][step % 3];
            for b in bytes.iter_mut().skip(at).take(16) {
                *b = b.wrapping_mul(31).wrapping_add(step as u8);
            }
            if step % 4 == 0 {
                bytes.truncate(clean.len() * (step + 1) / 70);
            }
            let Ok(font) = FontRef::try_from_slice(&bytes) else { continue };
            tried += 1;
            let glyphs: Vec<GlyphId> = (0..40).map(GlyphId).collect();
            let _ = embed(&font, &glyphs, MAX_EMBEDDED);
            let _ = face_name(&font, &glyphs);
        }
        assert!(tried > 10, "{tried} scrambled files parsed");
    }
}
