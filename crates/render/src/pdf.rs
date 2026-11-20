//! A PDF writer. A page is either one picture (a rendered page, at the page's size) or a
//! [`Drawing`] kept as vectors: its paths, text and pictures become the page's content, its text
//! set in its faces embedded as subsets, or in fonts whose glyphs are their outlines (Type 3)
//! ([`Text`]); searchable and copyable either way, through each font's map back to Unicode. A
//! page's map frames can be georeferenced ([`Viewport`]), which makes the document a GeoPDF. A
//! drawing's layers (layout elements, map layers, their labels) become optional content groups,
//! which vector editors open as layers ([`Options::layers`]).

use tiny_skia::Pixmap;

use crate::vector::{Drawing, Item, deflate_rows};

mod content;
mod embed;
mod fonts;

/// One PDF page and its size in millimetres. Pictures are compressed as they are made, so a long
/// document never holds its uncompressed images at once.
pub struct Page {
    content: Content,
    pub width_mm: f64,
    pub height_mm: f64,
    viewports: Vec<Viewport>,
}

/// A georeferenced map frame's place on a page, or the part of it no other frame is drawn over,
/// and where on the Earth it shows, so that a reader can locate and measure in it: an ISO 32000
/// geospatial viewport, which makes the document a GeoPDF (#1191). Every viewport has a
/// geospatial measure: Acrobat Reader shows no coordinates on a page one of whose viewports has
/// none, and stops when its pointer enters one measured otherwise (a rectilinear measure).
#[derive(Clone, Debug, PartialEq)]
pub struct Viewport {
    /// The frame's name.
    pub name: String,
    /// Its rectangle on the page, in millimetres from the page's top-left corner: x, y, width,
    /// height.
    pub rect_mm: [f64; 4],
    /// Where on the Earth it shows.
    pub measure: GeoMeasure,
}

/// Where a georeferenced frame shows on the Earth ([`Viewport`]).
#[derive(Clone, Debug, PartialEq)]
pub struct GeoMeasure {
    /// Its corners' latitude and longitude, in degrees on its coordinate system's own datum,
    /// clockwise from the top-left.
    pub corners: [(f64, f64); 4],
    /// Its coordinate system as WKT (1), projected or geographic, and its EPSG code when it has one.
    pub wkt: String,
    pub projected: bool,
    pub epsg: Option<u16>,
}

impl Viewport {
    /// As a PDF viewport dictionary on a page `page_h` points tall: the frame's box in points from
    /// the page's bottom-left corner, its corners at the unit square's (the measure's `/LPTS`).
    fn dictionary(&self, page_h: f64) -> String {
        let k = 72.0 / 25.4;
        let [x, y, w, h] = self.rect_mm;
        let (x1, x2, top, bottom) = (x * k, (x + w) * k, page_h - y * k, page_h - (y + h) * k);
        let m = &self.measure;
        let gpts: Vec<String> = m.corners.iter().map(|(lat, lon)| format!("{lat:.9} {lon:.9}")).collect();
        let kind = if m.projected { "PROJCS" } else { "GEOGCS" };
        let epsg = m.epsg.map(|c| format!(" /EPSG {c}")).unwrap_or_default();
        let measure = format!(
            " /Measure << /Type /Measure /Subtype /GEO /Bounds [0 0 0 1 1 1 1 0] /GPTS [{}] /LPTS [0 1 1 1 1 0 0 0] \
             /GCS << /Type /{kind} /WKT {}{epsg} >> /PDU [/M /SQM /DEG] >>",
            gpts.join(" "),
            literal(&m.wkt),
        );
        format!("<< /Type /Viewport /BBox [{x1:.3} {bottom:.3} {x2:.3} {top:.3}] /Name {}{measure} >>", literal(&self.name))
    }
}

enum Content {
    /// Zlib-compressed RGB pixels, `width` × `height`.
    Picture {
        width: u32,
        height: u32,
        rgb_z: Vec<u8>,
    },
    Vector {
        width: f32,
        height: f32,
        items: Vec<Item>,
        cost: usize,
        overflowed: bool,
    },
}

