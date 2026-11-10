//! Web tile layers: basemaps and overlays drawn from tile services, under the map's own layers.
//!
//! A map keeps a stack of them ([`crate::Map::tiles`], index 0 on top) over its built-in basemap.
//! Three kinds of service are read: XYZ/TMS raster tiles (PNG, JPEG or WebP, and WMS through a
//! `{bbox-epsg-3857}` template), Mapbox Vector Tiles drawn with a built-in style, and MapLibre
//! style documents (vector and raster sources together). The tiles themselves are fetched and
//! drawn by `rustgis-tiles` and the renderer; this is only what the project saves.

use serde::{Deserialize, Serialize};

/// The most tile layers a map holds.
pub const MAX_TILE_LAYERS: usize = 32;

/// What a tile layer's address points at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TileKind {
    /// Raster tiles from a URL template (`{z}/{x}/{y}`, `{-y}`, `{s}`, `{q}`, `{bbox-epsg-3857}`).
    #[default]
    Raster,
    /// Mapbox Vector Tiles from a URL template, or a TileJSON document, in a built-in style.
    Vector,
    /// A MapLibre style document (`style.json`).
    Style,
    /// A PMTiles archive of raster or vector tiles: a web address (read by byte ranges) or, on the
    /// desktop, a file.
    #[serde(rename = "pmtiles")]
    PmTiles,
    /// An MBTiles archive of raster or vector tiles: a file on the desktop, or `upload:<name>`
    /// for one added to the web app (kept in memory, not in the project).
    #[serde(rename = "mbtiles")]
    MbTiles,
    /// A cloud-optimised GeoTIFF by its web address: drawn from the overview and tiles the view
    /// needs, read by byte range.
    #[serde(rename = "cog")]
    Cog,
}

impl TileKind {
    pub const ALL: [TileKind; 6] =
        [TileKind::Raster, TileKind::Vector, TileKind::Style, TileKind::PmTiles, TileKind::MbTiles, TileKind::Cog];
    pub fn code(self) -> &'static str {
        match self {
            TileKind::Raster => "raster",
            TileKind::Vector => "vector",
            TileKind::Style => "style",
            TileKind::PmTiles => "pmtiles",
            TileKind::MbTiles => "mbtiles",
            TileKind::Cog => "cog",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            TileKind::Raster => "Raster tiles (XYZ)",
            TileKind::Vector => "Vector tiles (MVT)",
            TileKind::Style => "Map style (JSON)",
            TileKind::PmTiles => "PMTiles archive",
            TileKind::MbTiles => "MBTiles archive",
            TileKind::Cog => "Cloud-optimised GeoTIFF",
        }
    }
    pub fn from_code(s: &str) -> Option<TileKind> {
        let s = s.trim();
        match s.to_ascii_lowercase().as_str() {
            "raster" | "xyz" | "tms" | "wms" => Some(TileKind::Raster),
            "vector" | "mvt" | "pbf" => Some(TileKind::Vector),
            "style" | "maplibre" | "mapbox" => Some(TileKind::Style),
            "pmtiles" => Some(TileKind::PmTiles),
            "mbtiles" => Some(TileKind::MbTiles),
            "cog" | "geotiff" => Some(TileKind::Cog),
            _ => None,
        }
    }
    /// The kind an address most likely is, from how it looks.
    pub fn guess(url: &str) -> TileKind {
        let url = rustgis_data::login_address::inner(url);
        let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
        if path.ends_with(".pmtiles") {
            TileKind::PmTiles
        } else if path.ends_with(".mbtiles") {
            TileKind::MbTiles
        } else if path.ends_with(".pbf") || path.ends_with(".mvt") || path.contains("/{z}/{x}/{y}.vector") {
            TileKind::Vector
        } else if path.ends_with("style.json") || (path.contains("/styles/") && !path.contains("{z}")) {
            TileKind::Style
        } else {
            TileKind::Raster
        }
    }
}

