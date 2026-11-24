//! Times making a vector tile style's meshes (see `tile_mesh`), against drawing its tile
//! pictures, single-threaded as in the browser:
//! `cargo run --release -p rustgis-render --example tile_mesh_bench`. Needs the network.
use rustgis_geom::{Coord, lonlat_to_merc};
use rustgis_render::rustgis_tiles::cache::Payload;
use rustgis_render::rustgis_tiles::shapes::{shapes, vector_tiles};
use rustgis_render::rustgis_tiles::{Frame, Kind, Spec, draw, template};
use rustgis_render::tile_mesh::tessellate;

fn main() {
    let spec = Spec {
        kind: Kind::Style,
        url: "https://tiles.openfreemap.org/styles/liberty".into(),
        subdomains: Vec::new(),
        min_zoom: 0,
        max_zoom: 22,
        tile_size: 512,
        tms: false,
        attribution: String::new(),
        layers: Vec::new(),
        style_layers: String::new(),
        hidden_layers: Vec::new(),
        nearest: false,
    };
    for (name, lon, lat, zoom) in
        [("Manhattan z14.3", -73.98, 40.75, 14.3f64), ("Manhattan z12.3", -73.98, 40.75, 12.3), ("Paris z15.3", 2.35, 48.86, 15.3)]
    {
        let c = lonlat_to_merc(Coord::new(lon, lat));
        let (w, h) = (1400u32, 900u32);
        let m_per_px = 2.0 * template::HALF_WORLD / (512.0 * zoom.exp2());
        let frame = Frame { left: c.x - f64::from(w) / 2.0 * m_per_px, top: c.y + f64::from(h) / 2.0 * m_per_px, m_per_px, ppp: 1.0 };
        let view =
            rustgis_geom::Extent::new(frame.left, frame.top - f64::from(h) * m_per_px, frame.left + f64::from(w) * m_per_px, frame.top);
        // Every tile fetched first.
        let Some(mut pm) = tiny_skia::Pixmap::new(w, h) else { return };
        // A minute at most a view.
        for _ in 0..12 {
            if !draw(&mut pm, &spec, 1.0, &frame, false).pending
                && vector_tiles(&spec, &frame, &view, None, None).is_some_and(|v| !v.pending)
            {
                break;
            }
            rustgis_render::rustgis_tiles::cache::wait(std::time::Duration::from_secs(5));
        }
        let Some(v) = vector_tiles(&spec, &frame, &view, None, None) else {
            println!("{name}: drawn as pictures");
            continue;
        };
        let t = std::time::Instant::now();
        let (mut vertices, mut indices, mut tiles) = (0, 0, 0);
        for d in &v.tiles {
            let Payload::Vector(tile) = &*d.payload else { continue };
            let m = tessellate(tile, &shapes(&v.style, &v.source, tile, v.level).runs, d.z);
            vertices += m.vertices.len();
            indices += m.indices.len();
            tiles += 1;
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{name}: {tiles} tiles in {ms:.0} ms on one thread, {vertices} vertices, {indices} indices, {:.1} MB",
            (vertices * std::mem::size_of::<rustgis_render::gpu_scene::TileMeshVertex>() + indices * 4) as f64 / 1e6
        );
    }
}