impl Page {
    /// Roughly what it adds to the document, in bytes: the compressed picture, or what the
    /// drawing's content takes ([`Drawing::cost`]).
    pub fn len(&self) -> usize {
        match &self.content {
            Content::Picture { rgb_z, .. } => rgb_z.len(),
            Content::Vector { cost, .. } => *cost,
        }
    }
    /// Whether it is a drawing that refused something drawn, having grown too large
    /// ([`Drawing::overflowed`]): incomplete, for its caller to draw as a picture instead.
    pub fn overflowed(&self) -> bool {
        matches!(self.content, Content::Vector { overflowed: true, .. })
    }

    /// Whether it holds nothing: no picture data, or a drawing with no items.
    pub fn is_empty(&self) -> bool {
        match &self.content {
            Content::Picture { rgb_z, .. } => rgb_z.is_empty(),
            Content::Vector { items, .. } => items.is_empty(),
        }
    }

    /// A page that is one picture, `image` stretched over it.
    pub fn new(image: &Pixmap, width_mm: f64, height_mm: f64) -> Page {
        // RGB without the alpha (pages are drawn on white, so they're opaque).
        let rgb_z = deflate_rows(image.pixels().chunks(image.width().max(1) as usize), |line, row| {
            row.extend(line.iter().flat_map(|px| {
                let c = px.demultiply();
                [c.red(), c.green(), c.blue()]
            }))
        });
        // Compressing into memory doesn't fail; if it ever did, the page is left blank (one
        // white pixel stretched over it) rather than holding a second full-size copy.
        let content = match rgb_z {
            Some(rgb_z) => Content::Picture { width: image.width(), height: image.height(), rgb_z },
            None => Content::Picture { width: 1, height: 1, rgb_z: miniz_oxide::deflate::compress_to_vec_zlib(&[255, 255, 255], 6) },
        };
        Page { content, width_mm: page_mm(width_mm), height_mm: page_mm(height_mm), viewports: Vec::new() }
    }

    /// The page with its map frames georeferenced ([`Viewport`]).
    pub fn with_viewports(mut self, viewports: Vec<Viewport>) -> Page {
        self.viewports = viewports;
        self
    }

    /// A page drawn as `drawing`, stretched over it; nothing paints its background but the
    /// drawing (a viewer shows white).
    pub fn drawing(drawing: Drawing, width_mm: f64, height_mm: f64) -> Page {
        let (cost, overflowed) = (drawing.cost(), drawing.overflowed());
        let (width, height, items) = drawing.finish();
        Page {
            content: Content::Vector { width, height, items, cost, overflowed },
            width_mm: page_mm(width_mm),
            height_mm: page_mm(height_mm),
            viewports: Vec::new(),
        }
    }
}

/// A page side that isn't a positive number would make the page box invalid.
fn page_mm(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 { v.min(5000.0) } else { 210.0 }
}

/// The objects of a document being written, in the order written; each is numbered when
/// reserved, so objects can refer to ones written later.
pub(crate) struct Doc {
    out: Vec<u8>,
    offsets: Vec<Option<usize>>,
    /// Shared pictures' image objects, by their pixels' address and whether smoothed (see
    /// [`crate::vector::Image::shared`]): written once, placed wherever drawn. The pixels are
    /// held, so their address isn't reused by others while it is a key.
    pub(crate) images: std::collections::HashMap<(usize, bool), (std::sync::Arc<crate::vector::Pixels>, usize)>,
}

impl Doc {
    /// A document with only its header written.
    fn new() -> Doc {
        Doc::of_version("1.4")
    }

    /// A document of PDF version `version` with only its header written.
    fn of_version(version: &str) -> Doc {
        let mut out = format!("%PDF-{version}\n").into_bytes();
        out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");
        Doc { out, offsets: Vec::new(), images: std::collections::HashMap::new() }
    }

    /// A new object's number.
    pub(crate) fn reserve(&mut self) -> usize {
        self.offsets.push(None);
        self.offsets.len()
    }

    /// Writes object `id`'s `body`; nothing for a number never reserved or already written.
    pub(crate) fn put(&mut self, id: usize, body: &[u8]) {
        let Some(slot @ None) = self.offsets.get_mut(id.wrapping_sub(1)) else { return };
        *slot = Some(self.out.len());
        self.out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        self.out.extend_from_slice(body);
        self.out.extend_from_slice(b"\nendobj\n");
    }