/// A tile layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileLayer {
    pub name: String,
    pub kind: TileKind,
    /// The URL template, TileJSON or style address.
    pub url: String,
    /// Advertised longitude/latitude coverage (west, south, east, north), kept from service
    /// capabilities so Zoom To Layer works without fetching the service again.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_bounds")]
    pub bounds: Option<[f64; 4]>,
    /// Subdomains that `{s}` takes in turn (`a`, `b`, `c` when empty and the URL has `{s}`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subdomains: Vec<String>,
    #[serde(default)]
    pub min_zoom: u8,
    #[serde(default = "max_zoom")]
    pub max_zoom: u8,
    /// Pixels per tile side (256, or 512 for most vector and high-density raster tiles).
    #[serde(default = "tile_size")]
    pub tile_size: u32,
    /// `y` counts from the south (TMS).
    #[serde(default, skip_serializing_if = "is_false")]
    pub tms: bool,
    /// The credit the provider asks for, shown in the corner of the map.
    #[serde(default)]
    pub attribution: String,
    #[serde(default = "yes")]
    pub visible: bool,
    /// 0 = invisible, 1 = opaque.
    #[serde(default = "one")]
    pub opacity: f32,
    /// A transparent layer meant to sit over a basemap (labels, roads, traffic).
    #[serde(default, skip_serializing_if = "is_false")]
    pub overlay: bool,
    /// The map layer it's drawn just over, under the next one up (#808): new overlays go on top
    /// of the map's layers, as every new layer does. None: under all the map layers, where
    /// basemaps sit, as every tile layer did before (and does in a project saved then).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<u32>,
    /// How it blends with the tile layers and basemap beneath (Multiply drapes imagery over a
    /// hillshade).
    #[serde(default, skip_serializing_if = "is_normal")]
    pub blend: crate::effects::Blend,
    /// The catalog entry it came from (`osm`, `esri-imagery`), when it came from one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// Vector tiles' layer names, when the service gave them (each drawn in a colour of its own).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// MapLibre style layers (a JSON array) that draw vector tiles instead of their own colours,
    /// as a dataset's own look (Fields of the World's fields by score); each layer's `source` is
    /// the tiles' own. Empty for the default look.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub style_layers: String,
    /// Vector tiles' source layers switched off in the Layers pane (`roads`, `pois`): the style
    /// layers drawing them are left out, so they neither draw nor answer identify.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_source_layers")]
    pub hidden_layers: Vec<String>,
    /// A URL with date placeholders (`{date:YYYY-MM-DD}`), filled from the time slider into
    /// `url` at each step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_url: Option<String>,
    /// The date `url` was last filled with.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub time_at: f64,
    /// A file per date, as a STAC data cube's tile lists them (opengeos/RustGIS#559): (date,
    /// address), in order. The time slider shows the last date in its window; `url` is it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub time_urls: Vec<(f64, String)>,
    /// No date of `time_urls` falls in the slider's window: the layer draws nothing for now.
    #[serde(skip)]
    pub time_hidden: bool,
    /// Why the archive can't be read (a moved file, or a relative path with no project folder): it draws nothing.
    #[serde(skip)]
    pub error: Option<String>,
    /// A WMS layer's GetFeatureInfo request, from the service's capabilities: what identify asks
    /// it. Kept for the session only (a project's layer reads the capabilities again); the type
    /// can be saved, should projects keep it.
    #[serde(skip)]
    pub feature_info: Option<rustgis_data::ogc::FeatureInfo>,
    /// Its legend pictures (a WMS or WMTS layer's, from its service's default style, or given to
    /// `map.addTiles`): the Layers pane shows the one for the map's scale under it, as ArcGIS
    /// Pro's Contents pane does. Web addresses only, at most [`MAX_LEGENDS`](rustgis_data::ogc::MAX_LEGENDS).
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_legends")]
    pub legends: Vec<rustgis_data::ogc::Legend>,
    /// Expanded in the Layers pane, its legend and source layers showing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub expanded: bool,
    /// The saved connection it was added from (its id, in the app's preferences), so a project
    /// reopened later logs in with that connection's login and never needs a token in its
    /// address. Empty for a layer that came from no connection. Never a secret.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub connection: String,
    /// Added by an anonymous plugin command: none of its addresses may acquire a saved
    /// web-service login, including after saving and reopening the project.
    #[serde(default, skip_serializing_if = "is_false")]
    pub anonymous: bool,
    /// Picked in the Layers pane (`map.selectTiles`), for the session only: it holds while no
    /// map layer is active (the engine's `Session::selected_tile`).
    #[serde(skip)]
    pub selected: bool,
    /// A layer drawn on request ([`TileLayer::on_request`]: a WMS, a map service's export) asked
    /// for one picture of the view instead of tiles (#1162), so its symbols and labels aren't cut
    /// at tile edges. Where that can't be (the globe, a tilted map, a projection its service
    /// doesn't offer) it is drawn in tiles. Off in a project saved before: tiles, as then.
    #[serde(default, skip_serializing_if = "is_false")]
    pub single_image: bool,
}

/// The longest legend address kept.
pub const MAX_LEGEND_URL: usize = 4096;

