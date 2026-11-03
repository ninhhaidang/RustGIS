//! Cloud-optimised GeoTIFFs by their web address, read in place: the file's header (its IFDs)
//! once, then only the tiles of the overview a map tile needs, by HTTP range requests. Each map
//! tile (`cog:{z}/{x}/{y}@<address>`) is drawn from them in Web Mercator, as a picture, so a COG is
//! a tile layer like a PMTiles archive: drawn as its tiles land, on the desktop and in the browser.
//!
//! Grey images are stretched between their 2nd and 98th percentiles (from the coarsest
//! overview); 8-bit colour images are shown as they are. An address may end with a [`Look`]
//! (`#bands=2,17,10&range=-0.3,0.3`): the bands shown as red, green and blue (or one, grey) and
//! their stretch. Bands stored apart (planar files, such as AlphaEarth's 64 embedding bands) are
//! read a band at a time, only those shown. Files are hostile input: header reads, tile counts and
//! sizes are bounded (see [`rustgis_data::raster::cog`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rustgis_data::raster::ColorHint;
use rustgis_data::raster::cog::{self, Layout, Parse, Sparse};
use rustgis_data::raster::crs::Transformer;
use rustgis_geom::Coord;
use web_time::{Duration, Instant};

use crate::cache::{Done, MAX_BYTES, RANGE};

/// The scheme of a map tile drawn from a COG.
pub const SCHEME: &str = "cog:";

/// The first read of a file: enough for a COG's IFDs, as GDAL writes them, in one request.
const HEAD: u64 = 64 << 10;
/// More of the header, when it's longer, is read in steps this large at first, doubling each
/// round ([`header_read`]).
const STEP: u64 = 256 << 10;
/// The most header reads before giving up.
const MAX_ROUNDS: usize = 12;
/// The most COG tiles one map tile may need (past that, a coarser overview is read).
const MAX_PER_TILE: usize = 16;
/// The most tiles of the coarsest overview read for the stretch.
const MAX_STATS_TILES: usize = 16;
/// Decoded tiles kept, in samples.
const BUDGET: usize = if cfg!(target_arch = "wasm32") { 24 << 20 } else { 64 << 20 };
/// A failed file is tried again after this long.
const RETRY: Duration = Duration::from_secs(60);
/// A map tile's side, in pixels.
const SIZE: u32 = 256;
/// Web Mercator's half extent.
const HALF: f64 = 20_037_508.342_789_244;

/// The template of a COG's map tiles (its address's [`Look`] kept).
pub fn template(url: &str) -> String {
    format!("{SCHEME}{{z}}/{{x}}/{{y}}@{url}")
}

/// The most looks' stretches a file keeps.
const MAX_LOOKS: usize = 64;
/// The most bands a [`Look`] names.
const MAX_LOOK_BANDS: usize = 3;
/// The highest band number a [`Look`] may name.
const MAX_BAND: usize = 65_535;

/// How a COG layer shows its bands, from the end of its address (`#bands=2,17,10&range=-0.3,0.3`,
/// bands counted from 1, `&dequantize=aef` for AlphaEarth's quantised embeddings).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    /// The bands shown (0-based): three as red, green and blue, or one, grey; empty for the file's
    /// own (its colour bands, else its first).
    pub bands: Vec<usize>,
    /// The values shown from black to full colour, for every band; `None` for each band's 2nd to
    /// 98th percentile.
    pub range: Option<(f32, f32)>,
    /// Values are AlphaEarth's quantised embeddings, shown as `sign(v)·(v/127.5)²`.
    pub aef: bool,
}

impl Look {
    /// The address part this look adds (empty for the file's own look).
    pub fn suffix(&self) -> String {
        let mut parts = Vec::new();
        if !self.bands.is_empty() {
            parts.push(format!("bands={}", self.bands.iter().map(|b| (b + 1).to_string()).collect::<Vec<_>>().join(",")));
        }
        if let Some((lo, hi)) = self.range {
            parts.push(format!("range={lo},{hi}"));
        }
        if self.aef {
            parts.push("dequantize=aef".into());
        }
        if parts.is_empty() { String::new() } else { format!("#{}", parts.join("&")) }
    }

    /// The look an address part says (`bands=…&range=…&dequantize=aef`); `None` when it isn't one.
    fn parse(part: &str) -> Option<Look> {
        let mut look = Look::default();
        for kv in part.split('&') {
            let (k, v) = kv.split_once('=')?;
            match k {
                "bands" => {
                    let bands: Vec<usize> = v.split(',').map(|b| b.trim().parse::<usize>().ok()).collect::<Option<_>>()?;
                    if bands.len() > MAX_LOOK_BANDS || bands.iter().any(|b| *b == 0 || *b > MAX_BAND) {
                        return None;
                    }
                    look.bands = bands.into_iter().map(|b| b - 1).collect();
                }
                "range" => {
                    let (lo, hi) = v.split_once(',')?;
                    let (lo, hi) = (lo.trim().parse::<f32>().ok()?, hi.trim().parse::<f32>().ok()?);
                    if !(lo.is_finite() && hi.is_finite() && hi > lo) {
                        return None;
                    }
                    look.range = Some((lo, hi));
                }
                "dequantize" => look.aef = v == "aef",
                _ => return None,
            }
        }
        Some(look)
    }
}

/// A COG's address without its [`Look`], and the look (the file's own when it names none).
pub fn split(url: &str) -> (&str, Look) {
    match url.rsplit_once('#') {
        Some((file, part)) => match Look::parse(part) {
            Some(look) => (file, look),
            None => (url, Look::default()),
        },
        None => (url, Look::default()),
    }
}

/// A COG's address showing `look` (any look it had replaced).
pub fn with_look(url: &str, look: &Look) -> String {
    format!("{}{}", split(url).0, look.suffix())
}

/// A value as a look shows it.
fn shown_value(v: f32, aef: bool) -> f32 {
    if aef {
        let q = v / 127.5;
        q * q * v.signum()
    } else {
        v
    }
}

/// Whether a tile address is a COG's.
pub fn is_tile(url: &str) -> bool {
    url.starts_with(SCHEME)
}

/// What a COG holds, ready to draw from.
pub struct Info {
    pub layout: Layout,
    header: Sparse,
    transformer: Transformer,
    /// Longitude/latitude bounds: west, south, east, north.
    pub bounds: [f64; 4],
    /// Its Web Mercator bounds: min x, min y, max x, max y.
    merc: [f64; 4],
    pub min_zoom: u8,
    /// The zoom whose pixels are about the size of the full image's.
    pub max_zoom: u8,
    /// Each shown band's (low, high) values, mapped to 0 and 255, by the bands shown and whether
    /// they are dequantised (the file's own bands' set once the coarsest overview is read, before
    /// the file is ready; others' as a look first needs them).
    stretch: Mutex<HashMap<(Vec<usize>, bool), Vec<(f32, f32)>>>,
    /// The stretches being read, with who waits for each: read once, however many map tiles
    /// ask for it first.
    stretching: Mutex<HashMap<(Vec<usize>, bool), Vec<StretchThen>>>,
    /// Stretches whose read failed, and when: drawn with the type's full range meanwhile (not
    /// kept), read again after [`RETRY`].
    stretch_failed: Mutex<HashMap<(Vec<usize>, bool), Instant>>,
    /// The file's own bands shown as red, green, blue (or one, grey), and alpha.
    rgb: bool,
    alpha: bool,
}

impl Info {
    /// The bands a look shows (0-based): three as red, green and blue (and the file's alpha, as a
    /// fourth, for its own), or one, grey.
    pub fn shown(&self, look: &Look) -> Vec<usize> {
        let n = self.layout.bands.max(1);
        // Bands the file has, as many as asked (one or three); otherwise the file's own look.
        let picked: Vec<usize> = look.bands.iter().copied().filter(|b| *b < n).collect();
        match picked.len() {
            1 | 3 if picked.len() == look.bands.len() => picked,
            _ if self.rgb && self.alpha => vec![0, 1, 2, 3],
            _ if self.rgb => vec![0, 1, 2],
            _ => vec![0],
        }
    }

