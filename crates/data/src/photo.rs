//! Geotagged photos: the GPS position, direction, time and camera a JPEG's EXIF holds, and a small
//! thumbnail, as point features. EXIF is read here (a TIFF block of IFDs in the JPEG's APP1
//! segment); every offset and count comes from the file, so each is checked against the bytes.

use crate::{Coord, Dataset, Feature, Field, FieldType, Geometry, GeometryType, ReadReport, Value};
use base64::Engine;

/// The longest side of a photo's thumbnail, in pixels.
pub const THUMB_SIZE: u32 = 160;
/// Photos in one layer at most (each keeps a thumbnail of a few kilobytes).
pub const MAX_PHOTOS: usize = 2000;
/// The largest photo read.
pub const MAX_BYTES: usize = 64 << 20;
/// Entries read from one IFD at most.
const MAX_ENTRIES: usize = 1000;
/// JPEG segments looked at before the image data, at most.
const MAX_SEGMENTS: usize = 256;

/// What a photo's EXIF says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exif {
    /// Longitude and latitude (WGS 84), when it holds a GPS position.
    pub position: Option<(f64, f64)>,
    /// Metres above sea level (below, negative).
    pub altitude: Option<f64>,
    /// Which way the camera faced, degrees from north.
    pub direction: Option<f64>,
    /// When it was taken (`YYYY-MM-DD HH:MM:SS`, the camera's clock), else the GPS time (UTC).
    pub time: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    /// How the picture is turned (EXIF orientation 1–8).
    pub orientation: u16,
}

/// Whether a file name is a photo RustGIS reads (a JPEG).
pub fn is_photo_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

/// The EXIF block (a TIFF header and its IFDs) of a JPEG, if it has one.
fn exif_block(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2usize;
    for _ in 0..MAX_SEGMENTS {
        // Fill bytes before a marker.
        while bytes.get(at) == Some(&0xFF) && bytes.get(at.checked_add(1)?) == Some(&0xFF) {
            at = at.checked_add(1)?;
        }
        if bytes.get(at)? != &0xFF {
            return None;
        }
        let marker = *bytes.get(at.checked_add(1)?)?;
        // Start of scan, or end of image: no EXIF before the picture.
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        // Markers without a length.
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            at = at.checked_add(2)?;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([*bytes.get(at.checked_add(2)?)?, *bytes.get(at.checked_add(3)?)?]));
        if len < 2 {
            return None;
        }
        let end = at.checked_add(2)?.checked_add(len)?;
        let body = bytes.get(at.checked_add(4)?..end)?;
        if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
            return body.get(6..);
        }
        at = end;
    }
    None
}

/// A TIFF block's byte order and bytes.
struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn new(b: &'a [u8]) -> Option<Tiff<'a>> {
        let le = match b.get(..4)? {
            [b'I', b'I', 42, 0] => true,
            [b'M', b'M', 0, 42] => false,
            _ => return None,
        };
        Some(Tiff { b, le })
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let s: [u8; 2] = self.b.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let s: [u8; 4] = self.b.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.le { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    }

    /// An IFD's entries at `offset`: (tag, type, count, where the value is).
    fn entries(&self, offset: usize) -> Vec<Entry> {
        let Some(n) = self.u16(offset) else { return Vec::new() };
        (0..usize::from(n).min(MAX_ENTRIES))
            .filter_map(|i| {
                let e = offset.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
                let (tag, ty, count) = (self.u16(e)?, self.u16(e.checked_add(2)?)?, self.u32(e.checked_add(4)?)?);
                let size = type_size(ty)?.checked_mul(usize::try_from(count).ok()?)?;
                // Four bytes or fewer sit in the entry itself; more, at the offset it holds.
                let value = e.checked_add(8)?;
                let at = if size <= 4 { value } else { usize::try_from(self.u32(value)?).ok()? };
                self.b.get(at..at.checked_add(size)?)?;
                Some(Entry { tag, ty, count: usize::try_from(count).ok()?, at })
            })
            .collect()
    }

    fn rational(&self, e: &Entry, i: usize) -> Option<f64> {
        if !matches!(e.ty, 5 | 10) || i >= e.count {
            return None;
        }
        let at = e.at.checked_add(i.checked_mul(8)?)?;
        let (n, d) = (self.u32(at)?, self.u32(at.checked_add(4)?)?);
        let (n, d) = if e.ty == 10 { (f64::from(n as i32), f64::from(d as i32)) } else { (f64::from(n), f64::from(d)) };
        (d != 0.0).then_some(n / d).filter(|v| v.is_finite())
    }

    fn short(&self, e: &Entry) -> Option<u16> {
        match e.ty {
            3 => self.u16(e.at),
            1 | 7 => self.b.get(e.at).map(|&v| u16::from(v)),
            _ => None,
        }
    }

    fn ascii(&self, e: &Entry) -> Option<String> {
        if e.ty != 2 {
            return None;
        }
        let s = self.b.get(e.at..e.at.checked_add(e.count.min(256))?)?;
        let s = s.split(|&c| c == 0).next().unwrap_or(s);
        let t = String::from_utf8_lossy(s).trim().to_string();
        (!t.is_empty()).then_some(t)
    }

    fn pointer(&self, entries: &[Entry], tag: u16) -> Option<usize> {
        let e = entries.iter().find(|e| e.tag == tag && matches!(e.ty, 4 | 13))?;
        usize::try_from(self.u32(e.at)?).ok()
    }
}

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    ty: u16,
    count: usize,
    at: usize,
}