/// Legend pictures as a tile layer keeps them: web addresses of a sane length, scales that are
/// positive numbers (others are open-ended), at most [`MAX_LEGENDS`](rustgis_data::ogc::MAX_LEGENDS).
pub fn checked_legends(legends: impl IntoIterator<Item = rustgis_data::ogc::Legend>) -> Vec<rustgis_data::ogc::Legend> {
    let scale = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
    legends
        .into_iter()
        .map(|l| rustgis_data::ogc::Legend { url: l.url.trim().to_string(), min_scale: scale(l.min_scale), max_scale: scale(l.max_scale) })
        .filter(|l| is_web(&l.url) && rustgis_data::login_address::inner(&l.url).len() <= MAX_LEGEND_URL)
        .take(rustgis_data::ogc::MAX_LEGENDS)
        .collect()
}

/// A project's legends, checked; one that isn't a legend is left out, and a value that isn't a
/// list is none, so a damaged entry doesn't make the project unreadable.
fn lenient_legends<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<rustgis_data::ogc::Legend>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let list = match value {
        serde_json::Value::Array(list) => list,
        _ => Vec::new(),
    };
    // Each read field by field: a scale of no number is open-ended, not the legend's loss.
    let legend = |v: serde_json::Value| {
        let url = v.get("url")?.as_str()?.to_string();
        let scale = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
        Some(rustgis_data::ogc::Legend { url, min_scale: scale("minScale"), max_scale: scale("maxScale") })
    };
    Ok(checked_legends(list.into_iter().filter_map(legend)))
}

/// The most dates a tile layer lists (as the time slider steps through at most).
pub const MAX_TIME_URLS: usize = crate::time::MAX_STEPS;

/// Whether an address is on the web (`http://` or `https://`, in any case).
pub fn is_web(url: &str) -> bool {
    let url = rustgis_data::login_address::inner(url);
    let scheme = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    scheme.starts_with("https://") || scheme.starts_with("http://")
}

/// Whether coverage is a finite, ordered geographic rectangle (west, south, east, north).
/// Half a degree of tolerance matches service capabilities parsing.
pub fn valid_bounds([w, s, e, n]: &[f64; 4]) -> bool {
    [*w, *s, *e, *n].iter().all(|v| v.is_finite()) && -180.5 <= *w && *w < *e && *e <= 180.5 && -90.5 <= *s && *s < *n && *n <= 90.5
}

/// Discards malformed optional project metadata after consuming its entire JSON value.
fn lenient_bounds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<[f64; 4]>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

fn is_zero(x: &f64) -> bool {
    *x == 0.0
}

/// The longest `style_layers` kept (JSON text).
pub const MAX_STYLE_LAYERS: usize = 64 << 10;
/// The most style layers kept (as many as the tiles crate draws of a layer's own style).
pub const MAX_STYLE_LAYER_COUNT: usize = 256;

/// Style layers given for vector tiles, checked: a JSON array of objects, not too long.
pub fn checked_style_layers(text: &str) -> Result<String, String> {
    if text.len() > MAX_STYLE_LAYERS {
        return Err(format!("style layers are at most {} KB", MAX_STYLE_LAYERS >> 10));
    }
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Array(a)) if a.len() > MAX_STYLE_LAYER_COUNT => Err(format!("at most {MAX_STYLE_LAYER_COUNT} style layers")),
        Ok(serde_json::Value::Array(a)) if a.iter().all(serde_json::Value::is_object) => Ok(text.to_string()),
        _ => Err("style layers are a JSON array of MapLibre style layers".into()),
    }
}

/// The most source layers a tile layer lists or hides.
pub const MAX_SOURCE_LAYERS: usize = rustgis_data::source_layers::MAX;

/// Source layer names as a tile layer keeps them: as
/// [`source_layers::distinct`](rustgis_data::source_layers::distinct) keeps an archive's, sorted
/// so the same set is the same list.
pub fn checked_source_layers<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut kept = rustgis_data::source_layers::distinct(names);
    kept.sort_unstable();
    kept.into_iter().map(str::to_string).collect()
}

/// A project's hidden source layers, checked; a name that isn't text is left out, and a value
/// that isn't a list is none, so a damaged entry doesn't make the project unreadable.
fn lenient_source_layers<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let list = match &value {
        serde_json::Value::Array(list) => list.as_slice(),
        _ => &[],
    };
    Ok(checked_source_layers(list.iter().filter_map(serde_json::Value::as_str)))
}

fn max_zoom() -> u8 {
    19
}
fn tile_size() -> u32 {
    256
}
fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn is_normal(b: &crate::effects::Blend) -> bool {
    *b == crate::effects::Blend::Normal
}