    /// The bands' (low, high) values for a look, when known.
    fn stretch_for(&self, look: &Look, shown: &[usize]) -> Option<Vec<(f32, f32)>> {
        let colour = &shown[..shown.len().min(3)];
        if let Some(r) = look.range {
            return Some(vec![r; colour.len()]);
        }
        self.stretch.lock().unwrap_or_else(|e| e.into_inner()).get(&(colour.to_vec(), look.aef)).cloned()
    }
}

enum State {
    Ready(Arc<Info>),
    Loading(Vec<Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>>),
    Failed(String, Instant),
}

fn infos() -> &'static Mutex<HashMap<String, State>> {
    static I: OnceLock<Mutex<HashMap<String, State>>> = OnceLock::new();
    I.get_or_init(Mutex::default)
}

type Bands = Arc<cog::Tile>;

enum TileState {
    Ready(Bands, usize),
    Loading(Vec<Box<dyn FnOnce(Result<Bands, String>) + Send>>),
}

#[derive(Default)]
struct Tiles {
    map: HashMap<(String, usize, u32), TileState>,
    samples: usize,
}

fn tiles() -> &'static Mutex<Tiles> {
    static T: OnceLock<Mutex<Tiles>> = OnceLock::new();
    T.get_or_init(Mutex::default)
}

/// Forgets every file and tile, so they are read again (with [`crate::cache::clear`]).
pub(crate) fn forget() {
    infos().lock().unwrap_or_else(|e| e.into_inner()).retain(|_, s| matches!(s, State::Loading(_)));
    let mut t = tiles().lock().unwrap_or_else(|e| e.into_inner());
    t.map.retain(|_, s| matches!(s, TileState::Loading(_)));
    t.samples = 0;
}

/// Forgets what was read of one file (its info and its tiles), but not a read under way: a
/// failed read is made again with whatever login now signs the file's address.
pub(crate) fn forget_file(url: &str) {
    let mut all = infos().lock().unwrap_or_else(|e| e.into_inner());
    if all.get(url).is_some_and(|s| !matches!(s, State::Loading(_))) {
        all.remove(url);
    }
    drop(all);
    let mut t = tiles().lock().unwrap_or_else(|e| e.into_inner());
    t.map.retain(|(u, _, _), s| u != url || matches!(s, TileState::Loading(_)));
}

/// A file's info: `Ok(None)` while it is being read (asking for it starts the read).
pub fn info(url: &str) -> Result<Option<Arc<Info>>, String> {
    let url = split(url).0;
    {
        let i = infos().lock().unwrap_or_else(|e| e.into_inner());
        match i.get(url) {
            Some(State::Ready(info)) => return Ok(Some(info.clone())),
            Some(State::Loading(_)) => return Ok(None),
            Some(State::Failed(e, at)) if at.elapsed() < RETRY => return Err(e.clone()),
            _ => {}
        }
    }
    with_info(url, Box::new(|_| crate::cache::notify()));
    Ok(None)
}

fn with_info(url: &str, then: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    {
        let mut i = infos().lock().unwrap_or_else(|e| e.into_inner());
        match i.get_mut(url) {
            Some(State::Ready(info)) => {
                let info = info.clone();
                drop(i);
                then(Ok(info));
                return;
            }
            Some(State::Loading(waiting)) => {
                waiting.push(then);
                return;
            }
            Some(State::Failed(e, at)) if at.elapsed() < RETRY => {
                let e = e.clone();
                drop(i);
                then(Err(e));
                return;
            }
            _ => {
                i.insert(url.to_string(), State::Loading(vec![then]));
            }
        }
    }
    let key = url.to_string();
    load(
        url.to_string(),
        Box::new(move |r| {
            let waiting = {
                let mut i = infos().lock().unwrap_or_else(|e| e.into_inner());
                let next = match &r {
                    Ok(info) => State::Ready(info.clone()),
                    Err(e) => State::Failed(e.clone(), Instant::now()),
                };
                match i.insert(key, next) {
                    Some(State::Loading(w)) => w,
                    _ => Vec::new(),
                }
            };
            for w in waiting {
                w(r.clone());
            }
            crate::cache::notify();
        }),
    );
}

/// Reads `len` bytes at `at` of the file at `url`.
fn read(url: &str, at: u64, len: u64, done: Done) {
    if len == 0 {
        done(Ok(Vec::new()));
        return;
    }
    if len > MAX_BYTES as u64 {
        done(Err(format!("{len} bytes is too large to read from a GeoTIFF")));
        return;
    }
    let end = at.saturating_add(len) - 1;
    crate::cache::fetch_counted(format!("{url}{RANGE}{at}-{end}"), done);
}

/// Reads a file's header (then its stretch): the start, then whatever more the IFDs need.
///
/// A file an API makes on request (an address not named `.tif`, such as Vantor's
/// `…/geotiff?…`) is asked for its headers first: one that says how long its TIFF header is
/// (`X-TIFF-Header-Length`) has it read in one request. One that says it is JSON, text or a page
/// and doesn't say it answers ranges isn't read as a GeoTIFF at all (a server ignoring the range
/// would send its whole answer); a GeoTIFF mislabelled so on a server that answers ranges is still
/// read. Without an answer, the first [`HEAD`] bytes are read as for any other file.
fn load(url: String, done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    if let Err(e) = crate::address::check(&url, None) {
        done(Err(e));
        return;
    }
    if named_tiff(&url) {
        return read_header(url, HEAD, done);
    }
    let u = url.clone();
    crate::cache::head(
        &url,
        Box::new(move |h| {
            let h = h.unwrap_or_default();
            if h.content_type.as_deref().is_some_and(not_tiff_type) && !h.accepts_ranges && h.tiff_header_length.is_none() {
                return done(Err(format!("{u}: not a GeoTIFF")));
            }
            read_header(u, header_length(h.tiff_header_length), done);
        }),
    );
}

/// Whether an address is named as a TIFF (its path's extension).
fn named_tiff(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    path.ends_with(".tif") || path.ends_with(".tiff")
}

/// Whether a `Content-Type` is plainly not a TIFF: JSON, text, a page or XML (a service's error,
/// a feature collection). Anything else, `application/octet-stream` included, is read to see.
fn not_tiff_type(content_type: &str) -> bool {
    let t = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    t.starts_with("text/") || t.ends_with("json") || t.ends_with("xml") || t.ends_with("+json")
}

/// The first read of a file whose server announced its TIFF header's length: that much (one
/// request), bounded as any header read; [`HEAD`] without an announcement.
fn header_length(announced: Option<u64>) -> u64 {
    announced.filter(|n| *n >= 8).map_or(HEAD, |n| n.min(cog::MAX_HEADER).min(MAX_BYTES as u64))
}

/// Reads a file's first `len` bytes, then whatever more the IFDs need.
fn read_header(url: String, len: u64, done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    let u = url.clone();
    read(
        &url,
        0,
        len,
        Box::new(move |r| match r {
            Ok(b) if cog::is_tiff(&b) => step(u, Sparse::default().with(0, b), 0, done),
            Ok(_) => done(Err(format!("{u}: not a GeoTIFF"))),
            Err(e) => done(Err(format!("{u}: {e}"))),
        }),
    );
}