    /// Writes object `id` as a stream of `data`, its dictionary's other entries `dict`,
    /// compressed unless it already is (`deflated`).
    pub(crate) fn stream(&mut self, id: usize, dict: &str, data: &[u8], deflated: bool) {
        let z;
        let (data, filter) = if deflated {
            (data, " /Filter /FlateDecode")
        } else if data.len() > 64 {
            z = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
            (z.as_slice(), " /Filter /FlateDecode")
        } else {
            (data, "")
        };
        let mut body = format!("<< {dict}{filter} /Length {} >>\nstream\n", data.len()).into_bytes();
        body.reserve(data.len() + 11);
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.put(id, &body);
    }

    /// The finished document: objects reserved and never written become nulls.
    fn finish(mut self, root: usize, info: usize) -> Vec<u8> {
        for id in 1..=self.offsets.len() {
            self.put(id, b"null");
        }
        let xref = self.out.len();
        let mut out = self.out;
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len() + 1).as_bytes());
        for o in &self.offsets {
            out.extend_from_slice(format!("{:010} 00000 n \n", o.unwrap_or(0)).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root {root} 0 R /Info {info} 0 R >>\nstartxref\n{xref}\n%%EOF\n", self.offsets.len() + 1)
                .as_bytes(),
        );
        out
    }
}

/// How a document's vector text is written (#1815).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    /// In the faces it is set in, embedded as subsets, so that a vector editor reads it as text;
    /// a face that can't be is set in outlines, and said ([`Outlined`]).
    Fonts,
    /// In fonts whose glyphs are their outlines (Type 3): searchable and copyable, but drawings
    /// in a vector editor, as a print shop asking for outlined type wants.
    Outlines,
}

/// A face whose text was set in outlines though [`Text::Fonts`] was asked for, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Outlined {
    pub face: String,
    pub why: String,
}

/// How a document is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// The drawings' layers as optional content groups (#1815): each layout element, and in a
    /// map frame each map layer and each layer's labels, a layer named after it, nested as drawn,
    /// so a vector editor that reads optional content (VectorCraft, where it was checked) opens
    /// the page with them as its layers. Off, the page is the same, without them.
    pub layers: bool,
    /// How its vector text is written: in its faces, or in outlines ([`Text`]).
    pub text: Text,
}

impl Default for Options {
    fn default() -> Options {
        Options { layers: true, text: Text::Fonts }
    }
}

/// The document's optional content groups, one for each layer drawn, in the order first drawn:
/// a layer is the same group on every page it is on (a map series' title, page after page), by
/// its name, the group it is in, and its id, or without one which of the layers so named there
/// it is.
#[derive(Default)]
pub(crate) struct Layers {
    /// Whether layers are written at all ([`Options::layers`]).
    pub(crate) on: bool,
    /// Each group's object number, name, and the group it is in (its place here).
    list: Vec<(usize, String, Option<usize>)>,
    by_key: std::collections::HashMap<(Option<usize>, String, Option<u64>, usize), usize>,
}

impl Layers {
    /// The group named `name` with `id` inside group `parent`, or without an id the `n`th so
    /// named there: its place here and object number, numbered the first time.
    pub(crate) fn group(&mut self, doc: &mut Doc, parent: Option<usize>, name: &str, id: Option<u64>, n: usize) -> (usize, usize) {
        let key = (parent, name.to_string(), id, if id.is_some() { 0 } else { n });
        if let Some(&at) = self.by_key.get(&key)
            && let Some(&(id, ..)) = self.list.get(at)
        {
            return (at, id);
        }
        let id = doc.reserve();
        self.list.push((id, name.to_string(), parent));
        self.by_key.insert(key, self.list.len() - 1);
        (self.list.len() - 1, id)
    }

    /// Writes the groups, and the catalog's `/OCProperties` entry naming them, every one on and
    /// listed as drawn, each inside the one it is in (`/Order`); empty for none.
    fn write(&self, doc: &mut Doc) -> String {
        if self.list.is_empty() {
            return String::new();
        }
        for (id, name, _) in &self.list {
            doc.put(*id, format!("<< /Type /OCG /Name {} >>", text_string(name)).as_bytes());
        }
        let ids: Vec<String> = self.list.iter().map(|(id, ..)| format!("{id} 0 R")).collect();
        format!(" /OCProperties << /OCGs [{}] /D << /Name (Layers) /BaseState /ON /Order [{}] >> >>", ids.join(" "), self.order(None))
    }

