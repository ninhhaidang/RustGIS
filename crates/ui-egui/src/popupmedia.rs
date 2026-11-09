//! The identify pop-up's web addresses: values that are links, and a picture fetched from one.
//!
//! A value that is an `http://` or `https://` address is a link. The pop-up shows a picture from
//! the `https://` address in a field named as one (`image`, `image_url`, `photo`, `photo_url`,
//! `picture`, `picture_url`, `thumbnail` or `thumbnail_url`, in any case, with or without the
//! `_`, `-` or space), or else from the first address whose path ends in `.jpg`, `.jpeg`, `.png`
//! or `.webp`, the fields taken in the order of their names. One that would be fetched
//! (`https://`, not on this computer or the local network) comes before one that wouldn't, so a
//! plain `http://` address doesn't hide an `https://` picture.
//!
//! Fetching a picture named by the data tells its server who opened the file and when, so only
//! RustGIS's own asset server ([`AUTO_HOST`], exactly, over `https://`: [`loads_by_itself`]) is
//! fetched as the pop-up opens. A redirect it answers with is handled as for any download:
//! followed natively, refused in the browser, which can't show where it leads. A
//! picture anywhere else shows a **Show Picture** button and nothing is fetched until it is
//! clicked. A plain `http://` picture, or one on this computer or the local network, is never
//! fetched; the pop-up says so.
//!
//! A picture is fetched through the tile cache's downloads (worker threads natively, the
//! browser's fetch on the web, where the browser's own HTTP cache applies), at most
//! [`MAX_PICTURE_BYTES`], and decoded with the limits of [`crate::mapview::decode_picture`],
//! its size checked before decoding. Each address's picture, or why there is none, is kept for
//! the session ([`MAX_PICTURES`] of them, the least recently shown dropped first), so it isn't
//! fetched again when the pop-up opens again, nor a failure every frame; all go when the tile
//! cache is cleared, as by a sign-out ([`Pictures::forget_if_cleared`]).

use rustgis_engine::render::rustgis_tiles::{address, cache};

use crate::Slot;

/// The longest value shown as a link, in bytes.
pub(crate) const MAX_LINK: usize = 2048;
/// The most bytes a picture fetched for the pop-up may have (as a picture kept in a value).
pub(crate) const MAX_PICTURE_BYTES: usize = 4 << 20;
/// The most pictures (or failures) kept, by address.
pub(crate) const MAX_PICTURES: usize = 16;
/// The longest side a fetched picture is kept at, in pixels: the pop-up shows it smaller.
const MAX_SIDE: u32 = 1024;

/// Field names (lowercase, without `_`, `-` and spaces) whose web address is a picture.
const PICTURE_FIELDS: [&str; 8] = ["image", "imageurl", "photo", "photourl", "picture", "pictureurl", "thumbnail", "thumbnailurl"];
/// Address path endings that are a picture, whatever the field is named.
const PICTURE_ENDINGS: [&str; 4] = [".jpg", ".jpeg", ".png", ".webp"];

/// `s` without `prefix`, matched in any case.
fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) { s.get(prefix.len()..) } else { None }
}

/// The scheme-less rest of an `http://` or `https://` address.
fn after_scheme(v: &str) -> Option<&str> {
    strip_prefix_ignore_case(v, "https://").or_else(|| strip_prefix_ignore_case(v, "http://"))
}

/// Whether an address's authority is a host name or an IP address (letters, digits, dots and
/// hyphens, or an IPv6 address in brackets), with a port of digits if any.
fn valid_authority(authority: &str) -> bool {
    let (host_ok, port) = match authority.strip_prefix('[') {
        Some(v6) => match v6.split_once(']') {
            Some((h, rest)) => (!h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.'), rest),
            None => return false,
        },
        None => {
            let (h, rest) = authority.find(':').map_or((authority, ""), |i| authority.split_at(i));
            let ok = !h.is_empty() && !h.starts_with(['.', '-']) && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            (ok, rest)
        }
    };
    let port_ok = match port.strip_prefix(':') {
        Some(p) => (1..=5).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()),
        None => port.is_empty(),
    };
    host_ok && port_ok
}