fn step(url: String, have: Sparse, rounds: usize, done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    match cog::layout(&have) {
        Ok(Parse::Done(layout)) => stats(url, have, *layout, done),
        Ok(Parse::NotTiled(why)) => done(Err(format!("{NOT_COG}: {why}"))),
        Ok(Parse::Need(a, b)) => {
            let Some(len) = header_read(&have, a, b, rounds) else {
                done(Err(format!("{url}: the GeoTIFF's header is too large or too scattered to read in place")));
                return;
            };
            let u = url.clone();
            read(
                &url,
                a,
                len,
                Box::new(move |r| match r {
                    // Nothing more to read: the file ends at or before `a`. Read again knowing so, a
                    // tag that points past the end (an optional one, GDAL's metadata or a colour
                    // table) is left out and said, as the whole-file read does; one the image needs
                    // fails to decode. Once only: a file whose length is known isn't asked past it.
                    Ok(more) if more.is_empty() && !have.len_known() => step(u, have.with_len(a), rounds + 1, done),
                    Ok(more) if more.is_empty() => done(Err(format!("{u}: the GeoTIFF is cut short"))),
                    Ok(more) => step(u, have.with(a, more), rounds + 1, done),
                    Err(e) => done(Err(format!("{u}: {e}"))),
                }),
            );
        }
        Err(e) => done(Err(format!("{url}: {e}"))),
    }
}

/// How many bytes of the header to read next, from `a`, when the layout stopped for `a..b`
/// after `rounds` reads; `None` past [`MAX_ROUNDS`] or [`cog::MAX_HEADER`]. The TIFF decoder
/// reads a tag's values 512 bytes at a time, so `a..b` is no longer than that even inside a long
/// array (a large image's tile index, 4 MB for MapBiomas's 346,092 tiles): the step doubles each
/// round, from [`STEP`] up to [`MAX_BYTES`], so such an index takes a few rounds, not one per
/// 256 KB. A read never takes what is held past [`cog::MAX_HEADER`], and stops where a part
/// already read starts, so no byte is held twice.
fn header_read(have: &Sparse, a: u64, b: u64, rounds: usize) -> Option<u64> {
    if rounds >= MAX_ROUNDS || have.held() >= cog::MAX_HEADER || a >= cog::MAX_HEADER.saturating_mul(64) {
        return None;
    }
    let room = cog::MAX_HEADER - have.held();
    // What the layout stopped for has to fit in what is left, or the read would be wasted.
    if b.saturating_sub(a) > room {
        return None;
    }
    // A part read after `a` (a round that went past it, or the tags at the header's end) isn't
    // read again.
    let gap = have.next_start(a).map_or(u64::MAX, |next| next - a);
    // `rounds` is below MAX_ROUNDS here, so the shift stays far from overflowing.
    Some(b.saturating_sub(a).max(STEP << rounds).min(MAX_BYTES as u64).min(room).min(gap))
}

/// What a file that isn't a cloud-optimised GeoTIFF says (it is read whole instead).
pub const NOT_COG: &str = "not a cloud-optimised GeoTIFF";

/// Reads the coarsest overview (a few tiles) for the stretch, then the file is ready.
fn stats(url: String, header: Sparse, layout: Layout, done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    let transformer = match layout.crs.transformer() {
        Ok(t) => t,
        Err(e) => {
            done(Err(format!("{url}: {e}")));
            return;
        }
    };
    let Some(full) = layout.levels.first().cloned() else {
        done(Err(format!("{url}: no image")));
        return;
    };
    // Bounds: the full image's edges, sampled (a projected square isn't square in degrees).
    let t = layout.transform;
    let (w, h) = (f64::from(full.width), f64::from(full.height));
    let mut lon = (f64::MAX, f64::MIN);
    let mut lat = (f64::MAX, f64::MIN);
    for i in 0..=8 {
        let f = f64::from(i) / 8.0;
        for (px, py) in [(f * w, 0.0), (f * w, h), (0.0, f * h), (w, f * h)] {
            let c = Coord::new(t.origin_x + px * t.pixel_width, t.origin_y + py * t.pixel_height);
            if let Some(ll) = transformer.to_lonlat(c) {
                lon = (lon.0.min(ll.x), lon.1.max(ll.x));
                lat = (lat.0.min(ll.y), lat.1.max(ll.y));
            }
        }
    }
    if lon.0 > lon.1 || lat.0 > lat.1 {
        done(Err(format!("{url}: the GeoTIFF's corners aren't on the globe")));
        return;
    }
    let bounds = [lon.0.max(-180.0), lat.0.max(-85.05), lon.1.min(180.0), lat.1.min(85.05)];
    let merc = {
        let a = rustgis_geom::lonlat_to_merc(Coord::new(bounds[0], bounds[1]));
        let b = rustgis_geom::lonlat_to_merc(Coord::new(bounds[2], bounds[3]));
        [a.x, a.y, b.x, b.y]
    };
    // The zoom whose pixels match the full image's.
    let res = (merc[2] - merc[0]).abs() / w.max(1.0);
    let max_zoom = if res > 0.0 { (2.0 * HALF / f64::from(SIZE) / res).log2().ceil().clamp(0.0, 24.0) as u8 } else { 18 };
    let rgb = layout.bands >= 3;
    let alpha = matches!(layout.color, ColorHint::Rgb { alpha: true });
    let bytes = layout.data_type == "UInt8";
    let info = Arc::new(Info {
        layout,
        header,
        transformer,
        bounds,
        merc,
        min_zoom: 0,
        max_zoom,
        stretch: Mutex::new(HashMap::new()),
        stretching: Mutex::new(HashMap::new()),
        stretch_failed: Mutex::new(HashMap::new()),
        rgb,
        alpha,
    });
    // 8-bit colour images are shown as they are; others between the coarsest overview's
    // percentiles.
    let own = Look::default();
    if bytes && rgb {
        info.stretch.lock().unwrap_or_else(|e| e.into_inner()).insert((vec![0, 1, 2], false), vec![(0.0, 255.0); 3]);
        done(Ok(info));
        return;
    }
    let i = info.clone();
    with_stretch(
        info,
        url.clone(),
        own,
        Box::new(move |r| match r {
            Ok(_) => done(Ok(i)),
            Err(e) => done(Err(format!("{url}: {e}"))),
        }),
    );
}

/// The image the stretch is read from: the coarsest at least 256 pixels wide (GDAL writes
/// overviews down to a pixel, too few to take percentiles of), else the full image.
fn stats_level(layout: &Layout) -> usize {
    layout.levels.iter().rposition(|l| l.width >= 256 && l.height >= 256).unwrap_or(0)
}

/// The full range of a file's type of values, as a look shows them (floats have none: 0–255 is a
/// rough default for them).
fn type_range(layout: &Layout, aef: bool) -> (f32, f32) {
    let r = match layout.data_type {
        "Int8" => (-128.0, 127.0),
        "Int16" => (-32768.0, 32767.0),
        "UInt16" => (0.0, 65535.0),
        "Int32" => (-2_147_483_648.0, 2_147_483_647.0),
        "UInt32" => (0.0, 4_294_967_295.0),
        _ => (0.0, 255.0),
    };
    (shown_value(r.0, aef), shown_value(r.1, aef))
}

/// Keeps a look's stretch: a few looks a file (a script trying many is hostile input); past them,
/// the others are read again when asked for.
fn keep_stretch(info: &Info, key: (Vec<usize>, bool), s: Vec<(f32, f32)>) {
    let own = (info.shown(&Look::default()).into_iter().take(3).collect::<Vec<_>>(), false);
    let mut kept = info.stretch.lock().unwrap_or_else(|e| e.into_inner());
    // Past the cap one other look goes, never the file's own (an 8-bit colour image's is "as it
    // is", not read).
    if kept.len() >= MAX_LOOKS
        && let Some(k) = kept.keys().find(|k| **k != own).cloned()
    {
        kept.remove(&k);
    }
    kept.insert(key, s);
}

/// The (low, high) values of a band's 2nd and 98th percentiles.
fn percentiles(mut v: Vec<f32>) -> (f32, f32) {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return (0.0, 1.0);
    }
    v.sort_by(f32::total_cmp);
    let at = |q: f64| v.get(((v.len() - 1) as f64 * q) as usize).copied().unwrap_or(0.0);
    let (lo, hi) = (at(0.02), at(0.98));
    if hi > lo { (lo, hi) } else { (lo, lo + 1.0) }
}

