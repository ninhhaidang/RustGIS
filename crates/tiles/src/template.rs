//! Tile addresses and Web Mercator tile math.

use rustgis_geom::{Extent, MERCATOR_R};

/// Half the Web Mercator world's width, in metres.
pub const HALF_WORLD: f64 = std::f64::consts::PI * MERCATOR_R;

/// The most tiles one source draws in a view.
pub const MAX_TILES: usize = 400;

/// A tile: zoom, column (west to east) and row (north to south, as XYZ counts them).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    /// The side of a tile at zoom `z`, in metres.
    pub fn span(z: u8) -> f64 {
        2.0 * HALF_WORLD / f64::from(1u32 << z.min(30))
    }
    /// The tile's Web Mercator extent.
    pub fn extent(self) -> Extent {
        let s = TileId::span(self.z);
        let (x0, y1) = (-HALF_WORLD + f64::from(self.x) * s, HALF_WORLD - f64::from(self.y) * s);
        Extent::new(x0, y1 - s, x0 + s, y1)
    }
    /// The tile `levels` zooms up that contains this one.
    pub fn ancestor(self, levels: u8) -> Option<TileId> {
        (levels <= self.z).then(|| TileId { z: self.z - levels, x: self.x >> levels, y: self.y >> levels })
    }
    /// The four tiles one zoom down.
    pub fn children(self) -> [TileId; 4] {
        let (z, x, y) = (self.z.saturating_add(1), self.x.saturating_mul(2), self.y.saturating_mul(2));
        [
            TileId { z, x, y },
            TileId { z, x: x.saturating_add(1), y },
            TileId { z, x, y: y.saturating_add(1) },
            TileId { z, x: x.saturating_add(1), y: y.saturating_add(1) },
        ]
    }
    /// The Bing Maps quadkey.
    pub fn quadkey(self) -> String {
        (1..=self.z)
            .rev()
            .map(|i| {
                let m = 1u32 << (i - 1);
                let d = u8::from(self.x & m != 0) + 2 * u8::from(self.y & m != 0);
                char::from(b'0' + d)
            })
            .collect()
    }
}

/// A tile in a view: which tile to fetch, and where its copy of the world puts it (`wx`, which
/// can run past the edges of the world when the view shows the world twice).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub id: TileId,
    /// The column in the unwrapped world (`id.x` plus a whole number of worlds).
    pub wx: i64,
}

impl Placed {
    /// The Web Mercator extent where this copy of the tile is drawn.
    pub fn extent(&self) -> Extent {
        let s = TileId::span(self.id.z);
        let x0 = -HALF_WORLD + self.wx as f64 * s;
        let y1 = HALF_WORLD - f64::from(self.id.y) * s;
        Extent::new(x0, y1 - s, x0 + s, y1)
    }
}

/// The columns (unwrapped) and rows of the tiles at zoom `z` (at most 30) over a Web Mercator
/// extent; None for an extent that isn't a number.
fn span(e: &Extent, z: u8) -> Option<(i64, i64, i64, i64)> {
    let n = 1i64 << z;
    let s = TileId::span(z);
    if !(e.min_x.is_finite() && e.max_x.is_finite() && e.min_y.is_finite() && e.max_y.is_finite()) {
        return None;
    }
    let col = |x: f64| ((x + HALF_WORLD) / s).floor().clamp(-4.0 * n as f64, 5.0 * n as f64) as i64;
    let row = |y: f64| ((HALF_WORLD - y) / s).floor().clamp(-1.0, n as f64) as i64;
    Some((col(e.min_x), col(e.max_x), row(e.max_y).max(0), row(e.min_y).min(n - 1)))
}

/// The deepest zoom up to `z` (and down to `min_z`) whose tiles over a Web Mercator extent are
/// at most [`MAX_TILES`]: a wide extent (the globe's whole-world mosaic) takes coarser tiles
/// rather than [`covering`] leaving out those farthest from its centre (#223).
pub fn fitting_zoom(e: &Extent, z: u8, min_z: u8) -> u8 {
    let mut z = z.min(30);
    while z > min_z {
        match span(e, z) {
            Some((x0, x1, y0, y1)) if (x1 - x0 + 1).max(0).saturating_mul((y1 - y0 + 1).max(0)) > MAX_TILES as i64 => z -= 1,
            _ => break,
        }
    }
    z
}

