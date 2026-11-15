//! Tessera's embeddings, read in place from its Zarr 3 store: a group a UTM zone (`utm17`), with
//! `embeddings` (signed bytes: year, 128 bands, y, x) and `scales` (32-bit floats: year, y, x),
//! the value being their product. Both are sharded: a shard file holds 4 096² pixels in inner
//! chunks of 32² (all 128 bands together), with an index of where each chunk is at its end.
//!
//! A view is read as a window of the zone's grid: the shard index's end of the file, then the
//! inner chunks the window needs (each about 115 kB), within one shard and at most
//! [`MAX_CHUNKS`] chunks; three bands of it are kept, as a 32-bit float raster in the zone's UTM
//! coordinates.
//!
//! The zones, the window and the shard's index are [`Store`]'s, shared with OlmoEarth's store
//! (see [`super::olmoearth`]), which is laid out the same way.

use std::sync::Arc;

use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, zarr};
use rustgis_data::source::remote::{self, Remote};
use rustgis_geom::Coord;
use serde_json::Value as Json;

use super::Tile;

/// The store.
pub const ROOT: &str = "https://data.source.coop/tessera/tessera/zarr/v1.1-dclimate";
/// The years it holds: its `time` axis, one step a year.
pub const FIRST_YEAR: i32 = 2017;
pub const LAST_YEAR: i32 = 2025;
/// Values a pixel.
pub const BANDS: usize = 128;
/// The bands shown first (0-based), Tessera's and OlmoEarth's.
pub const DEFAULT_BANDS: [usize; 3] = [0, 1, 2];
/// A shard's side and an inner chunk's, in pixels.
const SHARD: u64 = 4096;
const INNER: u64 = 32;
/// The most inner chunks one read takes (about 115 kB each).
pub const MAX_CHUNKS: usize = if cfg!(target_arch = "wasm32") { 256 } else { 400 };

/// A Zarr 3 store of embeddings in UTM zones, read a window at a time: a group a zone (`utm01`…
/// `utm60`, each in EPSG:326NN, its northings negative south of the equator) with a sharded
/// `embeddings` array (year, band, y, x), each shard's index at its end.
#[derive(Debug, PartialEq)]
pub struct Store {
    pub id: &'static str,
    pub name: &'static str,
    pub root: &'static str,
    pub first_year: i32,
    pub last_year: i32,
    /// Values a pixel.
    pub bands: usize,
    /// A shard's side and an inner chunk's, in pixels.
    pub shard: u64,
    pub inner: u64,
    /// The most inner chunks (of one band group) one read takes.
    pub max_chunks: usize,
}

/// Tessera's store.
pub const TESSERA: Store = Store {
    id: "tessera",
    name: "Tessera",
    root: ROOT,
    first_year: FIRST_YEAR,
    last_year: LAST_YEAR,
    bands: BANDS,
    shard: SHARD,
    inner: INNER,
    max_chunks: MAX_CHUNKS,
};

/// The store of a dataset (`tessera`, `olmoearth`), when it is a store of UTM zones.
pub fn store(id: &str) -> Option<&'static Store> {
    [&TESSERA, &super::olmoearth::OLMOEARTH].into_iter().find(|s| s.id == id)
}

/// The UTM zone of a longitude (the stores' groups follow the plain 6° zones).
pub fn zone_of(lon: f64) -> u32 {
    let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    (((lon + 180.0) / 6.0).floor() as i64 + 1).clamp(1, 60) as u32
}

/// A zone group's metadata.
pub fn group_url(store: &Store, zone: u32) -> String {
    format!("{}/utm{zone:02}/zarr.json", store.root)
}

/// A zone's grid, from its group's `spatial:` attributes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub zone: u32,
    pub x0: f64,
    pub y0: f64,
    pub dx: f64,
    pub dy: f64,
    pub width: u64,
    pub height: u64,
}

