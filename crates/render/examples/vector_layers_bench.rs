//! Times CPU map renders of a national vector map (#1067) as a user zooms and pans: every feature
//! layer of a GeoPackage, and its heaviest layers alone, at 1× and 2× pixel scale, each frame the
//! exact frame drawn once the map rests.
//!
//! `cargo run --release -p rustgis-render --example vector_layers_bench -- map.gpkg [more files]`
//!
//! - **whole:** the whole map, four times (the first builds its level of detail);
//! - **zoom:** from the whole map to about a metre a point over its centre, 1.5× a step, and back
//!   out (the way out finds the levels of detail built on the way in, those still kept);
//! - **pan 10 m / pan 3 m:** west to east across it at 10 and 3 Web Mercator metres a point, a
//!   quarter of the view a step, so every frame brings new features in;
//! - **tilt 10 m:** the same pan at 10 m, tilted 60°;
//! - **restyle:** the top layer's fill colour changed in each of 8 frames of one view, over the
//!   whole map and at 10 m a point (#1057), then the same with every layer renamed in each frame
//!   too, so that none looks as it did: what each frame drew before a frame kept what still
//!   stands (the picture under the changed layer on the CPU, each layer's mesh on the GPU).
//!
//! `BENCH_BACKEND=gpu` times the GPU scene instead (`Backend::Gpu`): what the CPU does for a GPU
//! frame, tessellating the layers the GPU draws and drawing the rest into pictures, not the
//! device's upload and draw. It says how many frames fell back to the CPU whole, the pictures
//! and vertices (mesh vertices and point instances) the median scene holds, and, after the
//! sequences, which layers alone draw on the CPU in a flat view. `BENCH_PNG` keeps CPU frames only,
//! so it is refused with it.
//!
//! Made for swisstopo's swiss-map-vector25 (`SMV25_CHLV95LN02.gpkg`, 336,000 features in LV95,
//! from data.geo.admin.ch, one map sheet). For a profiler, `BENCH_ONLY=2,zoom` (a pixel scale and a sequence) runs
//! that sequence three times on the whole map and nothing else; `BENCH_THREADS=1` draws on one thread.
use rustgis_geom::{Coord, lonlat_to_merc};
use rustgis_map::{Basemap, Camera, Map};
use rustgis_render::gpu_scene::DrawCmd;
use rustgis_render::{Backend, RenderOutput, View, render_backend};

/// Frames kept so far (`BENCH_PNG`).
static FRAME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// What a frame came out as.
#[derive(Clone, Copy, Default)]
struct Drawn {
    /// A GPU scene was asked for and the whole frame came back drawn on the CPU.
    cpu_frame: bool,
    /// A GPU scene's pictures (layers drawn on the CPU) and their pixels.
    images: usize,
    image_px: u64,
    /// A GPU scene's mesh and extrusion vertices, and point instances.
    vertices: usize,
}

impl Drawn {
    fn of(r: &RenderOutput) -> Drawn {
        let RenderOutput::Gpu(scene) = r else { return Drawn { cpu_frame: true, ..Drawn::default() } };
        let mut d = Drawn::default();
        for c in &scene.cmds {
            match c {
                DrawCmd::Image { pixmap } => {
                    d.images += 1;
                    d.image_px += u64::from(pixmap.width()) * u64::from(pixmap.height());
                }
                DrawCmd::Mesh { vertices, .. } => d.vertices += vertices.len(),
                DrawCmd::Extrusion { vertices, .. } => d.vertices += vertices.len(),
                DrawCmd::Points { instances, .. } => d.vertices += instances.len(),
                _ => {}
            }
        }
        d
    }
}