/// The tiles at zoom `z` covering a Web Mercator extent, nearest the centre first. At most
/// [`MAX_TILES`].
pub fn covering(e: &Extent, z: u8) -> Vec<Placed> {
    let z = z.min(30);
    let n = 1i64 << z;
    let Some((x0, x1, y0, y1)) = span(e, z) else { return Vec::new() };
    let mut out = Vec::new();
    if x1 < x0 || y1 < y0 || (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) > MAX_TILES as i64 * 4 {
        return out;
    }
    let (cx, cy) = ((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
    for wx in x0..=x1 {
        for y in y0..=y1 {
            let id = TileId { z, x: wx.rem_euclid(n) as u32, y: y as u32 };
            out.push(Placed { id, wx });
        }
    }
    out.sort_by(|a, b| {
        let d = |p: &Placed| (p.wx as f64 - cx).powi(2) + (f64::from(p.id.y) - cy).powi(2);
        d(a).total_cmp(&d(b))
    });
    out.truncate(MAX_TILES);
    out
}

/// The zoom level whose `tile_size`-pixel tiles best match `m_per_px` metres per pixel, rounded
/// to the sharper level a little before halfway. Never below 0 or above `max`.
pub fn zoom_for(m_per_px: f64, tile_size: u32, max: u8) -> u8 {
    let z = (2.0 * HALF_WORLD / (f64::from(tile_size.max(1)) * m_per_px)).log2();
    if !z.is_finite() {
        return 0;
    }
    (z + 0.4).floor().clamp(0.0, f64::from(max)) as u8
}

/// The MapLibre zoom (512-pixel world at zoom 0) at `m_per_px` metres per logical pixel.
pub fn style_zoom(m_per_px: f64) -> f32 {
    let z = (2.0 * HALF_WORLD / (512.0 * m_per_px)).log2();
    if z.is_finite() { z.clamp(0.0, 24.0) as f32 } else { 0.0 }
}

/// Fills in a tile address template: `{z}`, `{x}`, `{y}`, `{-y}` (rows from the south), `{s}`
/// (one of `subdomains`, chosen by the tile), `{switch:a,b,c}`, `{q}`/`{quadkey}`,
/// `{bbox-epsg-3857}` (a WMS box), `{width}`/`{height}`, `{r}` (dropped) and `{ratio}` (1).
/// `tms` makes `{y}` count from the south too.
pub fn expand(template: &str, t: TileId, tile_size: u32, tms: bool, subdomains: &[String]) -> String {
    let n = 1u64 << t.z.min(30);
    let flipped = n.saturating_sub(1).saturating_sub(u64::from(t.y));
    let (y, neg_y) = if tms { (flipped, u64::from(t.y)) } else { (u64::from(t.y), flipped) };
    let pick = |opts: &[&str]| -> String {
        if opts.is_empty() {
            return String::new();
        }
        let i = (u64::from(t.x) + u64::from(t.y)) as usize % opts.len();
        opts.get(i).copied().unwrap_or_default().to_string()
    };
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open).unwrap_or_default());
        let after = rest.get(open + 1..).unwrap_or_default();
        let Some(close) = after.find('}') else {
            out.push_str(rest.get(open..).unwrap_or_default());
            rest = "";
            break;
        };
        let token = after.get(..close).unwrap_or_default();
        let value = match token {
            "z" | "zoom" => Some(t.z.to_string()),
            "x" => Some(t.x.to_string()),
            "y" => Some(y.to_string()),
            "-y" => Some(neg_y.to_string()),
            "q" | "quadkey" => Some(t.quadkey()),
            "s" => {
                let subs: Vec<&str> =
                    if subdomains.is_empty() { vec!["a", "b", "c"] } else { subdomains.iter().map(String::as_str).collect() };
                Some(pick(&subs))
            }
            "bbox-epsg-3857" => {
                let e = t.extent();
                Some(format!("{:.3},{:.3},{:.3},{:.3}", e.min_x, e.min_y, e.max_x, e.max_y))
            }
            "width" | "height" => Some(tile_size.to_string()),
            "r" => Some(String::new()),
            "ratio" => Some("1".into()),
            _ => token.strip_prefix("switch:").map(|opts| pick(&opts.split(',').map(str::trim).collect::<Vec<_>>())),
        };
        match value {
            Some(v) => out.push_str(&v),
            // Not a placeholder this knows: kept as it is.
            None => out.push_str(rest.get(open..open + close + 2).unwrap_or_default()),
        }
        rest = after.get(close + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// Resolves `href` against the document at `base` (for addresses inside a style or TileJSON).
/// An invalid bound base stays refused by request validation rather than resolving anonymously.
pub fn join(base: &str, href: &str) -> String {
    let (plain_base, anonymous_base) = crate::s3::split_anonymous(base);
    let (plain_href, anonymous_href) = crate::s3::split_anonymous(href);
    if anonymous_base || anonymous_href {
        if crate::s3::split_anonymous(plain_base).1 {
            return base.to_string();
        }
        if crate::s3::split_anonymous(plain_href).1 {
            return href.to_string();
        }
        // An anonymous child narrows authority, including when its parent chose an account.
        let base = crate::logins::inner(rustgis_data::s3_address::inner(plain_base));
        return crate::s3::anonymous(&join(base, plain_href));
    }
    // Only the supplying document's base carries authority. A link may name a resource,
    // but neither a service-login nor an S3 selector in it can choose an account.
    let href = match rustgis_data::login_address::parse(href) {
        Ok(Some(bound)) => bound.url,
        Err(_) => return href.to_string(),
        Ok(None) => href,
    };
    let href = match rustgis_data::s3_address::parse(href) {
        Ok(Some(bound)) => bound.url,
        Err(_) => return href.to_string(),
        Ok(None) => href,
    };
    match rustgis_data::s3_address::parse(base) {
        Ok(Some(bound)) => {
            let joined = join(bound.url, href);
            return crate::address::inherit(base, &joined).unwrap_or_else(|_| {
                // As for bound service logins, keep the actual derived address even when
                // it exceeds the parser's limit. Request validation then refuses it; never
                // substitute the parent document or drop the selected account.
                format!("{}{}:{}:{}:{joined}", rustgis_data::s3_address::PREFIX, bound.binding, bound.endpoint.len(), bound.endpoint)
            });
        }
        Err(_) => return base.to_string(),
        Ok(None) => {}
    }
    match rustgis_data::login_address::parse(base) {
        Ok(Some(bound)) => return crate::logins::inherit(base, &join(bound.url, href)),
        Err(_) => return base.to_string(),
        Ok(None) => {}
    }
    // URL-like text in the query or fragment is data, not an absolute reference.
    let (href_path, tail) = match href.find(['?', '#']) {
        Some(i) => (href.get(..i).unwrap_or(href), href.get(i..).unwrap_or("")),
        None => (href, ""),
    };
    if href_path.contains("://") {
        return href.to_string();
    }
    let Some((scheme, rest)) = base.split_once("://") else { return href.to_string() };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest.get(..host_end).unwrap_or(rest);
    if let Some(path) = href.strip_prefix("//") {
        return format!("{scheme}://{path}");
    }
    if href.starts_with('/') {
        return format!("{scheme}://{host}{}{tail}", remove_dots(href_path));
    }
    let path = rest.get(host_end..).unwrap_or("").split(['?', '#']).next().unwrap_or("");
    if href_path.is_empty() {
        // RFC 3986 §5.2.2: an empty reference path keeps the document, and keeps its
        // query only when the reference supplies none. A base fragment is never inherited.
        let query = if href.starts_with('?') {
            ""
        } else {
            rest.split('#').next().and_then(|r| r.find('?').and_then(|i| r.get(i..))).unwrap_or("")
        };
        return format!("{scheme}://{host}{path}{query}{tail}");
    }
    let dir = path.rfind('/').map_or("/", |i| path.get(..=i).unwrap_or("/"));
    format!("{scheme}://{host}{}{tail}", remove_dots(&format!("{dir}{href_path}")))
}

/// A path with its `.` and `..` segments resolved (never above the root).
fn remove_dots(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len().saturating_sub(1);
    for (i, seg) in segments.iter().enumerate() {
        match *seg {
            "." => {}
            ".." => {
                // The root's empty segment stays.
                if out.len() > 1 {
                    out.pop();
                }
            }
            s => out.push(s),
        }
        // A path ending in `.` or `..` names a directory.
        if i == last && matches!(*seg, "." | "..") {
            out.push("");
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty-path references keep the document path and inherit only an absent query.
    #[test]
    fn empty_path_links_keep_the_bound_document_path() {
        let url = "https://s3.example/dir/style.json?old=1#old";
        for base in [
            url.to_string(),
            rustgis_data::s3_address::bind("0123456789abcdef", "https://s3.example", url).unwrap(),
            rustgis_data::login_address::bind("0123456789abcdef", "https://s3.example", url).unwrap(),
            crate::s3::anonymous(url),
        ] {
            for (href, expected) in [
                ("?f=json", "https://s3.example/dir/style.json?f=json"),
                ("?next=https://other.example/tiles", "https://s3.example/dir/style.json?next=https://other.example/tiles"),
                ("#https://other.example/tiles", "https://s3.example/dir/style.json?old=1#https://other.example/tiles"),
                ("#new", "https://s3.example/dir/style.json?old=1#new"),
                ("", "https://s3.example/dir/style.json?old=1"),
                ("?", "https://s3.example/dir/style.json?"),
                ("#", "https://s3.example/dir/style.json?old=1#"),
            ] {
                let joined = join(&base, href);
                let expected = if let Some(bound) = rustgis_data::s3_address::parse(&base).unwrap() {
                    rustgis_data::s3_address::bind(bound.binding, bound.endpoint, expected).unwrap()
                } else if let Some(bound) = rustgis_data::login_address::parse(&base).unwrap() {
                    rustgis_data::login_address::bind(bound.binding, bound.origin, expected).unwrap()
                } else if crate::s3::split_anonymous(&base).1 {
                    crate::s3::anonymous(expected)
                } else {
                    expected.to_string()
                };
                assert_eq!(joined, expected, "reference {href:?} keeps the document's authority wrapper");
            }
        }
    }

    #[test]
    fn derived_links_keep_only_the_document_binding() {
        let base = rustgis_data::login_address::bind("0123456789abcdef", "https://a.test", "https://a.test/style.json").unwrap();
        for href in ["tiles/{z}/{x}/{y}.pbf", "/sprite", "https://A.test:443/info"] {
            let next = join(&base, href);
            assert_eq!(rustgis_data::login_address::parse(&next).unwrap().unwrap().binding, "0123456789abcdef");
        }
        for href in ["http://a.test/tile", "https://a.test:444/tile", "https://other.test/tile"] {
            assert_eq!(join(&base, href), href);
        }
        let supplied = rustgis_data::login_address::bind("fedcba9876543210", "https://a.test", "https://a.test/tile").unwrap();
        assert_eq!(join("https://a.test/style.json", &supplied), "https://a.test/tile");
        assert_eq!(rustgis_data::login_address::parse(&join(&base, &supplied)).unwrap().unwrap().binding, "0123456789abcdef");
        assert_eq!(join("rustgis-login:broken", "/tile"), "rustgis-login:broken");
    }

    /// A document may name a resource, but cannot choose another saved S3 account for it.
    #[test]
    fn document_s3_selectors_do_not_choose_an_account() {
        let endpoint = "https://s3-links.example";
        let base = rustgis_data::s3_address::bind("0123456789abcdef", endpoint, &format!("{endpoint}/bucket/style.json")).unwrap();
        for url in [format!("{endpoint}/bucket/tile"), "https://other.example/tile".into()] {
            let supplied = rustgis_data::s3_address::bind("fedcba9876543210", endpoint, &url).unwrap();
            assert_eq!(join("https://public.example/style.json", &supplied), url, "the supplied selector is removed");
            let joined = join(&base, &supplied);
            let selected = rustgis_data::s3_address::parse(&joined).unwrap();
            if url.starts_with(endpoint) {
                assert_eq!(selected.unwrap().binding, "0123456789abcdef", "only the document's own account is inherited");
            } else {
                assert!(selected.is_none(), "a different server has no inherited account");
            }
        }
        for malformed in ["rustgis-s3:broken", "rustgis-login:broken"] {
            let joined = join(&base, malformed);
            assert_eq!(joined, malformed, "malformed selectors remain refused, not relative paths");
            assert!(crate::address::check(&joined, None).is_err());
        }
    }

    #[test]
    fn joins_relative_addresses() {
        let base = "https://x.org/a/b/styles/root.json";
        assert_eq!(join(base, "../../"), "https://x.org/a/");
        assert_eq!(join(base, "../sprites/sprite"), "https://x.org/a/b/sprites/sprite");
        assert_eq!(join(base, "./tile/{z}/{y}/{x}.pbf?f=1"), "https://x.org/a/b/styles/tile/{z}/{y}/{x}.pbf?f=1");
        assert_eq!(join(base, "../../../../../x"), "https://x.org/x", "never above the root");
        assert_eq!(join(base, "/root/./y"), "https://x.org/root/y");
        assert_eq!(
            join("https://x.org/a/VectorTileServer/?f=json", "tile/{z}/{y}/{x}.pbf"),
            "https://x.org/a/VectorTileServer/tile/{z}/{y}/{x}.pbf"
        );
    }

    #[test]
    fn expands_templates() {
        let t = TileId { z: 3, x: 5, y: 2 };
        assert_eq!(expand("https://tile.example.com/{z}/{x}/{y}.png", t, 256, false, &[]), "https://tile.example.com/3/5/2.png");
        assert_eq!(expand("https://x/{z}/{x}/{-y}.png", t, 256, false, &[]), "https://x/3/5/5.png");
        assert_eq!(expand("https://x/{z}/{x}/{y}.png", t, 256, true, &[]), "https://x/3/5/5.png");
        assert_eq!(expand("https://{s}.x/{z}", t, 256, false, &[]), "https://b.x/3");
        assert_eq!(expand("https://{s}.x/{z}", t, 256, false, &["1".into(), "2".into(), "3".into(), "4".into()]), "https://4.x/3");
        assert_eq!(expand("https://{switch:mt0,mt1}.x/{z}", t, 256, false, &[]), "https://mt1.x/3");
        assert_eq!(expand("https://x/{q}?k={api_key}&{", t, 256, false, &[]), "https://x/121?k={api_key}&{");
        assert_eq!(TileId { z: 3, x: 3, y: 5 }.quadkey(), "213");
        let wms = expand("https://x/wms?BBOX={bbox-epsg-3857}&WIDTH={width}", TileId { z: 0, x: 0, y: 0 }, 256, false, &[]);
        assert_eq!(wms, "https://x/wms?BBOX=-20037508.343,-20037508.343,20037508.343,20037508.343&WIDTH=256");
        assert_eq!(expand("https://x/{z}/{x}/{y}{r}.png", t, 256, false, &[]), "https://x/3/5/2.png");
    }

    #[test]
    fn wide_extents_take_coarser_tiles() {
        let world = Extent::new(-HALF_WORLD, -HALF_WORLD, HALF_WORLD, HALF_WORLD);
        // 16 × 16 tiles fit; 32 × 32 don't.
        assert_eq!(fitting_zoom(&world, 8, 0), 4);
        assert!(covering(&world, fitting_zoom(&world, 8, 0)).len() <= MAX_TILES);
        // Not below the source's least zoom, and a small extent keeps its zoom.
        assert_eq!(fitting_zoom(&world, 8, 6), 6);
        assert_eq!(fitting_zoom(&Extent::new(0.0, 0.0, 1e5, 1e5), 12, 0), 12);
        assert_eq!(fitting_zoom(&Extent::new(f64::NAN, 0.0, 1.0, 1.0), 12, 0), 12);
    }

    #[test]
    fn covers_views() {
        let all = covering(&Extent::new(-HALF_WORLD, -HALF_WORLD, HALF_WORLD - 1.0, HALF_WORLD), 1);
        assert_eq!(all.len(), 4);
        // Past the antimeridian, the tiles wrap.
        let wrapped = covering(&Extent::new(HALF_WORLD + 1.0, 1.0, HALF_WORLD + 2.0, 2.0), 1);
        assert_eq!(wrapped.len(), 1);
        assert_eq!((wrapped[0].id.x, wrapped[0].wx), (0, 2));
        assert!(covering(&Extent::new(f64::NAN, 0.0, 1.0, 1.0), 3).is_empty());
        assert!(covering(&Extent::new(-HALF_WORLD, -HALF_WORLD, HALF_WORLD, HALF_WORLD), 30).is_empty());
        assert!(covering(&Extent::new(0.0, 0.0, 1e6, 1e6), 12).len() <= MAX_TILES);
    }

    #[test]
    fn zooms() {
        assert_eq!(zoom_for(156_543.03, 256, 22), 0);
        assert_eq!(zoom_for(156_543.03 / 1024.0, 256, 22), 10);
        assert_eq!(zoom_for(156_543.03 / 1024.0, 512, 22), 9);
        assert_eq!(zoom_for(0.001, 256, 19), 19);
        assert_eq!(zoom_for(f64::NAN, 256, 19), 0);
        assert!((style_zoom(78_271.517) - 0.0).abs() < 1e-3);
        let e = TileId { z: 1, x: 1, y: 0 }.extent();
        assert!((e.min_x - 0.0).abs() < 1e-6 && (e.max_y - HALF_WORLD).abs() < 1e-6);
    }

    #[test]
    fn joins() {
        assert_eq!(join("https://a.com/styles/x/style.json?k=1", "tiles.json"), "https://a.com/styles/x/tiles.json");
        assert_eq!(join("https://a.com/styles/x", "/planet"), "https://a.com/planet");
        assert_eq!(join("https://a.com/x", "//b.com/y"), "https://b.com/y");
        assert_eq!(join("https://a.com/x", "https://c.com/z"), "https://c.com/z");
        assert_eq!(join("https://a.com/x", "tiles.json?next=https://b.com"), "https://a.com/tiles.json?next=https://b.com");
        assert_eq!(join("https://a.com/x", "/tiles.json#https://b.com"), "https://a.com/tiles.json#https://b.com");
        assert_eq!(join("https://tiles.example?token=x", "/tile/0"), "https://tiles.example/tile/0");
        assert_eq!(join("https://tiles.example#view", "tile/0"), "https://tiles.example/tile/0");
    }

    #[test]
    fn direct_anonymous_sources_keep_one_marker() {
        let anonymous = crate::s3::anonymous("https://tiles.example/{z}/{x}/{y}");
        assert_eq!(join(&anonymous, &anonymous), anonymous);
        assert_eq!(join("https://tiles.example/style.json", &anonymous), anonymous);
        assert_eq!(join(&anonymous, "tile/0"), crate::s3::anonymous("https://tiles.example/{z}/{x}/tile/0"));
        let malformed_child = crate::s3::anonymous(&anonymous);
        let joined = join(&crate::s3::anonymous("https://tiles.example/style.json"), &malformed_child);
        assert!(crate::address::check(&joined, None).is_err(), "a malformed child was replaced with {joined}");
        assert_eq!(joined, malformed_child);
        let malformed_base = crate::s3::anonymous(&crate::s3::anonymous("https://tiles.example/style.json"));
        assert!(crate::address::check(&join(&malformed_base, "tile/0"), None).is_err());
    }
}
