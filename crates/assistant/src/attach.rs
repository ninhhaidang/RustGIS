//! Pictures the user attaches to a message: a reference map, a screenshot of a problem, a photo of
//! a sketch. [`prepare`] reads one whichever way it came (the pane's picker or a paste, the command
//! line's `--image`), and makes it ready to send to any provider: decoded within limits, turned
//! upright, drawn smaller when it is larger than models read, and encoded again, which leaves out
//! whatever the file carried besides its pixels (a photo's location among them).
//!
//! Plain functions without I/O, as the rest of the crate: the front end reads the file.

use std::io::Cursor;

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

use crate::provider::Note;

/// The largest file a picture is read from; a front end refuses a larger one before reading it.
pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;

/// The most pictures one message takes.
pub const MAX_PER_MESSAGE: usize = 4;

/// The longest side a picture is sent at: what Claude reads before it scales a picture down
/// itself, and within what the other providers take.
pub const LONGEST_SIDE: u32 = 1568;

/// The largest picture read: a phone's 50-megapixel photo fits, and decoding one never takes more
/// than [`MAX_DECODE_BYTES`].
const MAX_DECODED_SIDE: u32 = 8192;
const MAX_DECODE_BYTES: u64 = 320 * 1024 * 1024;

/// The most a picture sent may weigh, as `view_map`'s: 5 MiB once base64-encoded.
const MAX_SENT_BYTES: usize = crate::tools::MAX_PNG_BYTES;

/// The longest name a picture keeps, in characters.
const MAX_NAME: usize = 80;

/// The file types a picture is read from, for a picker's filter.
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp"];

/// How a picture's caption starts ([`Picture::caption`]).
pub const CAPTION: &str = "[a picture the user attached, ";

/// A picture ready to go with the user's message.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    /// The file it came from (or "pasted picture"), for the model and the transcript.
    pub name: String,
    /// The picture as sent: a PNG when it has transparency, else a JPEG.
    pub data: Vec<u8>,
    pub mime: &'static str,
    /// Its size in pixels as sent.
    pub width: u32,
    pub height: u32,
    /// Its size in pixels as given, larger when it was drawn smaller to send.
    pub original: (u32, u32),
}

impl Picture {
    /// What stands in its place in the conversation once it's no longer sent.
    pub fn caption(&self) -> String {
        format!("{CAPTION}{}, {} × {}, no longer attached]", self.name, self.width, self.height)
    }

    /// Whether it is sent smaller than it was given.
    pub fn shrunk(&self) -> bool {
        (self.width, self.height) != self.original
    }

    pub(crate) fn base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&self.data)
    }

    pub(crate) fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.base64())
    }
}

/// A picture from the bytes of a PNG, JPEG or WebP file named `name`. Refused, saying why in a
/// [`Note`] the interface translates: a file over [`MAX_FILE_BYTES`], another format, one that
/// doesn't decode, or one over [`MAX_DECODED_SIDE`] pixels a side.
///
/// Decoding is the long step, one with no check for Cancel (up to about a second for the largest
/// photo): the desktop runs it off the window's thread. It holds the file, the decoded picture
/// (at most [`MAX_DECODE_BYTES`]) and the picture drawn small, never another full copy.
pub fn prepare(name: &str, bytes: &[u8]) -> Result<Picture, Note> {
    prepare_within(name, bytes, MAX_DECODE_BYTES)
}

/// [`prepare`] decoding in at most `memory` bytes.
pub(crate) fn prepare_within(name: &str, bytes: &[u8], memory: u64) -> Result<Picture, Note> {
    let name = clean_name(name);
    if bytes.len() > MAX_FILE_BYTES {
        return Err(too_large(name));
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| Note::with("{name}: the picture can't be read ({why})", [("name", name.clone()), ("why", e.to_string())]))?;
    if !matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)) {
        return Err(Note::with("{name}: a picture must be a PNG, JPEG or WebP file", [("name", name)]));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODED_SIDE);
    limits.max_image_height = Some(MAX_DECODED_SIDE);
    limits.max_alloc = Some(memory);
    reader.limits(limits);
    // Past the limits: too many pixels a side, or, within them, more memory than a picture may
    // take to read (16 bits a channel, say), each said as it is.
    let too_deep = || {
        Note::with(
            "{name}: the picture needs more than {limit} MiB to read",
            [("name", name.clone()), ("limit", (memory >> 20).to_string())],
        )
    };
    let unreadable = |e: image::ImageError| match e {
        image::ImageError::Limits(_) if !header_fits(bytes) => too_many_pixels(name.clone()),
        image::ImageError::Limits(_) => too_deep(),
        e => Note::with("{name}: the picture can't be read ({why})", [("name", name.clone()), ("why", e.to_string())]),
    };
    let mut decoder = reader.into_decoder().map_err(unreadable)?;
    // A photo taken with the phone on its side says so. A note that can't be read leaves the
    // picture as it was stored, which is how it is shown without the note.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    // The pixels' buffer is allocated whole before they are read, outside the decoder's limits:
    // its size is checked here (a 16-bit picture takes twice an 8-bit one's).
    if decoder.total_bytes() > memory {
        return Err(too_deep());
    }
    let img = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    finish(name, img, orientation)
}