/// Calls `then` with a look's stretch: given, known, or read from the coarsest overview (a few
/// tiles of the bands shown) and kept.
type StretchThen = Box<dyn FnOnce(Result<Vec<(f32, f32)>, String>) + Send>;

fn with_stretch(info: Arc<Info>, file: String, look: Look, then: StretchThen) {
    let shown = info.shown(&look);
    if let Some(s) = info.stretch_for(&look, &shown) {
        then(Ok(s));
        return;
    }
    let colour: Vec<usize> = shown.iter().copied().take(3).collect();
    let coarsest = stats_level(&info.layout);
    let n = info.layout.levels.get(coarsest).map(|l| l.tiles() as usize).unwrap_or(0);
    // Too many tiles to read for it: the values' full range for the type.
    if n == 0 || n > MAX_STATS_TILES {
        let s = vec![type_range(&info.layout, look.aef); colour.len()];
        keep_stretch(&info, (colour, look.aef), s.clone());
        then(Ok(s));
        return;
    }
    // Read once: later asks wait for the read under way.
    let key = (colour.clone(), look.aef);
    // A read that failed lately: the type's full range for now.
    if info.stretch_failed.lock().unwrap_or_else(|e| e.into_inner()).get(&key).is_some_and(|at| at.elapsed() < RETRY) {
        then(Ok(vec![type_range(&info.layout, look.aef); colour.len()]));
        return;
    }
    {
        let mut reading = info.stretching.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(waiting) = reading.get_mut(&key) {
            waiting.push(then);
            return;
        }
        reading.insert(key.clone(), vec![then]);
    }
    let tiles: Vec<u32> = (0..n as u32).collect();
    let i = info.clone();
    let shown_bands = colour.len();
    let key_aef = look.aef;
    let done = move |r: Result<Vec<(f32, f32)>, String>| {
        let waiting = i.stretching.lock().unwrap_or_else(|e| e.into_inner()).remove(&key).unwrap_or_default();
        // An overview that can't be read: the type's full range for now (not kept: the failure may
        // pass), and no read again until RETRY has passed.
        let s = match r {
            Ok(s) => {
                keep_stretch(&i, key, s.clone());
                s
            }
            Err(_) => {
                let mut failed = i.stretch_failed.lock().unwrap_or_else(|e| e.into_inner());
                if failed.len() >= MAX_LOOKS {
                    failed.clear();
                }
                failed.insert(key, Instant::now());
                vec![type_range(&i.layout, key_aef); shown_bands]
            }
        };
        for w in waiting {
            w(Ok(s.clone()));
        }
    };
    fetch_bands(
        info,
        file,
        coarsest,
        tiles,
        colour.clone(),
        Box::new(move |r| match r {
            Ok(picked) => {
                let s: Vec<(f32, f32)> = (0..colour.len())
                    .map(|j| {
                        percentiles(
                            picked
                                .iter()
                                .filter_map(|t| t.get(j))
                                .filter_map(|(chunk, b)| chunk.get(*b))
                                .flat_map(|v| v.iter().map(|x| shown_value(*x, look.aef)))
                                .collect(),
                        )
                    })
                    .collect();
                done(Ok(s))
            }
            Err(e) => done(Err(e)),
        }),
    );
}

/// The bands of a tile a look shows: for each, the decoded chunk holding it and its place there.
type Picked = Vec<(Bands, usize)>;

/// Reads the bands `bands` (0-based) of some tiles of a level, then calls `then` with each tile's
/// (in the order asked). Bands stored apart are read alone; together, a tile's chunk once.
fn fetch_bands(
    info: Arc<Info>,
    file: String,
    level: usize,
    tiles: Vec<u32>,
    bands: Vec<usize>,
    then: Box<dyn FnOnce(Result<Vec<Picked>, String>) + Send>,
) {
    let Some(l) = info.layout.levels.get(level) else {
        then(Err("no such overview".into()));
        return;
    };
    let mut places = Vec::with_capacity(tiles.len() * bands.len());
    for &t in &tiles {
        for &b in &bands {
            match info.layout.chunk(l, t, b) {
                Some(p) => places.push(p),
                None => {
                    then(Err(format!("no band {} in tile {t}", b + 1)));
                    return;
                }
            }
        }
    }
    let mut chunks: Vec<u32> = places.iter().map(|(c, _)| *c).collect();
    chunks.sort_unstable();
    chunks.dedup();
    let want = chunks.clone();
    let nb = bands.len();
    fetch_all(
        info,
        file,
        level,
        want,
        Box::new(move |r| {
            then(r.and_then(|got| {
                if got.len() != chunks.len() {
                    return Err("a GeoTIFF tile went missing".into());
                }
                Ok(places
                    .chunks(nb.max(1))
                    .map(|tile| {
                        tile.iter()
                            .filter_map(|(c, at)| chunks.binary_search(c).ok().and_then(|i| got.get(i)).map(|b| (b.clone(), *at)))
                            .collect()
                    })
                    .collect())
            }))
        }),
    );
}

type Gathered = Box<dyn FnOnce(Result<Vec<Bands>, String>) + Send>;

/// Reads tiles of a level (each once, shared by the map tiles that need it), then calls `then`
/// with their bands in the order asked.
fn fetch_all(info: Arc<Info>, url: String, level: usize, wanted: Vec<u32>, then: Gathered) {
    if wanted.is_empty() {
        then(Ok(Vec::new()));
        return;
    }
    struct Gather {
        got: Vec<Option<Bands>>,
        left: usize,
        failed: Option<String>,
        then: Option<Gathered>,
    }
    let g = Arc::new(Mutex::new(Gather { got: vec![None; wanted.len()], left: wanted.len(), failed: None, then: Some(then) }));
    for (k, idx) in wanted.into_iter().enumerate() {
        let g = g.clone();
        get_tile(
            info.clone(),
            url.clone(),
            level,
            idx,
            Box::new(move |r| {
                let mut s = g.lock().unwrap_or_else(|e| e.into_inner());
                match r {
                    Ok(b) => {
                        if let Some(slot) = s.got.get_mut(k) {
                            *slot = Some(b);
                        }
                    }
                    Err(e) => s.failed = Some(e),
                }
                s.left = s.left.saturating_sub(1);
                if s.left == 0
                    && let Some(then) = s.then.take()
                {
                    let r = match s.failed.take() {
                        Some(e) => Err(e),
                        None => Ok(s.got.iter_mut().filter_map(Option::take).collect()),
                    };
                    drop(s);
                    then(r);
                }
            }),
        );
    }
}

