//! Picture graphics: PNG or JPEG images (base64 in the graphic's `image` attribute) stretched over
//! the graphic's rectangle. And picture markers ([`Picture`]), drawn at a point.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::sync::{Arc, Mutex, OnceLock};

use crate::Canvas;
use crate::vector::Pixels;
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
/// The most pixels a picture marker's image may declare (2048 × 2048, by area, so a long strip
/// passes): past it, it isn't decoded and is drawn as a circle. What a marker costs to check and
/// draw is its pixels (decoding and scaling one takes some 19 ns a pixel), not its bytes: a PNG of
/// a few kilobytes can declare hundreds of megapixels. So the cap is set by that cost, one at it
/// taking some 80 ms; a marker is drawn from at most [`MARKER_SIDE`] pixels a side whatever its
/// image's size.
pub const MARKER_PIXELS: u64 = MARKER_PIXELS_SIDE as u64 * MARKER_PIXELS_SIDE as u64;
/// The side of the square [`MARKER_PIXELS`] is, for messages (theirs, written out, a test pins).
pub const MARKER_PIXELS_SIDE: u32 = 2048;
/// The most pixels of different picture markers' images a run decodes, about 5 s of decoding;
/// with [`RUN_MARKER_BYTES`], past either, a new one is drawn as a circle. A run is the process,
/// until RustGIS restarts, as the images it keeps ([`Picture`]) are. A cap per image
/// alone isn't enough: a run keeps [`rustgis_map::symbology::MAX_PICTURES`] images, and that many
/// at [`MARKER_PIXELS`] take some 80 s. Each image is decoded once for the whole run, on whichever
/// thread meets it first, and kept ([`Store`]): none is decoded again, however many threads, frames
/// or checks meet it, so these bound all the decoding markers do. As many images as a run keeps
/// fit when each is at most 177 × 177 pixels once decoded (a larger one is scaled down to
/// [`MARKER_SIDE`]).
pub const RUN_MARKER_PIXELS: u64 = 256_000_000;
/// The most memory picture markers' images hold decoded before a new one is drawn as a circle.
/// Their copies compressed for vector exports are charged to it too, leaving less for new images,
/// but never refused ([`Decoded::packed`]): at most about as much again, some 256 MiB in all.
pub const RUN_MARKER_BYTES: u64 = 128 << 20;
/// What each image is charged at least (an icon's decoding costs about the same whatever its
/// size), so the images charged are at most [`RUN_MARKER_PIXELS`] / this many.
const MARKER_CHARGE: u64 = 64 * 64;

/// Why a picture marker's image is drawn as a circle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerRefusal {
    /// It isn't a PNG or JPEG image that decodes.
    NotAnImage,
    /// It declares more than [`MARKER_PIXELS`].
    TooManyPixels,
    /// It would take the run past [`RUN_MARKER_PIXELS`] or [`RUN_MARKER_BYTES`].
    PastRunPixels,
}

impl MarkerRefusal {
    /// Whether it lasts whatever the run: the image itself is the reason, not what the run spent
    /// before meeting it ([`MarkerRefusal::PastRunPixels`], which a restart clears). An image kept
    /// already and refused only for the run is kept as it is, drawn as a circle meanwhile.
    pub const fn lasts(self) -> bool {
        !matches!(self, MarkerRefusal::PastRunPixels)
    }
}

/// Each refusal's phrase, written once for both its forms.
macro_rules! refusal_phrases {
    ($($refusal:ident => $what:literal),* $(,)?) => {
        impl MarkerRefusal {
            /// What is wrong with the image, after "the image" or "this one":
            /// `"has more pixels than 2048 × 2048"`.
            pub const fn what(self) -> &'static str {
                match self {
                    $(MarkerRefusal::$refusal => $what),*
                }
            }

            /// [`MarkerRefusal::what`] after "whose image", for a list of what an import couldn't
            /// carry over.
            pub const fn whose(self) -> &'static str {
                match self {
                    $(MarkerRefusal::$refusal => concat!("whose image ", $what)),*
                }
            }
        }
    };
}

refusal_phrases! {
    NotAnImage => "isn't a PNG or JPEG RustGIS reads",
    TooManyPixels => "has more pixels than 2048 × 2048",
    PastRunPixels => "is past what RustGIS decodes of marker images until it restarts (256 megapixels, 128 MiB)",
}