/// A zone's grid from its group's `zarr.json`.
pub fn parse_group(store: &Store, zone: u32, bytes: &[u8]) -> Result<Grid, String> {
    let name = store.name;
    let v: Json = serde_json::from_slice(bytes).map_err(|e| format!("{name}'s zone {zone}: {e}"))?;
    let a = &v["attributes"];
    let t: Vec<f64> = a["spatial:transform"].as_array().map(|t| t.iter().filter_map(Json::as_f64).collect()).unwrap_or_default();
    let shape: Vec<u64> = a["spatial:shape"].as_array().map(|s| s.iter().filter_map(Json::as_u64).collect()).unwrap_or_default();
    match (t.as_slice(), shape.as_slice()) {
        ([dx, 0.0, x0, 0.0, dy, y0], [h, w]) if *dx > 0.0 && *dy < 0.0 && *w > 0 && *h > 0 && x0.is_finite() && y0.is_finite() => {
            Ok(Grid { zone, x0: *x0, y0: *y0, dx: *dx, dy: *dy, width: *w, height: *h })
        }
        _ => Err(format!("{name}'s zone {zone}: its grid isn't one RustGIS reads")),
    }
}

/// What a read takes: a window of a zone's grid (pixels, `col0`, `row0`, `width` × `height`),
/// within one shard, and that shard's inner chunks it needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub store: &'static Store,
    pub grid: Grid,
    pub year: i32,
    pub shard: (u64, u64),
    pub col0: u64,
    pub row0: u64,
    pub width: u64,
    pub height: u64,
    /// Inner chunks (row, column) within the shard.
    pub chunks: Vec<(u64, u64)>,
    /// Whether the window is smaller than the box asked for (a part around its centre).
    pub cut: bool,
}

impl Plan {
    pub(super) fn shard_key(&self, array: &str) -> String {
        let t = (self.year - self.store.first_year).max(0);
        let (sy, sx) = self.shard;
        let (root, zone) = (self.store.root, self.grid.zone);
        match array {
            "embeddings" => format!("{root}/utm{zone:02}/embeddings/c/{t}/0/{sy}/{sx}"),
            _ => format!("{root}/utm{zone:02}/scales/c/{t}/{sy}/{sx}"),
        }
    }
}

/// The window of a box (longitude/latitude) for a year: the box's pixels in its zone, cut to the
/// shard at its centre and to the store's `max_chunks` chunks around it. Refuses an unplaceable
/// boundary sample rather than shrinking the requested area to only the surviving samples.
pub fn plan(store: &'static Store, grid: Grid, bbox: [f64; 4], year: i32) -> Result<Plan, String> {
    let (name, shard_side, inner) = (store.name, store.shard, store.inner);
    if !(store.first_year..=store.last_year).contains(&year) {
        return Err(format!("{name} has the years {}–{}", store.first_year, store.last_year));
    }
    let crs = Crs::from_epsg(32600 + grid.zone).ok_or("no such UTM zone")?;
    let t = crs.transformer()?;
    let [w, s, e, n] = super::check_bbox(bbox)?;
    let mut cols = (f64::MAX, f64::MIN);
    let mut rows = (f64::MAX, f64::MIN);
    for (x, y) in [(w, s), (w, n), (e, s), (e, n), ((w + e) / 2.0, s), ((w + e) / 2.0, n)] {
        let c = t
            .from_lonlat(Coord::new(x, y))
            .ok_or_else(|| format!("the area's boundary at ({x}, {y}) can't be placed in {name}'s zone; choose a smaller area"))?;
        let (cx, ry) = ((c.x - grid.x0) / grid.dx, (c.y - grid.y0) / grid.dy);
        cols = (cols.0.min(cx), cols.1.max(cx));
        rows = (rows.0.min(ry), rows.1.max(ry));
    }
    if cols.0 > cols.1 || rows.0 > rows.1 {
        return Err("the area isn't in the zone".into());
    }
    let (wf, hf) = (grid.width as f64, grid.height as f64);
    let (c0, c1) = (cols.0.clamp(0.0, wf), cols.1.clamp(0.0, wf));
    let (r0, r1) = (rows.0.clamp(0.0, hf), rows.1.clamp(0.0, hf));
    if c1 - c0 < 1.0 || r1 - r0 < 1.0 {
        return Err(format!("the area is outside {name}'s grid"));
    }
    // The shard at the centre, and the window within it.
    let (cc, rc) = (((c0 + c1) / 2.0) as u64, ((r0 + r1) / 2.0) as u64);
    let shard = (rc / shard_side, cc / shard_side);
    let (sx0, sy0) = (shard.1 * shard_side, shard.0 * shard_side);
    let clip = |a: f64, lo: u64| (a as u64).clamp(lo, lo + shard_side);
    let (mut col0, mut col1) = (clip(c0, sx0), clip(c1.ceil(), sx0));
    let (mut row0, mut row1) = (clip(r0, sy0), clip(r1.ceil(), sy0));
    let mut cut = col0 > c0 as u64 || row0 > r0 as u64 || col1 < c1 as u64 || row1 < r1 as u64;
    // At most max_chunks chunks: a square of them around the centre.
    // (One chunk less a side: an unaligned window meets one more.)
    let side = ((store.max_chunks as f64).sqrt().floor() as u64).saturating_sub(1).max(1) * inner;
    if col1 - col0 > side {
        let mid = cc.clamp(col0 + side / 2, col1 - side / 2);
        (col0, col1) = (mid - side / 2, mid + side / 2);
        cut = true;
    }
    if row1 - row0 > side {
        let mid = rc.clamp(row0 + side / 2, row1 - side / 2);
        (row0, row1) = (mid - side / 2, mid + side / 2);
        cut = true;
    }
    if col1 <= col0 || row1 <= row0 {
        return Err(format!("the area is outside {name}'s grid"));
    }
    let per_side = shard_side / inner;
    let (ic0, ic1) = ((col0 - sx0) / inner, (col1 - sx0).div_ceil(inner).min(per_side));
    let (ir0, ir1) = ((row0 - sy0) / inner, (row1 - sy0).div_ceil(inner).min(per_side));
    let chunks: Vec<(u64, u64)> = (ir0..ir1).flat_map(|r| (ic0..ic1).map(move |c| (r, c))).collect();
    Ok(Plan { store, grid, year, shard, col0, row0, width: col1 - col0, height: row1 - row0, chunks, cut })
}