/// One tile of a level, decoded: from the tiles kept, or read.
fn get_tile(info: Arc<Info>, url: String, level: usize, idx: u32, then: Box<dyn FnOnce(Result<Bands, String>) + Send>) {
    let Some(l) = info.layout.levels.get(level) else {
        then(Err("no such overview".into()));
        return;
    };
    let (Some(&at), Some(&len)) = (l.offsets.get(idx as usize), l.counts.get(idx as usize)) else {
        then(Err("no such tile".into()));
        return;
    };
    let nb = info.layout.chunk_bands();
    let per_band = (l.tile_width as usize) * (l.tile_height as usize);
    let samples = per_band * nb;
    // A tile the file leaves out (GDAL's sparse files: no bytes) is empty.
    if len == 0 {
        then(Ok(Arc::new(vec![vec![f32::NAN; per_band]; nb].into())));
        return;
    }
    // Bytes at offset 0 would be the file's header: a broken index, said rather than drawn blank.
    if at == 0 {
        then(Err(format!("tile {idx} of level {level} has bytes but no offset")));
        return;
    }
    let key = (url.clone(), level, idx);
    {
        let mut t = tiles().lock().unwrap_or_else(|e| e.into_inner());
        match t.map.get_mut(&key) {
            Some(TileState::Ready(b, _)) => {
                let b = b.clone();
                drop(t);
                then(Ok(b));
                return;
            }
            Some(TileState::Loading(w)) => {
                w.push(then);
                return;
            }
            None => {
                t.map.insert(key.clone(), TileState::Loading(vec![then]));
            }
        }
    }
    read(
        &url,
        at,
        len,
        Box::new(move |r| {
            let r = r
                .and_then(|b| cog::decode_tile_counted(&info.header, &info.layout, level, idx, b).map_err(|e| e.to_string()))
                .map(Arc::new);
            let waiting = {
                let mut t = tiles().lock().unwrap_or_else(|e| e.into_inner());
                let old = match &r {
                    Ok(b) => {
                        // Over the budget: tiles ready make room (the cache is a working set).
                        while t.samples + samples > BUDGET {
                            let Some(k) = t.map.iter().find(|(_, s)| matches!(s, TileState::Ready(..))).map(|(k, _)| k.clone()) else {
                                break;
                            };
                            if let Some(TileState::Ready(_, n)) = t.map.remove(&k) {
                                t.samples = t.samples.saturating_sub(n);
                            }
                        }
                        t.samples += samples;
                        t.map.insert(key, TileState::Ready(b.clone(), samples))
                    }
                    // A failed tile is read again by the next map tile that needs it.
                    Err(_) => t.map.remove(&key),
                };
                match old {
                    Some(TileState::Loading(w)) => w,
                    _ => Vec::new(),
                }
            };
            for w in waiting {
                w(r.clone());
            }
        }),
    );
}

/// `cog:z/x/y@address` → its parts.
fn parse_url(url: &str) -> Option<(u8, u32, u32, String)> {
    let rest = url.strip_prefix(SCHEME)?;
    let (zxy, archive) = rest.split_once('@')?;
    let mut p = zxy.split('/');
    let z: u8 = p.next()?.parse().ok()?;
    let x: u32 = p.next()?.parse().ok()?;
    let y: u32 = p.next()?.parse().ok()?;
    (z <= 30 && !archive.is_empty()).then(|| (z, x, y, archive.to_string()))
}

/// Draws a map tile (`cog:z/x/y@address`): empty where the file has nothing.
pub(crate) fn fetch(url: &str, done: Done) {
    let Some((z, x, y, archive)) = parse_url(url) else {
        done(Err(format!("{url}: not a COG tile address")));
        return;
    };
    let (file, look) = split(&archive);
    let file = file.to_string();
    let f = file.clone();
    with_info(
        &file,
        Box::new(move |info| match info {
            Ok(info) => {
                let i = info.clone();
                let f2 = f.clone();
                with_stretch(
                    info,
                    f,
                    look.clone(),
                    Box::new(move |s| match s {
                        Ok(s) => draw(i, f2, &look, &s, z, x, y, done),
                        Err(e) => done(Err(e)),
                    }),
                );
            }
            Err(e) => done(Err(e)),
        }),
    );
}