/// The deepest zoom level tiles are requested at.
pub const MAX_ZOOM: u8 = 24;

pub use rustgis_data::ogc::warp::sized;

/// Whether a raster tile address is one its service draws on request, for any box and size
/// ([`TileLayer::on_request`]): asked for by its box, with a size [`sized`] can write (a WMS's
/// `WIDTH` and `HEIGHT`, a map service export's `size`, as numbers or size placeholders). An address
/// with a box and no such size (a generic template) is asked for at its service's default size,
/// so it is drawn in its own tiles.
pub fn drawn_on_request(url: &str) -> bool {
    let base = url.split('#').next().unwrap_or_default();
    (base.contains("{bbox-epsg-3857}") || base.contains("{nbbox}")) && sizable(base)
}

/// Whether a picture's size can be written into `base`: it has the size keys, each a number
/// ([`sized`] writes it) or a size placeholder (`{width}`, `{height}`, `{nw}`, `{nh}`).
fn sizable(base: &str) -> bool {
    let Some((path, query)) = base.split_once('?') else { return false };
    let lower = path.to_ascii_lowercase();
    let keys: &[&str] = if lower.ends_with("/export") || lower.ends_with("/exportimage") { &["size"] } else { &["width", "height"] };
    keys.iter().all(|key| {
        query.split('&').filter_map(|p| p.split_once('=')).any(|(k, v)| {
            k.eq_ignore_ascii_case(key)
                && (!v.contains('{') || matches!(v, "{nw}" | "{nh}" | "{nw},{nh}" | "{width}" | "{height}" | "{width},{height}"))
        })
    })
}

/// Content discarded or settings changed while checking a tile layer read from a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileRepairs {
    /// Invalid, repeated or excess dated addresses removed.
    pub addresses: usize,
    /// Invalid or excess layer names and subdomain entries removed.
    pub entries: usize,
    /// Unreadable style definitions removed.
    pub styles: usize,
    /// Address, name, attribution or identifier fields shortened or replaced.
    pub text: usize,
    /// Coverage or numeric settings removed or brought into range.
    pub settings: usize,
}

impl TileLayer {
    /// The local archive that Repair Data Source replaces.
    pub fn archive_file(&self) -> Option<&str> {
        (matches!(self.kind, TileKind::PmTiles | TileKind::MbTiles) && is_local_archive(&self.url) && !self.url.starts_with(UPLOAD))
            .then_some(self.url.as_str())
    }

    pub fn new(name: impl Into<String>, kind: TileKind, url: impl Into<String>) -> TileLayer {
        TileLayer {
            name: name.into(),
            kind,
            url: url.into(),
            bounds: None,
            subdomains: Vec::new(),
            min_zoom: 0,
            max_zoom: max_zoom(),
            tile_size: tile_size(),
            tms: false,
            attribution: String::new(),
            visible: true,
            opacity: 1.0,
            overlay: false,
            over: None,
            blend: crate::effects::Blend::Normal,
            id: String::new(),
            layers: Vec::new(),
            style_layers: String::new(),
            hidden_layers: Vec::new(),
            time_url: None,
            time_at: 0.0,
            time_urls: Vec::new(),
            time_hidden: false,
            error: None,
            feature_info: None,
            legends: Vec::new(),
            connection: String::new(),
            anonymous: false,
            expanded: false,
            selected: false,
            single_image: false,
        }
    }

    /// Whether the service draws its pictures on request, for any box and size (a WMS's GetMap,
    /// a map service's export), rather than cutting them from a tile set: what can be asked for
    /// as one picture of the view, or in tiles of another size.
    pub fn on_request(&self) -> bool {
        self.kind == TileKind::Raster && drawn_on_request(&self.url)
    }

    /// Whether its tiles are asked for in Web Mercator by their box, so their size in pixels is
    /// the layer's to choose (256, 512 or 1024): not one warped from a system of its own.
    pub fn sized_tiles(&self) -> bool {
        let (base, fragment) = self.url.split_once('#').unwrap_or((&self.url, ""));
        // Asked for in Web Mercator: with no mark, or one saying so (a WMS that offers other
        // systems too, drawn in Web Mercator as it comes). A `{width}` placeholder is filled
        // with 256 whatever the tiles' size.
        let mercator = fragment.is_empty() || fragment.split(';').any(|p| p == "crs=3857");
        self.kind == TileKind::Raster
            && base.contains("{bbox-epsg-3857}")
            && !base.contains("{width}")
            && !base.contains("{height}")
            && sizable(base)
            && mercator
    }