/// A picture from `width` × `height` pixels of RGBA, row by row (a pasted screenshot). The size
/// is checked first: [`fits`] says whether pixels of that size are taken, before they are copied.
pub fn from_rgba(name: &str, width: u32, height: u32, rgba: Vec<u8>) -> Result<Picture, Note> {
    let name = clean_name(name);
    if !fits(width, height) {
        return Err(too_many_pixels(name));
    }
    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| Note::with("{name}: the picture's pixels don't match its size", [("name", name.clone())]))?;
    finish(name, DynamicImage::ImageRgba8(img), Orientation::NoTransforms)
}

/// Whether the picture whose file is `bytes` says it has at most [`MAX_DECODED_SIDE`] pixels a
/// side (read from its header alone); one whose header can't be read doesn't.
fn header_fits(bytes: &[u8]) -> bool {
    ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok()).is_some_and(|(w, h)| fits(w, h))
}

/// Whether a picture of `width` × `height` pixels is taken.
pub fn fits(width: u32, height: u32) -> bool {
    width <= MAX_DECODED_SIDE && height <= MAX_DECODED_SIDE
}

/// The refusal of a file over [`MAX_FILE_BYTES`], as every front end says it.
pub fn too_large(name: String) -> Note {
    Note::with("{name}: a picture can be at most {limit} MiB", [("name", name), ("limit", (MAX_FILE_BYTES >> 20).to_string())])
}

/// The refusal of a picture over [`MAX_DECODED_SIDE`] pixels a side.
pub fn too_many_pixels(name: String) -> Note {
    Note::with("{name}: a picture can be at most {limit} pixels a side", [("name", name), ("limit", MAX_DECODED_SIDE.to_string())])
}

/// Draws `img` no larger than [`LONGEST_SIDE`], turns it as `orientation` says (once small, so a
/// large picture isn't copied whole to turn it) and encodes it, smaller again while it weighs
/// more than [`MAX_SENT_BYTES`].
fn finish(name: String, img: DynamicImage, orientation: Orientation) -> Result<Picture, Note> {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(Note::with("{name}: the picture is empty", [("name", name)]));
    }
    // Its size as it is seen, upright.
    let turned =
        matches!(orientation, Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH);
    let original = if turned { (h, w) } else { (w, h) };
    // Each smaller try is drawn from the last one, not from the whole picture, which is let go
    // once drawn small; turning upright is done on the small picture.
    let shrink = |from: &DynamicImage, side: u32| {
        if from.width().max(from.height()) > side { from.resize(side, side, image::imageops::FilterType::Triangle) } else { from.clone() }
    };
    let upright = |small: &DynamicImage| {
        let mut turned = small.clone();
        turned.apply_orientation(orientation);
        turned
    };
    let mut side = LONGEST_SIDE;
    let mut small = shrink(&img, side);
    drop(img);
    let mut fitted = upright(&small);
    // Opaque pixels go as a JPEG, far lighter for a photo; transparency needs a PNG. Looked at
    // once drawn small, so a large picture isn't copied whole to look.
    let alpha = fitted.color().has_alpha() && fitted.to_rgba8().pixels().any(|p| p.0[3] < 255);
    loop {
        let (data, mime) = encode(&fitted, alpha)
            .map_err(|why| Note::with("{name}: the picture can't be encoded ({why})", [("name", name.clone()), ("why", why)]))?;
        if data.len() <= MAX_SENT_BYTES || side <= 256 {
            if data.len() > MAX_SENT_BYTES {
                return Err(Note::with("{name}: the picture is too heavy to send, even drawn small", [("name", name)]));
            }
            return Ok(Picture { name, data, mime, width: fitted.width(), height: fitted.height(), original });
        }
        side = side * 3 / 4;
        small = shrink(&small, side);
        fitted = upright(&small);
    }
}

/// The file at `path`, read no further than one byte past [`MAX_FILE_BYTES`]: [`prepare`] then
/// refuses it, and a huge file isn't read whole to be refused.
#[cfg(not(target_arch = "wasm32"))]
pub fn read(path: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
    Ok(bytes)
}

/// `img` as a PNG when `alpha` (it has transparent pixels), else as a JPEG at quality 85, with
/// its media type.
fn encode(img: &DynamicImage, alpha: bool) -> Result<(Vec<u8>, &'static str), String> {
    let mut out = Vec::new();
    if alpha {
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(|e| e.to_string())?;
        Ok((out, "image/png"))
    } else {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        img.to_rgb8().write_with_encoder(encoder).map_err(|e| e.to_string())?;
        Ok((out, "image/jpeg"))
    }
}

/// The file's name without its folders or control characters, at most [`MAX_NAME`] characters.
fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let clean = clean.trim();
    if clean.is_empty() { "picture".into() } else { clean.into() }
}