/// Where each pixel of a map tile falls in the file, at a level: (tile, sample) or none.
fn locate(info: &Info, level: usize, native: &[Option<Coord>]) -> Vec<Option<(u32, usize)>> {
    let Some(l) = info.layout.levels.get(level) else { return vec![None; native.len()] };
    let t = info.layout.transform_of(l);
    let (tw, th) = (l.tile_width.max(1), l.tile_height.max(1));
    native
        .iter()
        .map(|c| {
            let c = (*c)?;
            let px = (c.x - t.origin_x) / t.pixel_width;
            let py = (c.y - t.origin_y) / t.pixel_height;
            if !(px >= 0.0 && py >= 0.0 && px < f64::from(l.width) && py < f64::from(l.height)) {
                return None;
            }
            let (px, py) = (px as u32, py as u32);
            let idx = (py / th) * l.across() + px / tw;
            Some((idx, ((py % th) * tw + px % tw) as usize))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn draw(info: Arc<Info>, url: String, look: &Look, stretch: &[(f32, f32)], z: u8, x: u32, y: u32, done: Done) {
    let n = (1u64 << z) as f64;
    let size = 2.0 * HALF / n;
    let (x0, y1) = (-HALF + f64::from(x) * size, HALF - f64::from(y) * size);
    let [mx0, my0, mx1, my1] = info.merc;
    if x0 > mx1 || x0 + size < mx0 || y1 < my0 || y1 - size > my1 {
        done(Ok(Vec::new()));
        return;
    }
    // Native coordinates of every pixel's centre: exact on a grid every 16 pixels, between
    // them interpolated (projections are smooth at a tile's scale).
    const STEP_PX: usize = 16;
    let px = size / f64::from(SIZE);
    let s = SIZE as usize;
    let g = s / STEP_PX + 1;
    let grid: Vec<Option<Coord>> = (0..g * g)
        .map(|k| {
            let (i, j) = ((k % g) * STEP_PX, (k / g) * STEP_PX);
            info.transformer.from_merc(Coord::new(x0 + i as f64 * px, y1 - j as f64 * px))
        })
        .collect();
    let native: Vec<Option<Coord>> = (0..s * s)
        .map(|k| {
            let (i, j) = (k % s, k / s);
            let (fx, fy) = ((i as f64 + 0.5) / STEP_PX as f64, (j as f64 + 0.5) / STEP_PX as f64);
            let (gi, gj) = ((fx as usize).min(g - 2), (fy as usize).min(g - 2));
            let (tx, ty) = (fx - gi as f64, fy - gj as f64);
            let at = |a: usize, b: usize| grid.get(b * g + a).copied().flatten();
            match (at(gi, gj), at(gi + 1, gj), at(gi, gj + 1), at(gi + 1, gj + 1)) {
                (Some(a), Some(b), Some(c), Some(d)) => {
                    let top = (a.x + (b.x - a.x) * tx, a.y + (b.y - a.y) * tx);
                    let bottom = (c.x + (d.x - c.x) * tx, c.y + (d.y - c.y) * tx);
                    Some(Coord::new(top.0 + (bottom.0 - top.0) * ty, top.1 + (bottom.1 - top.1) * ty))
                }
                // Near a projection's edge: exactly.
                _ => info.transformer.from_merc(Coord::new(x0 + (i as f64 + 0.5) * px, y1 - (j as f64 + 0.5) * px)),
            }
        })
        .collect();
    // The output pixel's size in the file's units, from the grid across the tile.
    let target = match (grid.first().copied().flatten(), grid.get(g - 1).copied().flatten()) {
        (Some(a), Some(b)) => ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt() / s as f64,
        _ => f64::MAX,
    };
    // The coarsest overview fine enough for the tile; a coarser one when too many tiles of it
    // are needed.
    let levels = &info.layout.levels;
    let mut level = levels
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| info.layout.transform_of(l).pixel_width.abs() <= target * 1.25)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let (spots, wanted) = loop {
        let spots = locate(&info, level, &native);
        let mut wanted: Vec<u32> = spots.iter().flatten().map(|(t, _)| *t).collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.len() <= MAX_PER_TILE || level + 1 >= levels.len() {
            break (spots, wanted);
        }
        level += 1;
    };
    if wanted.is_empty() {
        done(Ok(Vec::new()));
        return;
    }
    if wanted.len() > MAX_PER_TILE * 4 {
        done(Err("too many GeoTIFF tiles for one map tile".into()));
        return;
    }
    let shown = info.shown(look);
    let pen = Pen { rgb: shown.len() >= 3, alpha: shown.len() >= 4, aef: look.aef, stretch: stretch.to_vec() };
    let w2 = wanted.clone();
    fetch_bands(
        info,
        url,
        level,
        wanted,
        shown,
        Box::new(move |r| match r {
            Ok(bands) => done(paint(&pen, &spots, &w2, &bands)),
            Err(e) => done(Err(e)),
        }),
    );
}

/// How a map tile's pixels are coloured from the bands shown.
struct Pen {
    /// Three bands as red, green and blue (else one, grey), and a fourth as alpha.
    rgb: bool,
    alpha: bool,
    aef: bool,
    stretch: Vec<(f32, f32)>,
}

/// The map tile's pixels, from the tiles read, as a raw picture (none when nothing is there).
fn paint(pen: &Pen, spots: &[Option<(u32, usize)>], wanted: &[u32], tiles: &[Picked]) -> Result<Vec<u8>, String> {
    let scale = |b: usize, v: f32| -> u8 {
        let (lo, hi) = pen.stretch.get(b).copied().unwrap_or((0.0, 255.0));
        let v = shown_value(v, pen.aef);
        (((v - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0).round() as u8
    };
    let mut rgba = vec![0u8; spots.len() * 4];
    let mut any = false;
    for (k, spot) in spots.iter().enumerate() {
        let Some((t, at)) = spot else { continue };
        let Some(bands) = wanted.binary_search(t).ok().and_then(|i| tiles.get(i)) else { continue };
        let v = |b: usize| bands.get(b).and_then(|(chunk, i)| chunk.get(*i)).and_then(|band| band.get(*at)).copied().unwrap_or(f32::NAN);
        let px = if pen.rgb {
            let (r, g, b) = (v(0), v(1), v(2));
            if !(r.is_finite() && g.is_finite() && b.is_finite()) {
                continue;
            }
            let a = if pen.alpha { v(3) } else { 255.0 };
            if pen.alpha && (a.is_nan() || a <= 0.0) {
                continue;
            }
            [scale(0, r), scale(1, g), scale(2, b), a.clamp(0.0, 255.0) as u8]
        } else {
            let g = v(0);
            if !g.is_finite() {
                continue;
            }
            let g = scale(0, g);
            [g, g, g, 255]
        };
        if let Some(out) = rgba.get_mut(k * 4..k * 4 + 4) {
            out.copy_from_slice(&px);
            any = true;
        }
    }
    if !any {
        return Ok(Vec::new());
    }
    bleed(&mut rgba, spots);
    let mut out = Vec::with_capacity(crate::cache::RAW.len() + 8 + rgba.len());
    out.extend_from_slice(crate::cache::RAW);
    out.extend_from_slice(&SIZE.to_le_bytes());
    out.extend_from_slice(&SIZE.to_le_bytes());
    out.extend_from_slice(&rgba);
    Ok(out)
}

/// Carries the file's edge one pixel out: each pixel outside the file (`spots` none) beside one
/// painted, or touching one at a corner, takes its colour. A map tile is drawn scaled with
/// bilinear filtering, which turns the step from painted to empty into a half-transparent pixel;
/// at the edge between two files laid side by side (a data cube's tiles) the two half-transparent
/// edges let the map show through as a thin line. Carried out, the step falls outside the file and
/// its own pixels stay opaque. Pixels with no data inside the file are left empty. Where no file
/// lies beside, at the data's outer edge, the picture grows by that pixel (1/256 of a map tile).
/// The colour is copied whole, alpha included, so a half-transparent edge is carried out as it is.
/// A file's edge on a map tile's own edge needs none: a tile is drawn clamped at its edges, and the
/// pixels past them are the next tile's.
fn bleed(rgba: &mut [u8], spots: &[Option<(u32, usize)>]) {
    let s = SIZE as usize;
    let painted = |k: usize| rgba.get(k * 4 + 3).is_some_and(|a| *a > 0);
    // A pixel of the file with a colour: what the edge is carried out from.
    let source = |m: usize| spots.get(m).is_some_and(Option::is_some) && painted(m);
    let mut fill: Vec<(usize, usize)> = Vec::new();
    for (k, spot) in spots.iter().enumerate() {
        if spot.is_some() || painted(k) {
            continue;
        }
        let (i, j) = (k % s, k / s);
        let (west, east, north, south) = (i > 0, i + 1 < s, j > 0, j + 1 < s);
        // Beside first, then at a corner, where filtering at the file's corner reaches too.
        let near = [
            west.then(|| k - 1),
            east.then(|| k + 1),
            north.then(|| k - s),
            south.then(|| k + s),
            (north && west).then(|| k - s - 1),
            (north && east).then(|| k - s + 1),
            (south && west).then(|| k + s - 1),
            (south && east).then(|| k + s + 1),
        ];
        if let Some(from) = near.into_iter().flatten().find(|m| source(*m)) {
            fill.push((k, from));
        }
    }
    for (k, from) in fill {
        rgba.copy_within(from * 4..from * 4 + 4, k * 4);
    }
}

/// Whether a failure says the file is a GeoTIFF to read whole rather than in place.
fn read_whole(e: &str) -> bool {
    e.contains(NOT_COG) || e.contains("not a GeoTIFF")
}

/// Whether the file at `url` is a cloud-optimised GeoTIFF, waiting for the answer (the command
/// line): `Ok(false)` for any other file, read whole instead.
#[cfg(not(target_arch = "wasm32"))]
pub fn probe_blocking(url: &str) -> Result<bool, String> {
    let url = split(url).0;
    let (tx, rx) = std::sync::mpsc::channel();
    with_info(url, Box::new(move |r| tx.send(r.map(|_| ())).unwrap_or_default()));
    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(())) => Ok(true),
        Ok(Err(e)) if read_whole(&e) => Ok(false),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("{url}: no answer in a minute")),
    }
}

/// Whether the file at `url` is a cloud-optimised GeoTIFF, off the frame (the app): `then` gets
/// `Ok(false)` for any other file, read whole instead.
pub fn probe(url: &str, then: Box<dyn FnOnce(Result<bool, String>) + Send>) {
    let url = split(url).0;
    with_info(
        url,
        Box::new(move |r| {
            then(match r {
                Ok(_) => Ok(true),
                Err(e) if read_whole(&e) => Ok(false),
                Err(e) => Err(e),
            })
        }),
    );
}

/// The most tiles of a file one read of values takes.
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_VALUE_TILES: usize = 4096;

/// The most values one read returns: 64 million, 256 MB, refused before anything is allocated.
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_VALUES_READ: u64 = 64_000_000;

/// A file's header, waiting for it: for an analysis that reads the values (opengeos/RustGIS#579).
#[cfg(not(target_arch = "wasm32"))]
pub fn info_blocking(url: &str) -> Result<Arc<Info>, String> {
    let url = split(url).0;
    let (tx, rx) = std::sync::mpsc::channel();
    with_info(url, Box::new(move |r| tx.send(r).unwrap_or_default()));
    rx.recv_timeout(Duration::from_secs(120)).map_err(|_| format!("{url}: no answer in two minutes"))?
}

/// Band `band`'s values (0-based) in a window of a level: columns `cols.0..cols.1` and rows
/// `rows.0..rows.1` of that level's grid, row by row, waiting for the tiles. No-data, and the
/// window's pixels outside the file, are NaN. The values are as stored (no scale or offset).
/// A read returns up to [`MAX_VALUES_READ`] values (256 MB): a caller reading on several threads
/// bounds its windows itself (a STAC trend's slices are a million pixels at most). It waits up to
/// two minutes for the tiles, as [`info_blocking`] does for the header.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_values_blocking(
    info: &Arc<Info>,
    url: &str,
    level: usize,
    band: usize,
    cols: (u32, u32),
    rows: (u32, u32),
) -> Result<Vec<f32>, String> {
    read_values_counted_blocking(info, url, level, band, cols, rows).map(|(values, _, _)| values)
}