    /// Writes its tile size into its address (and its dated template's), for a layer asked for
    /// by its tiles' box: their pictures are asked for that many pixels a side.
    pub fn size_tiles(&mut self) {
        if !self.sized_tiles() {
            return;
        }
        let side = self.tile_size.to_string();
        self.url = sized(&self.url, &side, &side);
        if let Some(template) = self.time_url.as_mut() {
            *template = sized(template, &side, &side);
        }
    }

    /// Whether the layer draws now: shown, and with a date in the time slider's window when it
    /// follows one.
    pub fn draws(&self) -> bool {
        self.visible && !self.time_hidden && self.error.is_none()
    }

    /// Selects this tile's date and address from a normalized slider, without fetching it.
    /// With no slider, a dated list shows its last entry and a template keeps its saved date.
    pub fn apply_time(&mut self, time: Option<&crate::time::TimeSlider>) {
        let window = time.map(crate::time::TimeSlider::window);
        let current = time.map(|t| t.current);
        if !self.time_urls.is_empty() {
            let at = match window {
                Some((from, to)) => self.time_urls.iter().rposition(|(d, _)| *d >= from && *d < to),
                None => self.time_urls.len().checked_sub(1),
            };
            self.time_hidden = at.is_none();
            if let Some((d, u)) = at.and_then(|i| self.time_urls.get(i)) {
                self.url.clone_from(u);
                self.time_at = *d;
            }
            return;
        }
        // Only web addresses take dates (a file's template is dropped when it is repaired).
        if let Some(template) = self.time_url.as_ref().filter(|u| is_web(u)) {
            self.url = rustgis_data::time::fill_template(template, current.unwrap_or(self.time_at));
            self.time_at = current.unwrap_or(self.time_at);
        }
    }

    /// Checks the address and brings the numbers into range, after reading one from a project
    /// file or a command.
    pub fn repair(&mut self) -> Result<(), String> {
        self.repair_counted().map(|_| ())
    }

    /// Applies the same checks, returning losses for a reader or command to report on success.
    pub fn repair_counted(&mut self) -> Result<TileRepairs, String> {
        let mut repairs = TileRepairs::default();
        repairs.text += usize::from(self.url.trim().len() != self.url.len());
        let dated = self.time_urls.len() + usize::from(self.time_url.is_some());
        self.url = self.url.trim().to_string();
        // A dated address is a web address whose date is filled in after the scheme and host.
        if self.time_url.as_ref().is_some_and(|u| !is_web(u) || rustgis_data::login_address::inner(u).len() > 4096) {
            self.time_url = None;
        }
        // A file per date: web addresses, finite dates, in order.
        self.time_urls.retain(|(d, u)| d.is_finite() && is_web(u) && rustgis_data::login_address::inner(u).len() <= 4096);
        // Sorted before the cap, so a list out of order keeps its first dates, not any.
        self.time_urls.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.time_urls.dedup_by(|a, b| a.0 == b.0);
        self.time_urls.truncate(MAX_TIME_URLS);
        repairs.addresses = dated - self.time_urls.len() - usize::from(self.time_url.is_some());
        let local = matches!(self.kind, TileKind::PmTiles | TileKind::MbTiles) && is_local_archive(&self.url);
        if let Some(name) = self.url.strip_prefix(UPLOAD).filter(|_| self.kind == TileKind::MbTiles) {
            if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
                return Err("an uploaded MBTiles file needs a name".into());
            }
        } else if local {
            if self.error.is_none() {
                check_path(&self.url, if self.kind == TileKind::MbTiles { "mbtiles" } else { "pmtiles" })?;
            }
        } else {
            check_url(&self.url)?;
        }
        repairs.text += usize::from(self.name.trim().len() != self.name.len() || self.name.chars().nth(120).is_some());
        self.name = self.name.trim().chars().take(120).collect();
        if self.name.is_empty() {
            let file = || {
                std::path::Path::new(self.url.trim_start_matches(UPLOAD).trim_start_matches("file://"))
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            };
            self.name = if local { file() } else { host_of(&self.url).map(str::to_string) }.unwrap_or_else(|| "Tiles".into());
        }
        // Layer names (a project file is hostile input): at most 64, each short and printable.
        let entries = self.layers.len() + self.subdomains.len();
        self.layers.retain(|l| !l.trim().is_empty() && l.len() <= 200 && !l.chars().any(char::is_control));
        self.layers.truncate(64);
        // A project file's style layers that don't read are dropped: the tiles keep their colours.
        if !self.style_layers.is_empty() && checked_style_layers(&self.style_layers).is_err() {
            self.style_layers.clear();
            repairs.styles += 1;
        }
        // Bounds from a project must describe a finite geographic rectangle. Invalid metadata
        // doesn't make the tiles unreadable; it only leaves their coverage unknown.
        let settings = (self.bounds, self.min_zoom, self.max_zoom, self.tile_size, self.opacity);
        self.bounds = self.bounds.filter(valid_bounds);
        self.max_zoom = self.max_zoom.min(MAX_ZOOM);
        self.min_zoom = self.min_zoom.min(self.max_zoom);
        self.tile_size = match self.tile_size {
            ..384 => 256,
            // A layer asked for by its tiles' box takes tiles of 1024 pixels too.
            768.. if self.sized_tiles() => 1024,
            _ => 512,
        };
        if !self.opacity.is_finite() {
            self.opacity = 1.0;
        }
        self.opacity = self.opacity.clamp(0.0, 1.0);
        self.subdomains.retain(|s| !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        self.subdomains.truncate(16);
        repairs.entries = entries - self.layers.len() - self.subdomains.len();
        repairs.settings = usize::from(settings.0 != self.bounds)
            + usize::from(settings.1 != self.min_zoom)
            + usize::from(settings.2 != self.max_zoom)
            + usize::from(settings.3 != self.tile_size)
            + usize::from(settings.4 != self.opacity);
        repairs.text += usize::from(self.attribution.chars().nth(500).is_some());
        repairs.text += usize::from(
            self.id.chars().any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))) || self.id.chars().nth(80).is_some(),
        );
        self.attribution = self.attribution.chars().take(500).collect();
        self.id = self.id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).take(80).collect();
        Ok(repairs)
    }
}