/// The zones a box meets, as search results (a window of each read where the map is): the three
/// nearest the box's centre, and whether that is all of them.
pub fn tiles(store: &Store, bbox: [f64; 4], year: i32) -> (Vec<Tile>, bool) {
    let Ok([w, s, e, n]) = super::check_bbox(bbox) else { return (Vec::new(), true) };
    let (z0, z1) = (zone_of(w), zone_of(e.min(179.999)));
    let mut zones: Vec<u32> = (z0..=z1.max(z0)).collect();
    let centre = zone_of((w + e) / 2.0);
    zones.sort_by_key(|z| z.abs_diff(centre));
    let all = zones.len() <= 3;
    zones.truncate(3);
    zones.sort_unstable();
    let tiles = zones
        .into_iter()
        .map(|zone| {
            let zw = -180.0 + f64::from(zone - 1) * 6.0;
            let b = [w.max(zw), s, e.min(zw + 6.0), n];
            Tile {
                dataset: store.id,
                id: format!("utm{zone:02}"),
                title: format!("{} {year} · UTM {zone}", store.name),
                detail: format!("EPSG:{} · {} bands · 10 m · {year}", 32600 + zone, store.bands),
                url: format!("{}/utm{zone:02}", store.root),
                year: Some(year),
                bbox: b,
                footprint: Vec::new(),
            }
        })
        .collect();
    (tiles, all)
}

/// A chunk's decoder: its stored bytes and the bytes it holds decoded.
pub(super) type Decode = fn(&[u8], usize) -> rustgis_data::Result<Vec<u8>>;

/// Reads a plan's inner chunks of a shard whose index lists `groups` band groups, for each band
/// group in `of` (in that order, the plan's chunks each): `bytes` decoded bytes each, `None` for a
/// chunk the shard leaves out. Fails with [`remote::NEED`] until what it needs is fetched.
pub(super) fn read_chunks(
    file: &Remote,
    plan: &Plan,
    groups: u64,
    of: &[u64],
    bytes: usize,
    decode: Decode,
) -> rustgis_data::Result<Vec<Option<Vec<u8>>>> {
    let per_side = plan.store.shard / plan.store.inner;
    // A shard's index: an offset and a length a chunk, then its checksum.
    let index_len = groups * per_side * per_side * 16 + 4;
    let (tail, _) = file.tail(index_len).ok_or_else(remote::need)?;
    let index = tail.get(tail.len().saturating_sub(index_len as usize)..).unwrap_or_default();
    if (index.len() as u64) < index_len {
        return Err(rustgis_data::DataError::Format(format!("{}: a shard's index is cut", plan.store.name)));
    }
    // A stored chunk is at most its bytes and a little (zstd keeps what it can't shrink as is).
    let most = (bytes as u64).saturating_mul(2).max(4 << 20);
    let entry = |g: u64, r: u64, c: u64| index_entry(plan.store, index, ((g * per_side + r) * per_side + c) * 16, most);
    let wanted = || of.iter().flat_map(|&g| plan.chunks.iter().map(move |&(r, c)| (g, r, c)));
    let mut held = true;
    for (g, r, c) in wanted() {
        if let Some((off, len)) = entry(g, r, c)? {
            held &= file.want(off, len);
        }
    }
    if !held {
        return Err(remote::need());
    }
    wanted()
        .map(|(g, r, c)| match entry(g, r, c)? {
            Some((off, len)) => {
                let b = file.bytes(off, len).ok_or_else(remote::need)?;
                decode(&b, bytes).map(Some)
            }
            None => Ok(None),
        })
        .collect()
}