/// `v` as a web address the pop-up links to: `http://` or `https://` (in any case), a host
/// name or IP address, and nothing that can't be in an address (spaces, control characters,
/// quotes, angle brackets, backslashes), at most [`MAX_LINK`] bytes. A user name before the host
/// (`https://bank.example@other.example/`) passes for another site, so it is text too.
pub(crate) fn web_link(v: &str) -> Option<&str> {
    if v.len() > MAX_LINK {
        return None;
    }
    let rest = after_scheme(v)?;
    if v.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\\' | '`' | '{' | '}' | '^')) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    (!authority.contains('@') && valid_authority(authority)).then_some(v)
}

/// Whether a field is named as a picture's address (`image_url`, `Photo`, `thumbnail`…); not
/// `image_page_url` or `image_credit`.
pub(crate) fn picture_field(name: &str) -> bool {
    if name.len() > 64 {
        return false;
    }
    let plain: String = name.chars().filter(|c| !matches!(c, '_' | '-' | ' ')).flat_map(char::to_lowercase).collect();
    PICTURE_FIELDS.contains(&plain.as_str())
}

/// Whether a web address's path (not its host, query or fragment) ends as a picture file does.
pub(crate) fn picture_path(url: &str) -> bool {
    let Some(rest) = after_scheme(url) else { return false };
    // The query and fragment go first: a slash in them (`?next=/a.jpg`) doesn't start a path.
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let Some((_, path)) = rest.split_once('/') else { return false };
    PICTURE_ENDINGS
        .iter()
        .any(|end| path.len() > end.len() && path.get(path.len() - end.len()..).is_some_and(|t| t.eq_ignore_ascii_case(end)))
}

/// The web address the pop-up shows a picture from, among a feature's text values (name,
/// value): a link in a picture field first, else the first link whose path is a picture file's;
/// and before either, one that would be fetched ([`refusal`] has nothing against it), so a plain
/// `http://` or local address is chosen (to say why it isn't fetched) only when no other is.
pub(crate) fn picture_link<'a>(attrs: impl Iterator<Item = (&'a str, &'a str)> + Clone) -> Option<&'a str> {
    // Each candidate: whether its field is named as a picture's, and its address.
    let candidates = attrs.filter_map(|(k, v)| Some((picture_field(k), web_link(v)?))).filter(|(named, v)| *named || picture_path(v));
    let first = |named: bool, fetched: bool| candidates.clone().find(|(n, v)| *n == named && (!fetched || refusal(v).is_none()));
    first(true, true).or_else(|| first(false, true)).or_else(|| first(true, false)).or_else(|| first(false, false)).map(|(_, v)| v)
}

/// The one host whose pictures the pop-up fetches without being asked: RustGIS's own asset
/// server, which the project runs and which serves the sample data's pictures.
pub(crate) const AUTO_HOST: &str = "assets.rustgis.app";

/// The authority (host and port) of a web address the pop-up links to, as written.
pub(crate) fn authority(url: &str) -> Option<&str> {
    let rest = after_scheme(web_link(url)?)?;
    rest.split(['/', '?', '#']).next()
}

/// Whether the pop-up fetches the picture at `url` as soon as it shows it: an `https://` address
/// (the scheme in any case) whose authority is exactly [`AUTO_HOST`], compared without regard to
/// case as host names are. Nothing else passes: no port (not even `:443`), no user name (which
/// [`web_link`] refuses), no subdomain or longer name (`assets.rustgis.app.example.com`), no
/// trailing dot, no percent-encoded or non-ASCII host (neither is a link at all), no `http://`.
pub(crate) fn loads_by_itself(url: &str) -> bool {
    strip_prefix_ignore_case(url, "https://").is_some() && authority(url).is_some_and(|a| a.eq_ignore_ascii_case(AUTO_HOST))
}

/// The largest picture decoded, pixels a side (as [`crate::mapview::decode_picture`] takes).
const MAX_DECODED_SIDE: u32 = crate::mapview::MAX_PICTURE_SIDE;

/// Why there is no picture to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The fetch failed: the network, the server's answer, the browser's CORS check, its size
    /// in bytes (the fetcher stops reading past [`MAX_PICTURE_BYTES`]).
    Fetch(String),
    /// What came back isn't a picture that decodes.
    NotPicture,
    /// A picture in a format that isn't read here (its name, such as `BMP` or `TIFF`).
    Unsupported(String),
    /// A picture of more pixels than are decoded (width, height), refused before decoding.
    TooLarge(u32, u32),
    /// Only `https://` pictures are fetched.
    NotHttps,
    /// Addresses on this computer or the local network aren't fetched for a value.
    Local,
    /// Link-local and unspecified addresses, and cloud metadata hosts, are never fetched.
    Forbidden,
}

impl Failure {
    /// The line the pop-up shows: "Picture unavailable: …".
    pub(crate) fn line(&self) -> String {
        match self {
            Failure::Fetch(e) => {
                let cut = e.chars().count() > 200;
                let reason: String = e.chars().take(200).chain(cut.then_some('…')).collect();
                crate::i18n::fmt("Picture unavailable: {reason}", &[("reason", &reason)])
            }
            Failure::NotPicture => crate::i18n::fmt("Picture unavailable: not a picture", &[]),
            Failure::Unsupported(format) => {
                crate::i18n::fmt("Picture unavailable: {format} pictures aren't supported", &[("format", format)])
            }
            Failure::TooLarge(w, h) => crate::i18n::fmt(
                "Picture unavailable: {w} × {h} pixels is larger than {max} × {max}",
                &[("w", &w.to_string()), ("h", &h.to_string()), ("max", &MAX_DECODED_SIDE.to_string())],
            ),
            Failure::NotHttps => crate::i18n::fmt("Picture unavailable: only https:// pictures are fetched", &[]),
            Failure::Local => crate::i18n::fmt("Picture unavailable: pictures aren't fetched from this computer or the local network", &[]),
            Failure::Forbidden => crate::i18n::fmt("Picture unavailable: that address is never fetched", &[]),
        }
    }
}