/// Decoded pictures (with the base64 they came from, compared on every hit so pictures whose
/// cheap keys collide never swap), and their keys oldest first (for eviction).
#[derive(Default)]
struct Cache {
    /// Shared, so a frame drawn in bands keeps the picture, not a copy ([`Canvas::draw_shared`]).
    images: HashMap<u64, (String, Option<Arc<Pixmap>>)>,
    order: std::collections::VecDeque<u64>,
}

thread_local! {
    static DECODED: RefCell<Cache> = RefCell::new(Cache::default());
    /// The picture markers this thread has met, each with its image in the [`Store`] or why it is
    /// drawn as a circle, so a frame finds it with no lock. Never emptied: only pictures a run keeps
    /// ([`rustgis_map::symbology::MAX_PICTURES`]) are met, and each entry shares the store's image.
    static MARKERS_MET: RefCell<HashMap<Picture, Marker>> = RefCell::new(HashMap::new());
}

/// A picture marker's image, decoded once for the run and shared by every thread.
struct Decoded {
    /// At most [`MARKER_SIDE`] pixels a side. Shared, so a frame drawn in bands keeps it, not a
    /// copy ([`Canvas::draw_shared`]).
    image: Arc<Pixmap>,
    /// Its height for its width, from before it was scaled down (a one-pixel side can't keep a very
    /// narrow image's).
    aspect: f32,
    /// The image compressed for recording canvases (vector exports), the first time one records
    /// it: every point then shares it, and a writer keeps it once. Charged to the run's bytes
    /// ([`pack`]), leaving less for new images, but never refused: without it each place would
    /// compress a copy of its own. At most about as much again as the decoded copies.
    packed: OnceLock<Option<Arc<Pixels>>>,
}

/// A picture marker's decoded image, or why it is drawn as a circle.
type Marker = Result<Arc<Decoded>, MarkerRefusal>;

/// What the run has decoded of picture markers' images: the pixels they declared and the bytes
/// they hold, decoded or compressed ([`RUN_MARKER_PIXELS`], [`RUN_MARKER_BYTES`]: compressed
/// copies can take the bytes past it).
#[derive(Default)]
struct Budget {
    pixels: u64,
    bytes: u64,
}

impl Budget {
    /// Charges an image declaring `pixels` (at least [`MARKER_CHARGE`]) and holding `bytes`
    /// decoded, within `limits`; refused, charging nothing, past either.
    fn take(&mut self, pixels: u64, bytes: u64, (max_pixels, max_bytes): (u64, u64)) -> Result<(), MarkerRefusal> {
        let pixels = pixels.max(MARKER_CHARGE);
        if self.pixels.saturating_add(pixels) > max_pixels || self.bytes.saturating_add(bytes) > max_bytes {
            return Err(MarkerRefusal::PastRunPixels);
        }
        self.pixels += pixels;
        self.bytes += bytes;
        Ok(())
    }