/// The entry with `tag`, if there is one.
fn find(es: &[Entry], tag: u16) -> Option<&Entry> {
    es.iter().find(|e| e.tag == tag)
}

fn type_size(ty: u16) -> Option<usize> {
    Some(match ty {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

/// What a JPEG's EXIF says; `None` when it has no EXIF.
pub fn read_exif(bytes: &[u8]) -> Option<Exif> {
    let t = Tiff::new(exif_block(bytes)?)?;
    let ifd0 = t.entries(usize::try_from(t.u32(4)?).ok()?);
    let mut x = Exif { orientation: 1, ..Exif::default() };
    x.make = find(&ifd0, 0x010F).and_then(|e| t.ascii(e));
    x.model = find(&ifd0, 0x0110).and_then(|e| t.ascii(e));
    x.orientation = find(&ifd0, 0x0112).and_then(|e| t.short(e)).filter(|o| (1..=8).contains(o)).unwrap_or(1);
    let tidy =
        |s: String| (s.len() >= 19).then(|| format!("{} {}", s.get(..10).unwrap_or("").replace(':', "-"), s.get(11..19).unwrap_or("")));
    if let Some(exif) = t.pointer(&ifd0, 0x8769) {
        let es = t.entries(exif);
        x.time = find(&es, 0x9003).and_then(|e| t.ascii(e)).and_then(tidy);
    }
    if x.time.is_none() {
        x.time = find(&ifd0, 0x0132).and_then(|e| t.ascii(e)).and_then(tidy);
    }
    if let Some(gps) = t.pointer(&ifd0, 0x8825) {
        let es = t.entries(gps);
        let dms = |tag: u16| {
            let e = find(&es, tag)?;
            let (d, m, s) = (t.rational(e, 0)?, t.rational(e, 1).unwrap_or(0.0), t.rational(e, 2).unwrap_or(0.0));
            Some(d + m / 60.0 + s / 3600.0)
        };
        let reference = |tag: u16| find(&es, tag).and_then(|e| t.ascii(e)).unwrap_or_default().to_ascii_uppercase();
        if let (Some(lat), Some(lon)) = (dms(0x0002), dms(0x0004)) {
            let lat = if reference(0x0001).starts_with('S') { -lat } else { lat };
            let lon = if reference(0x0003).starts_with('W') { -lon } else { lon };
            // 0, 0 is what some cameras write before they have a fix.
            if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && (lat, lon) != (0.0, 0.0) {
                x.position = Some((lon, lat));
            }
        }
        x.altitude = find(&es, 0x0006).and_then(|e| t.rational(e, 0)).map(|a| {
            let below = find(&es, 0x0005).and_then(|e| t.short(e)) == Some(1);
            if below { -a } else { a }
        });
        x.direction = find(&es, 0x0011).and_then(|e| t.rational(e, 0)).filter(|d| (0.0..=360.0).contains(d));
        if x.time.is_none() {
            let date = find(&es, 0x001D).and_then(|e| t.ascii(e));
            let hms = find(&es, 0x0007).and_then(|e| Some((t.rational(e, 0)?, t.rational(e, 1)?, t.rational(e, 2)?)));
            if let (Some(d), Some((h, m, s))) = (date, hms) {
                x.time = Some(format!("{} {:02}:{:02}:{:02} UTC", d.replace(':', "-"), h as u32, m as u32, s as u32));
            }
        }
    }
    Some(x)
}

/// A JPEG's thumbnail, at most [`THUMB_SIZE`] pixels a side and turned as its EXIF says, as a
/// `data:image/jpeg;base64,…` address (kept with the feature, so a project needs no other file).
pub fn thumbnail(bytes: &[u8], orientation: u16) -> crate::Result<String> {
    let bad = |e: image::ImageError| crate::DataError::Format(format!("the photo: {e}"));
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    // A photo is decoded whole to make its thumbnail: bounded so a hostile file can't take more
    // than a large camera photo needs (a 50 MP JPEG fits), and less on the web, where running out
    // of memory aborts the app.
    let (side, alloc) = if cfg!(target_arch = "wasm32") { (8192, 160 << 20) } else { (16384, 256 << 20) };
    limits.max_image_width = Some(side);
    limits.max_image_height = Some(side);
    limits.max_alloc = Some(alloc);
    reader.limits(limits);
    let img = reader.decode().map_err(bad)?.thumbnail(THUMB_SIZE, THUMB_SIZE);
    let img = match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 70).encode_image(&img.to_rgb8()).map_err(bad)?;
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(out)))
}

