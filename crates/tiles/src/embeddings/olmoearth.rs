//! OlmoEarth v1.3's embeddings (Ai2), read in place from its public Zarr 3 store on Source
//! Cooperative, laid out like Tessera's (a group a UTM zone, see [`Store`]): `embeddings` holds
//! signed bytes (year, 128 bands, y, x) in shards of 2 048² pixels, and these in inner chunks of
//! 256² pixels and 64 bands (bands 1–64 and 65–128 apart), each compressed with zstd. A value is
//! its byte's signed square, `sign(q)·(|q|/127.5)²` (`geoemb:quantization`), −128 being no data.
//!
//! A view reads the shard's index, then the inner chunks of only the band groups it shows (three
//! bands from the first 64 read one group), each about 3.6 MB: at most [`MAX_CHUNKS`] a group,
//! about 5 km a side.

use std::sync::Arc;

use rustgis_data::raster::Raster;
use rustgis_data::source::remote;

use super::tessera::{self, Plan, Store};

/// The store.
pub const ROOT: &str = "https://data.source.coop/olmoearth/olmoearth-v1-3-embeddings/embeddings.zarr";
/// A shard's side and an inner chunk's, in pixels, and the bands an inner chunk holds.
const SHARD: u64 = 2048;
const INNER: u64 = 256;
const CHUNK_BANDS: usize = 64;
/// Values a pixel.
const BANDS: usize = 128;
/// The most inner chunks of a band group one read takes (about 3.6 MB each).
pub const MAX_CHUNKS: usize = if cfg!(target_arch = "wasm32") { 4 } else { 9 };
/// The licence's terms that must travel with what is made from the data (its section 3(c)).
pub const LICENSE: &str = "OlmoEarth Artifact License (cite Ai2; no military, surveillance, policing or extractive uses such as mining, drilling or deforestation)";

/// OlmoEarth's store.
pub const OLMOEARTH: Store = Store {
    id: "olmoearth",
    name: "OlmoEarth",
    root: ROOT,
    first_year: 2017,
    last_year: 2025,
    bands: BANDS,
    shard: SHARD,
    inner: INNER,
    max_chunks: MAX_CHUNKS,
};

/// The band groups (inner chunks along the bands) three bands need, in order.
fn groups(bands: [usize; 3]) -> Vec<u64> {
    let mut g: Vec<u64> = bands.iter().map(|b| ((*b).min(BANDS - 1) / CHUNK_BANDS) as u64).collect();
    g.sort_unstable();
    g.dedup();
    g
}

/// A chunk's zstd frame, decoded: exactly `want` bytes, or an error.
fn unzstd(d: &[u8], want: usize) -> rustgis_data::Result<Vec<u8>> {
    use std::io::Read;
    let bad = |e: String| rustgis_data::DataError::Format(format!("OlmoEarth: a chunk: {e}"));
    let dec = ruzstd::decoding::StreamingDecoder::new(d).map_err(|e| bad(e.to_string()))?;
    let mut out = Vec::with_capacity(want);
    dec.take(want as u64 + 1).read_to_end(&mut out).map_err(|e| bad(e.to_string()))?;
    if out.len() != want {
        return Err(bad(format!("{} bytes, not {want}", out.len())));
    }
    Ok(out)
}

