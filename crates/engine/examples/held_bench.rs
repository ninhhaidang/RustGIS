//! Times a large line layer held in longitude/latitude against the same layer held in UTM 32N
//! (#2032 C2): holding it, converting it back for a tool, the first frame, a change of
//! projection, panning and zooming:
//! `cargo run --release -p rustgis-engine --example held_bench -- [vertices, default 1000000]`.
use rustgis_data::raster::Crs;
use rustgis_data::{Coord, Dataset, Geometry, GeometryType};
use rustgis_engine::render::{View, render};
use rustgis_engine::{Project, Session};
use rustgis_geom::extent_to_merc;
use rustgis_map::Camera;
use serde_json::json;
use std::time::Instant;

/// Lines of 1000 vertices each, zigzagging across a degree around 11.5°E 48.7°N.
fn lines(vertices: usize) -> Dataset {
    let mut d = Dataset::new("lines", GeometryType::Polyline, vec![]);
    let per = 1000;
    for k in 0..vertices.div_ceil(per) {
        let y = 48.2 + (k as f64 * 0.618_033).fract();
        // The last line has what is left, so the layer has the vertices asked for.
        let n = (vertices - k * per).min(per);
        let run = (0..n).map(|i| Coord::new(11.0 + i as f64 / per as f64, y + if i % 2 == 0 { 1e-4 } else { -1e-4 })).collect();
        d.push(Some(Geometry::Line(vec![run])), vec![]);
    }
    d
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() -> Result<(), String> {
    let vertices: usize = std::env::args().nth(1).and_then(|v| v.parse().ok()).unwrap_or(1_000_000);
    println!("{vertices} vertices; times in ms");
    println!("{:<34}{:>12}{:>12}", "", "lon/lat", "UTM 32N");
    let mut rows: Vec<(&str, [f64; 2])> = Vec::new();
    let mut put = |what: &'static str, i: usize, v: f64| match rows.iter_mut().find(|r| r.0 == what) {
        Some(r) => r.1[i] = v,
        None => {
            let mut r = (what, [f64::NAN; 2]);
            r.1[i] = v;
            rows.push(r);
        }
    };
    for (i, crs) in [Crs::LonLat, Crs::Epsg(32632)].into_iter().enumerate() {
        let mut d = lines(vertices);
        let t = Instant::now();
        d.hold_in(crs)?;
        put("hold in its system", i, ms(t));
        let t = Instant::now();
        let back = d.in_lonlat()?;
        put("in longitude/latitude (a tool)", i, ms(t));
        drop(back);
        let mut s = Session::new(Project::default());
        s.map_mut().layers.clear();
        s.map_mut().add_dataset(d);
        let e = extent_to_merc(&s.map().full_extent());
        let (w, h) = (1600.0, 1000.0);
        let res = (e.width() / w).max(e.height() / h);
        let mut camera = Camera { center: Coord::new((e.min_x + e.max_x) / 2.0, (e.min_y + e.max_y) / 2.0), res, bearing: 0.0, pitch: 0.0 };
        let frame = |s: &mut Session, camera: Camera| {
            let view = View { width: w, height: h, pixels_per_point: 1.0, camera, sky: false };
            let t = Instant::now();
            render(s.map_mut(), &view).ok_or("no frame")?;
            Ok::<_, String>(ms(t))
        };
        put("first frame (Web Mercator)", i, frame(&mut s, camera)?);
        put("next frame", i, frame(&mut s, camera)?);
        camera.center.x += e.width() / 10.0;
        put("pan", i, frame(&mut s, camera)?);
        camera.res /= 4.0;
        put("zoom in 4x", i, frame(&mut s, camera)?);
        s.run("map.projection", &json!({"type": "epsg:3035"}))?;
        s.run("layer.zoomTo", &json!({"layer": "lines"}))?;
        let camera = s.map().camera;
        put("first frame after projection change", i, frame(&mut s, camera)?);
        put("next frame (EPSG:3035)", i, frame(&mut s, camera)?);
    }
    for (what, [a, b]) in rows {
        println!("{what:<34}{a:>12.1}{b:>12.1}");
    }
    Ok(())
}
