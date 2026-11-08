//! OGC web services: what a service offers, from its capabilities document — WMS and WMTS map
//! layers (as tile templates in Web Mercator), WFS feature types, OGC API Features collections
//! (as GeoJSON addresses) and OGC API Tiles tilesets (map and vector tiles in WebMercatorQuad) —
//! and ArcGIS REST services (FeatureServer, MapServer, ImageServer,
//! VectorTileServer). The documents are hostile input: no DTD is read, nodes are capped, and
//! only layers RustGIS can draw are listed (the rest are counted in a note).

use roxmltree::{Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::rc::Rc;

#[path = "ogc_info.rs"]
mod info;
#[path = "ogc_warp.rs"]
pub mod warp;
pub use info::{
    FeatureInfo, InfoFeature, MAX_INFO_BYTES, answer_text, at_map_time, map_time, may_answer_in_another_format, read_feature_info,
    wms_layer_of,
};

/// What a layer of a service becomes.
#[derive(Clone, Debug, PartialEq)]
pub enum Offer {
    /// Map tiles: an XYZ template (`{z}/{x}/{y}`, or `{bbox-epsg-3857}` for WMS).
    Tiles { template: String },
    /// Features: an address answering GeoJSON.
    Features { url: String },
    /// A MapLibre style document (an ArcGIS vector tile service's).
    Style { url: String },
    /// Vector tiles (`{z}/{x}/{y}`, Mapbox Vector Tiles) and the names of their layers, as an OGC
    /// API Tiles collection gives them.
    VectorTiles { template: String, layers: Vec<String> },
    /// A WCS coverage, added as a raster from GetCoverage (a GeoTIFF of the view, or of its
    /// bounds).
    Coverage(Coverage),
    /// Another service or a folder that an ArcGIS services directory lists: opened (its own
    /// layers listed) rather than added.
    Service { url: String },
}

/// A layer's time dimension (a WMTS `Dimension` named `Time`, a WMS `Dimension` named `time`):
/// its tile template has `{Time}`, which takes the default unless a time is asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerTime {
    /// The time the service draws when none is given (`default` is GIBS's latest day).
    pub default: String,
    /// The times or ranges it lists (`2000-02-24/2026-10-19/P1D`), the first few.
    pub values: Vec<String>,
    /// Whether several times may be asked for at once: a list or an interval (a WMS layer's
    /// `multipleValues`, WMS 1.3.0 Annex C.2); otherwise one time.
    pub multiple: bool,
    /// How many of its times are left out: the earliest past `MAX_TIME_VALUES`, and those past
    /// [`MAX_DOCUMENT_TIME_VALUES`], kept for all of a document's layers together.
    pub cut: usize,
}

/// Most time values kept for a layer: enough for every year of a century and more (swisstopo's
/// aerial photos list 92), as the time slider steps through them.
pub const MAX_TIME_VALUES: usize = 1000;

/// Most time values of a layer's dimension read for its default and latest, the last it lists
/// (each a few bytes of the document, whose size is capped).
const MAX_TIMES_READ: usize = 20_000;

/// The last `n` of `values` (lists of times run from the earliest to the latest).
fn last(mut values: Vec<String>, n: usize) -> Vec<String> {
    values.drain(..values.len().saturating_sub(n));
    values
}

/// Most time values kept for all of a WMS document's layers together (swisstopo's 56 list about
/// 2,000).
pub const MAX_DOCUMENT_TIME_VALUES: usize = 50_000;

/// The most steps the time slider takes (`rustgis_map::time::MAX_STEPS`).
const MAX_SLIDER_STEPS: usize = 20_000;

/// The date formats a layer's times may be written in, as `{date:…}` takes them: years, compact
/// dates (swisstopo's `19951231`), months, dates, date-times (with milliseconds, as GeoServer and
/// ncWMS write them, always zero there).
const DATE_FORMATS: [&str; 9] = [
    "YYYY",
    "YYYYMMDD",
    "YYYY-MM",
    "YYYY-MM-DD",
    "YYYY-MM-DD[T]HH:mm:ss[Z]",
    "YYYY-MM-DD[T]HH:mm:ss[.000Z]",
    "YYYY-MM-DD[T]HH:mm[Z]",
    "YYYY-MM-DD[T]HH:mm:ss",
    "YYYY-MM-DD[T]HH:mm",
];

/// The start of the year 3000 (milliseconds since 1970): times past it stand for the latest
/// (swisstopo's `9999`, `99991231`), no time the slider steps to.
const LATEST_STAND_IN: f64 = 32_503_680_000_000.0;

/// How a layer's times go on the time slider: the format they are written in (`{date:…}`'s), the
/// slider's steps, and the time it starts at (the layer's default, else its latest).
#[derive(Clone, Debug, PartialEq)]
pub struct SliderTime {
    pub format: &'static str,
    pub steps: SliderSteps,
    pub at: Option<f64>,
}

/// The time slider's steps for a layer: the times it lists, or every so often between two (one
/// period it lists, `1952/2026/P1Y`).
#[derive(Clone, Debug, PartialEq)]
pub enum SliderSteps {
    Dates(Vec<f64>),
    Every { start: f64, end: f64, unit: crate::time::Unit, interval: u32 },
}

/// A time written in `format` (and no stand-in for the latest), as milliseconds since 1970.
fn written_as(v: &str, format: &str) -> Option<f64> {
    let t = crate::time::parse_input(&serde_json::Value::String(v.to_string()))?;
    (t < LATEST_STAND_IN && crate::time::format(t, format) == v).then_some(t)
}

/// An ISO 8601 period the slider steps by: years, months, weeks, days or hours (`P1Y`, `PT6H`).
fn period_step(p: &str) -> Option<(crate::time::Unit, u32)> {
    use crate::time::Unit;
    let (body, hours) = match p.strip_prefix("PT") {
        Some(b) => (b, true),
        None => (p.strip_prefix('P')?, false),
    };
    let n: u32 = body.get(..body.len().checked_sub(1)?)?.parse().ok().filter(|n| *n > 0)?;
    match (hours, body.chars().last()?) {
        (true, 'H') => Some((Unit::Hour, n)),
        (false, 'Y') => Some((Unit::Year, n)),
        (false, 'M') => Some((Unit::Month, n)),
        (false, 'W') => Some((Unit::Day, n.checked_mul(7)?)),
        (false, 'D') => Some((Unit::Day, n)),
        _ => None,
    }
}

impl SliderTime {
    /// Whether `t` is one of its steps (each counted from the start, as the slider counts them).
    pub fn has(&self, t: f64) -> bool {
        match &self.steps {
            SliderSteps::Dates(dates) => dates.contains(&t),
            SliderSteps::Every { start, end, unit, interval } => {
                crate::time::steps(*start, *end, *unit, *interval, MAX_SLIDER_STEPS).contains(&t)
            }
        }
    }
}

impl LayerTime {
    /// Whether it lists more than one time (several, or a range): one that lists a single time
    /// (swisstopo's maps, most of them `current` alone) has no other to show and is a plain map.
    pub fn changes(&self) -> bool {
        match self.values.as_slice() {
            [] => false,
            [one] => one.contains('/'),
            _ => true,
        }
    }

    /// Its times on the time slider; `None` when they aren't dates it can step to (one time, as
    /// `current` alone, or written in no format of [`DATE_FORMATS`]).
    pub fn slider(&self) -> Option<SliderTime> {
        use crate::time::Unit;
        if !self.changes() {
            return None;
        }
        // The format: the first time listed (or a range's start) that reads as a date in one.
        let first =
            self.values.iter().flat_map(|v| v.split('/').take(2)).find(|v| DATE_FORMATS.iter().any(|f| written_as(v, f).is_some()))?;
        let format = DATE_FORMATS.iter().copied().find(|f| written_as(first, f).is_some())?;
        // A range without a period steps by the finest unit its times name.
        let finest = match format {
            "YYYY" => Unit::Year,
            "YYYY-MM" => Unit::Month,
            "YYYYMMDD" | "YYYY-MM-DD" => Unit::Day,
            _ => Unit::Hour,
        };
        let range = |v: &str| -> Option<(f64, f64, Unit, u32)> {
            let pieces: Vec<&str> = v.split('/').collect();
            let (a, z) = (written_as(pieces.first()?, format)?, written_as(pieces.get(1)?, format)?);
            let (unit, n) = match pieces.get(2) {
                Some(p) => period_step(p)?,
                None => (finest, 1),
            };
            (a <= z).then_some((a, z, unit, n))
        };
        let steps = match self.values.as_slice() {
            [one] if one.contains('/') => {
                let (start, end, unit, mut interval) = range(one)?;
                // Within the slider's steps, every second, fourth… of the period's: still times the
                // layer lists, from its start to (about) its end.
                while crate::time::steps(start, end, unit, interval, MAX_SLIDER_STEPS + 1).len() > MAX_SLIDER_STEPS {
                    interval = interval.checked_mul(2)?;
                }
                SliderSteps::Every { start, end, unit, interval }
            }
            values => {
                let mut dates = Vec::new();
                for v in values {
                    if v.contains('/') {
                        let Some((a, z, unit, n)) = range(v) else { continue };
                        // Each step from the start, as the slider counts them: a month-end range keeps
                        // to month ends (31 January, 29 February, 31 March), not one day earlier
                        // after February.
                        dates.extend(crate::time::steps(a, z, unit, n, (MAX_SLIDER_STEPS + 1).saturating_sub(dates.len())));
                    } else if let Some(t) = written_as(v, format) {
                        dates.push(t);
                    }
                    if dates.len() > MAX_SLIDER_STEPS {
                        return None;
                    }
                }
                dates.sort_by(f64::total_cmp);
                dates.dedup();
                if dates.is_empty() {
                    return None;
                }
                SliderSteps::Dates(dates)
            }
        };
        // At its default; one that isn't a date (`current`, swisstopo's `99991231`), its latest.
        let latest = match &steps {
            SliderSteps::Dates(dates) => dates.last().copied(),
            SliderSteps::Every { end, .. } => Some(*end),
        };
        let at = written_as(&self.default, format).or(latest);
        Some(SliderTime { format, steps, at })
    }
}

/// A time given for a layer's `{Time}`: an ISO 8601 date or date-time (or `default`, or OGC's
/// `current`), never anything else, since it goes into tile addresses.
pub fn checked_time(t: &str) -> Result<String, String> {
    let t = t.trim();
    // A date starts with a digit, so `.` and `..` (path segments, in a tile address) never pass.
    let dated = t.as_bytes().first().is_some_and(u8::is_ascii_digit);
    if (dated || t == "default" || t == "current")
        && t.len() <= 40
        && t.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b':' | b'.' | b'+'))
    {
        Ok(t.to_string())
    } else {
        Err(format!("'{}': a time is a date like 2024-06-01", t.chars().take(60).collect::<String>()))
    }
}

/// Whether `p` is an ISO 8601 duration, as a WMS interval's resolution is: `P`, then numbers each
/// with its designator in order (`Y`, `M`, `W`, `D`; after `T`, `H`, `M`, `S`, seconds with a
/// fraction too), at least one (`P1Y`, `P1DT12H`, `PT30M`, `PT0.5S`).
fn is_duration(p: &str) -> bool {
    let Some(rest) = p.strip_prefix('P').filter(|r| !r.is_empty() && p.len() <= 40) else { return false };
    let (date, time) = match rest.split_once('T') {
        Some((d, t)) if !t.is_empty() => (d, Some(t)),
        Some(_) => return false,
        None => (rest, None),
    };
    // Each part a number and a designator, in the order given; only seconds take a fraction.
    let parts = |s: &str, order: &[u8]| -> Option<usize> {
        let (mut at, mut next, mut count) = (0, 0, 0);
        let b = s.as_bytes();
        while at < b.len() {
            let start = at;
            while b.get(at).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
                at += 1;
            }
            let number = s.get(start..at)?;
            let designator = *b.get(at)?;
            let place = order.iter().skip(next).position(|d| *d == designator)? + next;
            let fraction = number.contains('.');
            if number.matches('.').count() > 1
                || number.is_empty()
                || number.starts_with('.')
                || number.ends_with('.')
                || (fraction && designator != b'S')
            {
                return None;
            }
            (next, count, at) = (place + 1, count + 1, at + 1);
        }
        Some(count)
    };
    let dated = parts(date, b"YMWD");
    let timed = time.map_or(Some(0), |t| parts(t, b"HMS"));
    matches!((dated, timed), (Some(d), Some(t)) if d + t > 0)
}

/// Most characters of a WMS layer's time asked for (a list or interval of times).
const MAX_WMS_TIME: usize = 1000;

/// A time given for a WMS layer's `TIME` (WMS 1.3.0 Annex C, Table C.2): one time, a
/// comma-separated list of times, or an interval `start/end`, with its resolution or not
/// (`1952/2026/P1Y`); each time as [`checked_time`] takes it. It goes into a query, encoded.
pub fn checked_wms_time(t: &str) -> Result<String, String> {
    let t = t.trim();
    let error = || {
        format!(
            "'{}': a time is a date like 2024-06-01, a list (1990,2000) or an interval (1990/2000/P1Y)",
            t.chars().take(60).collect::<String>()
        )
    };
    if t.is_empty() || t.len() > MAX_WMS_TIME {
        return Err(error());
    }
    let one = |v: &str| checked_time(v).is_ok();
    // Each item and each part of an interval as `checked_time` takes it, trimmed.
    let parts: Vec<String> = t.split(',').map(|part| part.split('/').map(str::trim).collect::<Vec<_>>().join("/")).collect();
    let valid = |part: &String| {
        let pieces: Vec<&str> = part.split('/').collect();
        match pieces.as_slice() {
            [v] => one(v),
            [start, end] => one(start) && one(end),
            [start, end, p] => one(start) && one(end) && is_duration(p),
            _ => false,
        }
    };
    if parts.iter().all(valid) { Ok(parts.join(",")) } else { Err(error()) }
}

/// A tile template with its `{Time}` filled with `t` (a [`checked_time`], or for a WMS layer a
/// [`checked_wms_time`]): percent-encoded where
/// it is a query value (`TIME={Time}`, a WMS layer's or a WMTS one asked by key), as it is in a
/// path (a checked time has no `/`).
pub fn with_time(template: &str, t: &str) -> String {
    template.replace("={Time}", &format!("={}", encode(t))).replace("{Time}", t)
}

/// A time dimension from its default (`None` when the service gives none) and the values it
/// lists: the default when `check` takes it, else the latest time listed (a range's end); with
/// neither, none. Every value read is looked through for the latest; the last few are kept.
fn layer_time(default: Option<&str>, all: Vec<String>, check: fn(&str) -> Result<String, String>, multiple: bool) -> Option<LayerTime> {
    let all: Vec<String> = all.into_iter().filter(|v| v.len() <= 100).collect();
    let listed = all.iter().rev().find_map(|v| {
        let mut parts = v.split('/');
        let first = parts.next().unwrap_or("");
        checked_time(parts.next().unwrap_or(first)).ok()
    });
    let default = default.and_then(|t| check(t).ok()).or(listed)?;
    // The latest few: those the default and the latest are among.
    let cut = all.len().saturating_sub(MAX_TIME_VALUES);
    let values = all.into_iter().skip(cut).collect();
    Some(LayerTime { default, values, multiple, cut })
}

/// The note for layers whose own times run past [`MAX_TIME_VALUES`] (`layers` of them).
fn long_times_note(layers: usize) -> String {
    format!(
        "{layers} layers list more than {MAX_TIME_VALUES} times: RustGIS keeps the latest {MAX_TIME_VALUES} of each, and the earliest are left out"
    )
}

/// A WMS layer's own time dimension: its `Dimension` named `time` with its values (WMS 1.3.0
/// Annex C), or its `Extent` of that name (WMS 1.1.1, whose `Dimension` only declares it): a
/// comma-separated list of times and ranges (`1990,2000`, `1952/2026/P1Y`). Its default may be any
/// time `TIME` takes, a range too (`2005/2025`, swisstopo's strips of aerial photos).
/// `None` when the layer declares none of its own; `Some(None)` when it declares one that gives no
/// time to draw it at (in WMS 1.3.0, `v13`, an empty one too; a WMS 1.1.1 `Dimension` without its
/// `Extent` only declares the one it inherits).
fn wms_time(layer: &Node, v13: bool) -> Option<Option<LayerTime>> {
    let is_time = |n: &Node| n.attribute("name").is_some_and(|a| a.trim().eq_ignore_ascii_case("time"));
    let d = children(layer, "Dimension")
        .filter(is_time)
        .find(|d| !text_of(d).is_empty())
        .or_else(|| children(layer, "Extent").find(is_time))
        .or_else(|| children(layer, "Dimension").filter(|_| v13).find(is_time))?;
    let all: Vec<String> = text_of(&d).split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
    let all = last(all, MAX_TIMES_READ);
    let multiple = d.attribute("multipleValues").is_some_and(|m| m.trim() == "1" || m.trim().eq_ignore_ascii_case("true"));
    Some(layer_time(d.attribute("default").map(str::trim), all, checked_wms_time, multiple))
}

/// A layer a service offers.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceLayer {
    /// The name the service knows it by.
    pub name: String,
    pub title: String,
    pub offer: Offer,
    /// Its longitude/latitude bounds (west, south, east, north), when the service says.
    pub bbox: Option<[f64; 4]>,
    /// An OGC API tileset list (JSON) whose WebMercatorQuad tileset advertises the tile template:
    /// the offer's template is the standard path, used when the advertised one can't be read.
    pub tilesets: Option<String>,
    /// Its time dimension, when its tiles change with time (the template then has `{Time}`).
    pub time: Option<LayerTime>,
    /// A WMS layer's GetFeatureInfo request (its formats empty when the layer isn't queryable).
    pub feature_info: Option<FeatureInfo>,
    /// A WMS or WMTS layer's legend pictures, the first to show first: a WMS layer's from the
    /// service's GetLegendGraphic, then its default style's picture; a WMTS layer's default
    /// style's, each for its scales. None when the service gives none.
    pub legends: Vec<Legend>,
    /// The Web Mercator zooms its tiles are drawn at, least and greatest, when the service says:
    /// a WMS layer's scale hints, a WMTS layer's tile matrices. `None`: the usual range.
    pub zooms: Option<(u8, u8)>,
}

/// A map layer's legend picture as its service gives it: its address and, as a WMTS style's
/// legend may say, the scales it is for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Legend {
    pub url: String,
    /// The scale denominators it is for (WMTS 1.0.0's `minScaleDenominator`, inclusive, and
    /// `maxScaleDenominator`, exclusive); none is open-ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_scale: Option<f64>,
}

impl Legend {
    /// Whether it is for the map at scale 1:`scale`.
    pub fn covers(&self, scale: f64) -> bool {
        self.min_scale.is_none_or(|m| scale >= m) && self.max_scale.is_none_or(|m| scale < m)
    }
}

/// The legends among a layer's for the map at scale 1:`scale`, the one to show first first (the
/// next is for when it can't be shown).
pub fn legends_at(legends: &[Legend], scale: f64) -> impl Iterator<Item = &Legend> {
    legends.iter().filter(move |l| l.covers(scale))
}

/// The most legend pictures a layer keeps.
pub const MAX_LEGENDS: usize = 16;

/// Whether a legend picture's format is one RustGIS shows (PNG or JPEG); a legend that names
/// none is taken to be one.
fn legend_format(format: Option<&str>) -> bool {
    format.is_none_or(|f| {
        let f = f.trim().to_ascii_lowercase();
        f.is_empty() || f.starts_with("image/png") || f.starts_with("image/jpeg") || f.starts_with("image/jpg")
    })
}

/// A scale denominator a legend gives: a positive number.
fn scale_attr(n: &Node, key: &str) -> Option<f64> {
    n.attribute(key).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0)
}

/// A service's capabilities: its kind (`WMS`, `WMTS`, `WFS`, `OGC API Features`), title, the
/// layers RustGIS can add, and notes on what it left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Service {
    pub kind: String,
    pub title: String,
    pub layers: Vec<ServiceLayer>,
    pub notes: Vec<String>,
    /// An OGC API landing page's advertised JSON collections document, resolved against it.
    pub collections_url: Option<String>,
    /// How many of `layers` a listing shows, when not all of them: the others are read only to be
    /// added by name.
    pub listed: Option<usize>,
    /// Some advertised collections were left out (past the listing limit, or with an items
    /// address RustGIS doesn't fetch): the listing isn't the whole service.
    pub incomplete: bool,
    /// Destinations that did not inherit one or more query pairs; reported by the caller.
    pub withheld_addresses: u32,
    /// The document goes past what is read of it: the first note saying so, also in `notes`.
    /// (Notes can quote the document, so this isn't told from their text.)
    pub past_read_limit: Option<String>,
}

/// Most layers or collections `service.layers` lists from one document. An OGC API collection
/// can make up to three layers (features, map tiles, vector tiles), so its listing can pass this
/// many layers. In an OGC document the others are still read ([`Service::listed`]), so that they
/// can be added by name: which ones come first is up to the server, and some change their order
/// between answers (#1519). Reading stops at [`MAX_READ_LAYERS`], or sooner when the layers'
/// request addresses are long ([`read_limit`], [`MAX_COPIED_ADDRESS_BYTES`]); past that none is
/// read. An ArcGIS list is read only as far as it is listed.
const MAX_LISTED_LAYERS: usize = 2_000;
/// Most layers or collections read from one document, which is hostile input.
const MAX_READ_LAYERS: usize = if cfg!(test) { 3_000 } else { 100_000 };
/// Most bytes of a document's own request addresses (its GetMap's, GetFeature's…) copied into
/// its layers together: each layer's address repeats them, so long ones lower how many are read.
const MAX_COPIED_ADDRESS_BYTES: usize = if cfg!(test) { 8 << 20 } else { 64 << 20 };

/// How many layers are read from a document whose layers each copy `per_layer` bytes of its
/// request addresses: [`MAX_READ_LAYERS`], fewer for long addresses, never fewer than are listed.
fn read_limit(per_layer: usize) -> usize {
    (MAX_COPIED_ADDRESS_BYTES / per_layer.max(1)).clamp(MAX_LISTED_LAYERS, MAX_READ_LAYERS)
}

/// Says why a document past a [`read_limit`] lower than [`MAX_READ_LAYERS`] was read no further.
fn note_copied_addresses(service: &mut Service, per_layer: usize, limit: usize) {
    if limit < MAX_READ_LAYERS {
        service.notes.push(format!(
            "each of its layers copies {per_layer} bytes of the document's request addresses, so RustGIS reads {limit} of them, not {MAX_READ_LAYERS}"
        ));
    }
}
/// The bytes of addresses a layer holds: what its offer is fetched from, and its tile list.
fn address_bytes(layer: &ServiceLayer) -> usize {
    let offer = match &layer.offer {
        Offer::Tiles { template } | Offer::VectorTiles { template, .. } => template.len(),
        Offer::Features { url } | Offer::Style { url } | Offer::Service { url } => url.len(),
        Offer::Coverage(c) => c.endpoint.len() + c.id.len(),
    };
    offer + layer.tilesets.as_ref().map_or(0, String::len)
}

/// How a note says that a document went past what is read of it.
const PAST_READ_LIMIT: &str = "RustGIS reads at most";
/// Features asked of a WFS in one request.
pub const FEATURE_LIMIT: usize = 50_000;
/// Features asked of an OGC API in one request: servers cap their pages (often at 10,000) and
/// some refuse a larger `limit` outright.
pub const OGC_API_LIMIT: usize = 10_000;

/// Notes the layers of a document past the `limit` that is read of it, and keeps the first such
/// note as [`Service::past_read_limit`].
fn note_past_read_limit(service: &mut Service, left: usize, what: &str, limit: usize) {
    let note = format!("{PAST_READ_LIMIT} {limit} {what} from one document: {left} left out");
    service.past_read_limit.get_or_insert_with(|| note.clone());
    service.notes.push(note);
}

/// Stops the listing at [`MAX_LISTED_LAYERS`], for a document whose every entry makes one layer.
fn cut_listing(service: &mut Service) {
    if service.layers.len() > MAX_LISTED_LAYERS {
        service.listed = Some(MAX_LISTED_LAYERS);
    }
}

/// The address to fetch a service's capabilities from: OGC API landing pages and collection lists
/// are JSON; WMS, WMTS and WFS addresses get a GetCapabilities request unless they have one.
/// A runtime service-login binding stays attached only to same-origin derived requests.
pub fn capabilities_url(url: &str, kind: Option<&str>) -> String {
    if let Ok(Some(bound)) = crate::login_address::parse(url) {
        return crate::login_address::inherit(url, &capabilities_url(bound.url, kind));
    }
    // ArcGIS REST: the service's JSON description.
    if kind.is_some_and(|k| k.eq_ignore_ascii_case("arcgis")) || (kind.is_none() && is_arcgis(url)) {
        return if query_value(url, "f").is_some_and(|f| f.eq_ignore_ascii_case("json") || f.eq_ignore_ascii_case("pjson")) {
            url.to_string()
        } else {
            with_query(url, "f=json")
        };
    }
    if query_value(url, "request").is_some_and(|r| r.eq_ignore_ascii_case("getcapabilities"))
        || base(url).to_ascii_lowercase().ends_with(".xml")
    {
        return url.to_string();
    }
    // The guess reads the path and query, never the host (`rapid.example.org` isn't an API).
    let rest = after_host(url).to_ascii_lowercase();
    let segments: Vec<&str> = base(&rest).split('/').filter(|s| !s.is_empty()).collect();
    let service = match kind.map(str::to_ascii_lowercase).as_deref() {
        Some("wms") => "WMS",
        Some("wmts") => "WMTS",
        Some("wfs") => "WFS",
        Some("wcs") => "WCS",
        Some("ogcapi") | Some("features") => return json_url(url),
        _ if rest.contains("wmts") => "WMTS",
        _ if rest.contains("wfs") => "WFS",
        _ if rest.contains("wcs") => "WCS",
        _ if rest.contains("wms") => "WMS",
        // An OGC API (`…/collections`, `…/ogc/features`, `…/api/…`): asked as JSON.
        _ if segments.iter().any(|s| matches!(*s, "collections" | "api" | "ogcapi" | "ogc")) => return json_url(url),
        // Anything else is taken as a WMS (the commonest service).
        _ => "WMS",
    };
    with_query(url, &format!("SERVICE={service}&REQUEST=GetCapabilities"))
}

/// Requests JSON once, retaining the address's other query parameters.
pub fn json_url(address: &str) -> String {
    if let Ok(Some(bound)) = crate::login_address::parse(address) {
        return crate::login_address::inherit(address, &json_url(bound.url));
    }
    let Ok(mut url) = url::Url::parse(address) else { return with_query(address, "f=json") };
    // Authentication and signed query values can depend on their original byte encoding.
    let mut format_seen = false;
    let mut pairs: Vec<&str> = url
        .query()
        .filter(|q| !q.is_empty())
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter_map(|pair| {
            if url::form_urlencoded::parse(pair.as_bytes()).next().is_some_and(|(key, _)| key.eq_ignore_ascii_case("f")) {
                let first = !format_seen;
                format_seen = true;
                first.then_some("f=json")
            } else {
                Some(pair)
            }
        })
        .collect();
    if !format_seen {
        pairs.push("f=json");
    }
    let query = pairs.join("&");
    url.set_fragment(None);
    url.set_query(Some(&query));
    url.into()
}

/// A collections link uses either the short or full OGC data relation.
fn is_data_link(link: &serde_json::Value) -> bool {
    matches!(link.get("rel").and_then(|rel| rel.as_str()), Some("data" | "http://www.opengis.net/def/rel/ogc/1.0/data"))
}

/// Chooses JSON before an untyped data link, without guessing an endpoint or copying credentials.
fn collections_link(address: &str, links: &[serde_json::Value]) -> Option<(String, bool)> {
    let base = url::Url::parse(crate::login_address::inner(address)).ok()?;
    links
        .iter()
        .filter(|l| is_data_link(l))
        .filter_map(|l| {
            let priority = match l.get("type").and_then(|v| v.as_str()) {
                Some(t) if t.split(';').next().is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json")) => 0,
                None => 1,
                _ => return None,
            };
            let href = l.get("href")?.as_str()?.trim();
            if href.is_empty() || href.len() > 8192 {
                return None;
            }
            let mut next = base.join(href).ok()?;
            if !matches!(next.scheme(), "http" | "https") {
                return None;
            }
            next.set_fragment(None);
            // A typed JSON link already selects its representation; its query is authoritative.
            let next: String = if priority == 0 { next.into() } else { json_url(next.as_str()) };
            let inherited =
                crate::address::inherit_query(crate::login_address::inner(address), &next, crate::address::Relation::Advertised, &["f"]);
            // The chosen representation/link stays authoritative. Count settings it omits,
            // including safe options we deliberately did not append to this advertised link.
            let withheld = inherited.withheld || inherited.url != next;
            Some((priority, (crate::login_address::inherit(address, &next), withheld)))
        })
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, url)| url)
}

/// Whether an address is an ArcGIS REST service (or its directory), not one of its OGC endpoints
/// (`…/MapServer/WMSServer`, `…/MapServer/WMTS`), which are read as WMS and WMTS.
fn is_arcgis(url: &str) -> bool {
    let b = base(url).trim_end_matches('/').to_ascii_lowercase();
    if ["/wmsserver", "/wfsserver", "/wmts"].iter().any(|k| b.contains(k)) {
        return false;
    }
    b.contains("/rest/services") || ["featureserver", "mapserver", "imageserver", "vectortileserver"].iter().any(|k| b.ends_with(k))
}

/// The part of an address after its host: the path, query and fragment.
fn after_host(url: &str) -> &str {
    let url = crate::login_address::inner(url);
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.find(['/', '?', '#']).and_then(|i| rest.get(i..)).unwrap_or("")
}

/// The scheme and host of an address (`https://x.org`).
fn origin(url: &str) -> &str {
    let host_at = url.find("://").map(|i| i + 3).unwrap_or(0);
    let end = url.get(host_at..).and_then(|r| r.find(['/', '?', '#'])).map(|i| host_at + i).unwrap_or(url.len());
    url.get(..end).unwrap_or(url)
}

/// The value of a query parameter, by its whole name (any case).
fn query_value<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let url = crate::login_address::inner(url);
    let q = url.split('#').next()?.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (if key == "token" { crate::address::normalized_name(k).as_deref() == Some("token") } else { k.eq_ignore_ascii_case(key) })
            .then_some(v)
    })
}

/// Whether an address is a web address (`http://` or `https://`), not another scheme that only
/// starts with those letters.
fn is_web(url: &str) -> bool {
    let url = crate::login_address::inner(url);
    let l = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// A link from a document at `from`, resolved: absolute, scheme-relative (`//host/…`),
/// host-relative (`/…`) or relative to the document's folder. `None` for other schemes.
/// A supplied runtime wrapper contributes only its URL, never its authority: only the bound
/// document's canonical origin retains that document's runtime login binding.
fn resolve(from: &str, href: &str) -> Option<String> {
    crate::login_address::parse(from).ok()?;
    crate::login_address::parse(href.trim()).ok()?;
    let url = resolve_inner(crate::login_address::inner(from), crate::login_address::inner(href.trim()))?;
    Some(crate::login_address::inherit(from, &url))
}

fn resolve_inner(from: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if is_web(href) {
        return Some(href.to_string());
    }
    if href.starts_with('?') {
        return Some(format!("{}{href}", base(from)));
    }
    if href.starts_with('#') {
        return Some(format!("{}{href}", from.split('#').next().unwrap_or(from)));
    }
    let has_scheme = href.split_once(':').is_some_and(|(scheme, _)| {
        let mut bytes = scheme.bytes();
        bytes.next().is_some_and(|b| b.is_ascii_alphabetic()) && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    });
    if has_scheme || href.is_empty() {
        return None;
    }
    if let Some(rest) = href.strip_prefix("//") {
        let scheme = from.split_once("://").map(|(s, _)| s).unwrap_or("https");
        let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let mut url = String::with_capacity(scheme.len() + 3 + rest.len());
        url.push_str(scheme);
        url.push_str("://");
        url.push_str(rest.get(..host_end)?);
        let root = url.len();
        push_resolved_path(&mut url, rest.get(host_end..)?, root);
        return Some(url);
    }
    let host = origin(from);
    let b = base(from);
    let path = b.get(host.len()..).unwrap_or("");
    let dir = if href.starts_with('/') { "" } else { path.rfind('/').and_then(|i| path.get(..=i)).unwrap_or("/") };
    let mut url = String::with_capacity(host.len() + dir.len() + href.len());
    url.push_str(host);
    push_resolved_path(&mut url, dir, host.len());
    push_resolved_path(&mut url, href, host.len());
    Some(url)
}

/// Appends a relative URL's path with literal dot segments resolved (RFC 3986). Templates,
/// percent escapes and the query/fragment tail stay byte-for-byte; parents stop at the root.
fn push_resolved_path(out: &mut String, href: &str, root: usize) {
    let tail_at = href.find(['?', '#']).unwrap_or(href.len());
    let path = href.get(..tail_at).unwrap_or(href);
    if !path.split('/').any(|segment| matches!(segment, "." | "..")) {
        out.push_str(href);
        return;
    }
    for segment in path.split_inclusive('/') {
        match segment.strip_suffix('/').unwrap_or(segment) {
            "." => {}
            ".." => {
                let end = out.len().saturating_sub(usize::from(out.ends_with('/')));
                let parent = out.get(root..end).and_then(|path| path.rfind('/')).map(|i| root + i + 1).unwrap_or(root);
                out.truncate(parent);
                if out.len() == root {
                    out.push('/');
                }
            }
            _ => out.push_str(segment),
        }
    }
    out.push_str(href.get(tail_at..).unwrap_or(""));
}

/// The words in the notes on OGC API collections left out as 3D content, by which
/// `service.add` finds why a layer isn't listed.
pub const LEFT_OUT_3D: &str = "which RustGIS doesn't draw";

/// What an OGC API collection's items link, in the document at `from`, offers when it is 3D
/// content, not features: 3D Tiles (its type, or an address ending in `/3dtiles`) or quantized
/// mesh terrain (an address ending in `/quantized-mesh`; its type is only `application/json`).
/// A relative address is judged resolved, so a bare `3dtiles` counts.
fn items_3d(from: &str, link: &serde_json::Value) -> Option<&'static str> {
    let kind = link.get("type").and_then(|t| t.as_str()).unwrap_or_default().to_ascii_lowercase();
    let href = link.get("href").and_then(|h| h.as_str()).unwrap_or_default();
    let href = resolve(from, href).unwrap_or_else(|| href.to_string());
    let path = href.split(['?', '#']).next().unwrap_or_default().trim_end_matches('/').to_ascii_lowercase();
    if kind.contains("3dtiles") || path.ends_with("/3dtiles") {
        Some("3D Tiles")
    } else if kind.contains("quantized-mesh") || path.ends_with("/quantized-mesh") {
        Some("quantized mesh terrain")
    } else {
        None
    }
}

fn with_query(url: &str, q: &str) -> String {
    if let Ok(Some(bound)) = crate::login_address::parse(url) {
        return crate::login_address::inherit(url, &with_query(bound.url, q));
    }
    let (base, frag) = url.split_once('#').map(|(b, f)| (b, Some(f))).unwrap_or((url, None));
    let sep = if base.contains('?') { if base.ends_with('?') || base.ends_with('&') { "" } else { "&" } } else { "?" };
    match frag {
        Some(f) => format!("{base}{sep}{q}#{f}"),
        None => format!("{base}{sep}{q}"),
    }
}

/// A service request endpoint without OGC operation parameters, keeping unrelated query values
/// (MapServer's `map=`, a token), whether advertised or used as a fallback.
fn endpoint(url: &str) -> String {
    if let Ok(Some(bound)) = crate::login_address::parse(url) {
        return crate::login_address::inherit(url, &endpoint(bound.url));
    }
    let b = base(url);
    let kept: Vec<&str> = url
        .split('#')
        .next()
        .and_then(|u| u.split_once('?'))
        .map(|(_, q)| q)
        .unwrap_or("")
        .split('&')
        .filter(|kv| {
            let k = kv.split_once('=').map(|(k, _)| k).unwrap_or(kv).to_ascii_lowercase();
            !kv.is_empty() && !matches!(k.as_str(), "service" | "request" | "version" | "acceptversions" | "sections" | "f")
        })
        .collect();
    if kept.is_empty() { b.to_string() } else { format!("{b}?{}", kept.join("&")) }
}

/// The part of an address before its query.
fn base(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// Reads a capabilities document (XML or OGC API JSON) fetched from `url`.
pub fn parse(url: &str, body: &[u8]) -> Result<Service, String> {
    let text = std::str::from_utf8(body).map_err(|_| "the service's answer isn't UTF-8 text".to_string())?;
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with('{') || text.starts_with('[') {
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the service's JSON: {e}"))?;
        if let Some(e) = v.get("error") {
            let message = e.get("message").and_then(|m| m.as_str()).unwrap_or("(no message)");
            // ArcGIS's "Token Required" (499) and "Invalid Token" (498): say how to sign in.
            let hint = match e.get("code").and_then(serde_json::Value::as_i64) {
                Some(499) => {
                    " (the service is secured: sign in to its ArcGIS portal with Sign in to ArcGIS or arcgis.signIn, or add its address with ?token=…)"
                }
                Some(498) => " (the ArcGIS token was refused, perhaps expired: sign in to the portal again)",
                _ => "",
            };
            return Err(format!("the service answered with an error: {message}{hint}"));
        }
        if is_arcgis(url) || v.get("currentVersion").is_some() {
            return arcgis(url, &v);
        }
        return ogc_api(url, &v);
    }
    let doc = xml(text, 5_000_000).map_err(|e| format!("the service's XML: {e}"))?;
    let root = doc.root_element();
    match local(&root) {
        "WMS_Capabilities" | "WMT_MS_Capabilities" => Ok(wms(url, &root)),
        "Capabilities" if root.descendants().any(|n| n.is_element() && local(&n) == "CoverageSummary") => Ok(wcs(url, &root)),
        "Capabilities" if child(&root, "Contents").is_some() => Ok(wmts(url, &root)),
        "WFS_Capabilities" => Ok(wfs(url, &root)),
        // WCS 1.0 (`WCS_Capabilities`), 1.1 and 2.0 (`Capabilities` with coverage summaries).
        "WCS_Capabilities" => Ok(wcs(url, &root)),
        "ServiceExceptionReport" | "ExceptionReport" => {
            Err(format!("the service answered with an error: {}", text_of(&root).chars().take(300).collect::<String>()))
        }
        other => Err(format!("not a WMS, WMTS, WFS or WCS capabilities document (<{other}>)")),
    }
}

/// Parses a service's XML document through [`crate::xml::parse`], which refuses deep nesting
/// (a stack-overflow hazard) and, with the DTD allowed here, entities holding markup.
fn xml(text: &str, nodes_limit: u32) -> Result<roxmltree::Document<'_>, String> {
    // WMS 1.1.1 documents start with a DOCTYPE, so DTDs are allowed; roxmltree still refuses
    // entity bombs and external entities, and the node count is capped.
    let opts = ParsingOptions { allow_dtd: true, nodes_limit, ..ParsingOptions::default() };
    crate::xml::parse(text.trim_start(), opts)
}

fn local<'a>(n: &Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

fn child<'a, 'i>(n: &Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && local(c) == name)
}

fn children<'a, 'i>(n: &Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(move |c| c.is_element() && local(c) == name)
}

fn text_of(n: &Node) -> String {
    n.descendants().filter(|c| c.is_text()).filter_map(|c| c.text()).collect::<String>().trim().to_string()
}

fn child_text(n: &Node, name: &str) -> Option<String> {
    child(n, name).map(|c| text_of(&c)).filter(|t| !t.is_empty())
}

fn href(n: &Node) -> Option<String> {
    n.attributes().find(|a| a.name() == "href").map(|a| a.value().to_string())
}

/// A finite longitude/latitude box.
fn lonlat_box(w: f64, s: f64, e: f64, n: f64) -> Option<[f64; 4]> {
    ([w, s, e, n].iter().all(|v| v.is_finite()) && w < e && s < n && w >= -180.5 && e <= 180.5 && s >= -90.5 && n <= 90.5)
        .then_some([w, s, e, n])
}

/// Codes a server uses for Web Mercator.
fn is_web_mercator(code: &str) -> bool {
    let c = code.to_ascii_uppercase();
    ["EPSG:3857", "EPSG:900913", "EPSG:102100", "EPSG:102113", "EPSG::3857", "OSGEO:41001"].iter().any(|k| c.contains(k))
        || c.ends_with(":3857")
}

// ---------------------------------------------------------------------------------------------
// WMS

/// The greatest zoom a WMS layer is drawn at when its service gives no scale hint: a GetMap is
/// drawn at any scale, and this is about 4 cm a pixel at the equator (an aerial photo's 10 cm
/// fills it).
const WMS_MAX_ZOOM: u8 = 22;

/// A WMS layer's scale hints: the scale denominators it is drawn between (WMS 1.3.0's
/// `MinScaleDenominator` and `MaxScaleDenominator`; 1.1.1's `ScaleHint`, a pixel's diagonal on
/// the ground in metres).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Scales {
    min: Option<f64>,
    max: Option<f64>,
}

impl Scales {
    /// A layer's hints, each its own or else its parent's (`inherited`): WMS 1.3.0 replaces
    /// each element on its own (Table 7).
    fn of(layer: &Node, inherited: Scales) -> Scales {
        let num = |t: Option<&str>| t.and_then(|t| t.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0);
        let hint = child(layer, "ScaleHint");
        let scale = |k: &str| num(hint.as_ref().and_then(|h| h.attribute(k))).map(|d| d / std::f64::consts::SQRT_2 / 0.000_28);
        let min = num(child_text(layer, "MinScaleDenominator").as_deref()).or_else(|| scale("min"));
        let max = num(child_text(layer, "MaxScaleDenominator").as_deref()).or_else(|| scale("max"));
        Scales { min: min.or(inherited.min), max: max.or(inherited.max) }
    }

    /// The Web Mercator zooms the layer is drawn at: down to the last whose scale is at or above
    /// its least (else [`WMS_MAX_ZOOM`]), out to the first at or below its greatest. A service
    /// works out the scale of a request in Web Mercator in its metres, as the zooms' scales are
    /// (a hair past a hint counts: hints are rounded).
    fn zooms(self) -> (u8, u8) {
        let at = |scale: f64| (Z0_SCALE / scale).log2();
        let max = self.min.map_or(WMS_MAX_ZOOM, |s| (at(s) + 0.05).floor().clamp(0.0, 24.0) as u8);
        let min = self.max.map_or(0, |s| (at(s) - 0.05).ceil().clamp(0.0, 24.0) as u8).min(max);
        (min, max)
    }
}

fn wms(url: &str, root: &Node) -> Service {
    let v13 = root.attribute("version").is_none_or(|v| v.starts_with("1.3"));
    let title = child(root, "Service").and_then(|s| child_text(&s, "Title")).unwrap_or_default();
    let get_map = root
        .descendants()
        .find(|n| n.is_element() && local(n) == "GetMap")
        .and_then(|g| g.descendants().find(|n| n.is_element() && local(n) == "Get"))
        .and_then(|g| child(&g, "OnlineResource"))
        .and_then(|o| href(&o))
        .and_then(|h| resolve(url, &h))
        // The request's own parameters replace any the advertised address carries (a `TIME`).
        .map(|h| info::without_wms_keys(&h))
        .unwrap_or_else(|| endpoint(url));
    let formats: Vec<String> = root
        .descendants()
        .find(|n| n.is_element() && local(n) == "GetMap")
        .map(|g| children(&g, "Format").map(|f| text_of(&f)).collect())
        .unwrap_or_default();
    let format = ["image/png", "image/png8", "image/jpeg"]
        .iter()
        .find(|f| formats.iter().any(|g| g.eq_ignore_ascii_case(f)))
        .copied()
        .unwrap_or("image/png");
    let version = if v13 { "1.3.0" } else { "1.1.1" };
    let info_request = info::request_of(root, url, version, format);
    let mut service = Service { kind: "WMS".into(), title, ..Service::default() };
    let (mut skipped, mut too_long) = (0, 0);
    let mut time_budget = MAX_DOCUMENT_TIME_VALUES;
    // The layers whose times were cut short (per layer past MAX_TIME_VALUES, and for the document).
    let (mut cut_times, mut long_times) = (0, 0);
    let mut layer_boxes = LayerBoxes::new();
    // Layers inherit their parents' coordinate systems, nearest valid geographic bounds, queryable
    // attribute and time dimension.
    // The SLD profile's GetLegendGraphic (WMS 1.3.0 with SLD 1.1, 1.1.1 with SLD 1.0), when the
    // service offers it in a format RustGIS shows: a layer's legend, drawn from its default style.
    let legend_graphic = wms_legend_graphic(root, url, &get_map, version);
    let per_layer =
        get_map.len() + info_request.as_ref().map_or(0, |r| r.url.len()) + legend_graphic.as_ref().map_or(0, |g| g("").url.len());
    let limit = read_limit(per_layer);
    // (What a layer passes on is shared by its sublayers, not copied for each: a hostile document
    // may give one layer a great many.)
    type Inherited<'a, 'i> = (Node<'a, 'i>, Rc<Systems>, Option<[f64; 4]>, bool, Rc<[Node<'a, 'i>]>, Option<Rc<LayerTime>>, Scales);
    let mut stack: Vec<Inherited<'_, '_>> = root
        .descendants()
        .filter(|n| n.is_element() && local(n) == "Capability")
        .flat_map(|c| children(&c, "Layer").collect::<Vec<_>>())
        .map(|l| (l, Rc::default(), None, false, Rc::from([]), None, Scales::default()))
        .collect();
    // Read in document order (the last pushed is the first read), so the layers past the read
    // limit are the document's last, and a parent comes before its sublayers.
    stack.reverse();
    while let Some((layer, inherited_crs, inherited_bbox, inherited_queryable, inherited_styles, inherited_time, inherited_scales)) =
        stack.pop()
    {
        if service.layers.len() >= limit {
            let named = |l: &Node| l.descendants().filter(|n| n.is_element() && local(n) == "Layer" && child(n, "Name").is_some()).count();
            let left = named(&layer) + stack.iter().map(|entry| named(&entry.0)).sum::<usize>();
            if left > 0 {
                note_past_read_limit(&mut service, left, "layers", limit);
                note_copied_addresses(&mut service, per_layer, limit);
            }
            break;
        }
        // (A layer naming none of its own shares its parent's list, and what is worked out of it.)
        let own: Vec<String> =
            layer.children().filter(|c| c.is_element() && matches!(local(c), "CRS" | "SRS")).map(|c| text_of(&c)).collect();
        let crs: Rc<Systems> = if own.is_empty() {
            inherited_crs
        } else {
            Rc::new(Systems { names: inherited_crs.names.iter().cloned().chain(own).collect(), ..Systems::default() })
        };
        let bbox = child(&layer, "EX_GeographicBoundingBox")
            .and_then(|b| {
                let f = |k: &str| child_text(&b, k).and_then(|t| t.parse::<f64>().ok());
                lonlat_box(f("westBoundLongitude")?, f("southBoundLatitude")?, f("eastBoundLongitude")?, f("northBoundLatitude")?)
            })
            .or_else(|| {
                let b = child(&layer, "LatLonBoundingBox")?;
                let f = |k: &str| b.attribute(k).and_then(|t| t.parse::<f64>().ok());
                lonlat_box(f("minx")?, f("miny")?, f("maxx")?, f("maxy")?)
            })
            .or(inherited_bbox);
        let queryable =
            layer.attribute("queryable").map_or(inherited_queryable, |q| q.trim() == "1" || q.trim().eq_ignore_ascii_case("true"));
        // Its own styles first, then those it inherits (a few: its parents' are each kept).
        let styles: Rc<[Node]> = children(&layer, "Style").chain(inherited_styles.iter().copied()).take(MAX_STYLES).collect();
        // Its own time dimension replaces the one it inherits (WMS 1.3.0 §7.2.4.8), even one it
        // can't be drawn at.
        let time = match wms_time(&layer, v13) {
            Some(own) => own.map(Rc::new),
            None => inherited_time,
        };
        // Its own scale hints replace those it inherits (WMS 1.3.0 §7.2.4.8).
        let scales = Scales::of(&layer, inherited_scales);
        for sub in children(&layer, "Layer").collect::<Vec<_>>().into_iter().rev() {
            stack.push((sub, Rc::clone(&crs), bbox, queryable, Rc::clone(&styles), time.clone(), scales));
        }
        let Some(name) = child_text(&layer, "Name") else { continue };
        // In Web Mercator, as the service draws it, else in a system of its own, warped (#769);
        // with the other systems it is offered in, for a map drawn in one of them.
        let mercator = crs.names.iter().any(|c| c.split_whitespace().any(is_web_mercator));
        let listed = crs.candidates.get_or_init(|| wms_candidates(&crs.names));
        let mut native = wms_native(&layer, listed, v13, bbox, mercator, &mut layer_boxes);
        if !mercator && native.is_none() {
            skipped += 1;
            continue;
        }
        let crs_key = if v13 { "CRS" } else { "SRS" };
        let (code, area) = match &native {
            Some((_, w)) if w.epsg != 3857 => ("{ncrs}".to_string(), "BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}"),
            _ => ("EPSG:3857".to_string(), "BBOX={bbox-epsg-3857}&WIDTH=256&HEIGHT=256"),
        };
        let template = with_query(
            &get_map,
            &format!(
                "SERVICE=WMS&VERSION={version}&REQUEST=GetMap&LAYERS={}&STYLES=&{crs_key}={code}&{area}&FORMAT={}&TRANSPARENT=TRUE",
                encode(&name),
                encode(format)
            ),
        );
        // At its time: the service's default, unless another is asked for when it is added.
        let template = if time.is_some() { with_query(&template, "TIME={Time}") } else { template };
        // An address the map can fetch (4096 bytes at most): the last of the other systems go
        // first, then, for a layer offered in Web Mercator, all of them.
        let mut address = native.as_ref().map(|(_, w)| w.address(&template));
        while let (Some(a), Some((_, w))) = (&address, &mut native)
            && a.len() > MAX_ADDRESS
            && w.systems.len() > 1
        {
            w.systems.pop();
            address = Some(w.address(&template));
        }
        if address.as_ref().is_some_and(|a| a.len() > MAX_ADDRESS) {
            if !mercator {
                too_long += 1;
                continue;
            }
            (native, address) = (None, None);
        }
        let template = address.unwrap_or(template);
        let title = child_text(&layer, "Title").unwrap_or_else(|| name.clone());
        // A service without GetFeatureInfo still gives each layer one, never asked, so Identify
        // doesn't fetch the capabilities again to find out.
        let feature_info = Some(
            info_request
                .as_ref()
                .map_or_else(|| FeatureInfo { layer: name.clone(), ..FeatureInfo::default() }, |r| r.for_layer(&name, queryable)),
        )
        .map(|f| match &native {
            // Asked in the system the layer is drawn in.
            Some((code, warp::Warp { epsg, grid: warp::Grid::Map { lat_first }, .. })) if *epsg != 3857 => {
                FeatureInfo { crs: code.clone(), lat_first: *lat_first, ..f }
            }
            _ => f,
        });
        // Its legend: the one the service draws from the layer's styling (the SLD profile's
        // GetLegendGraphic, symbols with their names, as ArcGIS Pro and QGIS show a WMS layer's),
        // then its default style's `LegendURL` picture (WMS 1.3.0 §7.2.4.6.4, 1.1.1 §7.1.4.5.14: of
        // its own styles and those it inherits), which some services make a cover page only; the
        // Layers pane shows the first it can.
        let mut legends: Vec<Legend> = legend_graphic.as_ref().map(|g| g(&name)).into_iter().collect();
        if let Some(l) = wms_legend(url, &styles).filter(|l| !legends.contains(l)) {
            legends.push(l);
        }
        service.layers.push(ServiceLayer {
            zooms: Some(scales.zooms()),
            name,
            title,
            offer: Offer::Tiles { template },
            bbox,
            tilesets: None,
            // The values listed for the whole document are capped (a parent's many, inherited by a
            // great many layers, aren't copied to each), spent in document order: those a listing
            // shows first.
            time: time.as_deref().map(|t| {
                let keep = t.values.len().min(time_budget);
                let values: Vec<String> = t.values.iter().skip(t.values.len() - keep).cloned().collect();
                time_budget -= values.len();
                cut_times += usize::from(keep < t.values.len());
                long_times += usize::from(t.cut > 0);
                LayerTime { cut: t.cut + t.values.len() - keep, values, default: t.default.clone(), multiple: t.multiple }
            }),
            feature_info,
            legends,
        });
    }
    if cut_times > 0 {
        service.notes.push(format!(
            "{cut_times} layers keep fewer or none of their times: RustGIS keeps {MAX_DOCUMENT_TIME_VALUES} for one document, and they come after them"
        ));
    }
    if long_times > 0 {
        service.notes.push(long_times_note(long_times));
    }
    cut_listing(&mut service);
    if skipped > 0 {
        service.notes.push(format!("{skipped} layers aren't offered in a coordinate system RustGIS can draw and are left out"));
    }
    if too_long > 0 {
        service.notes.push(format!(
            "{too_long} layers in another coordinate system are left out: their addresses are longer than {MAX_ADDRESS} bytes"
        ));
    }
    service
}

/// The coordinate systems a WMS layer is offered in, its own and those it inherits, and those of
/// them RustGIS knows (worked out once, for the layers sharing the list).
#[derive(Default)]
struct Systems {
    names: Vec<String>,
    candidates: std::cell::OnceCell<Vec<Candidate>>,
}

/// A system of a WMS layer's list that RustGIS knows: its name there, its EPSG code, it, and its
/// place in the list.
type Candidate = (String, u32, crate::raster::Crs, usize);

/// The systems of a WMS layer's list that RustGIS knows, one name each, in order of preference:
/// projected ones first (the service's own, as a rule), then CRS:84, then other
/// longitude/latitude ones, each kind as listed. (A GeoServer lists some 5,000 systems by
/// default, each layer inheriting them: only a system's definition is looked up here, once a
/// list; [`wms_native`] builds transformers, in this order, until `MAX_SYSTEMS` are kept.)
fn wms_candidates(crs: &[String]) -> Vec<Candidate> {
    let mut seen = std::collections::HashSet::new();
    let mut listed: Vec<Candidate> = crs
        .iter()
        .flat_map(|c| c.split_whitespace())
        .filter(|c| c.len() <= warp::MAX_SYSTEM_NAME && !is_web_mercator(c) && seen.insert(c.to_ascii_uppercase()))
        .enumerate()
        .filter_map(|(i, c)| {
            let code = warp::epsg_of(c)?;
            let system = crate::raster::Crs::from_epsg(code).filter(|s| *s != crate::raster::Crs::WebMercator)?;
            Some((c.to_string(), code, system, i))
        })
        .collect();
    // One name for each system (EPSG:4326 and CRS:84 differ only in their axes: CRS:84 kept).
    listed.sort_by_cached_key(|(name, code, _, _)| (*code, !warp::is_crs84(name)));
    listed.dedup_by_key(|(_, code, _, _)| *code);
    let rank = |(name, _, c, _): &Candidate| match (c.is_geographic(), warp::is_crs84(name)) {
        (false, _) => 0,
        (true, true) => 1,
        (true, false) => 2,
    };
    // In the order the service lists them, the preferred first.
    listed.sort_by_key(|l| (rank(l), l.3));
    listed
}

/// The systems a WMS layer is offered in that RustGIS can draw (#769), of its `listed`
/// candidates (see [`wms_candidates`]), and the one a Web Mercator map takes its pictures in: Web
/// Mercator itself (`mercator`), else the first. `None` for a layer offered in Web Mercator
/// alone (its address stays as it was) or in nothing usable. In WMS 1.3.0 a bounding box follows
/// the system's axes as EPSG defines them ([`crate::raster::epsg_axes`]); for a system whose
/// order RustGIS doesn't know, as the layer's own `BoundingBox` in it is written (else latitude
/// first in a geographic system other than CRS:84).
fn wms_native<'a, 'i>(
    layer: &Node<'a, 'i>,
    listed: &[Candidate],
    v13: bool,
    area: Option<[f64; 4]>,
    mercator: bool,
    read: &mut LayerBoxes<'a, 'i>,
) -> Option<(String, warp::Warp)> {
    // Its boxes by system (its own, else the nearest it inherits: WMS 1.3.0 §7.2.4.6.8), read
    // once for each layer they belong to.
    if v13 {
        for a in layer.ancestors().filter(|a| a.is_element() && local(a) == "Layer") {
            read.entry(a.id()).or_insert_with(|| {
                let mut boxes = std::collections::HashMap::new();
                for b in children(&a, "BoundingBox") {
                    // (By code: the box may name the system otherwise than the list, `EPSG:2056` for
                    // `urn:ogc:def:crs:EPSG::2056`; but a CRS:84 box, longitude first, isn't an
                    // EPSG:4326 one.)
                    if let Some(code) = b.attribute("CRS").or(b.attribute("SRS")).filter(|c| !warp::is_crs84(c)).and_then(warp::epsg_of) {
                        boxes.entry(code).or_insert(b);
                    }
                }
                boxes
            });
        }
    }
    let read = &*read;
    let lat_first = |name: &str, system: crate::raster::Crs| {
        v13 && !warp::is_crs84(name) && {
            // In the order of the system's axes as EPSG defines them (WMS 1.3.0 §6.7.3.3); for a
            // system newer than RustGIS's table, the layer's box in it, against its area there.
            if let Some(north) = epsg_north_first(system) {
                return north;
            }
            let own = warp::epsg_of(name).and_then(|code| layer.ancestors().find_map(|a| read.get(&a.id())?.get(&code)));
            let written = own.and_then(|b| {
                let f = |k: &str| b.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
                Some([f("minx")?, f("miny")?, f("maxx")?, f("maxy")?])
            });
            match written {
                Some(b) => {
                    let native = area.and_then(|a| warp::native_box(&*warp::transformer(u32::from(system.epsg()))?, a));
                    warp::north_first(b, native, system.is_geographic())
                }
                None => system.is_geographic(),
            }
        }
    };
    let systems: Vec<warp::System> = listed
        .iter()
        .filter(|(_, code, _, _)| warp::usable(*code).is_some())
        .take(warp::MAX_SYSTEMS)
        .filter_map(|(name, code, system, _)| {
            Some(warp::System { epsg: u16::try_from(*code).ok()?, name: name.clone(), lat_first: lat_first(name, *system) })
        })
        .collect();
    let first = systems.first()?.clone();
    let (epsg, lat_first) = if mercator { (3857, false) } else { (first.epsg, first.lat_first) };
    Some((first.name, warp::Warp { epsg, grid: warp::Grid::Map { lat_first }, area, systems }))
}

/// The `BoundingBox`es of the WMS layers read so far, by system. A service's layers often share one
/// parent: looking through its children or its boxes again for each would take the square of
/// their number.
type LayerBoxes<'a, 'i> = std::collections::HashMap<roxmltree::NodeId, std::collections::HashMap<u32, Node<'a, 'i>>>;

/// Whether `system` is written north (or latitude) first as EPSG defines its axes, when
/// RustGIS's table knows it ([`crate::raster::epsg_axes`]).
fn epsg_north_first(system: crate::raster::Crs) -> Option<bool> {
    match system {
        crate::raster::Crs::Custom(_) => None,
        s => crate::raster::epsg_axes::north_first(s.epsg()),
    }
}

/// The longest address a layer in another system keeps (the map's own limit on an address).
const MAX_ADDRESS: usize = 4096;

/// The most styles a WMS layer's legend is looked for among, its own and those it inherits.
const MAX_STYLES: usize = 64;

/// A WMS layer's legend: its default style's (the one a GetMap with `STYLES=` draws: the one
/// named `default`, else the first), the first of its `LegendURL`s in a format RustGIS shows.
fn wms_legend(url: &str, styles: &[Node]) -> Option<Legend> {
    let style =
        styles.iter().find(|s| child_text(s, "Name").is_some_and(|n| n.eq_ignore_ascii_case("default"))).or_else(|| styles.first())?;
    children(style, "LegendURL").find_map(|l| {
        if !legend_format(child_text(&l, "Format").as_deref()) {
            return None;
        }
        let href = child(&l, "OnlineResource").and_then(|o| href(&o))?;
        let url = resolve(url, &href).filter(|h| is_web(h))?;
        Some(Legend { url, ..Legend::default() })
    })
}

/// A service's GetLegendGraphic request for a layer, when its capabilities offer one (the SLD
/// profile's `GetLegendGraphic` among its requests) in PNG or JPEG.
fn wms_legend_graphic(root: &Node, url: &str, get_map: &str, version: &str) -> Option<impl Fn(&str) -> Legend + use<>> {
    let request = root.descendants().find(|n| n.is_element() && local(n) == "GetLegendGraphic")?;
    let formats: Vec<String> = children(&request, "Format").map(|f| text_of(&f)).collect();
    let format = ["image/png", "image/jpeg"].into_iter().find(|f| formats.iter().any(|g| g.trim().eq_ignore_ascii_case(f)))?;
    let get = request
        .descendants()
        .find(|n| n.is_element() && local(n) == "Get")
        .and_then(|g| child(&g, "OnlineResource"))
        .and_then(|o| href(&o))
        .and_then(|h| resolve(url, &h))
        .filter(|h| is_web(h))
        .unwrap_or_else(|| get_map.to_string());
    // WMS 1.3.0 asks for the SLD version the request is of (SLD 1.1.0's profile).
    let sld = if version.starts_with("1.3") { "&SLD_VERSION=1.1.0" } else { "" };
    let version = version.to_string();
    Some(move |layer: &str| Legend {
        url: with_query(
            &get,
            &format!("SERVICE=WMS&VERSION={version}&REQUEST=GetLegendGraphic&LAYER={}&FORMAT={}{sld}", encode(layer), encode(format)),
        ),
        ..Legend::default()
    })
}

/// Percent-encodes a query value.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b',' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// WMTS

/// The scale denominator of Web Mercator zoom 0 for 256-pixel tiles (OGC's 0.28 mm pixels).
const Z0_SCALE: f64 = 559_082_264.028_717_8;

/// A tile matrix set's zoom levels, when it is Web Mercator's own pyramid: each matrix's
/// identifier and its zoom.
fn mercator_matrices(tms: &Node) -> Option<Vec<(String, u8)>> {
    let crs = child_text(tms, "SupportedCRS")?;
    if !is_web_mercator(&crs) {
        return None;
    }
    let mut out = Vec::new();
    for m in children(tms, "TileMatrix") {
        let id = child_text(&m, "Identifier")?;
        let sd = child_text(&m, "ScaleDenominator")?.parse::<f64>().ok()?;
        let w = child_text(&m, "TileWidth").and_then(|t| t.parse::<u32>().ok()).unwrap_or(256);
        // Zoom from the scale: a pyramid of 512 px tiles has half the scale denominator of a
        // 256 px one at the same zoom.
        let z = (Z0_SCALE / sd * 256.0 / f64::from(w)).log2();
        let zr = z.round();
        if (z - zr).abs() > 0.02 || !(0.0..=30.0).contains(&zr) {
            return None;
        }
        // The pyramid's corner must be Web Mercator's.
        let corner: Vec<f64> = child_text(&m, "TopLeftCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        let ok = |a: f64, b: f64| (a.abs() - b).abs() < 1.0;
        if corner.len() != 2 || !corner.iter().all(|c| ok(*c, 20_037_508.342_789_244)) {
            return None;
        }
        out.push((id, zr as u8));
    }
    (!out.is_empty()).then_some(out)
}

/// The zooms a WMTS layer's tiles are at in its Web Mercator tile matrix set `set` (of `sets`,
/// each matrix's identifier and zoom): its matrices', or only those its link's
/// `TileMatrixSetLimits` names when it gives them (WMTS 1.0.0 §7.2.4.6.3).
fn wmts_zooms(layer: &Node, set: &str, sets: &[(String, Vec<(String, u8)>)]) -> Option<(u8, u8)> {
    let matrices = &sets.iter().find(|(id, _)| id == set)?.1;
    children(layer, "TileMatrixSetLink").find(|l| child_text(l, "TileMatrixSet").as_deref() == Some(set))?;
    let limited = wmts_limits(layer, set);
    let zooms = matrices.iter().filter(|(id, _)| limited.is_empty() || limited.contains(id)).map(|(_, z)| *z);
    let (lo, hi) = zooms.fold((u8::MAX, 0), |(lo, hi), z| (lo.min(z), hi.max(z)));
    (lo <= hi.min(24)).then_some((lo, hi.min(24)))
}

/// The tile matrices of `set` a WMTS layer's link limits it to (its `TileMatrixSetLimits`);
/// none when it doesn't.
fn wmts_limits(layer: &Node, set: &str) -> Vec<String> {
    children(layer, "TileMatrixSetLink")
        .find(|l| child_text(l, "TileMatrixSet").as_deref() == Some(set))
        .and_then(|link| child(&link, "TileMatrixSetLimits"))
        .map(|l| children(&l, "TileMatrixLimits").filter_map(|m| child_text(&m, "TileMatrix")).collect())
        .unwrap_or_default()
}

/// A tile matrix as a WMTS document gives it, its corner in the order written.
struct RawMatrix {
    id: String,
    scale: f64,
    corner: (f64, f64),
    tile: (u32, u32),
    size: (u32, u32),
}

/// The most `TileMatrix` entries read of a tile matrix set in another system.
const MAX_RAW_MATRICES: usize = 256;

/// A tile matrix set in another system than Web Mercator that RustGIS can draw (#769): its EPSG
/// code and its matrices RustGIS can draw, coarsest first.
fn native_matrices(tms: &Node) -> Option<(u16, crate::raster::Crs, Vec<RawMatrix>)> {
    let name = child_text(tms, "SupportedCRS")?;
    let code = warp::epsg_of(&name)?;
    let system = warp::usable(code)?;
    let unit = warp::metres_per_unit(code);
    let mut out = Vec::new();
    // (In any order, up to `MAX_RAW_MATRICES` read: a real set has a few dozen.)
    for m in children(tms, "TileMatrix").take(MAX_RAW_MATRICES) {
        // One that can't be read or drawn is left out, not the set; only one RustGIS can draw
        // counts towards `MAX_MATRICES`.
        if let Some(raw) = raw_matrix(&m)
            && warp::Matrix::from_scale(raw.id.clone(), raw.scale, unit, raw.corner, raw.tile, raw.size).is_some()
        {
            out.push(raw);
        }
    }
    // Coarsest first. (Each layer keeps those its limits name, at most `MAX_MATRICES`; the
    // corners' order is settled per layer, against its area.)
    out.sort_by(|a, b| b.scale.total_cmp(&a.scale));
    (!out.is_empty()).then_some((u16::try_from(code).ok()?, system, out))
}

/// A `TileMatrix` as the document gives it, if it gives all a matrix needs.
fn raw_matrix(m: &Node) -> Option<RawMatrix> {
    let n = |k: &str| child_text(m, k).and_then(|t| t.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    let u = |k: &str| child_text(m, k).and_then(|t| t.trim().parse::<u32>().ok()).filter(|v| *v > 0);
    let corner: Vec<f64> = child_text(m, "TopLeftCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    let [a, b] = corner.as_slice() else { return None };
    Some(RawMatrix {
        id: child_text(m, "Identifier")?,
        scale: n("ScaleDenominator").filter(|v| *v > 0.0)?,
        corner: (*a, *b),
        tile: (u("TileWidth")?, u("TileHeight")?),
        size: (u("MatrixWidth")?, u("MatrixHeight")?),
    })
}

/// Past `MAX_MATRICES` the middle of `ms` (coarsest first) goes, the coarsest half kept for a
/// wide view and the finest half for a close one.
fn keep_matrices<T>(ms: &mut Vec<T>) {
    let (coarse, len) = (warp::MAX_MATRICES / 2, ms.len());
    if len > warp::MAX_MATRICES {
        ms.drain(coarse..len - (warp::MAX_MATRICES - coarse));
    }
}

fn wmts(url: &str, root: &Node) -> Service {
    let title = child(root, "ServiceIdentification").and_then(|s| child_text(&s, "Title")).unwrap_or_default();
    let mut service = Service { kind: "WMTS".into(), title, ..Service::default() };
    let Some(contents) = child(root, "Contents") else { return service };
    // Tile matrix sets that are Web Mercator pyramids, with a zoom per matrix identifier.
    let sets: Vec<(String, Vec<(String, u8)>)> =
        children(&contents, "TileMatrixSet").filter_map(|t| Some((child_text(&t, "Identifier")?, mercator_matrices(&t)?))).collect();
    // Others in a system RustGIS can transform, drawn warped into the map's tiles (#769).
    let natives: Vec<(String, u16, crate::raster::Crs, bool, Vec<RawMatrix>)> = children(&contents, "TileMatrixSet")
        .filter_map(|t| {
            let id = child_text(&t, "Identifier")?;
            let crs84 = child_text(&t, "SupportedCRS").is_some_and(|c| warp::is_crs84(&c));
            let (code, system, ms) = native_matrices(&t)?;
            Some((id, code, system, crs84, ms))
        })
        .collect();
    let kvp = root
        .descendants()
        .find(|n| n.is_element() && local(n) == "GetTile")
        .and_then(|g| g.descendants().find(|n| n.is_element() && local(n) == "Get"))
        .and_then(|g| href(&g))
        .and_then(|h| resolve(url, &h))
        .map(|h| endpoint(&h))
        .unwrap_or_else(|| endpoint(url));
    let (mut skipped, mut too_long, mut long_times) = (0, 0, 0);
    let limit = read_limit(kvp.len());
    let left = children(&contents, "Layer").count().saturating_sub(limit);
    if left > 0 {
        note_past_read_limit(&mut service, left, "layers", limit);
        note_copied_addresses(&mut service, kvp.len(), limit);
    }
    for layer in children(&contents, "Layer").take(limit) {
        let Some(name) = child_text(&layer, "Identifier") else { continue };
        let title = child_text(&layer, "Title").unwrap_or_else(|| name.clone());
        let default_style = children(&layer, "Style").find(|s| s.attribute("isDefault") == Some("true")).or_else(|| child(&layer, "Style"));
        let style = default_style.and_then(|s| child_text(&s, "Identifier")).unwrap_or_else(|| "default".into());
        // Its default style's legends (WMTS 1.0.0 §7.1.1.1.1), each for the scales it gives.
        let legends: Vec<Legend> = default_style
            .iter()
            .flat_map(|s| children(s, "LegendURL"))
            .filter(|l| legend_format(l.attribute("format")))
            .filter_map(|l| {
                let url = href(&l).and_then(|h| resolve(url, &h)).filter(|h| is_web(h))?;
                Some(Legend { url, min_scale: scale_attr(&l, "minScaleDenominator"), max_scale: scale_attr(&l, "maxScaleDenominator") })
            })
            .take(MAX_LEGENDS)
            .collect();
        let linked: Vec<String> = children(&layer, "TileMatrixSetLink").filter_map(|l| child_text(&l, "TileMatrixSet")).collect();
        let bbox = child(&layer, "WGS84BoundingBox").and_then(|b| {
            let lo: Vec<f64> = child_text(&b, "LowerCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            let hi: Vec<f64> = child_text(&b, "UpperCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            lonlat_box(*lo.first()?, *lo.get(1)?, *hi.first()?, *hi.get(1)?)
        });
        // A Web Mercator set whose matrix identifiers are a fixed prefix then the zoom, drawn as
        // it is; else a set in another system, warped.
        let mercator = sets.iter().filter(|(id, _)| linked.contains(id)).find_map(|(set, matrices)| {
            let prefix = matrices.iter().find_map(|(id, z)| id.strip_suffix(&z.to_string()).map(str::to_string));
            let consistent = prefix.as_ref().is_some_and(|p| matrices.iter().all(|(id, z)| *id == format!("{p}{z}")));
            prefix.filter(|_| consistent).map(|p| (set.clone(), p))
        });
        let native = match mercator {
            Some(_) => None,
            None => natives.iter().filter(|n| linked.contains(&n.0)).find_map(|(set, code, system, crs84, raw)| {
                let geographic = system.is_geographic();
                let area = bbox.and_then(|b| warp::native_box(&*warp::transformer(u32::from(*code))?, b));
                // Only the matrices the layer's link limits it to (WMTS 1.0.0 §7.2.4.6.3), when it does.
                let limited = wmts_limits(&layer, set);
                let mut raw: Vec<&RawMatrix> = raw.iter().filter(|m| limited.is_empty() || limited.contains(&m.id)).collect();
                keep_matrices(&mut raw);
                let unit = warp::metres_per_unit(u32::from(*code));
                let first = raw.first()?;
                let m = warp::Matrix::from_scale(first.id.clone(), first.scale, unit, first.corner, first.tile, first.size)?;
                let span = (m.cell * f64::from(m.tile_w) * f64::from(m.cols), m.cell * f64::from(m.tile_h) * f64::from(m.rows));
                // In the order of the system's axes as EPSG defines them (WMTS 1.0.0 §6.1). A
                // geographic corner is often written longitude first anyway: one whose first number
                // can't be a latitude is so; one whose numbers could be either is read longitude
                // first only when its matrix, read in EPSG's order, misses the layer and read the
                // other way doesn't. For a system newer than RustGIS's table, the order whose
                // matrix covers more of the layer, else latitude first in a geographic system.
                let swap = !*crs84
                    && match epsg_north_first(*system) {
                        Some(_) if geographic && first.corner.0.abs() > 90.0 => false,
                        // Read the other way only when EPSG's reading misses the layer altogether
                        // and that one doesn't: a correct corner is kept, however the other covers.
                        Some(north) if geographic => match warp::corner_coverage(first.corner, span, area) {
                            Some((east, north_cov)) => {
                                let (kept, other) = if north { (north_cov, east) } else { (east, north_cov) };
                                if kept <= 0.0 && other > 0.0 { !north } else { north }
                            }
                            None => north,
                        },
                        Some(north) => north,
                        None => warp::corner_north_first(first.corner, span, area).unwrap_or(geographic),
                    };
                let matrices: Vec<warp::Matrix> = raw
                    .iter()
                    .filter_map(|m| {
                        let corner = if swap { (m.corner.1, m.corner.0) } else { m.corner };
                        warp::Matrix::from_scale(m.id.clone(), m.scale, unit, corner, m.tile, m.size)
                    })
                    .collect();
                (!matrices.is_empty())
                    .then(|| (set.clone(), warp::Warp { epsg: *code, grid: warp::Grid::Tiles(matrices), area: bbox, systems: Vec::new() }))
            }),
        };
        let (set, matrix, row, col) = match (&mercator, &native) {
            (Some((set, prefix)), _) => (set.clone(), format!("{prefix}{{z}}"), "{y}", "{x}"),
            (None, Some((set, _))) => (set.clone(), "{nm}".to_string(), "{nrow}", "{ncol}"),
            (None, None) => {
                skipped += 1;
                continue;
            }
        };
        let set = &set;
        // A time dimension: the template with `{Time}` is preferred, which takes the time asked
        // for (its default otherwise); without one, a template with no `{Time}` is.
        // Its default: the one given, else the last time its values list (a range's end); with
        // neither, the layer is drawn without a time.
        let time = children(&layer, "Dimension")
            .find(|d| child_text(d, "Identifier").is_some_and(|i| i.eq_ignore_ascii_case("time")))
            .and_then(|d| {
                layer_time(
                    child_text(&d, "Default").as_deref(),
                    last(children(&d, "Value").map(|v| text_of(&v)).collect(), MAX_TIMES_READ),
                    checked_time,
                    false,
                )
            });
        let templates: Vec<String> = children(&layer, "ResourceURL")
            .filter(|r| r.attribute("resourceType") == Some("tile"))
            .filter(|r| {
                let f = r.attribute("format").unwrap_or("");
                !f.contains("pbf") && !f.contains("mvt")
            })
            .filter_map(|r| r.attribute("template").and_then(|t| resolve(url, t)))
            .collect();
        let timed = |t: &&String| t.contains("{Time}");
        let resource =
            if time.is_some() { templates.iter().find(timed).or_else(|| templates.first()) } else { templates.iter().find(|t| !timed(t)) }
                .cloned();
        // A `{Time}` the layer has no dimension for can't be filled.
        let time = time.filter(|_| resource.as_ref().is_none_or(|t| t.contains("{Time}")));
        let template = match resource {
            Some(t) => t
                .replace("{TileMatrixSet}", set)
                .replace("{Style}", &style)
                .replace("{TileMatrix}", &matrix)
                .replace("{TileRow}", row)
                .replace("{TileCol}", col),
            None => {
                let format =
                    children(&layer, "Format").map(|f| text_of(&f)).find(|f| f.starts_with("image/")).unwrap_or_else(|| "image/png".into());
                let get = with_query(
                    &kvp,
                    &format!(
                        "SERVICE=WMTS&REQUEST=GetTile&VERSION=1.0.0&LAYER={}&STYLE={}&TILEMATRIXSET={}&TILEMATRIX={matrix}&TILEROW={row}&TILECOL={col}&FORMAT={}",
                        encode(&name),
                        encode(&style),
                        encode(set),
                        encode(&format)
                    ),
                );
                if time.is_some() { format!("{get}&TIME={{Time}}") } else { get }
            }
        };
        let template = match &native {
            Some((_, w)) => w.address(&template),
            None => template,
        };
        // An address in another system no longer than a tile layer keeps (a set of a great many
        // matrices, in its fragment).
        if native.is_some() && template.len() > MAX_ADDRESS {
            too_long += 1;
            continue;
        }
        long_times += usize::from(time.as_ref().is_some_and(|t| t.cut > 0));
        service.layers.push(ServiceLayer {
            // A Web Mercator set's own zooms, else those a warped set's matrices make (#769).
            zooms: wmts_zooms(&layer, set, &sets).or_else(|| native.as_ref().and_then(|(_, w)| w.zooms())),
            name,
            title,
            offer: Offer::Tiles { template },
            bbox,
            tilesets: None,
            time,
            feature_info: None,
            legends,
        });
    }
    cut_listing(&mut service);
    if skipped > 0 {
        service.notes.push(format!("{skipped} layers have no tile matrix set RustGIS can draw and are left out"));
    }
    if too_long > 0 {
        service.notes.push(format!(
            "{too_long} layers in another coordinate system are left out: their addresses are longer than {MAX_ADDRESS} bytes"
        ));
    }
    if long_times > 0 {
        service.notes.push(long_times_note(long_times));
    }
    service
}

// ---------------------------------------------------------------------------------------------
// WFS

/// The output formats an `outputFormat` parameter lists among `scope`'s nodes.
fn output_formats<'a, 'i: 'a>(scope: impl Iterator<Item = Node<'a, 'i>>) -> Vec<String> {
    scope
        .filter(|n| {
            n.is_element() && local(n) == "Parameter" && n.attribute("name").is_some_and(|a| a.eq_ignore_ascii_case("outputFormat"))
        })
        .flat_map(|p| p.descendants().filter(|v| v.is_element() && local(v) == "Value").map(|v| text_of(&v)).collect::<Vec<_>>())
        .collect()
}

/// Whether a coordinate system code is OGC's CRS84 (longitude/latitude by definition).
fn is_crs84(code: &str) -> bool {
    let c = code.to_ascii_uppercase();
    c.contains("CRS84") || c.contains("CRS:84")
}

fn wfs(url: &str, root: &Node) -> Service {
    let title =
        child(root, "ServiceIdentification").or_else(|| child(root, "Service")).and_then(|s| child_text(&s, "Title")).unwrap_or_default();
    let mut service = Service { kind: "WFS".into(), title, ..Service::default() };
    let version = root.attribute("version").unwrap_or("1.1.0");
    // GetFeature's address: an `xlink:href` (WFS 1.1, 2.0) or an `onlineResource` attribute
    // (WFS 1.0); the capabilities address when the document gives no web address.
    let get = root
        .descendants()
        .find(|n| {
            n.is_element()
                && matches!(local(n), "Operation" | "GetFeature")
                && (local(n) == "GetFeature" || n.attribute("name") == Some("GetFeature"))
        })
        .and_then(|g| g.descendants().find(|n| n.is_element() && local(n) == "Get"))
        .and_then(|g| href(&g).or_else(|| g.attribute("onlineResource").map(str::to_string)))
        .and_then(|h| resolve(url, &h))
        .map(|h| endpoint(&h))
        .unwrap_or_else(|| endpoint(url));
    // The GeoJSON output format the server names (GetFeature's, else the service-wide list);
    // `application/json` when it lists none.
    let formats: Vec<String> = {
        let op = root.descendants().find(|n| n.is_element() && local(n) == "Operation" && n.attribute("name") == Some("GetFeature"));
        let in_op = output_formats(op.iter().flat_map(|o| o.descendants()));
        if in_op.is_empty() { output_formats(root.descendants()) } else { in_op }
    };
    let json = if formats.is_empty() {
        Some("application/json".to_string())
    } else {
        formats.iter().find(|f| f.to_ascii_lowercase().contains("json")).cloned()
    };
    let Some(json) = json else {
        service.notes.push("the service offers no GeoJSON output, so its feature types can't be added".into());
        return service;
    };
    let Some(list) = child(root, "FeatureTypeList") else { return service };
    let (per_layer, limit) = (get.len() + json.len(), read_limit(get.len() + json.len()));
    let left = children(&list, "FeatureType").count().saturating_sub(limit);
    if left > 0 {
        note_past_read_limit(&mut service, left, "feature types", limit);
        note_copied_addresses(&mut service, per_layer, limit);
    }
    for ft in children(&list, "FeatureType").take(limit) {
        let Some(name) = child_text(&ft, "Name") else { continue };
        let title = child_text(&ft, "Title").unwrap_or_else(|| name.clone());
        let bbox = child(&ft, "WGS84BoundingBox")
            .and_then(|b| {
                let lo: Vec<f64> = child_text(&b, "LowerCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                let hi: Vec<f64> = child_text(&b, "UpperCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                lonlat_box(*lo.first()?, *lo.get(1)?, *hi.first()?, *hi.get(1)?)
            })
            .or_else(|| {
                let b = child(&ft, "LatLongBoundingBox")?;
                let f = |k: &str| b.attribute(k).and_then(|t| t.parse::<f64>().ok());
                lonlat_box(f("minx")?, f("miny")?, f("maxx")?, f("maxy")?)
            });
        // CRS84 when the type offers it: longitude first by definition. Otherwise EPSG:4326 (MapServer
        // refuses a CRS84 the type doesn't list), which every server tried in #1053 answered
        // longitude first; deegree answers latitude first when asked in the URN form instead. The
        // `crs` an answer names doesn't tell the order (GeoServer names its longitude-first GeoJSON
        // `urn:ogc:def:crs:EPSG::4326`), so an answer is turned round only when its positions miss
        // the type's bounds and fit them turned (`fix_axes`).
        let crs84 = ft.children().any(|c| {
            c.is_element() && matches!(local(&c), "DefaultCRS" | "OtherCRS" | "DefaultSRS" | "OtherSRS" | "SRS") && is_crs84(&text_of(&c))
        });
        let srs = if crs84 { "urn:ogc:def:crs:OGC:1.3:CRS84" } else { "EPSG:4326" };
        // WFS 2.0 names the type and the count differently.
        let query = if version.starts_with('2') {
            format!(
                "SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature&TYPENAMES={}&OUTPUTFORMAT={}&SRSNAME={srs}&COUNT={FEATURE_LIMIT}",
                encode(&name),
                encode(&json)
            )
        } else {
            let v = if version.starts_with("1.0") { "1.0.0" } else { "1.1.0" };
            format!(
                "SERVICE=WFS&VERSION={v}&REQUEST=GetFeature&TYPENAME={}&OUTPUTFORMAT={}&SRSNAME={srs}&MAXFEATURES={FEATURE_LIMIT}",
                encode(&name),
                encode(&json)
            )
        };
        let url = with_query(&get, &query);
        service.layers.push(ServiceLayer {
            zooms: None,
            name,
            title,
            offer: Offer::Features { url },
            bbox,
            tilesets: None,
            time: None,
            feature_info: None,
            legends: Vec::new(),
        });
    }
    cut_listing(&mut service);
    service
}

// ---------------------------------------------------------------------------------------------
// Features over an area

/// Whether an address's path ends in `/items` (an OGC API collection's), in any case.
fn ends_with_items(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    p.len() >= 6 && p.get(p.len() - 6..).is_some_and(|tail| tail.eq_ignore_ascii_case("/items"))
}

/// Query parameters that already filter a request by place or by feature, or that name another
/// coordinate system for a box (OGC API's `bbox-crs`): an address carrying one is left as it is.
const SPATIAL_KEYS: [&str; 7] = ["bbox", "bbox-crs", "filter", "cql_filter", "featureid", "resourceid", "geometry"];

/// An [`Offer::Features`] address that asks only for the features crossing a longitude/latitude
/// `area` (west, south, east, north), in each service's own terms:
/// - a WFS 1.1 or 2.0 GetFeature: `BBOX` in CRS84 (longitude first) when its `SRSNAME` is
///   CRS84, else in `urn:ogc:def:crs:EPSG::4326`, latitude first as that URN fixes (a bare
///   `EPSG:4326` is read either way by different servers);
/// - an ArcGIS layer's query: an envelope in EPSG:4326 that the features intersect;
/// - an OGC API collection's items: `bbox`, longitude first by the standard (its `next` links
///   keep it).
///
/// `None` when the request can't carry the box: a WFS 1.0 GetFeature (its `BBOX` is in the
/// type's own coordinate system, which its capabilities may not give as longitude/latitude), an
/// address that already filters, or an area across the antimeridian or off the globe.
pub fn features_in_area(url: &str, area: [f64; 4]) -> Option<String> {
    let [w, s, e, n] = area;
    let lon = |x: f64| (-180.0..=180.0).contains(&x);
    let lat = |y: f64| (-90.0..=90.0).contains(&y);
    if !(lon(w) && lon(e) && lat(s) && lat(n) && w <= e && s <= n) {
        return None;
    }
    if SPATIAL_KEYS.iter().any(|k| query_value(url, k).is_some()) {
        return None;
    }
    let is = |key: &str, value: &str| query_value(url, key).is_some_and(|v| v.eq_ignore_ascii_case(value));
    let q = if is("service", "wfs") && is("request", "getfeature") {
        if query_value(url, "version").is_none_or(|v| v.starts_with("1.0")) {
            return None;
        }
        if query_value(url, "srsname").is_some_and(is_crs84) {
            format!("BBOX={w},{s},{e},{n},urn:ogc:def:crs:OGC:1.3:CRS84")
        } else {
            format!("BBOX={s},{w},{n},{e},urn:ogc:def:crs:EPSG::4326")
        }
    } else if is_arcgis_query(url) {
        format!("geometry={w},{s},{e},{n}&geometryType=esriGeometryEnvelope&inSR=4326&spatialRel=esriSpatialRelIntersects")
    } else if ends_with_items(base(url)) {
        format!("bbox={w},{s},{e},{n}")
    } else {
        return None;
    };
    Some(with_query(url, &q))
}

// ---------------------------------------------------------------------------------------------
// WCS

/// The largest side, in pixels, of a coverage asked for (GetCoverage scales it to fit).
pub const COVERAGE_MAX_PIXELS: u32 = 2048;

/// A WCS coverage: what DescribeCoverage and GetCoverage need.
#[derive(Clone, Debug, PartialEq)]
pub struct Coverage {
    /// The service's address for its requests (the capabilities' GetCoverage address).
    pub endpoint: String,
    /// The protocol asked in: `2.0.1` or `1.0.0` (a WCS 1.1 service is asked in 1.0, which the
    /// common servers answer too, rather than 1.1's multipart answers).
    pub version: String,
    pub id: String,
    /// The output formats the capabilities list (WCS 2.0's `formatSupported`): one list for all
    /// of a document's coverages, which may be a great many.
    pub formats: std::sync::Arc<[String]>,
}

/// What DescribeCoverage says a coverage is: the GeoTIFF format to ask for, the names of its
/// longitude-like and latitude-like axes (WCS 2.0's subsets), and its longitude/latitude bounds
/// when they can be known without reprojecting.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageInfo {
    pub format: String,
    pub axes: Option<(String, String)>,
    pub bbox: Option<[f64; 4]>,
}

fn wcs(url: &str, root: &Node) -> Service {
    let title =
        child(root, "ServiceIdentification").or_else(|| child(root, "Service")).and_then(|s| child_text(&s, "Title")).unwrap_or_default();
    let doc_version = root.attribute("version").unwrap_or("1.0.0");
    let v2 = doc_version.starts_with('2');
    let version = if v2 { "2.0.1" } else { "1.0.0" };
    let mut service = Service { kind: "WCS".into(), title, ..Service::default() };
    if doc_version.starts_with("1.1") {
        service.notes.push("a WCS 1.1 service: its coverages are asked for in WCS 1.0".into());
    }
    // GetCoverage's address: an `ows:Operation` (1.1, 2.0) or a `GetCoverage` request's
    // `OnlineResource` (1.0); the capabilities address when the document gives no web address.
    let get = root
        .descendants()
        .find(|n| n.is_element() && (local(n) == "GetCoverage" || (local(n) == "Operation" && n.attribute("name") == Some("GetCoverage"))))
        .and_then(|g| g.descendants().find(|n| n.is_element() && local(n) == "Get"))
        .and_then(|g| href(&g).or_else(|| g.descendants().find(|n| n.is_element() && local(n) == "OnlineResource").and_then(|o| href(&o))))
        // MapServer escapes its `&` twice: `&amp;` is still there after the XML is read.
        .map(|h| h.replace("&amp;", "&"))
        .and_then(|h| resolve(url, &h))
        .map(|h| endpoint(&h))
        .unwrap_or_else(|| endpoint(url));
    let formats: std::sync::Arc<[String]> = root
        .descendants()
        .filter(|n| n.is_element() && matches!(local(n), "formatSupported" | "SupportedFormat"))
        .map(|n| text_of(&n))
        .filter(|f| !f.is_empty() && f.len() < 200)
        .take(64)
        .collect();
    let corners = |b: &Node| -> Option<[f64; 4]> {
        let lo: Vec<f64> = child_text(b, "LowerCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        let hi: Vec<f64> = child_text(b, "UpperCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        lonlat_box(*lo.first()?, *lo.get(1)?, *hi.first()?, *hi.get(1)?)
    };
    let mut add = |id: String, title: Option<String>, bbox: Option<[f64; 4]>| {
        let coverage =
            Coverage { endpoint: get.clone(), version: version.into(), id: id.clone(), formats: std::sync::Arc::clone(&formats) };
        service.layers.push(ServiceLayer {
            zooms: None,
            title: title.filter(|t| !t.is_empty()).unwrap_or_else(|| id.clone()),
            name: id,
            offer: Offer::Coverage(coverage),
            bbox,
            tilesets: None,
            time: None,
            feature_info: None,
            legends: Vec::new(),
        });
    };
    let (mut n, mut left, mut long_ids) = (0, 0, 0);
    let limit = read_limit(get.len());
    // WCS 1.1 and 2.0: `CoverageSummary` (nested ones too), named by `CoverageId` or `Identifier`.
    for s in root.descendants().filter(|n| n.is_element() && local(n) == "CoverageSummary") {
        let Some(id) = child_text(&s, "CoverageId").or_else(|| child_text(&s, "Identifier")) else { continue };
        if id.len() > 500 {
            long_ids += 1;
            continue;
        }
        if n >= limit {
            left += 1;
            continue;
        }
        n += 1;
        let bbox = child(&s, "WGS84BoundingBox").and_then(|b| corners(&b));
        add(id, child_text(&s, "Title"), bbox);
    }
    // WCS 1.0: `CoverageOfferingBrief`, named by `name`, its bounds a `lonLatEnvelope`.
    for b in root.descendants().filter(|n| n.is_element() && local(n) == "CoverageOfferingBrief") {
        let Some(id) = child_text(&b, "name") else { continue };
        if id.len() > 500 {
            long_ids += 1;
            continue;
        }
        if n >= limit {
            left += 1;
            continue;
        }
        n += 1;
        let bbox = child(&b, "lonLatEnvelope").and_then(|e| {
            let pos: Vec<f64> = children(&e, "pos")
                .flat_map(|p| text_of(&p).split_whitespace().filter_map(|t| t.parse().ok()).collect::<Vec<f64>>())
                .collect();
            lonlat_box(*pos.first()?, *pos.get(1)?, *pos.get(2)?, *pos.get(3)?)
        });
        add(id, child_text(&b, "label"), bbox);
    }
    cut_listing(&mut service);
    if left > 0 {
        note_past_read_limit(&mut service, left, "coverages", limit);
        note_copied_addresses(&mut service, get.len(), limit);
    }
    if long_ids > 0 {
        service.notes.push(format!("{long_ids} coverages are left out: their identifiers are longer than 500 bytes"));
    }
    service
}

/// The DescribeCoverage request of a coverage.
pub fn describe_coverage_url(c: &Coverage) -> String {
    let q = if c.version.starts_with('2') {
        format!("SERVICE=WCS&VERSION={}&REQUEST=DescribeCoverage&COVERAGEID={}", c.version, encode(&c.id))
    } else {
        format!("SERVICE=WCS&VERSION=1.0.0&REQUEST=DescribeCoverage&COVERAGE={}", encode(&c.id))
    };
    with_query(&c.endpoint, &q)
}

/// The GeoTIFF format among those offered: one naming TIFF (`image/tiff`, `GeoTIFF`,
/// `GEOTIFF_INT16`), else `image/tiff`.
fn tiff_format<'a>(offered: impl IntoIterator<Item = &'a str>) -> String {
    offered.into_iter().find(|f| f.to_ascii_lowercase().contains("tif")).unwrap_or("image/tiff").to_string()
}

/// Which of a coverage's axes, by their labels, runs east-west and which north-south: by name
/// (`Long`/`Lat`, `x`/`y`, `E`/`N`), else in the order given (latitude first for EPSG:4326).
fn lonlat_axes(labels: &[&str], srs: &str) -> Option<(String, String)> {
    let (a, b) = (*labels.first()?, *labels.get(1)?);
    let ok = |s: &str| !s.is_empty() && s.len() <= 32 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
    if !ok(a) || !ok(b) {
        return None;
    }
    let is_lat = |s: &str| matches!(s.to_ascii_lowercase().as_str(), "lat" | "latitude" | "y" | "n" | "north" | "northing");
    let is_lon = |s: &str| matches!(s.to_ascii_lowercase().as_str(), "long" | "lon" | "longitude" | "x" | "e" | "east" | "easting");
    Some(if is_lon(a) || is_lat(b) {
        (a.to_string(), b.to_string())
    } else if is_lat(a) || is_lon(b) || srs.ends_with("/4326") || srs.ends_with(":4326") {
        (b.to_string(), a.to_string())
    } else {
        (a.to_string(), b.to_string())
    })
}

/// Reads a coverage's DescribeCoverage answer.
pub fn parse_describe(c: &Coverage, body: &[u8]) -> Result<CoverageInfo, String> {
    let text = std::str::from_utf8(body).map_err(|_| "the coverage's description isn't UTF-8 text".to_string())?;
    let doc = xml(text, 1_000_000).map_err(|e| format!("the coverage's description: {e}"))?;
    let root = doc.root_element();
    if matches!(local(&root), "ServiceExceptionReport" | "ExceptionReport") {
        return Err(format!("the service answered with an error: {}", text_of(&root).chars().take(300).collect::<String>()));
    }
    if c.version.starts_with('2') {
        let Some(env) = root.descendants().find(|n| n.is_element() && local(n) == "Envelope") else {
            return Err(format!("{}: the description gives no envelope", c.id));
        };
        let srs = env.attribute("srsName").unwrap_or("").to_string();
        let labels_text = env.attribute("axisLabels").unwrap_or("").to_string();
        let labels: Vec<&str> = labels_text.split_whitespace().collect();
        let axes = lonlat_axes(&labels, &srs);
        let corner = |k: &str| -> Option<Vec<f64>> {
            let t = child_text(&env, k)?;
            Some(t.split_whitespace().filter_map(|v| v.parse().ok()).collect())
        };
        let s = srs.to_ascii_lowercase();
        let bbox = (|| {
            let (lo, hi) = (corner("lowerCorner")?, corner("upperCorner")?);
            let (lo0, lo1, hi0, hi1) = (*lo.first()?, *lo.get(1)?, *hi.first()?, *hi.get(1)?);
            // Which corner value is the longitude: by the axes' order in the labels; without
            // labels, EPSG:4326's own order (latitude first), CRS84's and others' longitude first.
            let lon_first = match &axes {
                Some((lon, _)) => labels.first() == Some(&lon.as_str()),
                None => !(s.ends_with("/4326") || s.ends_with(":4326")),
            };
            if s.ends_with("/4326") || s.ends_with(":4326") || s.contains("crs84") {
                if lon_first { lonlat_box(lo0, lo1, hi0, hi1) } else { lonlat_box(lo1, lo0, hi1, hi0) }
            } else if is_web_mercator(&srs) {
                let (w, so) = merc_to_lonlat(lo0, lo1);
                let (e, n) = merc_to_lonlat(hi0, hi1);
                lonlat_box(w, so, e, n)
            } else {
                None
            }
        })();
        let native = root.descendants().find(|n| n.is_element() && local(n) == "nativeFormat").map(|n| text_of(&n));
        let offered: Vec<&str> =
            native.iter().map(String::as_str).filter(|f| !f.is_empty()).chain(c.formats.iter().map(String::as_str)).collect();
        return Ok(CoverageInfo { format: tiff_format(offered), axes, bbox });
    }
    // WCS 1.0: the formats it answers in, and its longitude/latitude envelope.
    let formats: Vec<String> = root
        .descendants()
        .filter(|n| n.is_element() && local(n) == "formats")
        .map(|n| text_of(&n))
        .filter(|f| !f.is_empty() && f.len() < 200)
        .take(64)
        .collect();
    if root.descendants().all(|n| !(n.is_element() && local(&n) == "CoverageOffering")) {
        return Err(format!("{}: not a coverage description", c.id));
    }
    let bbox = root.descendants().find(|n| n.is_element() && local(n) == "lonLatEnvelope").and_then(|e| {
        let pos: Vec<f64> =
            children(&e, "pos").flat_map(|p| text_of(&p).split_whitespace().filter_map(|t| t.parse().ok()).collect::<Vec<f64>>()).collect();
        lonlat_box(*pos.first()?, *pos.get(1)?, *pos.get(2)?, *pos.get(3)?)
    });
    Ok(CoverageInfo { format: tiff_format(formats.iter().map(String::as_str)), axes: None, bbox })
}

/// The pixel size of a coverage asked for over a longitude/latitude `area`: its longer side
/// [`COVERAGE_MAX_PIXELS`], square degrees.
pub fn coverage_size(area: [f64; 4]) -> (u32, u32) {
    let (dx, dy) = ((area[2] - area[0]).abs().max(1e-9), (area[3] - area[1]).abs().max(1e-9));
    let m = f64::from(COVERAGE_MAX_PIXELS);
    let (w, h) = if dx >= dy { (m, (m * dy / dx).round()) } else { ((m * dx / dy).round(), m) };
    (w.clamp(1.0, m) as u32, h.clamp(1.0, m) as u32)
}

/// The GetCoverage request of a coverage over a longitude/latitude `area`, as a GeoTIFF in
/// EPSG:4326 (the server reprojects; WCS 2.0 through its CRS extension), scaled to fit
/// [`COVERAGE_MAX_PIXELS`].
pub fn get_coverage_url(c: &Coverage, info: &CoverageInfo, area: [f64; 4]) -> String {
    let [w, s, e, n] = area;
    let (pw, ph) = coverage_size(area);
    let q = if c.version.starts_with('2') {
        let (lon, lat) = info.axes.clone().unwrap_or_else(|| ("x".into(), "y".into()));
        let crs = "http://www.opengis.net/def/crs/EPSG/0/4326";
        format!(
            "SERVICE=WCS&VERSION={}&REQUEST=GetCoverage&COVERAGEID={}&FORMAT={}&SUBSETTINGCRS={crs}&OUTPUTCRS={crs}&SUBSET={lon}({w},{e})&SUBSET={lat}({s},{n})&SCALESIZE={lon}({pw}),{lat}({ph})",
            c.version,
            encode(&c.id),
            encode(&info.format)
        )
    } else {
        format!(
            "SERVICE=WCS&VERSION=1.0.0&REQUEST=GetCoverage&COVERAGE={}&CRS=EPSG:4326&BBOX={w},{s},{e},{n}&WIDTH={pw}&HEIGHT={ph}&FORMAT={}",
            encode(&c.id),
            encode(&info.format)
        )
    };
    with_query(&c.endpoint, &q)
}

/// The part of `area` a coverage covers (all of `area` when its bounds aren't known); `None` when
/// they don't meet.
pub fn coverage_area(info: &CoverageInfo, area: [f64; 4]) -> Option<[f64; 4]> {
    let Some([w, s, e, n]) = info.bbox else { return lonlat_box(area[0], area[1], area[2], area[3]) };
    lonlat_box(area[0].max(w), area[1].max(s), area[2].min(e), area[3].min(n))
}

// ---------------------------------------------------------------------------------------------
// CSW

/// Records asked of a catalogue in one request.
pub const RECORDS_PAGE: u32 = 20;

/// A catalogue record's link: a service (`wms`, `wmts`, `wfs`, `wcs`, `ogcapi`, `arcgis`) RustGIS
/// can open, a file to download, or another web page.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordLink {
    pub url: String,
    pub kind: &'static str,
    /// The layer the link names, when it names one (a WMS link's layer).
    pub name: Option<String>,
    pub protocol: String,
}

/// A catalogue record (Dublin Core): its identifier, title, abstract, bounds and links.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub bbox: Option<[f64; 4]>,
    pub links: Vec<RecordLink>,
}

/// A page of a catalogue search: how many records match, the next page's first position, and the
/// records.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordPage {
    pub matched: u64,
    pub next: Option<u64>,
    pub records: Vec<Record>,
}

/// The GetRecords request (CSW 2.0.2, Dublin Core records) of a search: words anywhere in the
/// records, within a longitude/latitude box, from position `start` (1-based).
pub fn records_url(endpoint_url: &str, words: &str, bbox: Option<[f64; 4]>, start: u64) -> String {
    let words: String = words.chars().filter(|c| !c.is_control()).take(200).collect::<String>().trim().replace('\'', "''");
    let mut cql = Vec::new();
    if !words.is_empty() {
        cql.push(format!("AnyText LIKE '%{words}%'"));
    }
    if let Some([w, s, e, n]) = bbox {
        cql.push(format!("BBOX(ows:BoundingBox,{w},{s},{e},{n})"));
    }
    let mut q = format!(
        "SERVICE=CSW&VERSION=2.0.2&REQUEST=GetRecords&typeNames=csw:Record&resultType=results&elementSetName=full&outputSchema={}&maxRecords={RECORDS_PAGE}&startPosition={}",
        encode("http://www.opengis.net/cat/csw/2.0.2"),
        start.max(1)
    );
    if !cql.is_empty() {
        q.push_str(&format!("&constraintLanguage=CQL_TEXT&constraint_language_version=1.1.0&constraint={}", encode(&cql.join(" AND "))));
    }
    with_query(&endpoint(endpoint_url), &q)
}

/// What a link's protocol (or, when it says nothing useful, its address) makes it.
fn link_kind(protocol: &str, url: &str) -> &'static str {
    let p = protocol.to_ascii_lowercase();
    let u = url.to_ascii_lowercase();
    let says = |k: &str| p.contains(k);
    if says("wmts") {
        "wmts"
    } else if says("wms") {
        "wms"
    } else if says("wfs") {
        "wfs"
    } else if says("wcs") {
        "wcs"
    } else if says("ogc:api") || says("ogcapi") || says("ogc api") {
        "ogcapi"
    } else if says("esri:rest") || says("arcgis") {
        "arcgis"
    } else if says("download") {
        "download"
    } else if u.contains("service=wmts") || u.contains("/wmts") {
        "wmts"
    } else if u.contains("service=wms") || u.contains("/wms") {
        "wms"
    } else if u.contains("service=wfs") || u.contains("/wfs") {
        "wfs"
    } else if u.contains("service=wcs") || u.contains("/wcs") {
        "wcs"
    } else if u.contains("/rest/services/") {
        "arcgis"
    } else {
        "web"
    }
}

/// The first line of an attribute or text that repeats itself across languages (geocat's).
fn first_line(s: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(500).collect()
}

/// Reads a GetRecords answer fetched from `url`.
pub fn parse_records(url: &str, body: &[u8]) -> Result<RecordPage, String> {
    let text = std::str::from_utf8(body).map_err(|_| "the catalogue's answer isn't UTF-8 text".to_string())?;
    let doc = xml(text, 2_000_000).map_err(|e| format!("the catalogue's XML: {e}"))?;
    let root = doc.root_element();
    match local(&root) {
        "GetRecordsResponse" => {}
        "ServiceExceptionReport" | "ExceptionReport" => {
            return Err(format!("the catalogue answered with an error: {}", text_of(&root).chars().take(300).collect::<String>()));
        }
        other => return Err(format!("not a catalogue's (CSW) answer (<{other}>)")),
    }
    let mut page = RecordPage::default();
    if let Some(r) = root.descendants().find(|n| n.is_element() && local(n) == "SearchResults") {
        let num = |k: &str| r.attribute(k).and_then(|v| v.trim().parse::<u64>().ok());
        page.matched = num("numberOfRecordsMatched").unwrap_or(0);
        page.next = num("nextRecord").filter(|n| *n > 0 && *n <= page.matched);
    }
    for rec in root
        .descendants()
        .filter(|n| n.is_element() && matches!(local(n), "Record" | "SummaryRecord" | "BriefRecord"))
        .take(RECORDS_PAGE as usize * 5)
    {
        let field = |k: &str| child(&rec, k).map(|c| first_line(&text_of(&c))).filter(|t| !t.is_empty());
        let id = field("identifier").unwrap_or_default();
        let title = field("title").unwrap_or_else(|| id.clone());
        let summary: String =
            child(&rec, "abstract").or_else(|| child(&rec, "description")).map(|c| first_line(&text_of(&c))).unwrap_or_default();
        // Bounds: a WGS84 box (longitude first), else a box in EPSG:4326 (latitude first when
        // given by URN, as the standard has it), else CRS84.
        let bbox = child(&rec, "WGS84BoundingBox")
            .and_then(|b| {
                let lo: Vec<f64> = child_text(&b, "LowerCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                let hi: Vec<f64> = child_text(&b, "UpperCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                lonlat_box(*lo.first()?, *lo.get(1)?, *hi.first()?, *hi.get(1)?)
            })
            .or_else(|| {
                let b = child(&rec, "BoundingBox")?;
                let crs = b.attribute("crs").unwrap_or("").to_ascii_lowercase();
                let lo: Vec<f64> = child_text(&b, "LowerCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                let hi: Vec<f64> = child_text(&b, "UpperCorner")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                let (a, b0, c, d) = (*lo.first()?, *lo.get(1)?, *hi.first()?, *hi.get(1)?);
                if crs.contains("urn:") && crs.contains("4326") { lonlat_box(b0, a, d, c) } else { lonlat_box(a, b0, c, d) }
            });
        let mut links: Vec<RecordLink> = Vec::new();
        for l in rec.children().filter(|c| c.is_element() && matches!(local(c), "URI" | "references")).take(50) {
            let Some(href) = text_of(&l).split_whitespace().next().and_then(|u| resolve(url, u)) else { continue };
            if href.len() > 2000 || links.iter().any(|k| k.url == href) {
                continue;
            }
            let protocol = l.attribute("protocol").or_else(|| l.attribute("scheme")).map(first_line).unwrap_or_default();
            let name = l.attribute("name").map(first_line).filter(|n| !n.is_empty());
            links.push(RecordLink { kind: link_kind(&protocol, &href), url: href, name, protocol });
        }
        page.records.push(Record { id, title, summary, bbox, links });
    }
    Ok(page)
}

// ---------------------------------------------------------------------------------------------
// ArcGIS REST

/// Web Mercator metres to longitude/latitude.
fn merc_to_lonlat(x: f64, y: f64) -> (f64, f64) {
    const R: f64 = 6_378_137.0;
    ((x / R).to_degrees(), (2.0 * (y / R).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees())
}

/// An ArcGIS extent (`{xmin, ymin, xmax, ymax, spatialReference}`) in longitude/latitude, for
/// WGS 84 and Web Mercator extents.
fn arcgis_extent(e: &serde_json::Value) -> Option<[f64; 4]> {
    let f = |k: &str| e.get(k).and_then(|v| v.as_f64());
    let (x0, y0, x1, y1) = (f("xmin")?, f("ymin")?, f("xmax")?, f("ymax")?);
    let wkid = e.pointer("/spatialReference/latestWkid").or_else(|| e.pointer("/spatialReference/wkid")).and_then(|w| w.as_i64())?;
    match wkid {
        4326 => lonlat_box(x0, y0, x1, y1),
        3857 | 102100 | 102113 | 900913 => {
            let (w, s) = merc_to_lonlat(x0, y0);
            let (e, n) = merc_to_lonlat(x1, y1);
            lonlat_box(w.max(-180.0), s.max(-85.06), e.min(180.0), n.min(85.06))
        }
        _ => None,
    }
}

/// Whether a cached map service's tiles are Web Mercator's own pyramid (zoom 0 at 156543 m/px).
fn arcgis_mercator_cache(v: &serde_json::Value) -> bool {
    let Some(ti) = v.get("tileInfo") else { return false };
    let wkid = ti.pointer("/spatialReference/latestWkid").or_else(|| ti.pointer("/spatialReference/wkid")).and_then(|w| w.as_i64());
    let first = ti.pointer("/lods/0/resolution").and_then(|r| r.as_f64());
    let level0 = ti.pointer("/lods/0/level").and_then(|l| l.as_i64());
    let rows = ti.get("rows").and_then(|r| r.as_i64()).unwrap_or(256);
    matches!(wkid, Some(3857 | 102100 | 102113))
        && rows == 256
        && level0 == Some(0)
        && first.is_some_and(|r| (r - 156_543.033_928_041).abs() < 0.01)
}

fn arcgis(url: &str, v: &serde_json::Value) -> Result<Service, String> {
    let b = base(url).trim_end_matches('/').to_string();
    let lower = b.to_ascii_lowercase();
    // A name, never the service's description (often a paragraph of HTML). "Layers" and "Map"
    // are ArcGIS's defaults, which say nothing: the service's own name in the address does.
    let title = ["/mapName", "/name", "/documentInfo/Title"]
        .iter()
        .filter_map(|k| v.pointer(k).and_then(|t| t.as_str()).map(str::trim))
        .find(|t| !t.is_empty() && !["Layers", "Map"].contains(t) && !t.contains(['<', '\n']) && t.chars().count() <= 200)
        .unwrap_or_else(|| b.rsplit('/').nth(1).filter(|n| !n.is_empty()).unwrap_or("ArcGIS"))
        .to_string();
    let full = v.get("fullExtent").or_else(|| v.get("extent")).and_then(arcgis_extent);
    let mut service = Service { kind: "ArcGIS".into(), title: title.clone(), ..Service::default() };
    // A directory of services: its folders and the services RustGIS can add, each to open. Known
    // by its answer (services or folders, and no layers or capabilities of a service's own), not
    // its address: a folder may be named like a service.
    let service_answer = ["layers", "capabilities", "serviceDescription", "tileInfo"].iter().any(|k| v.get(k).is_some());
    if (v.get("services").is_some() || v.get("folders").is_some()) && !service_answer {
        return Ok(arcgis_directory(&b, v));
    }
    // Feature layers (FeatureServer, and a MapServer's layers): queried as GeoJSON, so only from a
    // service that allows queries: "Query", or "Data" on a MapServer (a cached basemap's
    // capabilities are often just "Map,Tilemap").
    let features = |service: &mut Service| {
        let queries = v
            .get("capabilities")
            .and_then(|c| c.as_str())
            .is_none_or(|c| c.split(',').any(|k| ["query", "data"].iter().any(|q| k.trim().eq_ignore_ascii_case(q))));
        if !queries {
            if v.get("layers").and_then(|l| l.as_array()).is_some_and(|a| !a.is_empty()) {
                service.notes.push("the service doesn't allow queries, so its layers' features can't be read".into());
            }
            return;
        }
        for l in v.get("layers").and_then(|l| l.as_array()).into_iter().flatten().take(MAX_LISTED_LAYERS) {
            let (Some(id), Some(name)) = (l.get("id").and_then(|i| i.as_i64()), l.get("name").and_then(|n| n.as_str())) else { continue };
            // Group layers have sub-layers and no features of their own.
            if l.get("subLayerIds").is_some_and(|s| s.as_array().is_some_and(|a| !a.is_empty())) {
                continue;
            }
            let url = format!("{b}/{id}/query?where=1%3D1&outFields=*&returnGeometry=true&outSR=4326&f=geojson");
            service.layers.push(ServiceLayer {
                zooms: None,
                name: id.to_string(),
                title: name.to_string(),
                offer: Offer::Features { url },
                bbox: full,
                tilesets: None,
                time: None,
                feature_info: None,
                legends: Vec::new(),
            });
        }
    };
    if lower.ends_with("/featureserver") {
        service.kind = "ArcGIS FeatureServer".into();
        features(&mut service);
    } else if lower.ends_with("/mapserver") {
        service.kind = "ArcGIS MapServer".into();
        // The whole map: its cache when it is Web Mercator's pyramid, else exported images.
        let template = if v.get("singleFusedMapCache").and_then(|c| c.as_bool()) == Some(true) && arcgis_mercator_cache(v) {
            format!("{b}/tile/{{z}}/{{y}}/{{x}}")
        } else {
            format!("{b}/export?bbox={{bbox-epsg-3857}}&bboxSR=3857&imageSR=3857&size=256,256&format=png32&transparent=true&f=image")
        };
        service.layers.push(ServiceLayer {
            zooms: None,
            name: "map".into(),
            title: format!("{title} (map)"),
            offer: Offer::Tiles { template },
            bbox: full,
            tilesets: None,
            time: None,
            feature_info: None,
            legends: Vec::new(),
        });
        features(&mut service);
        // A dynamic map's layers, each drawn alone (`layers=show:<id>`): a national layer is
        // seen whole this way, where its features would stop at the server's record limit.
        if !template_is_cache(&service) {
            for l in v.get("layers").and_then(|l| l.as_array()).into_iter().flatten().take(MAX_LISTED_LAYERS) {
                let (Some(id), Some(name)) = (l.get("id").and_then(|i| i.as_i64()), l.get("name").and_then(|n| n.as_str())) else {
                    continue;
                };
                let template = format!(
                    "{b}/export?bbox={{bbox-epsg-3857}}&bboxSR=3857&imageSR=3857&size=256,256&format=png32&transparent=true&f=image&layers=show:{id}"
                );
                service.layers.push(ServiceLayer {
                    zooms: None,
                    name: format!("map:{id}"),
                    title: format!("{name} (map)"),
                    offer: Offer::Tiles { template },
                    bbox: full,
                    tilesets: None,
                    time: None,
                    feature_info: None,
                    legends: Vec::new(),
                });
            }
        }
    } else if lower.ends_with("/imageserver") {
        service.kind = "ArcGIS ImageServer".into();
        let template = format!("{b}/exportImage?bbox={{bbox-epsg-3857}}&bboxSR=3857&imageSR=3857&size=256,256&format=png&f=image");
        service.layers.push(ServiceLayer {
            zooms: None,
            name: "image".into(),
            title: title.clone(),
            offer: Offer::Tiles { template },
            bbox: full,
            tilesets: None,
            time: None,
            feature_info: None,
            legends: Vec::new(),
        });
    } else if lower.ends_with("/vectortileserver") {
        service.kind = "ArcGIS VectorTileServer".into();
        let style = v
            .get("defaultStyles")
            .and_then(|s| s.as_str())
            .map(|s| format!("{b}/{}/root.json", s.trim_matches('/')))
            .unwrap_or_else(|| format!("{b}/resources/styles/root.json"));
        service.layers.push(ServiceLayer {
            zooms: None,
            name: "style".into(),
            title: title.clone(),
            offer: Offer::Style { url: style },
            bbox: full,
            tilesets: None,
            time: None,
            feature_info: None,
            legends: Vec::new(),
        });
    } else {
        return Err("an ArcGIS address, but not a FeatureServer, MapServer, ImageServer or VectorTileServer".into());
    }
    let left = v.get("layers").and_then(|l| l.as_array()).map_or(0, Vec::len).saturating_sub(MAX_LISTED_LAYERS);
    if left > 0 {
        note_past_read_limit(&mut service, left, "layers", MAX_LISTED_LAYERS);
    }
    // A feature layer is read past the server's record limit, page by page, where the layer
    // supports it ([`begin`]). A cut-off answer is pointed out when it lands.
    if let Some(max) = v.get("maxRecordCount").and_then(|m| m.as_i64())
        && service.layers.iter().any(|l| matches!(l.offer, Offer::Features { .. }))
    {
        service.notes.push(format!(
            "a feature layer is read up to {} features on this computer, {max} per request, where the server pages its layers",
            max_paged_features()
        ));
    }
    // Offers are constructed under the service. A suffix supplied by its document still has
    // to pass the raw-path check before it may receive the service's token.
    if let Some(token) = query_value(url, "token").filter(|t| !t.is_empty()) {
        for l in &mut service.layers {
            match &mut l.offer {
                Offer::Tiles { template: u }
                | Offer::Features { url: u }
                | Offer::Style { url: u }
                | Offer::VectorTiles { template: u, .. }
                | Offer::Service { url: u }
                | Offer::Coverage(Coverage { endpoint: u, .. }) => {
                    if crate::address::Scope::of(crate::login_address::inner(url))
                        .is_some_and(|scope| scope.covers_below(crate::login_address::inner(u)))
                    {
                        *u = with_query(u, &format!("token={token}"));
                    } else {
                        service.withheld_addresses = service.withheld_addresses.saturating_add(1);
                    }
                }
            }
        }
    }
    Ok(service)
}

/// Whether a MapServer's whole map was offered as its tile cache (which draws no layer alone).
fn template_is_cache(service: &Service) -> bool {
    service.layers.first().is_some_and(|l| matches!(&l.offer, Offer::Tiles { template } if template.contains("/tile/{z}/")))
}

/// The ArcGIS service types a directory lists that RustGIS can add.
const ARCGIS_TYPES: [&str; 4] = ["MapServer", "FeatureServer", "ImageServer", "VectorTileServer"];

/// A name an ArcGIS directory gives a folder or service, checked: it goes into addresses, so only
/// letters, digits, `_`, `-`, `.` and `/` between non-empty parts that aren't `.` or `..`.
fn arcgis_name(n: &str) -> Option<&str> {
    let ok = !n.is_empty()
        && n.len() <= 200
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/'))
        && n.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
    ok.then_some(n)
}

/// An ArcGIS services directory (`…/rest/services`, or a folder of it): its folders and the
/// services RustGIS can add, each offered to open. Services are named with their folder
/// (`Folder/Name`), so their addresses start at the directory's root, not at the folder.
fn arcgis_directory(b: &str, v: &serde_json::Value) -> Service {
    let lower = b.to_ascii_lowercase();
    let found = lower.find("/rest/services").and_then(|i| b.get(..i + "/rest/services".len()));
    // Without `/rest/services` (a proxy's own path) the address is taken as where the listing
    // is, so a folder's services (`Folder/Name`) go under it by their own names.
    let rooted = found.is_some();
    let root = found.unwrap_or(b);
    let here = b.get(root.len()..).unwrap_or("").trim_matches('/');
    let title = if here.is_empty() { "ArcGIS services".to_string() } else { here.to_string() };
    let mut service = Service { kind: "ArcGIS Services Directory".into(), title, ..Service::default() };
    let mut skipped = 0;
    // Names RustGIS won't put into an address (spaces, `?`, `..`): counted in a note.
    let mut unnamed = 0;
    // ArcGIS's own system folder holds geometry and printing tools, never maps.
    for f in v.get("folders").and_then(|f| f.as_array()).into_iter().flatten().take(MAX_LISTED_LAYERS) {
        let Some(name) = f.as_str().and_then(|n| {
            let ok = arcgis_name(n);
            unnamed += usize::from(ok.is_none());
            ok
        }) else {
            continue;
        };
        if name.eq_ignore_ascii_case("Utilities") {
            continue;
        }
        let path = if here.is_empty() { name.to_string() } else { format!("{here}/{name}") };
        service.layers.push(ServiceLayer {
            zooms: None,
            name: format!("{path}/"),
            title: format!("{name}/"),
            offer: Offer::Service { url: format!("{root}/{path}") },
            bbox: None,
            tilesets: None,
            time: None,
            feature_info: None,
            legends: Vec::new(),
        });
    }
    for s in v.get("services").and_then(|s| s.as_array()).into_iter().flatten().take(MAX_LISTED_LAYERS) {
        let Some(kind) = s.get("type").and_then(|t| t.as_str()) else {
            skipped += 1;
            continue;
        };
        let Some(name) = s.get("name").and_then(|n| n.as_str()).and_then(arcgis_name) else {
            unnamed += 1;
            continue;
        };
        let Some(kind) = ARCGIS_TYPES.iter().find(|t| t.eq_ignore_ascii_case(kind)) else {
            skipped += 1;
            continue;
        };
        let short = name.rsplit('/').next().unwrap_or(name);
        service.layers.push(ServiceLayer {
            zooms: None,
            name: format!("{name}/{kind}"),
            title: format!("{short} ({kind})"),
            offer: Offer::Service { url: format!("{root}/{}/{kind}", if rooted { name } else { short }) },
            bbox: None,
            tilesets: None,
            time: None,
            feature_info: None,
            legends: Vec::new(),
        });
    }
    if skipped > 0 {
        service.notes.push(format!("{skipped} services of other or unknown types (geoprocessing, geometry, …) are left out"));
    }
    if unnamed > 0 {
        service.notes.push(format!("{unnamed} folders or services with names RustGIS can't put in an address are left out"));
    }
    for (key, what) in [("folders", "folders"), ("services", "services")] {
        let left = v.get(key).and_then(|l| l.as_array()).map_or(0, Vec::len).saturating_sub(MAX_LISTED_LAYERS);
        if left > 0 {
            note_past_read_limit(&mut service, left, what, MAX_LISTED_LAYERS);
        }
    }
    service
}

// ---------------------------------------------------------------------------------------------
// OGC API Features

fn ogc_api(url: &str, v: &serde_json::Value) -> Result<Service, String> {
    let collections = v.get("collections").and_then(|c| c.as_array());
    let Some(collections) = collections else {
        // A landing page can offer both dataset tiles and a collections document to discover.
        let title = v.get("title").and_then(|t| t.as_str()).unwrap_or_default().to_string();
        let mut service = Service { kind: "OGC API Tiles".into(), title: title.clone(), ..Service::default() };
        let links = v.get("links").and_then(|l| l.as_array()).cloned().unwrap_or_default();
        if let Some((next, withheld)) = collections_link(url, &links) {
            service.collections_url = Some(next);
            service.withheld_addresses = u32::from(withheld);
        }
        let name = if title.is_empty() { "tiles".to_string() } else { title };
        // The dataset's layers aren't listed here, so its vector tiles can't be styled: map tiles only.
        if let Some((t, list, withheld)) = tileset(url, &links, TileRel::Map) {
            service.withheld_addresses = service.withheld_addresses.saturating_add(u32::from(withheld));
            service.layers.push(ServiceLayer {
                zooms: None,
                name: "map".into(),
                title: format!("{name} (map tiles)"),
                offer: Offer::Tiles { template: t },
                bbox: None,
                tilesets: Some(list),
                time: None,
                feature_info: None,
                legends: Vec::new(),
            });
        }
        if !service.layers.is_empty() || service.collections_url.is_some() {
            if service.layers.is_empty() {
                service.kind = "OGC API Features".into();
            }
            return Ok(service);
        }
        return Err(if links.iter().any(is_data_link) {
            "the OGC API landing page has no usable JSON collections link".into()
        } else {
            "not an OGC API collections document (no \"collections\")".into()
        });
    };
    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or_default().to_string();
    let mut service = Service { kind: "OGC API Features".into(), title, ..Service::default() };
    let root = base(url).trim_end_matches('/').to_string();
    let mut unreadable = 0;
    let mut tiled = 0;
    // Collections whose items are 3D content, not features, by what it is and whether their tiles
    // are listed.
    let mut three_d: Vec<((&str, bool), Vec<&str>)> = Vec::new();
    // A collection's layers copy the document's own address when their items or tiles links are
    // missing or relative, and nothing bounds that address: past the listing, reading stops once
    // the layers' addresses reach `MAX_COPIED_ADDRESS_BYTES`.
    let (mut copied, mut counted) = (0usize, 0);
    let mut read = collections.len().min(MAX_READ_LAYERS);
    for (i, c) in collections.iter().take(MAX_READ_LAYERS).enumerate() {
        copied = service.layers.get(counted..).unwrap_or_default().iter().map(address_bytes).fold(copied, usize::saturating_add);
        counted = service.layers.len();
        // A collection may make several layers: the list stops at a collection, with all its own.
        if i == MAX_LISTED_LAYERS {
            service.listed = Some(service.layers.len());
        }
        if i >= MAX_LISTED_LAYERS && copied > MAX_COPIED_ADDRESS_BYTES {
            read = i;
            break;
        }
        let Some(id) = c.get("id").and_then(|i| i.as_str()) else { continue };
        let links = c.get("links").and_then(|l| l.as_array()).cloned().unwrap_or_default();
        let title = c.get("title").and_then(|t| t.as_str()).unwrap_or(id).to_string();
        // Its extent in CRS84, the standard's default: a box in another system it names isn't
        // read as longitude/latitude (its features' axes are judged against it, `fix_axes`). Four
        // numbers, or six with heights.
        let lonlat = c.pointer("/extent/spatial/crs").and_then(|v| v.as_str()).is_none_or(is_crs84);
        let bbox = c.pointer("/extent/spatial/bbox/0").and_then(|b| b.as_array()).filter(|_| lonlat).and_then(|b| {
            let f = |i: usize| b.get(i).and_then(|v| v.as_f64());
            let (e, n) = match b.len() {
                4 => (2, 3),
                6 => (3, 4),
                _ => return None,
            };
            lonlat_box(f(0)?, f(1)?, f(e)?, f(n)?)
        });
        // OGC API Tiles: the collection's map and vector tiles in WebMercatorQuad. Its vector
        // tiles' layer is named after the collection, as the standard has it.
        let map = tileset(url, &links, TileRel::Map);
        let vector = tileset(url, &links, TileRel::Vector);
        if let Some((t, list, withheld)) = &map {
            service.withheld_addresses = service.withheld_addresses.saturating_add(u32::from(*withheld));
            service.layers.push(ServiceLayer {
                zooms: None,
                name: format!("{id} (map tiles)"),
                title: format!("{title} (map tiles)"),
                offer: Offer::Tiles { template: t.clone() },
                bbox,
                tilesets: Some(list.clone()),
                time: None,
                feature_info: None,
                legends: Vec::new(),
            });
        }
        if let Some((t, list, withheld)) = &vector {
            service.withheld_addresses = service.withheld_addresses.saturating_add(u32::from(*withheld));
            service.layers.push(ServiceLayer {
                zooms: None,
                name: format!("{id} (vector tiles)"),
                title: format!("{title} (vector tiles)"),
                offer: Offer::VectorTiles { template: t.clone(), layers: vec![id.chars().take(200).collect()] },
                bbox,
                tilesets: Some(list.clone()),
                time: None,
                feature_info: None,
                legends: Vec::new(),
            });
        }
        if map.is_some() || vector.is_some() {
            tiled += 1;
        }
        let json_items: Vec<&serde_json::Value> = links
            .iter()
            .filter(|l| l.get("rel").and_then(|r| r.as_str()) == Some("items"))
            .filter(|l| {
                items_3d(url, l).is_some() || l.get("type").and_then(|t| t.as_str()).is_none_or(|t| t.to_ascii_lowercase().contains("json"))
            })
            .collect();
        let link = json_items.iter().copied().find(|l| items_3d(url, l).is_none());
        let tiles = map.is_some() || vector.is_some();
        // Items that are only 3D content (PDOK links 3D Tiles and quantized meshes as items, for
        // 3D viewers) aren't offered, and the notes say so: the collection is left out, or listed
        // by its tiles alone.
        if let (None, Some(what)) = (link, json_items.first().and_then(|l| items_3d(url, l))) {
            match three_d.iter_mut().find(|(k, _)| *k == (what, tiles)) {
                Some((_, ids)) => ids.push(id),
                None => three_d.push(((what, tiles), vec![id])),
            }
            continue;
        }
        // A collection with tiles and no items link has no features to offer.
        if link.is_none() && tiles {
            continue;
        }
        let declared_geojson =
            link.and_then(|l| l.get("type").and_then(|t| t.as_str())).is_some_and(|t| t.to_ascii_lowercase().contains("geo+json"));
        // Without an items link: the standard path under the collections address. A relative
        // link is resolved against the document's address.
        let items = match link.and_then(|l| l.get("href").and_then(|h| h.as_str())) {
            Some(h) => match resolve(url, h) {
                Some(u) => u,
                None => {
                    unreadable += 1;
                    continue;
                }
            },
            None => format!("{root}/{}/items", encode(id)),
        };
        let items = if query_value(&items, "limit").is_some() { items } else { with_query(&items, &format!("limit={OGC_API_LIMIT}")) };
        // GeoJSON asked for unless the link says it is already.
        let items = if declared_geojson || query_value(&items, "f").is_some() { items } else { with_query(&items, "f=json") };
        service.layers.push(ServiceLayer {
            zooms: None,
            name: id.to_string(),
            title,
            offer: Offer::Features { url: items },
            bbox,
            tilesets: None,
            time: None,
            feature_info: None,
            legends: Vec::new(),
        });
    }
    if unreadable > 0 {
        service.notes.push(format!("{unreadable} collections link to their items with an address RustGIS doesn't fetch and are left out"));
    }
    for ((what, tiles), ids) in &three_d {
        let mut named = ids.iter().take(20).map(|i| format!("'{i}'")).collect::<Vec<_>>().join(", ");
        if ids.len() > 20 {
            named.push_str(&format!(", and {} more", ids.len() - 20));
        }
        let one = ids.len() == 1;
        let (noun, verb) = if one { ("collection", "offers") } else { ("collections", "offer") };
        service.notes.push(match (tiles, one) {
            (false, true) => format!("{noun} {named} {verb} {what}, {LEFT_OUT_3D}, so it is left out"),
            (false, false) => format!("{noun} {named} {verb} {what}, {LEFT_OUT_3D}, so they are left out"),
            (true, true) => format!("{noun} {named} also {verb} {what}, {LEFT_OUT_3D}; its tiles are listed"),
            (true, false) => format!("{noun} {named} also {verb} {what}, {LEFT_OUT_3D}; their tiles are listed"),
        });
    }
    if collections.len() > read {
        note_past_read_limit(&mut service, collections.len() - read, "collections", read);
        note_copied_addresses(&mut service, copied.div_ceil(read.max(1)), read);
    }
    service.incomplete = unreadable > 0 || collections.len() > MAX_LISTED_LAYERS;
    if service.layers.iter().any(|l| matches!(l.offer, Offer::Features { .. })) {
        service.notes.push(format!("a collection is read up to {} features on this computer, page by page", max_paged_features()));
    }
    if tiled > 0 {
        service.kind = if service.layers.iter().any(|l| matches!(l.offer, Offer::Features { .. })) {
            "OGC API Features and Tiles".into()
        } else {
            "OGC API Tiles".into()
        };
    }
    Ok(service)
}

/// Which tiles of an OGC API: map (pictures) or vector (Mapbox Vector Tiles).
#[derive(Clone, Copy, PartialEq)]
enum TileRel {
    Map,
    Vector,
}

/// The tiles of a dataset or collection in WebMercatorQuad, from its `tilesets-map` or
/// `tilesets-vector` link: the template at the usual path under the tileset list
/// (`…/WebMercatorQuad/{tileMatrix}/{tileRow}/{tileCol}`), and the list's address (JSON), whose
/// WebMercatorQuad tileset advertises the template [`resolve_tiles`] reads (the usual path is what
/// is used when that can't be read). The constructed template keeps the link's query except
/// `f` only under its checked source path; the third result counts a withheld destination.
fn tileset(from: &str, links: &[serde_json::Value], which: TileRel) -> Option<(String, String, bool)> {
    let suffix = match which {
        TileRel::Map => "tilesets-map",
        TileRel::Vector => "tilesets-vector",
    };
    let link = links
        .iter()
        .take(200)
        .filter(|l| l.get("rel").and_then(|r| r.as_str()).is_some_and(|r| r == suffix || r.ends_with(&format!("/{suffix}"))));
    // The JSON link first (or one that names no type), else any.
    let chosen: Vec<&serde_json::Value> = link.collect();
    let l = chosen.iter().find(|l| l.get("type").and_then(|t| t.as_str()).is_none_or(|t| t.contains("json"))).or_else(|| chosen.first())?;
    let href = resolve(from, l.get("href")?.as_str()?)?;
    let at = base(&href).trim_end_matches('/');
    if at.contains(['{', '}']) || at.len() > 2000 {
        return None;
    }
    // Map tiles come in the server's own image format (PNG or JPEG, which it picks); vector
    // tiles are asked as MVT.
    let template = match which {
        TileRel::Map => format!("{at}/WebMercatorQuad/{{z}}/{{y}}/{{x}}"),
        TileRel::Vector => format!("{at}/WebMercatorQuad/{{z}}/{{y}}/{{x}}?f=mvt"),
    };
    let list = if query_value(&href, "f").is_some() { href.clone() } else { with_query(&href, "f=json") };
    let inherited = crate::address::inherit_query(
        crate::login_address::inner(&href),
        crate::login_address::inner(&template),
        crate::address::Relation::Constructed,
        &["f"],
    );
    Some((crate::login_address::inherit(&template, &inherited.url), list, inherited.withheld))
}

/// Notice template shared by native discovery and the translated asynchronous UI resolver.
pub const QUERY_WITHHELD_NOTICE: &str = "{n} address(es) the service listed didn't get its keys or other settings: RustGIS sends those only to addresses it builds itself from the one given; add such an address with its own key if it needs one";

/// What an OGC API tiles document says on the way to a tile template: the template, or the
/// address of the document to read next (a tileset list's WebMercatorQuad tileset).
#[derive(Clone, Debug, PartialEq)]
pub enum TilesStep {
    Template(String),
    Next(String),
}

/// One discovery result and the number of destinations that lost query settings. Callers must
/// report this count, including when a later fetch fails and falls back to a constructed URL.
#[derive(Clone, Debug, PartialEq)]
pub struct TilesResolution {
    pub step: TilesStep,
    pub withheld_addresses: u32,
}

/// Applies the advertised-link policy; the document cannot claim a constructed relationship.
fn advertised_tiles(list: &str, to: &str, next: bool) -> TilesResolution {
    let inherited = crate::address::inherit_query(
        crate::login_address::inner(list),
        crate::login_address::inner(to),
        crate::address::Relation::Advertised,
        &["f"],
    );
    // Preserve only the destination's binding: a foreign document cannot recover a login
    // merely by linking back to the original query source's origin.
    let url = crate::login_address::inherit(to, &inherited.url);
    let step = if next {
        TilesStep::Next(if query_value(&url, "f").is_some() { url } else { with_query(&url, "f=json") })
    } else {
        TilesStep::Template(url)
    };
    TilesResolution { step, withheld_addresses: u32::from(inherited.withheld) }
}

/// One step of reading an OGC API tile template: `body` is the tileset list (or a tileset)
/// fetched from `doc`; `list` is the query source. Advertised links receive SAFE_OPTIONS only;
/// withheld settings are counted for the caller's notice. `None` when
/// the document names no WebMercatorQuad tileset or tile template.
pub fn resolve_tiles(list: &str, doc: &str, body: &[u8], vector: bool) -> Option<TilesResolution> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let links = |v: &serde_json::Value| v.get("links").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    let Some(sets) = v.get("tilesets").and_then(|t| t.as_array()) else {
        // A tileset: its tile template.
        return item_template(doc, &links(&v), vector).map(|t| advertised_tiles(list, &t, false));
    };
    let mercator = |t: &serde_json::Value| {
        let tiling = links(t);
        let named: Vec<String> = ["tileMatrixSetURI", "tileMatrixSetId", "tileMatrixSet", "id"]
            .iter()
            .filter_map(|k| t.get(*k).and_then(|x| x.as_str()))
            .chain(
                tiling
                    .iter()
                    .filter(|l| l.get("rel").and_then(|r| r.as_str()).is_some_and(|r| r.contains("tiling-scheme")))
                    .filter_map(|l| l.get("href").and_then(|h| h.as_str())),
            )
            .map(|n| base(n).trim_end_matches('/').to_ascii_lowercase())
            .collect();
        named.iter().any(|n| n.ends_with("webmercatorquad") || n.ends_with("googlemapscompatible"))
    };
    let kind_fits = |t: &serde_json::Value| match t.get("dataType").and_then(|d| d.as_str()) {
        Some(d) => (d == "vector") == vector,
        None => true,
    };
    let t = sets.iter().take(200).find(|t| mercator(t) && kind_fits(t))?;
    let ls = links(t);
    if let Some(template) = item_template(doc, &ls, vector) {
        return Some(advertised_tiles(list, &template, false));
    }
    let next = ls
        .iter()
        .find(|l| l.get("rel").and_then(|r| r.as_str()).is_some_and(|r| r == "self" || r.ends_with("/tileset")))
        .and_then(|l| resolve(doc, l.get("href")?.as_str()?))?;
    Some(advertised_tiles(list, &next, true))
}

/// A tileset's advertised tile template (its `item` link, `{tileMatrix}/{tileRow}/{tileCol}`) as
/// `{z}/{y}/{x}`, in the tiles' format: MVT for vector tiles, an image (PNG first) for map tiles.
fn item_template(from: &str, links: &[serde_json::Value], vector: bool) -> Option<String> {
    let items: Vec<&serde_json::Value> = links
        .iter()
        .take(200)
        .filter(|l| l.get("rel").and_then(|r| r.as_str()).is_some_and(|r| r == "item" || r.ends_with("/item")))
        .filter(|l| l.get("href").and_then(|h| h.as_str()).is_some_and(|h| h.contains("{tileMatrix}")))
        .collect();
    let ty = |l: &&&serde_json::Value| l.get("type").and_then(|t| t.as_str()).unwrap_or("").to_ascii_lowercase();
    let pick = if vector {
        items.iter().find(|l| {
            let t = ty(l);
            t.contains("mapbox-vector-tile") || t.contains("mvt") || t.contains("protobuf")
        })
    } else {
        items.iter().find(|l| ty(l) == "image/png").or_else(|| items.iter().find(|l| ty(l).starts_with("image/")))
    }
    .or_else(|| items.first())?;
    let href = pick.get("href")?.as_str()?;
    if href.len() > 2000 {
        return None;
    }
    let t = href.replace("{tileMatrix}", "{z}").replace("{tileRow}", "{y}").replace("{tileCol}", "{x}");
    // Only the three coordinates are filled in.
    let rest = t.replace("{z}", "").replace("{y}", "").replace("{x}", "");
    if rest.contains(['{', '}']) {
        return None;
    }
    let t = resolve(from, &t)?;
    is_web(&t).then_some(t)
}

// ---------------------------------------------------------------------------------------------
// Feature answers

/// How much of an answer's head and tail is searched for its paging members (they sit at the
/// start or the end of a feature collection).
const EDGE: usize = 64 * 1024;

/// The head and tail of an answer, as text.
fn edges(bytes: &[u8]) -> (String, String) {
    let head = bytes.get(..bytes.len().min(EDGE)).unwrap_or(bytes);
    let tail = bytes.get(bytes.len().saturating_sub(EDGE)..).unwrap_or(bytes);
    (String::from_utf8_lossy(head).into_owned(), String::from_utf8_lossy(tail).into_owned())
}

/// The number after a JSON key (`"numberMatched": 1234`), in `text`.
fn number_after(text: &str, key: &str) -> Option<u64> {
    let at = text.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = text.get(at..)?.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whether `text` holds `"key": true`.
fn is_true(text: &str, key: &str) -> bool {
    text.match_indices(&format!("\"{key}\"")).any(|(i, m)| {
        text.get(i + m.len()..).and_then(|r| r.trim_start().strip_prefix(':')).is_some_and(|r| r.trim_start().starts_with("true"))
    })
}

/// A note when a service's GeoJSON answer is only part of what it holds: an ArcGIS layer past its
/// record limit (`exceededTransferLimit`), or an OGC API or WFS 2.0 page (`numberMatched` above
/// `numberReturned`, or a `next` link).
pub fn partial_note(bytes: &[u8]) -> Option<String> {
    let (head, tail) = edges(bytes);
    if is_true(&head, "exceededTransferLimit") || is_true(&tail, "exceededTransferLimit") {
        return Some("the server sent its first features only (its record limit); the rest aren't read".into());
    }
    let n = |k: &str| number_after(&head, k).or_else(|| number_after(&tail, k));
    if let (Some(matched), Some(returned)) = (n("numberMatched"), n("numberReturned"))
        && matched > returned
    {
        return Some(format!("the server sent {returned} of its {matched} features (one page); the rest aren't read"));
    }
    let next = |t: &str| t.contains("\"rel\":\"next\"") || t.contains("\"rel\": \"next\"");
    (next(&head) || next(&tail)).then(|| "the server sent one page of its features; the rest aren't read".into())
}

/// The fewest features [`max_paged_features`] allows, and its limit in the browser.
pub const PAGED_FEATURES_FLOOR: usize = 100_000;
/// The most [`max_paged_features`] allows, however much memory the computer has.
pub const PAGED_FEATURES_CEILING: usize = 1_000_000;

/// Most features read from a collection that comes in pages: on the desktop, as many as a quarter
/// of the computer's memory holds at 20 KB each (about 100,000 for every 8 GB, the pages read
/// included), from [`PAGED_FEATURES_FLOOR`] up to [`PAGED_FEATURES_CEILING`] (#1318).
pub fn max_paged_features() -> usize {
    crate::memory::scaled(crate::memory::physical(), PAGED_FEATURES_FLOOR, PAGED_FEATURES_CEILING, 20 << 10)
}

/// The most bytes of why a layer's protocol buffer pages couldn't be read that its note says.
const MAX_REASON: usize = 200;

/// The note of a layer read in GeoJSON because its protocol buffer pages couldn't be read (`why`),
/// before the reading's own `note`.
fn read_in_geojson(why: &str, note: Option<String>) -> String {
    let said = format!("its protocol buffer pages couldn't be read ({why}), so it is read in GeoJSON");
    match note {
        Some(note) => format!("{said}; {note}"),
        None => said,
    }
}

/// The least memory [`max_decoded_bytes`] allows, and its limit in the browser.
pub const DECODED_FLOOR: usize = 512 << 20;
/// The most memory [`max_decoded_bytes`] allows, however much the computer has.
pub const DECODED_CEILING: usize = 3 << 30;

/// Most memory, in bytes, an ArcGIS layer's protocol buffer pages may take once read: on the
/// desktop a quarter of the computer's memory, from [`DECODED_FLOOR`] up to [`DECODED_CEILING`].
/// The download budget doesn't bound it: a vertex takes 1 to 3 bytes in a page and 16 read.
pub fn max_decoded_bytes() -> usize {
    crate::memory::scaled(crate::memory::physical(), DECODED_FLOOR, DECODED_CEILING, 1)
}

/// A feature collection page's JSON `next` link, resolved against the page's address.
fn next_page(url: &str, page: &serde_json::Value) -> Option<String> {
    let link = page.get("links")?.as_array()?.iter().find(|l| {
        l.get("rel").and_then(|r| r.as_str()) == Some("next") && l.get("type").and_then(|t| t.as_str()).is_none_or(|t| t.contains("json"))
    })?;
    resolve(url, link.get("href")?.as_str()?)
}

/// Whether an ArcGIS answer says it stopped at the server's record limit (`exceededTransferLimit`,
/// a member of the collection or, in GeoJSON, of its `properties`).
fn exceeded(page: &serde_json::Value) -> bool {
    [page.get("exceededTransferLimit"), page.pointer("/properties/exceededTransferLimit")]
        .iter()
        .any(|v| v.and_then(|v| v.as_bool()) == Some(true))
}

/// Whether an address is an ArcGIS layer's query (`…/FeatureServer/0/query`, `…/MapServer/3/query`).
fn is_arcgis_query(url: &str) -> bool {
    let b = base(url).trim_end_matches('/').to_ascii_lowercase();
    b.ends_with("/query") && (b.contains("/featureserver/") || b.contains("/mapserver/"))
}

/// How an ArcGIS layer is read past its record limit: pages of `size` features, in the order of
/// its object ids (`order`) so that no feature is skipped or read twice.
#[derive(Debug, PartialEq)]
struct ArcgisPaging {
    size: Option<usize>,
    order: Option<String>,
}

/// How a layer's description (`…/FeatureServer/0?f=json`) lets it be read in pages: `None` unless it
/// says it supports them (`advancedQueryCapabilities.supportsPagination`, from ArcGIS Server 10.3).
/// Its `maxRecordCount` is the page size when it is a count RustGIS would ask for; its object id
/// field, the order, when it is a plain field name (it goes into the address).
fn arcgis_paging(layer: &serde_json::Value) -> Option<ArcgisPaging> {
    if layer.pointer("/advancedQueryCapabilities/supportsPagination").and_then(|p| p.as_bool()) != Some(true) {
        return None;
    }
    let size = layer
        .get("maxRecordCount")
        .and_then(|m| m.as_u64())
        .and_then(|m| usize::try_from(m).ok())
        .filter(|m| (1..=max_paged_features()).contains(m));
    let oid_field = || {
        layer
            .get("fields")?
            .as_array()?
            .iter()
            .find(|f| f.get("type").and_then(|t| t.as_str()) == Some("esriFieldTypeOID"))?
            .get("name")?
            .as_str()
    };
    let order = layer
        .get("objectIdField")
        .and_then(|o| o.as_str())
        .or_else(oid_field)
        .filter(|n| !n.is_empty() && n.len() <= 128 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .map(str::to_string);
    Some(ArcgisPaging { size, order })
}

/// Whether a layer's description lists protocol buffers among the formats its queries answer in
/// (`supportedQueryFormats`, such as `"JSON, geoJSON, PBF"`).
fn supports_pbf(layer: &serde_json::Value) -> bool {
    layer.get("supportedQueryFormats").and_then(|f| f.as_str()).is_some_and(|f| f.split(',').any(|f| f.trim().eq_ignore_ascii_case("pbf")))
}

/// An ArcGIS query asking for its answer as `format` (`f`, in any case), in place of the format
/// it named; its fragment kept, as [`with_query`] keeps it.
fn with_format(query: &str, format: &str) -> String {
    let (url, fragment) = query.split_once('#').map_or((query, None), |(u, f)| (u, Some(f)));
    let (base, q) = url.split_once('?').unwrap_or((url, ""));
    let kept: Vec<&str> =
        q.split('&').filter(|kv| !kv.is_empty() && !kv.split('=').next().is_some_and(|k| k.eq_ignore_ascii_case("f"))).collect();
    let asked = with_query(&with_query(base, &kept.join("&")), &format!("f={format}"));
    if let Some(f) = fragment { format!("{asked}#{f}") } else { asked }
}

/// The description of the layer an ArcGIS query reads (`…/FeatureServer/0?f=json`), with the
/// query's token.
fn arcgis_layer_url(query: &str) -> String {
    if let Ok(Some(bound)) = crate::login_address::parse(query) {
        return crate::login_address::inherit(query, &arcgis_layer_url(bound.url));
    }
    let b = base(query).trim_end_matches('/');
    let layer = b.get(..b.len().saturating_sub("/query".len())).unwrap_or(b);
    match query_value(query, "token").filter(|t| !t.is_empty()) {
        Some(token) => format!("{layer}?f=json&token={token}"),
        None => format!("{layer}?f=json"),
    }
}

/// The page of an ArcGIS query that starts after `offset` features (`resultOffset`), `size` long.
fn arcgis_page_url(query: &str, offset: usize, size: usize, order: Option<&str>) -> String {
    let url = with_query(query, &format!("resultOffset={offset}&resultRecordCount={size}"));
    match order {
        Some(o) if query_value(&url, "orderByFields").is_none() => with_query(&url, &format!("orderByFields={o}")),
        _ => url,
    }
}

/// Whether a request can use WFS 2.0 offset paging (OGC 09-025r2 §7.6.3).
/// Explicit windows and hits requests are left as the caller asked for them.
fn is_wfs_query(url: &str) -> bool {
    let is = |key, value: &str| query_value(url, key).is_some_and(|v| v.eq_ignore_ascii_case(value));
    is("SERVICE", "WFS")
        && is("REQUEST", "GetFeature")
        && query_value(url, "VERSION").is_some_and(|v| v.starts_with("2.0"))
        && query_value(url, "STARTINDEX").is_none()
        && query_value(url, "COUNT").is_none_or(|c| c.trim().parse::<usize>().ok() == Some(FEATURE_LIMIT))
        && !is("RESULTTYPE", "hits")
}

/// Whether a feature request uses legacy WFS, whose reported totals can expose a short read.
fn is_legacy_wfs_query(url: &str) -> bool {
    [("SERVICE", "WFS"), ("REQUEST", "GetFeature")].iter().all(|(k, v)| query_value(url, k).is_some_and(|s| s.eq_ignore_ascii_case(v)))
        && query_value(url, "VERSION").is_some_and(|v| v.starts_with("1."))
}

/// Replaces WFS presentation parameters, retaining the query, filters and authentication.
/// Hits use the default XML format; feature pages retain the requested GeoJSON format.
fn wfs_page_url(url: &str, offset: Option<usize>, size: usize) -> String {
    let kept = url
        .split('#')
        .next()
        .and_then(|u| u.split_once('?'))
        .map(|(_, q)| q)
        .unwrap_or_default()
        .split('&')
        .filter(|kv| {
            let key = kv.split_once('=').map(|(k, _)| k).unwrap_or(kv);
            !["STARTINDEX", "COUNT", "RESULTTYPE"].iter().any(|k| key.eq_ignore_ascii_case(k))
                && (offset.is_some() || !key.eq_ignore_ascii_case("OUTPUTFORMAT"))
        })
        .collect::<Vec<_>>()
        .join("&");
    let url = with_query(base(url), &kept);
    match offset {
        Some(offset) => with_query(&url, &format!("STARTINDEX={offset}&COUNT={size}")),
        None => with_query(&url, "RESULTTYPE=hits"),
    }
}

/// The total from a bounded WFS hits response; unknown counts and exception reports aren't totals.
fn wfs_matched(bytes: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(bytes).ok()?;
    let doc = xml(text, 1024).ok()?;
    let root = doc.root_element();
    if local(&root) != "FeatureCollection" {
        return None;
    }
    root.attribute("numberMatched").or_else(|| root.attribute("numberOfFeatures"))?.trim().parse().ok()
}

/// A feature's service id, retaining numeric text without rounding and normalizing strings.
/// JSON quoting distinguishes string ids from numeric ids with the same characters.
fn feature_id(feature: &RawValue) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Id<'a> {
        #[serde(borrow)]
        id: Option<&'a RawValue>,
    }
    let id = serde_json::from_str::<Id<'_>>(feature.get()).ok()?.id?.get();
    match id.as_bytes().first()? {
        b'"' => serde_json::from_str::<String>(id).ok().map(|s| serde_json::Value::String(s).to_string()),
        b'-' | b'0'..=b'9' => Some(id.to_string()),
        _ => None,
    }
}

/// A content fingerprint for an ID-less row, used only to warn about possible overlap.
/// Rows remain distinct: equal content (or a hash collision) never causes one to be dropped.
fn unidentified_hash(feature: &RawValue) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    if feature_id(feature).is_some() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(feature.get()).ok()?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hash);
    Some(hash.finish())
}

/// A service's features at `url` read page by page, each answer `get` returns, up to `max`
/// features, as [`begin`] leads (the blocking way to drive it). The pages' features in one
/// collection, and a note when some features were left unread; a first page that leads nowhere
/// comes back unchanged. An error when the features couldn't be fetched at all.
pub fn join_pages(
    url: &str,
    max: usize,
    mut get: impl FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<(Vec<u8>, Option<String>), String> {
    let mut step = begin(url, max);
    loop {
        match step {
            Step::Done(bytes, note) => return Ok((bytes, note)),
            // Not given by [`begin`], which reads in GeoJSON.
            Step::Decoded(..) => return Err("the pages were read as protocol buffers".into()),
            Step::Failed(e) => return Err(e),
            Step::Fetch(pages) => {
                let answer = get(pages.next_url());
                step = (*pages).land(answer);
            }
        }
    }
}

/// Where reading a collection page by page has got to ([`begin`]).
#[derive(Debug)]
pub enum Step {
    /// The next answer to fetch ([`Pages::next_url`]), to hand to [`Pages::land`].
    Fetch(Box<Pages>),
    /// The collection as read (the pages joined, or the first page as it came) and a note when
    /// some features were left unread, or it was read in GeoJSON because its protocol buffer
    /// pages couldn't be read.
    Done(Vec<u8>, Option<String>),
    /// An ArcGIS layer read in protocol buffer pages (`f=pbf`, see [`begin_with_pbf`]): its
    /// features, what reading them said, and a note when some were left unread.
    Decoded(Box<crate::Dataset>, crate::ReadReport, Option<String>),
    /// The features couldn't be fetched at all: why.
    Failed(String),
}

/// A feature collection being read page by page, sans I/O: it says which address to fetch next
/// and takes the answer, so a blocking command ([`join_pages`]) and the app, which can't wait,
/// read the same way.
#[derive(Debug)]
pub struct Pages {
    /// The first page's address.
    url: String,
    /// The first page, without its features (null until a page is read).
    doc: serde_json::Value,
    /// The features read, each its JSON text as the server wrote it: read into values, a
    /// feature's every coordinate would be one, several times the text's size (#830).
    features: Vec<Box<RawValue>>,
    max: usize,
    seen: std::collections::HashSet<String>,
    /// The address to fetch now, and the one it was found from.
    next: String,
    from: String,
    phase: Phase,
    /// The protocol buffer pages read, as one layer, and what reading them said.
    decoded: Option<(crate::Dataset, crate::ReadReport, crate::esripbf::Losses)>,
    /// The memory those pages take, in bytes, and the most they may ([`max_decoded_bytes`]).
    decoded_bytes: usize,
    decoded_budget: usize,
    /// Why the layer's first protocol buffer page couldn't be read, when it is read in GeoJSON
    /// pages instead: said in the note it ends with.
    pbf_failed: Option<String>,
}

/// What the answer being fetched is.
#[derive(Debug)]
enum Phase {
    /// An ArcGIS layer's description, fetched before its first page: it says whether the layer
    /// can be read in pages, and how long they may be. `pbf`: in protocol buffer pages when it
    /// lists them.
    Describe { pbf: bool },
    /// The address asked for, as it is: the first page. `paging`: how the ArcGIS layer it queries
    /// is read in pages when this one is cut off (its description named no page size RustGIS
    /// asks for, so the pages are as long as this one).
    First { paging: Option<ArcgisPaging> },
    /// A WFS hits response, used when its GeoJSON gives no next link.
    WfsHits,
    /// WFS offsets count returned rows, while ids prevent overlapping pages adding duplicates.
    WfsOffsets {
        matched: u64,
        offset: usize,
        size: usize,
        ids: std::collections::HashSet<String>,
        unidentified: std::collections::HashSet<u64>,
        overlap: bool,
        duplicates: usize,
    },
    /// An OGC API or WFS page, retaining the largest reported legacy total across the chain.
    Linked { total: Option<u64> },
    /// An ArcGIS page, `size` long from an offset, in `order`. `start` is the first such page's
    /// first feature (a server that ignores the offset sends it again), once it is read; until
    /// then `first` keeps the first page as it came, to fall back on (none: the address asked
    /// for is fetched as it is instead). `pbf`: the pages are asked for as protocol buffers.
    Offsets { size: usize, order: Option<String>, start: Option<serde_json::Value>, first: Option<Vec<u8>>, pbf: bool },
}

/// A feature collection page's members other than its features (an object), and its features
/// as their JSON text; `None` when it is JSON but not a feature collection, an error when it can't
/// be read (as when it nests deeper than [`MAX_DEPTH`]).
fn split_page(bytes: &[u8]) -> Result<Option<(serde_json::Value, Vec<Box<RawValue>>)>, String> {
    if too_deep(bytes) {
        return Err("recursion limit exceeded".into());
    }
    let mut members: std::collections::BTreeMap<String, Box<RawValue>> = match serde_json::from_slice(bytes) {
        Ok(members) => members,
        // JSON that isn't an object is read, but isn't a collection.
        Err(e) => return serde_json::from_slice::<serde::de::IgnoredAny>(bytes).map(|_| None).map_err(|_| e.to_string()),
    };
    let Some(features) = members.remove("features").and_then(|f| serde_json::from_str::<Vec<Box<RawValue>>>(f.get()).ok()) else {
        return Ok(None);
    };
    let rest = members.into_iter().map(|(k, v)| Ok((k, serde_json::from_str(v.get())?))).collect::<serde_json::Result<_>>();
    Ok(Some((serde_json::Value::Object(rest.map_err(|e| e.to_string())?), features)))
}

/// Most arrays and objects a page may nest, as reading it into values allows (serde_json's limit).
const MAX_DEPTH: usize = 127;

/// Whether JSON text nests arrays and objects deeper than [`MAX_DEPTH`]: kept as text, a feature
/// isn't held to the limit reading it into values holds it to, and the joined collection would be
/// unreadable as a whole.
fn too_deep(text: &[u8]) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for &b in text {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return true;
                    }
                }
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    false
}

/// The first of a page's features as a value, to compare with another page's (the same feature
/// may be spaced differently).
fn first_value(features: &[Box<RawValue>]) -> Option<serde_json::Value> {
    serde_json::from_str(features.first()?.get()).ok()
}

/// What tells a protocol buffer page from another, to find a page sent again: its first feature's
/// object id, or else (no id, or an empty one) its fingerprint, taken from the page as it was sent
/// (ids are numbered within each page, two pages may start with the same row, and a page sent
/// again may be cut shorter by the limit).
fn first_mark(page: &crate::esripbf::Page) -> Option<serde_json::Value> {
    let f = page.dataset.features.first()?;
    let id = page
        .object_id_field
        .and_then(|i| f.attrs.get(i))
        .filter(|id| **id != crate::Value::Null)
        .and_then(|id| serde_json::to_value(id).ok());
    id.or_else(|| Some(serde_json::Value::String(format!("{:016x}", page.fingerprint))))
}

/// Starts reading a service's features at `url`, up to `max`, before anything is fetched.
///
/// An ArcGIS layer's query (one that doesn't ask for a window of its own: `resultOffset`,
/// `resultRecordCount`) fetches the layer's description first. When it says the layer can be read
/// in pages (`advancedQueryCapabilities.supportsPagination`) of a size it names
/// (`maxRecordCount`), the layer is read in pages (`resultOffset`, `resultRecordCount`) in the
/// order of its object ids from the start, so a layer past its record limit isn't read once in no
/// order and again in pages. Otherwise, or when the paging fails before a page is read, the query
/// is fetched as it is; a description naming no page size pages it, when it is cut off
/// (`exceededTransferLimit`), in pages as long as its answer.
///
/// Any other address is fetched as it is. An OGC API or WFS page leads to the next by its `next`
/// link. A WFS request without a next link uses a hits count and STARTINDEX pages (WFS 2.0).
/// Legacy WFS without links warns when its requested MAXFEATURES limit is reached.
///
/// The pages' features end up in one collection, with the first page's other members and
/// without its paging ones. A first page that leads nowhere is done at once, unchanged (and
/// [`partial_note`] says what it holds).
pub fn begin(url: &str, max: usize) -> Step {
    start(url, max, false)
}

/// Like [`begin`], but an ArcGIS layer whose description lists protocol buffers among its query
/// formats (`supportedQueryFormats`) is read in `f=pbf` pages, which end in [`Step::Decoded`]. When
/// its first page can't be fetched or read as one, the layer is read in GeoJSON pages instead. A
/// layer whose description names no page size is read in GeoJSON, as [`begin`] reads it: its pages are as
/// long as its first answer, asked for as it is.
pub fn begin_with_pbf(url: &str, max: usize) -> Step {
    start(url, max, true)
}

/// [`begin`], reading an ArcGIS layer in protocol buffer pages when `pbf` and it lists them.
fn start(url: &str, max: usize, pbf: bool) -> Step {
    // A query that asks for a window of its own is read as asked.
    let paged = is_arcgis_query(url) && ["resultOffset", "resultRecordCount"].iter().all(|k| query_value(url, k).is_none());
    let (next, phase) =
        if paged { (arcgis_layer_url(url), Phase::Describe { pbf }) } else { (url.to_string(), Phase::First { paging: None }) };
    Step::Fetch(Box::new(Pages {
        url: url.to_string(),
        doc: serde_json::Value::Null,
        features: Vec::new(),
        max,
        seen: std::collections::HashSet::new(),
        next,
        from: url.to_string(),
        phase,
        decoded: None,
        decoded_bytes: 0,
        decoded_budget: max_decoded_bytes(),
        pbf_failed: None,
    }))
}

/// Reads on from the first page (`first`, the answer to `url` as it is), as [`begin`] says;
/// `paging`, how the ArcGIS layer it queries is read in pages when it is cut off.
fn read_first(url: &str, first: Vec<u8>, max: usize, paging: Option<ArcgisPaging>) -> Step {
    let (head, tail) = edges(&first);
    let linked = head.contains("\"next\"") || tail.contains("\"next\"");
    let limited = paging.is_some() && (is_true(&head, "exceededTransferLimit") || is_true(&tail, "exceededTransferLimit"));
    let wfs = is_wfs_query(url);
    let legacy_wfs = is_legacy_wfs_query(url);
    if !(linked || limited || wfs || legacy_wfs) {
        return Step::Done(first, None);
    }
    let Ok(Some((doc, mut features))) = split_page(&first) else { return Step::Done(first, None) };
    let (next, phase) = if linked && let Some(next) = next_page(url, &doc) {
        (next, Phase::Linked { total: doc.get("totalFeatures").and_then(serde_json::Value::as_u64) })
    } else if limited
        && exceeded(&doc)
        && let Some(paging) = paging
        && (paging.size.is_some() || !features.is_empty())
    {
        // Offsets count in one order: this page, asked for in none, may hold other features than
        // the paged order's first, so the layer is read again from the start.
        let size = paging.size.unwrap_or(features.len());
        features.clear();
        (
            arcgis_page_url(url, 0, size, paging.order.as_deref()),
            Phase::Offsets { size, order: paging.order, start: None, first: Some(first), pbf: false },
        )
    } else if wfs {
        (wfs_page_url(url, None, 0), Phase::WfsHits)
    } else {
        let read = features.len();
        let note =
            if legacy_wfs && let Some(total) = doc.get("totalFeatures").and_then(serde_json::Value::as_u64).filter(|n| *n > read as u64) {
                Some(format!(
                    "{read} of {total} WFS features read: the server reported more features than it returned; the read may be incomplete"
                ))
            } else if legacy_wfs
                && doc.get("totalFeatures").and_then(serde_json::Value::as_u64).is_none()
                && let Some(limit) = query_value(url, "MAXFEATURES").and_then(|s| s.parse::<usize>().ok()).filter(|n| *n > 0)
                && read >= limit
            {
                Some(format!(
                    "{read} WFS features read: the requested limit of {limit} was reached without a next link; the read may be incomplete"
                ))
            } else {
                None
            };
        return Step::Done(first, note);
    };
    let pages = Pages {
        url: url.to_string(),
        doc,
        features,
        max,
        seen: std::collections::HashSet::from([url.to_string()]),
        next,
        from: url.to_string(),
        phase,
        decoded: None,
        decoded_bytes: 0,
        decoded_budget: max_decoded_bytes(),
        pbf_failed: None,
    };
    pages.advance()
}

/// Longest a reader of [`Pages`] waits for an ArcGIS layer's description, read before its
/// features ([`Pages::describing`]): one that hangs is given up on, and the layer's query is
/// fetched as it is.
pub const DESCRIPTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Most bytes a reader of [`Pages`] takes for an ArcGIS layer's description, which doesn't count
/// against the features' budget: a few kilobytes as a rule, so one larger than this is given up
/// on, and the layer's query fetched as it is.
pub const DESCRIPTION_MAX: usize = 4 << 20;

impl Pages {
    /// Whether the answer to fetch now is an ArcGIS layer's description, read before its features.
    pub fn describing(&self) -> bool {
        matches!(self.phase, Phase::Describe { .. })
    }

    /// The address to fetch now.
    pub fn next_url(&self) -> &str {
        &self.next
    }

    /// The address [`next_url`](Self::next_url) was found from (the page or query that leads to
    /// it), which it may only point where that one may.
    pub fn referrer(&self) -> &str {
        &self.from
    }

    /// How many features have been read so far.
    pub fn read(&self) -> usize {
        self.features.len() + self.decoded.as_ref().map_or(0, |(ds, ..)| ds.features.len())
    }

    /// Takes the answer to [`next_url`](Self::next_url) (or why it couldn't be fetched): the next
    /// address to fetch, or the collection as read. A layer read in GeoJSON because its protocol
    /// buffer pages couldn't be read says so in its note.
    pub fn land(mut self, answer: Result<Vec<u8>, String>) -> Step {
        let Some(why) = self.pbf_failed.take() else { return self.land_page(answer) };
        match self.land_page(answer) {
            // Carried to the next page, whichever reading goes on with it.
            Step::Fetch(mut pages) => {
                pages.pbf_failed = Some(why);
                Step::Fetch(pages)
            }
            Step::Done(bytes, note) => Step::Done(bytes, Some(read_in_geojson(&why, note))),
            Step::Decoded(ds, report, note) => Step::Decoded(ds, report, Some(read_in_geojson(&why, note))),
            step => step,
        }
    }

    fn land_page(mut self, answer: Result<Vec<u8>, String>) -> Step {
        if matches!(self.phase, Phase::WfsHits) {
            // Why the request failed is kept, so the notice says why completeness is unknown.
            let matched = match answer {
                Ok(b) => wfs_matched(&b).ok_or(None),
                // An error holding an address isn't repeated: `redact` keeps its path, which can
                // hold a key.
                Err(e) => Err(Some(e).filter(|e| !e.contains("://"))),
            };
            let matched = match matched {
                Ok(matched) => matched,
                Err(why) => {
                    // Without the credentials an address in the error may hold, and not all of a
                    // long one.
                    let why = why
                        .map(|e| {
                            let mut e = crate::diagnostics::redact(&e);
                            crate::diagnostics::cut_with_ellipsis(&mut e, MAX_REASON);
                            format!(" ({e})")
                        })
                        .unwrap_or_default();
                    return self.stop(format!("the WFS total couldn't be read{why}; the features received may be incomplete"));
                }
            };
            let offset = self.features.len();
            if offset > self.max {
                return self.finish_at_max();
            }
            if matched == offset as u64 {
                return self.finish(None);
            }
            if matched < offset as u64 {
                return self.stop("the WFS total changed; the features read may be incomplete".into());
            }
            if offset == 0 {
                return self.stop(format!("the WFS sent no features of its {matched}; the rest aren't read"));
            }
            let mut ids = std::collections::HashSet::new();
            self.features.retain(|f| feature_id(f).is_none_or(|id| ids.insert(id)));
            let duplicates = offset.saturating_sub(self.features.len());
            self.next = wfs_page_url(&self.url, Some(offset), offset.min(self.max));
            self.phase = Phase::WfsOffsets {
                matched,
                offset,
                size: offset.min(self.max),
                ids,
                unidentified: self.features.iter().filter_map(|f| unidentified_hash(f)).collect(),
                overlap: false,
                duplicates,
            };
            return self.advance();
        }
        if let Phase::Describe { pbf } = self.phase {
            let layer: Option<serde_json::Value> = answer.ok().and_then(|b| serde_json::from_slice(&b).ok());
            let pbf = pbf && layer.as_ref().is_some_and(supports_pbf);
            return match layer.as_ref().and_then(arcgis_paging) {
                Some(ArcgisPaging { size: Some(size), order }) => {
                    let page = arcgis_page_url(&self.url, 0, size, order.as_deref());
                    self.next = if pbf { with_format(&page, "pbf") } else { page };
                    self.phase = Phase::Offsets { size, order, start: None, first: None, pbf };
                    self.advance()
                }
                paging => self.ask_as_it_is(paging),
            };
        }
        if matches!(self.phase, Phase::Offsets { pbf: true, .. }) {
            return self.land_pbf(answer);
        }
        if let Phase::First { paging } = &mut self.phase {
            let paging = paging.take();
            return match answer {
                Ok(first) => read_first(&self.url, first, self.max, paging),
                Err(e) => Step::Failed(e),
            };
        }
        let read = self.features.len();
        let (page, mut more) = match answer.and_then(|b| split_page(&b)) {
            Ok(Some(page)) => page,
            Ok(None) => return self.stop(format!("the page after feature {read} isn't a feature collection; the rest aren't read")),
            Err(e) => return self.stop(format!("the page after feature {read} couldn't be read ({e}); the rest aren't read")),
        };
        let following = match &mut self.phase {
            Phase::WfsOffsets { matched, offset, size, ids, unidentified, overlap, duplicates } => {
                let returned = more.len();
                if returned == 0 {
                    return self.stop("the WFS sent an empty page before its reported total; the rest aren't read".into());
                }
                let hashes: Vec<_> = more.iter().filter_map(|f| unidentified_hash(f)).collect();
                *overlap |= hashes.iter().any(|h| unidentified.contains(h));
                unidentified.extend(hashes);
                more.retain(|f| feature_id(f).is_none_or(|id| ids.insert(id)));
                *duplicates = duplicates.saturating_add(returned.saturating_sub(more.len()));
                if more.is_empty() {
                    return self.stop("the WFS repeated an earlier page instead of advancing; the rest aren't read".into());
                }
                *offset = offset.saturating_add(returned);
                // Bound fetched rows too: overlapping pages cannot keep the browser downloading forever.
                if *offset >= self.max && (*offset as u64) < *matched {
                    self.features.extend(more);
                    self.features.truncate(self.max);
                    return self.stop("the WFS paging row limit was reached; the rest aren't read".into());
                }
                if (*offset as u64) >= *matched {
                    let matched = *matched;
                    let overlap = *overlap;
                    self.features.extend(more);
                    if self.features.len() > self.max {
                        return self.finish_at_max();
                    }
                    if self.features.len() as u64 != matched {
                        return self.stop("the WFS pages overlap or its total changed; the features read may be incomplete".into());
                    }
                    if overlap {
                        return self.stop(
                            "possible overlap in rows without ids; all returned rows are kept and the read may be incomplete".into(),
                        );
                    }
                    return self.finish(None);
                }
                Some(wfs_page_url(&self.url, Some(*offset), *size))
            }
            Phase::Offsets { size, order, start, first, .. } => {
                // A server that doesn't honour `resultOffset` sends its first page again.
                if start.is_some() && first_value(&more) == *start {
                    return self.stop("the server sent its first features again instead of the next ones; the rest aren't read".into());
                }
                if start.is_none() && !more.is_empty() {
                    *start = first_value(&more);
                    *first = None;
                }
                exceeded(&page).then(|| arcgis_page_url(&self.url, read + more.len(), *size, order.as_deref()))
            }
            Phase::Linked { total } => {
                *total = (*total).max(page.get("totalFeatures").and_then(serde_json::Value::as_u64));
                next_page(&self.next, &page)
            }
            _ => next_page(&self.next, &page),
        };
        // An ArcGIS layer read in pages from its description: the first page read gives the
        // collection's other members.
        if self.doc.is_null() {
            self.doc = page;
        }
        self.features.extend(more);
        if self.features.len() > self.max {
            return self.finish_at_max();
        }
        match following {
            Some(n) => {
                self.from = std::mem::replace(&mut self.next, n);
                self.advance()
            }
            None if matches!(self.phase, Phase::Linked { .. }) && is_wfs_query(&self.url) => {
                // A next-link chain can end at a server cap too; verify the accumulated total.
                self.from = self.url.clone();
                self.next = wfs_page_url(&self.url, None, 0);
                self.phase = Phase::WfsHits;
                self.advance()
            }
            None if let Phase::Linked { total } = self.phase
                && is_legacy_wfs_query(&self.url) =>
            {
                let read = self.features.len();
                let note = total.filter(|n| *n > read as u64).map(|total| {
                    format!("{read} of {total} WFS features read: the server reported more features than it returned; the read may be incomplete")
                });
                self.finish(note)
            }
            None => self.finish(None),
        }
    }

    /// Takes a protocol buffer page of an ArcGIS layer: its features joined to those read. A first
    /// page that can't be fetched, or comes but can't be read as one, has the layer read in GeoJSON
    /// pages instead (a server may list protocol buffers and fail them); a later one ends the read
    /// with the pages read.
    fn land_pbf(mut self, answer: Result<Vec<u8>, String>) -> Step {
        let read = self.read();
        // The features past the limit aren't read, so what they lost isn't counted.
        let room = self.max.saturating_sub(read);
        let Phase::Offsets { size, order, pbf, .. } = &mut self.phase else { return self.finish(None) };
        let page = answer.and_then(|bytes| crate::esripbf::read_page_within(&bytes, "query", room).map_err(|e| e.to_string()));
        let page = match page {
            Ok(page) => page,
            Err(e) if self.decoded.is_none() => {
                *pbf = false;
                // Said in a notice: without the credentials an address or a service's own error
                // message may hold, and not all of a long one.
                let mut why = crate::diagnostics::redact(&e);
                crate::diagnostics::cut_with_ellipsis(&mut why, MAX_REASON);
                self.pbf_failed = Some(why);
                // A query that asked for protocol buffers itself is read in GeoJSON too.
                if query_value(&self.url, "f").is_some_and(|f| f.eq_ignore_ascii_case("pbf")) {
                    self.url = with_format(&self.url, "geojson");
                }
                self.next = arcgis_page_url(&self.url, 0, *size, order.as_deref());
                return self.advance();
            }
            Err(e) => return self.stop(format!("the page after feature {read} couldn't be read ({e}); the rest aren't read")),
        };
        self.join_page(page)
    }

    /// Joins a protocol buffer page to those read, and asks for the next one.
    fn join_page(mut self, page: crate::esripbf::Page) -> Step {
        let read = self.read();
        let Phase::Offsets { size, order, start, .. } = &mut self.phase else { return self.finish(None) };
        // A server that doesn't honour `resultOffset` sends its first page again.
        let mark = first_mark(&page);
        if start.is_some() && mark == *start {
            return self.stop("the server sent its first features again instead of the next ones; the rest aren't read".into());
        }
        if start.is_none() {
            *start = mark;
        }
        // The first page is kept whatever it takes (the reader bounds a page); a later one that
        // would take the layer past its memory budget isn't joined.
        let bytes = self.decoded_bytes.saturating_add(page.memory);
        if self.decoded.is_some() && bytes > self.decoded_budget {
            let mb = self.decoded_budget >> 20;
            return self.stop(format!("the page after feature {read} would take the layer past {mb} MB of memory; the rest aren't read"));
        }
        self.decoded_bytes = bytes;
        let cut = page.left_out > 0;
        let returned = page.dataset.features.len();
        let following =
            page.exceeded_transfer_limit.then(|| with_format(&arcgis_page_url(&self.url, read + returned, *size, order.as_deref()), "pbf"));
        // What the features lost is summed over the pages, and said once (in `finish`); the
        // page's other messages, about its fields and system, once each.
        let report = page.report;
        match &mut self.decoded {
            None => self.decoded = Some((page.dataset, report, page.losses)),
            Some((ds, joined, losses)) => {
                // By name and type: the first page's aliases are kept.
                let same = |a: &crate::Field, b: &crate::Field| a.name == b.name && a.ty == b.ty;
                if ds.fields.len() != page.dataset.fields.len() || !ds.fields.iter().zip(&page.dataset.fields).all(|(a, b)| same(a, b)) {
                    return self.stop(format!("the page after feature {read} has other fields than the first; the rest aren't read"));
                }
                // Each page's positions are in its own system, and its geometries of its own type;
                // a page that doesn't say one is joined.
                let other_crs = ds.crs.is_some() && page.dataset.crs.is_some() && ds.crs != page.dataset.crs;
                let none = crate::GeometryType::None;
                let other_type =
                    ds.geometry_type != none && page.dataset.geometry_type != none && ds.geometry_type != page.dataset.geometry_type;
                if other_crs || other_type {
                    let what = if other_crs { "coordinate system" } else { "geometry type" };
                    return self.stop(format!("the page after feature {read} has another {what} than the first; the rest aren't read"));
                }
                // A page without features may not say its geometry type.
                if ds.geometry_type == none {
                    ds.geometry_type = page.dataset.geometry_type;
                }
                ds.zm_dropped |= page.dataset.zm_dropped;
                ds.crs_incomplete |= page.dataset.crs_incomplete;
                for f in page.dataset.features.into_vec() {
                    ds.push_next(f.geometry, f.attrs);
                }
                joined.skipped = joined.skipped.saturating_add(report.skipped);
                for m in report.messages {
                    if !joined.messages.contains(&m) {
                        joined.messages.push(m);
                    }
                }
                losses.add(&page.losses);
            }
        }
        if cut || self.read() > self.max {
            return self.finish_at_max();
        }
        match following {
            Some(n) => {
                self.from = std::mem::replace(&mut self.next, n);
                self.advance()
            }
            None => self.finish(None),
        }
    }

    /// Done early, with a note; or, when an ArcGIS layer's paged reading stops before any of its
    /// features are read, with the first page as it came (which says it was cut off), or the
    /// query fetched as it is when none was.
    fn stop(mut self, note: String) -> Step {
        let note = match self.phase {
            Phase::WfsOffsets { matched, .. } => format!("{} of {matched} WFS features read: {note}", self.features.len()),
            Phase::WfsHits => format!("{} WFS features read: {note}", self.features.len()),
            _ => note,
        };
        if let Phase::Offsets { first, start: None, .. } = &mut self.phase {
            return match first.take() {
                Some(first) => Step::Done(first, None),
                None => self.ask_as_it_is(None),
            };
        }
        self.finish(Some(note))
    }

    /// The query fetched as it is, from nothing read: an ArcGIS layer that can't be read in pages
    /// from its description (`paging`: in pages as long as its answer, when it is cut off).
    fn ask_as_it_is(mut self, paging: Option<ArcgisPaging>) -> Step {
        self.features.clear();
        self.decoded = None;
        self.doc = serde_json::Value::Null;
        self.next = self.url.clone();
        self.from = self.url.clone();
        self.phase = Phase::First { paging };
        Step::Fetch(Box::new(self))
    }

    /// The next page fetched, unless the features are all read that may be or the pages lead
    /// back to one already read.
    fn advance(mut self) -> Step {
        if self.read() > self.max || (self.read() == self.max && !matches!(self.phase, Phase::WfsHits)) {
            return self.finish_at_max();
        }
        if !self.seen.insert(self.next.clone()) {
            return self.stop("the service's pages lead back to an earlier page; the rest aren't read".into());
        }
        Step::Fetch(Box::new(self))
    }

    /// Done with the first `max` features.
    fn finish_at_max(mut self) -> Step {
        let max = self.max;
        self.features.truncate(max);
        if let Some((ds, ..)) = &mut self.decoded {
            ds.features.truncate(max);
        }
        // Not through `stop`, whose "N WFS features read:" would say the count twice.
        let note = match self.phase {
            Phase::WfsOffsets { matched, .. } => format!("the first {max} of {matched} WFS features are read; the rest aren't"),
            _ => format!("the first {max} features are read; the rest aren't"),
        };
        self.finish(Some(note))
    }

    /// The pages' features in the first page's collection, without its paging members.
    fn finish(mut self, note: Option<String>) -> Step {
        let note = if let Phase::WfsOffsets { duplicates, .. } = &self.phase
            && *duplicates > 0
        {
            let removed = format!("{duplicates} repeated service-id rows ignored");
            Some(match note {
                Some(note) => format!("{note}; {removed}"),
                None => removed,
            })
        } else {
            note
        };
        if let Some((ds, mut report, losses)) = self.decoded.take() {
            report.messages.extend(losses.messages());
            return Step::Decoded(Box::new(ds), report, note);
        }
        // Done before a page was read (a limit of none).
        if self.doc.is_null() {
            self.doc = serde_json::json!({"type": "FeatureCollection"});
        }
        if let Some(members) = self.doc.as_object_mut() {
            for paging in ["links", "numberMatched", "numberReturned", "exceededTransferLimit"] {
                members.remove(paging);
            }
            if let Some(p) = members.get_mut("properties").and_then(|p| p.as_object_mut()) {
                p.remove("exceededTransferLimit");
            }
        }
        // The other members written (an object, which always writes), then the features' text
        // spliced in as it came.
        let head = serde_json::to_vec(&self.doc).unwrap_or_default();
        let Some(open) = head.strip_suffix(b"}") else { return Step::Done(Vec::new(), note) };
        // `{` alone when the collection has no other members.
        let key: &[u8] = if open.len() > 1 { b",\"features\":[" } else { b"\"features\":[" };
        let size = self.features.iter().map(|f| f.get().len() + 1).sum::<usize>();
        let mut out = Vec::with_capacity(open.len() + key.len() + size + 2);
        out.extend_from_slice(open);
        out.extend_from_slice(key);
        for (i, f) in self.features.into_iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            out.extend_from_slice(f.get().as_bytes());
        }
        out.extend_from_slice(b"]}");
        Step::Done(out, note)
    }
}

/// How far past a layer's bounds an answer's positions may reach and still fit them: a tenth of
/// the bounds' size along that axis, or half a degree when that is more.
fn margin(lo: f64, hi: f64) -> f64 {
    ((hi - lo) / 10.0).max(0.5)
}

/// Whether the extent `[west, south, east, north]` lies within `bounds`, give or take [`margin`].
fn fits(extent: [f64; 4], bounds: [f64; 4]) -> bool {
    let [w, s, e, n] = bounds;
    let (mx, my) = (margin(w, e), margin(s, n));
    extent[0] >= w - mx && extent[2] <= e + mx && extent[1] >= s - my && extent[3] <= n + my
}

/// The extent of the positions in a GeoJSON answer (a collection, a feature or a geometry), read
/// without keeping them: `[min x, min y, max x, max y]`, or `None` for an answer that isn't JSON
/// or has no position.
fn positions_extent(bytes: &[u8]) -> Option<[f64; 4]> {
    use serde::de::DeserializeSeed;
    let mut extent = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let mut de = serde_json::Deserializer::from_slice(bytes);
    Members { extent: &mut extent, depth: 0 }.deserialize(&mut de).ok()?;
    (extent[0] <= extent[2] && extent[1] <= extent[3]).then_some(extent)
}

/// A GeoJSON object (or an array of them) whose `features`, `geometry`, `geometries` and
/// `coordinates` are walked for [`positions_extent`]; its other members are skipped.
struct Members<'a> {
    extent: &'a mut [f64; 4],
    depth: usize,
}

impl<'de> serde::de::DeserializeSeed<'de> for Members<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Members<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a GeoJSON value")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "features" | "geometry" | "geometries" if self.depth < 16 => {
                    map.next_value_seed(Members { extent: &mut *self.extent, depth: self.depth + 1 })?;
                }
                "coordinates" => {
                    map.next_value_seed(Positions { extent: &mut *self.extent, depth: 0 })?;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if self.depth >= 16 {
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            return Ok(());
        }
        while seq.next_element_seed(Members { extent: &mut *self.extent, depth: self.depth + 1 })?.is_some() {}
        Ok(())
    }

    // A null geometry, or a member of an unexpected kind: nothing to read.
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
}

/// A geometry's `coordinates`, nested arrays whose innermost hold positions: an array whose first
/// two elements are numbers, as [`swap_positions`] takes one (and as deep as it goes). Gives the
/// number it is, or `None` for an array or anything else.
struct Positions<'a> {
    extent: &'a mut [f64; 4],
    depth: usize,
}

impl<'de> serde::de::DeserializeSeed<'de> for Positions<'_> {
    type Value = Option<f64>;

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Option<f64>, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Positions<'_> {
    type Value = Option<f64>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("GeoJSON coordinates")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Option<f64>, A::Error> {
        if self.depth > 8 {
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            return Ok(None);
        }
        let mut first = [None, None];
        let mut i = 0;
        while let Some(v) = seq.next_element_seed(Positions { extent: &mut *self.extent, depth: self.depth + 1 })? {
            if let Some(slot) = first.get_mut(i) {
                *slot = v;
            }
            i += 1;
        }
        if let [Some(x), Some(y)] = first
            && x.is_finite()
            && y.is_finite()
        {
            let e = &mut *self.extent;
            *e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
        }
        Ok(None)
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Option<f64>, E> {
        Ok(Some(v))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Option<f64>, E> {
        Ok(Some(v as f64))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Option<f64>, E> {
        Ok(Some(v as f64))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Option<f64>, E> {
        Ok(None)
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Option<f64>, E> {
        Ok(None)
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Option<f64>, E> {
        Ok(None)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Option<f64>, A::Error> {
        while map.next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

fn swap_positions(v: &mut serde_json::Value, depth: usize) {
    if depth > 8 {
        return;
    }
    let Some(a) = v.as_array_mut() else { return };
    if a.len() >= 2 && a.iter().take(2).all(serde_json::Value::is_number) {
        a.swap(0, 1);
        return;
    }
    for c in a {
        swap_positions(c, depth + 1);
    }
}

fn swap_geometry(g: &mut serde_json::Value, depth: usize) {
    if depth > 8 {
        return;
    }
    // A box written in the old order would be wrong after the turn; it is optional, so it goes.
    if let Some(o) = g.as_object_mut() {
        o.remove("bbox");
    }
    if let Some(c) = g.get_mut("coordinates") {
        swap_positions(c, 0);
    }
    if let Some(gs) = g.get_mut("geometries").and_then(|g| g.as_array_mut()) {
        for g in gs {
            swap_geometry(g, depth + 1);
        }
    }
}

/// A service's GeoJSON answer with latitude-first coordinates turned round to longitude first, and
/// its `crs` and boxes (`bbox`, on the answer, its features and geometries) dropped; `None` when it
/// is left as it came.
///
/// The data decide, not the `crs` name: GeoServer names longitude-first answers
/// `urn:ogc:def:crs:EPSG::4326`, and deegree's one latitude-first answer has no `crs` at all
/// (#1053). The positions are turned round only when their extent misses the layer's
/// longitude/latitude `bounds` from the capabilities (west, south, east, north) and the turned
/// extent fits them, give or take a tenth of their size or half a degree. With no bounds, or an
/// answer that fits both ways (or neither), it can't tell, and nothing is turned.
pub fn fix_axes(bytes: &[u8], bounds: Option<[f64; 4]>) -> Option<Vec<u8>> {
    let bounds = bounds?;
    let [x0, y0, x1, y1] = positions_extent(bytes)?;
    if fits([x0, y0, x1, y1], bounds) || !fits([y0, x0, y1, x1], bounds) {
        return None;
    }
    let mut v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    // A GeoJSON answer is one object; anything else (a list of features) is left as it came.
    let o = v.as_object_mut()?;
    o.remove("crs");
    o.remove("bbox");
    match v.get("type").and_then(|t| t.as_str()) {
        Some("FeatureCollection") => {
            for f in v.get_mut("features").and_then(|f| f.as_array_mut()).into_iter().flatten() {
                if let Some(obj) = f.as_object_mut() {
                    obj.remove("bbox");
                }
                if let Some(g) = f.get_mut("geometry") {
                    swap_geometry(g, 0);
                }
            }
        }
        Some("Feature") => {
            if let Some(g) = v.get_mut("geometry") {
                swap_geometry(g, 0);
            }
        }
        _ => swap_geometry(&mut v, 0),
    }
    serde_json::to_vec(&v).ok()
}

#[cfg(test)]
#[path = "../../../tests/support/ogc.rs"]
mod fixtures;

#[cfg(test)]
#[path = "ogc_document_tests.rs"]
mod document_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_and_collections_preserve_only_the_explicit_login() {
        let binding = "0123456789abcdef";
        let origin = "https://maps.example.org";
        for (url, kind) in [
            ("https://MAPS.example.org:443/wms?map=x", None),
            ("https://maps.example.org/ogc/features?f=html&key=a%2Fb", Some("ogcapi")),
            ("https://maps.example.org/rest/services/P/FeatureServer", Some("arcgis")),
            ("https://maps.example.org/capabilities.xml", None),
        ] {
            let address = crate::login_address::bind(binding, origin, url).unwrap();
            let requested = capabilities_url(&address, kind);
            let bound = crate::login_address::parse(&requested).unwrap().unwrap();
            assert_eq!((bound.binding, bound.origin), (binding, origin));
            assert_eq!(bound.url, capabilities_url(url, kind));
            assert_eq!(capabilities_url(&requested, kind), requested);
            assert!(crate::login_address::parse(&capabilities_url(url, kind)).unwrap().is_none());
        }
        let address = crate::login_address::bind(binding, origin, "https://maps.example.org/api?key=a%2Fb").unwrap();
        for href in ["/collections", "https://MAPS.example.org:443/collections"] {
            let links = serde_json::json!([{"rel":"data","type":"application/json","href":href}]);
            let (next, withheld) = collections_link(&address, links.as_array().unwrap()).unwrap();
            assert_eq!(crate::login_address::parse(&next).unwrap().unwrap().binding, binding);
            assert!(withheld, "the advertised destination omits the original key");
        }
        let links = serde_json::json!([{"rel":"data","type":"application/json","href":"https://other.example.org/collections"}]);
        assert_eq!(collections_link(&address, links.as_array().unwrap()).unwrap(), ("https://other.example.org/collections".into(), true));
    }

    #[test]
    fn linked_pages_drop_the_login_after_leaving_its_origin() {
        let first_url =
            crate::login_address::bind("0123456789abcdef", "https://maps.example.org", "https://maps.example.org/api/items").unwrap();
        let page = |id, href| {
            serde_json::to_vec(&serde_json::json!({
                "type":"FeatureCollection", "features":[{"id":id}],
                "links":[{"rel":"next","type":"application/geo+json","href":href}]
            }))
            .unwrap()
        };
        let Step::Fetch(start) = begin(&first_url, 100) else {
            panic!("the first page must be fetched");
        };
        let Step::Fetch(pages) = start.land(Ok(page(1, "https://MAPS.example.org:443/api/items?page=2"))) else {
            panic!("the second page must be fetched");
        };
        assert_eq!(pages.referrer(), first_url);
        let second_url = pages.next_url().to_string();
        let bound = crate::login_address::parse(&second_url).unwrap().unwrap();
        assert_eq!(bound.binding, "0123456789abcdef");
        assert_eq!(bound.url, "https://MAPS.example.org:443/api/items?page=2");
        let Step::Fetch(pages) = pages.land(Ok(page(2, "https://other.example.org/items?page=3"))) else {
            panic!("the foreign page must be fetched anonymously");
        };
        assert_eq!(pages.referrer(), second_url);
        assert_eq!(pages.next_url(), "https://other.example.org/items?page=3");
        let Step::Fetch(pages) = pages.land(Ok(page(3, "https://maps.example.org/api/items?page=4"))) else {
            panic!("the returning page must remain anonymous");
        };
        assert_eq!(pages.next_url(), "https://maps.example.org/api/items?page=4");
        assert!(crate::login_address::parse(pages.next_url()).unwrap().is_none());
        assert_eq!(
            resolve(&first_url, "/api/items?page=2").unwrap(),
            crate::login_address::bind("0123456789abcdef", "https://maps.example.org", "https://maps.example.org/api/items?page=2")
                .unwrap()
        );
        assert_eq!(resolve("https://maps.example.org/api", &first_url).unwrap(), "https://maps.example.org/api/items");
    }

    /// Reads as [`super::join_pages`] does, the address asked for answered with `first` (once) and
    /// every other with `get`.
    fn join_pages(
        url: &str,
        first: Vec<u8>,
        max: usize,
        mut get: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> (Vec<u8>, Option<String>) {
        let mut first = Some(first);
        super::join_pages(url, max, |u| match first.take_if(|_| u == url) {
            Some(first) => Ok(first),
            None => get(u),
        })
        .unwrap()
    }

    /// A layer's box names its system otherwise than its list (`EPSG:3035` for the URN): found by
    /// code, it still says the service writes LAEA Europe north first.
    #[test]
    fn a_box_is_found_by_its_systems_code() {
        let xml = r#"<Layer><CRS>urn:ogc:def:crs:EPSG::3035</CRS><BoundingBox CRS="EPSG:3035" minx="2700000" miny="4000000" maxx="3600000" maxy="4700000"/></Layer>"#;
        let doc = roxmltree::Document::parse(xml).unwrap();
        let listed = wms_candidates(&["urn:ogc:def:crs:EPSG::3035".to_string()]);
        let (_, w) = wms_native(&doc.root_element(), &listed, true, Some([6.0, 47.0, 15.0, 55.0]), false, &mut LayerBoxes::new()).unwrap();
        assert_eq!((w.epsg, w.grid), (3035, warp::Grid::Map { lat_first: true }));
    }

    /// A WMS 1.3.0 layer is asked in the order EPSG gives its system's axes, with no box to read
    /// it from (Gauss-Krüger zone 4, north first) and over a box written the other way (LV95,
    /// east first); WMS 1.1.1 is always east first.
    #[test]
    fn a_wms_layer_follows_epsg_axis_order() {
        let order = |xml: &str, crs: &str, v13: bool| {
            let doc = roxmltree::Document::parse(xml).unwrap();
            let listed = wms_candidates(&[crs.to_string()]);
            let (_, w) =
                wms_native(&doc.root_element(), &listed, v13, Some([10.0, 47.5, 13.0, 50.5]), false, &mut LayerBoxes::new()).unwrap();
            w.grid
        };
        let gk4 = "<Layer><CRS>EPSG:31468</CRS></Layer>";
        assert_eq!(order(gk4, "EPSG:31468", true), warp::Grid::Map { lat_first: true });
        assert_eq!(order(gk4, "EPSG:31468", false), warp::Grid::Map { lat_first: false });
        let lv95 = r#"<Layer><CRS>EPSG:2056</CRS><BoundingBox CRS="EPSG:2056" minx="1070000" miny="2480000" maxx="1300000" maxy="2840000"/></Layer>"#;
        assert_eq!(order(lv95, "EPSG:2056", true), warp::Grid::Map { lat_first: false });
    }

    /// A tile matrix set keeps the matrices it can read when one can't be (no identifier), and
    /// past `MAX_MATRICES` its 20 coarsest and 20 finest.
    #[test]
    fn a_tile_matrix_set_keeps_what_it_can_read() {
        let matrix = |id: &str, scale: f64| {
            format!(
                "<TileMatrix>{id}<ScaleDenominator>{scale}</ScaleDenominator><TopLeftCorner>2420000 1350000</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>4</MatrixWidth><MatrixHeight>3</MatrixHeight></TileMatrix>"
            )
        };
        let set = |ms: &str| format!("<TileMatrixSet><SupportedCRS>EPSG:2056</SupportedCRS>{ms}</TileMatrixSet>");
        let xml = set(&(matrix("<Identifier>a</Identifier>", 2_000_000.0) + &matrix("", 1_000_000.0)));
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let (code, _, ms) = native_matrices(&doc.root_element()).unwrap();
        assert_eq!((code, ms.iter().map(|m| m.id.as_str()).collect::<Vec<_>>()), (2056, vec!["a"]));
        let many: String = (0..50).map(|i| matrix(&format!("<Identifier>{i}</Identifier>"), 1e7 / f64::from(i + 1))).collect();
        let xml = set(&many);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let mut ms = native_matrices(&doc.root_element()).unwrap().2;
        keep_matrices(&mut ms);
        let ids: Vec<&str> = ms.iter().map(|m| m.id.as_str()).collect();
        assert_eq!((ids.len(), ids[0], ids[19], ids[20], ids[39]), (40, "0", "19", "30", "49"));
    }

    /// #167: World_Street_Map's map is named "Layers" and titled "Map", and its description is
    /// HTML: the service is named from its address, never from the description.
    #[test]
    fn arcgis_title_is_a_name_not_the_description() {
        let street = r#"{"currentVersion": 11.5, "mapName": "Layers",
            "serviceDescription": "<div style='text-align:Left'><p><span>This worldwide street map presents highway-level data.</span></p></div>",
            "documentInfo": {"Title": "Map", "Comments": "<DIV>This worldwide street map</DIV>"},
            "layers": [{"id": 0, "name": "World Street Map"}]}"#;
        let s = parse("https://services.arcgisonline.com/arcgis/rest/services/World_Street_Map/MapServer", street.as_bytes()).unwrap();
        assert_eq!(s.title, "World_Street_Map");
        assert_eq!(s.layers[0].title, "World_Street_Map (map)");
        assert_eq!(s.layers[1].title, "World Street Map");
        // Its capabilities are "Map,Tilemap": no queries, so only the map (and its layer drawn
        // alone) is offered.
        let cached = street.replace(r#""mapName""#, r#""capabilities": "Map,Tilemap", "mapName""#);
        let s = parse("https://services.arcgisonline.com/arcgis/rest/services/World_Street_Map/MapServer", cached.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), 2);
        assert!(s.layers.iter().all(|l| matches!(&l.offer, Offer::Tiles { .. })));
        assert!(s.notes.iter().any(|n| n.contains("doesn't allow queries")), "{:?}", s.notes);
        let queried = street.replace(r#""mapName""#, r#""capabilities": "Map,Query,Data", "mapName""#);
        assert_eq!(parse("https://x.org/arcgis/rest/services/S/MapServer", queried.as_bytes()).unwrap().layers.len(), 3);
        let data = street.replace(r#""mapName""#, r#""capabilities": "Map,Data", "mapName""#);
        assert_eq!(parse("https://x.org/arcgis/rest/services/S/MapServer", data.as_bytes()).unwrap().layers.len(), 3);
        // A title of its own is kept; one that is HTML is not.
        let titled = r#"{"mapName": "Layers", "documentInfo": {"Title": "Street Map"}, "layers": []}"#;
        assert_eq!(parse("https://x.org/arcgis/rest/services/S/MapServer", titled.as_bytes()).unwrap().title, "Street Map");
        let html = r#"{"name": "<b>Parcels</b>", "serviceDescription": "Parcels", "layers": []}"#;
        assert_eq!(parse("https://x.org/arcgis/rest/services/Folder/P/FeatureServer", html.as_bytes()).unwrap().title, "P");
    }

    #[test]
    fn arcgis_services() {
        assert_eq!(
            capabilities_url("https://x.org/arcgis/rest/services/A/FeatureServer", None),
            "https://x.org/arcgis/rest/services/A/FeatureServer?f=json"
        );
        let fs = r#"{"currentVersion": 11.1, "serviceDescription": "Parcels", "maxRecordCount": 2000,
            "fullExtent": {"xmin": -1000000, "ymin": 0, "xmax": 1000000, "ymax": 1000000, "spatialReference": {"wkid": 102100, "latestWkid": 3857}},
            "layers": [{"id": 0, "name": "Parcels"}, {"id": 1, "name": "Group", "subLayerIds": [2]}, {"id": 2, "name": "Roads"}]}"#;
        let s = parse("https://x.org/arcgis/rest/services/A/FeatureServer", fs.as_bytes()).unwrap();
        assert_eq!(s.kind, "ArcGIS FeatureServer");
        assert_eq!(s.layers.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), ["Parcels", "Roads"]);
        assert!(
            matches!(&s.layers[0].offer, Offer::Features { url } if url == "https://x.org/arcgis/rest/services/A/FeatureServer/0/query?where=1%3D1&outFields=*&returnGeometry=true&outSR=4326&f=geojson")
        );
        let b = s.layers[0].bbox.unwrap();
        assert!((b[0] + 8.983).abs() < 0.01 && b[1].abs() < 1e-9, "{b:?}");
        let tiled = r#"{"currentVersion": 10.9, "mapName": "World", "singleFusedMapCache": true,
            "tileInfo": {"rows": 256, "spatialReference": {"wkid": 102100}, "lods": [{"level": 0, "resolution": 156543.03392800014}]}, "layers": []}"#;
        let s = parse("https://x.org/arcgis/rest/services/World/MapServer?f=json", tiled.as_bytes()).unwrap();
        assert_eq!(s.title, "World");
        assert!(
            matches!(&s.layers[0].offer, Offer::Tiles { template } if template == "https://x.org/arcgis/rest/services/World/MapServer/tile/{z}/{y}/{x}")
        );
        let dynamic = r#"{"currentVersion": 10.9, "mapName": "Dyn", "layers": [{"id": 3, "name": "Wells"}]}"#;
        let s = parse("https://x.org/arcgis/rest/services/Dyn/MapServer", dynamic.as_bytes()).unwrap();
        assert!(matches!(&s.layers[0].offer, Offer::Tiles { template } if template.contains("/export?bbox={bbox-epsg-3857}")));
        assert!(matches!(&s.layers[1].offer, Offer::Features { url } if url.contains("/MapServer/3/query?")));
        // Each layer of a dynamic map is also drawn alone.
        assert_eq!(s.layers[2].name, "map:3");
        assert!(matches!(&s.layers[2].offer, Offer::Tiles { template } if template.ends_with("&layers=show:3")));
        // A cached map draws no layer alone.
        let tiled_layers = tiled.replace(r#""layers": []"#, r#""layers": [{"id": 0, "name": "Roads"}]"#);
        let s = parse("https://x.org/arcgis/rest/services/World/MapServer", tiled_layers.as_bytes()).unwrap();
        assert!(!s.layers.iter().any(|l| l.name.starts_with("map:")));
        let vts = r#"{"currentVersion": 11, "name": "Basemap", "defaultStyles": "resources/styles"}"#;
        let s = parse("https://x.org/arcgis/rest/services/B/VectorTileServer", vts.as_bytes()).unwrap();
        assert!(
            matches!(&s.layers[0].offer, Offer::Style { url } if url == "https://x.org/arcgis/rest/services/B/VectorTileServer/resources/styles/root.json")
        );
        let img = r#"{"currentVersion": 11, "name": "Elev"}"#;
        let s = parse("https://x.org/arcgis/rest/services/E/ImageServer", img.as_bytes()).unwrap();
        assert!(matches!(&s.layers[0].offer, Offer::Tiles { template } if template.contains("/exportImage?")));
        let dir = r#"{"currentVersion": 11, "folders": ["Hydro", "Utilities", "../x", "a b"], "services": [{"name": "A", "type": "FeatureServer"},
            {"name": "Geo", "type": "GPServer"}, {"name": "B?f=x", "type": "MapServer"}]}"#;
        let s = parse("https://x.org/arcgis/rest/services", dir.as_bytes()).unwrap();
        assert_eq!(s.kind, "ArcGIS Services Directory");
        let offers: Vec<_> = s.layers.iter().map(|l| (l.name.as_str(), &l.offer)).collect();
        assert_eq!(
            offers,
            [
                ("Hydro/", &Offer::Service { url: "https://x.org/arcgis/rest/services/Hydro".into() }),
                ("A/FeatureServer", &Offer::Service { url: "https://x.org/arcgis/rest/services/A/FeatureServer".into() })
            ]
        );
        assert!(s.notes.iter().any(|n| n.contains("1 services")));
        assert!(s.notes.iter().any(|n| n.starts_with("3 folders or services")), "{:?}", s.notes);
        // A folder lists its services by their full names, which start at the root.
        let folder = r#"{"folders": [], "services": [{"name": "Hydro/Rivers", "type": "MapServer"}]}"#;
        let s = parse("https://x.org/arcgis/rest/services/Hydro?f=json", folder.as_bytes()).unwrap();
        assert_eq!(s.title, "Hydro");
        assert_eq!(s.layers[0].title, "Rivers (MapServer)");
        assert_eq!(s.layers[0].offer, Offer::Service { url: "https://x.org/arcgis/rest/services/Hydro/Rivers/MapServer".into() });
        // A proxy's path without `/rest/services`: the folder's services go under the address.
        let proxied = folder.replace(r#"{"folders""#, r#"{"currentVersion": 11, "folders""#);
        let s = parse("https://proxy.example.org/gis/Hydro?f=json", proxied.as_bytes()).unwrap();
        assert_eq!(s.layers[0].offer, Offer::Service { url: "https://proxy.example.org/gis/Hydro/Rivers/MapServer".into() });
        assert!(
            parse("https://x.org/arcgis/rest/services/A/FeatureServer", br#"{"error": {"code": 499, "message": "Token Required"}}"#)
                .unwrap_err()
                .contains("Token Required")
        );
        // A secured service's refusal says how to sign in.
        let e = parse("https://x.org/arcgis/rest/services/A/FeatureServer", br#"{"error": {"code": 499, "message": "Token Required"}}"#)
            .unwrap_err();
        assert!(e.contains("arcgis.signIn"), "{e}");
    }

    #[test]
    fn capabilities_addresses() {
        assert_eq!(capabilities_url("https://x.org/wms", None), "https://x.org/wms?SERVICE=WMS&REQUEST=GetCapabilities");
        assert_eq!(
            capabilities_url("https://x.org/geoserver/ows?", Some("wfs")),
            "https://x.org/geoserver/ows?SERVICE=WFS&REQUEST=GetCapabilities"
        );
        assert_eq!(
            capabilities_url("https://x.org/a?map=b&request=GetCapabilities&service=WMS", None),
            "https://x.org/a?map=b&request=GetCapabilities&service=WMS"
        );
        assert_eq!(capabilities_url("https://x.org/ogc/features/collections", None), "https://x.org/ogc/features/collections?f=json");
        assert_eq!(
            capabilities_url("https://ows.example.org/osm/service", None),
            "https://ows.example.org/osm/service?SERVICE=WMS&REQUEST=GetCapabilities"
        );
        // The host doesn't decide the type: only the path and query do.
        assert_eq!(
            capabilities_url("https://rapid.example.org/geoserver/ows", None),
            "https://rapid.example.org/geoserver/ows?SERVICE=WMS&REQUEST=GetCapabilities"
        );
        assert_eq!(
            capabilities_url("https://wfs.example.org/maps", None),
            "https://wfs.example.org/maps?SERVICE=WMS&REQUEST=GetCapabilities"
        );
        assert_eq!(capabilities_url("https://x.org/geo/api/v1", None), "https://x.org/geo/api/v1?f=json");
        assert_eq!(capabilities_url("https://x.org/rapids/v1", None), "https://x.org/rapids/v1?SERVICE=WMS&REQUEST=GetCapabilities");
        // Whole query keys: `prof=` isn't `f=`.
        assert_eq!(
            capabilities_url("https://x.org/arcgis/rest/services/A/MapServer?prof=json", None),
            "https://x.org/arcgis/rest/services/A/MapServer?prof=json&f=json"
        );
        // ArcGIS's own OGC endpoints are read as WMS and WMTS.
        assert_eq!(
            capabilities_url("https://x.org/arcgis/rest/services/A/MapServer/WMSServer", None),
            "https://x.org/arcgis/rest/services/A/MapServer/WMSServer?SERVICE=WMS&REQUEST=GetCapabilities"
        );
        assert_eq!(query_value("https://x.org/a?F=json&limit=5#x", "f"), Some("json"));
        assert_eq!(query_value("https://x.org/a?shelf=1", "f"), None);
    }

    #[test]
    fn links_resolved_and_schemes_checked() {
        assert_eq!(
            resolve("https://x.org/api/collections?f=json", "collections/a/items").as_deref(),
            Some("https://x.org/api/collections/a/items")
        );
        assert_eq!(resolve("https://x.org/api/collections", "/ogc/a/items").as_deref(), Some("https://x.org/ogc/a/items"));
        assert_eq!(resolve("https://x.org", "a/items").as_deref(), Some("https://x.org/a/items"));
        assert_eq!(resolve("http://x.org/c", "//y.org/a").as_deref(), Some("http://y.org/a"));
        assert_eq!(resolve("https://x.org/c", "https://y.org/a").as_deref(), Some("https://y.org/a"));
        assert_eq!(
            resolve("https://x.org/cgi-bin/mapserv?SERVICE=WMS&REQUEST=GetCapabilities#old", "?map=advertised").as_deref(),
            Some("https://x.org/cgi-bin/mapserv?map=advertised")
        );
        assert_eq!(resolve("https://x.org/folder/c", "path/part:detail").as_deref(), Some("https://x.org/folder/path/part:detail"));
        assert_eq!(resolve("https://x.org/folder/c", "?key=value:detail").as_deref(), Some("https://x.org/folder/c?key=value:detail"));
        for (from, href, expected) in [
            ("https://x.org/cgi-bin/mapserv#old", "#frag", "https://x.org/cgi-bin/mapserv#frag"),
            ("https://x.org/cgi-bin/mapserv?map=kept#old", "#frag", "https://x.org/cgi-bin/mapserv?map=kept#frag"),
            ("https://x.org/cgi-bin/mapserv?map=kept#old", "#", "https://x.org/cgi-bin/mapserv?map=kept#"),
            (
                "https://x.org/cgi-bin/mapserv?old=discard#old",
                "?upstream=https://data.example/tiles",
                "https://x.org/cgi-bin/mapserv?upstream=https://data.example/tiles",
            ),
        ] {
            assert_eq!(resolve(from, href).as_deref(), Some(expected), "{href}");
        }
        for bad in ["file:///etc/passwd", "file:/etc/passwd", "mailto:x", "CuStOm+v1.0:payload", "httpfoo://x/a", "javascript:alert(1)", ""]
        {
            assert_eq!(resolve("https://x.org/c", bad), None, "{bad}");
        }
        assert!(is_web("HTTPS://x.org") && !is_web("httpfoo:x") && !is_web("http:/x"));
        // A capabilities document's endpoints in other schemes are not used.
        let xml = r#"<WMS_Capabilities version="1.3.0" xmlns:xlink="http://www.w3.org/1999/xlink"><Capability><Request><GetMap><DCPType><HTTP><Get><OnlineResource xlink:href="httpfoo://evil/wms"/></Get></HTTP></DCPType></GetMap></Request>
<Layer><Name>a</Name><CRS>EPSG:3857</CRS></Layer></Capability></WMS_Capabilities>"#;
        let s = parse("https://maps.example.org/wms", xml.as_bytes()).unwrap();
        assert!(matches!(&s.layers[0].offer, Offer::Tiles { template } if template.starts_with("https://maps.example.org/wms?")));
        // OGC API items links that are relative, or not web addresses.
        let json = r#"{"collections": [{"id": "a", "links": [{"rel": "items", "href": "collections/a/items"}]}, {"id": "b", "links": [{"rel": "items", "href": "file:///x"}]}]}"#;
        let s = parse("https://api.example.org/collections", json.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), 1);
        assert!(
            matches!(&s.layers[0].offer, Offer::Features { url } if url == "https://api.example.org/collections/a/items?limit=10000&f=json"),
            "{:?}",
            s.layers[0].offer
        );
        assert!(s.notes.iter().any(|n| n.contains("1 collections")));
    }

    /// OGC API collections whose items are 3D Tiles or quantized mesh terrain, for 3D viewers, as
    /// PDOK's 3D Basisvoorziening links them (#1267): left out, by name in the notes, and a
    /// collection that also links GeoJSON items is offered those.
    #[test]
    fn collections_of_3d_items_are_left_out() {
        let json = r#"{"collections": [
            {"id": "gebouwen", "itemType": "feature", "links": [{"rel": "items", "type": "application/json+3dtiles", "href": "https://x.org/collections/gebouwen/3dtiles?f=json"}]},
            {"id": "terreinen", "itemType": "feature", "links": [{"rel": "items", "type": "application/json+3dtiles", "href": "https://x.org/collections/terreinen/3dtiles?f=json"}]},
            {"id": "relative", "links": [{"rel": "items", "href": "3dtiles"}]},
            {"id": "tiled", "links": [{"rel": "items", "type": "application/json+3dtiles", "href": "https://x.org/collections/tiled/3dtiles"}, {"rel": "http://www.opengis.net/def/rel/ogc/1.0/tilesets-map", "href": "https://x.org/collections/tiled/map/tiles"}]},
            {"id": "bin", "links": [{"rel": "items", "type": "application/octet-stream", "href": "https://x.org/collections/bin/3dtiles"}]},
            {"id": "dtm", "itemType": "feature", "links": [{"rel": "items", "type": "application/json", "href": "https://x.org/collections/dtm/quantized-mesh?f=json"}]},
            {"id": "both", "links": [{"rel": "items", "href": "https://x.org/collections/both/3dtiles/"}, {"rel": "items", "type": "application/geo+JSON", "href": "https://x.org/collections/both/items?f=json"}]},
            {"id": "plain", "links": [{"rel": "items", "type": "application/geo+json", "href": "https://x.org/collections/plain/items?f=json"}]}
        ]}"#;
        let s = parse("https://x.org/collections", json.as_bytes()).unwrap();
        let offered: Vec<_> =
            s.layers.iter().filter(|l| matches!(l.offer, Offer::Features { .. })).map(|l| (l.name.as_str(), &l.offer)).collect();
        assert_eq!(offered.len(), 2, "{offered:?}");
        assert!(
            matches!(offered[0], ("both", Offer::Features { url }) if url == "https://x.org/collections/both/items?f=json&limit=10000"),
            "{offered:?}"
        );
        assert_eq!(offered[1].0, "plain");
        assert!(
            s.notes.contains(
                &"collections 'gebouwen', 'terreinen', 'relative', 'bin' offer 3D Tiles, which RustGIS doesn't draw, so they are left out"
                    .to_string()
            ),
            "{:?}",
            s.notes
        );
        assert!(
            s.notes.contains(&"collection 'dtm' offers quantized mesh terrain, which RustGIS doesn't draw, so it is left out".to_string()),
            "{:?}",
            s.notes
        );
        // A collection with tiles too is listed by them, and the note says its 3D items aren't.
        assert!(s.layers.iter().any(|l| l.name == "tiled (map tiles)"));
        assert!(
            s.notes.contains(&"collection 'tiled' also offers 3D Tiles, which RustGIS doesn't draw; its tiles are listed".to_string()),
            "{:?}",
            s.notes
        );
        assert!(!s.incomplete);
        // Past 20 names, the note says how many more.
        let many: Vec<String> =
            (0..23).map(|i| format!(r#"{{"id": "c{i}", "links": [{{"rel": "items", "href": "https://x.org/c{i}/3dtiles"}}]}}"#)).collect();
        let s = parse("https://x.org/collections", format!(r#"{{"collections": [{}]}}"#, many.join(",")).as_bytes()).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("'c19', and 3 more offer 3D Tiles")), "{:?}", s.notes);
        // Without them, no note on 3D content.
        let plain = r#"{"collections": [{"id": "plain", "links": [{"rel": "items", "type": "application/geo+json", "href": "https://x.org/collections/plain/items?f=json"}]}]}"#;
        let s = parse("https://x.org/collections", plain.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), 1);
        assert!(!s.notes.iter().any(|n| n.contains(LEFT_OUT_3D)), "{:?}", s.notes);
    }

    #[test]
    fn features_asked_for_over_an_area() {
        let piaui = [-45.99, -10.93, -40.37, -2.74];
        let offered = |caps: &str, body: &str| {
            let s = parse(caps, body.as_bytes()).unwrap();
            let Offer::Features { url } = &s.layers[0].offer else { panic!("{:?}", s.layers[0].offer) };
            url.clone()
        };
        // WFS 2.0 offering CRS84: the box in CRS84, longitude first.
        let v20 = r#"<WFS_Capabilities version="2.0.0"><FeatureTypeList><FeatureType><Name>CGMAT:munic</Name><DefaultCRS>urn:ogc:def:crs:EPSG::4674</DefaultCRS><OtherCRS>urn:ogc:def:crs:OGC:1.3:CRS84</OtherCRS></FeatureType></FeatureTypeList></WFS_Capabilities>"#;
        let url = offered("https://x.org/geoserver/wfs", v20);
        assert_eq!(features_in_area(&url, piaui).unwrap(), format!("{url}&BBOX=-45.99,-10.93,-40.37,-2.74,urn:ogc:def:crs:OGC:1.3:CRS84"));
        // WFS 1.1 without CRS84: latitude first, as urn:ogc:def:crs:EPSG::4326 fixes.
        let v11 = r#"<WFS_Capabilities version="1.1.0"><FeatureTypeList><FeatureType><Name>a</Name><DefaultSRS>urn:ogc:def:crs:EPSG::4326</DefaultSRS></FeatureType></FeatureTypeList></WFS_Capabilities>"#;
        let url = offered("https://x.org/wfs", v11);
        assert_eq!(features_in_area(&url, piaui).unwrap(), format!("{url}&BBOX=-10.93,-45.99,-2.74,-40.37,urn:ogc:def:crs:EPSG::4326"));
        // WFS 1.0 reads its box in the type's own coordinate system: not asked.
        let v10 = r#"<WFS_Capabilities version="1.0.0"><FeatureTypeList><FeatureType><Name>roads</Name><SRS>EPSG:4326</SRS></FeatureType></FeatureTypeList></WFS_Capabilities>"#;
        assert_eq!(features_in_area(&offered("https://old.example.org/wfs", v10), piaui), None);
        // ArcGIS: an envelope in EPSG:4326, kept by the pages that follow (they add to the query).
        let arcgis = r#"{"currentVersion": 11.1, "layers": [{"id": 0, "name": "Parcels"}]}"#;
        let url = offered("https://x.org/arcgis/rest/services/A/FeatureServer", arcgis);
        let asked = features_in_area(&url, piaui).unwrap();
        assert_eq!(
            asked,
            format!(
                "{url}&geometry=-45.99,-10.93,-40.37,-2.74&geometryType=esriGeometryEnvelope&inSR=4326&spatialRel=esriSpatialRelIntersects"
            )
        );
        assert!(arcgis_page_url(&asked, 2, 2, Some("FID")).contains("&geometry=-45.99,-10.93,-40.37,-2.74&"));
        // OGC API Features: bbox, longitude first, before a fragment.
        assert_eq!(
            features_in_area("https://api.example.org/collections/a/items?limit=10000&f=json#x", piaui).unwrap(),
            "https://api.example.org/collections/a/items?limit=10000&f=json&bbox=-45.99,-10.93,-40.37,-2.74#x"
        );
        assert_eq!(
            features_in_area("https://api.example.org/collections/a/items", [-1.5, 0.0, 2.0, 1e-7]).unwrap(),
            "https://api.example.org/collections/a/items?bbox=-1.5,0,2,0.0000001"
        );
        // An items path in any case, and with a trailing slash.
        assert_eq!(
            features_in_area("https://api.example.org/Collections/A/Items/", piaui).unwrap(),
            "https://api.example.org/Collections/A/Items/?bbox=-45.99,-10.93,-40.37,-2.74"
        );
        assert!(!ends_with_items("/items-old") && !ends_with_items("tems") && ends_with_items("/x/ITEMS"));
        // Across the antimeridian, off the globe, already filtered, or not a feature request.
        assert_eq!(features_in_area("https://api.example.org/collections/a/items", [170.0, -20.0, -170.0, -10.0]), None);
        assert_eq!(features_in_area("https://api.example.org/collections/a/items", [-200.0, -20.0, 10.0, 95.0]), None);
        assert_eq!(features_in_area("https://api.example.org/collections/a/items?bbox=1,2,3,4", piaui), None);
        let other_crs = "https://api.example.org/collections/a/items?bbox-crs=http://www.opengis.net/def/crs/EPSG/0/31983";
        assert_eq!(features_in_area(other_crs, piaui), None);
        assert_eq!(features_in_area(&format!("{}&CQL_FILTER=uf%3D'PI'", offered("https://x.org/wfs", v11)), piaui), None);
        assert_eq!(features_in_area("https://x.org/data.geojson", piaui), None);
    }

    #[test]
    fn wfs_versions_and_axes() {
        // WFS 1.0: onlineResource, TYPENAME and MAXFEATURES.
        let v10 = r#"<WFS_Capabilities version="1.0.0"><Capability><Request><GetFeature><DCPType><HTTP><Get onlineResource="https://old.example.org/wfs?"/></HTTP></DCPType></GetFeature></Request></Capability>
<FeatureTypeList><FeatureType><Name>roads</Name><SRS>EPSG:4326</SRS></FeatureType></FeatureTypeList></WFS_Capabilities>"#;
        let s = parse("https://old.example.org/wfs", v10.as_bytes()).unwrap();
        assert!(
            matches!(&s.layers[0].offer, Offer::Features { url } if url.starts_with("https://old.example.org/wfs?SERVICE=WFS&VERSION=1.0.0&REQUEST=GetFeature&TYPENAME=roads&") && url.contains("SRSNAME=EPSG:4326&MAXFEATURES=")),
            "{:?}",
            s.layers[0].offer
        );
        // WFS 2.0 offering CRS84: asked for it, longitude first by definition.
        let v20 = r#"<WFS_Capabilities version="2.0.0"><FeatureTypeList><FeatureType><Name>a</Name><DefaultCRS>urn:ogc:def:crs:EPSG::25832</DefaultCRS><OtherCRS>urn:ogc:def:crs:OGC:1.3:CRS84</OtherCRS></FeatureType></FeatureTypeList></WFS_Capabilities>"#;
        let s = parse("https://x.org/wfs", v20.as_bytes()).unwrap();
        assert!(
            matches!(&s.layers[0].offer, Offer::Features { url } if url.contains("VERSION=2.0.0") && url.contains("TYPENAMES=a") && url.contains("SRSNAME=urn:ogc:def:crs:OGC:1.3:CRS84&COUNT=")),
            "{:?}",
            s.layers[0].offer
        );
    }

    /// #1053: an answer's axes are judged from its positions against the layer's bounds, never
    /// from the `crs` it names.
    #[test]
    fn axes_judged_against_the_layer_bounds() {
        // IBGE (GeoServer): longitude first, named urn:ogc:def:crs:EPSG::4326. Left alone.
        let ibge = br#"{"type":"FeatureCollection","features":[{"type":"Feature","id":"v.1","geometry":{"type":"MultiPolygon","coordinates":[[[[-61.9307472,-11.9734947],[-62.8940789,-13.1189419],[-61.886511,-11.827457],[-61.9307472,-11.9734947]]]]},"properties":{"nome":"Rondonia"}},{"type":"Feature","id":"v.2","geometry":{"type":"Point","coordinates":[-35.2,-5.8]},"properties":{}}],"crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::4326"}},"bbox":[-62.8940789,-13.1189419,-35.2,-5.8]}"#;
        let brazil = Some([-71.99, -30.51, -34.12, 1.10]);
        assert!(fix_axes(ibge, brazil).is_none());

        // Latitude first with no crs, as deegree answers when asked in the URN form (RustGIS asks
        // EPSG:4326). Turned round, a GeometryCollection too; a null geometry and a property
        // holding numbers are no positions.
        let hamburg = Some([8.41, 53.40, 10.33, 53.96]);
        let deegree = br#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"at":[0,0]},"geometry":{"type":"LineString","coordinates":[[53.55,9.99],[53.6,10.1]]}},{"type":"Feature","properties":{},"geometry":{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[53.45,8.5]}]}},{"type":"Feature","properties":{},"geometry":null}]}"#;
        let fixed: serde_json::Value = serde_json::from_slice(&fix_axes(deegree, hamburg).unwrap()).unwrap();
        assert_eq!(fixed.pointer("/features/0/geometry/coordinates/0").unwrap(), &serde_json::json!([9.99, 53.55]));
        assert_eq!(fixed.pointer("/features/1/geometry/geometries/0/coordinates").unwrap(), &serde_json::json!([8.5, 53.45]));
        assert_eq!(fixed.pointer("/features/0/properties/at").unwrap(), &serde_json::json!([0, 0]));
        // A crs naming the latitude-first order is dropped with the turn.
        let named = br#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[53.55,9.99]}}],"crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::4326"}}}"#;
        let fixed: serde_json::Value = serde_json::from_slice(&fix_axes(named, hamburg).unwrap()).unwrap();
        assert!(fixed.get("crs").is_none());
        // Boxes in the latitude-first order are dropped with it: the answer's, a feature's and a
        // geometry's, within a collection too.
        let boxed = br#"{"type":"FeatureCollection","bbox":[53.55,9.99,53.6,10.1],"features":[{"type":"Feature","bbox":[53.55,9.99,53.6,10.1],"properties":{"bbox":"kept"},"geometry":{"type":"GeometryCollection","bbox":[53.55,9.99,53.6,10.1],"geometries":[{"type":"LineString","bbox":[53.55,9.99,53.6,10.1],"coordinates":[[53.55,9.99],[53.6,10.1]]}]}}]}"#;
        let fixed: serde_json::Value = serde_json::from_slice(&fix_axes(boxed, hamburg).unwrap()).unwrap();
        for at in ["", "/features/0", "/features/0/geometry", "/features/0/geometry/geometries/0"] {
            assert!(fixed.pointer(at).unwrap().get("bbox").is_none(), "{at}");
        }
        assert_eq!(fixed.pointer("/features/0/properties/bbox").unwrap(), "kept");
        assert_eq!(
            fixed.pointer("/features/0/geometry/geometries/0/coordinates").unwrap(),
            &serde_json::json!([[9.99, 53.55], [10.1, 53.6]])
        );
        // A lone feature and a bare geometry.
        let feature =
            br#"{"type":"Feature","bbox":[53.55,9.99,53.55,9.99],"properties":{},"geometry":{"type":"Point","coordinates":[53.55,9.99]}}"#;
        let fixed: serde_json::Value = serde_json::from_slice(&fix_axes(feature, hamburg).unwrap()).unwrap();
        assert_eq!(fixed.pointer("/geometry/coordinates").unwrap(), &serde_json::json!([9.99, 53.55]));
        assert!(fixed.get("bbox").is_none());
        let geometry = br#"{"type":"Point","coordinates":[53.55,9.99,12.5]}"#;
        let fixed: serde_json::Value = serde_json::from_slice(&fix_axes(geometry, hamburg).unwrap()).unwrap();
        assert_eq!(fixed.pointer("/coordinates").unwrap(), &serde_json::json!([9.99, 53.55, 12.5]));

        // No bounds: it can't tell, and nothing is turned.
        assert!(fix_axes(deegree, None).is_none());

        // Fits both ways (near 10°E 10°N, or a layer covering the world): left alone.
        let both = br#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[9.8,10.1],[10.2,9.9]]}}]}"#;
        assert!(fix_axes(both, Some([9.5, 9.5, 10.5, 10.5])).is_none());
        assert!(fix_axes(deegree, Some([-180.0, -90.0, 180.0, 90.0])).is_none());
        // Fits neither way: left alone.
        assert!(fix_axes(deegree, brazil).is_none());

        // Slightly past the bounds, within the margin (a tenth of their size, or half a degree):
        // it fits as it comes, though only the turned point lies strictly inside.
        let past = br#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[12.3,11.0]}}]}"#;
        assert!(fix_axes(past, Some([10.0, 10.0, 12.0, 14.0])).is_none());

        // Nothing to judge from: no features, not JSON.
        assert!(fix_axes(br#"{"type":"FeatureCollection","features":[]}"#, hamburg).is_none());
        assert!(fix_axes(b"urn:ogc:def:crs:EPSG::4326 not json", hamburg).is_none());
        // A list of features isn't a GeoJSON answer: judged, but left as it came.
        let listed = br#"[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[53.55,9.99]}}]"#;
        assert!(fix_axes(listed, hamburg).is_none());
        // Positions under arrays nested deeper than a GeoJSON answer goes aren't read.
        let deep = format!("{}{{\"type\":\"Point\",\"coordinates\":[53.55,9.99]}}{}", "[".repeat(20), "]".repeat(20));
        assert!(fix_axes(deep.as_bytes(), hamburg).is_none());
    }

    /// An OGC API collection's extent is its longitude/latitude bounds only in CRS84 (named, or
    /// by default), with or without heights: a box in EPSG:4326's latitude-first order would turn
    /// longitude-first features round (#1053).
    #[test]
    fn ogc_api_extent_read_only_in_crs84() {
        let doc = br#"{"collections": [
            {"id": "a", "extent": {"spatial": {"bbox": [[8.41, 53.39, 10.34, 53.97]]}}, "links": [{"rel": "items", "href": "https://x.org/collections/a/items"}]},
            {"id": "b", "extent": {"spatial": {"bbox": [[8.41, 53.39, 10.34, 53.97]], "crs": "http://www.opengis.net/def/crs/OGC/1.3/CRS84"}}, "links": [{"rel": "items", "href": "https://x.org/collections/b/items"}]},
            {"id": "c", "extent": {"spatial": {"bbox": [[8.41, 53.39, -5, 10.34, 53.97, 120]], "crs": "http://www.opengis.net/def/crs/OGC/0/CRS84h"}}, "links": [{"rel": "items", "href": "https://x.org/collections/c/items"}]},
            {"id": "d", "extent": {"spatial": {"bbox": [[53.39, 8.41, 53.97, 10.34]], "crs": "http://www.opengis.net/def/crs/EPSG/0/4326"}}, "links": [{"rel": "items", "href": "https://x.org/collections/d/items"}]},
            {"id": "e", "extent": {"spatial": {"bbox": [[8.41, 9.0, 53.39, 10.34, 53.97]]}}, "links": [{"rel": "items", "href": "https://x.org/collections/e/items"}]}]}"#;
        let s = parse("https://x.org/collections", doc).unwrap();
        let bbox = |id: &str| s.layers.iter().find(|l| l.name == id).map(|l| l.bbox);
        let hamburg = Some(Some([8.41, 53.39, 10.34, 53.97]));
        assert_eq!((bbox("a"), bbox("b"), bbox("c")), (hamburg, hamburg, hamburg));
        // A box in another system, or of neither four nor six numbers: no bounds.
        assert_eq!((bbox("d"), bbox("e")), (Some(None), Some(None)));
    }

    #[test]
    fn partial_answers_pointed_out() {
        assert!(
            partial_note(br#"{"type":"FeatureCollection","features":[],"properties":{"exceededTransferLimit":true}}"#)
                .is_some_and(|n| n.contains("record limit"))
        );
        assert!(
            partial_note(br#"{"type":"FeatureCollection","numberMatched": 25000, "numberReturned":10000,"features":[]}"#)
                .is_some_and(|n| n.contains("10000 of its 25000"))
        );
        assert!(partial_note(br#"{"type":"FeatureCollection","features":[],"links":[{"rel":"next","href":"x"}]}"#).is_some());
        assert!(
            partial_note(
                br#"{"type":"FeatureCollection","numberMatched":3,"numberReturned":3,"features":[],"exceededTransferLimit":false}"#
            )
            .is_none()
        );
        assert!(partial_note(b"").is_none());
    }

    /// A WFS server can cap a page without linking to the next one (#1262).
    #[test]
    fn wfs_pages_without_next_links_are_read_completely() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature&TYPENAMES=roads&COUNT=50000";
        let first = br#"{"type":"FeatureCollection","features":[{"id":"roads.1"},{"id":"roads.2"}]}"#;
        let (joined, note) = join_pages(url, first.to_vec(), 100, |next| {
            if query_value(next, "RESULTTYPE").is_some_and(|v| v.eq_ignore_ascii_case("hits")) {
                Ok(br#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" numberMatched="3" numberReturned="0"/>"#.to_vec())
            } else {
                assert_eq!(query_value(next, "STARTINDEX"), Some("2"));
                Ok(br#"{"type":"FeatureCollection","features":[{"id":"roads.3"}]}"#.to_vec())
            }
        });
        let doc: serde_json::Value = serde_json::from_slice(&joined).unwrap();
        assert_eq!(doc["features"].as_array().unwrap().len(), 3);
        assert_eq!(note, None);
    }

    /// Explicit count windows, below and above the generated request size, do not fetch more.
    #[test]
    fn wfs_explicit_count_windows_do_not_page() {
        let first = br#"{"features":[{"id":1},{"id":2}]}"#;
        let mut requests = Vec::new();
        for count in [2, FEATURE_LIMIT + 1] {
            let url = format!("https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature&COUNT={count}");
            let mut calls = 0;
            let _ = join_pages(&url, first.to_vec(), 100, |_| {
                calls += 1;
                Err("no extra request expected".into())
            });
            requests.push(calls);
        }
        assert_eq!(requests, [0, 0]);
    }

    /// A total reported only by an intermediate linked page remains evidence of a short read.
    #[test]
    fn wfs_legacy_intermediate_total_is_not_forgotten() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=1.1.0&REQUEST=GetFeature&MAXFEATURES=50000";
        let first = br#"{"features":[{"id":1}],"links":[{"rel":"next","href":"?page=2"}]}"#;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |u| {
            if query_value(u, "page") == Some("2") {
                Ok(br#"{"features":[{"id":2}],"totalFeatures":4,"links":[{"rel":"next","href":"?page=3"}]}"#.to_vec())
            } else {
                Ok(br#"{"features":[{"id":3}]}"#.to_vec())
            }
        });
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["features"].as_array().unwrap().len(), 3);
        assert!(note.unwrap().contains("3 of 4 WFS features read"));
    }

    /// Exhausted legacy links report a known shortfall, while a matching total stays clean.
    #[test]
    fn wfs_legacy_linked_total_is_not_silent() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=1.1.0&REQUEST=GetFeature&MAXFEATURES=50000";
        for total in [3, 2] {
            let first = serde_json::to_vec(&serde_json::json!({
                "features": [{"id": 1}], "totalFeatures": total,
                "links": [{"rel": "next", "href": "?page=2"}]
            }))
            .unwrap();
            let (bytes, note) = join_pages(url, first, 100, |_| Ok(br#"{"features":[{"id":2}]}"#.to_vec()));
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["features"].as_array().unwrap().len(), 2);
            if total == 3 {
                assert!(note.unwrap().contains("2 of 3 WFS features read"));
            } else {
                assert_eq!(note, None);
            }
        }
    }

    /// A matching legacy WFS total confirms completeness even at the requested cap.
    #[test]
    fn wfs_legacy_matching_total_at_cap_is_complete() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=1.1.0&REQUEST=GetFeature&MAXFEATURES=2";
        let first = br#"{"features":[{"id":1},{"id":2}],"totalFeatures":2}"#;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |_| panic!("legacy WFS has no offset paging"));
        assert_eq!(bytes, first);
        assert_eq!(note, None);
    }

    /// A reported legacy WFS total exposes truncation below the requested cap.
    #[test]
    fn wfs_legacy_reported_total_is_not_silent() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=1.1.0&REQUEST=GetFeature&MAXFEATURES=50000";
        let first = br#"{"features":[{"id":1},{"id":2}],"totalFeatures":3}"#;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |_| panic!("legacy WFS has no offset paging"));
        assert_eq!(bytes, first);
        let note = note.unwrap();
        assert!(note.contains("2 of 3 WFS features read"));
        assert!(note.contains("incomplete"));
        let complete = br#"{"features":[{"id":1},{"id":2}],"totalFeatures":2}"#;
        let (bytes, note) = join_pages(url, complete.to_vec(), 100, |_| panic!("legacy WFS has no offset paging"));
        assert_eq!(bytes, complete);
        assert_eq!(note, None);
    }

    /// Legacy WFS at its requested cap warns, while a shorter response stays clean.
    #[test]
    fn wfs_legacy_requested_cap_is_not_silent() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=1.1.0&REQUEST=GetFeature&MAXFEATURES=2";
        let first = br#"{"features":[{"id":1,"properties":{"next":null}},{"id":2}]}"#;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |_| panic!("legacy WFS has no offset paging"));
        assert_eq!(bytes, first);
        assert!(note.unwrap().contains("2"));
        let short = br#"{"features":[{"id":1}]}"#;
        let (bytes, note) = join_pages(url, short.to_vec(), 100, |_| panic!("legacy WFS has no offset paging"));
        assert_eq!(bytes, short);
        assert_eq!(note, None);
    }

    /// A WFS next-link chain still checks its total when the final link disappears.
    #[test]
    fn wfs_linked_pages_check_the_final_total() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature";
        let first = br#"{"features":[{"id":1}],"numberMatched":3,"links":[{"rel":"next","href":"?page=2"}]}"#;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |u| {
            if query_value(u, "RESULTTYPE").is_some() {
                Ok(br#"<FeatureCollection numberMatched="3"/>"#.to_vec())
            } else if query_value(u, "STARTINDEX") == Some("2") {
                Ok(br#"{"features":[{"id":3}]}"#.to_vec())
            } else {
                Ok(br#"{"features":[{"id":2}],"numberMatched":3}"#.to_vec())
            }
        });
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["features"].as_array().unwrap().len(), 3);
        assert_eq!(note, None);
    }

    /// Numeric service IDs beyond u64 stay distinct across pages.
    #[test]
    fn wfs_large_numeric_ids_remain_distinct() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature";
        let first = br#"{"features":[{"id":12345678901234567890123}]}"#;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |u| {
            if query_value(u, "RESULTTYPE").is_some() {
                Ok(br#"<FeatureCollection numberMatched="2"/>"#.to_vec())
            } else {
                Ok(br#"{"features":[{"id":12345678901234567890124}]}"#.to_vec())
            }
        });
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["features"].as_array().unwrap().len(), 2);
        assert_eq!(note, None);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("12345678901234567890123") && text.contains("12345678901234567890124"));
    }

    /// Identical ID-less page starts do not imply that the entire page was repeated.
    #[test]
    fn wfs_idless_pages_with_matching_starts_continue() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature";
        let first = br#"{"features":[{"properties":{"n":0}},{"properties":{"n":1}}]}"#;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |u| match query_value(u, "STARTINDEX") {
            Some("2") => Ok(br#"{"features":[{"properties":{"n":0}},{"properties":{"n":2}}]}"#.to_vec()),
            Some("4") => Ok(br#"{"features":[{"properties":{"n":0}},{"properties":{"n":3}}]}"#.to_vec()),
            _ => Ok(br#"<FeatureCollection numberMatched="6"/>"#.to_vec()),
        });
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["features"].as_array().unwrap().len(), 6);
        let note = note.unwrap();
        assert!(note.contains("possible overlap"));
        assert!(!note.contains("rest aren't read"));
    }

    /// Removing a repeated service id remains visible even when the unique total matches.
    #[test]
    fn wfs_duplicate_ids_are_counted_when_the_total_matches() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature";
        let first = br#"{"features":[{"id":1},{"id":2}]}"#;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |u| {
            if query_value(u, "RESULTTYPE").is_some() {
                Ok(br#"<FeatureCollection numberMatched="4"/>"#.to_vec())
            } else {
                Ok(br#"{"features":[{"id":2},{"id":3},{"id":4}]}"#.to_vec())
            }
        });
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["features"].as_array().unwrap().len(), 4);
        assert!(note.unwrap().contains("1 repeated service-id row"));
    }

    #[test]
    fn wfs_review_exact_limit_can_be_complete() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature";
        let page = |rows: serde_json::Value| serde_json::to_vec(&serde_json::json!({"features": rows})).unwrap();
        let first = page(serde_json::json!([{"id": 1}, {"id": 2}]));
        let (bytes, note) = join_pages(url, first.clone(), 2, |_| Ok(br#"<FeatureCollection numberMatched="2"/>"#.to_vec()));
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["features"].as_array().unwrap().len(), 2);
        assert_eq!(note, None);
    }

    #[test]
    fn wfs_review_padded_total_continues_paging() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature";
        let page = |rows: serde_json::Value| serde_json::to_vec(&serde_json::json!({"features": rows})).unwrap();
        let first = page(serde_json::json!([{"id": 1}, {"id": 2}]));
        let (bytes, note) = join_pages(url, first, 100, |u| {
            if query_value(u, "RESULTTYPE").is_some() {
                Ok(br#"<FeatureCollection numberMatched=" 3 "/>"#.to_vec())
            } else {
                Ok(page(serde_json::json!([{"id": 3}])))
            }
        });
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["features"].as_array().unwrap().len(), 3);
        assert_eq!(note, None);
    }

    #[test]
    fn wfs_review_idless_overlap_reports_uncertainty() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature";
        let page = |rows: serde_json::Value| serde_json::to_vec(&serde_json::json!({"features": rows})).unwrap();
        let first = page(serde_json::json!([{"properties": {"n": 1}}, {"id": 2}]));
        let (bytes, note) = join_pages(url, first, 100, |u| match query_value(u, "STARTINDEX") {
            Some("2") => Ok(page(serde_json::json!([{"id": 3}, {"id": 4}]))),
            Some("4") => Ok(page(serde_json::json!([{"id": 5}, {"properties": {"n": 1}}]))),
            _ => Ok(br#"<FeatureCollection numberMatched="6"/>"#.to_vec()),
        });
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["features"].as_array().unwrap().len(), 6);
        assert!(note.unwrap().contains("possible overlap"));
    }

    #[test]
    fn wfs_review_mixed_repeat_keeps_new_identified_rows() {
        let url = "https://wfs.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature";
        let page = |rows: serde_json::Value| serde_json::to_vec(&serde_json::json!({"features": rows})).unwrap();
        let first = page(serde_json::json!([{"id": 1}, {"properties": {"n": 2}}]));
        let (bytes, note) = join_pages(url, first, 100, |u| {
            if query_value(u, "RESULTTYPE").is_some() {
                Ok(br#"<FeatureCollection numberMatched="5"/>"#.to_vec())
            } else {
                Ok(page(serde_json::json!([{"id": 1}, {"properties": {"n": 2}}, {"id": 3}])))
            }
        });
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(doc["features"].as_array().unwrap().iter().any(|f| f["id"] == 3));
        assert_eq!(doc["features"].as_array().unwrap().len(), 4);
        assert!(note.is_some());
    }

    /// Paging preserves the selection query and reports each way a service can stop short.
    #[test]
    fn wfs_paging_keeps_filters_and_reports_incomplete_reads() {
        let url = "https://wfs.example.org/wfs?service=wfs&version=2.0.0&request=GetFeature&typeNames=roads&outputFormat=json&count=50000&BBOX=1,2,3,4&token=t";
        let first = br#"{"type":"FeatureCollection","features":[{"id":1},{"id":2}]}"#;
        let hits = br#"<FeatureCollection numberMatched="4"/>"#;
        let count = |bytes: &[u8]| serde_json::from_slice::<serde_json::Value>(bytes).unwrap()["features"].as_array().unwrap().len();
        let mut calls = 0;
        let (bytes, note) = join_pages(url, first.to_vec(), 100, |next| {
            calls += 1;
            assert_eq!(query_value(next, "BBOX"), Some("1,2,3,4"));
            assert_eq!(query_value(next, "token"), Some("t"));
            if calls == 1 {
                assert_eq!(query_value(next, "RESULTTYPE"), Some("hits"));
                assert!(query_value(next, "OUTPUTFORMAT").is_none());
                Ok(hits.to_vec())
            } else {
                assert_eq!(query_value(next, "STARTINDEX"), Some("2"));
                assert_eq!(query_value(next, "COUNT"), Some("2"));
                assert_eq!(query_value(next, "OUTPUTFORMAT"), Some("json"));
                Ok(br#"{"features":[{"id":3},{"id":4}]}"#.to_vec())
            }
        });
        assert_eq!((count(&bytes), note, calls), (4, None, 2));
        for answer in
            [Err("timeout".into()), Ok(b"<ExceptionReport/>".to_vec()), Ok(br#"<FeatureCollection numberMatched="unknown"/>"#.to_vec())]
        {
            let (bytes, note) = join_pages(url, first.to_vec(), 100, |_| answer.clone());
            assert_eq!(count(&bytes), 2);
            let note = note.unwrap();
            assert!(note.contains("incomplete"));
            // The failed request's reason is kept.
            assert_eq!(note.contains("(timeout)"), answer.is_err(), "{note}");
        }
        // A credential in the failed request's address isn't repeated in the notice.
        {
            let failed = Err::<Vec<u8>, String>("error sending request for url (https://x.test/wfs?token=abc123)".to_string());
            let (_, note) = join_pages(url, first.to_vec(), 100, |_| failed.clone());
            let note = note.unwrap();
            assert!(note.contains("incomplete") && !note.contains("abc123"), "{note}");
        }
        for last in [
            Err("timeout".into()),
            Ok(br#"{"features":[]}"#.to_vec()),
            Ok(first.to_vec()),
            Ok(br#"{"features":[{"id":2},{"id":3}]}"#.to_vec()),
        ] {
            let mut calls = 0;
            let (bytes, note) = join_pages(url, first.to_vec(), 100, |_| {
                calls += 1;
                if calls == 1 { Ok(hits.to_vec()) } else { last.clone() }
            });
            let n = count(&bytes);
            assert!((2..=3).contains(&n));
            assert!(note.unwrap().contains("of 4 WFS features read"));
            assert_eq!(calls, 2);
        }
        let mut calls = 0;
        let (bytes, note) = join_pages(url, first.to_vec(), 3, |_| {
            calls += 1;
            if calls == 1 { Ok(hits.to_vec()) } else { Ok(br#"{"features":[{"id":3},{"id":4}]}"#.to_vec()) }
        });
        assert_eq!(count(&bytes), 3);
        let note = note.unwrap();
        assert!(note.contains("the first 3 of 4 WFS features are read"), "{note}");
        assert!(!note.contains("WFS features read:"), "{note}");
        let (bytes, note) = join_pages(url, first.to_vec(), 1, |_| panic!("limit checked before hits"));
        assert_eq!(count(&bytes), 1);
        let note = note.unwrap();
        assert!(note.contains("first 1"), "{note}");
        assert!(!note.contains("WFS features read:"), "{note}");
        for suffix in ["&STARTINDEX=0", "&RESULTTYPE=hits"] {
            let (bytes, note) =
                join_pages(&format!("{url}{suffix}"), first.to_vec(), 100, |_| panic!("explicit request must stay unchanged"));
            assert_eq!((bytes, note), (first.to_vec(), None));
        }
    }

    #[test]
    fn pages_joined_up_to_a_limit() {
        // Page n holds features 2n and 2n + 1; its JSON `next` link is relative.
        let page = |n: u64, next: Option<&str>| {
            let mut p = serde_json::json!({"type": "FeatureCollection", "numberMatched": 6, "numberReturned": 2,
                "features": [{"id": 2 * n}, {"id": 2 * n + 1}]});
            if let Some(h) = next {
                p["links"] = serde_json::json!([{"rel": "next", "type": "text/html", "href": "x.html"},
                    {"rel": "next", "type": "application/geo+json", "href": h}]);
            }
            serde_json::to_vec(&p).unwrap()
        };
        let url = "https://api.example.org/collections/a/items?limit=2";
        let mut get = |u: &str| match u {
            "https://api.example.org/collections/a/items?page=1" => Ok(page(1, Some("items?page=2"))),
            "https://api.example.org/collections/a/items?page=2" => Ok(page(2, None)),
            "https://api.example.org/collections/a/items?page=9" => Ok(page(9, Some(url))),
            _ => Err("http status: 404".to_string()),
        };
        let ids = |b: &[u8]| -> Vec<u64> {
            let v: serde_json::Value = serde_json::from_slice(b).unwrap();
            v["features"].as_array().unwrap().iter().map(|f| f["id"].as_u64().unwrap()).collect()
        };
        let (joined, note) = join_pages(url, page(0, Some("items?page=1")), 100, &mut get);
        assert_eq!((ids(&joined), note), (vec![0, 1, 2, 3, 4, 5], None));
        let v: serde_json::Value = serde_json::from_slice(&joined).unwrap();
        assert!(v["type"] == "FeatureCollection" && v.get("links").is_none() && v.get("numberMatched").is_none());
        assert!(partial_note(&joined).is_none());
        let (joined, note) = join_pages(url, page(0, Some("items?page=1")), 3, &mut get);
        assert_eq!(ids(&joined), [0, 1, 2]);
        assert!(note.is_some_and(|n| n.contains("first 3 features")));
        let (joined, note) = join_pages(url, page(0, Some("items?page=9")), 100, &mut get);
        assert_eq!(ids(&joined), [0, 1, 18, 19]);
        assert!(note.is_some_and(|n| n.contains("lead back")));
        let (joined, note) = join_pages(url, page(0, Some("items?page=4")), 100, &mut get);
        assert_eq!(ids(&joined), [0, 1]);
        assert!(note.is_some_and(|n| n.contains("after feature 2 couldn't be read (http status: 404)")));
        let unasked = |_: &str| -> Result<Vec<u8>, String> { panic!("no page is asked for") };
        let (joined, note) = join_pages(url, page(0, Some("items?page=1")), 1, unasked);
        assert_eq!(ids(&joined), [0]);
        assert!(note.is_some_and(|n| n.contains("first 1 features")));
        // One page, or a `next` link that isn't JSON: the answer as it came.
        let single = page(0, None);
        assert_eq!(join_pages(url, single.clone(), 100, unasked), (single, None));
        let html_only = br#"{"type":"FeatureCollection","features":[],"links":[{"rel":"next","type":"text/html","href":"p2"}]}"#;
        assert_eq!(join_pages(url, html_only.to_vec(), 100, unasked), (html_only.to_vec(), None));
    }

    /// The pages' features are joined as the server wrote them, not read into values and written
    /// again (#830): a feature costs its text while the pages are read. Spacing, member order, a
    /// trailing zero, an escaped letter and an integer past `u64` all come through; the first
    /// page's other members stay, and a server's first page sent again, spaced differently, is
    /// still recognised.
    #[test]
    fn pages_joined_as_written() {
        let url = "https://api.example.org/collections/a/items";
        let features = [
            r#"{ "type": "Feature", "id": 1, "properties": { "area": 1.50, "parcel": 12345678901234567890123 }, "geometry": null }"#,
            r#"{"properties":{"name":"Caf\u00e9 \"Z\""},"type":"Feature","id":2,"geometry":{"type":"Point","coordinates":[1.0,2e1]}}"#,
            r#"{"type":"Feature","id":3,"properties":{},"geometry":null}"#,
        ];
        let first = format!(
            r#"{{"type":"FeatureCollection","name":"Parcels","numberMatched":3,"features":[{},{}],"links":[{{"rel":"next","href":"items?page=1"}}]}}"#,
            features[0], features[1]
        );
        let second = format!(r#"{{"type":"FeatureCollection","features":[ {} ]}}"#, features[2]);
        let (joined, note) = join_pages(url, first.into_bytes(), 100, |_| Ok(second.clone().into_bytes()));
        assert_eq!(note, None);
        let text = String::from_utf8(joined).unwrap();
        assert!(text.contains(&format!("[{}]", features.join(","))), "{text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["type"] == "FeatureCollection" && v["name"] == "Parcels" && v.get("links").is_none() && v.get("numberMatched").is_none());

        // An ArcGIS server ignoring `resultOffset`, its first page sent again with other spacing.
        let query = "https://x.org/arcgis/rest/services/P/FeatureServer/0/query?where=1%3D1&f=geojson";
        let page = |sep: &str| {
            format!(r#"{{"type":"FeatureCollection","features":[{{"id":0,{sep}"x":1}}],"properties":{{"exceededTransferLimit":true}}}}"#)
        };
        let layer = br#"{"objectIdField":"OBJECTID","maxRecordCount":1,"advancedQueryCapabilities":{"supportsPagination":true}}"#;
        let mut sent = 0;
        let (joined, note) = join_pages(query, page("").into_bytes(), 100, |u| {
            if !u.contains("resultOffset") {
                return Ok(layer.to_vec());
            }
            sent += 1;
            Ok(page(if sent == 1 { "" } else { " " }).into_bytes())
        });
        assert!(note.is_some_and(|n| n.contains("first features again")));
        assert_eq!(String::from_utf8(joined).unwrap().matches(r#""id":0"#).count(), 1);

        // A page nested deeper than values may be stops the reading, as it did read into values;
        // brackets in a string don't count.
        let nested = |n: usize| format!(r#"{{"features":[{{"id":9,"g":{}1{}}}]}}"#, "[".repeat(n), "]".repeat(n));
        let first = format!(r#"{{"features":[{{"id":1,"s":"{}"}}],"links":[{{"rel":"next","href":"p"}}]}}"#, "[".repeat(200));
        for (n, read) in [(120, true), (130, false)] {
            let (joined, note) = join_pages(url, first.clone().into_bytes(), 100, |_| Ok(nested(n).into_bytes()));
            assert_eq!(String::from_utf8(joined).unwrap().contains(r#""id":9"#), read, "{n}");
            assert_eq!(note.is_some_and(|n| n.contains("recursion limit")), !read, "{n}");
        }
    }

    /// An ArcGIS layer whose description lists protocol buffers is read in `f=pbf` pages, in the
    /// order of its object ids, into one layer (#1326); [`begin`] keeps reading it in GeoJSON. A
    /// first page that can't be fetched, or comes but can't be read as one, has the layer read in
    /// GeoJSON pages; a later one that fails, a server that
    /// sends its first page again, a page of other fields, coordinate system or geometry type and
    /// the limit end the read with the pages read so far, and a note. What the pages' features lost is said once, summed.
    #[test]
    fn arcgis_pbf_pages_joined_when_the_layer_lists_them() {
        use crate::esripbf::tests::{page as pbf, page_with_z as pbf_z};
        let query = "https://x.org/arcgis/rest/services/P/FeatureServer/0/query?where=1%3D1&outFields=*&f=geojson&token=t";
        let description = "https://x.org/arcgis/rest/services/P/FeatureServer/0?f=json&token=t";
        let layer = br#"{"objectIdField": "OBJECTID", "maxRecordCount": 2, "supportedQueryFormats": "JSON, geoJSON, PBF",
            "advancedQueryCapabilities": {"supportsPagination": true}}"#;
        let offset = |u: &str| query_value(u, "resultOffset").and_then(|v| v.parse::<u64>().ok());
        // Five features in pages of two, each cut off unless it reaches the last.
        let pbf_page = |u: &str| offset(u).map(|o| pbf(&(o..(o + 2).min(5)).collect::<Vec<_>>(), o + 2 < 5, "OBJECTID"));
        let json_page = |u: &str| {
            let o = offset(u)?;
            let features: Vec<_> = (o..(o + 2).min(5)).map(|i| serde_json::json!({"type": "Feature", "id": i, "properties": {}})).collect();
            let mut p = serde_json::json!({"type": "FeatureCollection", "features": features});
            if o + 2 < 5 {
                p["properties"] = serde_json::json!({"exceededTransferLimit": true});
            }
            serde_json::to_vec(&p).ok()
        };
        // Reads to the end, answering by address and keeping the addresses asked for.
        let run = |pbf_reads: bool, max: usize, answer: &dyn Fn(&str) -> Result<Vec<u8>, String>| {
            let mut asked = Vec::new();
            let mut step = if pbf_reads { begin_with_pbf(query, max) } else { begin(query, max) };
            loop {
                match step {
                    Step::Fetch(pages) => {
                        asked.push(pages.next_url().to_string());
                        let a = if pages.next_url() == description { Ok(layer.to_vec()) } else { answer(pages.next_url()) };
                        step = (*pages).land(a);
                    }
                    other => return (other, asked),
                }
            }
        };
        let ids = |ds: &crate::Dataset| -> Vec<f64> { ds.features.iter().filter_map(|f| f.attrs.first()?.as_f64()).collect() };
        let pbf_server = |u: &str| {
            if query_value(u, "f") == Some("pbf") { pbf_page(u).ok_or_else(|| "404".to_string()) } else { Err("not asked".into()) }
        };

        // The pages as protocol buffers, from offset 0, into one layer.
        let (step, asked) = run(true, 100, &pbf_server);
        let Step::Decoded(ds, report, note) = step else { panic!("{step:?}") };
        assert_eq!((ids(&ds), note, report.messages.len()), (vec![0.0, 1.0, 2.0, 3.0, 4.0], None, 0));
        assert_eq!(ds.features.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 3, 4, 5], "numbered on");
        assert_eq!(
            asked[1],
            format!("{}&f=pbf", query.replace("&f=geojson", "") + "&resultOffset=0&resultRecordCount=2&orderByFields=OBJECTID")
        );
        assert!(asked[2].contains("resultOffset=2&") && asked[3].contains("resultOffset=4&") && asked.len() == 4, "{asked:?}");
        assert!(asked[1..].iter().all(|u| query_value(u, "f") == Some("pbf") && query_value(u, "token") == Some("t")), "{asked:?}");

        // `begin` reads the same layer in GeoJSON.
        let (step, asked) = run(false, 100, &|u: &str| json_page(u).ok_or_else(|| "404".to_string()));
        assert!(matches!(step, Step::Done(_, None)), "{step:?}");
        assert!(asked[1..].iter().all(|u| query_value(u, "f") == Some("geojson")), "{asked:?}");

        // A first page refused as a protocol buffer: GeoJSON pages from the start.
        let (step, asked) = run(true, 100, &|u: &str| {
            if query_value(u, "f") == Some("pbf") {
                Ok(br#"{"error": {"code": 400}}"#.to_vec())
            } else {
                json_page(u).ok_or_else(|| "404".to_string())
            }
        });
        let Step::Done(joined, Some(note)) = step else { panic!("{step:?}") };
        assert!(note.starts_with("its protocol buffer pages couldn't be read (") && note.ends_with("so it is read in GeoJSON"), "{note}");
        let v: serde_json::Value = serde_json::from_slice(&joined).unwrap();
        assert_eq!(v["features"].as_array().map(Vec::len), Some(5));
        assert_eq!(asked.len(), 5, "the description, the refused page, then three in GeoJSON: {asked:?}");
        assert!(asked[2].contains("resultOffset=0&") && query_value(&asked[2], "f") == Some("geojson"), "{asked:?}");

        // A query that asks for protocol buffers itself is read in GeoJSON pages, when its first
        // page can't be read as one.
        let pbf_query = query.replace("f=geojson", "f=pbf");
        let mut asked = Vec::new();
        let mut step = begin_with_pbf(&pbf_query, 100);
        while let Step::Fetch(pages) = step {
            let u = pages.next_url().to_string();
            let a = if u == description {
                Ok(layer.to_vec())
            } else if query_value(&u, "f") == Some("pbf") {
                Ok(br#"{"error": {"code": 400}}"#.to_vec())
            } else {
                json_page(&u).ok_or_else(|| "404".to_string())
            };
            asked.push(u);
            step = (*pages).land(a);
        }
        let Step::Done(joined, Some(note)) = step else { panic!("{step:?}") };
        assert!(note.starts_with("its protocol buffer pages couldn't be read (") && note.ends_with("so it is read in GeoJSON"), "{note}");
        let v: serde_json::Value = serde_json::from_slice(&joined).unwrap();
        assert_eq!(v["features"].as_array().map(Vec::len), Some(5));
        assert!(asked.len() == 5 && asked[2..].iter().all(|u| query_value(u, "f") == Some("geojson")), "{asked:?}");

        // A first page that can't be fetched: GeoJSON pages from the start too, the whole layer
        // (the query as it is would hold its first page only).
        let (step, asked) = run(true, 100, &|u: &str| {
            if query_value(u, "f") == Some("pbf") { Err("http status: 500".into()) } else { json_page(u).ok_or_else(|| "404".to_string()) }
        });
        let Step::Done(joined, Some(note)) = step else { panic!("{step:?}") };
        assert!(note.starts_with("its protocol buffer pages couldn't be read (") && note.ends_with("so it is read in GeoJSON"), "{note}");
        let v: serde_json::Value = serde_json::from_slice(&joined).unwrap();
        assert_eq!(v["features"].as_array().map(Vec::len), Some(5));
        assert!(asked.len() == 5 && asked[2..].iter().all(|u| query_value(u, "f") == Some("geojson")), "{asked:?}");
        assert!(!asked.iter().any(|u| u == query), "{asked:?}");
        assert!(note.contains("(http status: 500)"), "{note}");
        // A long reason is said cut.
        let (step, _) = run(true, 100, &|u: &str| {
            if query_value(u, "f") == Some("pbf") { Err("x".repeat(1000)) } else { json_page(u).ok_or_else(|| "404".to_string()) }
        });
        let Step::Done(_, Some(note)) = step else { panic!("{step:?}") };
        assert!(note.len() < 300 && note.contains("x…)"), "{note}");
        // A credential in the reason isn't said (an address's query isn't, nor what follows it).
        let (step, _) = run(true, 100, &|u: &str| {
            if query_value(u, "f") == Some("pbf") {
                Err("timed out: https://x.org/q?token=hidden".into())
            } else {
                json_page(u).ok_or_else(|| "404".to_string())
            }
        });
        let Step::Done(_, Some(note)) = step else { panic!("{step:?}") };
        assert!(!note.contains("hidden") && note.contains("(timed out: https://x.org/q"), "{note}");

        // A later page that fails: the pages read, said.
        let (step, _) = run(true, 100, &|u: &str| if offset(u) == Some(2) { Err("http status: 500".into()) } else { pbf_server(u) });
        let Step::Decoded(ds, _, Some(note)) = step else { panic!("{step:?}") };
        assert_eq!(ids(&ds), [0.0, 1.0]);
        assert!(note.contains("after feature 2 couldn't be read"), "{note}");

        // A server that ignores the offset: its first page isn't read twice.
        let (step, _) = run(true, 100, &|u: &str| pbf_server(&u.replace("resultOffset=2&", "resultOffset=0&")));
        let Step::Decoded(ds, _, Some(note)) = step else { panic!("{step:?}") };
        assert_eq!(ids(&ds), [0.0, 1.0]);
        assert!(note.contains("first features again"), "{note}");

        // A page of other fields isn't joined.
        let (step, _) = run(true, 100, &|u: &str| {
            if offset(u) == Some(2) { Ok(pbf(&[2, 3], true, "FID")) } else { pbf_server(u) }
        });
        let Step::Decoded(ds, _, Some(note)) = step else { panic!("{step:?}") };
        assert_eq!(ds.features.len(), 2);
        assert!(note.contains("other fields"), "{note}");

        // The limit cuts it short, with a note.
        let (step, _) = run(true, 3, &pbf_server);
        let Step::Decoded(ds, _, Some(note)) = step else { panic!("{step:?}") };
        assert_eq!(ids(&ds), [0.0, 1.0, 2.0]);
        assert!(note.contains("first 3 features"), "{note}");
        // A page cut at the limit: what the features left out lost isn't counted.
        let z_server = |u: &str| -> Result<Vec<u8>, String> {
            let o = offset(u).ok_or_else(|| "404".to_string())?;
            Ok(pbf_z(&(o..(o + 2).min(5)).collect::<Vec<_>>(), o + 2 < 5, "OBJECTID"))
        };
        let (step, _) = run(true, 3, &z_server);
        let Step::Decoded(ds, report, Some(note)) = step else { panic!("{step:?}") };
        assert_eq!(ids(&ds), [0.0, 1.0, 2.0]);
        assert!(note.contains("first 3 features"), "{note}");
        assert_eq!(report.messages, [crate::zm_note(Some(3))]);

        let fresh = || Pages {
            url: query.into(),
            doc: serde_json::Value::Null,
            features: Vec::new(),
            max: 100,
            seen: std::collections::HashSet::new(),
            next: String::new(),
            from: String::new(),
            phase: Phase::Offsets { size: 2, order: None, start: None, first: None, pbf: true },
            decoded: None,
            decoded_bytes: 0,
            decoded_budget: max_decoded_bytes(),
            pbf_failed: None,
        };
        // A page in another coordinate system, or of another geometry type, isn't joined; one
        // that doesn't say its system is.
        let page = |ids: &[u64], crs: Option<crate::raster::Crs>, geometry_type: crate::GeometryType| {
            let mut page = crate::esripbf::read_page(&pbf(ids, true, "OBJECTID"), "query").unwrap();
            page.dataset.crs = crs;
            page.dataset.geometry_type = geometry_type;
            page
        };
        let (lonlat, point) = (Some(crate::raster::Crs::LonLat), crate::GeometryType::Point);
        for (crs, geometry_type, stopped) in [
            (Some(crate::raster::Crs::WebMercator), point, Some("another coordinate system")),
            (lonlat, crate::GeometryType::Polyline, Some("another geometry type")),
            (None, point, None),
            (lonlat, crate::GeometryType::None, None),
        ] {
            let Step::Fetch(first) = fresh().join_page(page(&[0, 1], lonlat, point)) else { panic!("the first page") };
            match ((*first).join_page(page(&[2, 3], crs, geometry_type)), stopped) {
                (Step::Decoded(ds, _, Some(note)), Some(what)) => {
                    assert_eq!(ds.features.len(), 2);
                    assert!(note.contains(what), "{note}");
                }
                (Step::Fetch(p), None) => assert_eq!(p.read(), 4),
                (other, _) => panic!("{crs:?} {geometry_type:?}: {other:?}"),
            }
        }
        // An empty object id doesn't mark a page: two pages that start with one are both read.
        let blank = |ids: &[u64]| {
            let mut p = page(ids, lonlat, point);
            let i = p.object_id_field.unwrap();
            p.dataset.features[0].attrs[i] = crate::Value::Null;
            p
        };
        let Step::Fetch(first) = fresh().join_page(blank(&[0, 1])) else { panic!("the first page") };
        let Step::Fetch(p) = (*first).join_page(blank(&[2, 3])) else { panic!("an empty object id taken for a page sent again") };
        assert_eq!(p.read(), 4);
        // A page without object ids sent again is told so, even cut shorter by the limit.
        let idless = |limit: usize| {
            let mut p = crate::esripbf::read_page_within(&pbf(&[0, 1], true, "OBJECTID"), "query", limit).unwrap();
            p.object_id_field = None;
            p
        };
        let Step::Fetch(first) = fresh().join_page(idless(usize::MAX)) else { panic!("the first page") };
        let Step::Decoded(ds, _, Some(note)) = (*first).join_page(idless(1)) else { panic!("a page sent again, cut, joined") };
        assert_eq!(ds.features.len(), 2);
        assert!(note.contains("first features again"), "{note}");
        // Pages are joined by their fields' names and types: another alias doesn't stop the read.
        let Step::Fetch(first) = fresh().join_page(page(&[0, 1], lonlat, point)) else { panic!("the first page") };
        let mut aliased = page(&[2, 3], lonlat, point);
        aliased.dataset.fields[0].alias = Some("Object id".into());
        let Step::Fetch(p) = (*first).join_page(aliased) else { panic!("another alias taken for other fields") };
        assert_eq!(p.read(), 4);
        // A page that would take the layer past its memory budget isn't joined, and says so; the
        // first page is kept whatever it takes, and one within the budget is joined.
        let first = page(&[0, 1], lonlat, point);
        assert!(first.memory > 0);
        let budget = first.memory * 3 / 2;
        let Step::Fetch(mut p) = fresh().join_page(first) else { panic!("the first page") };
        p.decoded_budget = budget;
        let Step::Decoded(ds, _, Some(note)) = (*p).join_page(page(&[2, 3], lonlat, point)) else { panic!("past the budget") };
        assert_eq!(ids(&ds), [0.0, 1.0]);
        assert!(note.contains("after feature 2 would take the layer past 0 MB of memory"), "{note}");
        let mut p = fresh();
        p.decoded_budget = 0;
        let Step::Fetch(mut p) = p.join_page(page(&[0, 1], lonlat, point)) else { panic!("the first page, over the budget") };
        p.decoded_budget = usize::MAX;
        let Step::Fetch(p) = (*p).join_page(page(&[2, 3], lonlat, point)) else { panic!("within the budget") };
        assert_eq!(p.read(), 4);
        assert!(p.decoded_bytes > 0);
        // What the features lost, summed over the pages and said once.
        let mut pages = fresh();
        for (ids, more, rings) in [(&[0, 1][..], true, 2), (&[2, 3][..], false, 3)] {
            let mut page = crate::esripbf::read_page(&pbf(ids, more, "OBJECTID"), "query").unwrap();
            page.losses.flat_rings = rings;
            pages = match pages.join_page(page) {
                Step::Fetch(p) => *p,
                Step::Decoded(_, report, None) => {
                    assert_eq!(report.messages, ["5 polygon rings have no area and are left out"]);
                    break;
                }
                other => panic!("{other:?}"),
            };
        }
    }

    /// Protocol buffer pages only when the layer's description lists them and names its page
    /// size: a layer that names none, or doesn't list them, is read in GeoJSON as before.
    #[test]
    fn arcgis_pbf_pages_only_when_listed_with_a_page_size() {
        let query = "https://x.org/arcgis/rest/services/P/FeatureServer/0/query?where=1%3D1&outFields=*&f=geojson";
        let description = "https://x.org/arcgis/rest/services/P/FeatureServer/0?f=json";
        // One page that holds the whole layer.
        let page = br#"{"type": "FeatureCollection", "features": [{"type": "Feature", "id": 0, "properties": {}}]}"#;
        for (layer, first) in [
            // No page size: the query as it is first, in GeoJSON.
            (r#"{"supportedQueryFormats": "JSON, geoJSON, PBF", "advancedQueryCapabilities": {"supportsPagination": true}}"#, Some(query)),
            // Protocol buffers not listed: GeoJSON pages from the start.
            (
                r#"{"maxRecordCount": 1, "supportedQueryFormats": "JSON, geoJSON", "advancedQueryCapabilities": {"supportsPagination": true}}"#,
                None,
            ),
        ] {
            let mut asked = Vec::new();
            let mut step = begin_with_pbf(query, 100);
            while let Step::Fetch(pages) = step {
                let u = pages.next_url().to_string();
                let a = if u == description { Ok(layer.as_bytes().to_vec()) } else { Ok(page.to_vec()) };
                asked.push(u);
                step = (*pages).land(a);
            }
            assert!(matches!(step, Step::Done(_, None)), "{layer}: {step:?}");
            assert!(asked.len() == 2 && asked[0] == description, "{layer}: {asked:?}");
            assert!(query_value(&asked[1], "f") == Some("geojson"), "{layer}: {asked:?}");
            if let Some(first) = first {
                assert_eq!(asked[1], first);
            }
        }
    }

    #[test]
    fn with_format_replaces_the_format_in_any_case_and_keeps_the_fragment() {
        let q = "https://x.org/FeatureServer/0/query?F=geojson&where=1%3D1&f=json#part";
        assert_eq!(with_format(q, "pbf"), "https://x.org/FeatureServer/0/query?where=1%3D1&f=pbf#part");
        assert_eq!(with_format("https://x.org/FeatureServer/0/query", "pbf"), "https://x.org/FeatureServer/0/query?f=pbf");
        assert_eq!(with_format("https://x.org/q?f=pbf", "geojson"), "https://x.org/q?f=geojson");
    }

    /// An ArcGIS layer's description first: a layer that pages is read from the start in pages of
    /// its `maxRecordCount` ordered by its object ids (a page asked for in no order may hold other
    /// features than the ordered first: Hawaii's zoning did), each from where the last stopped,
    /// until a page no longer says it was cut off; its query as it is is never fetched. A layer
    /// naming no page size is fetched as it is, then read again in pages as long as its answer
    /// when it was cut off. A layer that can't be paged, or whose paging fails before a page is
    /// read, comes back as it was (with its note).
    #[test]
    fn arcgis_pages_joined_from_offsets() {
        let query = "https://x.org/arcgis/rest/services/P/FeatureServer/0/query?where=1%3D1&outFields=*&f=geojson&token=t";
        // A page of the features `from..to` (of 5), cut off unless it reaches the last.
        let page = |from: u64, to: u64| {
            let features: Vec<_> = (from..to).map(|i| serde_json::json!({"type": "Feature", "id": i, "properties": {}})).collect();
            let mut p = serde_json::json!({"type": "FeatureCollection", "features": features});
            if to < 5 {
                p["properties"] = serde_json::json!({"exceededTransferLimit": true});
            }
            serde_json::to_vec(&p).unwrap()
        };
        let layer = |extra: &str| {
            format!(r#"{{"currentVersion": 11.1, "objectIdField": "OBJECTID", "advancedQueryCapabilities": {{"supportsPagination": true}}{extra}}}"#)
                .into_bytes()
        };
        let ids = |b: &[u8]| -> Vec<u64> {
            let v: serde_json::Value = serde_json::from_slice(b).unwrap();
            v["features"].as_array().unwrap().iter().map(|f| f["id"].as_u64().unwrap()).collect()
        };
        // Answers by address, keeping the addresses asked for.
        let server = |info: Vec<u8>| {
            move |asked: &mut Vec<String>, u: &str| -> Result<Vec<u8>, String> {
                asked.push(u.to_string());
                if u == "https://x.org/arcgis/rest/services/P/FeatureServer/0?f=json&token=t" {
                    return Ok(info.clone());
                }
                let at = |k: &str| query_value(u, k).and_then(|v| v.parse::<u64>().ok());
                match (at("resultOffset"), at("resultRecordCount")) {
                    (Some(o), Some(n)) => Ok(page(o, (o + n).min(5))),
                    _ => Err("http status: 404".into()),
                }
            }
        };
        // The description, then the pages: the query as it is (whose first page, in the server's
        // own order, would be read and thrown away) isn't fetched.
        let mut asked = Vec::new();
        let answer = server(layer(r#", "maxRecordCount": 2"#));
        let (joined, note) = super::join_pages(query, 100, |u| {
            assert_ne!(u, query, "the query isn't fetched as it is");
            answer(&mut asked, u)
        })
        .unwrap();
        assert_eq!((ids(&joined), note), (vec![0, 1, 2, 3, 4], None));
        assert_eq!(asked[0], "https://x.org/arcgis/rest/services/P/FeatureServer/0?f=json&token=t");
        assert_eq!(asked[1], format!("{query}&resultOffset=0&resultRecordCount=2&orderByFields=OBJECTID"));
        assert!(asked[2].contains("resultOffset=2&resultRecordCount=2") && asked[3].contains("resultOffset=4&resultRecordCount=2"));
        assert_eq!(asked.len(), 4);
        let v: serde_json::Value = serde_json::from_slice(&joined).unwrap();
        assert_eq!(v["type"], "FeatureCollection");
        assert!(v.pointer("/properties/exceededTransferLimit").is_none());
        assert!(partial_note(&joined).is_none());
        // A layer under its record limit: its description and one page.
        let mut asked = Vec::new();
        let answer = server(layer(r#", "maxRecordCount": 5"#));
        let (joined, note) = super::join_pages(query, 100, |u| answer(&mut asked, u)).unwrap();
        assert_eq!((ids(&joined), note, asked.len()), (vec![0, 1, 2, 3, 4], None, 2));
        // Pages that fail before one is read: the query as it is, as it came; an error when that
        // can't be fetched either.
        let mut asked = Vec::new();
        let answer = server(layer(r#", "maxRecordCount": 2"#));
        let (joined, note) = join_pages(query, page(0, 2), 100, |u| {
            if u.contains("resultOffset") { Err("http status: 500".into()) } else { answer(&mut asked, u) }
        });
        assert_eq!((joined, note, asked.len()), (page(0, 2), None, 1));
        let answer = server(layer(r#", "maxRecordCount": 2"#));
        let failed = super::join_pages(query, 100, |u| {
            if u.contains("resultOffset") || u == query { Err("http status: 500".into()) } else { answer(&mut Vec::new(), u) }
        });
        assert_eq!(failed, Err("http status: 500".into()));
        // The limit cuts it short, with a note.
        let answer = server(layer(r#", "maxRecordCount": 2"#));
        let (joined, note) = join_pages(query, page(0, 2), 3, |u| answer(&mut Vec::new(), u));
        assert_eq!(ids(&joined), [0, 1, 2]);
        assert!(note.is_some_and(|n| n.contains("first 3 features")));
        // A hostile `maxRecordCount` isn't asked for: the pages are as long as the first.
        for hostile in
            [r#", "maxRecordCount": 0"#, r#", "maxRecordCount": -5"#, r#", "maxRecordCount": 1e300"#, r#", "maxRecordCount": "x""#, ""]
        {
            let mut asked = Vec::new();
            let answer = server(layer(hostile));
            let (joined, note) = join_pages(query, page(0, 2), 100, |u| answer(&mut asked, u));
            assert_eq!((ids(&joined), note), (vec![0, 1, 2, 3, 4], None), "{hostile}");
            assert!(asked[1].contains("resultOffset=0&resultRecordCount=2&"), "{hostile}: {asked:?}");
        }
        let answer = server(layer(r#", "maxRecordCount": 18446744073709551615"#));
        assert_eq!(ids(&join_pages(query, page(0, 2), 100, |u| answer(&mut Vec::new(), u)).0), [0, 1, 2, 3, 4]);
        // A layer that doesn't page (or says nothing of it, before 10.3), or whose description
        // can't be read: the first page as it came, which says it was cut off.
        for info in [
            br#"{"currentVersion": 11, "advancedQueryCapabilities": {"supportsPagination": false}}"#.to_vec(),
            br#"{"currentVersion": 10.22, "maxRecordCount": 2}"#.to_vec(),
            b"<html>".to_vec(),
        ] {
            let mut asked = Vec::new();
            let answer = server(info);
            let (joined, note) = join_pages(query, page(0, 2), 100, |u| answer(&mut asked, u));
            assert_eq!((joined, note), (page(0, 2), None));
            assert_eq!(asked.len(), 1);
            assert!(partial_note(&page(0, 2)).is_some_and(|n| n.contains("record limit")));
        }
        // A server that ignores `resultOffset` sends its first page again: not read twice.
        let (joined, note) =
            join_pages(query, page(0, 2), 100, |u| if u.contains("resultOffset") { Ok(page(0, 2)) } else { Ok(layer("")) });
        assert_eq!(ids(&joined), [0, 1]);
        assert!(note.is_some_and(|n| n.contains("first features again")));
        // Paging that fails before a page is read (a page that adds nothing would ask for itself
        // again; a page that can't be fetched): the first page as it came.
        let empty = br#"{"type":"FeatureCollection","features":[],"properties":{"exceededTransferLimit":true}}"#;
        let (joined, note) =
            join_pages(query, page(0, 2), 100, |u| if u.contains("resultOffset") { Ok(empty.to_vec()) } else { Ok(layer("")) });
        assert_eq!((joined, note), (page(0, 2), None));
        let (joined, note) =
            join_pages(query, page(0, 2), 100, |u| if u.contains("resultOffset") { Err("http status: 500".into()) } else { Ok(layer("")) });
        assert_eq!((joined, note), (page(0, 2), None));
        // A page that fails later ends the reading with what was read, and says so.
        let answer = server(layer(r#", "maxRecordCount": 2"#));
        let (joined, note) = join_pages(query, page(0, 2), 100, |u| {
            if u.contains("resultOffset=2&") { Err("http status: 500".into()) } else { answer(&mut Vec::new(), u) }
        });
        assert_eq!(ids(&joined), [0, 1]);
        assert!(note.is_some_and(|n| n.contains("after feature 2 couldn't be read (http status: 500)")));
        // Only an ArcGIS query is described and paged, and not one that asks for a window of its
        // own; one whose layer names no page size, only when it was cut off.
        let unasked = |_: &str| -> Result<Vec<u8>, String> { panic!("no page is asked for") };
        let elsewhere = "https://x.org/collections/a/items?f=json";
        assert_eq!(join_pages(elsewhere, page(0, 2), 100, unasked), (page(0, 2), None));
        let window = format!("{query}&resultRecordCount=2");
        assert_eq!(join_pages(&window, page(0, 2), 100, unasked), (page(0, 2), None));
        let mut asked = Vec::new();
        let answer = server(layer(""));
        assert_eq!(join_pages(query, page(0, 5), 100, |u| answer(&mut asked, u)), (page(0, 5), None));
        assert_eq!(asked.len(), 1);
        // A limit of none: an empty collection, said.
        let answer = server(layer(r#", "maxRecordCount": 2"#));
        let (joined, note) = super::join_pages(query, 0, |u| answer(&mut Vec::new(), u)).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&joined).unwrap(),
            serde_json::json!({"type": "FeatureCollection", "features": []})
        );
        assert!(note.is_some_and(|n| n.contains("first 0 features")));
    }

    /// The addresses an ArcGIS layer is paged with: its description's (with the token), each page's
    /// (keeping the query's own order), and the order from the layer's object id field.
    #[test]
    fn arcgis_page_addresses() {
        let q = "https://x.org/a/MapServer/3/query?where=1%3D1&f=geojson#top";
        assert!(is_arcgis_query(q) && !is_arcgis_query("https://x.org/a/MapServer/3") && !is_arcgis_query("https://x.org/query"));
        assert_eq!(arcgis_layer_url(q), "https://x.org/a/MapServer/3?f=json");
        assert_eq!(
            arcgis_page_url(q, 4, 2, Some("FID")),
            "https://x.org/a/MapServer/3/query?where=1%3D1&f=geojson&resultOffset=4&resultRecordCount=2&orderByFields=FID#top"
        );
        // An order the query already has is kept.
        let ordered = "https://x.org/a/FeatureServer/0/query?orderByFields=NAME";
        assert_eq!(arcgis_page_url(ordered, 1, 1, Some("FID")), format!("{ordered}&resultOffset=1&resultRecordCount=1"));
        // The object id field from the fields when `objectIdField` is missing; never a name that
        // isn't plain.
        let paging = |v: serde_json::Value| arcgis_paging(&v);
        let pages = serde_json::json!({"supportsPagination": true});
        assert_eq!(
            paging(serde_json::json!({"advancedQueryCapabilities": pages, "fields": [{"name": "FID", "type": "esriFieldTypeOID"}]})),
            Some(ArcgisPaging { size: None, order: Some("FID".into()) })
        );
        assert_eq!(
            paging(serde_json::json!({"advancedQueryCapabilities": pages, "maxRecordCount": 1000, "objectIdField": "id&f=html"})),
            Some(ArcgisPaging { size: Some(1000), order: None })
        );
        assert_eq!(paging(serde_json::json!({"maxRecordCount": 1000})), None);
    }

    /// The zooms a layer's tiles are asked for at: a WMS layer down to its least scale (its own
    /// hint or its parent's; WMS 1.1.1's `ScaleHint` too), else zoom 22, and out to its greatest;
    /// a WMTS layer over its matrices, or those its `TileMatrixSetLimits` name.
    #[test]
    fn zooms_from_scale_hints_and_tile_matrices() {
        let wms = |version: &str, layers: &str| {
            format!(
                r#"<WMS_Capabilities version="{version}" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://m.example.org/wms?"/></Get></HTTP></DCPType></GetMap></Request>
<Layer><CRS>EPSG:3857</CRS><SRS>EPSG:3857</SRS>{layers}</Layer></Capability></WMS_Capabilities>"#
            )
        };
        let zooms = |doc: &str, name: &str| {
            parse("https://m.example.org/wms", doc.as_bytes()).unwrap().layers.into_iter().find(|l| l.name == name).unwrap().zooms
        };
        let equator = r#"<EX_GeographicBoundingBox><westBoundLongitude>-1</westBoundLongitude><eastBoundLongitude>1</eastBoundLongitude><southBoundLatitude>-1</southBoundLatitude><northBoundLatitude>1</northBoundLatitude></EX_GeographicBoundingBox>"#;
        let doc = wms(
            "1.3.0",
            &format!(
                r#"<Layer><Name>any</Name></Layer>
<Layer><Name>hinted</Name>{equator}<MinScaleDenominator>2132</MinScaleDenominator><MaxScaleDenominator>4400000</MaxScaleDenominator>
  <Layer><Name>child</Name>{equator}</Layer></Layer>"#
            ),
        );
        // No hint: down to 22.
        assert_eq!(zooms(&doc, "any"), Some((0, WMS_MAX_ZOOM)));
        // 1:2132 is zoom 18; 1:4 400 000 is past zoom 6's scale, within zoom 7's.
        assert_eq!(zooms(&doc, "hinted"), Some((7, 18)));
        assert_eq!(zooms(&doc, "child"), Some((7, 18)), "inherited");
        // Drawn down to 1:1000: zoom 19 (1:1066), not 20 (1:533, which it doesn't draw). A child
        // giving only its least scale keeps its parent's greatest.
        let deep = wms(
            "1.3.0",
            r#"<Layer><Name>deep</Name><MinScaleDenominator>1000</MinScaleDenominator><MaxScaleDenominator>4400000</MaxScaleDenominator>
  <Layer><Name>own least</Name><MinScaleDenominator>500</MinScaleDenominator></Layer></Layer>"#,
        );
        assert_eq!(zooms(&deep, "deep"), Some((7, 19)));
        assert_eq!(zooms(&deep, "own least"), Some((7, 20)));
        // WMS 1.1.1: a pixel's diagonal on the ground, 0.85 m (0.6 m a side: 1:2142).
        let old = wms("1.1.1", r#"<Layer><Name>old</Name><ScaleHint min="0.85" max="0"/></Layer>"#);
        assert_eq!(zooms(&old, "old"), Some((0, 18)));
        // A WMTS layer: its matrices' zooms, or those its limits name.
        let wmts = |limits: &str| {
            let matrix = |z: u32| {
                format!(
                    "<TileMatrix><ows:Identifier>{z}</ows:Identifier><ScaleDenominator>{}</ScaleDenominator><TopLeftCorner>-20037508.34278925 20037508.34278925</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>{}</MatrixWidth><MatrixHeight>{}</MatrixHeight></TileMatrix>",
                    Z0_SCALE / f64::from(1u32 << z),
                    1u32 << z,
                    1u32 << z
                )
            };
            let ms: String = (0..=20).map(matrix).collect();
            format!(
                r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0"><Contents>
<Layer><ows:Title>Photo</ows:Title><ows:Identifier>photo</ows:Identifier><TileMatrixSetLink><TileMatrixSet>gm</TileMatrixSet>{limits}</TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/jpeg" template="https://t.example.org/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.jpeg"/></Layer>
<TileMatrixSet><ows:Identifier>gm</ows:Identifier><ows:SupportedCRS>EPSG:3857</ows:SupportedCRS>{ms}</TileMatrixSet></Contents></Capabilities>"#
            )
        };
        let photo = |doc: &str| parse("https://t.example.org/WMTSCapabilities.xml", doc.as_bytes()).unwrap().layers[0].zooms;
        assert_eq!(photo(&wmts("")), Some((0, 20)), "past the usual 19");
        let limits = "<TileMatrixSetLimits><TileMatrixLimits><TileMatrix>8</TileMatrix></TileMatrixLimits><TileMatrixLimits><TileMatrix>17</TileMatrix></TileMatrixLimits></TileMatrixSetLimits>";
        assert_eq!(photo(&wmts(limits)), Some((8, 17)));
    }

    /// WMTS layers whose tile matrix sets are in another system (#769): swisstopo's LV95, its
    /// corners east first, and one in longitude/latitude written latitude first (EPSG:4326): read
    /// as matrices in the system, the layer's address asking for them by `{nm}`, `{ncol}`, `{nrow}`.
    #[test]
    fn wmts_layers_in_other_systems() {
        let caps = r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0"><Contents>
<Layer><ows:Title>Map</ows:Title><ows:Identifier>map</ows:Identifier>
 <ows:WGS84BoundingBox><ows:LowerCorner>5.1 45.4</ows:LowerCorner><ows:UpperCorner>11.5 48.2</ows:UpperCorner></ows:WGS84BoundingBox>
 <Style isDefault="true"><ows:Identifier>default</ows:Identifier></Style>
 <TileMatrixSetLink><TileMatrixSet>lv95</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/png" template="https://t.example.org/{Style}/{TileMatrixSet}/{TileMatrix}/{TileCol}/{TileRow}.png"/></Layer>
<Layer><ows:Title>World</ows:Title><ows:Identifier>world</ows:Identifier>
 <TileMatrixSetLink><TileMatrixSet>geo</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/png" template="https://t.example.org/w/{TileMatrix}/{TileRow}/{TileCol}.png"/></Layer>
<Layer><ows:Title>Elsewhere</ows:Title><ows:Identifier>odd</ows:Identifier>
 <TileMatrixSetLink><TileMatrixSet>odd</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/png" template="https://t.example.org/o/{TileMatrix}/{TileRow}/{TileCol}.png"/></Layer>
<TileMatrixSet><ows:Identifier>lv95</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG:2056</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>21</ows:Identifier><ScaleDenominator>17857.142857142857</ScaleDenominator><TopLeftCorner>2420000.0 1350000.0</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>375</MatrixWidth><MatrixHeight>250</MatrixHeight></TileMatrix>
 <TileMatrix><ows:Identifier>20</ows:Identifier><ScaleDenominator>35714.28571428571</ScaleDenominator><TopLeftCorner>2420000.0 1350000.0</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>188</MatrixWidth><MatrixHeight>125</MatrixHeight></TileMatrix>
</TileMatrixSet>
<TileMatrixSet><ows:Identifier>geo</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::4326</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>279541132.0143589</ScaleDenominator><TopLeftCorner>90 -180</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>2</MatrixWidth><MatrixHeight>1</MatrixHeight></TileMatrix>
</TileMatrixSet>
<TileMatrixSet><ows:Identifier>odd</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:IAU::49900</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>1000</ScaleDenominator><TopLeftCorner>0 0</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>1</MatrixWidth><MatrixHeight>1</MatrixHeight></TileMatrix>
</TileMatrixSet>
</Contents></Capabilities>"#;
        let s = parse("https://t.example.org/WMTSCapabilities.xml", caps.as_bytes()).unwrap();
        let names: Vec<&str> = s.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["map", "world"], "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.starts_with("1 layers have no tile matrix set")));
        let read = |i: usize| {
            let Offer::Tiles { template } = &s.layers[i].offer else { panic!() };
            let filled = template.replace("{z}/{x}/{y}", "0/0/0");
            let (base, _, w) = warp::Warp::parse(&filled).unwrap();
            (base.to_string(), w)
        };
        let (base, w) = read(0);
        assert_eq!(base, "https://t.example.org/default/lv95/{nm}/{ncol}/{nrow}.png");
        assert_eq!((w.epsg, w.area), (2056, Some([5.1, 45.4, 11.5, 48.2])));
        let warp::Grid::Tiles(ms) = &w.grid else { panic!() };
        // Coarsest first; 10 m and 5 m pixels; east first.
        assert_eq!(ms.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["20", "21"]);
        assert!((ms[0].cell - 10.0).abs() < 1e-9 && (ms[1].cell - 5.0).abs() < 1e-9);
        assert_eq!((ms[1].left, ms[1].top, ms[1].cols, ms[1].rows), (2_420_000.0, 1_350_000.0, 375, 250));
        // Latitude first in the document: read as west, north; its cells in degrees.
        let (base, w) = read(1);
        assert_eq!(base, "https://t.example.org/w/{nm}/{nrow}/{ncol}.png");
        let warp::Grid::Tiles(ms) = &w.grid else { panic!() };
        assert_eq!((w.epsg, ms[0].left, ms[0].top), (4326, -180.0, 90.0));
        assert!((ms[0].cell - 0.703_125).abs() < 1e-6, "{}", ms[0].cell);
    }

    /// A WMTS corner is read in EPSG's order even where the layer's box tips the guess the other
    /// way (swisstopo's LV03 set: its box reaches a little west of the corner), and a
    /// longitude/latitude corner written longitude first is read so.
    #[test]
    fn a_wmts_corner_follows_epsg_axis_order() {
        let caps = r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0"><Contents>
<Layer><ows:Title>Map</ows:Title><ows:Identifier>map</ows:Identifier>
 <ows:WGS84BoundingBox><ows:LowerCorner>5.140242 45.398181</ows:LowerCorner><ows:UpperCorner>11.47757 48.230651</ows:UpperCorner></ows:WGS84BoundingBox>
 <TileMatrixSetLink><TileMatrixSet>21781_27</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/png" template="https://t.example.org/{TileMatrix}/{TileCol}/{TileRow}.png"/></Layer>
<Layer><ows:Title>World</ows:Title><ows:Identifier>world</ows:Identifier>
 <TileMatrixSetLink><TileMatrixSet>geo</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/png" template="https://t.example.org/w/{TileMatrix}/{TileRow}/{TileCol}.png"/></Layer>
<TileMatrixSet><ows:Identifier>21781_27</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG:21781</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>14285714.285714287</ScaleDenominator><TopLeftCorner>420000.0 350000.0</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>1</MatrixWidth><MatrixHeight>1</MatrixHeight></TileMatrix>
</TileMatrixSet>
<TileMatrixSet><ows:Identifier>geo</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::4326</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>279541132.0143589</ScaleDenominator><TopLeftCorner>-180 90</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>2</MatrixWidth><MatrixHeight>1</MatrixHeight></TileMatrix>
</TileMatrixSet>
</Contents></Capabilities>"#;
        let s = parse("https://t.example.org/WMTSCapabilities.xml", caps.as_bytes()).unwrap();
        let corner = |i: usize| {
            let Offer::Tiles { template } = &s.layers[i].offer else { panic!() };
            let (_, _, w) = warp::Warp::parse(&template.replace("{z}/{x}/{y}", "0/0/0")).unwrap();
            let warp::Grid::Tiles(ms) = &w.grid else { panic!() };
            (w.epsg, ms[0].left, ms[0].top)
        };
        assert_eq!(corner(0), (21781, 420_000.0, 350_000.0));
        assert_eq!(corner(1), (4326, -180.0, 90.0));
    }

    /// A regional longitude/latitude WMTS corner whose numbers could be either way round: written
    /// longitude first (`5 50`) as some services do, it misses the layer read latitude first and
    /// is read longitude first; written latitude first (`50 5`) as the standard has it, it is kept,
    /// both at 5° E, 50° N. A correct corner is kept even where the other reading would cover
    /// more of the layer (`50 40` over lon 50..150, lat -60..40: 40° E, 50° N).
    #[test]
    fn a_regional_geographic_corner_is_read_by_its_layer() {
        let caps = |corner: &str, lower: &str, upper: &str, scale: f64| {
            format!(
                r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0"><Contents>
<Layer><ows:Title>Europe</ows:Title><ows:Identifier>europe</ows:Identifier>
 <ows:WGS84BoundingBox><ows:LowerCorner>{lower}</ows:LowerCorner><ows:UpperCorner>{upper}</ows:UpperCorner></ows:WGS84BoundingBox>
 <TileMatrixSetLink><TileMatrixSet>geo</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/png" template="https://t.example.org/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.png"/></Layer>
<TileMatrixSet><ows:Identifier>geo</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::4326</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>{scale}</ScaleDenominator><TopLeftCorner>{corner}</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>1</MatrixWidth><MatrixHeight>1</MatrixHeight></TileMatrix>
</TileMatrixSet>
</Contents></Capabilities>"#
            )
        };
        let europe = ("5 40", "15 50", 15_902_784.0);
        let wide = ("50 -60", "150 40", 155_300_628.896_866);
        for (written, (lower, upper, scale), at) in
            [("5 50", europe, (5.0, 50.0)), ("50 5", europe, (5.0, 50.0)), ("50 40", wide, (40.0, 50.0))]
        {
            let s = parse("https://t.example.org/WMTSCapabilities.xml", caps(written, lower, upper, scale).as_bytes()).unwrap();
            let Offer::Tiles { template } = &s.layers[0].offer else { panic!() };
            let (_, _, w) = warp::Warp::parse(&template.replace("{z}/{x}/{y}", "0/0/0")).unwrap();
            let warp::Grid::Tiles(ms) = &w.grid else { panic!() };
            assert_eq!((ms[0].left, ms[0].top), at, "{written}");
        }
    }

    #[test]
    fn wms_layers_in_web_mercator() {
        let xml = r#"<?xml version="1.0"?><WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Title>Demo WMS</Title></Service>
<Capability><Request><GetMap><Format>image/jpeg</Format><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://maps.example.org/wms?map=demo"/></Get></HTTP></DCPType></GetMap></Request>
<Layer><Title>Root</Title><CRS>EPSG:4326</CRS><CRS>EPSG:3857</CRS>
  <Layer><Name>roads</Name><Title>Roads</Title><EX_GeographicBoundingBox><westBoundLongitude>-10</westBoundLongitude><eastBoundLongitude>20</eastBoundLongitude><southBoundLatitude>35</southBoundLatitude><northBoundLatitude>60</northBoundLatitude></EX_GeographicBoundingBox></Layer>
  <Layer><Name>water bodies</Name></Layer>
</Layer>
<Layer><Name>utm only</Name><CRS>EPSG:32633</CRS><BoundingBox CRS="EPSG:32633" minx="500000" miny="5200000" maxx="700000" maxy="5400000"/></Layer>
<Layer><Name>automatic</Name><CRS>AUTO:42001</CRS></Layer></Capability></WMS_Capabilities>"#;
        let s = parse("https://maps.example.org/wms", xml.as_bytes()).unwrap();
        assert_eq!((s.kind.as_str(), s.title.as_str()), ("WMS", "Demo WMS"));
        let names: Vec<&str> = s.layers.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"roads") && names.contains(&"water bodies") && !names.contains(&"automatic"), "{names:?}");
        let roads = s.layers.iter().find(|l| l.name == "roads").unwrap();
        assert_eq!(roads.bbox, Some([-10.0, 35.0, 20.0, 60.0]));
        let Offer::Tiles { template } = &roads.offer else { panic!() };
        assert_eq!(
            template,
            "https://maps.example.org/wms?map=demo&SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=roads&STYLES=&CRS=EPSG:3857&BBOX={bbox-epsg-3857}&WIDTH=256&HEIGHT=256&FORMAT=image%2Fpng&TRANSPARENT=TRUE\
             #rustgis-warp={z}/{x}/{y};crs=3857;in=-10,35,20,60;sys=4326|n|EPSG:4326;map=east"
        );
        // Drawn in Web Mercator as it comes; its other system (latitude first, WMS 1.3.0's
        // EPSG:4326) kept for a map drawn in it.
        let tile = |t: &str| t.replace("{z}/{x}/{y}", "0/0/0");
        let roads_tile = tile(template);
        let (base, _, w) = warp::Warp::parse(&roads_tile).unwrap();
        let base = base.to_string();
        assert!(w.epsg == 3857 && base.contains("CRS=EPSG:3857&BBOX={bbox-epsg-3857}&"));
        assert_eq!(w.systems, [warp::System { epsg: 4326, name: "EPSG:4326".into(), lat_first: true }]);
        assert!(warp::Warp::map_template(&base).contains("&CRS={ncrs}&BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}&"));
        let water = s.layers.iter().find(|l| l.name == "water bodies").unwrap();
        assert!(matches!(&water.offer, Offer::Tiles { template } if template.contains("LAYERS=water%20bodies")));
        // A layer offered only in UTM: in UTM, warped (east first, as its own box says).
        let utm = s.layers.iter().find(|l| l.name == "utm only").unwrap();
        let Offer::Tiles { template } = &utm.offer else { panic!() };
        let filled = tile(template);
        let (base, _, w) = warp::Warp::parse(&filled).unwrap();
        assert!(base.contains("&CRS={ncrs}&BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}&"), "{base}");
        assert_eq!((w.epsg, &w.grid), (32633, &warp::Grid::Map { lat_first: false }));
        // Its identify asks in UTM too.
        assert_eq!(utm.feature_info.as_ref().map(|f| f.crs.as_str()), Some("EPSG:32633"));
        // One in a system RustGIS can't draw is left out, and said so.
        assert!(s.notes.iter().any(|n| n.contains("1 layers")));
    }

    /// A WMS layer's legend: the one the service draws (GetLegendGraphic, when its capabilities
    /// offer it in PNG or JPEG), then its default style's `LegendURL` picture (the style named
    /// `default`, else the first, of its own and those it inherits; the first `LegendURL` in a
    /// format RustGIS shows); WMS 1.1.1's alike, without the SLD version.
    #[test]
    fn wms_legends_from_the_service_and_the_default_style() {
        let caps = |version: &str, request: &str| {
            format!(
                r#"<WMS_Capabilities version="{version}" xmlns="http://www.opengis.net/wms" xmlns:sld="http://www.opengis.net/sld" xmlns:xlink="http://www.w3.org/1999/xlink">
<Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://maps.example.org/wms?"/></Get></HTTP></DCPType></GetMap>{request}</Request>
<Layer><CRS>EPSG:3857</CRS><SRS>EPSG:3857</SRS>
  <Style><Name>parent</Name><LegendURL><Format>image/png</Format><OnlineResource xlink:href="https://maps.example.org/legend/parent.png"/></LegendURL></Style>
  <Layer><Name>inherits</Name></Layer>
  <Layer><Name>own</Name>
    <Style><Name>first</Name><LegendURL><Format>image/gif</Format><OnlineResource xlink:href="https://maps.example.org/legend/first.gif"/></LegendURL>
      <LegendURL><Format>image/png</Format><OnlineResource xlink:href="/legend/first.png"/></LegendURL></Style>
  </Layer>
  <Layer><Name>named default</Name>
    <Style><Name>other</Name><LegendURL><Format>image/png</Format><OnlineResource xlink:href="https://maps.example.org/legend/other.png"/></LegendURL></Style>
    <Style><Name>default</Name><LegendURL><Format>image/png; mode=8bit</Format><OnlineResource xlink:href="https://maps.example.org/legend/default.png"/></LegendURL></Style>
  </Layer>
  <Layer><Name>only svg</Name><Style><Name>s</Name><LegendURL><Format>image/svg+xml</Format><OnlineResource xlink:href="https://maps.example.org/l.svg"/></LegendURL></Style></Layer>
  <Layer><Name>not on the web</Name><Style><Name>s</Name><LegendURL><Format>image/png</Format><OnlineResource xlink:href="file:///etc/legend.png"/></LegendURL></Style></Layer>
</Layer></Capability></WMS_Capabilities>"#
            )
        };
        let urls = |s: &Service, name: &str| -> Vec<String> {
            s.layers.iter().find(|l| l.name == name).unwrap().legends.iter().map(|l| l.url.clone()).collect()
        };
        // Without GetLegendGraphic: the default style's pictures.
        let s = parse("https://maps.example.org/wms", caps("1.3.0", "").as_bytes()).unwrap();
        assert_eq!(urls(&s, "inherits"), ["https://maps.example.org/legend/parent.png"]);
        assert_eq!(urls(&s, "own"), ["https://maps.example.org/legend/first.png"], "its own style first; the GIF passed over; resolved");
        assert_eq!(urls(&s, "named default"), ["https://maps.example.org/legend/default.png"]);
        // A picture RustGIS doesn't show, or not on the web: the parent's, or none.
        assert_eq!(urls(&s, "only svg"), Vec::<String>::new());
        assert_eq!(urls(&s, "not on the web"), Vec::<String>::new());
        assert!(s.layers.iter().flat_map(|l| &l.legends).all(|l| l.min_scale.is_none() && l.max_scale.is_none()));
        // With it (swisstopo's, in its SLD namespace): the service's legend first, the style's after.
        let glg = r#"<sld:GetLegendGraphic><Format>image/gif</Format><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://maps.example.org/de/?"/></Get></HTTP></DCPType></sld:GetLegendGraphic>"#;
        let s = parse("https://maps.example.org/wms", caps("1.3.0", glg).as_bytes()).unwrap();
        assert_eq!(
            urls(&s, "own"),
            [
                "https://maps.example.org/de/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetLegendGraphic&LAYER=own&FORMAT=image%2Fpng&SLD_VERSION=1.1.0",
                "https://maps.example.org/legend/first.png"
            ]
        );
        assert_eq!(
            urls(&s, "only svg"),
            [
                "https://maps.example.org/de/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetLegendGraphic&LAYER=only%20svg&FORMAT=image%2Fpng&SLD_VERSION=1.1.0"
            ]
        );
        // WMS 1.1.1 (SLD 1.0): no SLD version; its own address, else GetMap's.
        let glg = r#"<GetLegendGraphic><Format>image/jpeg</Format></GetLegendGraphic>"#;
        let s = parse("https://maps.example.org/wms", caps("1.1.1", glg).as_bytes()).unwrap();
        assert_eq!(
            urls(&s, "inherits").first().map(String::as_str),
            Some("https://maps.example.org/wms?SERVICE=WMS&VERSION=1.1.1&REQUEST=GetLegendGraphic&LAYER=inherits&FORMAT=image%2Fjpeg")
        );
        // Offered only in formats RustGIS doesn't show: not asked.
        let glg = r#"<GetLegendGraphic><Format>image/gif</Format></GetLegendGraphic>"#;
        let s = parse("https://maps.example.org/wms", caps("1.3.0", glg).as_bytes()).unwrap();
        assert_eq!(urls(&s, "own"), ["https://maps.example.org/legend/first.png"]);
    }

    /// A WMTS layer's legends: its default style's (`isDefault`, else the first), each with the
    /// scales it is for, in formats RustGIS shows.
    #[test]
    fn wmts_legends_for_their_scales() {
        let xml = r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" xmlns:xlink="http://www.w3.org/1999/xlink" version="1.0.0"><Contents>
<Layer><ows:Title>Land</ows:Title><ows:Identifier>land</ows:Identifier>
 <Style><ows:Identifier>dark</ows:Identifier><LegendURL format="image/png" xlink:href="https://t.example.org/dark.png"/></Style>
 <Style isDefault="true"><ows:Identifier>light</ows:Identifier>
  <LegendURL format="image/png" xlink:href="https://t.example.org/near.png" maxScaleDenominator="50000"/>
  <LegendURL format="image/svg+xml" xlink:href="https://t.example.org/far.svg" minScaleDenominator="50000"/>
  <LegendURL format="image/jpeg" xlink:href="far.jpg" minScaleDenominator="50000" maxScaleDenominator="junk"/></Style>
 <TileMatrixSetLink><TileMatrixSet>gm</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL format="image/png" resourceType="tile" template="https://t.example.org/{Style}/{TileMatrix}/{TileRow}/{TileCol}.png"/></Layer>
<TileMatrixSet><ows:Identifier>gm</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::3857</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>559082264.0287178</ScaleDenominator><TopLeftCorner>-20037508.34278925 20037508.34278925</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix></TileMatrixSet>
</Contents></Capabilities>"#;
        let s = parse("https://t.example.org/1.0.0/WMTSCapabilities.xml", xml.as_bytes()).unwrap();
        let legends = &s.layers[0].legends;
        assert_eq!(
            legends,
            &[
                Legend { url: "https://t.example.org/near.png".into(), min_scale: None, max_scale: Some(50_000.0) },
                Legend { url: "https://t.example.org/1.0.0/far.jpg".into(), min_scale: Some(50_000.0), max_scale: None },
            ]
        );
        // The one for the map's scale: up to below the maximum, from the minimum.
        let at = |scale: f64| legends_at(legends, scale).map(|l| l.url.as_str()).collect::<Vec<_>>();
        assert_eq!(at(10_000.0), ["https://t.example.org/near.png"]);
        assert_eq!(at(50_000.0), ["https://t.example.org/1.0.0/far.jpg"]);
        assert_eq!(at(1e9), ["https://t.example.org/1.0.0/far.jpg"]);
        assert!(legends_at(&legends[..1], 50_000.0).next().is_none(), "none at a scale none is for");
    }

    #[test]
    fn wms_13_geographic_bounds_inherit_and_override() {
        let xml = r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms"><Capability>
<Layer><CRS>EPSG:3857</CRS>
  <EX_GeographicBoundingBox><westBoundLongitude>-120</westBoundLongitude><southBoundLatitude>-40</southBoundLatitude><eastBoundLongitude>150</eastBoundLongitude><northBoundLatitude>70</northBoundLatitude></EX_GeographicBoundingBox>
  <Layer><Layer><Name>inherited</Name></Layer></Layer>
  <Layer>
    <EX_GeographicBoundingBox><westBoundLongitude>-20</westBoundLongitude><southBoundLatitude>10</southBoundLatitude><eastBoundLongitude>30</eastBoundLongitude><northBoundLatitude>50</northBoundLatitude></EX_GeographicBoundingBox>
    <Layer><Name>overridden</Name></Layer>
    <Layer>
      <EX_GeographicBoundingBox><westBoundLongitude>0</westBoundLongitude><southBoundLatitude>10</southBoundLatitude><eastBoundLongitude>20</eastBoundLongitude><northBoundLatitude>100</northBoundLatitude></EX_GeographicBoundingBox>
      <Layer><Name>invalid_keeps_nearest</Name></Layer>
    </Layer>
    <Layer><Name>direct</Name>
      <EX_GeographicBoundingBox><westBoundLongitude>1</westBoundLongitude><southBoundLatitude>2</southBoundLatitude><eastBoundLongitude>3</eastBoundLongitude><northBoundLatitude>4</northBoundLatitude></EX_GeographicBoundingBox>
      <Layer><Name>direct_descendant</Name></Layer>
    </Layer>
  </Layer>
  <Layer><Name>sibling</Name></Layer>
</Layer></Capability></WMS_Capabilities>"#;
        let s = parse("https://maps.example.org/wms", xml.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), 6);
        for (name, bbox) in [
            ("inherited", [-120.0, -40.0, 150.0, 70.0]),
            ("overridden", [-20.0, 10.0, 30.0, 50.0]),
            ("invalid_keeps_nearest", [-20.0, 10.0, 30.0, 50.0]),
            ("direct", [1.0, 2.0, 3.0, 4.0]),
            ("direct_descendant", [1.0, 2.0, 3.0, 4.0]),
            ("sibling", [-120.0, -40.0, 150.0, 70.0]),
        ] {
            assert_eq!(s.layers.iter().find(|l| l.name == name).unwrap().bbox, Some(bbox), "{name}");
        }
    }

    #[test]
    fn wms_11_geographic_bounds_inherit_and_override() {
        let xml = r#"<WMT_MS_Capabilities version="1.1.1"><Capability>
<Layer><SRS>EPSG:3857</SRS>
  <LatLonBoundingBox minx="-120" miny="-40" maxx="150" maxy="70"/>
  <Layer><Layer><Name>inherited</Name></Layer></Layer>
  <Layer>
    <LatLonBoundingBox minx="-20" miny="10" maxx="30" maxy="50"/>
    <Layer><Name>overridden</Name></Layer>
    <Layer>
      <LatLonBoundingBox minx="0" miny="10" maxx="20" maxy="100"/>
      <Layer><Name>invalid_keeps_nearest</Name></Layer>
    </Layer>
    <Layer><Name>direct</Name>
      <LatLonBoundingBox minx="1" miny="2" maxx="3" maxy="4"/>
      <Layer><Name>direct_descendant</Name></Layer>
    </Layer>
  </Layer>
  <Layer><Name>sibling</Name></Layer>
</Layer></Capability></WMT_MS_Capabilities>"#;
        let s = parse("https://maps.example.org/wms", xml.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), 6);
        for (name, bbox) in [
            ("inherited", [-120.0, -40.0, 150.0, 70.0]),
            ("overridden", [-20.0, 10.0, 30.0, 50.0]),
            ("invalid_keeps_nearest", [-20.0, 10.0, 30.0, 50.0]),
            ("direct", [1.0, 2.0, 3.0, 4.0]),
            ("direct_descendant", [1.0, 2.0, 3.0, 4.0]),
            ("sibling", [-120.0, -40.0, 150.0, 70.0]),
        ] {
            assert_eq!(s.layers.iter().find(|l| l.name == name).unwrap().bbox, Some(bbox), "{name}");
        }
    }

    #[test]
    fn wms_geographic_bounds_reject_invalid_without_ancestor() {
        for (root, version, crs) in [("WMS_Capabilities", "1.3.0", "CRS"), ("WMT_MS_Capabilities", "1.1.1", "SRS")] {
            for (w, s, e, n) in [
                ("NaN", "0", "20", "30"),
                ("0", "0", "inf", "30"),
                ("20", "0", "10", "30"),
                ("0", "30", "20", "10"),
                ("0", "0", "0", "30"),
                ("-181", "0", "20", "30"),
                ("0", "-91", "20", "30"),
                ("0", "0", "181", "30"),
                ("0", "0", "20", "91"),
                ("invalid", "0", "20", "30"),
                ("0", "0", "20", ""),
                ("", "", "", ""),
            ] {
                let bbox = if version == "1.3.0" {
                    format!(
                        "<EX_GeographicBoundingBox><westBoundLongitude>{w}</westBoundLongitude><southBoundLatitude>{s}</southBoundLatitude><eastBoundLongitude>{e}</eastBoundLongitude><northBoundLatitude>{n}</northBoundLatitude></EX_GeographicBoundingBox>"
                    )
                } else {
                    format!(r#"<LatLonBoundingBox minx="{w}" miny="{s}" maxx="{e}" maxy="{n}"/>"#)
                };
                for bbox in ["", bbox.as_str()] {
                    let xml = format!(
                        r#"<{root} version="{version}"><Capability><Layer><Name>parent</Name><{crs}>EPSG:3857</{crs}>{bbox}<Layer><Name>child</Name></Layer></Layer></Capability></{root}>"#
                    );
                    let service = parse("https://maps.example.org/wms", xml.as_bytes()).unwrap();
                    assert_eq!(service.layers.len(), 2);
                    assert!(service.layers.iter().all(|l| l.bbox.is_none()), "{version}: {bbox}");
                }
            }
        }
    }

    #[test]
    fn wmts_web_mercator_pyramids() {
        let mut matrices = String::new();
        for z in 0..=3 {
            let sd = Z0_SCALE / f64::from(1u32 << z);
            matrices.push_str(&format!("<TileMatrix><ows:Identifier>EPSG:3857:{z}</ows:Identifier><ScaleDenominator>{sd}</ScaleDenominator><TopLeftCorner>-20037508.3427892 20037508.3427892</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix>"));
        }
        let xml = format!(
            r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0">
<ows:ServiceIdentification><ows:Title>Tiles</ows:Title></ows:ServiceIdentification>
<Contents>
<Layer><ows:Title>Ortho</ows:Title><ows:Identifier>ortho</ows:Identifier><ows:WGS84BoundingBox><ows:LowerCorner>5 45</ows:LowerCorner><ows:UpperCorner>11 48</ows:UpperCorner></ows:WGS84BoundingBox>
 <Style isDefault="true"><ows:Identifier>normal</ows:Identifier></Style><Format>image/jpeg</Format><TileMatrixSetLink><TileMatrixSet>gm</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL format="image/jpeg" resourceType="tile" template="https://tiles.example.org/{{Style}}/{{TileMatrixSet}}/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.jpg"/></Layer>
<Layer><ows:Title>Swiss grid</ows:Title><ows:Identifier>lv95</ows:Identifier><TileMatrixSetLink><TileMatrixSet>2056</TileMatrixSet></TileMatrixSetLink></Layer>
<TileMatrixSet><ows:Identifier>gm</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::3857</ows:SupportedCRS>{matrices}</TileMatrixSet>
<TileMatrixSet><ows:Identifier>2056</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::2056</ows:SupportedCRS></TileMatrixSet>
</Contents></Capabilities>"#
        );
        let s = parse("https://tiles.example.org/WMTSCapabilities.xml", xml.as_bytes()).unwrap();
        assert_eq!(s.kind, "WMTS");
        assert_eq!(s.layers.len(), 1);
        let Offer::Tiles { template } = &s.layers[0].offer else { panic!() };
        assert_eq!(template, "https://tiles.example.org/normal/gm/EPSG:3857:{z}/{y}/{x}.jpg");
        assert_eq!(s.layers[0].bbox, Some([5.0, 45.0, 11.0, 48.0]));
        assert!(s.notes.iter().any(|n| n.contains("1 layers")));
    }

    /// A GIBS-like WMTS: a time-enabled layer takes its `{Time}` template, a timeless one its
    /// template without `{Time}`.
    const GIBS: &str = r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0">
<ows:ServiceIdentification><ows:Title>GIBS</ows:Title></ows:ServiceIdentification><Contents>
<Layer><ows:Title>True Color</ows:Title><ows:Identifier>TC</ows:Identifier><Style isDefault="true"><ows:Identifier>default</ows:Identifier></Style>
 <Dimension><ows:Identifier>Time</ows:Identifier><Default>2024-06-02</Default><Value>2000-02-24/2024-06-02/P1D</Value></Dimension>
 <TileMatrixSetLink><TileMatrixSet>GoogleMapsCompatible_Level9</TileMatrixSet></TileMatrixSetLink><Format>image/jpeg</Format>
 <ResourceURL resourceType="tile" format="image/jpeg" template="https://gibs.example.org/TC/default/{Time}/{TileMatrixSet}/{TileMatrix}/{TileRow}/{TileCol}.jpeg"/>
 <ResourceURL resourceType="tile" format="image/jpeg" template="https://gibs.example.org/TC/default/{TileMatrixSet}/{TileMatrix}/{TileRow}/{TileCol}.jpeg"/></Layer>
<Layer><ows:Title>Coastlines</ows:Title><ows:Identifier>Coast</ows:Identifier>
 <TileMatrixSetLink><TileMatrixSet>GoogleMapsCompatible_Level9</TileMatrixSet></TileMatrixSetLink><Format>image/png</Format>
 <ResourceURL resourceType="tile" format="image/png" template="https://gibs.example.org/Coast/default/{Time}/{TileMatrixSet}/{TileMatrix}/{TileRow}/{TileCol}.png"/>
 <ResourceURL resourceType="tile" format="image/png" template="https://gibs.example.org/Coast/default/{TileMatrixSet}/{TileMatrix}/{TileRow}/{TileCol}.png"/></Layer>
<Layer><ows:Title>Bad time</ows:Title><ows:Identifier>Bad</ows:Identifier>
 <Dimension><ows:Identifier>time</ows:Identifier><Default>../../x?y</Default></Dimension>
 <TileMatrixSetLink><TileMatrixSet>GoogleMapsCompatible_Level9</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/png" template="https://gibs.example.org/Bad/{Time}/{TileMatrix}/{TileRow}/{TileCol}.png"/></Layer>
<TileMatrixSet><ows:Identifier>GoogleMapsCompatible_Level9</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG:6.18:3:3857</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>559082264.0287178</ScaleDenominator><TopLeftCorner>-20037508.34278925 20037508.34278925</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix>
 <TileMatrix><ows:Identifier>1</ows:Identifier><ScaleDenominator>279541132.0143589</ScaleDenominator><TopLeftCorner>-20037508.34278925 20037508.34278925</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix>
</TileMatrixSet></Contents></Capabilities>"#;

    /// A WMTS whose only time is OGC's `current` and whose only template has `{Time}` (its
    /// server has no KVP GetTile).
    #[test]
    fn wmts_current_time() {
        let xml = r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0">
<ows:ServiceIdentification><ows:Title>Maps</ows:Title></ows:ServiceIdentification><Contents>
<Layer><ows:Title>Topo</ows:Title><ows:Identifier>topo</ows:Identifier>
 <Style><ows:Identifier>topo</ows:Identifier></Style><Format>image/jpeg</Format>
 <Dimension><ows:Identifier>Time</ows:Identifier><Default>current</Default><Value>current</Value></Dimension>
 <TileMatrixSetLink><TileMatrixSet>3857_19</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL format="image/jpeg" resourceType="tile" template="https://maps.example.org/1.0.0/topo/default/{Time}/3857/{TileMatrix}/{TileCol}/{TileRow}.jpeg"/></Layer>
<TileMatrixSet><ows:Identifier>3857_19</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG:3857</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>559082264.0287178</ScaleDenominator><TopLeftCorner>-20037508.342789244 20037508.342789244</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix>
 <TileMatrix><ows:Identifier>1</ows:Identifier><ScaleDenominator>279541132.0143589</ScaleDenominator><TopLeftCorner>-20037508.342789244 20037508.342789244</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix>
</TileMatrixSet></Contents></Capabilities>"#;
        let s = parse("https://maps.example.org/1.0.0/WMTSCapabilities.xml", xml.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), 1, "{:?}", s.notes);
        let l = &s.layers[0];
        assert_eq!(l.offer, Offer::Tiles { template: "https://maps.example.org/1.0.0/topo/default/{Time}/3857/{z}/{x}/{y}.jpeg".into() });
        assert_eq!(l.time.as_ref().map(|t| t.default.as_str()), Some("current"));
    }

    #[test]
    fn wmts_time_dimensions() {
        let s = parse("https://gibs.example.org/wmts/1.0.0/WMTSCapabilities.xml", GIBS.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), 3, "{:?}", s.notes);
        let tc = &s.layers[0];
        assert_eq!(
            tc.offer,
            Offer::Tiles { template: "https://gibs.example.org/TC/default/{Time}/GoogleMapsCompatible_Level9/{z}/{y}/{x}.jpeg".into() }
        );
        let time = tc.time.as_ref().unwrap();
        assert_eq!(time.default, "2024-06-02");
        assert_eq!(time.values, ["2000-02-24/2024-06-02/P1D"]);
        let coast = &s.layers[1];
        assert_eq!(coast.time, None);
        assert_eq!(
            coast.offer,
            Offer::Tiles { template: "https://gibs.example.org/Coast/default/GoogleMapsCompatible_Level9/{z}/{y}/{x}.png".into() }
        );
        // A default that isn't a time never reaches an address: with no time listed either, the
        // layer has no time.
        assert_eq!(s.layers[2].time, None);
        let listed = GIBS.replace("<Default>../../x?y</Default>", "<Default>?</Default><Value>2020-01-01/2020-02-01/P1D</Value>");
        let s = parse("https://gibs.example.org/wmts/1.0.0/WMTSCapabilities.xml", listed.as_bytes()).unwrap();
        assert_eq!(s.layers[2].time.as_ref().map(|t| t.default.as_str()), Some("2020-02-01"));
        // The latest of many listed times, past the few kept.
        let many: String = (0..MAX_TIME_VALUES + 100).map(|i| format!("<Value>{}-01-01</Value>", 1000 + i)).collect();
        let late = GIBS.replace("<Default>../../x?y</Default>", &many);
        let s = parse("https://gibs.example.org/wmts/1.0.0/WMTSCapabilities.xml", late.as_bytes()).unwrap();
        let t = s.layers[2].time.as_ref().unwrap();
        assert_eq!((t.default.as_str(), t.values.len()), ("2099-01-01", MAX_TIME_VALUES));
        assert!(t.cut == 100 && s.notes.contains(&long_times_note(1)), "{} {:?}", t.cut, s.notes);
        assert!(checked_time("2024-06-01").is_ok() && checked_time("2024-06-01T10:00:00Z").is_ok());
        assert!(checked_time("default").is_ok() && checked_time("current").is_ok());
        for bad in ["", "..", ".", "../x", "2024-06-01/../..", "a b", "x?y=1", "latest", &"9".repeat(50)] {
            assert!(checked_time(bad).is_err(), "{bad}");
        }
    }

    /// WMS layers with a time dimension (WMS 1.3.0 Annex C, as swisstopo's: years listed, a range
    /// by years, `99991231` for the latest): its GetMap template takes `{Time}`, at the default
    /// given (any value `TIME` takes, a range too), else the latest listed; a sublayer inherits
    /// its parent's.
    #[test]
    fn wms_time_dimensions() {
        let layer = |name: &str, dim: &str| {
            format!(r#"<Layer queryable="1"><Name>{name}</Name><Title>{name}</Title><CRS>EPSG:3857</CRS>{dim}</Layer>"#)
        };
        let dim = |attributes: &str, values: &str| format!(r#"<Dimension name="time" units="ISO8601" {attributes}>{values}</Dimension>"#);
        let parent = format!(
            "<Layer><Name>series</Name><Title>Series</Title><CRS>EPSG:3857</CRS>{}{}</Layer>",
            dim(r#"default="2020""#, "2000,2010,2020"),
            layer("part", "")
        );
        let layers = [
            layer("population", &dim(r#"default="2025" nearestValue="0""#, "1990, 2000,2025")),
            layer("strips", &dim(r#"default="2005/2025""#, "2005/2025")),
            layer("oblique", &dim("", "1927,1950,2004")),
            layer("photos", &dim(r#"default="99991231""#, "19291231,19341231,99991231")),
            layer("several", &dim(r#"default="2010" multipleValues="1""#, "1990/2020/P10Y")),
            layer("bad", &dim(r#"default="../x""#, "")),
            layer("plain", ""),
            parent,
        ]
        .concat();
        let caps = format!(
            r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms"><Service><Title>Maps</Title></Service><Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xmlns:xlink="http://www.w3.org/1999/xlink" xlink:href="https://maps.example.org/wms?"/></Get></HTTP></DCPType></GetMap></Request><Layer><Title>Root</Title>{layers}</Layer></Capability></WMS_Capabilities>"#
        );
        let url = "https://maps.example.org/wms?SERVICE=WMS&REQUEST=GetCapabilities";
        let s = parse(url, caps.as_bytes()).unwrap();
        let time = |name: &str| {
            let l = s.layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("{name}: {:?}", s.layers));
            let Offer::Tiles { template } = &l.offer else { panic!("{name}") };
            assert_eq!(template.ends_with("&TIME={Time}"), l.time.is_some(), "{name}: {template}");
            l.time.clone()
        };
        let population = time("population").unwrap();
        assert_eq!(
            (population.default.as_str(), population.values.as_slice(), population.multiple),
            ("2025", &["1990", "2000", "2025"].map(String::from)[..], false)
        );
        // The service's default as it gives it, a range too.
        assert_eq!(time("strips").map(|t| t.default), Some("2005/2025".into()));
        // No default: the latest listed.
        assert_eq!(time("oblique").map(|t| t.default), Some("2004".into()));
        assert_eq!(time("photos").map(|t| t.default), Some("99991231".into()));
        assert!(time("several").unwrap().multiple);
        // A default that isn't a time, and no time listed: no time, as for WMTS.
        assert_eq!(time("bad"), None);
        assert_eq!(time("plain"), None);
        assert_eq!(time("series").map(|t| t.default), Some("2020".into()));
        assert_eq!(time("part"), time("series"), "inherited");
        // WMS 1.1.1: the parent declares the dimension, the layer gives its extent.
        let old = caps.replace(r#"version="1.3.0""#, r#"version="1.1.1""#).replace("<CRS>", "<SRS>").replace("</CRS>", "</SRS>").replace(
            r#"<Dimension name="time" units="ISO8601" default="2025" nearestValue="0">1990, 2000,2025</Dimension>"#,
            r#"<Dimension name="time" units="ISO8601"/><Extent name="time" default="2000">1990,2000</Extent>"#,
        );
        let s = parse(url, old.as_bytes()).unwrap();
        let population = s.layers.iter().find(|l| l.name == "population").unwrap();
        assert_eq!(population.time.as_ref().map(|t| t.default.as_str()), Some("2000"));
        // What TIME takes (WMS 1.3.0 Table C.2): a time, a list, an interval with its resolution or
        // not; never a path or another parameter.
        for good in ["2024-06-01", "1990, 2000", "1990/2020", "1952/2026/P1Y", "2017-08-31T23:59:59Z/current/P1M", "1990,2000/2010"] {
            assert!(checked_wms_time(good).is_ok(), "{good}");
        }
        assert_eq!(checked_wms_time(" 1990, 2000 "), Ok("1990,2000".into()));
        assert_eq!(checked_wms_time("1990/ 2000 / P1Y, 2010"), Ok("1990/2000/P1Y,2010".into()));
        for good in ["P1DT12H", "PT30M", "PT0.5S", "P2W", "P1Y2M3D"] {
            assert!(is_duration(good), "{good}");
        }
        for bad in ["Pfoo", "P", "PT", "P1YT", "P1D1Y", "P1.5Y", "P.5D", "PT1H1H", "P1Q", "PT1..2S", "PT1.2.3S"] {
            assert!(!is_duration(bad), "{bad}");
        }
        for bad in [
            "",
            "1990,",
            "/",
            "1990/",
            "1990/2000/2010",
            "1990/2000/P",
            "1990/2000/Pfoo",
            "1990/2000/P1Y/x",
            "../x",
            "1990&LAYERS=x",
            &"1990,".repeat(300),
        ] {
            assert!(checked_wms_time(bad).is_err(), "{bad}");
        }
        // A sublayer's own dimension replaces its parent's, even one that gives no time.
        let broken = caps.replace(
            "<Name>part</Name><Title>part</Title><CRS>EPSG:3857</CRS>",
            r#"<Name>part</Name><Title>part</Title><CRS>EPSG:3857</CRS><Dimension name="time" units="ISO8601" default="../x">..</Dimension>"#,
        );
        let s = parse(url, broken.as_bytes()).unwrap();
        assert_eq!(s.layers.iter().find(|l| l.name == "part").unwrap().time, None);
        // An empty one too (WMS 1.3.0); in WMS 1.1.1 a `Dimension` alone declares the one inherited.
        let empty = caps.replace(
            "<Name>part</Name><Title>part</Title><CRS>EPSG:3857</CRS>",
            r#"<Name>part</Name><Title>part</Title><CRS>EPSG:3857</CRS><Dimension name="time" units="ISO8601"/>"#,
        );
        assert_eq!(parse(url, empty.as_bytes()).unwrap().layers.iter().find(|l| l.name == "part").unwrap().time, None);
        let declared = empty.replace(r#"version="1.3.0""#, r#"version="1.1.1""#).replace("<CRS>", "<SRS>").replace("</CRS>", "</SRS>");
        let s = parse(url, declared.as_bytes()).unwrap();
        assert_eq!(s.layers.iter().find(|l| l.name == "part").unwrap().time.as_ref().map(|t| t.default.as_str()), Some("2020"));
        // A document within the cap keeps all its times, and says nothing of them.
        let clean = parse(url, caps.as_bytes()).unwrap();
        assert!(!clean.notes.iter().any(|n| n.contains("of their times") || n.contains("list more than")));
        // A parent's many times, inherited by a great many layers, aren't kept for each.
        let many: String = (0..1000).map(|i| format!("{},", 1000 + i)).collect();
        let children: String = (0..1500).map(|i| layer(&format!("child{i}"), "")).collect();
        let crowd = format!(
            r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms"><Capability><Layer><Title>Root</Title><CRS>EPSG:3857</CRS>{}{children}</Layer></Capability></WMS_Capabilities>"#,
            dim(r#"default="1999""#, many.trim_end_matches(','))
        );
        let s = parse(url, crowd.as_bytes()).unwrap();
        let kept: usize = s.layers.iter().filter_map(|l| l.time.as_ref()).map(|t| t.values.len()).sum();
        assert!(s.layers.len() == 1500 && kept == MAX_DOCUMENT_TIME_VALUES, "{} layers, {kept} values", s.layers.len());
        assert!(s.layers.iter().all(|l| l.time.as_ref().is_some_and(|t| t.default == "1999")));
        // (They go to the layers listed first, and the others are counted.)
        let note = format!(
            "1450 layers keep fewer or none of their times: RustGIS keeps {MAX_DOCUMENT_TIME_VALUES} for one document, and they come after them"
        );
        assert!(s.notes.contains(&note), "{:?}", s.notes);
        assert!(s.layers[..MAX_DOCUMENT_TIME_VALUES / 1000].iter().all(|l| l.time.as_ref().is_some_and(|t| t.values.len() == 1000)));
        // Layers with dimensions of their own (not inherited): the budget goes in document order,
        // and the layers after it keep none, counted with the times they lose.
        let own: String = (0..MAX_DOCUMENT_TIME_VALUES / 1000 + 10)
            .map(|i| layer(&format!("own{i}"), &dim(r#"default="1999""#, many.trim_end_matches(','))))
            .collect();
        let owned = format!(
            r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms"><Capability><Layer><Title>Root</Title><CRS>EPSG:3857</CRS>{own}</Layer></Capability></WMS_Capabilities>"#
        );
        let s = parse(url, owned.as_bytes()).unwrap();
        let with_values = MAX_DOCUMENT_TIME_VALUES / 1000;
        assert!(s.layers[..with_values].iter().all(|l| l.time.as_ref().is_some_and(|t| t.values.len() == 1000 && t.cut == 0)));
        assert!(s.layers[with_values..].iter().all(|l| l.time.as_ref().is_some_and(|t| t.values.is_empty() && t.cut == 1000)));
        let note = format!(
            "10 layers keep fewer or none of their times: RustGIS keeps {MAX_DOCUMENT_TIME_VALUES} for one document, and they come after them"
        );
        assert!(s.notes.contains(&note), "{:?}", s.notes);
        // One layer's own list past its cap: the earliest are left out, and the note counts it.
        let long: String = (0..MAX_TIME_VALUES + 100).map(|i| format!("{},", 1000 + i)).collect();
        let doc = format!(
            r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms"><Capability><Layer><Title>Root</Title><CRS>EPSG:3857</CRS>{}</Layer></Capability></WMS_Capabilities>"#,
            layer("long", &dim(r#"default="2099""#, long.trim_end_matches(',')))
        );
        let s = parse(url, doc.as_bytes()).unwrap();
        let t = s.layers[0].time.as_ref().unwrap();
        assert_eq!((t.values.len(), t.cut, t.values[0].as_str()), (MAX_TIME_VALUES, 100, "1100"));
        assert!(s.notes.contains(&long_times_note(1)), "{:?}", s.notes);
        // So a document with more layers than are listed gives them to the listed ones (#1519).
        let children: String = (0..MAX_LISTED_LAYERS + 500).map(|i| layer(&format!("child{i}"), "")).collect();
        let crowd = format!(
            r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms"><Capability><Layer><Title>Root</Title><CRS>EPSG:3857</CRS>{}{children}</Layer></Capability></WMS_Capabilities>"#,
            dim(r#"default="1999""#, many.trim_end_matches(','))
        );
        let s = parse(url, crowd.as_bytes()).unwrap();
        let with_values = MAX_DOCUMENT_TIME_VALUES / 1000;
        assert_eq!(s.listed, Some(MAX_LISTED_LAYERS));
        assert!(s.layers[..with_values].iter().all(|l| l.time.as_ref().is_some_and(|t| t.values.len() == 1000)), "{:?}", s.layers[0].time);
        assert!(s.layers[with_values..].iter().all(|l| l.time.as_ref().is_some_and(|t| t.values.is_empty())));
        assert!(s.notes.iter().any(|n| n.starts_with("2450 layers keep fewer or none of their times")), "{:?}", s.notes);
        // A GetMap address that carries a TIME of its own (or other GetMap parameters) gives the
        // template one TIME, the layer's.
        let carried = caps.replace("https://maps.example.org/wms?", "https://maps.example.org/wms?map=m&amp;TIME=2000&amp;");
        let s = parse(url, carried.as_bytes()).unwrap();
        let Offer::Tiles { template } = &s.layers.iter().find(|l| l.name == "population").unwrap().offer else { panic!() };
        assert!(template.starts_with("https://maps.example.org/wms?map=m&SERVICE=WMS&"), "{template}");
        assert_eq!(template.matches("TIME=").count(), 1, "{template}");
        // GetFeatureInfo at the template's time; none for a layer without one, and before a fragment.
        let request = "https://maps.example.org/wms?REQUEST=GetFeatureInfo".to_string();
        assert_eq!(
            at_map_time(request.clone(), "https://maps.example.org/wms?LAYERS=x&TIME=1990%2F2000"),
            format!("{request}&TIME=1990%2F2000")
        );
        assert_eq!(at_map_time(request.clone(), "https://maps.example.org/wms?LAYERS=x"), request);
        assert_eq!(at_map_time(request.clone(), "https://maps.example.org/wms?LAYERS=x&TIME=../x"), request);
        assert_eq!(at_map_time(format!("{request}#v"), "https://maps.example.org/wms?TIME=1990"), format!("{request}&TIME=1990#v"));
        // Filled where `{Time}` is a query value, encoded; in a path, as it is.
        assert_eq!(
            with_time("https://a.example/wms?LAYERS=x&TIME={Time}", "2024-06-01T10:00:00+02:00/2024-06-02"),
            "https://a.example/wms?LAYERS=x&TIME=2024-06-01T10:00:00%2B02:00%2F2024-06-02"
        );
        assert_eq!(with_time("https://a.example/{Time}/{z}/{x}/{y}.png", "2024-06-01"), "https://a.example/2024-06-01/{z}/{x}/{y}.png");
    }

    /// A layer's times on the time slider, in the forms swisstopo's WMS and NASA's WMTS list them:
    /// the format they are written in (filled back to the very text listed), and steps: a period
    /// regular ones, a list (ranges in it too) dated ones, the latest's stand-ins left out.
    #[test]
    fn layer_times_on_the_time_slider() {
        use crate::time::{Unit, fill_template, parse_text};
        let lt = |default: &str, values: &[&str]| LayerTime {
            default: default.into(),
            values: values.iter().map(|v| v.to_string()).collect(),
            multiple: false,
            cut: 0,
        };
        let ms = |t: &str| parse_text(t).unwrap();
        let every = |start: &str, end: &str, unit, interval| SliderSteps::Every { start: ms(start), end: ms(end), unit, interval };
        // swisstopo's national map, a year at a time.
        let map = lt("2026", &["1952/2026/P1Y"]).slider().unwrap();
        assert_eq!((map.format, &map.steps, map.at), ("YYYY", &every("1952-01-01", "2026-01-01", Unit::Year, 1), Some(ms("2026-01-01"))));
        // A range without a period: by the unit its times name.
        assert_eq!(lt("1915", &["1870/1949"]).slider().unwrap().steps, every("1870-01-01", "1949-01-01", Unit::Year, 1));
        // NASA GIBS, day by day.
        let gibs = lt("2024-06-02", &["2000-02-24/2024-06-02/P1D"]).slider().unwrap();
        assert_eq!((gibs.format, gibs.steps), ("YYYY-MM-DD", every("2000-02-24", "2024-06-02", Unit::Day, 1)));
        // Years listed, a range among them.
        let settled = lt("2025", &["1919", "1950", "2019/2025"]).slider().unwrap();
        let years = |ys: &[i32]| SliderSteps::Dates(ys.iter().map(|y| ms(&format!("{y}-01-01"))).collect());
        assert_eq!(settled.steps, years(&[1919, 1950, 2019, 2020, 2021, 2022, 2023, 2024, 2025]));
        // Aerial photos by compact dates, the latest's stand-in left out (and no default to start at).
        let photos = lt("99991231", &["19291231", "19341231", "99991231"]).slider().unwrap();
        assert_eq!(
            (photos.format, &photos.steps, photos.at),
            ("YYYYMMDD", &SliderSteps::Dates(vec![ms("1929-12-31"), ms("1934-12-31")]), Some(ms("1934-12-31")))
        );
        // Date-times, filled back to exactly the times listed (the service takes no other).
        let ndvi = lt("2026-08-31T23:59:59Z", &["2025-08-31T23:59:59Z", "2026-08-31T23:59:59Z"]).slider().unwrap();
        assert_eq!(ndvi.format, "YYYY-MM-DD[T]HH:mm:ss[Z]");
        for (st, listed) in [(&ndvi, "2026-08-31T23:59:59Z"), (&photos, "19341231"), (&map, "2026")] {
            let at = match &st.steps {
                SliderSteps::Dates(d) => *d.last().unwrap(),
                SliderSteps::Every { end, .. } => *end,
            };
            assert_eq!(
                fill_template(&format!("https://wms.example.org/?TIME={{date:{}}}", st.format), at),
                format!("https://wms.example.org/?TIME={listed}")
            );
        }
        // Month ends in a list stay month ends; times with milliseconds (GeoServer's, ncWMS's).
        let months = lt("2024-04-30", &["2023-12-31", "2024-01-31/2024-04-30/P1M"]).slider().unwrap();
        let days = |ds: &[&str]| SliderSteps::Dates(ds.iter().map(|d| ms(d)).collect());
        assert_eq!(months.steps, days(&["2023-12-31", "2024-01-31", "2024-02-29", "2024-03-31", "2024-04-30"]));
        let geoserver = lt("2024-01-03T00:00:00.000Z", &["2024-01-01T00:00:00.000Z/2024-01-03T00:00:00.000Z/P1D"]).slider().unwrap();
        assert_eq!(geoserver.format, "YYYY-MM-DD[T]HH:mm:ss[.000Z]");
        assert_eq!(geoserver.steps, every("2024-01-01", "2024-01-03", Unit::Day, 1));
        assert_eq!(fill_template("TIME={date:YYYY-MM-DD[T]HH:mm:ss[.000Z]}", ms("2024-01-02")), "TIME=2024-01-02T00:00:00.000Z");
        // A range longer than the slider's steps: every second (fourth…) day of it, to its end.
        let long = lt("2025-01-01", &["1950-01-01/2025-01-01/P1D"]).slider().unwrap();
        assert_eq!(long.steps, every("1950-01-01", "2025-01-01", Unit::Day, 2));
        // Of a list longer than is read, the latest (and the default) still.
        let read: Vec<String> = (0..MAX_TIMES_READ + 500).map(|i| format!("{}", 1000 + i)).collect();
        assert_eq!(last(read, MAX_TIMES_READ).last().map(String::as_str), Some("21499"));
        // Of a long list, the latest are kept, those the default is among.
        let all: Vec<String> = (0..1500).map(|i| format!("{}", 1000 + i)).collect();
        let listed = layer_time(None, all, checked_wms_time, false).unwrap();
        assert_eq!((listed.default.as_str(), listed.values.len(), listed.values[0].as_str()), ("2499", MAX_TIME_VALUES, "1500"));
        // No dates to step to: one time (a name, a date), or none listed.
        assert_eq!(lt("current", &["current"]).slider(), None);
        assert_eq!(lt("1946", &["1946"]).slider(), None);
        assert!(!lt("current", &["current"]).changes());
        assert!(!lt("1946", &["1946"]).changes());
        assert!(!lt("2024", &[]).changes());
        assert!(lt("2026", &["1952/2026/P1Y"]).changes() && lt("current", &["current", "2025"]).changes());
        assert_eq!(lt("2024", &[]).slider(), None);
        assert_eq!(lt("2024", &["2024/2020"]).slider(), None);
        assert_eq!(period_step("PT6H"), Some((Unit::Hour, 6)));
        assert_eq!(period_step("P2W"), Some((Unit::Day, 14)));
        for bad in ["P", "PT", "P0Y", "P1S", "PT1D", "1Y", "P1.5Y"] {
            assert_eq!(period_step(bad), None, "{bad}");
        }
    }

    #[test]
    fn wmts_512_pyramids_and_kept_queries() {
        // A 512 px Web Mercator pyramid: half the scale denominator of a 256 px one at each zoom.
        let mut matrices = String::new();
        for z in 0..=3 {
            let sd = Z0_SCALE / f64::from(1u32 << (z + 1));
            matrices.push_str(&format!("<TileMatrix><ows:Identifier>{z}</ows:Identifier><ScaleDenominator>{sd}</ScaleDenominator><TopLeftCorner>-20037508.3427892 20037508.3427892</TopLeftCorner><TileWidth>512</TileWidth><TileHeight>512</TileHeight></TileMatrix>"));
        }
        let xml = format!(
            r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0">
<Contents><Layer><ows:Title>Big</ows:Title><ows:Identifier>big</ows:Identifier><Style isDefault="true"><ows:Identifier>s</ows:Identifier></Style><Format>image/png</Format><TileMatrixSetLink><TileMatrixSet>m512</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL format="image/png" resourceType="tile" template="https://t.example.org/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.png"/></Layer>
<TileMatrixSet><ows:Identifier>m512</ows:Identifier><ows:SupportedCRS>EPSG:3857</ows:SupportedCRS>{matrices}</TileMatrixSet></Contents></Capabilities>"#
        );
        let s = parse("https://t.example.org/WMTSCapabilities.xml", xml.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), 1, "{:?}", s.notes);
        // MapServer's map= (and a token) stay; the request's own parameters go.
        assert_eq!(
            endpoint("https://m.example.org/cgi-bin/mapserv?map=/maps/demo.map&SERVICE=WMS&REQUEST=GetCapabilities&token=t"),
            "https://m.example.org/cgi-bin/mapserv?map=/maps/demo.map&token=t"
        );
        assert_eq!(endpoint("https://m.example.org/wms?service=WMS"), "https://m.example.org/wms");
        // An ArcGIS service's token goes with its requests.
        let fs = br#"{"layers": [{"id": 0, "name": "Parcels"}]}"#;
        let s = parse("https://gis.example.org/arcgis/rest/services/P/FeatureServer?token=abc", fs).unwrap();
        let Offer::Features { url } = &s.layers[0].offer else { panic!() };
        assert!(url.ends_with("&token=abc"), "{url}");
        // A bound service constructs the same requests while retaining only its own login.
        for address in [
            "https://gis.example.org/arcgis/rest/services/P/FeatureServer",
            "https://gis.example.org/arcgis/rest/services/P/FeatureServer?token=abc",
        ] {
            let expected = parse(address, fs).unwrap();
            let bound = crate::login_address::bind("0123456789abcdef", "https://gis.example.org", address).unwrap();
            let service = parse(&bound, fs).unwrap();
            let Offer::Features { url } = &service.layers[0].offer else { panic!("features expected") };
            let offered = crate::login_address::parse(url).unwrap().unwrap();
            assert_eq!((offered.binding, offered.origin), ("0123456789abcdef", "https://gis.example.org"));
            let Offer::Features { url: expected } = &expected.layers[0].offer else { panic!("features expected") };
            assert_eq!(offered.url, expected, "{address}");
            assert_eq!(service.withheld_addresses, 0, "{address}");
        }
    }

    #[test]
    fn ogc_landing_fixture_discovers_json_and_resolves_items() {
        let address = "https://example.org/ogc/user/test-token?f=json";
        let service = parse(address, super::fixtures::fixture_landing().as_bytes()).unwrap();
        let catalog = service.collections_url.unwrap();
        assert_eq!(catalog, "https://example.org/ogc/user/test-token/catalog?f=json&key=fixture-key");
        let collections = parse(&catalog, super::fixtures::fixture_collections().as_bytes()).unwrap();
        assert_eq!(collections.layers.len(), 1);
        assert_eq!(collections.layers[0].title, "Places");
        assert_eq!(
            collections.layers[0].offer,
            Offer::Features { url: "https://example.org/ogc/user/test-token/collections/places/items?key=fixture-key&limit=10000".into() }
        );
        // The existing parser omits a point's zero-area bounds as a map-view extent.
        assert_eq!(collections.layers[0].bbox, None);
    }

    #[test]
    fn ogc_landing_links_are_resolved_without_inventing_paths_or_credentials() {
        let data_uri = "http://www.opengis.net/def/rel/ogc/1.0/data";
        for (href, expected) in [
            ("https://other.example/catalog?key=other", "https://other.example/catalog?key=other&f=json"),
            ("/catalog", "https://example.org/catalog?f=json"),
            ("catalog", "https://example.org/api/catalog?f=json"),
            ("./catalog", "https://example.org/api/catalog?f=json"),
            ("../catalog", "https://example.org/catalog?f=json"),
            ("?catalog=1", "https://example.org/api/root?catalog=1&f=json"),
        ] {
            let body = serde_json::json!({"links": [{"rel": "data", "href": href}]}).to_string();
            let result = parse("https://example.org/api/root?token=private&f=json", body.as_bytes()).unwrap();
            assert_eq!(result.collections_url.as_deref(), Some(expected));
        }
        let body = serde_json::json!({"links": [{"rel": data_uri, "href": "/catalog"}]}).to_string();
        let result = parse("https://example.org/api", body.as_bytes()).unwrap();
        assert_eq!(result.collections_url.as_deref(), Some("https://example.org/catalog?f=json"));
        for link in [
            serde_json::json!({"rel":"data", "href":"javascript:alert(1)"}),
            serde_json::json!({"rel":"data", "href":"file:///etc/passwd"}),
            serde_json::json!({"rel":"data", "href":"/catalog", "type":"text/html"}),
            serde_json::json!({"rel":"data", "href":""}),
            serde_json::json!({"rel":data_uri, "href":"/catalog", "type":"text/html"}),
        ] {
            let doc = serde_json::json!({"links":[link]}).to_string();
            assert!(parse("https://example.org/api", doc.as_bytes()).unwrap_err().contains("no usable JSON collections link"));
        }
        assert!(parse("https://example.org/api", br#"{"links":[]}"#).is_err());
        assert!(parse("https://example.org/api", br#"{"collections":[]}"#).unwrap().layers.is_empty());
        let chosen = parse(
            "https://example.org/api",
            br#"{"links":[
            {"rel":"data","href":"/untyped"},
            {"rel":"data","href":"/json","type":"application/json; charset=utf-8"}
        ]}"#,
        )
        .unwrap();
        assert_eq!(chosen.collections_url.as_deref(), Some("https://example.org/json"));
    }

    #[test]
    fn collections_links_count_only_settings_the_selected_link_omits() {
        let body = br#"{"links":[{"rel":"data","type":"application/json","href":"/catalog?f=json"}]}"#;
        for (from, count) in
            [("https://collections.invalid/root?f=json", 0), ("https://collections.invalid/root?f=json&token=RGSYNTH_link", 1)]
        {
            let service = parse(from, body).unwrap();
            assert_eq!(service.collections_url.as_deref(), Some("https://collections.invalid/catalog?f=json"));
            assert_eq!(service.withheld_addresses, count);
        }
    }

    #[test]
    fn ogc_json_format_is_idempotent() {
        let url = capabilities_url("https://example.org/api?f=html&key=a%2Fb&f=json#fragment", Some("ogcapi"));
        assert_eq!(url, "https://example.org/api?f=json&key=a%2Fb");
        assert_eq!(capabilities_url(&url, Some("ogcapi")), url);
    }

    #[test]
    fn ogc_format_preserves_opaque_query_bytes_and_typed_links() {
        let raw = "https://example.org/api?token=%FF%2f%20&f=json&signature=a~b&flag";
        assert_eq!(json_url(raw), raw);
        assert_eq!(json_url("https://example.org/api?token=%FF%2f%20&%66=html&signature=a~b&F=json&flag"), raw);
        for href in ["/catalog?token=%FF%2f%20&signature=a~b&flag", "/catalog?f=application%2Fjson&token=%FF%2f%20"] {
            let body = serde_json::json!({"links":[{"rel":"data","type":"application/json","href":href}]}).to_string();
            let service = parse("https://example.org/api", body.as_bytes()).unwrap();
            assert_eq!(service.collections_url, Some(format!("https://example.org{href}")));
        }
    }

    #[test]
    fn ogc_api_tiles() {
        // A collection with features and vector tiles, one with map tiles only, and one whose
        // tiles link is no web address.
        let doc = br#"{"title": "Wine", "collections": [
            {"id": "vineyards", "title": "Vineyards", "extent": {"spatial": {"bbox": [[6, 49, 8, 50]]}}, "links": [
                {"rel": "http://www.opengis.net/def/rel/ogc/1.0/tilesets-vector", "href": "https://x.org/wine/collections/vineyards/tiles"},
                {"rel": "http://www.opengis.net/def/rel/ogc/1.0/tilesets-vector", "type": "text/html", "href": "https://x.org/wine/collections/vineyards/tiles?f=html"},
                {"rel": "items", "type": "application/geo+json", "href": "https://x.org/wine/collections/vineyards/items"}]},
            {"id": "relief", "links": [{"rel": "tilesets-map", "type": "application/json", "href": "collections/relief/map/tiles?f=json"}]},
            {"id": "odd", "links": [{"rel": "tilesets-map", "href": "javascript:alert(1)"}]}]}"#;
        let s = parse("https://x.org/wine/collections", doc).unwrap();
        assert_eq!(s.kind, "OGC API Features and Tiles");
        let vt = s.layers.iter().find(|l| l.name == "vineyards (vector tiles)").unwrap();
        assert_eq!(
            vt.offer,
            Offer::VectorTiles {
                template: "https://x.org/wine/collections/vineyards/tiles/WebMercatorQuad/{z}/{y}/{x}?f=mvt".into(),
                layers: vec!["vineyards".into()]
            }
        );
        assert_eq!(vt.bbox, Some([6.0, 49.0, 8.0, 50.0]));
        assert!(s.layers.iter().any(|l| l.name == "vineyards" && matches!(l.offer, Offer::Features { .. })));
        // Map tiles only: no features offered; the relative link is resolved.
        let relief: Vec<&ServiceLayer> = s.layers.iter().filter(|l| l.name.starts_with("relief")).collect();
        assert_eq!(relief.len(), 1);
        assert_eq!(
            relief[0].offer,
            Offer::Tiles { template: "https://x.org/wine/collections/relief/map/tiles/WebMercatorQuad/{z}/{y}/{x}".into() }
        );
        assert!(!s.layers.iter().any(|l| l.name.starts_with("odd (")), "a link that isn't a web address is left out");
        // A landing page with the dataset's map tiles.
        let landing = br#"{"title": "Earth at Night", "links": [
            {"rel": "http://www.opengis.net/def/rel/ogc/1.0/tilesets-map", "href": "https://x.org/night/map/tiles"}]}"#;
        let s = parse("https://x.org/night/", landing).unwrap();
        assert_eq!(s.kind, "OGC API Tiles");
        assert_eq!(s.layers[0].offer, Offer::Tiles { template: "https://x.org/night/map/tiles/WebMercatorQuad/{z}/{y}/{x}".into() });
        // A landing page advertises the collections document instead of rejecting it.
        assert_eq!(
            parse("https://x.org/a/", br#"{"links": [{"rel": "data", "href": "https://x.org/a/collections"}]}"#)
                .unwrap()
                .collections_url
                .as_deref(),
            Some("https://x.org/a/collections?f=json")
        );
        // The tileset list to read the advertised template from, with the link's query kept (but
        // its `f`), on the list and the usual-path template alike.
        assert_eq!(vt.tilesets.as_deref(), Some("https://x.org/wine/collections/vineyards/tiles?f=json"));
        let tokened = br#"{"collections": [{"id": "roads", "links": [
            {"rel": "tilesets-map", "href": "https://x.org/c/roads/map/tiles?token=abc&f=json"}]}]}"#;
        let s = parse("https://x.org/c", tokened).unwrap();
        assert_eq!(
            s.layers[0].offer,
            Offer::Tiles { template: "https://x.org/c/roads/map/tiles/WebMercatorQuad/{z}/{y}/{x}?token=abc".into() }
        );
        assert_eq!(s.layers[0].tilesets.as_deref(), Some("https://x.org/c/roads/map/tiles?token=abc&f=json"));
    }

    #[test]
    fn bound_tiles_keep_query_rules_without_rebinding_after_a_foreign_hop() {
        let origin = "https://x.org";
        let bound = crate::login_address::bind("0123456789abcdef", origin, "https://x.org/c").unwrap();
        let document = br#"{"collections":[{"id":"roads","links":[{"rel":"tilesets-map","href":"https://x.org/c/roads/tiles?token=RGSYNTH_tiles_r15&f=json"}]}]}"#;
        let service = parse(&bound, document).unwrap();
        let Offer::Tiles { template } = &service.layers[0].offer else { panic!("tiles expected") };
        let template = crate::login_address::parse(template).unwrap().unwrap();
        assert_eq!(template.url, "https://x.org/c/roads/tiles/WebMercatorQuad/{z}/{y}/{x}?token=RGSYNTH_tiles_r15");
        assert_eq!(service.withheld_addresses, 0);
        let list = service.layers[0].tilesets.as_deref().unwrap();
        let foreign = br#"{"links":[{"rel":"item","type":"image/png","href":"https://x.org/back/{tileMatrix}/{tileRow}/{tileCol}.png"}]}"#;
        let resolution = resolve_tiles(list, "https://foreign-r15.example.invalid/tiles", foreign, false).unwrap();
        let TilesStep::Template(url) = resolution.step else { panic!("template expected") };
        assert_eq!(url, "https://x.org/back/{z}/{y}/{x}.png");
        assert!(crate::login_address::parse(&url).unwrap().is_none());
        assert_eq!(resolution.withheld_addresses, 1);
    }

    #[test]
    fn ogc_api_advertised_tile_templates() {
        let list = "https://x.org/c/roads/tiles?token=abc&f=json";
        // A list whose WebMercatorQuad tileset (named otherwise, as CubeWerx's `smerc`) points to
        // the tileset document; a tileset of another grid is passed over.
        let sets = br#"{"tilesets": [
            {"tileMatrixSetURI": "http://www.opengis.net/def/tilematrixset/OGC/1.0/WorldCRS84Quad", "links": [{"rel": "self", "href": "crs84"}]},
            {"dataType": "vector", "tileMatrixSetURI": "http://www.opengis.net/def/tilematrixset/OGC/1.0/WebMercatorQuad", "links": [{"rel": "self", "href": "https://x.org/c/roads/tiles/smerc"}]}]}"#;
        assert_eq!(
            resolve_tiles(list, list, sets, true).map(|r| r.step),
            Some(TilesStep::Next("https://x.org/c/roads/tiles/smerc?f=json".into()))
        );
        assert_eq!(resolve_tiles(list, list, sets, false).map(|r| r.step), None, "no map tileset in WebMercatorQuad");
        // The tileset's item links: MVT for vector tiles, PNG first for map tiles, the list's token withheld under F3.
        let tileset = br#"{"links": [
            {"rel": "item", "type": "image/jpeg", "templated": true, "href": "https://x.org/c/roads/tiles/smerc/{tileMatrix}/{tileRow}/{tileCol}.jpg"},
            {"rel": "item", "type": "image/png", "templated": true, "href": "https://x.org/c/roads/tiles/smerc/{tileMatrix}/{tileRow}/{tileCol}.png"},
            {"rel": "item", "type": "application/vnd.mapbox-vector-tile", "templated": true, "href": "smerc/{tileMatrix}/{tileRow}/{tileCol}.mvt"}]}"#;
        let doc = "https://x.org/c/roads/tiles/smerc?token=abc&f=json";
        assert_eq!(
            resolve_tiles(list, doc, tileset, true).map(|r| r.step),
            Some(TilesStep::Template("https://x.org/c/roads/tiles/smerc/{z}/{y}/{x}.mvt".into()))
        );
        assert_eq!(
            resolve_tiles(list, doc, tileset, false).map(|r| r.step),
            Some(TilesStep::Template("https://x.org/c/roads/tiles/smerc/{z}/{y}/{x}.png".into()))
        );
        // A list that carries its tilesets' item links needs no second document.
        let inline = br#"{"tilesets": [{"tileMatrixSetId": "WebMercatorQuad", "links": [
            {"rel": "item", "type": "image/png", "href": "https://x.org/t/{tileMatrix}/{tileRow}/{tileCol}"}]}]}"#;
        assert_eq!(
            resolve_tiles(list, list, inline, false).map(|r| r.step),
            Some(TilesStep::Template("https://x.org/t/{z}/{y}/{x}".into()))
        );
        // A template with other variables, or no web address, isn't taken.
        for bad in [
            br#"{"links": [{"rel": "item", "href": "https://x.org/{style}/{tileMatrix}/{tileRow}/{tileCol}"}]}"#.as_slice(),
            br#"{"links": [{"rel": "item", "href": "javascript:{tileMatrix}"}]}"#.as_slice(),
            b"not json",
        ] {
            assert_eq!(resolve_tiles(list, doc, bad, false).map(|r| r.step), None);
        }
        // F3 is independent of origin and directory containment. Destination-owned keys stay;
        // only the allowlisted collection choice may follow from the list.
        let list = "https://h.invalid/svc?token=RGSYNTH_list_20261103&k=RGSYNTH_unknown_20261103&collections=roads";
        for host in ["h.invalid", "elsewhere.invalid"] {
            let doc = format!(
                r#"{{"links":[{{"rel":"item","type":"image/png","href":"https://{host}/svc/{{tileMatrix}}/{{tileRow}}/{{tileCol}}?own=destination"}}]}}"#
            );
            let resolved = resolve_tiles(list, list, doc.as_bytes(), false).unwrap();
            assert_eq!(resolved.withheld_addresses, 1);
            let TilesStep::Template(template) = resolved.step else { panic!("expected template") };
            assert_eq!(template, format!("https://{host}/svc/{{z}}/{{y}}/{{x}}?own=destination&collections=roads"));
        }
        let clean = "https://h.invalid/svc?collections=roads";
        assert_eq!(resolve_tiles(clean, clean, tileset, false).unwrap().withheld_addresses, 0);
        // Even a suffix appended by RustGIS must pass the raw-path check. The normal style
        // stays under its service; an advertised '..' style does not inherit its token.
        let address = "https://h.invalid/rest/services/a/VectorTileServer?token=RGSYNTH_style_20261103";
        let bound = crate::login_address::bind("0123456789abcdef", "https://h.invalid", address).unwrap();
        for (style, withheld) in [("resources/styles", 0), ("../other/styles", 1), ("%2e%2e/other/styles", 1)] {
            let description = serde_json::json!({"defaultStyles": style});
            for address in [address, bound.as_str()] {
                let service = parse(address, description.to_string().as_bytes()).unwrap();
                assert_eq!(service.withheld_addresses, withheld, "{address}: {style}");
                let Offer::Style { url } = &service.layers[0].offer else { panic!("expected style") };
                assert_eq!(url.contains("RGSYNTH_style_20261103"), withheld == 0, "{address}: {style}");
            }
        }
    }

    #[test]
    fn wfs_and_ogc_api_features() {
        let xml = r#"<wfs:WFS_Capabilities version="2.0.0" xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:ows="http://www.opengis.net/ows/1.1" xmlns:xlink="http://www.w3.org/1999/xlink">
<ows:ServiceIdentification><ows:Title>Parcels</ows:Title></ows:ServiceIdentification>
<ows:OperationsMetadata><ows:Operation name="GetFeature"><ows:DCP><ows:HTTP><ows:Get xlink:href="https://gis.example.org/wfs?"/></ows:HTTP></ows:DCP></ows:Operation></ows:OperationsMetadata>
<wfs:FeatureTypeList><wfs:FeatureType><wfs:Name>ns:parcels</wfs:Name><wfs:Title>Parcels</wfs:Title><ows:WGS84BoundingBox><ows:LowerCorner>-1 2</ows:LowerCorner><ows:UpperCorner>3 4</ows:UpperCorner></ows:WGS84BoundingBox></wfs:FeatureType></wfs:FeatureTypeList></wfs:WFS_Capabilities>"#;
        let s = parse("https://gis.example.org/wfs", xml.as_bytes()).unwrap();
        assert_eq!(s.kind, "WFS");
        let Offer::Features { url } = &s.layers[0].offer else { panic!() };
        assert!(
            url.starts_with("https://gis.example.org/wfs?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature&TYPENAMES=ns:parcels&OUTPUTFORMAT=application%2Fjson&SRSNAME=EPSG:4326&COUNT="),
            "{url}"
        );
        assert_eq!(s.layers[0].bbox, Some([-1.0, 2.0, 3.0, 4.0]));
        // The JSON format the server names, or a note when it has none.
        let named = xml.replace("<ows:DCP>", r#"<ows:Parameter name="outputFormat"><ows:Value>text/xml; subtype=gml/3.1.1</ows:Value><ows:Value>application/json; subtype=geojson</ows:Value></ows:Parameter><ows:DCP>"#);
        let s = parse("https://gis.example.org/wfs", named.as_bytes()).unwrap();
        assert!(
            matches!(&s.layers[0].offer, Offer::Features { url } if url.contains("OUTPUTFORMAT=application%2Fjson%3B%20subtype%3Dgeojson"))
        );
        let gml_only = xml.replace(
            "<ows:DCP>",
            r#"<ows:Parameter name="outputFormat"><ows:Value>text/xml; subtype=gml/3.1.1</ows:Value></ows:Parameter><ows:DCP>"#,
        );
        let s = parse("https://gis.example.org/wfs", gml_only.as_bytes()).unwrap();
        assert!(s.layers.is_empty() && s.notes.iter().any(|n| n.contains("no GeoJSON")));

        let json = r#"{"title": "API", "collections": [
            {"id": "buildings", "title": "Buildings", "extent": {"spatial": {"bbox": [[7, 46, 8, 47]]}},
             "links": [{"rel": "items", "type": "application/geo+json", "href": "https://api.example.org/collections/buildings/items"}]},
            {"id": "roads"}]}"#;
        let s = parse("https://api.example.org/collections?f=json", json.as_bytes()).unwrap();
        assert_eq!(s.kind, "OGC API Features");
        assert_eq!(s.layers.len(), 2);
        assert!(
            matches!(&s.layers[0].offer, Offer::Features { url } if url == "https://api.example.org/collections/buildings/items?limit=10000")
        );
        assert!(
            matches!(&s.layers[1].offer, Offer::Features { url } if url == "https://api.example.org/collections/roads/items?limit=10000&f=json")
        );
        assert_eq!(s.layers[0].bbox, Some([7.0, 46.0, 8.0, 47.0]));
        assert!(parse("x", br#"{"links": [{"rel": "data", "href": "c"}]}"#).unwrap_err().contains("landing page"));
        assert!(parse("x", b"<ServiceExceptionReport>boom</ServiceExceptionReport>").unwrap_err().contains("boom"));
        assert!(parse("x", b"<html/>").is_err());
        assert!(parse("x", &[0xff]).is_err());
    }

    #[test]
    fn wcs_coverages() {
        let v2 = r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0" xmlns:xlink="http://www.w3.org/1999/xlink">
            <ows:ServiceIdentification><ows:Title>Soils</ows:Title></ows:ServiceIdentification>
            <ows:OperationsMetadata><ows:Operation name="GetCoverage"><ows:DCP><ows:HTTP>
              <ows:Get xlink:href="https://maps.example.org/mapserv?map=/m/soc.map&amp;amp;"/></ows:HTTP></ows:DCP></ows:Operation></ows:OperationsMetadata>
            <wcs:ServiceMetadata><wcs:formatSupported>image/png</wcs:formatSupported><wcs:formatSupported>image/tiff</wcs:formatSupported></wcs:ServiceMetadata>
            <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>soc_mean</wcs:CoverageId>
              <ows:WGS84BoundingBox><ows:LowerCorner>-180 -56</ows:LowerCorner><ows:UpperCorner>180 84</ows:UpperCorner></ows:WGS84BoundingBox>
            </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#;
        let s = parse("https://maps.example.org/mapserv?map=/m/soc.map&SERVICE=WCS&REQUEST=GetCapabilities", v2.as_bytes()).unwrap();
        assert_eq!((s.kind.as_str(), s.title.as_str()), ("WCS", "Soils"));
        assert_eq!(s.layers[0].bbox, Some([-180.0, -56.0, 180.0, 84.0]));
        let Offer::Coverage(c) = &s.layers[0].offer else { panic!("{:?}", s.layers[0].offer) };
        assert_eq!((c.version.as_str(), c.id.as_str()), ("2.0.1", "soc_mean"));
        assert_eq!(
            describe_coverage_url(c),
            "https://maps.example.org/mapserv?map=/m/soc.map&SERVICE=WCS&VERSION=2.0.1&REQUEST=DescribeCoverage&COVERAGEID=soc_mean"
        );
        let desc = r#"<wcs:CoverageDescriptions xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2">
            <wcs:CoverageDescription><gml:boundedBy><gml:Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="lat long">
              <gml:lowerCorner>-56 -180</gml:lowerCorner><gml:upperCorner>84 180</gml:upperCorner></gml:Envelope></gml:boundedBy>
            <wcs:ServiceParameters><wcs:nativeFormat>image/tiff</wcs:nativeFormat></wcs:ServiceParameters></wcs:CoverageDescription></wcs:CoverageDescriptions>"#;
        let info = parse_describe(c, desc.as_bytes()).unwrap();
        assert_eq!(info.axes, Some(("long".into(), "lat".into())));
        assert_eq!(info.bbox, Some([-180.0, -56.0, 180.0, 84.0]));
        assert_eq!(info.format, "image/tiff");
        let area = coverage_area(&info, [170.0, 80.0, 190.0, 90.0]).unwrap();
        assert_eq!(area, [170.0, 80.0, 180.0, 84.0]);
        assert_eq!(coverage_size(area), (2048, 819));
        assert_eq!(coverage_size([0.0, 0.0, 1.0, 4.0]), (512, 2048));
        assert!(coverage_area(&info, [0.0, 85.0, 1.0, 89.0]).is_none());
        let get = get_coverage_url(c, &info, [7.0, 46.0, 8.0, 47.0]);
        assert!(
            get.contains("COVERAGEID=soc_mean&FORMAT=image%2Ftiff&")
                && get.contains("&SUBSET=long(7,8)&SUBSET=lat(46,47)&SCALESIZE=long(2048),lat(2048)"),
            "{get}"
        );
        assert!(
            parse_describe(c, b"<ows:ExceptionReport xmlns:ows='o'>no such coverage</ows:ExceptionReport>")
                .unwrap_err()
                .contains("no such coverage")
        );
        assert!(parse_describe(c, b"<CoverageDescriptions/>").is_err());
        // No axis labels: EPSG:4326's corners are latitude first.
        let unlabelled = desc.replace(r#" axisLabels="lat long""#, "");
        assert_eq!(parse_describe(c, unlabelled.as_bytes()).unwrap().bbox, Some([-180.0, -56.0, 180.0, 84.0]));

        let v1 = r#"<WCS_Capabilities version="1.0.0" xmlns="http://www.opengis.net/wcs" xmlns:gml="http://www.opengis.net/gml" xmlns:xlink="http://www.w3.org/1999/xlink">
            <Service><name>MapServer WCS</name><label>Elevation</label></Service>
            <Capability><Request><GetCoverage><DCPType><HTTP><Get><OnlineResource xlink:href="https://dem.example.org/wcs?"/></Get></HTTP></DCPType></GetCoverage></Request></Capability>
            <ContentMetadata><CoverageOfferingBrief><name>dem</name><label>Terrain</label>
              <lonLatEnvelope srsName="urn:ogc:def:crs:OGC:1.3:CRS84"><gml:pos>5 45</gml:pos><gml:pos>11 48</gml:pos></lonLatEnvelope>
            </CoverageOfferingBrief></ContentMetadata></WCS_Capabilities>"#;
        let s = parse("https://dem.example.org/wcs", v1.as_bytes()).unwrap();
        assert_eq!((s.layers[0].title.as_str(), s.layers[0].bbox), ("Terrain", Some([5.0, 45.0, 11.0, 48.0])));
        let Offer::Coverage(c) = &s.layers[0].offer else { panic!() };
        assert_eq!(c.version, "1.0.0");
        let desc = r#"<CoverageDescription xmlns="http://www.opengis.net/wcs" xmlns:gml="http://www.opengis.net/gml"><CoverageOffering><name>dem</name>
            <lonLatEnvelope><gml:pos>5 45</gml:pos><gml:pos>11 48</gml:pos></lonLatEnvelope>
            <supportedFormats><formats>PNG</formats><formats>GEOTIFF_INT16</formats></supportedFormats></CoverageOffering></CoverageDescription>"#;
        let info = parse_describe(c, desc.as_bytes()).unwrap();
        assert_eq!((info.format.as_str(), info.bbox), ("GEOTIFF_INT16", Some([5.0, 45.0, 11.0, 48.0])));
        assert_eq!(
            get_coverage_url(c, &info, [6.0, 46.0, 7.0, 46.5]),
            "https://dem.example.org/wcs?SERVICE=WCS&VERSION=1.0.0&REQUEST=GetCoverage&COVERAGE=dem&CRS=EPSG:4326&BBOX=6,46,7,46.5&WIDTH=2048&HEIGHT=1024&FORMAT=GEOTIFF_INT16"
        );
        assert!(parse_describe(c, b"<CoverageDescription/>").is_err());

        let v11 = r#"<Capabilities version="1.1.1" xmlns="http://www.opengis.net/wcs/1.1.1" xmlns:ows="http://www.opengis.net/ows/1.1">
            <Contents><CoverageSummary><Identifier>ndvi</Identifier></CoverageSummary></Contents></Capabilities>"#;
        let s = parse("https://x.org/wcs", v11.as_bytes()).unwrap();
        assert_eq!(s.kind, "WCS");
        assert!(matches!(&s.layers[0].offer, Offer::Coverage(c) if c.version == "1.0.0" && c.endpoint == "https://x.org/wcs"));
        assert!(!s.notes.is_empty());
        assert_eq!(
            capabilities_url("https://x.org/geoserver/wcs", None),
            "https://x.org/geoserver/wcs?SERVICE=WCS&REQUEST=GetCapabilities"
        );

        assert_eq!(lonlat_axes(&["Lat", "Long"], ""), Some(("Long".into(), "Lat".into())));
        assert_eq!(lonlat_axes(&["x", "y"], "EPSG:4326"), Some(("x".into(), "y".into())));
        assert_eq!(lonlat_axes(&["a", "b"], "http://www.opengis.net/def/crs/EPSG/0/4326"), Some(("b".into(), "a".into())));
        assert_eq!(lonlat_axes(&["x(1)", "y"], ""), None);
        assert_eq!(lonlat_axes(&["x"], ""), None);
    }

    #[test]
    fn csw_records() {
        let u = records_url(
            "https://cat.example.org/csw?SERVICE=CSW&REQUEST=GetCapabilities",
            "O'Brien\u{7}",
            Some([7.0, 46.0, 8.0, 47.0]),
            21,
        );
        assert!(u.starts_with("https://cat.example.org/csw?SERVICE=CSW&VERSION=2.0.2&REQUEST=GetRecords&"), "{u}");
        assert!(u.contains("&startPosition=21&") && u.contains("O%27%27Brien") && u.contains("BBOX%28ows:BoundingBox,7,46,8,47%29"), "{u}");
        assert!(!records_url("https://cat.example.org/csw", " ", None, 0).contains("constraint="));

        let xml = r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dct="http://purl.org/dc/terms/" xmlns:ows="http://www.opengis.net/ows">
            <csw:SearchResults numberOfRecordsMatched="45" numberOfRecordsReturned="2" nextRecord="3">
              <csw:Record><dc:identifier>a1</dc:identifier><dc:title>Forests
                Wälder</dc:title><dct:abstract>Forest cover.</dct:abstract>
                <dc:URI protocol="OGC:WMS" name="forests">https://geo.example.org/wms?SERVICE=WMS&amp;REQUEST=GetCapabilities</dc:URI>
                <dc:URI protocol="WWW:DOWNLOAD-1.0-http--download">https://geo.example.org/forests.zip</dc:URI>
                <dc:URI protocol="WWW:DOWNLOAD-1.0-http--download">https://geo.example.org/forests.zip</dc:URI>
                <dct:references scheme="ESRI:REST">https://x.org/arcgis/rest/services/F/MapServer</dct:references>
                <dc:URI>/files/doc.pdf</dc:URI>
                <dc:URI>https://geo.example.org/geoserver/wfs</dc:URI>
                <ows:BoundingBox crs="urn:ogc:def:crs:EPSG:6.6:4326"><ows:LowerCorner>45.8 5.9</ows:LowerCorner><ows:UpperCorner>47.8 10.5</ows:UpperCorner></ows:BoundingBox>
              </csw:Record>
              <csw:Record><dc:title>Lakes</dc:title>
                <ows:WGS84BoundingBox><ows:LowerCorner>6 46</ows:LowerCorner><ows:UpperCorner>7 47</ows:UpperCorner></ows:WGS84BoundingBox></csw:Record>
            </csw:SearchResults></csw:GetRecordsResponse>"#;
        let page = parse_records("https://cat.example.org/csw?x=1", xml.as_bytes()).unwrap();
        assert_eq!((page.matched, page.next, page.records.len()), (45, Some(3), 2));
        let r = &page.records[0];
        assert_eq!((r.id.as_str(), r.title.as_str(), r.summary.as_str()), ("a1", "Forests", "Forest cover."));
        assert_eq!(r.bbox, Some([5.9, 45.8, 10.5, 47.8]));
        let kinds: Vec<&str> = r.links.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, ["wms", "download", "arcgis", "web", "wfs"]);
        assert_eq!(r.links[0].name.as_deref(), Some("forests"));
        assert_eq!(r.links[3].url, "https://cat.example.org/files/doc.pdf");
        assert_eq!((page.records[1].title.as_str(), page.records[1].bbox), ("Lakes", Some([6.0, 46.0, 7.0, 47.0])));

        let last = xml.replace(r#"nextRecord="3""#, r#"nextRecord="0""#);
        assert_eq!(parse_records("https://c.org/csw", last.as_bytes()).unwrap().next, None);
        assert!(
            parse_records("https://c.org/csw", b"<ows:ExceptionReport xmlns:ows='o'>bad CQL</ows:ExceptionReport>")
                .unwrap_err()
                .contains("bad CQL")
        );
        assert!(parse_records("https://c.org/csw", b"<WMS_Capabilities/>").is_err());
        assert!(parse_records("https://c.org/csw", &[0xff, 0xfe]).is_err());
        let deep = format!("<GetRecordsResponse>{}{}</GetRecordsResponse>", "<a>".repeat(5000), "</a>".repeat(5000));
        assert!(
            parse("https://c.org/wms", deep.replace("GetRecordsResponse", "WMS_Capabilities").as_bytes()).unwrap_err().contains("deep")
        );
        assert!(parse_records("https://c.org/csw", deep.as_bytes()).unwrap_err().contains("deep"));
        let many = format!("<GetRecordsResponse>{}</GetRecordsResponse>", "<Record><title>t</title></Record>".repeat(1000));
        assert_eq!(parse_records("https://c.org/csw", many.as_bytes()).unwrap().records.len(), RECORDS_PAGE as usize * 5);
    }

    /// Every kind of document is read up to the limit, and what's past it is counted in a note.
    #[test]
    fn a_document_past_the_read_limit_says_how_much_it_left_out() {
        fn many(n: usize, item: impl Fn(usize) -> String) -> String {
            (0..n).map(item).collect()
        }
        fn list(n: usize, item: impl Fn(usize) -> serde_json::Value) -> serde_json::Value {
            (0..n).map(item).collect()
        }
        let wfs = |n| {
            let types = many(n, |i| format!("<FeatureType><Name>t{i}</Name></FeatureType>"));
            format!(r#"<WFS_Capabilities version="2.0.0"><FeatureTypeList>{types}</FeatureTypeList></WFS_Capabilities>"#)
        };
        // Named layers in an unnamed one, which isn't counted.
        let wms = |n| {
            let layers = many(n, |i| format!("<Layer><Name>t{i}</Name></Layer>"));
            format!(
                r#"<WMS_Capabilities version="1.3.0"><Capability><Layer><CRS>EPSG:3857</CRS>{layers}</Layer></Capability></WMS_Capabilities>"#
            )
        };
        let wmts = |n| {
            let layers = many(n, |i| format!("<Layer><ows:Identifier>t{i}</ows:Identifier></Layer>"));
            format!(
                r#"<Capabilities xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0"><Contents>{layers}</Contents></Capabilities>"#
            )
        };
        let wcs2 = |n| {
            let coverages = many(n, |i| format!("<CoverageSummary><CoverageId>c{i}</CoverageId></CoverageSummary>"));
            format!("<Capabilities><Contents>{coverages}</Contents></Capabilities>")
        };
        let wcs1 = |n| {
            let coverages = many(n, |i| format!("<CoverageOfferingBrief><name>c{i}</name></CoverageOfferingBrief>"));
            format!("<WCS_Capabilities><ContentMetadata>{coverages}</ContentMetadata></WCS_Capabilities>")
        };
        let arcgis = |n| serde_json::json!({"layers": list(n, |i| serde_json::json!({"id": i, "name": format!("l{i}")}))}).to_string();
        let folders = |n| serde_json::json!({"folders": list(n, |i| serde_json::json!(format!("f{i}")))}).to_string();
        let services =
            |n| serde_json::json!({"services": list(n, |i| serde_json::json!({"name": format!("s{i}"), "type": "MapServer"}))}).to_string();
        let ogc = |n| {
            let items =
                |i| serde_json::json!({"id": format!("c{i}"), "links": [{"rel": "items", "href": format!("collections/c{i}/items")}]});
            serde_json::json!({"collections": list(n, items)}).to_string()
        };
        // (A WMTS layer needs a tile matrix set to be kept: only the note is looked at.)
        // (ArcGIS's lists are read as far as they are listed.)
        let kinds: [(&str, &str, &dyn Fn(usize) -> String, usize, bool); 9] = [
            ("feature types", "https://x.org/wfs", &wfs, MAX_READ_LAYERS, true),
            ("layers", "https://x.org/wms", &wms, MAX_READ_LAYERS, true),
            ("layers", "https://x.org/wmts", &wmts, MAX_READ_LAYERS, false),
            ("coverages", "https://x.org/wcs", &wcs2, MAX_READ_LAYERS, true),
            ("coverages", "https://x.org/wcs", &wcs1, MAX_READ_LAYERS, true),
            ("layers", "https://x.org/arcgis/rest/services/P/FeatureServer", &arcgis, MAX_LISTED_LAYERS, true),
            ("folders", "https://x.org/arcgis/rest/services", &folders, MAX_LISTED_LAYERS, true),
            ("services", "https://x.org/arcgis/rest/services", &services, MAX_LISTED_LAYERS, true),
            ("collections", "https://x.org/collections", &ogc, MAX_READ_LAYERS, true),
        ];
        for (what, url, doc, limit, kept) in kinds {
            let s = parse(url, doc(limit).as_bytes()).unwrap();
            assert!(!s.notes.iter().any(|n| n.contains(PAST_READ_LIMIT)), "{url}: {:?}", s.notes);
            assert_eq!(s.past_read_limit, None, "{url}");
            assert_eq!(s.layers.len(), if kept { limit } else { 0 }, "{url}");
            let s = parse(url, doc(limit + 2).as_bytes()).unwrap();
            let note = format!("{PAST_READ_LIMIT} {limit} {what} from one document: 2 left out");
            assert!(s.notes.contains(&note), "{url}: {:?}", s.notes);
            assert_eq!(s.past_read_limit.as_ref(), Some(&note), "{url}");
            assert_eq!(s.layers.len(), if kept { limit } else { 0 }, "{url}");
            // What was read past the listing is there to be found, but isn't listed.
            let cut = kept && limit > MAX_LISTED_LAYERS;
            assert_eq!(s.listed, cut.then_some(MAX_LISTED_LAYERS), "{url}");
            assert_eq!(parse(url, doc(MAX_LISTED_LAYERS).as_bytes()).unwrap().listed, None, "{url}");
        }
        // A long request address, copied into each layer, lowers how many are read, though never
        // below the listing.
        let href = format!("https://x.org/{}?", "a".repeat(4000));
        let types = many(MAX_READ_LAYERS, |i| format!("<FeatureType><Name>t{i}</Name></FeatureType>"));
        let long = format!(
            r#"<WFS_Capabilities version="2.0.0"><OperationsMetadata><Operation name="GetFeature"><DCP><HTTP><Get href="{href}"/></HTTP></DCP></Operation></OperationsMetadata><FeatureTypeList>{types}</FeatureTypeList></WFS_Capabilities>"#
        );
        let s = parse("https://x.org/wfs", long.as_bytes()).unwrap();
        let read = s.layers.len();
        assert!((MAX_LISTED_LAYERS..MAX_READ_LAYERS).contains(&read), "{read}");
        let note = format!("{PAST_READ_LIMIT} {read} feature types from one document: {} left out", MAX_READ_LAYERS - read);
        assert_eq!(s.past_read_limit.as_ref(), Some(&note));
        assert!(s.notes.iter().any(|n| n.starts_with("each of its layers copies 40")), "{:?}", s.notes);
        let long_wms = format!(
            r#"<WMS_Capabilities version="1.3.0"><Capability><Request><GetMap><DCPType><HTTP><Get><OnlineResource href="{href}"/></Get></HTTP></DCPType></GetMap></Request><Layer><CRS>EPSG:3857</CRS>{}</Layer></Capability></WMS_Capabilities>"#,
            many(MAX_READ_LAYERS, |i| format!("<Layer><Name>t{i}</Name></Layer>"))
        );
        let s = parse("https://x.org/wms", long_wms.as_bytes()).unwrap();
        assert!((MAX_LISTED_LAYERS..MAX_READ_LAYERS).contains(&s.layers.len()), "{}", s.layers.len());
        assert!(s.notes.iter().any(|n| n.starts_with("each of its layers copies 40")), "{:?}", s.notes);
        // An OGC API's collections without an items link copy the document's own address, which
        // nothing bounds: a long one lowers how many are read too, and a short one doesn't.
        let bare = serde_json::json!({"collections": list(MAX_READ_LAYERS, |i| serde_json::json!({"id": format!("c{i}")}))}).to_string();
        let s = parse(&format!("https://x.org/{}/collections", "a".repeat(4000)), bare.as_bytes()).unwrap();
        let read = s.layers.len();
        assert!((MAX_LISTED_LAYERS..MAX_READ_LAYERS).contains(&read), "{read}");
        let note = format!("{PAST_READ_LIMIT} {read} collections from one document: {} left out", MAX_READ_LAYERS - read);
        assert_eq!(s.past_read_limit.as_ref(), Some(&note));
        assert!(s.notes.iter().any(|n| n.starts_with("each of its layers copies 40")), "{:?}", s.notes);
        let s = parse("https://x.org/collections", bare.as_bytes()).unwrap();
        assert_eq!((s.layers.len(), s.past_read_limit), (MAX_READ_LAYERS, None));
        // A WMS is read in document order: the layers past the limit are its last, and a parent
        // comes before its sublayers.
        let s = parse("https://x.org/wms", wms(MAX_READ_LAYERS + 1).as_bytes()).unwrap();
        assert_eq!(s.layers[0].name, "t0");
        assert_eq!(s.layers[MAX_READ_LAYERS - 1].name, format!("t{}", MAX_READ_LAYERS - 1));
        let nested = r#"<WMS_Capabilities version="1.3.0"><Capability><Layer><CRS>EPSG:3857</CRS><Layer><Name>parent</Name><Layer><Name>child</Name></Layer></Layer><Layer><Name>next</Name></Layer></Layer></Capability></WMS_Capabilities>"#;
        let names: Vec<String> = parse("https://x.org/wms", nested.as_bytes()).unwrap().layers.into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["parent", "child", "next"]);
        // Past the limit here are only layers without a name, so nothing that could be added is
        // left out.
        let layers =
            many(MAX_READ_LAYERS, |i| format!("<Layer><Name>t{i}</Name></Layer>")) + &"<Layer><Title>group</Title></Layer>".repeat(2);
        let doc = format!(
            r#"<WMS_Capabilities version="1.3.0"><Capability><Layer><CRS>EPSG:3857</CRS>{layers}</Layer></Capability></WMS_Capabilities>"#
        );
        let s = parse("https://x.org/wms", doc.as_bytes()).unwrap();
        assert_eq!(s.layers.len(), MAX_READ_LAYERS);
        assert!(!s.notes.iter().any(|n| n.contains(PAST_READ_LIMIT)), "{:?}", s.notes);
        assert_eq!(s.past_read_limit, None);
        // A note quoting the document can carry the words of this one; it isn't taken for it.
        let quoted = format!(
            r#"{{"collections": [{{"id": "{PAST_READ_LIMIT}", "links": [{{"rel": "items", "href": "https://x.org/c/3dtiles"}}]}}]}}"#
        );
        let s = parse("https://x.org/collections", quoted.as_bytes()).unwrap();
        assert!(s.notes.iter().any(|n| n.contains(PAST_READ_LIMIT)), "{:?}", s.notes);
        assert_eq!(s.past_read_limit, None);
    }

    /// A document's format lists are one for all its layers: a document may have 100,000 of them.
    #[test]
    fn a_documents_format_lists_are_shared_by_its_layers() {
        let wcs = r#"<Capabilities version="2.0.1"><ServiceMetadata><formatSupported>image/tiff</formatSupported></ServiceMetadata><Contents><CoverageSummary><CoverageId>a</CoverageId></CoverageSummary><CoverageSummary><CoverageId>b</CoverageId></CoverageSummary></Contents></Capabilities>"#;
        let s = parse("https://x.org/wcs", wcs.as_bytes()).unwrap();
        let [Offer::Coverage(a), Offer::Coverage(b)] = [&s.layers[0].offer, &s.layers[1].offer] else { panic!("{:?}", s.layers) };
        assert_eq!(*a.formats, ["image/tiff"]);
        assert!(std::sync::Arc::ptr_eq(&a.formats, &b.formats));
        let wms = r#"<WMS_Capabilities version="1.3.0"><Capability><Request><GetFeatureInfo><Format>application/json</Format><DCPType><HTTP><Get><OnlineResource href="https://x.org/wms?"/></Get></HTTP></DCPType></GetFeatureInfo></Request><Layer><CRS>EPSG:3857</CRS><Layer queryable="1"><Name>a</Name></Layer><Layer queryable="1"><Name>b</Name></Layer></Layer></Capability></WMS_Capabilities>"#;
        let s = parse("https://x.org/wms", wms.as_bytes()).unwrap();
        let [Some(a), Some(b)] = [&s.layers[0].feature_info, &s.layers[1].feature_info] else { panic!("{:?}", s.layers) };
        assert_eq!(*a.formats, ["application/json"]);
        assert!(std::sync::Arc::ptr_eq(&a.formats, &b.formats));
    }

    /// A coverage whose identifier is too long for an address is counted in a note.
    #[test]
    fn a_coverage_with_a_long_identifier_is_said_to_be_left_out() {
        let long = "c".repeat(501);
        let v2 = format!(
            "<Capabilities><Contents><CoverageSummary><CoverageId>{long}</CoverageId></CoverageSummary><CoverageSummary><CoverageId>c</CoverageId></CoverageSummary></Contents></Capabilities>"
        );
        let v1 = format!(
            "<WCS_Capabilities><ContentMetadata><CoverageOfferingBrief><name>{long}</name></CoverageOfferingBrief><CoverageOfferingBrief><name>c</name></CoverageOfferingBrief></ContentMetadata></WCS_Capabilities>"
        );
        for doc in [v2, v1] {
            let s = parse("https://x.org/wcs", doc.as_bytes()).unwrap();
            assert_eq!(s.layers.len(), 1);
            assert_eq!(s.notes, ["1 coverages are left out: their identifiers are longer than 500 bytes"]);
            let clean = doc.replace(&long, "d");
            assert!(parse("https://x.org/wcs", clean.as_bytes()).unwrap().notes.is_empty());
        }
    }

    /// An OGC API past what's listed is read whole, and still marked for those that need it whole.
    #[test]
    fn collections_past_the_listing_are_read_and_marked_incomplete() {
        let doc = |n: usize| {
            let collections: Vec<_> = (0..n)
                .map(|i| serde_json::json!({"id": format!("c{i}"), "links": [{"rel": "items", "href": format!("collections/c{i}/items")}]}))
                .collect();
            serde_json::json!({"collections": collections}).to_string()
        };
        let s = parse("https://x.org/collections", doc(MAX_LISTED_LAYERS).as_bytes()).unwrap();
        assert!(!s.incomplete);
        assert_eq!(s.listed, None);
        let s = parse("https://x.org/collections", doc(MAX_LISTED_LAYERS + 1).as_bytes()).unwrap();
        assert!(s.incomplete);
        assert_eq!(s.layers.len(), MAX_LISTED_LAYERS + 1);
        assert_eq!(s.listed, Some(MAX_LISTED_LAYERS));
        assert!(!s.notes.iter().any(|n| n.contains(PAST_READ_LIMIT)), "{:?}", s.notes);
    }
}