/// A shard index's entry at byte `at`: the chunk's offset and length, `None` for a chunk the
/// shard leaves out (both all ones), an error for a length of 0 or past `most`, or an all-ones
/// offset alone (a damaged store).
fn index_entry(store: &Store, index: &[u8], at: u64, most: u64) -> rustgis_data::Result<Option<(u64, u64)>> {
    let bad = || rustgis_data::DataError::Format(format!("{}: a shard's index is damaged", store.name));
    let b = usize::try_from(at).ok().and_then(|at| index.get(at..at.checked_add(16)?)).ok_or_else(bad)?;
    let word = |r: std::ops::Range<usize>| b.get(r).and_then(|w| w.try_into().ok()).map(u64::from_le_bytes).ok_or_else(bad);
    let (off, len) = (word(0..8)?, word(8..16)?);
    if off == u64::MAX && len == u64::MAX {
        return Ok(None);
    }
    if off == u64::MAX || len == 0 || len > most {
        return Err(bad());
    }
    Ok(Some((off, len)))
}

/// The window's three bands (each embedding × its pixel's scale; NaN where nothing was
/// embedded) as a raster in the zone's UTM coordinates.
pub fn assemble(plan: &Plan, bands: [usize; 3], emb: &[Option<Vec<u8>>], scales: &[Option<Vec<u8>>]) -> Result<Raster, String> {
    let (w, h) = (plan.width as usize, plan.height as usize);
    let n = w.checked_mul(h).filter(|n| *n <= 4 << 20).ok_or("the window is too large")?;
    let mut out = vec![vec![f32::NAN; n]; 3];
    let (sx0, sy0) = (plan.shard.1 * SHARD, plan.shard.0 * SHARD);
    let cell = (INNER * INNER) as usize;
    for (k, &(r, c)) in plan.chunks.iter().enumerate() {
        let (Some(Some(e)), Some(Some(s))) = (emb.get(k), scales.get(k)) else { continue };
        for y in 0..INNER {
            let row = sy0 + r * INNER + y;
            if row < plan.row0 || row >= plan.row0 + plan.height {
                continue;
            }
            for x in 0..INNER {
                let col = sx0 + c * INNER + x;
                if col < plan.col0 || col >= plan.col0 + plan.width {
                    continue;
                }
                let p = (y * INNER + x) as usize;
                let scale = s.get(p * 4..p * 4 + 4).and_then(|b| b.try_into().ok()).map(f32::from_le_bytes).unwrap_or(f32::NAN);
                if !scale.is_finite() {
                    continue;
                }
                let at = (row - plan.row0) as usize * w + (col - plan.col0) as usize;
                for (band, slot) in bands.iter().zip(out.iter_mut()) {
                    let v = e.get((*band).min(BANDS - 1) * cell + p).map(|b| f32::from(*b as i8));
                    if let (Some(v), Some(o)) = (v, slot.get_mut(at)) {
                        *o = v * scale;
                    }
                }
            }
        }
    }
    raster(plan, bands, out)
}

/// A window's three bands as a 32-bit float raster in the zone's UTM coordinates.
pub(super) fn raster(plan: &Plan, bands: [usize; 3], out: Vec<Vec<f32>>) -> Result<Raster, String> {
    let g = plan.grid;
    let transform = GeoTransform {
        origin_x: g.x0 + plan.col0 as f64 * g.dx,
        origin_y: g.y0 + plan.row0 as f64 * g.dy,
        pixel_width: g.dx,
        pixel_height: g.dy,
    };
    let crs = Crs::from_epsg(32600 + g.zone).ok_or("no such UTM zone")?;
    let label = bands.iter().map(|b| (b + 1).to_string()).collect::<Vec<_>>().join(", ");
    let mut r = Raster::new(
        format!("{} {} ({label})", plan.store.name, plan.year),
        plan.width as u32,
        plan.height as u32,
        out,
        transform,
        crs,
        None,
        ColorHint::Rgb { alpha: false },
        "Float32",
    )
    .map_err(|e| e.to_string())?;
    r.source = Some(format!("{}/utm{:02}", plan.store.root, g.zone));
    Ok(r)
}