/// Why the picture at a web address won't be fetched at all, even when asked, if it won't.
fn refusal(url: &str) -> Option<Failure> {
    if strip_prefix_ignore_case(url, "https://").is_none() {
        Some(Failure::NotHttps)
    } else {
        match address::reach(url) {
            address::Reach::Public => None,
            address::Reach::Local => Some(Failure::Local),
            address::Reach::Forbidden => Some(Failure::Forbidden),
        }
    }
}

/// Where a fetched picture lands, decoded off the frame (natively).
pub(crate) type Landing = Slot<Result<image::RgbaImage, Failure>>;

/// A picture by address: being fetched, shown, or not to be had.
enum Entry {
    Loading(Landing),
    Ready(egui::TextureHandle),
    Failed(Failure),
}

/// What the pop-up shows for a picture address.
#[derive(Clone)]
pub(crate) enum Shown {
    /// Not fetched: a picture from another server, fetched only when the user asks.
    Offered,
    Loading,
    Ready(egui::TextureHandle),
    Failed(Failure),
}

/// The pictures fetched for the pop-up this session, the least recently shown first.
#[derive(Default)]
pub(crate) struct Pictures {
    entries: Vec<(String, Entry)>,
    /// [`cache::clears`] when [`Self::forget_if_cleared`] last looked.
    clears: u64,
}

impl Pictures {
    /// Forgets every picture (those being fetched too) when the tile cache was cleared since this
    /// was last called, `clears` being [`cache::clears`]: a sign-out or a login that ends clears
    /// it, and a picture may have been read with that login, so it goes with the cache's tiles.
    pub(crate) fn forget_if_cleared(&mut self, clears: u64) {
        if clears != self.clears {
            self.entries.clear();
            self.clears = clears;
        }
    }

    /// The picture at `url`. The first time it is asked for (or after it was dropped to make
    /// room), one from [`AUTO_HOST`] is started with `fetch`; any other is [`Shown::Offered`],
    /// not fetched, until [`Self::accept`]. A refused or failed one is kept like a picture, so
    /// it isn't fetched again.
    pub(crate) fn show(&mut self, ctx: &egui::Context, url: &str, fetch: impl FnOnce(&str, Landing)) -> Shown {
        let (url, mut entry) = match self.entries.iter().position(|(u, _)| u == url) {
            Some(i) => self.entries.remove(i),
            None => match refusal(url) {
                Some(f) => self.make_room(url, Entry::Failed(f)),
                None if loads_by_itself(url) => self.make_room(url, start(url, fetch)),
                None => return Shown::Offered,
            },
        };
        let landed = match &entry {
            Entry::Loading(landing) => landing.lock().unwrap_or_else(|e| e.into_inner()).take(),
            _ => None,
        };
        if let Some(r) = landed {
            entry = match r {
                Ok(img) => {
                    let size = [img.width() as usize, img.height() as usize];
                    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    Entry::Ready(ctx.load_texture(format!("identify_picture:{url}"), color, egui::TextureOptions::LINEAR))
                }
                Err(f) => Entry::Failed(f),
            };
        }
        let shown = match &entry {
            Entry::Loading(_) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
                Shown::Loading
            }
            Entry::Ready(t) => Shown::Ready(t.clone()),
            Entry::Failed(f) => Shown::Failed(f.clone()),
        };
        self.entries.push((url, entry));
        shown
    }

    /// The user asked to see the picture at `url`: it is started with `fetch` unless it is
    /// already kept (fetched, being fetched or failed) or is never fetched ([`refusal`]).
    pub(crate) fn accept(&mut self, url: &str, fetch: impl FnOnce(&str, Landing)) {
        if self.entries.iter().any(|(u, _)| u == url) {
            return;
        }
        let entry = match refusal(url) {
            Some(f) => Entry::Failed(f),
            None => start(url, fetch),
        };
        let kept = self.make_room(url, entry);
        self.entries.push(kept);
    }

    /// Room for one more address, the least recently shown dropped; the new one, to push.
    fn make_room(&mut self, url: &str, entry: Entry) -> (String, Entry) {
        if self.entries.len() >= MAX_PICTURES {
            self.entries.remove(0);
        }
        (url.to_string(), entry)
    }

    /// How many addresses are kept.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Starts fetching the picture at `url` with `fetch`.
fn start(url: &str, fetch: impl FnOnce(&str, Landing)) -> Entry {
    let landing = Landing::default();
    fetch(url, landing.clone());
    Entry::Loading(landing)
}

