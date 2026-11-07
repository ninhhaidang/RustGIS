//! Satellite embeddings: a catalog of pre-computed embedding datasets (vectors a geospatial
//! foundation model made for every pixel or image patch), and finding and reading those RustGIS
//! shows where the map is.
//!
//! - AlphaEarth Foundations (Google's Satellite Embedding): 64 signed-byte bands a 10 m pixel, in
//!   UTM tiles of 8 192² pixels, one GeoTIFF a tile and year. The tiles are found in the dataset's
//!   GeoParquet index, read in place (its bounding boxes, then the addresses of the rows that
//!   meet the map), and shown as cloud-optimised GeoTIFFs, three bands as red, green and blue
//!   ([`aef_look`]), read a band at a time.
//! - Earth Index (Earth Genome): a 384-value vector every ~320 m, one GeoParquet a Sentinel-2
//!   (MGRS) tile. The tiles are named from where the map is ([`mgrs_tile`]); a tile's points in
//!   the view are read in place and coloured by their vectors' first three principal components
//!   ([`pca_colors`]).
//! - Tessera (Cambridge): 128 values a 10 m pixel, in a Zarr store of UTM zones read in place
//!   (see [`tessera`]).
//!
//! Nothing here waits on the network: searches and reads take a callback (the app's, off the
//! frame), with blocking forms for the command line.

use rustgis_data::source::{Batch, Request, parquet::Vectors};
use rustgis_data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
use rustgis_geom::{Coord, Extent};
use serde_json::{Value as Json, json};

use crate::cog::Look;

pub mod tessera;

/// What a dataset in the catalog is and what RustGIS does with it.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddingDataset {
    pub id: &'static str,
    /// Its name (a proper noun, not translated).
    pub name: &'static str,
    pub provider: &'static str,
    pub model: &'static str,
    /// `pixel` (a band a dimension) or `patch` (a vector an image chip).
    pub kind: &'static str,
    /// The ground spacing of one embedding.
    pub resolution: &'static str,
    pub dimensions: u32,
    /// The years with data, first and last (none: no yearly series).
    pub years: Option<(i32, i32)>,
    pub coverage: &'static str,
    pub license: &'static str,
    /// Where the data is described.
    pub data_url: &'static str,
    /// The paper or model documentation.
    pub paper_url: &'static str,
    /// Whether RustGIS searches it and shows it on the map (otherwise it is listed with links).
    pub search: bool,
}

/// The datasets, in the order they are listed.
pub const DATASETS: [EmbeddingDataset; 6] = [
    EmbeddingDataset {
        id: "alphaearth",
        name: "AlphaEarth Foundations (Google Satellite Embedding)",
        provider: "Google / Google DeepMind",
        model: "AlphaEarth Foundations",
        kind: "pixel",
        resolution: "10 m",
        dimensions: 64,
        years: Some((2017, 2025)),
        coverage: "Global land and coastal waters",
        license: "CC-BY-4.0",
        data_url: "https://source.coop/tge-labs/aef",
        paper_url: "https://arxiv.org/abs/2507.22291",
        search: true,
    },
    EmbeddingDataset {
        id: "tessera",
        name: "Tessera",
        provider: "University of Cambridge",
        model: "Tessera",
        kind: "pixel",
        resolution: "10 m",
        dimensions: 128,
        years: Some((2017, 2025)),
        coverage: "Global land (UTM zones)",
        license: "",
        data_url: "https://registry.opendata.aws/tessera/",
        paper_url: "https://arxiv.org/abs/2506.20380",
        search: true,
    },
    EmbeddingDataset {
        id: "earth-index",
        name: "Earth Index",
        provider: "Earth Genome",
        model: "SoftCon (DINOv2 ViT-S/14)",
        kind: "patch",
        resolution: "~320 m",
        dimensions: 384,
        years: Some((2024, 2024)),
        coverage: "Global land (Sentinel-2 MGRS tiles)",
        license: "CC-BY-4.0",
        data_url: "https://source.coop/earthgenome/earthindexembeddings",
        paper_url: "https://github.com/zhu-xlab/softcon",
        search: true,
    },
    EmbeddingDataset {
        id: "clay",
        name: "Clay",
        provider: "Clay Foundation",
        model: "Clay v0 / v1.5",
        kind: "patch",
        resolution: "5.12 km",
        dimensions: 768,
        years: None,
        coverage: "Global (partial)",
        license: "ODC-By-1.0",
        data_url: "https://source.coop/clay/clay-model-v0-embeddings",
        paper_url: "https://clay-foundation.github.io/model/",
        search: false,
    },
    EmbeddingDataset {
        id: "major-tom",
        name: "Major TOM",
        provider: "ESA Φ-lab",
        model: "SigLIP, DINOv2, SSL4EO and others",
        kind: "patch",
        resolution: "2.1–3.6 km",
        dimensions: 2048,
        years: None,
        coverage: "Global",
        license: "CC-BY-SA-4.0",
        data_url: "https://huggingface.co/Major-TOM",
        paper_url: "https://arxiv.org/abs/2412.05600",
        search: false,
    },
    EmbeddingDataset {
        id: "copernicus-embed",
        name: "Copernicus-Embed",
        provider: "TUM / Zhu Lab",
        model: "Copernicus-FM",
        kind: "pixel",
        resolution: "0.25°",
        dimensions: 768,
        years: Some((2021, 2021)),
        coverage: "Global",
        license: "CC-BY-4.0",
        data_url: "https://github.com/zhu-xlab/Copernicus-FM",
        paper_url: "https://arxiv.org/abs/2503.11849",
        search: false,
    },
];