type Chunks = Vec<Option<Vec<u8>>>;

/// Reads a plan's window (off the caller's thread), from its store: Tessera's, the embeddings'
/// chunks, then the scales'.
pub fn read(plan: Plan, bands: [usize; 3], done: Box<dyn FnOnce(Result<Raster, String>) + Send>) {
    if plan.store.id != TESSERA.id {
        return super::olmoearth::read(plan, bands, done);
    }
    let plan = Arc::new(plan);
    let emb_file = remote::file(&plan.shard_key("embeddings"));
    let (f, p) = (emb_file.clone(), plan.clone());
    remote::drive(
        emb_file,
        Arc::new(move || read_chunks(&f, &p, 1, &[0], (INNER * INNER) as usize * BANDS, zarr::blosc_decode)),
        Box::new(move |emb: rustgis_data::Result<Chunks>| {
            let emb = match emb {
                Ok(e) => e,
                Err(e) => return done(Err(e.to_string())),
            };
            let scale_file = remote::file(&plan.shard_key("scales"));
            let (f, p) = (scale_file.clone(), plan.clone());
            remote::drive(
                scale_file,
                Arc::new(move || read_chunks(&f, &p, 1, &[0], (INNER * INNER) as usize * 4, zarr::blosc_decode)),
                Box::new(move |scales: rustgis_data::Result<Chunks>| {
                    done(scales.map_err(|e| e.to_string()).and_then(|s| assemble(&plan, bands, &emb, &s)))
                }),
            );
        }),
    );
}