/// The fields of a photo layer.
pub fn fields() -> Vec<Field> {
    vec![
        Field::new("name", FieldType::Text),
        Field::new("file", FieldType::Text),
        Field::new("time", FieldType::Text),
        Field::new("altitude", FieldType::Double),
        Field::new("direction", FieldType::Double),
        Field::new("camera", FieldType::Text),
        Field::new("thumbnail", FieldType::Text),
    ]
}

/// Whether a dataset is a photo layer (its fields are [`fields`]), so more photos can join it.
pub fn is_photo_layer(ds: &Dataset) -> bool {
    ds.geometry_type == GeometryType::Point && ds.fields == fields()
}

/// A photo as a feature of a photo layer, or why it isn't one (no GPS position, unreadable).
pub fn feature(file: &str, bytes: &[u8], id: u64) -> std::result::Result<Feature, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("{file}: larger than {} MB", MAX_BYTES >> 20));
    }
    let x = read_exif(bytes).ok_or_else(|| format!("{file}: no EXIF (not a camera's JPEG)"))?;
    let (lon, lat) = x.position.ok_or_else(|| format!("{file}: no GPS position"))?;
    let thumb = thumbnail(bytes, x.orientation).map_err(|e| format!("{file}: {e}"))?;
    let leaf = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let stem = leaf.rsplit_once('.').map(|(s, _)| s).unwrap_or(leaf);
    let camera = match (&x.make, &x.model) {
        // The model often repeats the make ("Canon" "Canon EOS 40D").
        (Some(m), Some(n)) if n.to_ascii_lowercase().starts_with(&m.to_ascii_lowercase()) => Some(n.clone()),
        (Some(m), Some(n)) => Some(format!("{m} {n}")),
        (m, n) => m.clone().or_else(|| n.clone()),
    };
    let text = |s: Option<String>| s.map(Value::Text).unwrap_or(Value::Null);
    let num = |v: Option<f64>| v.map(Value::Float).unwrap_or(Value::Null);
    Ok(Feature {
        id,
        geometry: Some(Geometry::Point(Coord::new(lon, lat))),
        attrs: vec![
            Value::Text(stem.to_string()),
            Value::Text(file.to_string()),
            text(x.time),
            num(x.altitude),
            num(x.direction),
            text(camera),
            Value::Text(thumb),
        ],
    })
}