/// The prefix of an MBTiles archive added to the web app, held in memory.
pub const UPLOAD: &str = "upload:";

/// Whether a PMTiles or MBTiles archive address is a file rather than a web address. An address
/// wrapped for an S3 connection or service login is never a file, well formed or not.
pub fn is_local_archive(url: &str) -> bool {
    if rustgis_data::s3_address::is_wrapper(url) || rustgis_data::login_address::is_wrapper(url) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("s3://"))
}

/// Whether an archive's file path may be read: absolute, a `.<ext>` file (`pmtiles`,
/// `mbtiles`), no longer than 4096 bytes.
pub fn check_path(path: &str, ext: &str) -> Result<(), String> {
    let p = path.strip_prefix("file://").unwrap_or(path);
    if p.len() > 4096 || p.chars().any(char::is_control) {
        return Err("the path is too long or has control characters".into());
    }
    let windows_drive = p.as_bytes().get(1) == Some(&b':') && p.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    // Not a network share (\\server\share, or file://server/…), which a project could use to
    // make Windows connect to another machine.
    let share = p.starts_with("\\\\") || p.starts_with("//") || (path.starts_with("file://") && !p.starts_with('/'));
    if share || !(p.starts_with('/') || windows_drive) {
        return Err(format!("'{p}': give the full path of the archive on this computer"));
    }
    if !p.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
        return Err(format!("'{p}' isn't a .{ext} file"));
    }
    Ok(())
}

/// Whether `url` may be fetched: HTTP, HTTPS or S3, with a host, and no longer than 4096 bytes.
/// Connection and service-login wrappers are judged by their inner address and their own bound;
/// no live login is needed to keep a saved address.
pub fn check_url(url: &str) -> Result<(), String> {
    let url = match rustgis_data::login_address::parse(url) {
        Ok(Some(b)) => b.url,
        Ok(None) => url,
        Err(e) => return Err(e.to_string()),
    };
    let url = match rustgis_data::s3_address::parse(url) {
        Ok(Some(b)) => b.url,
        Ok(None) => url,
        Err(e) => return Err(e.to_string()),
    };
    if url.len() > 4096 {
        return Err("the address is too long".into());
    }
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("s3://")) {
        return Err(format!("'{url}': tile addresses start with https://, http:// or s3://"));
    }
    if host_of(url).is_none_or(str::is_empty) {
        return Err(format!("'{url}' has no host"));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("'{url}' has spaces in it"));
    }
    Ok(())
}

/// The host part of an HTTP(S) address, inspecting the address inside a connection or login wrapper.
pub fn host_of(url: &str) -> Option<&str> {
    let url = rustgis_data::login_address::parse(url).ok()?.map_or(url, |b| b.url);
    let url = rustgis_data::s3_address::inner(url);
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    Some(host.split(':').next().unwrap_or(host))
}