/// Reads three bands of a box for a year, waiting for it (the command line): the zone's grid,
/// then the window.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_blocking(store: &'static Store, bbox: [f64; 4], year: i32, bands: [usize; 3]) -> Result<(Raster, bool), String> {
    let bbox = super::check_bbox(bbox)?;
    let zone = zone_of((bbox[0] + bbox[2]) / 2.0);
    let grid = parse_group(store, zone, &crate::cache::download_blocking(&group_url(store, zone))?)?;
    let plan = plan(store, grid, bbox, year)?;
    let cut = plan.cut;
    let (tx, rx) = std::sync::mpsc::channel();
    read(plan, bands, Box::new(move |r| tx.send(r).unwrap_or_default()));
    let r = rx
        .recv_timeout(std::time::Duration::from_secs(300))
        .unwrap_or_else(|_| Err(format!("{}: no answer in five minutes", store.name)))?;
    Ok((r, cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        parse_group(
            &TESSERA,
            17,
            br#"{"attributes": {"spatial:transform": [10, 0, 163840.0, 0, -10, 9338880.0], "spatial:shape": [1826816, 69632]}}"#,
        )
        .unwrap()
    }

    #[test]
    fn grids_and_windows() {
        assert_eq!(zone_of(-83.92), 17);
        assert_eq!(zone_of(179.99), 60);
        assert!(parse_group(&TESSERA, 1, b"{}").is_err() && parse_group(&TESSERA, 1, b"junk").is_err());
        assert_eq!(store("tessera"), Some(&TESSERA));
        assert!(store("alphaearth").is_none());
        let g = grid();
        // Knoxville, a little over a kilometre: one shard, a few chunks.
        let p = plan(&TESSERA, g, [-83.93, 35.96, -83.92, 35.97], 2024).unwrap();
        assert_eq!(p.shard, (130, 1));
        assert!(!p.cut && !p.chunks.is_empty() && p.chunks.len() < 30);
        assert!(p.width > 80 && p.width < 120 && p.height > 100 && p.height < 130);
        assert_eq!(p.shard_key("embeddings"), format!("{ROOT}/utm17/embeddings/c/7/0/130/1"));
        assert_eq!(p.shard_key("scales"), format!("{ROOT}/utm17/scales/c/7/130/1"));
        // A large view: a window of at most MAX_CHUNKS chunks, said to be cut.
        let p = plan(&TESSERA, g, [-84.5, 35.5, -83.5, 36.5], 2024).unwrap();
        assert!(p.cut && p.chunks.len() <= MAX_CHUNKS);
        assert!(plan(&TESSERA, g, [-83.93, 35.96, -83.92, 35.97], 2016).is_err());
        assert!(plan(&TESSERA, g, [100.0, 35.0, 101.0, 36.0], 2024).is_err());
        // South of the equator: the zone's group spans both hemispheres in EPSG:326xx, its
        // northings negative there (spatial:bbox from −8 929 280 m). Cape Town, zone 34.
        let south = Grid { zone: 34, ..g };
        let p = plan(&TESSERA, south, [18.42, -33.93, 18.43, -33.92], 2024).unwrap();
        let row = 9_338_880.0 - (-3_755_000.0);
        assert!(((p.row0 as f64) * 10.0 - row).abs() < 5_000.0, "{}", p.row0);
        assert!(!p.cut && !p.chunks.is_empty());
        let (t, all) = tiles(&TESSERA, [-84.0, 35.0, -77.0, 36.0], 2024);
        assert_eq!(t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["utm17", "utm18"]);
        assert!(all);
        // Six zones: the three around the centre, and not all of them.
        let (t, all) = tiles(&TESSERA, [-90.0, 35.0, -54.0, 36.0], 2024);
        assert_eq!(t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["utm18", "utm19", "utm20"]);
        assert!(!all);
    }

    /// A request with refused corners must not become a partial window from the surviving ones.
    #[test]
    fn a_refused_view_is_not_a_partial_window() {
        let error = plan(&TESSERA, grid(), [-170.0, 0.0, -80.0, 40.0], 2024).unwrap_err();
        assert!(error.contains("can't be placed"), "{error}");
    }

    #[test]
    fn windows_assembled() {
        let g = grid();
        let p = plan(&TESSERA, g, [-83.93, 35.96, -83.92, 35.97], 2024).unwrap();
        let cell = (INNER * INNER) as usize;
        // Every chunk: band b of a pixel is b − 64; scales 0.5, but the first pixel's NaN.
        let emb: Vec<Option<Vec<u8>>> =
            p.chunks.iter().map(|_| Some((0..BANDS * cell).map(|i| ((i / cell) as i32 - 64) as i8 as u8).collect())).collect();
        let mut sc: Vec<u8> = (0..cell).flat_map(|_| 0.5f32.to_le_bytes()).collect();
        sc[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        let scales: Vec<Option<Vec<u8>>> = p.chunks.iter().map(|_| Some(sc.clone())).collect();
        let r = assemble(&p, [0, 10, 127], &emb, &scales).unwrap();
        assert_eq!((r.width as u64, r.height as u64, r.bands.len()), (p.width, p.height, 3));
        let px = |b: usize, i: usize| r.bands[b].data[i];
        let i = r.width as usize + 1;
        assert_eq!((px(0, i), px(1, i), px(2, i)), (-32.0, -27.0, 31.5));
        assert_eq!(r.crs.epsg(), 32617);
        // Chunks missing, short or junk: blank, never a panic.
        let none: Vec<Option<Vec<u8>>> = vec![None; p.chunks.len()];
        // Nothing embedded: a raster of no valid pixels (the add says there are none).
        let blank = assemble(&p, [0, 1, 2], &none, &none).unwrap();
        assert!(blank.bands.iter().all(|b| b.stats.count == 0));
        let short: Vec<Option<Vec<u8>>> = p.chunks.iter().map(|_| Some(vec![1, 2, 3])).collect();
        let _ = assemble(&p, [0, 1, 2], &short, &short);
    }

    #[test]
    fn index_entries() {
        let e = |off: u64, len: u64| [off.to_le_bytes(), len.to_le_bytes()].concat();
        let index = [e(100, 50), e(u64::MAX, u64::MAX), e(7, 0), e(7, 5000), e(u64::MAX, 50)].concat();
        assert_eq!(index_entry(&TESSERA, &index, 0, 4096).unwrap(), Some((100, 50)));
        // A chunk the shard leaves out, then damaged entries: an error, not a blank.
        assert_eq!(index_entry(&TESSERA, &index, 16, 4096).unwrap(), None);
        assert!(index_entry(&TESSERA, &index, 32, 4096).is_err());
        assert!(index_entry(&TESSERA, &index, 48, 4096).is_err());
        assert!(index_entry(&TESSERA, &index, 64, 4096).is_err());
        assert!(index_entry(&TESSERA, &index, 76, 4096).is_err());
    }
}