    /// Charges `bytes` more held for an image already charged (its compressed copy), whatever is
    /// left: they then leave less for new images.
    fn hold(&mut self, bytes: u64) {
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

/// The picture markers' images decoded this run, by their whole base64 ([`content_id`]): each one
/// charged to the [`Budget`], decoded or not, so it is never decoded again. One kept
/// ([`Picture`]) and one only checked (before a command keeps it) are the same image here.
#[derive(Default)]
struct Store {
    images: HashMap<u128, Marker>,
    /// What each kept picture met so far came to, by where it is kept: found with no hashing of
    /// its image. A frame drawn in bands runs on threads made for that frame
    /// ([`crate::bands`]), whose [`MARKERS_MET`] start empty, so each thread's first look at a
    /// picture comes here, every frame. At most the pictures a run keeps. A refusal is kept too: a
    /// picture past the run's budget stays a circle for the run, even if a later image that
    /// doesn't decode gives back bytes, so a map doesn't change with what else was met.
    pictures: HashMap<Picture, Marker>,
    budget: Budget,
}

/// The [`Store`]. Its lock is held while an image is decoded, so two threads meeting the same new
/// image decode it once; another waits at most one image's decoding (80 ms at [`MARKER_PIXELS`]).
fn store() -> std::sync::MutexGuard<'static, Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Base64 `b64`'s identity in the [`Store`]: two hashes of all of it with different keys, 128 bits
/// together. Not the cheap [`key`] of the graphics' cache, which a crafted image could share with
/// one decoded already to pass free; and not a single 64-bit hash, in reach of a birthday search
/// where the keys are known (the web build may not randomize them; two keys still take 2^64).
fn content_id(b64: &str) -> u128 {
    static HASHERS: OnceLock<(RandomState, RandomState)> = OnceLock::new();
    let (a, b) = HASHERS.get_or_init(|| (RandomState::new(), RandomState::new()));
    (u128::from(a.hash_one(b64)) << 64) | u128::from(b.hash_one(b64))
}

/// Picture marker image `b64` from the [`Store`], decoded there the first time; or why it is drawn
/// as a circle: not a PNG or JPEG, past [`MARKER_PIXELS`] or what the run has left of
/// [`RUN_MARKER_PIXELS`] or [`RUN_MARKER_BYTES`], or one that doesn't decode. Every path to a
/// marker's image (checking one, drawing it, a preview's) comes here, so the caps hold for all of
/// them; graphics' pictures ([`decode`]) keep their own. What is refused before decoding (from its
/// first bytes or its header) isn't kept: finding that again costs as little.
///
/// Commands and a project's read check images on the UI thread. One image takes at most ~80 ms
/// ([`MARKER_PIXELS`] at ~19 ns a pixel); everything a run decodes, ~5 s ([`RUN_MARKER_PIXELS`]),
/// once: one call or file can spend it all, and after that a new image is refused from its header.
/// Kept on the UI thread for that bound: the check answers whether the command is taken.
fn marker(b64: &str) -> Marker {
    marker_in(&mut store(), b64)
}

/// [`marker`] in `store`, locked.
fn marker_in(store: &mut Store, b64: &str) -> Marker {
    use MarkerRefusal::*;
    let b64 = b64.trim();
    if !png_or_jpeg(b64) {
        return Err(NotAnImage);
    }
    let id = content_id(b64);
    if let Some(found) = store.images.get(&id) {
        return found.clone();
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| NotAnImage)?;
    let (w, h) = declared(&bytes).ok_or(NotAnImage)?;
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MARKER_PIXELS {
        return Err(TooManyPixels);
    }
    // What it holds once decoded, scaled down to fit `MARKER_SIDE` as `decode_bytes` does (settled
    // below to what it does hold, should the data not be what the header says).
    let (fw, fh) = fitted(w, h, MARKER_SIDE);
    let held = u64::from(fw) * u64::from(fh) * 4;
    store.budget.take(pixels, held, (RUN_MARKER_PIXELS, RUN_MARKER_BYTES))?;
    let found = decode_bytes(&bytes, MARKER_SIDE)
        .map(|(image, aspect)| Arc::new(Decoded { image: Arc::new(image), aspect, packed: OnceLock::new() }))
        .ok_or(NotAnImage);
    // Settled to what it holds: nothing for one that doesn't decode. Its pixels stay charged: one
    // can fail at its last row, having taken the time they stand for, and charging less would let
    // such images take that time again and again. One that fails sooner charges as much, as a
    // valid image declaring as many pixels would; the cap bounds the time either way.
    let holds = found.as_ref().map_or(0, |d| d.image.data().len() as u64);
    store.budget.bytes = store.budget.bytes.saturating_sub(held).saturating_add(holds);
    store.images.insert(id, found.clone());
    found
}

/// Picture marker `p` as this thread met it ([`MARKERS_MET`]), from the [`Store`] the first time.
fn marker_met(p: Picture) -> Marker {
    if p.is_default() {
        return Err(MarkerRefusal::NotAnImage);
    }
    if let Some(found) = MARKERS_MET.with(|met| met.borrow().get(&p).cloned()) {
        return found;
    }
    let found = {
        let mut store = store();
        match store.pictures.get(&p) {
            Some(found) => found.clone(),
            None => {
                let found = marker_in(&mut store, p.base64());
                store.pictures.insert(p, found.clone());
                found
            }
        }
    };
    MARKERS_MET.with(|met| met.borrow_mut().insert(p, found.clone()));
    found
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

/// The pixel size of base64 PNG or JPEG bytes, read from the header without decoding the image;
/// `None` if it doesn't say, or says 0.
pub fn size(b64: &str) -> Option<(u32, u32)> {
    declared(&base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?)
}

/// Decodes base64 PNG or JPEG bytes into a premultiplied pixmap, scaled down to at most
/// [`MAX_SIDE`] pixels per side. Pictures come from project files, so their declared size is
/// checked before decoding and the decoder's memory is capped.
pub fn decode(b64: &str) -> Option<Pixmap> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    let (w, h) = declared(&bytes)?;
    if w > MAX_DECLARED || h > MAX_DECLARED {
        return None;
    }
    decode_bytes(&bytes, MAX_SIDE).map(|(pm, _)| pm)
}

/// The pixel size image `bytes` declare, from their header; `None` if they don't say or say 0.
fn declared(bytes: &[u8]) -> Option<(u32, u32)> {
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// The size `w` × `h` pixels scale down to so neither side passes `side`, keeping their
/// proportions (each side at least one pixel); the same where they fit.
fn fitted(w: u32, h: u32, side: u32) -> (u32, u32) {
    if w <= side && h <= side {
        return (w, h);
    }
    let (long, short) = (u64::from(w.max(h)), u64::from(w.min(h)));
    let short = ((short * u64::from(side) + long / 2) / long).clamp(1, u64::from(side.max(1))) as u32;
    if w >= h { (side, short) } else { (short, side) }
}

/// Decodes image `bytes`, whose declared size the caller has checked, scaled down to at most
/// `side` pixels per side ([`fitted`], which a marker's charge is reckoned with), with their height
/// for their width from before.
fn decode_bytes(bytes: &[u8], side: u32) -> Option<(Pixmap, f32)> {
    let mut r = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    r.limits(limits);
    let img = r.decode().ok()?;
    let aspect = img.height() as f32 / img.width().max(1) as f32;
    let (w, h) = fitted(img.width(), img.height(), side);
    let img = if (w, h) != (img.width(), img.height()) { img.thumbnail_exact(w, h) } else { img };
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
            if cache.images.insert(key, (b64.clone(), decode(b64).map(Arc::new))).is_none() {
                cache.order.push_back(key);
            }
        }
        let Some((_, Some(img))) = cache.images.get(&key) else { return };
        let t = Transform::from_row(w / img.width() as f32, 0.0, 0.0, h / img.height() as f32, x0.min(x1), y0.min(y1));
        let paint = PixmapPaint { opacity, quality: FilterQuality::Bilinear, ..PixmapPaint::default() };
        pm.draw_shared(img, None, &paint, t);
    });
}