    /// The groups inside `parent`, in order, each followed by the array of those inside it.
    fn order(&self, parent: Option<usize>) -> String {
        let mut out = Vec::new();
        for (at, (id, _, p)) in self.list.iter().enumerate() {
            if *p == parent {
                out.push(format!("{id} 0 R"));
                let inner = self.order(Some(at));
                if !inner.is_empty() {
                    out.push(format!("[{inner}]"));
                }
            }
        }
        out.join(" ")
    }
}

/// Whether `items` hold a layer.
fn has_layer(items: &[Item]) -> bool {
    items.iter().any(|i| matches!(i, Item::Group(g) if g.layer.is_some() || has_layer(&g.items)))
}

/// Writes the pages as a PDF document, with their layers, its text in outlines
/// ([`Text::Outlines`]).
pub fn write(pages: &[Page], title: &str) -> Vec<u8> {
    write_with(pages, title, Options { text: Text::Outlines, ..Options::default() }).0
}

/// Writes the pages as a PDF document, as `options` say, and the faces set in outlines though
/// fonts were asked for. Each face is subset in one step; between faces it looks at Cancel
/// (`rustgis_data::progress`), and once cancelled sets the faces left in outlines, unsaid: a
/// cancelled export writes nothing.
pub fn write_with(pages: &[Page], title: &str, options: Options) -> (Vec<u8>, Vec<Outlined>) {
    // Every face's glyphs, gathered before any is written: a face is embedded or not as a whole,
    // and its glyphs' codes are the subset's.
    let mut used: Vec<(&'static ab_glyph::FontRef<'static>, Vec<ab_glyph::GlyphId>)> = Vec::new();
    if options.text == Text::Fonts {
        let mut seen = (std::collections::HashMap::new(), std::collections::HashSet::new());
        for p in pages {
            if let Content::Vector { items, .. } = &p.content {
                gather(items, &mut used, &mut seen);
            }
        }
    }
    let (mut real, mut outlined, mut room) = (Vec::new(), Vec::new(), embed::MAX_EMBEDDED);
    for (font, glyphs) in used {
        if rustgis_data::progress::check().is_err() {
            break;
        }
        match embed::embed(font, &glyphs, room) {
            Ok(e) => {
                room = room.saturating_sub(e.file.len());
                real.push((font, e));
            }
            Err(why) => outlined.push(Outlined { face: embed::face_name(font, &glyphs), why: why.to_string() }),
        }
    }
    // Georeferenced frames make the document PDF 1.7 with Adobe's extension level 3, where
    // geospatial measures are defined (since taken into ISO 32000-2); an embedded CFF face makes
    // it 1.6, where OpenType font files are; layers (optional content) make it 1.5; what else it
    // holds is 1.4.
    let geo = pages.iter().any(|p| !p.viewports.is_empty());
    let layered = options.layers && pages.iter().any(|p| matches!(&p.content, Content::Vector { items, .. } if has_layer(items)));
    let mut doc = if geo {
        Doc::of_version("1.7")
    } else if real.iter().any(|(_, e)| e.cff) {
        Doc::of_version("1.6")
    } else if layered {
        Doc::of_version("1.5")
    } else {
        Doc::new()
    };
    let (catalog, tree, info) = (doc.reserve(), doc.reserve(), doc.reserve());
    let ids: Vec<usize> = pages.iter().map(|_| doc.reserve()).collect();
    let mut fonts = fonts::Fonts::with_real(&mut doc, real);
    let mut layers = Layers { on: layered, ..Layers::default() };
    for (p, &page) in pages.iter().zip(&ids) {
        let (pw, ph) = (p.width_mm * 72.0 / 25.4, p.height_mm * 72.0 / 25.4);
        let (contents, resources) = match &p.content {
            Content::Picture { width, height, rgb_z } => {
                let (contents, image) = (doc.reserve(), doc.reserve());
                let draw = format!("q {pw:.2} 0 0 {ph:.2} 0 0 cm /Im0 Do Q");
                doc.stream(contents, "", draw.as_bytes(), false);
                doc.stream(
                    image,
                    &format!("/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8"),
                    rgb_z,
                    true,
                );
                (contents, format!("<< /XObject << /Im0 {image} 0 R >> >>"))
            }
            Content::Vector { width, height, items, .. } => {
                let resources = content::page(&mut doc, &mut fonts, &mut layers, (*width, *height), (pw, ph), items);
                (resources.0, format!("{} 0 R", resources.1))
            }
        };
        let viewports = if p.viewports.is_empty() {
            String::new()
        } else {
            let each: Vec<String> = p.viewports.iter().map(|v| v.dictionary(ph)).collect();
            format!(" /VP [{}]", each.join(" "))
        };
        doc.put(
            page,
            format!(
                "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Resources {resources} /Contents {contents} 0 R{viewports} >>"
            )
            .as_bytes(),
        );
    }
    fonts.write(&mut doc);
    let optional = layers.write(&mut doc);
    let extensions = if geo { " /Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 3 >> >>" } else { "" };
    doc.put(catalog, format!("<< /Type /Catalog /Pages {tree} 0 R{extensions}{optional} >>").as_bytes());
    let kids: Vec<String> = ids.iter().map(|i| format!("{i} 0 R")).collect();
    doc.put(tree, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).as_bytes());
    doc.put(info, format!("<< /Title {} /Producer (RustGIS) >>", text_string(title)).as_bytes());
    (doc.finish(catalog, info), outlined)
}

