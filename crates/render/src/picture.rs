//! Picture graphics: PNG or JPEG images (base64 in the graphic's `image` attribute) stretched over
//! the graphic's rectangle. And picture markers ([`Picture`]), drawn at a point.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::Canvas;
use base64::Engine;
use rustgis_data::{Dataset, Value};
use rustgis_map::Picture;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

/// Pictures are drawn from at most this many pixels per side (bigger ones are scaled down).
const MAX_SIDE: u32 = 4096;
/// Pictures declaring more pixels per side than this aren't decoded at all.
const MAX_DECLARED: u32 = 20_000;
/// Most memory a picture's decoder may allocate.
const MAX_ALLOC: u64 = 512 << 20;
/// Decoded pictures kept between frames, and the most pixel memory they may hold together.
const CACHE: usize = 32;
const CACHE_BYTES: usize = 256 << 20;
/// Picture markers are drawn from at most this many pixels per side: an 80 pt marker printed at
/// 600 dpi.
const MARKER_SIDE: u32 = 700;
/// Decoded picture markers kept between frames, and the most pixel memory they may hold together.
/// As many as a layer's classes can show ([`rustgis_map::symbology::MAX_PICTURES`] are kept).
const MARKERS: usize = 1024;
const MARKERS_BYTES: usize = 128 << 20;

/// Decoded pictures (with the base64 they came from, compared on every hit so pictures whose
/// cheap keys collide never swap), and their keys oldest first (for eviction).
#[derive(Default)]
struct Cache {
    images: HashMap<u64, (String, Option<Pixmap>)>,
    order: std::collections::VecDeque<u64>,
}

/// Decoded picture markers (none: the image doesn't decode), by the [`Picture`] (where its image
/// is kept, so a hit compares no bytes), the pixel memory they hold, and their pictures oldest
/// first (for eviction).
#[derive(Default)]
struct Markers {
    /// Each image as drawn, and its height for its width.
    images: HashMap<Picture, Option<(Pixmap, f32)>>,
    bytes: usize,
    order: std::collections::VecDeque<Picture>,
}

thread_local! {
    static DECODED: RefCell<Cache> = RefCell::new(Cache::default());
    static MARKERS_DECODED: RefCell<Markers> = RefCell::new(Markers::default());
}

/// A cheap key for a picture: its length and its first and last bytes (hashing megabytes of
/// base64 every frame would be slow).
fn key(b64: &str) -> u64 {
    let bytes = b64.as_bytes();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.len().hash(&mut h);
    bytes.get(..bytes.len().min(512)).hash(&mut h);
    bytes.get(bytes.len().saturating_sub(512)..).hash(&mut h);
    h.finish()
}

/// The pixel size of base64 PNG or JPEG bytes, read from the header without decoding the image.
pub fn size(b64: &str) -> Option<(u32, u32)> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

/// Decodes base64 PNG or JPEG bytes into a premultiplied pixmap. Pictures come from project
/// files, so their declared size is checked before decoding and the decoder's memory is capped.
pub fn decode(b64: &str) -> Option<Pixmap> {
    decode_within(b64, MAX_SIDE)
}

/// [`decode`], scaled down to at most `side` pixels per side.
fn decode_within(b64: &str, side: u32) -> Option<Pixmap> {
    decode_sized(b64, side).map(|(pm, _)| pm)
}

/// [`decode_within`], with the image's height for its width before it was scaled down (a
/// thumbnail's one-pixel side can't keep a very narrow image's).
fn decode_sized(b64: &str, side: u32) -> Option<(Pixmap, f32)> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    let reader = || image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok();
    let (w, h) = reader()?.into_dimensions().ok()?;
    if w == 0 || h == 0 || w > MAX_DECLARED || h > MAX_DECLARED {
        return None;
    }
    let mut r = reader()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    r.limits(limits);
    let img = r.decode().ok()?;
    let aspect = img.height() as f32 / img.width().max(1) as f32;
    let img = if img.width() > side || img.height() > side { img.thumbnail(side, side) } else { img };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut pm = Pixmap::new(w, h)?;
    for (dst, src) in pm.pixels_mut().iter_mut().zip(rgba.pixels()) {
        let [r, g, b, a] = src.0;
        let c = tiny_skia::ColorU8::from_rgba(r, g, b, a);
        *dst = c.premultiply();
    }
    Some((pm, aspect))
}