/// Stops the run: its numbers would be wrong.
fn fail(why: impl std::fmt::Display) -> ! {
    eprintln!("{why}");
    std::process::exit(1)
}

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: vector_layers_bench map.gpkg [more files]");
        return;
    }
    // `BENCH_THREADS=1` draws on one thread (the user's thread setting), to compare.
    if let Some(n) = std::env::var("BENCH_THREADS").ok().and_then(|n| n.parse().ok())
        && let Err(e) = rustgis_data::threads::set_max(Some(n))
    {
        eprintln!("{e}");
        return;
    }
    let backend = match std::env::var("BENCH_BACKEND").as_deref() {
        Err(_) | Ok("cpu") => Backend::Cpu,
        Ok("gpu") => Backend::Gpu,
        Ok(other) => fail(format!("BENCH_BACKEND={other}: cpu or gpu")),
    };
    if backend.is_gpu() && std::env::var_os("BENCH_PNG").is_some() {
        fail("BENCH_PNG keeps CPU frames: not with BENCH_BACKEND=gpu");
    }
    println!("{} threads, {} backend", rustgis_data::threads::max(), if backend.is_gpu() { "GPU" } else { "CPU" });
    let t = std::time::Instant::now();
    // Every table of every file, or no run: "all layers" would time fewer.
    let mut read = Vec::new();
    for path in &paths {
        let file = std::path::Path::new(path);
        let names = match rustgis_data::source::layers(file) {
            Ok(n) => n,
            Err(e) => fail(format!("{path}: {e}")),
        };
        for name in names {
            match rustgis_data::source::open_path(file, Some(&name)).and_then(|src| rustgis_data::source::materialize(src.as_ref())) {
                Ok((d, _)) => read.push(d),
                Err(e) => fail(format!("{name}: {e}")),
            }
        }
    }
    println!("read {} layers in {:.1} s", read.len(), t.elapsed().as_secs_f64());
    // The highest-numbered tables (land cover) at the bottom, as a topographic map stacks them.
    let mut datasets: Vec<_> = read.into_iter().filter(|d| !d.features.is_empty()).collect();
    datasets.sort_by(|a, b| b.name.cmp(&a.name));
    let mut m = Map::new("bench");
    m.basemap = Basemap::Blank;
    let mut layers = Vec::new();
    // The data's extent, longitude/latitude: the sequences are fitted to it.
    let mut extent = rustgis_geom::Extent::EMPTY;
    for d in datasets {
        extent = extent.union(&d.extent());
        let vertices: usize = d.features.iter().filter_map(|f| f.geometry.as_ref()).map(rustgis_map::lod::vertices).sum();
        let (name, rows) = (d.name.clone(), d.features.len());
        layers.push((m.add_dataset(d), name, rows, vertices));
    }
    let total: (usize, usize) = layers.iter().fold((0, 0), |(r, v), l| (r + l.2, v + l.3));
    println!("{} layers, {} features, {} vertices", layers.len(), total.0, total.1);
    if !(extent.min_x.is_finite() && extent.max_x.is_finite() && extent.min_y.is_finite() && extent.max_y.is_finite())
        || (extent.max_x <= extent.min_x && extent.max_y <= extent.min_y)
    {
        fail("no features with an extent to fit the views to");
    }

    let (lo, hi) = (lonlat_to_merc(Coord::new(extent.min_x, extent.min_y)), lonlat_to_merc(Coord::new(extent.max_x, extent.max_y)));
    let centre = Coord::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
    // Web Mercator units a pixel showing all of it across 1920 points (a 2× screen shows the same).
    let whole = ((hi.x - lo.x) / 1920.0).max((hi.y - lo.y) / 1080.0);
    if !(whole.is_finite() && whole > 0.0) {
        fail("an extent that doesn't project to Web Mercator (not longitude/latitude?)");
    }
    println!(
        "extent {:.3},{:.3} to {:.3},{:.3}; whole map at {whole:.1} Mercator m a point",
        extent.min_x, extent.min_y, extent.max_x, extent.max_y
    );
    let camera = |c: Coord, res: f64| Camera { center: c, res, bearing: 0.0, pitch: 0.0 };
    // West to east through the centre at `res`, a quarter of the view a step, from one edge of the
    // data to the other.
    let pan = |res: f64| -> Vec<Camera> {
        let half = 960.0 * res;
        let (a, b) = (lo.x + half, (hi.x - half).max(lo.x + half));
        let steps = ((b - a) / (480.0 * res)).ceil().max(1.0) as usize;
        (0..=steps).map(|i| camera(Coord::new(a + (b - a) * i as f64 / steps as f64, centre.y), res)).collect()
    };
    // From the whole map to about a metre a point, 1.5× a step, and back out; at least the whole
    // map, for a map smaller than that.
    let mut zoom: Vec<Camera> = vec![camera(centre, whole)];
    let mut res = whole / 1.5;
    while res > 1.0 {
        zoom.push(camera(centre, res));
        res /= 1.5;
    }
    let back: Vec<Camera> = zoom.iter().rev().skip(1).copied().collect();
    zoom.extend(back);
    let tilt: Vec<Camera> = pan(10.0).into_iter().map(|c| Camera { pitch: 60.0, ..c }).collect();
    let sequences: [(&str, Vec<Camera>); 5] = [
        ("whole", (0..4).map(|_| camera(centre, whole)).collect()),
        ("zoom", zoom),
        ("pan 10 m", pan(10.0)),
        ("pan 3 m", pan(3.0)),
        ("tilt 10 m", tilt),
    ];
    let ids: Vec<u32> = layers.iter().map(|l| l.0).collect();
    let show = |m: &mut Map, only: Option<u32>| {
        for id in &ids {
            if let Some(l) = m.layer_mut(*id) {
                l.visible = only.is_none_or(|o| o == *id);
            }
        }
    };
    let view = |camera: Camera, ppp: f64| View { width: 1920.0, height: 1080.0, pixels_per_point: ppp, camera, sky: false };
    // Each frame's time, ms, and what it came out as; a frame not drawn stops the run.
    let run_drawn = |m: &mut Map, cams: &[Camera], ppp: f64| -> Vec<(f64, Drawn)> {
        cams.iter()
            .map(|c| {
                let t = std::time::Instant::now();
                let Some(r) = render_backend(m, &view(*c, ppp), backend) else { fail(format!("no frame drawn at {:.1} m a point", c.res)) };
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                // `BENCH_PNG=dir` keeps each frame drawn on the CPU, to look at or compare.
                if let (Some(dir), RenderOutput::Cpu(r)) = (std::env::var_os("BENCH_PNG"), &r) {
                    let n = FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let to = std::path::Path::new(&dir).join(format!("frame{n:03}.png"));
                    if let Err(e) = r.pixmap.save_png(&to) {
                        fail(format!("{}: {e}", to.display()));
                    }
                }
                (ms, Drawn::of(&r))
            })
            .collect()
    };
    let run = |m: &mut Map, cams: &[Camera], ppp: f64| -> Vec<f64> { run_drawn(m, cams, ppp).into_iter().map(|d| d.0).collect() };
    // For a GPU scene: how many frames came back drawn on the CPU, and a scene's pictures and
    // vertices (the median frame's).
    let scenes = |drawn: &[(f64, Drawn)]| -> String {
        if !backend.is_gpu() {
            return String::new();
        }
        let cpu = drawn.iter().filter(|d| d.1.cpu_frame).count();
        let mut gpu: Vec<Drawn> = drawn.iter().filter(|d| !d.1.cpu_frame).map(|d| d.1).collect();
        gpu.sort_by_key(|d| d.vertices);
        let Some(mid) = gpu.get(gpu.len() / 2).copied() else {
            return format!("; {cpu} of {} frames on the CPU whole: no GPU scene", drawn.len());
        };
        format!(
            "; {cpu} of {} frames on the CPU whole; a scene: {} pictures ({:.1} Mpx), {:.0}k vertices",
            drawn.len(),
            mid.images,
            mid.image_px as f64 / 1e6,
            mid.vertices as f64 / 1e3
        )
    };
    let summary = |times: &[f64]| {
        let mut s = times.to_vec();
        s.sort_by(f64::total_cmp);
        let median = s.get(s.len() / 2).copied().unwrap_or(0.0);
        let max = s.last().copied().unwrap_or(0.0);
        format!("{:>3} frames, median {median:6.0} ms, slowest {max:6.0} ms, total {:7.0} ms", s.len(), s.iter().sum::<f64>())
    };
    // The top layer's fill changed in each frame; with `all`, every layer renamed too.
    let restyle = |m: &mut Map, cam: Camera, ppp: f64, all: bool| -> Vec<(f64, Drawn)> {
        let Some(top) = ids.last().copied() else { return Vec::new() };
        (0..8u8)
            .map(|i| {
                if let Some(l) = m.layer_mut(top) {
                    let fill = rustgis_map::Rgba::rgb(40 + i * 20, 120, 200 - i * 20);
                    l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol { fill, ..l.symbol(0) });
                }
                if all {
                    for id in &ids {
                        if let Some(l) = m.layer_mut(*id) {
                            l.name = format!("{} {i}", l.name.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' '));
                        }
                    }
                }
                run_drawn(m, &[cam], ppp).remove(0)
            })
            .collect()
    };
    show(&mut m, None);
    // `BENCH_ONLY` that names no pixel scale and sequence stops the run, not runs everything.
    if let Ok(only) = std::env::var("BENCH_ONLY") {
        let Some((ppp, name, cams)) = only.split_once(',').and_then(|(p, n)| {
            let cams = &sequences.iter().find(|s| s.0 == n)?.1;
            Some((p.parse::<f64>().ok().filter(|p| p.is_finite() && *p > 0.0)?, n, cams))
        }) else {
            fail(format!("BENCH_ONLY={only}: a pixel scale and a sequence, such as 2,zoom"))
        };
        for _ in 0..3 {
            let drawn = run_drawn(&mut m, cams, ppp);
            let times: Vec<f64> = drawn.iter().map(|d| d.0).collect();
            let each: Vec<String> = times.iter().zip(cams.iter()).map(|(t, c)| format!("{:.1}m:{t:.0}", c.res)).collect();
            println!("{ppp}x {name}: {}{}\n  {}", summary(&times), scenes(&drawn), each.join(" "));
        }
        return;
    }
    // The first frame projects every layer and builds the whole map's level of detail.
    let first = run(&mut m, &[camera(centre, whole)], 1.0);
    println!("first frame (projects every layer): {:.0} ms", first.first().copied().unwrap_or(0.0));
    for ppp in [1.0f64, 2.0] {
        for (name, cams) in &sequences {
            show(&mut m, None);
            let drawn = run_drawn(&mut m, cams, ppp);
            let times: Vec<f64> = drawn.iter().map(|d| d.0).collect();
            println!("{ppp}x {name:10} all layers: {}{}", summary(&times), scenes(&drawn));
            // Each layer alone on every fourth frame (each built once already, by the whole map's
            // run): the slowest six, with what they draw.
            let sample: Vec<Camera> = cams.iter().step_by(4).copied().collect();
            let mut alone: Vec<(f64, &(u32, String, usize, usize))> = layers
                .iter()
                .map(|l| {
                    show(&mut m, Some(l.0));
                    (run(&mut m, &sample, ppp).iter().sum::<f64>() / sample.len().max(1) as f64, l)
                })
                .collect();
            alone.sort_by(|a, b| b.0.total_cmp(&a.0));
            let sum: f64 = alone.iter().map(|a| a.0).sum();
            println!("{ppp}x {name:10}   every layer alone, summed: {sum:6.0} ms a frame; the slowest:");
            for (ms, (_, layer, rows, v)) in alone.iter().take(6) {
                println!("{ppp}x {name:10}   {layer:30} {rows:>7} features {v:>8} vertices: {ms:6.0} ms a frame");
            }
        }
    }
    show(&mut m, None);
    for ppp in [1.0f64, 2.0] {
        for res in [whole, 10.0] {
            for all in [false, true] {
                let drawn = restyle(&mut m, camera(centre, res), ppp, all);
                // The first frame of a view keeps; the next seven take what still stands.
                let times: Vec<f64> = drawn.iter().skip(1).map(|d| d.0).collect();
                let what = if all { "every layer changed" } else { "the top layer restyled" };
                println!("{ppp}x restyle at {res:.1} m, {what}: {}{}", summary(&times), scenes(&drawn));
            }
        }
    }
    // The layers a GPU scene draws on the CPU: each alone, over the whole map and at 3 m, flat;
    // after the sequences, so it warms no cache they time.
    if backend.is_gpu() {
        let mut on_cpu = Vec::new();
        for (id, name, rows, v) in &layers {
            show(&mut m, Some(*id));
            let drawn = run_drawn(&mut m, &[camera(centre, whole), camera(centre, 3.0)], 1.0);
            if drawn.iter().any(|d| d.1.cpu_frame || d.1.images > 0) {
                let why = match m.layer_mut(*id) {
                    Some(l) if l.has_strokes() && l.has_fills() => "stacked strokes and fills",
                    Some(l) if l.has_strokes() => "stacked strokes",
                    Some(l) if l.has_fills() => "stacked fills",
                    _ => "other: a style the GPU can't take, a mesh over its limit or too far in",
                };
                on_cpu.push(format!("  {name:30} {rows:>7} features {v:>8} vertices: {why}"));
            }
        }
        println!("{} of {} layers draw on the CPU in a GPU scene:\n{}", on_cpu.len(), layers.len(), on_cpu.join("\n"));
    }
}