impl TileLayer {
    /// All persisted tile addresses, with the archive URL also classified as a local path.
    pub(crate) fn output_slots(&self) -> Vec<(crate::TileRole, &str, bool)> {
        let TileLayer {
            name: _,
            kind: _,
            url,
            bounds: _,
            subdomains: _,
            min_zoom: _,
            max_zoom: _,
            tile_size: _,
            tms: _,
            attribution: _,
            visible: _,
            opacity: _,
            overlay: _,
            over: _,
            blend: _,
            id: _,
            layers: _,
            style_layers: _,
            hidden_layers: _,
            time_url,
            time_at: _,
            time_urls,
            time_hidden: _,
            error: _,
            feature_info: _,
            legends,
            connection: _,
            anonymous: _,
            expanded: _,
            selected: _,
            single_image: _,
        } = self;
        use crate::TileRole as R;
        let mut out = vec![(R::Url, url.as_str(), self.archive_file().is_some())];
        if let Some(v) = time_url {
            out.push((R::TimeUrl, v.as_str(), false));
        }
        out.extend(time_urls.iter().enumerate().map(|(i, (_, v))| (R::TimeAt(i), v.as_str(), false)));
        out.extend(legends.iter().enumerate().map(|(i, v)| {
            let rustgis_data::ogc::Legend { url, min_scale: _, max_scale: _ } = v;
            (R::Legend(i), url.as_str(), false)
        }));
        out
    }