/// [`read_values_blocking`], with how many of the window's values `f32` rounded (32- and 64-bit
/// whole numbers and 64-bit floats it doesn't hold exactly; no-data isn't counted), and how many
/// it can't hold at all (past its range: left out, NaN).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_values_counted_blocking(
    info: &Arc<Info>,
    url: &str,
    level: usize,
    band: usize,
    cols: (u32, u32),
    rows: (u32, u32),
) -> Result<(Vec<f32>, u64, u64), String> {
    let url = split(url).0.to_string();
    let l = info.layout.levels.get(level).ok_or("no such overview")?;
    if band >= info.layout.bands.max(1) {
        return Err(format!("the file has {} band(s)", info.layout.bands));
    }
    // The window's size checked before its values are allocated: a caller's window, not the file.
    let (w, h) = (cols.1.saturating_sub(cols.0) as usize, rows.1.saturating_sub(rows.0) as usize);
    let n = (w as u64).saturating_mul(h as u64);
    if n > MAX_VALUES_READ {
        return Err(format!("a window of {w} × {h} values; one read takes {MAX_VALUES_READ} at most"));
    }
    // The part of the window inside the file.
    let (x0, x1) = (cols.0.min(l.width), cols.1.min(l.width));
    let (y0, y1) = (rows.0.min(l.height), rows.1.min(l.height));
    if x1 <= x0 || y1 <= y0 {
        return Ok((rustgis_data::raster::filled(w, h, f32::NAN).map_err(|e| e.to_string())?, 0, 0));
    }
    let (tw, th, across) = (l.tile_width.max(1), l.tile_height.max(1), l.across().max(1));
    // The tiles the window takes, counted before they are listed, in checked arithmetic: a
    // hostile header can't make a long list or a wrapped index.
    let (c0, c1, r0, r1) = (x0 / tw, (x1 - 1) / tw, y0 / th, (y1 - 1) / th);
    let count = (u64::from(c1 - c0) + 1) * (u64::from(r1 - r0) + 1);
    if count > MAX_VALUE_TILES as u64 {
        return Err(format!("{count} tiles of the file for one read; at most {MAX_VALUE_TILES}: take a coarser level or a smaller area"));
    }
    let tiles: Vec<u32> =
        (r0..=r1).flat_map(|ty| (c0..=c1).filter_map(move |tx| ty.checked_mul(across).and_then(|t| t.checked_add(tx)))).collect();
    // Reserved first: a window the machine can't hold is an error, not a crash (#1291).
    let (mut out, mut rounded, mut overflowed) = (rustgis_data::raster::filled(w, h, f32::NAN).map_err(|e| e.to_string())?, 0u64, 0u64);
    let (tx, rx) = std::sync::mpsc::channel();
    fetch_bands(info.clone(), url.clone(), level, tiles.clone(), vec![band], Box::new(move |r| tx.send(r).unwrap_or_default()));
    let picked = rx.recv_timeout(Duration::from_secs(120)).map_err(|_| format!("{url}: tiles not read in two minutes"))??;
    let nodata = info.layout.nodata.map(|v| v as f32);
    for (&t, p) in tiles.iter().zip(&picked) {
        let Some((chunk, b)) = p.first() else { continue };
        let Some(values) = chunk.get(*b) else { continue };
        let (tx0, ty0) = ((t % across).saturating_mul(tw), (t / across).saturating_mul(th));
        for py in ty0.max(y0)..ty0.saturating_add(th).min(y1) {
            for px in tx0.max(x0)..tx0.saturating_add(tw).min(x1) {
                let i = (py - ty0) as usize * tw as usize + (px - tx0) as usize;
                let Some(&v) = values.get(i) else { continue };
                // A marked value past `f32`'s range is infinite: left out as no value, and
                // counted apart from those rounded. No-data isn't marked.
                if chunk.rounded_at(*b, i) {
                    if v.is_infinite() { overflowed += 1 } else { rounded += 1 }
                }
                if v.is_finite()
                    && Some(v) != nodata
                    && let Some(slot) = out.get_mut((py - rows.0) as usize * w + (px - cols.0) as usize)
                {
                    *slot = v;
                }
            }
        }
    }
    Ok((out, rounded, overflowed))
}

#[cfg(test)]
mod tests {
    #[test]
    fn announced_header_lengths_and_types() {
        use super::{HEAD, header_length, named_tiff, not_tiff_type};
        assert_eq!(header_length(Some(8192)), 8192);
        assert_eq!(header_length(None), HEAD);
        assert_eq!(header_length(Some(4)), HEAD, "too short for a TIFF header");
        assert_eq!(header_length(Some(u64::MAX)), crate::cache::MAX_BYTES as u64, "bounded as any read");
        for t in ["application/json", "application/geo+json; charset=utf-8", "text/html", "application/xml", "TEXT/PLAIN"] {
            assert!(not_tiff_type(t), "{t}");
        }
        for t in ["image/tiff", "image/tiff; application=geotiff", "application/octet-stream", ""] {
            assert!(!not_tiff_type(t), "{t}");
        }
        assert!(named_tiff("https://x.org/a/scene.TIF?sig=1") && !named_tiff("https://x.org/a/geotiff?f=tif"));
    }

    use super::*;

    #[test]
    fn tile_addresses() {
        assert_eq!(template("https://x.org/a.tif"), "cog:{z}/{x}/{y}@https://x.org/a.tif");
        assert_eq!(parse_url("cog:3/2/1@https://x.org/a.tif"), Some((3, 2, 1, "https://x.org/a.tif".into())));
        assert!(parse_url("cog:99/0/0@x").is_none() && parse_url("cog:1/0/0@").is_none() && !is_tile("pmtiles:1/0/0@x"));
    }

    #[test]
    fn stretch_read_from_an_overview_large_enough() {
        let file = rustgis_data::raster::cog::example(1024, 64);
        let rustgis_data::raster::cog::Parse::Done(l) = cog::layout(&Sparse::default().with(0, file)).unwrap() else { panic!() };
        // Overviews down to 64 pixels: the 256-pixel one is read.
        assert_eq!(l.levels.last().map(|l| l.width), Some(64));
        assert_eq!(l.levels[stats_level(&l)].width, 256);
    }