/// A dataset by its id.
pub fn dataset(id: &str) -> Option<&'static EmbeddingDataset> {
    DATASETS.iter().find(|d| d.id == id)
}

impl EmbeddingDataset {
    /// The catalog entry as JSON (`embeddings.datasets`).
    pub fn to_json(&self) -> Json {
        json!({
            "id": self.id, "name": self.name, "provider": self.provider, "model": self.model, "kind": self.kind,
            "resolution": self.resolution, "dimensions": self.dimensions,
            "years": self.years.map(|(a, b)| (a..=b).collect::<Vec<_>>()).unwrap_or_default(),
            "coverage": self.coverage, "license": self.license, "dataUrl": self.data_url, "paperUrl": self.paper_url,
            "search": self.search,
        })
    }
}

/// A file (or tile of a store) found: what it is, where, and its footprint.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub dataset: &'static str,
    pub id: String,
    pub title: String,
    /// What it is, as a card's dim line shows it (its coordinate system, size, year).
    pub detail: String,
    /// The file's web address.
    pub url: String,
    pub year: Option<i32>,
    /// West, south, east, north.
    pub bbox: [f64; 4],
    /// Its outline in longitude/latitude (closed), when known.
    pub footprint: Vec<(f64, f64)>,
}

impl Tile {
    pub fn to_json(&self) -> Json {
        json!({"dataset": self.dataset, "id": self.id, "title": self.title, "detail": self.detail, "url": self.url,
            "year": self.year, "bbox": self.bbox})
    }
}