/// The window's three bands (NaN where nothing was embedded) from the chunks of the band groups
/// `of`, a plan's chunks each, as a raster in the zone's UTM coordinates.
pub fn assemble(plan: &Plan, bands: [usize; 3], of: &[u64], chunks: &[Option<Vec<u8>>]) -> Result<Raster, String> {
    let (w, h) = (plan.width as usize, plan.height as usize);
    let n = w.checked_mul(h).filter(|n| *n <= 4 << 20).ok_or("the window is too large")?;
    let mut out = vec![vec![f32::NAN; n]; 3];
    let (sx0, sy0) = (plan.shard.1 * SHARD, plan.shard.0 * SHARD);
    let per = plan.chunks.len();
    for (band, slot) in bands.iter().zip(out.iter_mut()) {
        let b = (*band).min(BANDS - 1);
        let Some(g) = of.iter().position(|g| *g == (b / CHUNK_BANDS) as u64) else { continue };
        let first = (b % CHUNK_BANDS) * (INNER * INNER) as usize;
        for (k, &(r, c)) in plan.chunks.iter().enumerate() {
            let Some(Some(chunk)) = chunks.get(g * per + k) else { continue };
            let (y0, x0) = (sy0 + r * INNER, sx0 + c * INNER);
            for row in y0.max(plan.row0)..(y0 + INNER).min(plan.row0 + plan.height) {
                for col in x0.max(plan.col0)..(x0 + INNER).min(plan.col0 + plan.width) {
                    let p = first + ((row - y0) * INNER + (col - x0)) as usize;
                    let Some(&q) = chunk.get(p) else { continue };
                    if q as i8 == i8::MIN {
                        continue;
                    }
                    let v = f32::from(q as i8) / 127.5;
                    if let Some(o) = slot.get_mut((row - plan.row0) as usize * w + (col - plan.col0) as usize) {
                        *o = v * v.abs();
                    }
                }
            }
        }
    }
    tessera::raster(plan, bands, out)
}

