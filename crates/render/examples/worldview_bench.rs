//! Times map renders with a detailed worldview (#1750): the world outline every basemap draws and
//! a Countries layer, both from one countries file, at world, Europe and city zoom.
//!
//! `cargo run --release -p rustgis-render --example worldview_bench -- countries_default.geojson`
//!
//! The file is a worldview's countries, such as Natural Earth's 1:10m ones from
//! `sample_data::WORLDVIEWS_URL` (`original/countries_default.geojson.gz`, unzipped). Without one
//! the built-in 1:110m countries are timed.
use rustgis_geom::{Coord, lonlat_to_merc};
use rustgis_map::{Basemap, Camera, Map};
use rustgis_render::sample_data::{self, Detail, View as WorldView, Worldview};
use rustgis_render::{View, render};

fn main() {
    let text = match std::env::args().nth(1) {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{path}: {e}");
                return;
            }
        },
        None => sample_data::COUNTRIES.to_string(),
    };
    let wv = WorldView::new(Worldview::DEFAULT, Detail::Original);
    if let Err(e) = sample_data::install(wv, text) {
        eprintln!("{e}");
        return;
    }
    let t = std::time::Instant::now();
    rustgis_render::prepare_world(wv);
    println!("prepare_world: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    sample_data::set_view(wv);
    let Some(ds) = sample_data::dataset(wv) else { return };
    let mut m = Map::new("m");
    let id = m.add_dataset(ds.clone());
    if let Some(l) = m.layer_mut(id) {
        l.labels.enabled = true;
    }
    let views = [("world", 0.0, 20.0, 27_000.0), ("Europe", 10.0, 50.0, 5_000.0), ("city", 2.35, 48.85, 150.0)];
    let variants: [(&str, bool, bool); 3] = [("both", true, true), ("outline only", true, false), ("layer only", false, true)];
    for (vname, lon, lat, res) in views {
        for (name, outline, layer) in variants {
            m.basemap = if outline { Basemap::default() } else { Basemap::Blank };
            if let Some(l) = m.layer_mut(id) {
                l.visible = layer;
            }
            let frame = |i: usize| {
                let c = lonlat_to_merc(Coord::new(lon, lat));
                let camera = Camera { center: Coord::new(c.x + i as f64 * res * 3.0, c.y), res, bearing: 0.0, pitch: 0.0 };
                View { width: 1480.0, height: 900.0, pixels_per_point: 1.0, camera, sky: false }
            };
            // The first frame at a zoom builds its display level (on the first variant, which draws
            // both; the others reuse it); then the caches are warm.
            let t = std::time::Instant::now();
            let _ = render(&mut m, &frame(0));
            let cold = t.elapsed().as_secs_f64() * 1000.0;
            for i in 1..3 {
                let _ = render(&mut m, &frame(i));
            }
            let n = 15;
            let t = std::time::Instant::now();
            for i in 0..n {
                let _ = render(&mut m, &frame(i));
            }
            let warm = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            if outline && layer {
                println!("{vname:7} {name:13}: {warm:6.1} ms/frame, first (building the level) {cold:6.1} ms");
            } else {
                println!("{vname:7} {name:13}: {warm:6.1} ms/frame");
            }
        }
    }
}