/// The tiles' footprints as a GeoJSON FeatureCollection (their outlines, else their boxes).
pub fn footprints_geojson(tiles: &[Tile]) -> Json {
    let features: Vec<Json> = tiles
        .iter()
        .map(|t| {
            let ring: Vec<[f64; 2]> = if t.footprint.len() >= 4 {
                t.footprint.iter().map(|(x, y)| [*x, *y]).collect()
            } else {
                let [w, s, e, n] = t.bbox;
                vec![[w, s], [e, s], [e, n], [w, n], [w, s]]
            };
            json!({"type": "Feature", "properties": {"title": t.title, "year": t.year, "url": t.url},
                "geometry": {"type": "Polygon", "coordinates": [ring]}})
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// A box (west, south, east, north) checked and kept to the globe.
pub fn check_bbox(b: [f64; 4]) -> Result<[f64; 4], String> {
    let [w, s, e, n] = b;
    if !(w.is_finite() && s.is_finite() && e.is_finite() && n.is_finite()) || w >= e || s >= n {
        return Err("bbox is [west, south, east, north], west of east and south of north".into());
    }
    let c = [w.max(-180.0), s.max(-90.0), e.min(180.0), n.min(90.0)];
    // A box wholly past the globe's edges is nowhere.
    if c[0] >= c[2] || c[1] >= c[3] {
        return Err("bbox is outside the globe".into());
    }
    Ok(c)
}

// AlphaEarth Foundations.

/// The index of AlphaEarth's tiles: a GeoParquet row a tile and year.
pub const AEF_INDEX: &str = "https://data.source.coop/tge-labs/aef/v1/annual/aef_index_stac_geoparquet.parquet";
/// The index's nested columns read beside its own: the tile's address.
const AEF_HREF: &str = "assets.data.href";
/// Earth Engine's example bands (0-based): A01, A16, A09.
pub const AEF_DEFAULT_BANDS: [usize; 3] = [1, 16, 9];
/// Earth Engine's example stretch: dequantised values in [−0.3, 0.3].
pub const AEF_DEFAULT_STRETCH: f32 = 0.3;
/// The most index rows one search reads (a few hundred tiles a year).
const MAX_INDEX_ROWS: usize = 4000;
/// The most tiles a search lists.
pub const MAX_TILES: usize = 60;

/// How AlphaEarth's bands (0-based, three) are shown: as red, green and blue, dequantised and
/// stretched over [−stretch, stretch].
pub fn aef_look(bands: [usize; 3], stretch: f32) -> Look {
    let s = if stretch.is_finite() && stretch > 0.0 { stretch.min(1.0) } else { AEF_DEFAULT_STRETCH };
    Look { bands: bands.iter().map(|b| (*b).min(63)).collect(), range: Some((-s, s)), aef: true, cube: false }
}

/// A file's web address from the index's `s3://` one.
fn https(href: &str) -> String {
    match href.strip_prefix("s3://us-west-2.opendata.source.coop/") {
        Some(key) => format!("https://data.source.coop/{key}"),
        None => href.to_string(),
    }
}

/// The year of a timestamp since 1970: milliseconds, or microseconds or nanoseconds when that
/// large (none of them is a year past 5000 in a smaller unit).
fn year_of(t: i64) -> i32 {
    let ms = match t.unsigned_abs() {
        a if a > 100_000_000_000_000_000 => t / 1_000_000,
        a if a > 100_000_000_000_000 => t / 1000,
        _ => t,
    };
    let days = ms.div_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    i32::try_from(y).unwrap_or(0)
}

/// The tiles of a year in what the index read, nearest the box's centre first (at most
/// [`MAX_TILES`]).
pub fn aef_tiles(fields: &[Field], batch: &Batch, year: i32, bbox: [f64; 4]) -> Vec<Tile> {
    let col = |n: &str| fields.iter().position(|f| f.name == n);
    let (href, epsg, date, id) = (col(AEF_HREF), col("proj:epsg"), col("datetime"), col("id"));
    let text = |f: &Feature, c: Option<usize>| c.and_then(|c| f.attrs.get(c)).map(|v| v.to_string()).unwrap_or_default();
    let (cx, cy) = ((bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0);
    let mut out: Vec<(f64, Tile)> = batch
        .features
        .iter()
        .filter_map(|f| {
            let y = date.and_then(|c| f.attrs.get(c)).and_then(|v| match v {
                Value::Int(t) => Some(year_of(*t)),
                Value::Text(t) => t.get(..4).and_then(|y| y.parse().ok()),
                _ => None,
            })?;
            if y != year {
                return None;
            }
            let url = https(&text(f, href));
            if !url.starts_with("https://") {
                return None;
            }
            let g = f.geometry.as_ref()?;
            let e = g.extent();
            let footprint = match g {
                Geometry::Polygon(p) => {
                    p.first().and_then(|poly| poly.first()).map(|r| r.iter().map(|c| (c.x, c.y)).collect()).unwrap_or_default()
                }
                _ => Vec::new(),
            };
            // `EPSG:32610`, or the bare number.
            let code = text(f, epsg).trim_start_matches("EPSG:").to_string();
            let crs = format!("EPSG:{code}");
            let zone = code.strip_prefix("327").map(|z| format!("{z}S")).or_else(|| code.strip_prefix("326").map(|z| format!("{z}N")));
            let stem = url.rsplit('/').next().unwrap_or("").trim_end_matches(".tiff").trim_end_matches(".tif").to_string();
            let title = match &zone {
                Some(z) => format!("AlphaEarth {year} · UTM {}", z.trim_start_matches('0')),
                None => format!("AlphaEarth {year}"),
            };
            let d = (e.center().x - cx).powi(2) + (e.center().y - cy).powi(2);
            Some((
                d,
                Tile {
                    dataset: "alphaearth",
                    id: if stem.is_empty() { text(f, id) } else { stem },
                    title,
                    detail: format!("{crs} · 64 bands · 10 m · {year}"),
                    url,
                    year: Some(year),
                    bbox: [e.min_x, e.min_y, e.max_x, e.max_y],
                    footprint,
                },
            ))
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.dedup_by(|a, b| a.1.url == b.1.url);
    out.into_iter().take(MAX_TILES).map(|(_, t)| t).collect()
}

/// What a search found: the tiles, and whether the index was read whole for the box (a box
/// holding more rows than one search reads is read in part, and the map should be zoomed in).
pub type Found = (Vec<Tile>, bool);

fn aef_request(bbox: [f64; 4]) -> Request {
    Request { extent: Some(Extent::new(bbox[0], bbox[1], bbox[2], bbox[3])), limit: MAX_INDEX_ROWS }
}

/// Finds AlphaEarth's tiles of a year in a box (longitude/latitude), reading the index in place.
pub fn search_aef(bbox: [f64; 4], year: i32, done: Box<dyn FnOnce(Result<Found, String>) + Send>) {
    let bbox = match check_bbox(bbox) {
        Ok(b) => b,
        Err(e) => return done(Err(e)),
    };
    rustgis_data::source::query_url(
        AEF_INDEX,
        vec![AEF_HREF.into()],
        aef_request(bbox),
        Box::new(move |r| {
            done(r.map(|(fields, batch)| (aef_tiles(&fields, &batch, year, bbox), batch.complete)).map_err(|e| e.to_string()))
        }),
    );
}

/// [`search_aef`], waiting for it (the command line).
#[cfg(not(target_arch = "wasm32"))]
pub fn search_aef_blocking(bbox: [f64; 4], year: i32) -> Result<Found, String> {
    let bbox = check_bbox(bbox)?;
    let (fields, batch) =
        rustgis_data::source::query_url_blocking(AEF_INDEX, vec![AEF_HREF.into()], aef_request(bbox)).map_err(|e| e.to_string())?;
    Ok((aef_tiles(&fields, &batch, year, bbox), batch.complete))
}

// Earth Index.

/// Where Earth Index's files are.
pub const EARTH_INDEX_BASE: &str = "https://data.source.coop/earthgenome/earthindexembeddings";
/// The one year Earth Index has.
pub const EARTH_INDEX_YEAR: i32 = 2024;
/// The most points one read takes, and the most row groups (about 2 MB of vectors each).
pub const EARTH_INDEX_MAX_POINTS: usize = 30_000;
pub const EARTH_INDEX_MAX_GROUPS: usize = if cfg!(target_arch = "wasm32") { 32 } else { 64 };
/// The most MGRS tiles a search names.
const MAX_MGRS: usize = 24;

/// The file of an MGRS tile (`17SKV`).
pub fn earth_index_url(tile: &str) -> String {
    format!("{EARTH_INDEX_BASE}/{EARTH_INDEX_YEAR}/{tile}_{EARTH_INDEX_YEAR}-01-01_{}-01-01.parquet", EARTH_INDEX_YEAR + 1)
}

/// The UTM zone of a place, with Norway's and Svalbard's exceptions.
fn utm_zone(lon: f64, lat: f64) -> u32 {
    let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    let mut zone = (((lon + 180.0) / 6.0).floor() as i64 + 1).clamp(1, 60) as u32;
    if (56.0..64.0).contains(&lat) && (3.0..12.0).contains(&lon) {
        zone = 32;
    }
    if (72.0..84.0).contains(&lat) {
        zone = match lon {
            l if (0.0..9.0).contains(&l) => 31,
            l if (9.0..21.0).contains(&l) => 33,
            l if (21.0..33.0).contains(&l) => 35,
            l if (33.0..42.0).contains(&l) => 37,
            _ => zone,
        };
    }
    zone
}

/// The Sentinel-2 (MGRS 100 km) tile holding a place, such as `17SKV`; `None` outside MGRS's
/// UTM latitudes (80° S to 84° N).
pub fn mgrs_tile(lon: f64, lat: f64) -> Option<String> {
    if !(lon.is_finite() && lat.is_finite() && (-80.0..84.0).contains(&lat)) {
        return None;
    }
    let zone = utm_zone(lon, lat);
    // Band X runs from 72° N to 84° N (12°, not 8°).
    let band = *b"CDEFGHJKLMNPQRSTUVWX".get((((lat + 80.0) / 8.0).floor() as usize).min(19))?;
    let code = if lat >= 0.0 { 32600 + zone } else { 32700 + zone };
    let t = rustgis_data::raster::Crs::from_epsg(code)?.transformer().ok()?;
    let c = t.from_lonlat(Coord::new(lon, lat))?;
    let cols: &[u8] = match zone % 3 {
        1 => b"ABCDEFGH",
        2 => b"JKLMNPQR",
        _ => b"STUVWXYZ",
    };
    let col = *cols.get(((c.x / 100_000.0).floor() as i64 - 1).clamp(0, 7) as usize)?;
    let rows = b"ABCDEFGHJKLMNPQRSTUV";
    let offset = if zone.is_multiple_of(2) { 5 } else { 0 };
    let row = *rows.get((((c.y / 100_000.0).floor() as i64 + offset).rem_euclid(20)) as usize)?;
    Some(format!("{zone:02}{}{}{}", band as char, col as char, row as char))
}

/// The MGRS tiles a box meets (sampled on a grid finer than a tile), each with the part of the box
/// it holds, nearest the box's centre first: at most a few dozen, and whether that is all of them.
pub fn mgrs_tiles(bbox: [f64; 4]) -> (Vec<(String, [f64; 4])>, bool) {
    let [w, s, e, n] = bbox;
    // About every 20 km, at most 48 a side.
    let step = |a: f64, b: f64| ((b - a) / 0.2).ceil().clamp(1.0, 48.0) as usize;
    let (nx, ny) = (step(w, e), step(s, n));
    let mut out: Vec<(String, [f64; 4])> = Vec::new();
    for j in 0..=ny {
        for i in 0..=nx {
            let (x, y) = (w + (e - w) * i as f64 / nx as f64, s + (n - s) * j as f64 / ny as f64);
            let Some(t) = mgrs_tile(x, y) else { continue };
            // At most 49² samples: every tile they name is kept, then the nearest.
            match out.iter_mut().find(|(k, _)| *k == t) {
                Some((_, b)) => *b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)],
                None => out.push((t, [x, y, x, y])),
            }
        }
    }
    let (cx, cy) = ((w + e) / 2.0, (s + n) / 2.0);
    let d = |b: &[f64; 4]| ((b[0] + b[2]) / 2.0 - cx).powi(2) + ((b[1] + b[3]) / 2.0 - cy).powi(2);
    out.sort_by(|a, b| d(&a.1).total_cmp(&d(&b.1)));
    let all = out.len() <= MAX_MGRS;
    out.truncate(MAX_MGRS);
    (out, all)
}

/// The Earth Index files a box may have (named from its MGRS tiles, not checked), and whether
/// they are all of them.
pub fn earth_index_candidates(bbox: [f64; 4]) -> (Vec<Tile>, bool) {
    let (tiles, all) = mgrs_tiles(bbox);
    let tiles = tiles
        .into_iter()
        .map(|(t, b)| Tile {
            dataset: "earth-index",
            id: t.clone(),
            title: format!("Earth Index · MGRS {t}"),
            detail: format!("384 values · ~320 m · {EARTH_INDEX_YEAR}"),
            url: earth_index_url(&t),
            year: Some(EARTH_INDEX_YEAR),
            bbox: b,
            footprint: Vec::new(),
        })
        .collect();
    (tiles, all)
}

/// Finds Earth Index's files in a box: its MGRS tiles whose file exists (each asked for its
/// first bytes).
pub fn search_earth_index(bbox: [f64; 4], done: Box<dyn FnOnce(Result<Found, String>) + Send>) {
    let bbox = match check_bbox(bbox) {
        Ok(b) => b,
        Err(e) => return done(Err(e)),
    };
    let (tiles, all) = earth_index_candidates(bbox);
    if tiles.is_empty() {
        return done(Ok((Vec::new(), all)));
    }
    struct Probe {
        left: usize,
        found: Vec<Option<Tile>>,
        failed: Option<String>,
        done: Option<Box<dyn FnOnce(Result<Found, String>) + Send>>,
    }
    let n = tiles.len();
    let probe = std::sync::Arc::new(std::sync::Mutex::new(Probe { left: n, found: vec![None; n], failed: None, done: Some(done) }));
    for (k, t) in tiles.into_iter().enumerate() {
        let p = probe.clone();
        let url = format!("{}{}0-3", t.url, crate::cache::RANGE);
        crate::cache::fetch_counted(
            url,
            Box::new(move |r| {
                let mut s = p.lock().unwrap_or_else(|e| e.into_inner());
                match r {
                    Ok(b) if b.starts_with(b"PAR1") => {
                        if let Some(slot) = s.found.get_mut(k) {
                            *slot = Some(t);
                        }
                    }
                    Ok(_) => {}
                    Err(e) if matches!(crate::cache::http_status(&e), Some(403 | 404)) => {}
                    Err(e) => {
                        s.failed.get_or_insert(e);
                    }
                }
                s.left = s.left.saturating_sub(1);
                if s.left == 0
                    && let Some(done) = s.done.take()
                {
                    let found: Vec<Tile> = s.found.iter_mut().filter_map(Option::take).collect();
                    let failed = s.failed.take();
                    drop(s);
                    done(match failed {
                        Some(e) if found.is_empty() => Err(e),
                        _ => Ok((found, all)),
                    });
                }
            }),
        );
    }
}

/// [`search_earth_index`], waiting for it (the command line).
#[cfg(not(target_arch = "wasm32"))]
pub fn search_earth_index_blocking(bbox: [f64; 4]) -> Result<Found, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    search_earth_index(bbox, Box::new(move |r| tx.send(r).unwrap_or_default()));
    rx.recv_timeout(std::time::Duration::from_secs(120)).unwrap_or_else(|_| Err("Earth Index: no answer in two minutes".into()))
}

fn ei_extent(bbox: [f64; 4]) -> Extent {
    Extent::new(bbox[0], bbox[1], bbox[2], bbox[3])
}

/// Reads an Earth Index file's points in a box, with their vectors (off the caller's thread).
pub fn load_points(url: &str, bbox: [f64; 4], done: Box<dyn FnOnce(Result<Vectors, String>) + Send>) {
    let bbox = match check_bbox(bbox) {
        Ok(b) => b,
        Err(e) => return done(Err(e)),
    };
    rustgis_data::source::vectors_url(
        url,
        ei_extent(bbox),
        "embedding".into(),
        (EARTH_INDEX_MAX_GROUPS, EARTH_INDEX_MAX_POINTS),
        Box::new(move |r| done(r.map_err(|e| e.to_string()))),
    );
}

/// [`load_points`], waiting for it (the command line).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_points_blocking(url: &str, bbox: [f64; 4]) -> Result<Vectors, String> {
    let bbox = check_bbox(bbox)?;
    rustgis_data::source::vectors_url_blocking(url, ei_extent(bbox), "embedding".into(), (EARTH_INDEX_MAX_GROUPS, EARTH_INDEX_MAX_POINTS))
        .map_err(|e| e.to_string())
}

/// The vectors' first three principal components, each stretched (2nd to 98th percentile) to
/// 0–255, so similar vectors get similar colours. The covariance is estimated from at most 2 000
/// vectors and diagonalised by power iteration with deflation (deterministic, so colours don't
/// change between reads).
pub fn pca_colors(values: &[f32], dims: usize) -> Vec<[u8; 3]> {
    if dims == 0 || values.len() < dims {
        return Vec::new();
    }
    let n = values.len() / dims;
    let row = |i: usize| values.get(i * dims..(i + 1) * dims).unwrap_or_default();
    let mut mean = vec![0f64; dims];
    for i in 0..n {
        for (m, v) in mean.iter_mut().zip(row(i)) {
            *m += f64::from(*v);
        }
    }
    for m in &mut mean {
        *m /= n as f64;
    }
    let stride = (n / 2000).max(1);
    let mut cov = vec![0f64; dims * dims];
    let mut centred = vec![0f64; dims];
    let mut samples = 0usize;
    for i in (0..n).step_by(stride) {
        for ((c, v), m) in centred.iter_mut().zip(row(i)).zip(&mean) {
            *c = f64::from(*v) - m;
        }
        for a in 0..dims {
            let ca = centred.get(a).copied().unwrap_or(0.0);
            if ca == 0.0 {
                continue;
            }
            for b in a..dims {
                if let Some(slot) = cov.get_mut(a * dims + b) {
                    *slot += ca * centred.get(b).copied().unwrap_or(0.0);
                }
            }
        }
        samples += 1;
    }
    let div = (samples.max(2) - 1) as f64;
    for a in 0..dims {
        for b in a..dims {
            let v = cov.get(a * dims + b).copied().unwrap_or(0.0) / div;
            if let Some(s) = cov.get_mut(a * dims + b) {
                *s = v;
            }
            if let Some(s) = cov.get_mut(b * dims + a) {
                *s = v;
            }
        }
    }
    let mut components: Vec<Vec<f64>> = Vec::new();
    for k in 0..dims.min(3) {
        let mut v: Vec<f64> = (0..dims).map(|d| 1.0 + ((d * 7 + k * 13) % 5) as f64).collect();
        let mut eigen = 0.0;
        for _ in 0..100 {
            let next: Vec<f64> = (0..dims)
                .map(|a| (0..dims).map(|b| cov.get(a * dims + b).copied().unwrap_or(0.0) * v.get(b).copied().unwrap_or(0.0)).sum())
                .collect();
            let norm = next.iter().map(|x| x * x).sum::<f64>().sqrt();
            if norm == 0.0 || !norm.is_finite() {
                break;
            }
            let next: Vec<f64> = next.into_iter().map(|x| x / norm).collect();
            let delta: f64 = next.iter().zip(&v).map(|(a, b)| (a - b).abs()).sum();
            v = next;
            eigen = norm;
            if delta < 1e-9 {
                break;
            }
        }
        for a in 0..dims {
            for b in 0..dims {
                if let Some(s) = cov.get_mut(a * dims + b) {
                    *s -= eigen * v.get(a).copied().unwrap_or(0.0) * v.get(b).copied().unwrap_or(0.0);
                }
            }
        }
        components.push(v);
    }
    let scores: Vec<Vec<f64>> = components
        .iter()
        .map(|c| (0..n).map(|i| row(i).iter().zip(&mean).zip(c).map(|((x, m), w)| (f64::from(*x) - m) * w).sum()).collect())
        .collect();
    let stretched: Vec<Vec<u8>> = scores
        .iter()
        .map(|s| {
            let mut sorted = s.clone();
            sorted.sort_by(f64::total_cmp);
            let at = |q: f64| sorted.get(((sorted.len().saturating_sub(1)) as f64 * q) as usize).copied().unwrap_or(0.0);
            let (lo, hi) = (at(0.02), at(0.98));
            let span = if hi > lo { hi - lo } else { 1.0 };
            s.iter().map(|v| (((v - lo) / span).clamp(0.0, 1.0) * 255.0).round() as u8).collect()
        })
        .collect();
    (0..n)
        .map(|i| {
            let c = |k: usize| stretched.get(k).and_then(|s| s.get(i)).copied().unwrap_or(128);
            [c(0), c(1), c(2)]
        })
        .collect()
}

/// Levels each colour channel is rounded to for the points' classes (4³ = 64 colours).
const LEVELS: u8 = 4;

/// A colour rounded to one of the 64 the points are drawn in, as `#rrggbb`.
pub fn class_color(c: [u8; 3]) -> String {
    let q = |v: u8| -> u8 {
        let step = 255 / (LEVELS - 1);
        ((u16::from(v) + u16::from(step) / 2) / u16::from(step)).min(u16::from(LEVELS - 1)) as u8 * step
    };
    format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]))
}