/// Draws row `row`'s picture into the pixel rectangle (x0, y0)–(x1, y1).
pub fn draw(pm: &mut Canvas, ds: &Dataset, row: usize, (x0, y0, x1, y1): (f32, f32, f32, f32), opacity: f32) {
    let Some(Value::Text(b64)) = ds.field_index("image").map(|fi| ds.value(row, fi)) else { return };
    let (w, h) = ((x1 - x0).abs(), (y1 - y0).abs());
    if !(w.is_finite() && h.is_finite()) || w < 1.0 || h < 1.0 || w > 1e5 || h > 1e5 {
        return;
    }
    let key = key(b64);
    DECODED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.images.get(&key).is_none_or(|(src, _)| src.as_str() != b64.as_str()) {
            // Evict the oldest pictures (not all of them) to stay within the count and memory budget.
            let held = |c: &Cache| c.images.values().filter_map(|(_, p)| p.as_ref()).map(|p| p.data().len()).sum::<usize>();
            while cache.images.len() >= CACHE || (!cache.images.is_empty() && held(&cache) > CACHE_BYTES) {
                let Some(old) = cache.order.pop_front() else { break };
                cache.images.remove(&old);
            }
            if cache.images.insert(key, (b64.clone(), decode(b64))).is_none() {
                cache.order.push_back(key);
            }
        }
        let Some((_, Some(img))) = cache.images.get(&key) else { return };
        let t = Transform::from_row(w / img.width() as f32, 0.0, 0.0, h / img.height() as f32, x0.min(x1), y0.min(y1));
        let paint = PixmapPaint { opacity, quality: FilterQuality::Bilinear, ..PixmapPaint::default() };
        pm.draw_pixmap(0, 0, img.as_ref(), &paint, t, None);
    });
}

/// Runs `f` on picture marker `p`'s decoded image and its height for its width; `None` if it has
/// none or it doesn't decode.
fn with_marker<R>(p: Picture, f: impl FnOnce(&Pixmap, f32) -> R) -> Option<R> {
    if p.is_default() {
        return None;
    }
    MARKERS_DECODED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.images.contains_key(&p) {
            let img = marker_image(p, MARKER_SIDE);
            let size = img.as_ref().map_or(0, |(i, _)| i.data().len());
            // Evict the oldest (not all of them) to stay within the count and memory budget.
            while cache.images.len() >= MARKERS || (cache.bytes + size > MARKERS_BYTES && !cache.images.is_empty()) {
                let Some(old) = cache.order.pop_front() else { break };
                if let Some(Some((gone, _))) = cache.images.remove(&old) {
                    cache.bytes = cache.bytes.saturating_sub(gone.data().len());
                }
            }
            cache.bytes += size;
            cache.images.insert(p, img);
            cache.order.push_back(p);
        }
        cache.images.get(&p)?.as_ref().map(|(img, aspect)| f(img, *aspect))
    })
}

/// Picture marker `p`'s image at most `side` pixels per side, and its height for its width; `None`
/// where it's drawn as a circle (no image, whose base64 is empty, not a PNG or JPEG, or one that
/// doesn't decode).
pub(crate) fn marker_image(p: Picture, side: u32) -> Option<(Pixmap, f32)> {
    png_or_jpeg(p.base64()).then(|| decode_sized(p.base64(), side)).flatten()
}

/// Picture marker `p`'s height for its width; `None` if its image doesn't decode (it's drawn as a
/// circle).
pub(crate) fn marker_aspect(p: Picture) -> Option<f32> {
    with_marker(p, |_, aspect| aspect)
}