/// Reads a plan's window (off the caller's thread): the shard's index, then the chunks of the
/// band groups the bands need.
pub(super) fn read(plan: Plan, bands: [usize; 3], done: Box<dyn FnOnce(Result<Raster, String>) + Send>) {
    let plan = Arc::new(plan);
    let of = groups(bands);
    let file = remote::file(&plan.shard_key("embeddings"));
    let (f, p, o) = (file.clone(), plan.clone(), of.clone());
    let bytes = (INNER * INNER) as usize * CHUNK_BANDS;
    let groups_in_index = (BANDS / CHUNK_BANDS) as u64;
    remote::drive(
        file,
        Arc::new(move || tessera::read_chunks(&f, &p, groups_in_index, &o, bytes, unzstd)),
        Box::new(move |chunks: rustgis_data::Result<Vec<Option<Vec<u8>>>>| {
            done(chunks.map_err(|e| e.to_string()).and_then(|c| assemble(&plan, bands, &of, &c)))
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::tessera::Grid;

    /// Zone 23's grid, as its group's `zarr.json` gives it.
    fn grid() -> Grid {
        tessera::parse_group(
            &OLMOEARTH,
            23,
                br#"{"attributes": {"proj:code": "EPSG:32623", "spatial:transform": [10, 0.0, 0, 0.0, -10, 9502720], "spatial:shape": [1867776, 98304]}}"#,
            )
            .unwrap()
    }

    #[test]
    fn windows_north_and_south() {
        assert_eq!(tessera::store("olmoearth"), Some(&OLMOEARTH));
        // Viçosa, Brazil, about a kilometre: south of the equator, so a negative northing
        // (−2 296 km) a long way down the zone's rows; one shard, one inner chunk or a few.
        let p = tessera::plan(&OLMOEARTH, grid(), [-42.885, -20.76, -42.875, -20.75], 2025).unwrap();
        let northing = 9_502_720.0 - p.row0 as f64 * 10.0;
        assert!((northing - -2_296_000.0).abs() < 5_000.0, "{northing}");
        assert_eq!(p.shard, (p.row0 / SHARD, p.col0 / SHARD));
        assert!(!p.cut && !p.chunks.is_empty() && p.chunks.len() <= 4, "{:?}", p.chunks);
        assert!(p.width > 90 && p.width < 120 && p.height > 100 && p.height < 125);
        let (sy, sx) = p.shard;
        assert_eq!(p.shard_key("embeddings"), format!("{ROOT}/utm23/embeddings/c/8/0/{sy}/{sx}"));
        // North of the equator, the same zone (the open sea at 45° W, 2.8° N): a positive
        // northing, the first year.
        let p = tessera::plan(&OLMOEARTH, grid(), [-45.005, 2.8, -44.995, 2.81], 2017).unwrap();
        let northing = 9_502_720.0 - p.row0 as f64 * 10.0;
        assert!((northing - 310_700.0).abs() < 5_000.0, "{northing}");
        assert!(p.shard_key("embeddings").contains("/embeddings/c/0/0/"));
        // A large view: at most MAX_CHUNKS chunks, about 5 km a side, said to be cut.
        let p = tessera::plan(&OLMOEARTH, grid(), [-43.5, -21.5, -42.5, -20.5], 2024).unwrap();
        assert!(p.cut && p.chunks.len() <= MAX_CHUNKS && p.width <= 512 && p.height <= 512, "{p:?}");
        assert!(tessera::plan(&OLMOEARTH, grid(), [-42.885, -20.76, -42.875, -20.75], 2026).is_err());
        let (t, all) = tessera::tiles(&OLMOEARTH, [-43.5, -21.0, -42.5, -20.0], 2025);
        assert!(all && t.len() == 1 && t[0].dataset == "olmoearth" && t[0].url == format!("{ROOT}/utm23"));
        assert_eq!(groups([0, 1, 2]), vec![0]);
        assert_eq!(groups([70, 3, 127]), vec![0, 1]);
    }

    #[test]
    fn windows_assembled() {
        let p = tessera::plan(&OLMOEARTH, grid(), [-42.885, -20.76, -42.875, -20.75], 2025).unwrap();
        let cell = (INNER * INNER) as usize;
        // Band b of every pixel holds b − 64 (bands 64–127 in the second group's chunks), but the
        // first pixel of the window is no data (−128) in band 0.
        let chunk =
            |g: usize| -> Vec<u8> { (0..CHUNK_BANDS * cell).map(|i| ((g * CHUNK_BANDS + i / cell) as i32 - 64) as i8 as u8).collect() };
        let of = groups([1, 64, 127]);
        let mut chunks: Vec<Option<Vec<u8>>> = of.iter().flat_map(|g| p.chunks.iter().map(move |_| Some(chunk(*g as usize)))).collect();
        let (r0, c0) = p.chunks[0];
        let (y0, x0) = (p.shard.0 * SHARD + r0 * INNER, p.shard.1 * SHARD + c0 * INNER);
        let first = ((p.row0 - y0) * INNER + (p.col0 - x0)) as usize;
        chunks[0].as_mut().unwrap()[first] = 0x80;
        let r = assemble(&p, [1, 64, 127], &of, &chunks).unwrap();
        assert_eq!((r.width as u64, r.height as u64, r.crs.epsg()), (p.width, p.height, 32623));
        let i = r.width as usize + 1;
        let sq = |q: f32| (q / 127.5) * (q / 127.5).abs();
        assert_eq!((r.bands[0].data[i], r.bands[1].data[i], r.bands[2].data[i]), (sq(-63.0), sq(0.0), sq(63.0)));
        // No data in band 0 only: band 1 there is still read.
        let r = assemble(&p, [0, 1, 2], &[0], &chunks).unwrap();
        assert!(r.bands[0].data[0].is_nan() && r.bands[1].data[0] == sq(-63.0));
        // Missing, short or junk chunks: blank, never a panic.
        let blank = assemble(&p, [0, 1, 2], &[0], &vec![None; p.chunks.len()]).unwrap();
        assert!(blank.bands.iter().all(|b| b.stats.count == 0));
        let _ = assemble(&p, [0, 1, 2], &[0], &[Some(vec![1, 2, 3])]);
        assert!(unzstd(b"junk", 8).is_err());
        let frame = ruzstd::encoding::compress_to_vec(&[7u8; 64][..], ruzstd::encoding::CompressionLevel::Fastest);
        assert_eq!(unzstd(&frame, 64).unwrap(), vec![7u8; 64]);
        assert!(unzstd(&frame, 32).is_err() && unzstd(&frame, 128).is_err());
    }
}