/// A reader of a fetched picture's header, allowed the memory [`crate::mapview::decode_picture`]
/// allows ([`crate::mapview::MAX_PICTURE_ALLOC`]): reading a PNG's header unpacks its colour
/// profile, which a few megabytes can make hundreds of megabytes. Not its pixel limits, so a
/// picture past them is still measured, and refused saying how large it is.
fn header(bytes: &[u8]) -> Result<image::ImageReader<std::io::Cursor<&[u8]>>, Failure> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|_| Failure::NotPicture)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(crate::mapview::MAX_PICTURE_ALLOC);
    reader.limits(limits);
    Ok(reader)
}

/// The picture formats the pop-up shows. A fixed list, not whatever decoders `image` happens to be
/// built with: another crate in the build can add one (`arboard`, through the clipboard, adds BMP
/// on Windows and TIFF on macOS), and the same file must give the same answer on every platform.
const SHOWN: [image::ImageFormat; 4] =
    [image::ImageFormat::Png, image::ImageFormat::Jpeg, image::ImageFormat::WebP, image::ImageFormat::Gif];

/// Decodes a fetched picture for the pop-up: refused when it isn't one, or is in a format outside
/// [`SHOWN`], or when its header says it is larger than [`crate::mapview::decode_picture`] takes
/// (before anything is decoded, so a small file can't unpack into a huge picture), and kept at
/// most [`MAX_SIDE`] pixels a side.
pub(crate) fn decode(bytes: &[u8]) -> Result<image::RgbaImage, Failure> {
    let header = header(bytes)?;
    let Some(format) = header.format() else { return Err(Failure::NotPicture) };
    // A picture in another format says so, rather than that it isn't one.
    if !SHOWN.contains(&format) {
        return Err(Failure::Unsupported(format!("{format:?}").to_uppercase()));
    }
    let (w, h) = header.into_dimensions().map_err(|_| Failure::NotPicture)?;
    if w > MAX_DECODED_SIDE || h > MAX_DECODED_SIDE {
        return Err(Failure::TooLarge(w, h));
    }
    let img = crate::mapview::decode_picture(bytes).map_err(|_| Failure::NotPicture)?;
    let (w, h) = (img.width().max(1), img.height().max(1));
    if w.max(h) <= MAX_SIDE {
        return Ok(img);
    }
    let k = MAX_SIDE as f32 / w.max(h) as f32;
    let (tw, th) = (((w as f32 * k) as u32).clamp(1, MAX_SIDE), ((h as f32 * k) as u32).clamp(1, MAX_SIDE));
    Ok(image::imageops::thumbnail(&img, tw, th))
}