    /// Resolves a fixed location, independent of the address's current spelling.
    pub(crate) fn output_slot_mut(&mut self, role: crate::TileRole) -> Option<&mut String> {
        match role {
            crate::TileRole::Url => Some(&mut self.url),
            crate::TileRole::TimeUrl => self.time_url.as_mut(),
            crate::TileRole::TimeAt(i) => Some(&mut self.time_urls.get_mut(i)?.1),
            crate::TileRole::Legend(i) => Some(&mut self.legends.get_mut(i)?.url),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_bound_tile_and_legend_addresses_are_web_addresses() {
        let bound = rustgis_data::login_address::bind("0123456789abcdef", "https://maps.test", "https://maps.test/style.json").unwrap();
        assert!(is_web(&bound));
        assert!(check_url(&bound).is_ok());
        assert_eq!(host_of(&bound), Some("maps.test"));
        assert_eq!(TileKind::guess(&bound), TileKind::Style);
        let legend = rustgis_data::ogc::Legend { url: bound.clone(), ..Default::default() };
        assert_eq!(checked_legends([legend]).first().unwrap().url, bound);
        assert!(check_url("rustgis-login:broken").is_err());
        assert!(!is_web("rustgis-login:broken"));
    }

    #[test]
    fn same_origin_legend_with_a_space_keeps_its_login() {
        let document = rustgis_data::login_address::bind("0123456789abcdef", "https://maps.test", "https://maps.test/wms").unwrap();
        let url = rustgis_data::login_address::inherit(&document, "https://maps.test/legend?LAYER=a b");
        let legend = rustgis_data::ogc::Legend { url: url.clone(), ..Default::default() };
        assert_eq!(checked_legends([legend]).first().map(|l| l.url.as_str()), Some(url.as_str()));
        assert_eq!(rustgis_data::login_address::inner(&url), "https://maps.test/legend?LAYER=a%20b");
        assert!(check_url(&url).is_ok());
    }

    #[test]
    fn style_layers_are_checked() {
        assert!(checked_style_layers(r#"[{"id": "a", "type": "fill"}]"#).is_ok());
        assert!(checked_style_layers("{}").is_err());
        assert!(checked_style_layers("[1]").is_err());
        let many = format!("[{}]", vec!["{}"; MAX_STYLE_LAYER_COUNT + 1].join(","));
        assert!(checked_style_layers(&many).is_err());
    }

    #[test]
    fn guesses_kinds() {
        assert_eq!(TileKind::guess("https://tile.openstreetmap.org/{z}/{x}/{y}.png"), TileKind::Raster);
        assert_eq!(TileKind::guess("https://example.com/tiles/{z}/{x}/{y}.pbf?key=1"), TileKind::Vector);
        assert_eq!(TileKind::guess("https://tiles.openfreemap.org/styles/liberty"), TileKind::Style);
        assert_eq!(TileKind::guess("https://api.maptiler.com/maps/streets/style.json?key=k"), TileKind::Style);
    }

    #[test]
    fn repairs() {
        let mut t = TileLayer::new("  ", TileKind::Raster, " https://a.tile.example.com/{z}/{x}/{y}.png ");
        t.max_zoom = 200;
        t.min_zoom = 250;
        t.opacity = f32::NAN;
        t.tile_size = 7;
        assert!(t.repair().is_ok());
        assert_eq!(t.name, "a.tile.example.com");
        assert_eq!((t.min_zoom, t.max_zoom, t.tile_size, t.opacity), (MAX_ZOOM, MAX_ZOOM, 256, 1.0));
        for bad in ["file:///etc/passwd", "ftp://x/{z}", "https://", "https:// x.com/{z}", "javascript:alert(1)"] {
            assert!(TileLayer::new("x", TileKind::Raster, bad).repair().is_err(), "{bad}");
        }
    }

    #[test]
    fn invalid_coverage_does_not_hide_tiles() {
        let mut t = TileLayer::new("WMS", TileKind::Raster, "https://maps.example.com/wms");
        for bounds in [
            [f64::NAN, 40.0, 10.0, 50.0],
            [0.0, 40.0, f64::INFINITY, 50.0],
            [10.0, 40.0, 0.0, 50.0],
            [0.0, 50.0, 10.0, 40.0],
            [0.0, 40.0, 0.0, 50.0],
            [0.0, 91.0, 10.0, 92.0],
            [-200.0, 40.0, 10.0, 50.0],
        ] {
            t.bounds = Some(bounds);
            t.repair().unwrap();
            assert_eq!(t.bounds, None, "{bounds:?}");
            assert!(t.visible);
        }
        t.bounds = Some([-180.0, -90.0, 180.0, 90.0]);
        t.repair().unwrap();
        assert_eq!(t.bounds, Some([-180.0, -90.0, 180.0, 90.0]));
    }

    #[test]
    fn reads_old_and_short_json() {
        let t: TileLayer =
            serde_json::from_str(r#"{"name": "OSM", "kind": "raster", "url": "https://tile.openstreetmap.org/{z}/{x}/{y}.png"}"#)
                .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.visible && t.opacity == 1.0 && t.max_zoom == 19 && t.tile_size == 256);
    }

    /// A tile layer's legends and whether they show are kept; a layer without them is written as
    /// it always was; damaged legends are left out without the rest failing to read.
    #[test]
    fn legends_are_kept_and_old_layers_save_as_before() {
        use rustgis_data::ogc::{Legend, MAX_LEGENDS};
        let old = r#"{"name":"WMS","kind":"raster","url":"https://maps.example.org/wms?{bbox-epsg-3857}"}"#;
        let t: TileLayer = serde_json::from_str(old).unwrap();
        assert!(t.legends.is_empty() && !t.expanded);
        let saved = serde_json::to_value(&t).unwrap();
        assert!(saved.get("legends").is_none() && saved.get("expanded").is_none());
        let mut t = t;
        t.legends = vec![
            Legend { url: "https://maps.example.org/legend.png".into(), ..Legend::default() },
            Legend { url: "https://maps.example.org/far.png".into(), min_scale: Some(5e4), max_scale: None },
        ];
        t.expanded = true;
        let text = serde_json::to_string(&t).unwrap();
        assert!(
            text.contains(
                r#""legends":[{"url":"https://maps.example.org/legend.png"},{"url":"https://maps.example.org/far.png","minScale":50000.0}]"#
            ),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<TileLayer>(&text).unwrap(), t);
        // Damaged: entries that aren't legends, not on the web, scales of no number, too many.
        let many: Vec<String> = (0..40).map(|i| format!(r#""https://x.org/{i}.png""#)).collect();
        for (legends, want) in [
            (r#""pink""#.to_string(), 0),
            (
                r#"[7, {"url": 3}, {"url": "file:///etc/passwd"}, {"url": "https://ok.org/l.png", "minScale": -4, "maxScale": "x"}]"#
                    .into(),
                1,
            ),
            (format!("[{}]", many.iter().map(|u| format!(r#"{{"url": {u}}}"#)).collect::<Vec<_>>().join(",")), MAX_LEGENDS),
        ] {
            let t: TileLayer =
                serde_json::from_str(&format!(r#"{{"name":"W","kind":"raster","url":"https://w.org/{{z}}","legends":{legends}}}"#))
                    .unwrap_or_else(|e| panic!("{legends}: {e}"));
            assert_eq!(t.legends.len(), want, "{legends}");
            assert!(t.legends.iter().all(|l| l.min_scale.is_none() && l.max_scale.is_none() || l.min_scale.is_some_and(|m| m > 0.0)));
        }
        assert_eq!(checked_legends([Legend { url: format!("https://x.org/{}", "a".repeat(MAX_LEGEND_URL)), ..Legend::default() }]), []);
    }
}