/// Builds a photo layer one photo at a time, so only one file's bytes need be held at once.
/// Photos without a GPS position (or unreadable) are left out and named in the report.
pub struct Reader {
    ds: Dataset,
    report: ReadReport,
    left_out: Vec<String>,
    room: usize,
    over: bool,
}

impl Reader {
    /// A reader for a layer named `name` that takes at most `room` photos (and never more than
    /// [`MAX_PHOTOS`]).
    pub fn new(name: &str, room: usize) -> Reader {
        Reader {
            ds: Dataset::new(name, GeometryType::Point, fields()),
            report: ReadReport::default(),
            left_out: Vec::new(),
            room: room.min(MAX_PHOTOS),
            over: false,
        }
    }

    /// Whether the layer is full: further photos would be left out.
    pub fn is_full(&self) -> bool {
        self.ds.features.len() >= self.room
    }

    /// Reads one photo (its file name and contents).
    pub fn add(&mut self, file: &str, bytes: &[u8]) {
        if self.is_full() {
            self.left_over(1);
            return;
        }
        match feature(file, bytes, self.ds.features.len() as u64) {
            Ok(f) => self.ds.features.push(f),
            Err(e) => self.leave_out(e),
        }
    }

    /// A photo that could not be read at all (`why` names it).
    pub fn leave_out(&mut self, why: String) {
        self.report.skipped += 1;
        self.left_out.push(why);
    }

    /// `n` photos left out because the layer is full.
    pub fn left_over(&mut self, n: usize) {
        self.report.skipped += n;
        self.over |= n > 0;
    }

    /// The layer and its report.
    pub fn finish(mut self) -> (Dataset, ReadReport) {
        let left_out = &self.left_out;
        if !left_out.is_empty() {
            let shown: Vec<&str> = left_out.iter().take(10).map(String::as_str).collect();
            let more = if left_out.len() > 10 { format!(" (and {} more)", left_out.len() - 10) } else { String::new() };
            let what = if left_out.len() == 1 { "1 photo".to_string() } else { format!("{} photos", left_out.len()) };
            self.report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::DroppedFeature,
                format!("{what} left out: {}{more}", shown.join("; ")),
            ));
        }
        if self.over {
            self.report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::DroppedOverLimit,
                format!("a photo layer holds up to {MAX_PHOTOS} photos; the rest were left out"),
            ));
        }
        (self.ds, self.report)
    }
}

/// Photos as a point layer named `name`: those without a GPS position (or unreadable) are left
/// out and named in the report.
pub fn read(name: &str, photos: &[(String, Vec<u8>)]) -> (Dataset, ReadReport) {
    let mut r = Reader::new(name, MAX_PHOTOS);
    for (file, bytes) in photos {
        r.add(file, bytes);
    }
    r.finish()
}