/// Fetches a picture for the pop-up through the tile cache's downloads (the browser's fetch on
/// the web), at most [`MAX_PICTURE_BYTES`], decoding it where it lands (off the frame natively).
/// Natively it connects only to internet addresses: a public-looking name, or a redirect, that
/// resolves to this computer or the local network is refused where it resolves, past what
/// [`refusal`] can tell from the address.
pub(crate) fn fetch(url: &str, landing: Landing) {
    // The scheme in lowercase, as the downloads compare it: a redirect from `HTTPS://` down to
    // `http://` is refused as one from `https://` is.
    let url = match url.split_once("://") {
        Some((scheme, rest)) => format!("{}://{rest}", scheme.to_ascii_lowercase()),
        None => url.to_string(),
    };
    cache::download_public_limited(
        &url,
        MAX_PICTURE_BYTES,
        Box::new(move |r| {
            let r = r.map_err(Failure::Fetch).and_then(|bytes| decode(&bytes));
            *landing.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        }),
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn links_are_http_and_https_addresses_only() {
        for good in [
            "https://example.org",
            "https://example.org/",
            "http://example.org/a/b?c=d&e=f#g",
            "HTTPS://EXAMPLE.ORG/PATH",
            "HtTp://example.org",
            "https://example.org:8443/x",
            "https://[2001:db8::1]:443/x",
            "https://192.0.2.1/x",
            "https://de.wikipedia.org/wiki/Zürich",
        ] {
            assert_eq!(web_link(good), Some(good), "{good}");
        }
        for bad in [
            "",
            "example.org",
            "www.example.org/page",
            "javascript:alert(1)",
            "JavaScript://example.org/%0aalert(1)",
            "data:text/html,<b>hi</b>",
            "data:image/png;base64,iVBORw0KGgo=",
            "file:///etc/passwd",
            "ftp://example.org/x",
            "mailto:someone@example.org",
            "https://",
            "https:///path",
            "https://exa mple.org",
            "https://example.org/a b",
            " https://example.org",
            "https://example.org ",
            "https://example.org/\n",
            "https://example.org/\u{0}",
            "https://example.org/\"onmouseover=\"x",
            "https://example.org/<script>",
            "https://bank.example@evil.example/",
            "https://user:pass@example.org/",
            "https://exam_ple.org/",
            "https://.example.org/",
            "https://example.org:port/",
            "https://example.org:123456/",
            "https://[nothex]/",
            "https://[::1/",
            "see https://example.org",
            "https://例え.jp/",
            "httpx://example.org",
            "http:/example.org",
        ] {
            assert_eq!(web_link(bad), None, "{bad:?}");
        }
        // Long values are text, at the byte cap exactly too.
        let at_cap = format!("https://example.org/{}", "a".repeat(MAX_LINK - 20));
        assert_eq!(at_cap.len(), MAX_LINK);
        assert!(web_link(&at_cap).is_some());
        let over = format!("{at_cap}a");
        assert!(web_link(&over).is_none());
        assert!(web_link(&format!("https://example.org/{}", "é".repeat(5000))).is_none());
        // Multi-byte text at the scheme's length doesn't split a character.
        assert!(web_link("ééééé").is_none());
        assert!(web_link("httpsé").is_none());
    }

    #[test]
    fn picture_fields_and_paths() {
        for name in [
            "image",
            "IMAGE",
            "image_url",
            "Image URL",
            "imageUrl",
            "image-url",
            "photo",
            "Photo_URL",
            "picture",
            "thumbnail",
            "thumbnailUrl",
        ] {
            assert!(picture_field(name), "{name}");
        }
        for name in ["image_page_url", "image_credit", "images", "img", "photographer", "url", "website", "", &"image".repeat(20)] {
            assert!(!picture_field(name), "{name}");
        }
        for url in [
            "https://x.org/a.jpg",
            "https://x.org/a/B.JPEG",
            "https://x.org/a.png?w=200",
            "https://x.org/a.webp#top",
            "HTTPS://X.ORG/A.PNG",
        ] {
            assert!(picture_path(url), "{url}");
        }
        for url in [
            "https://x.org/a.html",
            "https://example.png",
            "https://example.png/",
            "https://x.org/page?file=a.jpg",
            "https://x.org/a#b.png",
            // A slash in the query or fragment doesn't start a path.
            "https://x.org?next=/a.jpg",
            "https://x.org#/a.jpg",
            "https://x.org/.png",
            "https://x.org/a.jpgx",
            "not a url.png",
        ] {
            assert!(!picture_path(url), "{url}");
        }
    }

    #[test]
    fn the_picture_shown_is_a_picture_field_first() {
        let pick = |attrs: &[(&'static str, &'static str)]| picture_link(attrs.iter().copied());
        // A picture field wins over a file address listed before it.
        assert_eq!(pick(&[("a_link", "https://x.org/other.jpg"), ("image_url", "https://x.org/photo")]), Some("https://x.org/photo"));
        // Without one, the first address of a picture file.
        assert_eq!(
            pick(&[("website", "https://x.org/"), ("pic", "https://x.org/1.png"), ("pic2", "https://x.org/2.png")]),
            Some("https://x.org/1.png")
        );
        // A page or a credit is not a picture; nor is a picture field that holds no address.
        assert_eq!(pick(&[("image_page_url", "https://x.org/details/1"), ("image_credit", "NASA/JPL")]), None);
        assert_eq!(pick(&[("image", "photo.jpg"), ("photo", "javascript:alert(1)")]), None);
        // A link whose query names a picture file is a link, not a picture.
        assert_eq!(pick(&[("website", "https://assets.rustgis.app?next=/a.jpg")]), None);
        // A plain http picture field is chosen (and then refused, saying why).
        assert_eq!(pick(&[("photo", "http://x.org/p")]), Some("http://x.org/p"));
        // ...but not over a picture that would be fetched, in either kind of candidate.
        assert_eq!(
            pick(&[("photo", "http://old.example/p"), ("thumbnail", "https://images.example.org/p.jpg")]),
            Some("https://images.example.org/p.jpg")
        );
        assert_eq!(pick(&[("a", "http://x.org/1.jpg"), ("b", "https://x.org/2.jpg")]), Some("https://x.org/2.jpg"));
        assert_eq!(pick(&[("image", "https://192.168.1.2/p.jpg"), ("photo", "https://x.org/p")]), Some("https://x.org/p"));
        // A file address that would be fetched over a picture field that wouldn't.
        assert_eq!(pick(&[("a", "https://x.org/1.jpg"), ("photo", "http://x.org/p")]), Some("https://x.org/1.jpg"));
        // None would be: a picture field's first, to say why.
        assert_eq!(
            pick(&[("a", "http://x.org/1.jpg"), ("photo", "https://localhost/p"), ("thumbnail", "http://x.org/t")]),
            Some("https://localhost/p")
        );
        assert_eq!(pick(&[("a", "http://x.org/1.jpg"), ("b", "http://x.org/2.jpg")]), Some("http://x.org/1.jpg"));
        assert_eq!(refusal("http://x.org/p"), Some(Failure::NotHttps));
        assert_eq!(refusal("https://192.168.1.2/p.jpg"), Some(Failure::Local));
        assert_eq!(refusal("https://localhost/p.jpg"), Some(Failure::Local));
        assert_eq!(refusal("https://169.254.169.254/p.jpg"), Some(Failure::Forbidden));
        assert_eq!(refusal("https://metadata.google.internal/p.jpg"), Some(Failure::Forbidden));
        assert_eq!(refusal("HTTPS://x.org/p.jpg"), None);
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::new(w, h).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn decoding_keeps_the_caps() {
        assert_eq!(decode(&png(160, 120)).unwrap().dimensions(), (160, 120));
        // Larger than the pop-up shows, under the cap: kept smaller, in proportion.
        assert_eq!(decode(&png(2048, 512)).unwrap().dimensions(), (1024, 256));
        // Past the cap: refused from its header (a blank picture compresses to far less than it
        // unpacks into), saying how large it is.
        assert_eq!(decode(&png(2049, 10)), Err(Failure::TooLarge(2049, 10)));
        assert_eq!(decode(&png(10, 2049)), Err(Failure::TooLarge(10, 2049)));
        assert_eq!(decode(&png(4000, 4000)), Err(Failure::TooLarge(4000, 4000)));
        assert_eq!(Failure::TooLarge(4000, 3000).line(), "Picture unavailable: 4000 × 3000 pixels is larger than 2048 × 2048");
        assert_eq!(decode(b"<html>Not found</html>"), Err(Failure::NotPicture));
        assert_eq!(decode(&[]), Err(Failure::NotPicture));
        // A picture in a format the pop-up doesn't show says so, not that it isn't a picture, whatever
        // decoders the build has (the clipboard adds BMP on Windows and TIFF on macOS).
        let mut bmp = b"BM".to_vec();
        bmp.resize(64, 0);
        assert_eq!(decode(&bmp), Err(Failure::Unsupported("BMP".into())));
        let mut tiff = b"II*\0".to_vec();
        tiff.resize(64, 0);
        assert_eq!(decode(&tiff), Err(Failure::Unsupported("TIFF".into())));
        // A GIF is shown (a 1 × 1 one).
        let gif = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff!\xf9\x04\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";
        assert_eq!(decode(gif).unwrap().dimensions(), (1, 1));
        assert_eq!(Failure::Unsupported("BMP".into()).line(), "Picture unavailable: BMP pictures aren't supported");
        // A picture cut short doesn't decode.
        let whole = png(64, 64);
        assert_eq!(decode(whole.get(..whole.len() / 2).unwrap()), Err(Failure::NotPicture));
    }

    /// PNG's CRC-32 of a chunk's type and data.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    /// A zlib stream unpacking into `n` zero bytes, in about 13 bits per 258 of them: a literal
    /// zero, then copies of 258 bytes from one back, in a block of fixed Huffman codes (RFC 1951).
    fn zlib_zeros(n: usize) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        let (mut acc, mut bits) = (0u64, 0u32);
        // A Huffman code goes most significant bit first; other fields least significant first.
        let mut put = |out: &mut Vec<u8>, v: u32, len: u32, code: bool| {
            let v = if code { v.reverse_bits() >> (32 - len) } else { v };
            acc |= u64::from(v) << bits;
            bits += len;
            while bits >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        };
        put(&mut out, 1, 1, false); // the last block
        put(&mut out, 1, 2, false); // fixed codes
        let mut left = n;
        while left > 0 {
            if left > 258 && left < n {
                put(&mut out, 0b1100_0101, 8, true); // a length of 258
                put(&mut out, 0, 5, true); // from one byte back
                left -= 258;
            } else {
                put(&mut out, 0x30, 8, true); // a literal zero
                left -= 1;
            }
        }
        put(&mut out, 0, 7, true); // end of block
        put(&mut out, 0, 7, false);
        // Adler-32 of `n` zero bytes.
        out.extend_from_slice(&((((n % 65521) as u32) << 16) | 1).to_be_bytes());
        out
    }

    /// A 1 × 1 PNG whose colour profile (iCCP chunk) unpacks into `profile` bytes.
    fn png_with_profile(profile: usize) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let chunk = |out: &mut Vec<u8>, kind: &[u8], data: &[u8]| {
            out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
            let typed = [kind, data].concat();
            out.extend_from_slice(&typed);
            out.extend_from_slice(&crc32(&typed).to_be_bytes());
        };
        chunk(&mut out, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        chunk(&mut out, b"iCCP", &[b"icc\0\0".as_slice(), &zlib_zeros(profile)].concat());
        // One row: no filter, one transparent pixel, as a stored block.
        chunk(&mut out, b"IDAT", &[0x78, 0x01, 0x01, 5, 0, 0xfa, 0xff, 0, 0, 0, 0, 0, 0, 5, 0, 1]);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn a_picture_header_is_read_within_the_memory_decoding_takes() {
        use image::ImageDecoder;
        // A colour profile is read with the header: this PNG is well made.
        let small = png_with_profile(3000);
        let mut d = header(&small).unwrap().into_decoder().unwrap();
        assert_eq!(d.icc_profile().unwrap().map(|p| p.len()), Some(3000));
        assert_eq!(decode(&small).unwrap().dimensions(), (1, 1));
        // A profile past the memory decoding may take, in far fewer bytes than are fetched, isn't
        // unpacked by the header's reader (whose own limit would let the 4 MB fetched unpack
        // into 512 MiB), and the picture still shows.
        let past = usize::try_from(crate::mapview::MAX_PICTURE_ALLOC).unwrap() + (1 << 20);
        let big = png_with_profile(past);
        assert!(big.len() < MAX_PICTURE_BYTES / 8, "{}", big.len());
        let mut d = header(&big).unwrap().into_decoder().unwrap();
        assert_eq!(d.dimensions(), (1, 1));
        assert_eq!(d.icc_profile().unwrap(), None);
        assert_eq!(decode(&big).unwrap().dimensions(), (1, 1));
    }

    #[test]
    fn only_the_asset_server_loads_by_itself() {
        for yes in [
            "https://assets.rustgis.app/images/moon.webp",
            "https://assets.rustgis.app/",
            "https://assets.rustgis.app",
            "https://assets.rustgis.app?x=1",
            "https://assets.rustgis.app#a",
            "HTTPS://ASSETS.RUSTGIS.APP/A.PNG",
            "https://Assets.RustGIS.app/a.png",
        ] {
            assert!(loads_by_itself(yes), "{yes}");
        }
        for no in [
            // Plain http, other schemes.
            "http://assets.rustgis.app/a.png",
            "ftp://assets.rustgis.app/a.png",
            "//assets.rustgis.app/a.png",
            "assets.rustgis.app/a.png",
            // Longer, shorter or other names.
            "https://assets.rustgis.app.evil.example/a.png",
            "https://assets.rustgis.appx/a.png",
            "https://xassets.rustgis.app/a.png",
            "https://evil-assets.rustgis.app/a.png",
            "https://cdn.assets.rustgis.app/a.png",
            "https://rustgis.app/a.png",
            "https://assets-rustgis.app/a.png",
            "https://assets.rustgis.app./a.png",
            // A port, even the default one.
            "https://assets.rustgis.app:443/a.png",
            "https://assets.rustgis.app:8443/a.png",
            // A user name that passes for the host, either way round.
            "https://assets.rustgis.app@evil.example/a.png",
            "https://evil.example@assets.rustgis.app/a.png",
            "https://assets.rustgis.app:pw@evil.example/a.png",
            // The name elsewhere in the address.
            "https://evil.example/assets.rustgis.app/a.png",
            "https://evil.example/?u=https://assets.rustgis.app/a.png",
            "https://evil.example#assets.rustgis.app",
            // Percent-encoded, backslashed, non-ASCII look-alikes and stray characters.
            "https://assets%2Erustgis.app/a.png",
            "https://assets.rustgis%2eapp/a.png",
            "https://%61ssets.rustgis.app/a.png",
            "https://assets.rustgis.app\\@evil.example/a.png",
            "https://\u{430}ssets.rustgis.app/a.png",
            "https://assets.rustgis.app\u{3002}evil.example/a.png",
            "https://assets.rustgis.app;evil.example/a.png",
            " https://assets.rustgis.app/a.png",
            "https://assets.rustgis.app/a b.png",
            "https://[::1]/a.png",
            "",
        ] {
            assert!(!loads_by_itself(no), "{no:?}");
        }
    }

    /// A fetch that counts its calls and answers with `answer`, or never when it's `None`.
    fn fake(calls: &std::cell::Cell<usize>, answer: Option<Result<Vec<u8>, String>>) -> impl FnOnce(&str, Landing) + '_ {
        move |_, landing| {
            calls.set(calls.get() + 1);
            if let Some(r) = answer {
                *landing.lock().unwrap() = Some(r.map_err(Failure::Fetch).and_then(|b| decode(&b)));
            }
        }
    }

    #[test]
    fn pictures_are_fetched_once_and_failures_kept() {
        let ctx = egui::Context::default();
        let mut pics = Pictures::default();
        let calls = std::cell::Cell::new(0);
        let url = "https://assets.rustgis.app/a.png";
        // Fetched once; shown when it lands, without asking again.
        let landing = std::cell::RefCell::new(None);
        let shown = pics.show(&ctx, url, |_, l| {
            calls.set(calls.get() + 1);
            *landing.borrow_mut() = Some(l);
        });
        assert!(matches!(shown, Shown::Loading));
        assert!(matches!(pics.show(&ctx, url, fake(&calls, None)), Shown::Loading));
        *landing.borrow().as_ref().unwrap().lock().unwrap() = Some(decode(&png(8, 4)));
        match pics.show(&ctx, url, fake(&calls, None)) {
            Shown::Ready(t) => assert_eq!(t.size(), [8, 4]),
            _ => panic!("not shown"),
        }
        assert!(matches!(pics.show(&ctx, url, fake(&calls, None)), Shown::Ready(_)));
        assert_eq!(calls.get(), 1);
        // A failure is kept: asked again, it isn't fetched again.
        let bad = "https://assets.rustgis.app/missing.png";
        for answer in [Some(Err("http status: 404".to_string())), None, None] {
            match pics.show(&ctx, bad, fake(&calls, answer)) {
                Shown::Failed(f) => assert_eq!(f.line(), "Picture unavailable: http status: 404"),
                _ => panic!("not failed"),
            }
        }
        assert_eq!(calls.get(), 2);
        // Not a picture, and addresses never fetched, even when asked.
        assert!(matches!(
            pics.show(&ctx, "https://assets.rustgis.app/page.png", fake(&calls, Some(Ok(b"<html/>".to_vec())))),
            Shown::Failed(Failure::NotPicture)
        ));
        assert_eq!(calls.get(), 3);
        for (refused, why) in [("http://x.org/p.png", Failure::NotHttps), ("https://10.0.0.1/p.png", Failure::Local)] {
            assert!(matches!(pics.show(&ctx, refused, fake(&calls, Some(Ok(png(1, 1))))), Shown::Failed(f) if f == why));
            pics.accept(refused, fake(&calls, Some(Ok(png(1, 1)))));
            assert!(matches!(pics.show(&ctx, refused, fake(&calls, None)), Shown::Failed(f) if f == why));
        }
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn another_server_is_asked_only_once_accepted() {
        let ctx = egui::Context::default();
        let mut pics = Pictures::default();
        let calls = std::cell::Cell::new(0);
        let url = "https://images.example.org/a.png";
        // Offered, frame after frame, and nothing kept or fetched.
        for _ in 0..5 {
            assert!(matches!(pics.show(&ctx, url, fake(&calls, Some(Ok(png(2, 2))))), Shown::Offered));
        }
        assert_eq!((calls.get(), pics.len()), (0, 0));
        // Accepted: fetched once, then shown; accepting again asks nothing more.
        pics.accept(url, fake(&calls, Some(Ok(png(2, 2)))));
        pics.accept(url, fake(&calls, Some(Ok(png(2, 2)))));
        assert_eq!(calls.get(), 1);
        assert!(matches!(pics.show(&ctx, url, fake(&calls, None)), Shown::Ready(_)));
        assert!(matches!(pics.show(&ctx, url, fake(&calls, None)), Shown::Ready(_)));
        assert_eq!(calls.get(), 1);
        // Another address on that server still waits to be asked.
        assert!(matches!(pics.show(&ctx, "https://images.example.org/b.png", fake(&calls, None)), Shown::Offered));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn pictures_go_when_the_cache_is_cleared() {
        let ctx = egui::Context::default();
        let mut pics = Pictures::default();
        let calls = std::cell::Cell::new(0);
        let shown = "https://assets.rustgis.app/a.png";
        let accepted = "https://images.example.org/b.png";
        pics.forget_if_cleared(7);
        pics.show(&ctx, shown, fake(&calls, Some(Ok(png(2, 2)))));
        pics.accept(accepted, fake(&calls, Some(Ok(png(2, 2)))));
        // The cache not cleared since: both kept.
        pics.forget_if_cleared(7);
        assert_eq!((calls.get(), pics.len()), (2, 2));
        assert!(matches!(pics.show(&ctx, accepted, fake(&calls, None)), Shown::Ready(_)));
        // Cleared (a sign-out): both forgotten; the asset server's picture is fetched again, the
        // other waits to be asked again.
        pics.forget_if_cleared(8);
        assert_eq!(pics.len(), 0);
        assert!(matches!(pics.show(&ctx, accepted, fake(&calls, None)), Shown::Offered));
        assert!(matches!(pics.show(&ctx, shown, fake(&calls, Some(Ok(png(2, 2))))), Shown::Ready(_)));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn the_least_recently_shown_picture_makes_room() {
        let ctx = egui::Context::default();
        let mut pics = Pictures::default();
        let calls = std::cell::Cell::new(0);
        let url = |i: usize| format!("https://assets.rustgis.app/{i}.png");
        for i in 0..MAX_PICTURES {
            pics.show(&ctx, &url(i), fake(&calls, Some(Ok(png(1, 1)))));
        }
        // The first shown again: the second is now the least recent.
        pics.show(&ctx, &url(0), fake(&calls, None));
        assert_eq!(calls.get(), MAX_PICTURES);
        pics.show(&ctx, &url(MAX_PICTURES), fake(&calls, Some(Ok(png(1, 1)))));
        assert_eq!(pics.len(), MAX_PICTURES);
        assert_eq!(calls.get(), MAX_PICTURES + 1);
        // The first is still kept; the second is fetched again.
        pics.show(&ctx, &url(0), fake(&calls, None));
        assert_eq!(calls.get(), MAX_PICTURES + 1);
        pics.show(&ctx, &url(1), fake(&calls, Some(Ok(png(1, 1)))));
        assert_eq!((calls.get(), pics.len()), (MAX_PICTURES + 2, MAX_PICTURES));
        // An accepted picture makes room the same way.
        pics.accept("https://images.example.org/x.png", fake(&calls, Some(Ok(png(1, 1)))));
        assert_eq!((calls.get(), pics.len()), (MAX_PICTURES + 3, MAX_PICTURES));
    }
}