    /// Reads `file`'s layout as [`load`] and [`step`] do, a range at a time: the layout, how many
    /// reads past the first it took and the bytes held, or the reads made when it gave up.
    fn layout_by_ranges(file: &[u8]) -> Result<(Box<Layout>, usize, u64), usize> {
        let part = |a: u64, len: u64| file.get(a as usize..file.len().min((a + len) as usize)).unwrap_or_default().to_vec();
        let mut have = Sparse::default().with(0, part(0, HEAD));
        let mut rounds = 0;
        loop {
            match cog::layout(&have) {
                Ok(Parse::Done(layout)) => return Ok((layout, rounds, have.held())),
                Ok(Parse::Need(a, b)) => {
                    let len = header_read(&have, a, b, rounds).ok_or(rounds)?;
                    let more = part(a, len);
                    // An empty read past the end of a file whose length isn't known: read again
                    // knowing it, as `step` does (once).
                    have = if more.is_empty() && !have.len_known() { have.with_len(a) } else { have.with(a, more) };
                    rounds += 1;
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// GDAL's metadata tag pointing past the end of a file whose length isn't known: the empty
    /// read that comes back gives the length, as in [`step`], and the layout is read with the tag
    /// left out and said.
    #[test]
    fn a_tag_past_the_end_of_a_file_of_unknown_length_is_left_out() {
        use rustgis_data::raster::cog::{Example, example_with};
        let mut bad = example_with(&Example { size: 512, tile: 256, classes: true, ..Example::default() });
        let n = usize::from(u16::from_le_bytes([bad[8], bad[9]]));
        let len = u32::try_from(bad.len()).unwrap();
        let entry = (0..n).map(|k| 10 + k * 12).find(|&e| bad[e..e + 2] == [0x80, 0xA4]).unwrap();
        bad[entry + 8..entry + 12].copy_from_slice(&(len + 1000).to_le_bytes());
        let (l, ..) = layout_by_ranges(&bad).expect("read");
        let said: Vec<&String> = l.lost.iter().chain(l.described.iter().flat_map(|d| d.lost.iter())).collect();
        assert!(said.iter().any(|x| x.contains("band and class names are left out")), "{said:?}");
    }

    /// The header alone of a tiled GeoTIFF, `side` pixels a side in 256-pixel tiles, laid out as
    /// MapBiomas's: the tiles' byte counts and offsets right after the IFD, its georeferencing
    /// after them. The tiles themselves aren't there.
    fn big_index(side: u32) -> Vec<u8> {
        let n = side.div_ceil(256).pow(2);
        let counts_at = 8 + 2 + 12 * 14 + 4;
        let offsets_at = counts_at + 4 * n;
        let scale_at = offsets_at + 4 * n;
        let (tie_at, keys_at) = (scale_at + 24, scale_at + 72);
        // (tag, type, count, value or where the values are); 3 short, 4 long, 12 double.
        let entries: [(u16, u16, u32, u32); 14] = [
            (256, 4, 1, side),
            (257, 4, 1, side),
            (258, 3, 1, 8),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (277, 3, 1, 1),
            (284, 3, 1, 1),
            (322, 3, 1, 256),
            (323, 3, 1, 256),
            (324, 4, n, offsets_at),
            (325, 4, n, counts_at),
            (33550, 12, 3, scale_at),
            (33922, 12, 6, tie_at),
            (34735, 3, 12, keys_at),
        ];
        let mut out = b"II*\0".to_vec();
        out.extend(8u32.to_le_bytes());
        out.extend(14u16.to_le_bytes());
        for (tag, kind, count, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        out.extend(0u32.to_le_bytes());
        let data_at = keys_at + 24;
        out.extend((0..n).flat_map(|_| 1u32.to_le_bytes()));
        out.extend((0..n).flat_map(|i| (data_at + i).to_le_bytes()));
        let px = 1.0 / 3600.0;
        out.extend([px, px, 0.0, 0.0, 0.0, 0.0, 0.0, -74.0, 6.0, 0.0].iter().flat_map(|v: &f64| v.to_le_bytes()));
        out.extend([1u16, 1, 0, 2, 1024, 0, 1, 2, 2048, 0, 1, 4326].iter().flat_map(|v| v.to_le_bytes()));
        out
    }

    /// A tile index of 3.9 MB (490,000 tiles) is read within the rounds allowed: the decoder asks
    /// for 512 bytes at a time, and steps of a fixed 256 KB stopped at about 3 MB.
    #[test]
    fn a_large_tile_index_is_read_in_a_few_rounds() {
        let file = big_index(700 * 256);
        let (layout, rounds, held) = layout_by_ranges(&file).unwrap_or_else(|r| panic!("gave up after {r} reads"));
        assert_eq!(layout.levels.len(), 1);
        assert_eq!(layout.levels[0].offsets.len(), 490_000);
        assert_eq!(layout.levels[0].width, 179_200);
        assert!(rounds <= 6, "{rounds} reads");
        // No byte is read twice: a read stops where a part already read starts.
        assert!(held <= file.len() as u64, "{held} bytes held of a {}-byte header", file.len());
        // A cloud-optimised file's header is in its first read, as before.
        assert_eq!(layout_by_ranges(&rustgis_data::raster::cog::example(1024, 64)).map(|(_, r, _)| r), Ok(0));
        // A read stops at the header's cap, however large the step has grown.
        let near = Sparse::default().with(0, vec![0; cog::MAX_HEADER as usize - 1000]);
        assert_eq!(header_read(&near, cog::MAX_HEADER, cog::MAX_HEADER + 512, 8), Some(1000));
        assert_eq!(header_read(&near, cog::MAX_HEADER, cog::MAX_HEADER + 1001, 8), None, "more than is left");
        assert_eq!(header_read(&near.with(cog::MAX_HEADER, vec![0; 1000]), cog::MAX_HEADER + 1000, cog::MAX_HEADER + 1512, 9), None);
    }

    #[test]
    fn looks_in_addresses() {
        let u = "https://x.org/a.tiff";
        let look = Look { bands: vec![1, 16, 9], range: Some((-0.3, 0.3)), aef: true };
        let shown = with_look(u, &look);
        assert_eq!(shown, "https://x.org/a.tiff#bands=2,17,10&range=-0.3,0.3&dequantize=aef");
        assert_eq!(split(&shown), (u, look));
        // Another look replaces it; the file's own adds nothing.
        assert_eq!(with_look(&shown, &Look::default()), u);
        // A template keeps the look, so each look's map tiles are their own.
        let (_, _, _, archive) = parse_url(&template(&shown).replace("{z}/{x}/{y}", "1/0/0")).unwrap();
        assert_eq!(split(&archive).0, u);
        // Anything else after `#` is part of the address; bad looks are no looks.
        for bad in ["#page=2", "#bands=0,1,2", "#bands=1,2,3,4", "#range=1,1", "#range=x,1", "#bands=99999999999", "#bands="] {
            let a = format!("{u}{bad}");
            assert_eq!(split(&a), (a.as_str(), Look::default()), "{bad}");
        }
        assert!((shown_value(127.5, true) - 1.0).abs() < 1e-6 && (shown_value(-63.75, true) + 0.25).abs() < 1e-6);
        assert_eq!(shown_value(-5.0, false), -5.0);
    }

    #[test]
    fn paints_the_bands_shown() {
        let chunk = |v: f32| -> Bands { Arc::new(vec![vec![v; 4]].into()) };
        // One tile, its three bands from three chunks (stored apart); pixel 1 has no data.
        let mut red = vec![10.0; 4];
        red[1] = f32::NAN;
        let tile: Picked = vec![(Arc::new(vec![red].into()), 0), (chunk(20.0), 0), (chunk(30.0), 0)];
        let spots: Vec<Option<(u32, usize)>> = (0..(SIZE * SIZE) as usize).map(|k| if k < 2 { Some((7, k)) } else { None }).collect();
        let pen = Pen { rgb: true, alpha: false, aef: false, stretch: vec![(0.0, 40.0); 3] };
        let out = paint(&pen, &spots, &[7], &[tile]).unwrap();
        let px = &out[crate::cache::RAW.len() + 8..];
        assert_eq!(&px[..4], &[64, 128, 191, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 0]);
        // Outside the file, beside a painted pixel: its colour, carried out (pixel 0's below);
        // beside only empty ones, nothing.
        let below = SIZE as usize * 4;
        assert_eq!(&px[below..below + 4], &[64, 128, 191, 255]);
        assert_eq!(&px[below + 4..below + 8], &[64, 128, 191, 255], "touching pixel 0 only at a corner");
        assert_eq!(&px[8..12], &[0, 0, 0, 0], "beside pixel 1, which has no data");
        assert_eq!(&px[below + 8..below + 12], &[0, 0, 0, 0]);
        // At the tile's own border: a file pixel in the top row's last column is carried left, down
        // and down-left, not on into the next row's first pixel.
        let s = SIZE as usize;
        let last: Vec<Option<(u32, usize)>> = (0..s * s).map(|k| (k == s - 1).then_some((7, 0))).collect();
        let tile: Picked = vec![(chunk(10.0), 0), (chunk(20.0), 0), (chunk(30.0), 0)];
        let out = paint(&pen, &last, &[7], &[tile]).unwrap();
        let px = &out[crate::cache::RAW.len() + 8..];
        let at = |k: usize| &px[k * 4..k * 4 + 4];
        for k in [s - 2, 2 * s - 2, 2 * s - 1] {
            assert_eq!(at(k), &[64, 128, 191, 255], "pixel {k}");
        }
        assert_eq!(at(s), &[0, 0, 0, 0], "the next row's first pixel");
        // Nothing there: no picture.
        assert!(paint(&pen, &vec![None; spots.len()], &[], &[]).unwrap().is_empty());
    }
}