/// A small JPEG with an EXIF block, for tests: a little-endian or big-endian TIFF with IFD0 (make, model,
/// orientation, pointers), an Exif IFD (the time) and a GPS IFD.
#[doc(hidden)]
pub fn example(le: bool, lat: (f64, &str), lon: (f64, &str)) -> Vec<u8> {
    let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let mut t: Vec<u8> = if le { b"II*\0".to_vec() } else { b"MM\0*".to_vec() };
    t.extend(w32(8));
    // Layout: IFD0 at 8 (6 entries), Exif IFD, GPS IFD, then the data area.
    let ifd0 = 8usize;
    let ifd0_len = 2 + 6 * 12 + 4;
    let exif = ifd0 + ifd0_len;
    let exif_len = 2 + 12 + 4;
    let gps = exif + exif_len;
    let gps_len = 2 + 7 * 12 + 4;
    let mut data_at = gps + gps_len;
    let mut data: Vec<u8> = Vec::new();
    let mut put = |bytes: &[u8], data: &mut Vec<u8>| {
        let at = data_at;
        data.extend_from_slice(bytes);
        data_at += bytes.len();
        at as u32
    };
    let entry = |tag: u16, ty: u16, count: u32, value: [u8; 4]| {
        let mut e = Vec::new();
        e.extend(w16(tag));
        e.extend(w16(ty));
        e.extend(w32(count));
        e.extend(value);
        e
    };
    let make = put(b"NIKON\0", &mut data);
    let model = put(b"NIKON COOLPIX P6000\0", &mut data);
    let time = put(b"2008:10:22 16:29:49\0", &mut data);
    let rat = |v: &[(u32, u32)]| v.iter().flat_map(|(n, d)| [w32(*n), w32(*d)]).flatten().collect::<Vec<u8>>();
    let dms = |deg: f64| {
        let d = deg.trunc();
        let m = ((deg - d) * 60.0).trunc();
        let s = ((deg - d) * 60.0 - m) * 60.0;
        rat(&[(d as u32, 1), (m as u32, 1), ((s * 1000.0).round() as u32, 1000)])
    };
    let la = put(&dms(lat.0), &mut data);
    let lo = put(&dms(lon.0), &mut data);
    let alt = put(&rat(&[(1234, 10)]), &mut data);
    let dir = put(&rat(&[(2705, 10)]), &mut data);
    let ref4 = |s: &str| {
        let mut v = [0u8; 4];
        v[0] = s.as_bytes().first().copied().unwrap_or(b'N');
        v
    };
    let mut ifd: Vec<u8> = Vec::new();
    ifd.extend(w16(6));
    ifd.extend(entry(0x010F, 2, 6, w32(make)));
    ifd.extend(entry(0x0110, 2, 20, w32(model)));
    let mut o = [0u8; 4];
    o[0] = w16(1)[0];
    o[1] = w16(1)[1];
    ifd.extend(entry(0x0112, 3, 1, o));
    ifd.extend(entry(0x8769, 4, 1, w32(exif as u32)));
    ifd.extend(entry(0x8825, 4, 1, w32(gps as u32)));
    ifd.extend(entry(0x9999, 1, 1, [0; 4]));
    ifd.extend(w32(0));
    ifd.extend(w16(1));
    ifd.extend(entry(0x9003, 2, 20, w32(time)));
    ifd.extend(w32(0));
    ifd.extend(w16(7));
    ifd.extend(entry(0x0001, 2, 2, ref4(lat.1)));
    ifd.extend(entry(0x0002, 5, 3, w32(la)));
    ifd.extend(entry(0x0003, 2, 2, ref4(lon.1)));
    ifd.extend(entry(0x0004, 5, 3, w32(lo)));
    ifd.extend(entry(0x0005, 1, 1, [0; 4]));
    ifd.extend(entry(0x0006, 5, 1, w32(alt)));
    ifd.extend(entry(0x0011, 5, 1, w32(dir)));
    ifd.extend(w32(0));
    t.extend(ifd);
    t.extend(data);
    // A real (tiny) JPEG with the EXIF segment after its SOI.
    let mut jpeg = Vec::new();
    let encoded =
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg).encode_image(&image::RgbImage::from_pixel(64, 48, image::Rgb([200, 120, 40])));
    let _ = encoded;
    let mut seg = vec![0xFF, 0xE1];
    seg.extend(((t.len() + 8) as u16).to_be_bytes());
    seg.extend(b"Exif\0\0");
    seg.extend(t);
    let mut out = jpeg.get(..2).unwrap_or_default().to_vec();
    out.extend(seg);
    out.extend(jpeg.get(2..).unwrap_or_default());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gps_time_and_camera_in_both_byte_orders() {
        for le in [true, false] {
            let x = read_exif(&example(le, (43.467_157, "N"), (11.885_126, "E"))).unwrap();
            let (lon, lat) = x.position.unwrap();
            assert!((lat - 43.467_157).abs() < 1e-5 && (lon - 11.885_126).abs() < 1e-5, "{lon} {lat}");
            assert_eq!(x.altitude, Some(123.4));
            assert_eq!(x.direction, Some(270.5));
            assert_eq!(x.time.as_deref(), Some("2008-10-22 16:29:49"));
            assert_eq!((x.make.as_deref(), x.model.as_deref(), x.orientation), (Some("NIKON"), Some("NIKON COOLPIX P6000"), 1));
        }
        // South and west are negative.
        let x = read_exif(&example(true, (33.9, "S"), (151.2, "W"))).unwrap();
        let (lon, lat) = x.position.unwrap();
        assert!(lat < 0.0 && lon < 0.0);
    }

    #[test]
    fn photos_become_points_with_thumbnails() {
        let photos = vec![
            ("trip/a.jpg".to_string(), example(true, (43.4, "N"), (11.8, "E"))),
            ("trip/b.jpeg".to_string(), b"\xFF\xD8\xFF\xD9".to_vec()),
            ("c.txt".to_string(), b"hello".to_vec()),
        ];
        let (ds, report) = read("Trip", &photos);
        assert_eq!(ds.features.len(), 1);
        assert!(is_photo_layer(&ds));
        let f = &ds.features[0];
        assert_eq!(f.attrs[0], Value::Text("a".into()));
        assert_eq!(f.attrs[5], Value::Text("NIKON COOLPIX P6000".into()));
        let Value::Text(thumb) = &f.attrs[6] else { panic!() };
        assert!(thumb.starts_with("data:image/jpeg;base64,") && thumb.len() < 40_000);
        assert_eq!(report.skipped, 2);
        assert!(report.messages[0].contains("b.jpeg") && report.messages[0].contains("c.txt"), "{:?}", report.messages);
    }

    #[test]
    fn every_truncation_and_junk_is_none_not_a_panic() {
        let full = example(false, (43.4, "N"), (11.8, "E"));
        for n in 0..full.len().min(700) {
            let _ = read_exif(&full[..n]);
            let _ = feature("x.jpg", &full[..n], 0);
        }
        // Offsets and counts pointing anywhere.
        let mut junk = full.clone();
        for i in (20..200).step_by(7) {
            if let Some(b) = junk.get_mut(i) {
                *b = 0xFF;
            }
        }
        let _ = read_exif(&junk);
        assert!(read_exif(b"not a jpeg").is_none());
        assert!(is_photo_name("A.JPG") && is_photo_name("b.jpeg") && !is_photo_name("c.png"));
    }

    #[test]
    fn offsets_near_the_top_of_the_address_space_are_none() {
        // Exif and GPS pointers and a value offset near u32::MAX: on 32-bit wasm, usize is u32,
        // so any unchecked addition would overflow.
        for le in [true, false] {
            let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
            let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
            let mut tiff = if le { b"II*\0".to_vec() } else { b"MM\0*".to_vec() };
            tiff.extend(w32(8));
            tiff.extend(w16(3));
            for (tag, ty, count, value) in [(0x8769u16, 4u16, 1u32, u32::MAX - 3), (0x8825, 4, 1, u32::MAX), (0x010F, 2, 64, u32::MAX - 7)]
            {
                tiff.extend(w16(tag));
                tiff.extend(w16(ty));
                tiff.extend(w32(count));
                tiff.extend(w32(value));
            }
            let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
            jpeg.extend(u16::try_from(tiff.len() + 8).unwrap_or(0).to_be_bytes());
            jpeg.extend(b"Exif\0\0");
            jpeg.extend(&tiff);
            jpeg.extend([0xFF, 0xD9]);
            let x = read_exif(&jpeg).unwrap_or_default();
            assert!(x.position.is_none() && x.make.is_none() && x.time.is_none());
            let t = Tiff { b: &tiff, le };
            for top in [u32::MAX as usize, usize::MAX, usize::MAX - 1, usize::MAX - 11] {
                assert!(t.entries(top).is_empty() && t.u16(top).is_none() && t.u32(top).is_none());
                let e = Entry { tag: 2, ty: 5, count: 3, at: top };
                assert!(t.rational(&e, 0).is_none() && t.rational(&e, 2).is_none());
                assert!(t.ascii(&Entry { ty: 2, ..e }).is_none() && t.short(&Entry { ty: 1, ..e }).is_none());
                assert!(t.pointer(&[Entry { tag: 0x8825, ty: 4, count: 1, at: top }], 0x8825).is_none());
            }
        }
        // A JPEG segment length that would run past the end.
        assert!(read_exif(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF, b'E']).is_none());
    }
}