/// Draws picture marker `p` `width` pixels wide (as tall as its proportions make it), centred at
/// `t`'s origin and turned with it. False if its image doesn't decode.
pub(crate) fn draw_marker(pm: &mut Canvas, p: Picture, width: f32, t: Transform, opacity: f32) -> bool {
    with_marker(p, |img, aspect| {
        let height = width * aspect;
        let at = t.pre_translate(-width / 2.0, -height / 2.0).pre_scale(width / img.width() as f32, height / img.height() as f32);
        let paint = PixmapPaint { opacity, quality: FilterQuality::Bilinear, ..PixmapPaint::default() };
        pm.draw_pixmap(0, 0, img.as_ref(), &paint, at, None);
    })
    .is_some()
}

/// Whether base64 `b64` is a PNG or JPEG image that decodes (a picture marker's, before it's set).
pub fn readable(b64: &str) -> bool {
    png_or_jpeg(b64) && decode_within(b64, MARKER_SIDE).is_some()
}

/// Whether base64 `b64` starts as a PNG or a JPEG does. Picture markers take only those, on every
/// platform: the desktop's decoder also reads GIF (the engine's `gif` feature, for raster files),
/// the web's doesn't, so a GIF marker would draw on one and not the other. Graphics' pictures
/// ([`decode`]) keep what the decoder reads.
fn png_or_jpeg(b64: &str) -> bool {
    // 16 characters of base64 are 12 bytes, more than either signature.
    let b64 = b64.trim().as_bytes();
    let head = b64.get(..16).unwrap_or(b64);
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(head) else { return false };
    matches!(image::guess_format(&bytes), Ok(image::ImageFormat::Png | image::ImageFormat::Jpeg))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Picture markers take PNG and JPEG alone, read from the first bytes: not a GIF (which the
    /// desktop's decoder reads and the web's doesn't), nor what isn't base64 or an image.
    #[test]
    fn a_marker_is_a_png_or_a_jpeg() {
        let encoded = |format: image::ImageFormat| {
            let mut bytes = Vec::new();
            image::RgbImage::from_pixel(4, 2, image::Rgb([10, 20, 30])).write_to(&mut std::io::Cursor::new(&mut bytes), format).unwrap();
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        };
        let (png, jpeg) = (encoded(image::ImageFormat::Png), encoded(image::ImageFormat::Jpeg));
        assert!(png_or_jpeg(&png) && png_or_jpeg(&format!(" {jpeg}\n")));
        assert!(readable(&png) && readable(&jpeg));
        // A 1x1 GIF; a multi-byte character where the signature is read; text; base64 of text.
        let gif = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
        for not in [gif, "iVBORw0KGgoAAAAé", "", "%%%", "bm90IGFuIGltYWdl"] {
            assert!(!png_or_jpeg(not) && !readable(not), "{not}");
        }
    }

    /// A marker's image comes scaled down to the side asked for, keeping its proportions from
    /// before; one the map draws as a circle gives none.
    #[test]
    fn a_marker_image_fits_the_side_asked_for() {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(200, 50, image::Rgb([10, 20, 30]))
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let wide = Picture::new(&base64::engine::general_purpose::STANDARD.encode(&bytes));
        let (img, aspect) = marker_image(wide, 64).unwrap();
        assert_eq!((img.width(), img.height(), aspect), (64, 16, 0.25));
        // Smaller than the side: as it is.
        assert_eq!(marker_image(wide, 400).map(|(i, _)| (i.width(), i.height())), Some((200, 50)));
        let gif = Picture::new("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7");
        for none in [Picture::default(), gif, Picture::new("bm90IGFuIGltYWdl")] {
            assert!(marker_image(none, 64).is_none());
        }
    }

    /// Graphics' pictures don't check the signature: they keep what the decoder reads.
    #[test]
    fn graphics_pictures_still_decode_what_the_decoder_reads() {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(4, 2, image::Rgb([10, 20, 30]))
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let png = base64::engine::general_purpose::STANDARD.encode(&bytes);
        assert_eq!(decode(&png).map(|p| (p.width(), p.height())), Some((4, 2)));
        // A GIF, where the decoder reads one (the desktop's, with the engine's `gif` feature):
        // drawn as a graphic, though not taken as a marker.
        let gif = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
        assert_eq!(decode(gif).is_some(), image::ImageFormat::Gif.reading_enabled());
        assert!(!readable(gif));
    }
}
