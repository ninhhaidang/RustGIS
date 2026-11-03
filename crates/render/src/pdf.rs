//! A PDF writer. A page is either one picture (a rendered page, at the page's size) or a
//! [`Drawing`] kept as vectors: its paths, text and pictures become the page's content, its text
//! set in fonts whose glyphs are their outlines (Type 3), searchable and copyable through each
//! font's map back to Unicode.

use tiny_skia::Pixmap;

use crate::vector::{Drawing, Item, deflate_rows};

mod content;
mod fonts;

/// One PDF page and its size in millimetres. Pictures are compressed as they are made, so a long
/// document never holds its uncompressed images at once.
pub struct Page {
    content: Content,
    pub width_mm: f64,
    pub height_mm: f64,
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
        Page { content, width_mm: page_mm(width_mm), height_mm: page_mm(height_mm) }
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
}

impl Doc {
    /// A document with only its header written.
    fn new() -> Doc {
        Doc { out: b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec(), offsets: Vec::new() }
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

/// Writes the pages as a PDF document.
pub fn write(pages: &[Page], title: &str) -> Vec<u8> {
    let mut doc = Doc::new();
    let (catalog, tree, info) = (doc.reserve(), doc.reserve(), doc.reserve());
    let ids: Vec<usize> = pages.iter().map(|_| doc.reserve()).collect();
    let mut fonts = fonts::Fonts::default();
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
                let resources = content::page(&mut doc, &mut fonts, (*width, *height), (pw, ph), items);
                (resources.0, format!("{} 0 R", resources.1))
            }
        };
        doc.put(
            page,
            format!("<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Resources {resources} /Contents {contents} 0 R >>")
                .as_bytes(),
        );
    }
    fonts.write(&mut doc);
    doc.put(catalog, format!("<< /Type /Catalog /Pages {tree} 0 R >>").as_bytes());
    let kids: Vec<String> = ids.iter().map(|i| format!("{i} 0 R")).collect();
    doc.put(tree, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).as_bytes());
    doc.put(info, format!("<< /Title {} /Producer (RustGIS) >>", text_string(title)).as_bytes());
    doc.finish(catalog, info)
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