/// Faces' places in the list of glyphs used, by their address, and the glyphs already listed.
type Seen = (std::collections::HashMap<usize, usize>, std::collections::HashSet<(usize, u16)>);

/// Adds the glyphs `items` set to `used`, face by face, each once.
fn gather(items: &[Item], used: &mut Vec<(&'static ab_glyph::FontRef<'static>, Vec<ab_glyph::GlyphId>)>, seen: &mut Seen) {
    for item in items {
        match item {
            Item::Glyphs(run) => {
                let key = std::ptr::from_ref(run.font) as usize;
                let at = *seen.0.entry(key).or_insert_with(|| {
                    used.push((run.font, Vec::new()));
                    used.len() - 1
                });
                for g in &run.glyphs {
                    if seen.1.insert((key, g.id.0))
                        && let Some((_, glyphs)) = used.get_mut(at)
                    {
                        glyphs.push(g.id);
                    }
                }
            }
            Item::Group(g) => gather(&g.items, used, seen),
            Item::Fill { .. } | Item::Stroke { .. } | Item::Image(_) => {}
        }
    }
}

/// A PDF literal string of any length (a WKT): ASCII in parentheses, its backslashes and
/// parentheses escaped; anything else as UTF-16BE hex.
fn literal(s: &str) -> String {
    if s.is_ascii() && !s.chars().any(|c| c.is_control()) {
        let escaped: String = s.chars().flat_map(|c| if matches!(c, '(' | ')' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
        format!("({escaped})")
    } else {
        let hex: String = s.encode_utf16().map(|u| format!("{u:04X}")).collect();
        format!("<FEFF{hex}>")
    }
}

/// A PDF text string: plain ASCII in parentheses, anything else as UTF-16BE hex.
fn text_string(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).take(200).collect();
    if s.is_ascii() {
        let escaped: String = s.chars().flat_map(|c| if matches!(c, '(' | ')' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
        format!("({escaped})")
    } else {
        let hex: String = s.encode_utf16().map(|u| format!("{u:04X}")).collect();
        format!("<FEFF{hex}>")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The objects of `pdf` by number, each body between `obj` and `endobj`, read through the
    /// cross-reference table (so its offsets are checked too), and the streams inflated.
    pub(crate) fn objects(pdf: &[u8]) -> Vec<(String, Vec<u8>)> {
        let find = |from: usize, needle: &[u8]| pdf[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from);
        let start = std::str::from_utf8(&pdf[find(0, b"startxref\n").unwrap() + 10..]).unwrap();
        let xref: usize = start.lines().next().unwrap().parse().unwrap();
        let table = std::str::from_utf8(&pdf[xref..]).unwrap();
        let mut lines = table.lines().skip(1);
        let count: usize = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
        let mut out = Vec::new();
        for (id, line) in lines.skip(1).take(count - 1).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            let head = format!("{} 0 obj\n", id + 1);
            assert!(pdf[off..].starts_with(head.as_bytes()), "object {} at {off}", id + 1);
            let body = &pdf[off + head.len()..find(off, b"\nendobj\n").unwrap()];
            let (dict, data) = match body.windows(8).position(|w| w == b"\nstream\n") {
                Some(s) => {
                    let data = &body[s + 8..body.len() - b"\nendstream".len()];
                    let dict = String::from_utf8_lossy(&body[..s]).into_owned();
                    let data = if dict.contains("/FlateDecode") {
                        miniz_oxide::inflate::decompress_to_vec_zlib(data).unwrap()
                    } else {
                        data.to_vec()
                    };
                    (dict, data)
                }
                None => (String::from_utf8_lossy(body).into_owned(), Vec::new()),
            };
            out.push((dict, data));
        }
        out
    }

    #[test]
    fn writes_a_two_page_pdf() {
        let mut a = Pixmap::new(20, 30).unwrap();
        a.fill(tiny_skia::Color::WHITE);
        let pdf = write(&[Page::new(&a, 210.0, 297.0), Page::new(&a, 297.0, 210.0)], "Atlas (draft)");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4") && text.trim_end().ends_with("%%EOF"));
        assert!(text.contains("/Count 2") && text.contains("/MediaBox [0 0 595.28 841.89]") && text.contains("Atlas \\(draft\\)"));
        assert_eq!(text_string("España"), "<FEFF004500730070006100F10061>");
        let objs = objects(&pdf);
        assert!(objs[0].0.contains("/Type /Catalog") && objs[3].0.contains("/Type /Page "));
    }

    #[test]
    fn pages_compress_row_by_row_and_sanitise_their_size() {
        let mut a = Pixmap::new(7, 5).unwrap();
        a.fill(tiny_skia::Color::from_rgba8(10, 200, 30, 255));
        let p = Page::new(&a, f64::NAN, 0.0);
        let Content::Picture { rgb_z, .. } = &p.content else { panic!("a picture page") };
        let rgb = miniz_oxide::inflate::decompress_to_vec_zlib(rgb_z).unwrap();
        assert_eq!(rgb.len(), 7 * 5 * 3);
        assert!(rgb.chunks(3).all(|c| c == [10, 200, 30]));
        assert_eq!((p.width_mm, p.height_mm), (210.0, 210.0));
    }

    /// A georeferenced frame is a viewport on its page (#1191): its box measured from the page's
    /// bottom, its corners as latitude then longitude, its WKT whole and escaped.
    #[test]
    fn a_georeferenced_frame_is_a_viewport_on_its_page() {
        let mut a = Pixmap::new(2, 2).unwrap();
        a.fill(tiny_skia::Color::WHITE);
        let wkt = format!("PROJCS[\"A (b)\\c\",{}]", "x".repeat(400));
        let v = Viewport {
            name: "Main map".into(),
            rect_mm: [25.4, 25.4, 50.8, 25.4],
            measure: GeoMeasure {
                corners: [(-19.5, -45.5), (-19.5, -44.5), (-20.5, -44.5), (-20.5, -45.5)],
                wkt: wkt.clone(),
                projected: true,
                epsg: Some(31983),
            },
        };
        let page_h = 297.0 * 72.0 / 25.4;
        let d = v.dictionary(page_h);
        // An inch in from the left and the top, two inches wide and one tall.
        assert!(d.contains(&format!("/BBox [72.000 {:.3} 216.000 {:.3}]", page_h - 144.0, page_h - 72.0)), "{d}");
        assert!(d.contains("/GPTS [-19.500000000 -45.500000000 -19.500000000 -44.500000000 -20.500000000 -44.500000000"), "{d}");
        assert!(d.contains("/LPTS [0 1 1 1 1 0 0 0]") && d.contains("/Type /PROJCS") && d.contains("/EPSG 31983"), "{d}");
        assert!(d.contains(&format!("/WKT (PROJCS[\"A \\(b\\)\\\\c\",{}])", "x".repeat(400))), "{d}");
        let pdf = write(&[Page::new(&a, 210.0, 297.0).with_viewports(vec![v]), Page::new(&a, 210.0, 297.0)], "Geo");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.7") && text.contains("/Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 3 >> >>"));
        let objs = objects(&pdf);
        let pages: Vec<&String> = objs.iter().map(|o| &o.0).filter(|d| d.contains("/Type /Page ")).collect();
        assert!(pages[0].contains(" /VP [<< /Type /Viewport") && !pages[1].contains("/VP"), "{pages:?}");
        // A document without a georeferenced frame stays as it was.
        let plain = write(&[Page::new(&a, 210.0, 297.0)], "Plain");
        assert!(plain.starts_with(b"%PDF-1.4") && !String::from_utf8_lossy(&plain).contains("/Extensions"));
        assert_eq!(literal("é"), "<FEFF00E9>");
        // A frame's name is written whole, however long.
        let measure = GeoMeasure { corners: [(0.0, 0.0); 4], wkt: "W".into(), projected: true, epsg: None };
        let long = Viewport { name: "n".repeat(300), rect_mm: [0.0, 0.0, 1.0, 1.0], measure };
        assert!(long.dictionary(page_h).contains(&format!("/Name ({})", "n".repeat(300))));
    }

    /// A frame's viewport is where the page draws it: from the whole pixel it starts on at the
    /// export's dpi, the page's pixels stretched over its paper (#1191).
    #[test]
    fn a_viewport_is_where_the_page_draws_its_frame() {
        let mut layout: rustgis_map::layout::Layout = serde_json::from_value(serde_json::json!({
            "name": "L", "width": 297.0, "height": 210.0,
            "elements": [{"id": 1, "name": "Main", "x": 10.0, "y": 10.0, "w": 100.0, "h": 80.0,
                "kind": {"MapFrame": {"map": "Map", "center": {"x": -45.0, "y": -20.0}, "scale": 50000.0}}}]
        }))
        .unwrap();
        layout.clamp();
        let maps = [rustgis_map::Map::new("Map")];
        let geo = crate::layout::frame_viewports(&layout, &maps, 72.0);
        let [x, y, w, _] = geo.viewports[0].rect_mm;
        // At 72 dpi a millimetre is about 2.83 pixels: 10 mm falls inside pixel 28, where the frame starts.
        let (wpx, _, k) = crate::layout::page_geometry(&layout, 72.0);
        let sx = 297.0 / f64::from(wpx);
        assert!((x - (10.0 * k).round() * sx).abs() < 1e-9 && (x - 10.0).abs() > 0.05, "{x}");
        assert!((y - (10.0 * k).round() * (210.0 / f64::from(crate::layout::page_geometry(&layout, 72.0).1))).abs() < 1e-9, "{y}");
        assert!((w - 100.0 * k * sx).abs() < 1e-9, "{w}");
        // At 254 dpi, ten pixels to the millimetre, it is the frame's own box.
        let geo = crate::layout::frame_viewports(&layout, &maps, 254.0);
        assert!((geo.viewports[0].rect_mm[0] - 10.0).abs() < 1e-6, "{:?}", geo.viewports[0].rect_mm);
    }

    /// A page of layers as a layout draws them: a title, a map frame (placed, clipped) holding
    /// a map layer and its labels, and two elements named alike, each square in its own colour.
    fn layered_page() -> Page {
        use crate::vector::{Brush, Drawing};
        let square = |d: &mut Drawing, c: u8| {
            let r = tiny_skia::Rect::from_xywh(1.0, 1.0, 4.0, 4.0).unwrap();
            let fill = Brush::Solid(tiny_skia::Color::from_rgba8(c, 0, 0, 255));
            d.fill(tiny_skia::PathBuilder::from_rect(r), fill, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity());
        };
        let mut d = Drawing::new(20.0, 20.0);
        d.begin_layer("Título (rascunho)", None);
        square(&mut d, 1);
        d.end_group();
        d.begin_layer("Mapa", None);
        let mut frame = Drawing::new(20.0, 20.0);
        frame.begin_layer("Países", Some(7));
        square(&mut frame, 2);
        frame.end_group();
        frame.begin_layer("Labels: Países", Some(7));
        square(&mut frame, 3);
        frame.end_group();
        d.place(frame, tiny_skia::Transform::from_translate(2.0, 2.0), 1.0, rustgis_map::effects::Blend::Normal, None);
        d.end_group();
        for c in [4, 5] {
            d.begin_layer("Texto", None);
            square(&mut d, c);
            d.end_group();
        }
        Page::drawing(d, 20.0, 20.0)
    }

    /// A drawing's layers are optional content groups (#1815): named, nested in `/Order` as
    /// drawn, each one's content marked inside its BDC/EMC; two elements named alike are two
    /// groups, and a map series' pages share theirs. Without layers, the same page unmarked.
    #[test]
    fn layers_are_optional_content_groups() {
        let pdf = write(&[layered_page(), layered_page()], "Layers");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.5"), "optional content is PDF 1.5");
        let objs = objects(&pdf);
        let ocg = |name: &str| {
            let want = format!("<< /Type /OCG /Name {} >>", text_string(name));
            objs.iter()
                .position(|o| o.0 == want)
                .map(|i| i + 1)
                .unwrap_or_else(|| panic!("{name}: {objs:?}", objs = objs.iter().map(|o| &o.0).collect::<Vec<_>>()))
        };
        let (title, map, layer, labels) = (ocg("Título (rascunho)"), ocg("Mapa"), ocg("Países"), ocg("Labels: Países"));
        let texts: Vec<usize> = objs.iter().enumerate().filter(|o| o.1.0.contains("/Name (Texto)")).map(|o| o.0 + 1).collect();
        assert_eq!(texts.len(), 2, "two elements named alike, two groups");
        assert_eq!(objs.iter().filter(|o| o.0.contains("/Type /OCG")).count(), 6, "the second page reuses the first's");
        let catalog = &objs[0].0;
        let order = format!("/Order [{title} 0 R {map} 0 R [{layer} 0 R {labels} 0 R] {} 0 R {} 0 R]", texts[0], texts[1]);
        assert!(
            catalog.contains("/OCProperties << /OCGs [") && catalog.contains(&order) && catalog.contains("/BaseState /ON"),
            "{catalog}"
        );
        // Each square inside its own group's marks, the map's inside the frame's.
        let content = objs.iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|c| c.contains(" BDC")).unwrap();
        let inside = |id: usize, colour: &str| {
            let start = content.find(&format!("/OC /OC{id} BDC")).unwrap_or_else(|| panic!("{id}: {content}"));
            let mut depth = 0;
            let mut end = start;
            for (i, line) in content[start..].lines().scan(start, |at, l| {
                let here = *at;
                *at += l.len() + 1;
                Some((here, l))
            }) {
                if line.ends_with(" BDC") {
                    depth += 1;
                } else if line == "EMC" {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
            }
            assert!(content[start..end].contains(colour), "{id} holds {colour}: {}", &content[start..end]);
        };
        inside(title, "0.004 0 0 rg");
        inside(map, "0.008 0 0 rg");
        inside(layer, "0.008 0 0 rg");
        inside(labels, "0.012 0 0 rg");
        inside(texts[1], "0.02 0 0 rg");
        assert_eq!(content.matches(" BDC").count(), content.matches("EMC\n").count());
        let page = objs.iter().find(|o| o.0.contains("/Properties <<")).map(|o| &o.0).unwrap();
        assert!(page.contains(&format!("/OC{title} {title} 0 R")), "{page}");
        // Without layers: no groups, no marks, and the same drawing.
        let plain = write_with(&[layered_page()], "Layers", Options { layers: false, text: Text::Outlines }).0;
        let plain_text = String::from_utf8_lossy(&plain);
        assert!(plain_text.starts_with("%PDF-1.4") && !plain_text.contains("/OCProperties") && !plain_text.contains("/Type /OCG"));
        let plain_content =
            objects(&plain).into_iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|c| c.contains(" rg")).unwrap();
        // The same things painted in the same order: only the marks and the frame's clip and
        // transform, set again for each of its layers, differ.
        let paints = |c: &str| -> String {
            c.lines()
                .filter(|l| !(l.ends_with(" BDC") || *l == "EMC" || *l == "q" || *l == "Q" || l.ends_with(" cm")))
                .map(|l| format!("{l}\n"))
                .collect()
        };
        assert_eq!(paints(&content), paints(&plain_content));
        // The frame's layers are marked outside its transform, each moved on its own: an editor
        // can't put a layer inside a clipping group.
        for id in [layer, labels] {
            assert!(content.contains(&format!("/OC /OC{id} BDC\nq\n1 0 0 1 2 2 cm\n")), "{id}: {content}");
        }
        // A page with no layers is written as before.
        let mut a = Pixmap::new(2, 2).unwrap();
        a.fill(tiny_skia::Color::WHITE);
        assert!(!String::from_utf8_lossy(&write(&[Page::new(&a, 10.0, 10.0)], "Picture")).contains("/OCProperties"));
    }

    #[test]
    fn unwritten_objects_become_nulls_and_writing_twice_keeps_the_first() {
        let mut doc = Doc::new();
        let (a, b) = (doc.reserve(), doc.reserve());
        doc.put(a, b"<< /Type /Catalog >>");
        doc.put(a, b"<< /Twice true >>");
        doc.put(99, b"never reserved");
        let pdf = doc.finish(a, b);
        let objs = objects(&pdf);
        assert_eq!(objs.len(), 2);
        assert_eq!((objs[0].0.as_str(), objs[1].0.as_str()), ("<< /Type /Catalog >>", "null"));
    }
}