/// Runs `f` on picture marker `p`'s decoded image, its height for its width and, with `pack`, the
/// image compressed (see [`Decoded::packed`]); or why it is drawn as a circle.
fn with_marker<R>(p: Picture, pack: bool, f: impl FnOnce(&Arc<Pixmap>, f32, Option<&Arc<Pixels>>) -> R) -> Result<R, MarkerRefusal> {
    let found = marker_met(p)?;
    let charge = |bytes| store().budget.hold(bytes);
    let packed = if pack { found.packed.get_or_init(|| self::pack(&found.image, charge)).as_ref() } else { None };
    Ok(f(&found.image, found.aspect, packed))
}

/// Marker image `image` compressed ([`Decoded::packed`]) and its bytes charged with `charge`
/// (after compressing, so the [`Store`] isn't held meanwhile); `None` where it can't be.
fn pack(image: &Pixmap, charge: impl FnOnce(u64)) -> Option<Arc<Pixels>> {
    let packed = Pixels::new(image.as_ref())?;
    charge(packed.cost() as u64);
    Some(Arc::new(packed))
}

/// Picture marker `p`'s image at most `side` pixels per side (a preview's), and its height for its
/// width; `None` where it's drawn as a circle. Scaled from the run's decoded copy by averaging
/// (the image isn't decoded again), so a much smaller side doesn't alias; a new copy each call.
pub(crate) fn marker_image(p: Picture, side: u32) -> Option<(Pixmap, f32)> {
    let found = marker_met(p).ok()?;
    let (w, h) = (found.image.width(), found.image.height());
    if w <= side && h <= side {
        return Some(((*found.image).clone(), found.aspect));
    }
    let scale = side.max(1) as f32 / w.max(h) as f32;
    let (sw, sh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    // Averaging premultiplied pixels keeps them premultiplied.
    let view = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(w, h, found.image.data())?;
    let small = image::imageops::thumbnail(&view, sw, sh);
    let pm = Pixmap::from_vec(small.into_raw(), tiny_skia::IntSize::from_wh(sw, sh)?)?;
    Some((pm, found.aspect))
}

/// Picture marker `p`'s height for its width; or why it is drawn as a circle.
pub(crate) fn marker_drawn(p: Picture) -> Result<f32, MarkerRefusal> {
    marker_met(p).map(|found| found.aspect)
}

/// Why kept picture marker `p` can't be given again: a refusal that lasts whatever the run
/// ([`MarkerRefusal::lasts`]). One drawn as a circle only for what the run has spent is kept as it
/// is, to draw after a restart. Commands and the symbol editors alike go by this.
pub(crate) fn marker_kept_refusal(p: Picture) -> Option<MarkerRefusal> {
    lasting(&marker_met(p))
}

/// [`marker_kept_refusal`] of what a picture met came to.
fn lasting(found: &Marker) -> Option<MarkerRefusal> {
    found.as_ref().err().copied().filter(|why| why.lasts())
}

/// [`marker_drawn`], `None` where it is drawn as a circle.
pub(crate) fn marker_aspect(p: Picture) -> Option<f32> {
    marker_drawn(p).ok()
}

/// Draws picture marker `p` `width` pixels wide (as tall as its proportions make it), centred at
/// `t`'s origin and turned with it. False if its image doesn't decode.
pub(crate) fn draw_marker(pm: &mut Canvas, p: Picture, width: f32, t: Transform, opacity: f32) -> bool {
    // Compressed once for every point of a vector export.
    let pack = pm.is_recording();
    with_marker(p, pack, |img, aspect, packed| {
        let height = width * aspect;
        let at = t.pre_translate(-width / 2.0, -height / 2.0).pre_scale(width / img.width() as f32, height / img.height() as f32);
        let paint = PixmapPaint { opacity, quality: FilterQuality::Bilinear, ..PixmapPaint::default() };
        pm.draw_shared(img, packed, &paint, at);
    })
    .is_ok()
}

/// Base64 `b64` as a picture marker's image (one already kept, or one before it's set): its
/// height for its width, or why it would be drawn as a circle. Decoded into the run's store, where
/// drawing it finds it.
pub fn check(b64: &str) -> Result<f32, MarkerRefusal> {
    match Picture::kept(b64) {
        Some(kept) => marker_drawn(kept),
        None => marker(b64).map(|found| found.aspect),
    }
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
        assert!(check(&png).is_ok() && check(&jpeg).is_ok());
        // A 1x1 GIF; a multi-byte character where the signature is read; text; base64 of text.
        let gif = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
        for not in [gif, "iVBORw0KGgoAAAAé", "", "%%%", "bm90IGFuIGltYWdl"] {
            assert!(!png_or_jpeg(not) && check(not) == Err(MarkerRefusal::NotAnImage), "{not}");
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

    fn png(w: u32, h: u32) -> String {
        let mut bytes = Vec::new();
        image::GrayImage::from_pixel(w, h, image::Luma([90]))
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    }

    /// A one-pixel PNG whose header says `w` × `h`: the size of a crafted image whose few bytes
    /// would decode to far more.
    fn declaring(w: u32, h: u32) -> String {
        let mut bytes = base64::engine::general_purpose::STANDARD.decode(png(1, 1)).unwrap();
        // The header chunk: its length, "IHDR", width and height, …, and its CRC over type and data.
        bytes[16..20].copy_from_slice(&w.to_be_bytes());
        bytes[20..24].copy_from_slice(&h.to_be_bytes());
        let crc = bytes[12..29].iter().fold(!0u32, |mut c, &b| {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
            c
        });
        bytes[29..33].copy_from_slice(&(!crc).to_be_bytes());
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    }

    /// A marker's image is taken up to 2048 × 2048 pixels by area (a long strip too), and one past
    /// it is refused from its header, before it is decoded: one declaring 11,000 × 11,000 with a
    /// pixel's bytes says so, not that it doesn't decode. Graphics' pictures keep their own limit.
    #[test]
    fn a_marker_image_past_its_pixels_is_refused_before_it_is_decoded() {
        assert_eq!(check(&png(2048, 2048)), Ok(1.0));
        assert_eq!(check(&png(4096, 1024)), Ok(0.25));
        assert_eq!(check(&png(2049, 2048)), Err(MarkerRefusal::TooManyPixels));
        let crafted = declaring(11_000, 11_000);
        assert_eq!(size(&crafted), Some((11_000, 11_000)));
        assert_eq!(check(&crafted), Err(MarkerRefusal::TooManyPixels));
        assert!(marker_image(Picture::new(&crafted), 64).is_none());
        // A picture graphic as large is still drawn.
        assert_eq!(decode(&png(2049, 2048)).map(|p| (p.width(), p.height())), Some((2049, 2048)));
    }

    /// The run's budget: an image charged at least an icon's worth, refused (charging nothing) past
    /// its pixels or its bytes, so a smaller one still fits.
    #[test]
    fn a_run_decodes_marker_images_within_its_pixels_and_bytes() {
        let mut b = Budget::default();
        let limits = (10 * MARKER_CHARGE, 1000);
        assert_eq!(b.take(8 * MARKER_CHARGE, 100, limits), Ok(()));
        assert_eq!(b.take(3 * MARKER_CHARGE, 100, limits), Err(MarkerRefusal::PastRunPixels));
        assert_eq!(b.take(MARKER_CHARGE, 901, limits), Err(MarkerRefusal::PastRunPixels), "past its bytes");
        assert_eq!((b.pixels, b.bytes), (8 * MARKER_CHARGE, 100), "a refused one charges nothing");
        // Two icons of a pixel are charged an icon's worth each, the second filling the run.
        assert_eq!(b.take(1, 4, limits), Ok(()));
        assert_eq!(b.take(1, 4, limits), Ok(()));
        assert_eq!(b.take(1, 4, limits), Err(MarkerRefusal::PastRunPixels));
        // What a run keeps fits, at 177 × 177 each.
        let keeps = rustgis_map::symbology::MAX_PICTURES as u64;
        assert!(keeps * 177 * 177 <= RUN_MARKER_PIXELS && keeps * 177 * 177 * 4 <= RUN_MARKER_BYTES);
        // A compressed copy is charged on top, even past the bytes, leaving none for new images.
        b.hold(8);
        b.hold(2000);
        assert_eq!(b.bytes, 2116);
        assert_eq!(b.take(1, 4, limits), Err(MarkerRefusal::PastRunPixels));
    }

    /// An image is charged what it holds once scaled down, with its proportions: a wide one is
    /// charged its strip, not a square, so as many fit as hold the run's bytes.
    #[test]
    fn a_marker_image_is_charged_what_it_holds_scaled_down() {
        assert_eq!(fitted(1400, 700, 700), (700, 350));
        assert_eq!(fitted(700, 2048, 700), (239, 700));
        assert_eq!(fitted(4096, 1, 700), (700, 1));
        // 31.5 rows, rounded up here and by the decoding alike.
        assert_eq!(fitted(2000, 90, 700), (700, 32));
        assert_eq!(fitted(300, 200, 700), (300, 200));
        let mut store = Store::default();
        let wide = png(1400, 700);
        let found = marker_in(&mut store, &wide).unwrap();
        assert_eq!((found.image.width(), found.image.height()), (700, 350));
        assert_eq!(store.budget.bytes, 700 * 350 * 4);
        // It fits where only its strip is left, which a square's charge wouldn't.
        let mut store = Store::default();
        store.budget.bytes = RUN_MARKER_BYTES - 700 * 350 * 4;
        assert!(marker_in(&mut store, &wide).is_ok());
        assert_eq!(store.budget.bytes, RUN_MARKER_BYTES);
        // So does one whose height rounds at half a pixel: decoded the size it was charged.
        let mut store = Store::default();
        store.budget.bytes = RUN_MARKER_BYTES - 700 * 32 * 4;
        let found = marker_in(&mut store, &png(2000, 90)).unwrap();
        assert_eq!((found.image.width(), found.image.height()), (700, 32));
        assert_eq!(store.budget.bytes, RUN_MARKER_BYTES);
    }

    /// A marker image's compressed copy is charged to the run's bytes, and kept past them (each
    /// place would otherwise compress its own).
    #[test]
    fn a_marker_image_compressed_is_charged_to_the_run() {
        let mut image = Pixmap::new(64, 64).unwrap();
        image.fill(tiny_skia::Color::from_rgba8(200, 30, 30, 255));
        let mut budget = Budget::default();
        let packed = pack(&image, |b| budget.hold(b)).expect("compressed");
        assert_eq!(budget.bytes, packed.cost() as u64);
        budget.bytes = RUN_MARKER_BYTES;
        assert!(pack(&image, |b| budget.hold(b)).is_some());
        assert_eq!(budget.bytes, RUN_MARKER_BYTES + packed.cost() as u64);
    }

    /// Only the run's refusal passes with a restart: an image kept already keeps it.
    #[test]
    fn only_the_runs_refusal_passes() {
        use MarkerRefusal::*;
        assert_eq!([NotAnImage, TooManyPixels, PastRunPixels].map(MarkerRefusal::lasts), [true, true, false]);
        // A kept image the run's spent budget refuses is kept as it is; one refused for itself
        // isn't; one that draws is.
        let mut store = Store::default();
        let fine = png(8, 8);
        assert_eq!(lasting(&marker_in(&mut store, &fine)), None);
        assert_eq!(lasting(&marker_in(&mut store, "bm90IGFuIGltYWdl")), Some(NotAnImage));
        assert_eq!(lasting(&marker_in(&mut store, &declaring(2049, 2048))), Some(TooManyPixels));
        store.budget.pixels = RUN_MARKER_PIXELS;
        let spent = marker_in(&mut store, &png(9, 9));
        assert_eq!((spent.as_ref().err(), lasting(&spent)), (Some(&PastRunPixels), None));
    }

    /// A marker's image is decoded once for the run: the same image met on another thread, or
    /// checked as base64, is the same decoded copy, and a preview is scaled from it.
    #[test]
    fn a_marker_image_is_decoded_once_for_every_thread() {
        let p = Picture::new(&png(300, 150));
        let here = marker_met(p).unwrap();
        let there = std::thread::spawn(move || marker_met(p).unwrap()).join().unwrap();
        assert!(Arc::ptr_eq(&here, &there));
        assert!(Arc::ptr_eq(&here, &marker(p.base64()).unwrap()));
        assert_eq!(marker_image(p, 96).map(|(i, a)| (i.width(), i.height(), a)), Some((96, 48, 0.5)));
    }

    /// An image whose header is fine but whose data doesn't decode keeps its pixels charged (its
    /// decoding took that time) and gives back its bytes (it holds none); met again, it is found
    /// refused, charging nothing more.
    #[test]
    fn a_marker_image_that_does_not_decode_gives_back_its_bytes() {
        let mut store = Store::default();
        // A pixel's data under a header saying 100 × 100.
        let broken = declaring(100, 100);
        assert!(marker_in(&mut store, &broken).is_err_and(|r| r == MarkerRefusal::NotAnImage));
        assert_eq!((store.budget.pixels, store.budget.bytes), (100 * 100, 0));
        assert!(marker_in(&mut store, &broken).is_err());
        assert_eq!(store.budget.pixels, 100 * 100, "found, not charged again");
        let fine = png(100, 50);
        assert_eq!(marker_in(&mut store, &fine).map(|m| m.aspect), Ok(0.5));
        assert_eq!(store.budget.bytes, 100 * 50 * 4);
    }

    /// The refusals' phrases name the caps they are about.
    #[test]
    fn refusals_name_their_caps() {
        let side = MARKER_PIXELS_SIDE.to_string();
        assert!(MarkerRefusal::TooManyPixels.what().contains(&format!("{side} × {side}")));
        let run = format!("({} megapixels, {} MiB)", RUN_MARKER_PIXELS / 1_000_000, RUN_MARKER_BYTES >> 20);
        assert!(MarkerRefusal::PastRunPixels.what().contains(&run));
        assert_eq!(MarkerRefusal::NotAnImage.whose(), "whose image isn't a PNG or JPEG RustGIS reads");
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
        assert!(check(gif).is_err());
    }
}