/// Earth Index's points as a layer's dataset: each point's principal components (`pc1`–`pc3`,
/// 0–255), the colour they make (`color`, rounded to 64), and, when asked for, one of its values
/// (`dim`, 0-based) as `value`.
pub fn points_dataset(name: &str, v: &Vectors, dim: Option<usize>) -> Dataset {
    let mut fields = vec![
        Field::new("pc1", FieldType::Integer),
        Field::new("pc2", FieldType::Integer),
        Field::new("pc3", FieldType::Integer),
        Field::new("color", FieldType::Text),
    ];
    let dim = dim.filter(|d| *d < v.dims);
    if dim.is_some() {
        fields.push(Field::new("value", FieldType::Double));
    }
    let mut ds = Dataset::new(name, GeometryType::Point, fields);
    let colors = pca_colors(&v.values, v.dims);
    for (i, ((x, y), c)) in v.points.iter().zip(&colors).enumerate() {
        let mut attrs =
            vec![Value::Int(i64::from(c[0])), Value::Int(i64::from(c[1])), Value::Int(i64::from(c[2])), Value::Text(class_color(*c))];
        if let Some(d) = dim {
            attrs.push(v.values.get(i * v.dims + d).map(|x| Value::Float(f64::from(*x))).unwrap_or_default());
        }
        ds.features.push(Feature { id: i as u64 + 1, geometry: Some(Geometry::Point(Coord::new(*x, *y))), attrs });
    }
    ds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog() {
        assert_eq!(DATASETS.len(), 6);
        assert_eq!(DATASETS.iter().filter(|d| d.search).count(), 3);
        assert_eq!(dataset("alphaearth").map(|d| d.dimensions), Some(64));
        assert!(dataset("nope").is_none());
        assert_eq!(dataset("earth-index").unwrap().to_json()["years"], json!([2024]));
    }

    #[test]
    fn mgrs_tiles_by_place() {
        // Knoxville, Tennessee; Cape Town; Bergen (Norway's exception); the poles are outside.
        assert_eq!(mgrs_tile(-83.92, 35.96).as_deref(), Some("17SKV"));
        assert_eq!(mgrs_tile(18.42, -33.92).as_deref(), Some("34HBH"));
        assert_eq!(mgrs_tile(5.32, 60.39).map(|t| t[..3].to_string()).as_deref(), Some("32V"));
        assert!(mgrs_tile(0.0, 85.0).is_none() && mgrs_tile(f64::NAN, 0.0).is_none());
        // Band X reaches 84° N (northern Greenland, Svalbard).
        assert_eq!(mgrs_tile(-40.0, 82.0).map(|t| t[2..3].to_string()).as_deref(), Some("X"));
        let (t, all) = mgrs_tiles([-84.0, 35.9, -83.8, 36.0]);
        assert_eq!(t.first().map(|(k, _)| k.as_str()), Some("17SKV"));
        assert!(all);
        // A huge box names a bounded number of tiles, the nearest its centre, and says so.
        let (t, all) = mgrs_tiles([-10.0, -10.0, 10.0, 10.0]);
        assert!(t.len() == MAX_MGRS && !all);
        assert!(t.first().is_some_and(|(k, _)| k.starts_with("31N") || k.starts_with("31M")), "{t:?}");
        assert_eq!(earth_index_url("17SKV"), format!("{EARTH_INDEX_BASE}/2024/17SKV_2024-01-01_2025-01-01.parquet"));
    }

    #[test]
    fn years_and_addresses() {
        assert_eq!(year_of(1_609_459_200_000), 2021);
        assert_eq!(year_of(1_609_459_200_000_000), 2021);
        assert_eq!(year_of(1_609_459_200_000_000_000), 2021);
        assert_eq!(year_of(-1), 1969);
        assert_eq!(https("s3://us-west-2.opendata.source.coop/tge-labs/aef/x.tiff"), "https://data.source.coop/tge-labs/aef/x.tiff");
        let look = aef_look([1, 16, 99], f32::NAN);
        assert_eq!((look.bands, look.range, look.aef), (vec![1, 16, 63], Some((-0.3, 0.3)), true));
        assert!(check_bbox([1.0, 0.0, 0.0, 1.0]).is_err() && check_bbox([f64::NAN, 0.0, 1.0, 1.0]).is_err());
        assert_eq!(check_bbox([-200.0, -95.0, 200.0, 95.0]), Ok([-180.0, -90.0, 180.0, 90.0]));
        assert!(check_bbox([200.0, 0.0, 210.0, 1.0]).is_err() && check_bbox([0.0, 91.0, 1.0, 95.0]).is_err());
    }

    #[test]
    fn index_rows_to_tiles() {
        let fields = vec![
            Field::new("id", FieldType::Text),
            Field::new("datetime", FieldType::Integer),
            Field::new(AEF_HREF, FieldType::Text),
            Field::new("proj:epsg", FieldType::Text),
        ];
        let ring = |x: f64| {
            vec![vec![vec![
                Coord::new(x, 40.0),
                Coord::new(x + 1.0, 40.0),
                Coord::new(x + 1.0, 41.0),
                Coord::new(x, 41.0),
                Coord::new(x, 40.0),
            ]]]
        };
        let row = |x: f64, ms: i64, href: &str| Feature {
            id: 1,
            geometry: Some(Geometry::Polygon(ring(x))),
            attrs: vec![Value::Text("1".into()), Value::Int(ms), Value::Text(href.into()), Value::Text("EPSG:32610".into())],
        };
        let batch = Batch {
            features: vec![
                row(-124.0, 1_609_459_200_000, "s3://us-west-2.opendata.source.coop/a/2021/10N/far.tiff"),
                row(-123.2, 1_609_459_200_000, "s3://us-west-2.opendata.source.coop/a/2021/10N/near.tiff"),
                row(-123.2, 1_577_836_800_000, "s3://us-west-2.opendata.source.coop/a/2020/10N/other-year.tiff"),
                row(-123.0, 1_609_459_200_000, "ftp://nowhere"),
            ],
            matched: 4,
            complete: true,
            skipped: 0,
        };
        let t = aef_tiles(&fields, &batch, 2021, [-123.0, 40.0, -122.0, 41.0]);
        assert_eq!(t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["near", "far"]);
        assert_eq!(t[0].title, "AlphaEarth 2021 · UTM 10N");
        // A bare EPSG number reads the same.
        let mut bare = batch.clone();
        for f in bare.features.iter_mut() {
            f.attrs[3] = Value::Int(32610);
        }
        let b = aef_tiles(&fields, &bare, 2021, [-123.0, 40.0, -122.0, 41.0]);
        assert_eq!((b[0].title.as_str(), b[0].detail.starts_with("EPSG:32610 ·")), ("AlphaEarth 2021 · UTM 10N", true));
        assert_eq!(t[0].url, "https://data.source.coop/a/2021/10N/near.tiff");
        assert_eq!(t[0].footprint.len(), 5);
        let fc = footprints_geojson(&t);
        assert_eq!(fc["features"].as_array().map(Vec::len), Some(2));
        // Nothing usable: nothing found, never a panic.
        assert!(aef_tiles(&[], &batch, 2021, [0.0, 0.0, 1.0, 1.0]).is_empty());
    }

    #[test]
    fn principal_component_colours() {
        // Two clusters along one direction: their first component tells them apart.
        let mut values = Vec::new();
        for i in 0..200 {
            let a = if i % 2 == 0 { 1.0 } else { -1.0 };
            values.extend_from_slice(&[a, a * 0.5, (i % 7) as f32 * 0.01, 0.0]);
        }
        let c = pca_colors(&values, 4);
        assert_eq!(c.len(), 200);
        assert_ne!(c[0][0], c[1][0]);
        assert_eq!(c[0][0], c[2][0]);
        assert!(pca_colors(&[], 4).is_empty() && pca_colors(&[1.0], 0).is_empty());
        assert_eq!(class_color([0, 100, 255]), "#0055ff");
        let v = Vectors { points: vec![(1.0, 2.0), (3.0, 4.0)], dims: 2, values: vec![0.0, 1.0, 2.0, 3.0] };
        let ds = points_dataset("ei", &v, Some(1));
        assert_eq!(ds.features.len(), 2);
        assert_eq!(ds.features[1].attrs.get(4), Some(&Value::Float(3.0)));
        assert_eq!(points_dataset("ei", &v, Some(9)).fields.len(), 4);
    }
}
