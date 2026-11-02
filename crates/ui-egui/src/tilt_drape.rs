//! The moving tilted map. While a tilted map pans, zooms, turns or tilts, its ground drawn flat
//! once in bands (`render::render_tilt_drape`) is laid over the tilted ground as textured meshes,
//! so a frame only moves vertices, as the globe's drape does; the sky above the horizon is a
//! gradient. Extruded layers stand up over it in perspective as MapLibre's fill extrusions do:
//! tessellated once, then projected, lit and ordered far to near every frame. Point markers stand
//! upright over it, facing the viewer, as in the settled frame. The exact frame is drawn again
//! once the map settles. When the map moves past the drape, or tiles arrive, the next one is drawn
//! off the UI thread from a copy of the map (it shares the features and caches) while the last one
//! stays on screen: drawn on the UI thread, a busy map (a tiled overlay, many buildings) stopped
//! the gesture for a few hundred milliseconds each time. In the browser there are no threads: it
//! is drawn at once.

use egui::epaint::Vertex;
use egui::{Color32, Mesh, Painter, Rect, Shape, TextureFilter, TextureHandle, TextureOptions, pos2};
use rustgis_engine::geom::{Coord, Extent};
use rustgis_engine::map::camera::{Band, FAR};
use rustgis_engine::map::{Camera, Map};
use rustgis_engine::render::{self, ExtrudedShape, UprightMarker, View};

use crate::globe_drape::DrapeKey;
use crate::theme::c32;

/// How far past its band each picture reaches on every side, as a fraction of its size: the map
/// can move this far before the drape is drawn again.
const MARGIN: f64 = 0.3;
/// Zooming in past this factor of the drape's scale draws it again, sharper.
const MAX_ZOOM_IN: f64 = 2.0;
/// Seconds between drawing the drape again for tiles that have arrived meanwhile.
const TILE_REFRESH: f64 = 0.5;
/// Mesh cells along each side of a band: fine enough that the texture's straight-line
/// interpolation across a cell doesn't show the perspective.
const CELLS: usize = 16;
/// Mesh cells along each side of a band over terrain: the relief needs finer ones (the
/// settled frame's own).
const TERRAIN_CELLS: usize = render::terrain::CELLS;

struct Drape {
    key: DrapeKey,
    ppp: f32,
    /// The camera's resolution when it was drawn.
    res: f64,
    tiles: u64,
    built: f64,
    /// Each band and its picture.
    bands: Vec<(Band, TextureHandle)>,
    /// Over terrain: each band's grid of ground heights (map units), its cells a side.
    relief: Option<(usize, Vec<Vec<f64>>)>,
    /// The extruded features, tessellated.
    shapes: Vec<ExtrudedShape>,
    /// The point markers standing upright.
    markers: Vec<UprightMarker>,
}

/// A drape drawn, its pictures not yet textures.
struct Drawn {
    key: DrapeKey,
    ppp: f32,
    /// The view's camera it was drawn for.
    camera: Camera,
    tiles: u64,
    built: f64,
    patches: Vec<render::TiltPatch>,
    relief: Option<(usize, Vec<Vec<f64>>)>,
    shapes: Vec<ExtrudedShape>,
    markers: Vec<UprightMarker>,
}

/// The drape of the moving tilted map, kept between frames while the map doesn't change.
#[derive(Default)]
pub(crate) struct TiltDrape {
    drape: Option<Drape>,
    /// The next drape, being drawn from a copy of the map handed back with it, and the map
    /// change it is for.
    #[cfg(not(target_arch = "wasm32"))]
    job: Option<(DrapeKey, std::sync::mpsc::Receiver<(Drawn, Map)>)>,
    /// A job let go (its map change is past) whose worker may still be drawing: no other starts
    /// until it ends, so one worker draws at a time.
    #[cfg(not(target_arch = "wasm32"))]
    let_go: Option<std::sync::mpsc::Receiver<(Drawn, Map)>>,
    /// The map change whose drape couldn't be drawn off the UI thread (it panicked): the last one
    /// is drawn on the UI thread until the map changes.
    #[cfg(not(target_arch = "wasm32"))]
    failed: Option<DrapeKey>,
    /// The map change whose drape couldn't be drawn on the UI thread either: the last drape
    /// stays until the map changes.
    #[cfg(not(target_arch = "wasm32"))]
    broken: Option<DrapeKey>,
    /// The map change and view a worker found nothing to drape in: `warm` doesn't draw them
    /// again (each would wake the next frame) until one of them changes.
    #[cfg(not(target_arch = "wasm32"))]
    empty: Option<(DrapeKey, Camera)>,
}

impl TiltDrape {
    /// Lets go of the textures (the map no longer tilts), and of the drape being drawn.
    pub fn clear(&mut self) {
        self.drape = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.let_job_go();
            // A let-go worker already done needn't hold its map copy until the map tilts again.
            self.let_go_running();
        }
    }

    /// Lets go of the drape being drawn; its worker draws on to the end (see the `let_go` field).
    #[cfg(not(target_arch = "wasm32"))]
    fn let_job_go(&mut self) {
        if let Some((_, rx)) = self.job.take() {
            self.let_go = Some(rx);
        }
    }

    /// Whether the worker of a job let go is still drawing.
    #[cfg(not(target_arch = "wasm32"))]
    fn let_go_running(&mut self) -> bool {
        let running = self.let_go.as_ref().is_some_and(|rx| matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        if !running {
            self.let_go = None;
        }
        running
    }

    /// Whether the next drape is being drawn.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn busy(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.job.is_some();
        #[cfg(target_arch = "wasm32")]
        return false;
    }

    /// The drape drawn off the UI thread, once it is ready; the caches its copy of the map
    /// filled go to `map`.
    #[cfg(not(target_arch = "wasm32"))]
    fn take(&mut self, map: &mut Map) -> Option<Drawn> {
        let (key, rx) = self.job.as_ref()?;
        let key = *key;
        match rx.try_recv() {
            Ok((drawn, copy)) => {
                self.job = None;
                map.adopt_caches(&copy);
                Some(drawn)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            // The worker ended without an answer: it panicked, for the map change it was started
            // for.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.failed = Some(key);
                None
            }
        }
    }

    /// Starts drawing the drape of `map` (a copy) for `view` on a worker, waking `ctx` when it
    /// is done. False when no thread could be started.
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_arguments)]
    fn start(&mut self, ctx: &egui::Context, mut map: Map, view: View, max_side: u32, key: DrapeKey, tiles: u64, time: f64) -> bool {
        self.start_with(ctx, key, move || {
            let drawn = draw(&mut map, &view, max_side, key, tiles, time);
            (drawn, map)
        })
    }

    /// Starts `work` drawing the next drape (see [`TiltDrape::start`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn start_with(&mut self, ctx: &egui::Context, key: DrapeKey, work: impl FnOnce() -> (Drawn, Map) + Send + 'static) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let work = std::panic::AssertUnwindSafe(work);
        let diagnostics = rustgis_engine::data::diagnostics::Capture::current();
        let spawned = std::thread::Builder::new().name("rustgis-tilt-drape".into()).spawn(move || {
            let _diagnostics = diagnostics.enter();
            // Without an answer (a panic) `take` finds the channel closed. It is closed before the
            // frame is woken, so the woken frame finds the job over, not still running.
            if let Ok(done) = std::panic::catch_unwind(work) {
                let _ = tx.send(done);
            }
            drop(tx);
            ctx.request_repaint();
        });
        if spawned.is_ok() {
            self.job = Some((key, rx));
        }
        spawned.is_ok()
    }

    /// Makes `drawn` the drape, its pictures textures (reusing the last drape's).
    fn show(&mut self, ctx: &egui::Context, drawn: Drawn) {
        // Something to drape: `warm` keeps it up to date again.
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.empty = None;
        }
        let options = TextureOptions { mipmap_mode: Some(TextureFilter::Linear), ..TextureOptions::LINEAR };
        let mut old = self.drape.take().map(|d| d.bands).unwrap_or_default().into_iter().map(|b| b.1);
        let bands: Vec<(Band, TextureHandle)> = drawn
            .patches
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let img =
                    egui::ColorImage::from_rgba_premultiplied([p.pixmap.width() as usize, p.pixmap.height() as usize], p.pixmap.data());
                let tex = match old.next() {
                    Some(mut t) => {
                        t.set(img, options);
                        t
                    }
                    None => ctx.load_texture(format!("tilt_drape_{i}"), img, options),
                };
                (p.band, tex)
            })
            .collect();
        let Drawn { key, ppp, camera, tiles, built, relief, shapes, markers, .. } = drawn;
        self.drape = Some(Drape { key, ppp, res: camera.res, tiles, built, bands, relief, shapes, markers });
    }

    /// Shows the drape a worker finished for this map change (`key`, at `drape_ppp`), and lets
    /// go of a job for an earlier one (an edit while it ran): its drape would be thrown away.
    #[cfg(not(target_arch = "wasm32"))]
    fn adopt(&mut self, ctx: &egui::Context, map: &mut Map, key: DrapeKey, drape_ppp: f32) {
        // One begun before the drape shown was drawn (by `prepare`, on the UI thread) is older.
        if let Some(drawn) = self.take(map)
            && drawn.key == key
            && drawn.ppp == drape_ppp
            && self.drape.as_ref().is_none_or(|d| d.built <= drawn.built)
        {
            if drawn.patches.is_empty() {
                // Nothing to drape where the view was when it started; it may have moved on to
                // ground since, so the drape is drawn here for where it is now.
                self.drape = None;
                self.empty = Some((key, drawn.camera));
            } else {
                self.show(ctx, drawn);
            }
        }
        if self.job.as_ref().is_some_and(|(k, _)| *k != key) {
            self.let_job_go();
        }
    }

    /// Whether the drape shows this map change as it is now over the `w × h` view of `cam`:
    /// drawn at about its zoom, covering it, and with the tiles come since (or drawn just now).
    #[allow(clippy::too_many_arguments)]
    fn fresh(&self, cam: &Camera, key: DrapeKey, drape_ppp: f32, tiles: u64, w: f64, h: f64, time: f64) -> bool {
        self.drape.as_ref().is_some_and(|d| {
            d.key == key
                && d.ppp == drape_ppp
                && cam.res * MAX_ZOOM_IN >= d.res
                && (d.tiles == tiles || time - d.built < TILE_REFRESH)
                && covers(cam, w, h, &d.bands)
        })
    }

    /// While the map rests, draws its drape again off the UI thread when it is out of date (tiles
    /// came, the view moved past it) or there is none, so the next gesture starts on a drape of
    /// the map as it is: one drawn when the gesture starts shows what the map showed then (on a
    /// map just opened, nothing). In the browser, with no threads, the gesture draws it.
    #[allow(clippy::too_many_arguments)]
    pub fn warm(&mut self, ctx: &egui::Context, map: &mut Map, key: DrapeKey, tiles: u64, w: f64, h: f64, ppp: f32, time: f64) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let drape_ppp = ppp.min(1.0);
            self.adopt(ctx, map, key, drape_ppp);
            if self.busy()
                || self.let_go_running()
                || self.failed == Some(key)
                || self.empty == Some((key, map.camera))
                || self.fresh(&map.camera, key, drape_ppp, tiles, w, h, time)
            {
                return;
            }
            let max_side = ctx.input(|i| i.max_texture_side).clamp(256, 4096) as u32;
            let view = View { width: w, height: h, pixels_per_point: f64::from(drape_ppp), camera: map.camera, sky: true };
            render::sync_tilted_view(map, &view);
            // No worker: the next gesture draws it on the UI thread, as `prepare` does.
            if !self.start(ctx, map.clone(), view, max_side, key, tiles, time) {
                self.failed = Some(key);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (ctx, map, key, tiles, w, h, ppp, time);
    }

    /// Makes sure the drape covers the tilted `w × h` view of `map`, drawing it again when the
    /// view has moved or zoomed past it or the map changed. The first drape of a map change is
    /// drawn at once; one following the view is drawn off the UI thread, the last staying on
    /// screen meanwhile. False when there is nothing to drape.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(&mut self, ctx: &egui::Context, map: &mut Map, key: DrapeKey, tiles: u64, w: f64, h: f64, ppp: f32, time: f64) -> bool {
        let cam = map.camera;
        // A screen's worth of detail at most: the settled frame is sharp.
        let drape_ppp = ppp.min(1.0);
        #[cfg(not(target_arch = "wasm32"))]
        self.adopt(ctx, map, key, drape_ppp);
        if self.fresh(&cam, key, drape_ppp, tiles, w, h, time) {
            return true;
        }
        let max_side = ctx.input(|i| i.max_texture_side).clamp(256, 4096) as u32;
        let view = View { width: w, height: h, pixels_per_point: f64::from(drape_ppp), camera: cam, sky: true };
        // The view moved on, or tiles came: the last drape shows until the next is drawn. One
        // that couldn't be drawn off the UI thread is drawn here.
        #[cfg(not(target_arch = "wasm32"))]
        if self.drape.as_ref().is_some_and(|d| d.key == key && d.ppp == drape_ppp) && self.failed != Some(key) {
            if self.busy() || self.let_go_running() {
                return true;
            }
            // Read what the view shows into the map itself, as the settled frame does.
            render::sync_tilted_view(map, &view);
            if self.start(ctx, map.clone(), view, max_side, key, tiles, time) {
                return true;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let drawn = if self.failed == Some(key) {
            if self.broken == Some(key) {
                return self.drape.is_some();
            }
            // On a copy, the panic caught: one from the map's own data comes again here.
            let mut copy = map.clone();
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw(&mut copy, &view, max_side, key, tiles, time))) {
                Ok(drawn) => {
                    map.adopt_caches(&copy);
                    drawn
                }
                Err(_) => {
                    self.broken = Some(key);
                    return self.drape.is_some();
                }
            }
        } else {
            draw(map, &view, max_side, key, tiles, time)
        };
        #[cfg(target_arch = "wasm32")]
        let drawn = draw(map, &view, max_side, key, tiles, time);
        if drawn.patches.is_empty() {
            self.drape = None;
            return false;
        }
        self.show(ctx, drawn);
        true
    }

    /// Paints the tilted map in `rect`: the sky, and the drape's bands, farthest first.
    pub fn paint(&self, painter: &Painter, rect: Rect, map: &Map) {
        let (w, h) = (rect.width() as f64, rect.height() as f64);
        let cam = map.camera;
        let (sky, fog) = render::tilt_sky(map);
        painter.rect_filled(rect, 0.0, c32(fog));
        if let Some(y) = cam.sky_y(h) {
            let y = rect.min.y + (y as f32).min(rect.height());
            let mut m = Mesh::default();
            let (top, bottom) = (c32(sky), c32(fog));
            for (p, c) in [(rect.left_top(), top), (rect.right_top(), top), (pos2(rect.max.x, y), bottom), (pos2(rect.min.x, y), bottom)] {
                m.colored_vertex(p, c);
            }
            m.add_triangle(0, 1, 2);
            m.add_triangle(0, 2, 3);
            painter.add(Shape::mesh(m));
        }
        for mesh in self.meshes(rect, &cam, w, h) {
            painter.add(Shape::mesh(mesh));
        }
        if let Some(mesh) = self.extrusions(rect, &cam, fog) {
            painter.add(Shape::mesh(mesh));
        }
        if let Some(mesh) = self.markers(rect, &cam, fog) {
            painter.add(Shape::mesh(mesh));
        }
    }

    /// The point markers standing upright on the drape, as one mesh in their order: each one's
    /// selection halo, fill and outline, sized and fogged by its distance as in the settled frame.
    fn markers(&self, rect: Rect, cam: &Camera, fog: rustgis_engine::map::Rgba) -> Option<Mesh> {
        let d = self.drape.as_ref()?;
        let (w, h) = (rect.width() as f64, rect.height() as f64);
        let mut mesh = Mesh::default();
        for m in &d.markers {
            let Some((x, y, k, t)) = m.place(cam, w, h) else { continue };
            let s = m.look(k, fog, t);
            let (x, y) = (rect.min.x + x as f32, rect.min.y + y as f32);
            let r = s.size.max(1.0) / 2.0;
            let reach = r * 1.2 + s.stroke_width.max(0.0) + 4.0;
            if x < rect.min.x - reach || y < rect.min.y - reach || x > rect.max.x + reach || y > rect.max.y + reach {
                continue;
            }
            let outline = render::marker_outline(s.marker, x, y, r);
            if m.selected {
                ring(&mut mesh, (x, y), &outline, 1.75, c32(render::SELECTION));
            }
            fan(
                &mut mesh,
                std::iter::once(pos2(x, y))
                    .chain(outline.iter().map(|p| pos2(p.0, p.1)))
                    .chain(outline.first().map(|p| pos2(p.0, p.1)))
                    .map(Some),
                c32(s.fill),
            );
            if s.stroke_width > 0.0 && s.stroke.a() > 0 {
                ring(&mut mesh, (x, y), &outline, s.stroke_width / 2.0, c32(s.stroke));
            }
        }
        (!mesh.indices.is_empty()).then_some(mesh)
    }

    /// The extruded features standing on the drape in perspective, as one mesh: features far to
    /// near, each one's walls (facing the eye, lit by their facing and darker toward their foot,
    /// far first) then its roof; fading into `fog` with distance. Heights are each
    /// feature's own, in the camera's units at its place.
    fn extrusions(&self, rect: Rect, cam: &Camera, fog: rustgis_engine::map::Rgba) -> Option<Mesh> {
        let d = self.drape.as_ref()?;
        let (w, h) = (rect.width() as f64, rect.height() as f64);
        let pos = |p: (f64, f64)| pos2(rect.min.x + p.0 as f32, rect.min.y + p.1 as f32);
        let ground =
            |s: &ExtrudedShape, p: &[f32; 2]| cam.ground_px(Coord::new(s.origin.x + f64::from(p[0]), s.origin.y + f64::from(p[1])));
        // Far (up the screen) first. A feature is drawn while any of its outline is in front of
        // the eye and within the view's reach (past it is sky); its middle places it and fogs it.
        let mut order: Vec<(f64, f64, usize)> = d
            .shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.rings.iter().flatten().any(|c| cam.depth_scale(*c, h).is_some_and(|k| k * FAR >= 1.0)))
            .map(|(i, s)| {
                let depth = cam.depth_scale(s.center, h).map_or(1.0, |k| 1.0 / k.max(1e-9));
                (cam.ground_px(s.center).1, depth, i)
            })
            .collect();
        order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        type Rings = Vec<Vec<(f64, f64)>>;
        let to_ground = |rings: &[Vec<Coord>]| -> Rings { rings.iter().map(|r| r.iter().map(|c| cam.ground_px(*c)).collect()).collect() };
        let shapes: Vec<(&ExtrudedShape, f64, f64, Rings, Rings)> = order
            .iter()
            .filter_map(|&(_, depth, i)| {
                let s = d.shapes.get(i)?;
                Some((s, depth, (s.height / cam.res.max(1e-9)).clamp(0.0, 1e7), to_ground(&s.rings), to_ground(&s.holes)))
            })
            .collect();
        let mut mesh = Mesh::default();
        // Shadows on the ground first, under every feature.
        let shade = Color32::from_black_alpha(60);
        for (s, _, up, rings, _) in shapes.iter().filter(|x| x.0.shadows) {
            for q in render::tilted_shadow_quads(cam, w, h, rings, *up) {
                fan(&mut mesh, q.into_iter().map(|p| Some(pos(p))), shade);
            }
            // The footprint moved away from the light: the roof's triangles on the ground.
            let (du, dv) = render::shadow_shift(*up);
            for [a, b, c] in s.roof_indices.as_chunks::<3>().0 {
                let tri: Option<Vec<(f64, f64)>> =
                    [a, b, c].iter().map(|&&k| s.roof.get(k as usize).map(|p| ground(s, p)).map(|(u, v)| (u + du, v + dv))).collect();
                let Some(tri) = tri else { continue };
                let cut = render::clip_near(&tri, cam.near_v(0.0, h));
                fan(&mut mesh, cut.into_iter().map(|(u, v)| cam.project_raised(u, v, 0.0, w, h).map(pos)), shade);
            }
        }
        for (s, depth, up, rings, holes) in &shapes {
            let t = ((depth - FAR * 0.5) / (FAR * 0.5)).clamp(0.0, 1.0);
            let base = s.fill.lerp(fog, t * t * (3.0 - 2.0 * t));
            for wall in render::tilted_walls(cam, w, h, rings, holes, *up, s.face_culling, s.lighting) {
                let (top, foot) = (c32(base.shade(wall.top)), c32(base.shade(wall.foot)));
                let [fa, fb, tb, ta] = wall.quad;
                let k = mesh.vertices.len() as u32;
                for (p, c) in [(fa, foot), (fb, foot), (tb, top), (ta, top)] {
                    mesh.colored_vertex(pos(p), c);
                }
                mesh.add_triangle(k, k + 1, k + 2);
                mesh.add_triangle(k, k + 2, k + 3);
            }
            // The roof's triangles, each cut where it reaches behind the eye.
            let color = c32(base);
            let v_max = cam.near_v(*up, h);
            for [a, b, c] in s.roof_indices.as_chunks::<3>().0 {
                let tri: Option<Vec<(f64, f64)>> = [a, b, c].iter().map(|&&k| s.roof.get(k as usize).map(|p| ground(s, p))).collect();
                let Some(tri) = tri else { continue };
                let cut = render::clip_near(&tri, v_max);
                fan(&mut mesh, cut.into_iter().map(|(u, v)| cam.project_raised(u, v, *up, w, h).map(pos)), color);
            }
        }
        (!mesh.indices.is_empty()).then_some(mesh)
    }

    /// The drape's meshes for `cam` in `rect`, farthest band first. A band with nothing in front
    /// of the camera has no mesh: egui-wgpu can't draw an empty one. Over terrain each vertex is
    /// raised to the ground's height, each band keeps to its own stretch of ground, and its
    /// cells are ordered far to near, so nearer slopes hide what lies behind them.
    fn meshes(&self, rect: Rect, cam: &Camera, w: f64, h: f64) -> Vec<Mesh> {
        let Some(d) = &self.drape else { return Vec::new() };
        let last = d.bands.len().saturating_sub(1);
        d.bands
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(bi, (b, tex))| {
                let relief = d.relief.as_ref().and_then(|(n, hs)| Some((*n, hs.get(bi)?)));
                let cells = relief.map_or(CELLS, |(n, _)| n);
                let n = cells + 1;
                let mut mesh = Mesh::with_texture(tex.id());
                let mut depth = Vec::with_capacity(n * n);
                for j in 0..n {
                    for i in 0..n {
                        let (u, v) = (i as f64 / cells as f64, j as f64 / cells as f64);
                        let m = b.camera.to_map(u * b.width, v * b.height, b.width, b.height);
                        // Past the view's reach is sky, as in the settled frame.
                        let k = cam.depth_scale(m, h).filter(|k| *k >= 1.0 / FAR);
                        let up = relief.and_then(|(_, hs)| hs.get(j * n + i)).map_or(0.0, |hm| hm / cam.res.max(1e-9));
                        let (gu, gv) = cam.ground_px(m);
                        let s = cam.project_raised(gu, gv, up, w, h).filter(|s| k.is_some() && s.0.is_finite() && s.1.is_finite());
                        depth.push(s.and(k).map(|k| 1.0 / k));
                        let (x, y) = s.unwrap_or((0.0, 0.0));
                        mesh.vertices.push(Vertex {
                            pos: pos2(rect.min.x + x as f32, rect.min.y + y as f32),
                            uv: pos2(u as f32, v as f32),
                            color: Color32::WHITE,
                        });
                    }
                }
                // Over terrain, its own ground: the nearest band reaches the bottom of the view,
                // the farthest the sky; a little over at the seams.
                let (d0, d1) = if relief.is_some() {
                    let (d0, d1) = (if bi == 0 { 0.0 } else { b.depth.0 }, if bi == last { f64::INFINITY } else { b.depth.1 });
                    let margin = ((d1.min(FAR) - d0) * 0.02).max(0.0);
                    (d0 - margin, d1 + margin)
                } else {
                    (f64::NEG_INFINITY, f64::INFINITY)
                };
                let mut quads: Vec<(f64, [u32; 4])> = Vec::new();
                for j in 0..cells {
                    for i in 0..cells {
                        let k = j * n + i;
                        let quad = [k, k + 1, k + n, k + n + 1];
                        let ds: Option<Vec<f64>> = quad.iter().map(|&q| depth.get(q).copied().flatten()).collect();
                        let Some(ds) = ds else { continue };
                        let mean = ds.iter().sum::<f64>() / 4.0;
                        if mean >= d0 && mean <= d1 {
                            quads.push((mean, quad.map(|q| q as u32)));
                        }
                    }
                }
                if relief.is_some() {
                    quads.sort_by(|a, b| b.0.total_cmp(&a.0));
                }
                for (_, [a, b, c, e]) in quads {
                    mesh.add_triangle(a, b, e);
                    mesh.add_triangle(a, e, c);
                }
                (!mesh.indices.is_empty()).then_some(mesh)
            })
            .collect()
    }
}

/// Draws the drape of `map` for `view`: its bands flat, at most `max_side` pixels a side, the
/// extruded features and point markers standing on it, and over terrain the ground's height
/// under each band's grid.
fn draw(map: &mut Map, view: &View, max_side: u32, key: DrapeKey, tiles: u64, time: f64) -> Drawn {
    let cam = view.camera;
    let ppp = view.pixels_per_point as f32;
    let patches = render::render_tilt_drape(map, view, MARGIN, max_side);
    if patches.is_empty() {
        return Drawn { key, ppp, camera: cam, tiles, built: time, patches, relief: None, shapes: Vec::new(), markers: Vec::new() };
    }
    // A turned flat map's band is a square over the circle round the view: what stands on it is
    // taken over all of it, as the view turns there.
    let reach = match render::turned_band(view) {
        Some(b) => View { width: b.width, height: b.height, camera: b.camera, ..*view },
        None => *view,
    };
    let shapes = render::extruded_shapes(map, &reach);
    let markers = render::upright_markers(map, &reach, MARGIN);
    // The ground's height under each vertex, read once.
    let relief = render::terrain::Ground::of(map, &cam).map(|g| {
        let n = TERRAIN_CELLS;
        let heights = patches
            .iter()
            .map(|p| {
                let b = &p.band;
                (0..=n)
                    .flat_map(|j| (0..=n).map(move |i| (i, j)))
                    .map(|(i, j)| {
                        let m = b.camera.to_map(b.width * i as f64 / n as f64, b.height * j as f64 / n as f64, b.width, b.height);
                        g.height(m).map_or(0.0, |h| h / g.per_unit(m))
                    })
                    .collect()
            })
            .collect();
        (n, heights)
    });
    Drawn { key, ppp, camera: cam, tiles, built: time, patches, relief, shapes, markers }
}

/// Adds a convex polygon (screen points, `None` where one couldn't be placed) to `mesh` as a fan
/// of triangles in one colour; nothing when a point is missing or there are fewer than three.
fn fan(mesh: &mut Mesh, pts: impl Iterator<Item = Option<egui::Pos2>>, color: Color32) {
    let pts: Option<Vec<egui::Pos2>> = pts.map(|p| p.filter(|p| p.x.is_finite() && p.y.is_finite())).collect();
    let Some(pts) = pts.filter(|p| p.len() >= 3) else { return };
    let k = mesh.vertices.len() as u32;
    for p in &pts {
        mesh.colored_vertex(*p, color);
    }
    for i in 1..pts.len() as u32 - 1 {
        mesh.add_triangle(k, k + i, k + i + 1);
    }
}

/// Adds a band `half` points either side of `outline` (a polygon seen whole from `centre`) to
/// `mesh` in one colour, its points moved along the line from the centre: a marker's outline.
fn ring(mesh: &mut Mesh, centre: (f32, f32), outline: &[(f32, f32)], half: f32, color: Color32) {
    let n = outline.len() as u32;
    if n < 3 {
        return;
    }
    let k = mesh.vertices.len() as u32;
    for &(px, py) in outline {
        let (dx, dy) = (px - centre.0, py - centre.1);
        let len = dx.hypot(dy).max(1e-6);
        let (ux, uy) = (dx / len, dy / len);
        mesh.colored_vertex(pos2(px - ux * half, py - uy * half), color);
        mesh.colored_vertex(pos2(px + ux * half, py + uy * half), color);
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b, c, e) = (k + 2 * i, k + 2 * i + 1, k + 2 * j, k + 2 * j + 1);
        mesh.add_triangle(a, b, e);
        mesh.add_triangle(a, e, c);
    }
}

/// Whether the bands cover the ground a tilted `w × h` view of `cam` shows (sampled on a grid;
/// the sky needs no cover).
fn covers(cam: &Camera, w: f64, h: f64, bands: &[(Band, TextureHandle)]) -> bool {
    let extents: Vec<Extent> = bands.iter().map(|(b, _)| b.camera.extent(b.width, b.height)).collect();
    let n = 6;
    (0..=n).all(|j| {
        (0..=n).all(|i| {
            let (x, y) = (w * f64::from(i) / f64::from(n), h * f64::from(j) / f64::from(n));
            match cam.try_to_map(x, y, w, h) {
                Some(m) => extents.iter().any(|e| e.contains(Coord::new(m.x, m.y))),
                None => true,
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use rustgis_engine::geom::lonlat_to_merc;

    use super::*;

    fn tilted(pitch: f64) -> Map {
        let mut m = Map::new("m");
        m.camera = Camera { center: lonlat_to_merc(Coord::new(10.0, 48.0)), res: 2_000.0, bearing: 20.0, pitch };
        m
    }

    fn fog() -> rustgis_engine::map::Rgba {
        rustgis_engine::map::Rgba::rgb(200, 210, 220)
    }

    #[test]
    fn extrusions_stand_up_on_the_drape() {
        use rustgis_engine::data::{Dataset, Geometry, GeometryType};
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(55.0);
        let mut ds = Dataset::new("blocks", GeometryType::Polygon, vec![]);
        let ring =
            vec![Coord::new(9.0, 47.0), Coord::new(11.0, 47.0), Coord::new(11.0, 49.0), Coord::new(9.0, 49.0), Coord::new(9.0, 47.0)];
        ds.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let id = m.add_dataset(ds);
        if let Some(l) = m.layer_mut(id) {
            l.extrusion = Some(rustgis_engine::map::effects::Extrusion {
                field: None,
                height: 100_000.0,
                factor: 1.0,
                face_culling: true,
                lighting: true,
                shadows: false,
            });
        }
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.0));
        let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let mesh = d.extrusions(rect, &m.camera, fog()).expect("the block stands up");
        assert!(mesh.is_valid() && mesh.indices.len() > 6);
        // A long block whose first point is behind the eye still stands up where it reaches into
        // view (it was kept or dropped by that point alone, and vanished while moving).
        if let Some(l) = m.layer_mut(id) {
            let long = vec![
                Coord::new(10.0, 20.0),
                Coord::new(11.0, 20.0),
                Coord::new(11.0, 48.5),
                Coord::new(10.0, 48.5),
                Coord::new(10.0, 20.0),
            ];
            l.dataset.features.clear();
            l.dataset.push(Some(Geometry::Polygon(vec![vec![long]])), vec![]);
            l.touch_geometry();
        }
        let behind = rustgis_engine::geom::lonlat_to_merc(Coord::new(10.0, 20.0));
        assert!(m.camera.try_to_screen(behind, 800.0, 600.0).is_none(), "the first point is behind the eye");
        let mut d2 = TiltDrape::default();
        assert!(d2.prepare(&ctx, &mut m, DrapeKey { rev: 1, sources: 0 }, 0, 800.0, 600.0, 1.0, 0.0));
        assert!(d2.extrusions(rect, &m.camera, fog()).is_some_and(|mesh| mesh.is_valid() && !mesh.indices.is_empty()));
        // Its roof stands above its foot on screen.
        let foot = m.camera.to_screen(rustgis_engine::geom::lonlat_to_merc(Coord::new(10.0, 47.0)), 800.0, 600.0).1 as f32;
        let top = mesh.vertices.iter().map(|v| v.pos.y).fold(f32::MAX, f32::min);
        assert!(top < foot - 20.0, "{top} vs {foot}");
    }

    #[test]
    fn point_markers_stand_upright_on_the_drape() {
        use rustgis_engine::data::{Dataset, Geometry, GeometryType};
        let ctx = egui::Context::default();
        let mut m = tilted(70.0);
        let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(10.0, 48.0))), vec![]);
        let id = m.add_dataset(ds);
        if let Some(l) = m.layer_mut(id) {
            l.selection = vec![0];
        }
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, DrapeKey { rev: 0, sources: 0 }, 0, 800.0, 600.0, 1.0, 0.0));
        let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let mesh = d.markers(rect, &m.camera, fog()).expect("the marker stands up");
        assert!(mesh.is_valid());
        // At the centre, as tall as it is wide: facing the viewer, not lying on the ground.
        let (mut lo, mut hi) = (pos2(f32::MAX, f32::MAX), pos2(f32::MIN, f32::MIN));
        for v in &mesh.vertices {
            (lo.x, lo.y, hi.x, hi.y) = (lo.x.min(v.pos.x), lo.y.min(v.pos.y), hi.x.max(v.pos.x), hi.y.max(v.pos.y));
        }
        let (w, h) = (hi.x - lo.x, hi.y - lo.y);
        assert!(w > 6.0 && (w - h).abs() < 0.5, "{w} × {h}");
        assert!(((lo.x + hi.x) / 2.0 - 400.0).abs() < 1.0 && ((lo.y + hi.y) / 2.0 - 300.0).abs() < 1.0);
        // Behind the camera, it isn't drawn.
        m.camera.center = rustgis_engine::geom::lonlat_to_merc(Coord::new(10.0, 70.0));
        assert!(d.markers(rect, &m.camera, fog()).is_none());
    }

    #[test]
    fn the_drape_stands_on_its_terrain() {
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let ctx = egui::Context::default();
        // A 4000 m hill around (10°, 48°), where the view looks.
        let n = 32usize;
        let heights: Vec<f32> = (0..n * n)
            .map(|i| {
                let (x, y) = ((i % n) as f32 / n as f32 - 0.5, (i / n) as f32 / n as f32 - 0.5);
                4000.0 * (-(x * x + y * y) * 20.0).exp()
            })
            .collect();
        let gt = GeoTransform { origin_x: 9.0, origin_y: 49.0, pixel_width: 2.0 / n as f64, pixel_height: -2.0 / n as f64 };
        let dem = Raster::new("dem", n as u32, n as u32, vec![heights], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let drape = |terrain: bool| {
            let mut m = tilted(60.0);
            m.camera.res = 300.0;
            let id = m.add_raster("dem", rustgis_engine::map::RasterLayer::from_path(dem.clone(), "dem.tif"));
            if let Some(l) = m.layer_mut(id) {
                l.visible = false;
            }
            if terrain {
                m.view.terrain = Some(rustgis_engine::map::view::Terrain {
                    source: rustgis_engine::map::view::TerrainSource::Layer("dem".into()),
                    exaggeration: 3.0,
                });
            }
            let mut d = TiltDrape::default();
            assert!(d.prepare(&ctx, &mut m, DrapeKey { rev: 0, sources: 0 }, 0, 800.0, 600.0, 1.0, 0.0));
            let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
            let meshes = d.meshes(rect, &m.camera, 800.0, 600.0);
            assert!(!meshes.is_empty() && meshes.iter().all(|mesh| mesh.is_valid()));
            d
        };
        // Flat, no relief; on the hill's terrain, the ground's heights under the grid, up to the
        // hill's: 12 km exaggerated, about 17,900 map units where a unit spans 0.67 m (48°).
        assert!(drape(false).drape.as_ref().is_some_and(|d| d.relief.is_none()));
        let d = drape(true);
        let (_, heights) = d.drape.as_ref().and_then(|d| d.relief.as_ref()).expect("relief");
        let most = heights.iter().flatten().copied().fold(0.0, f64::max);
        assert!(most > 12_000.0 && most < 18_000.0, "{most}");
    }

    #[test]
    fn the_drape_covers_the_view_and_is_kept_while_it_does() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(60.0);
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.0));
        let built = d.drape.as_ref().map(|d| d.built);
        assert!(d.drape.as_ref().is_some_and(|d| d.bands.len() >= 2), "near and far bands");
        // A small pan stays inside the margin: nothing is drawn.
        m.camera.pan_px(30.0, 20.0);
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.1));
        assert_eq!(d.drape.as_ref().map(|d| d.built), built);
        // No extruded layer, no extrusion mesh.
        assert!(d.extrusions(Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0)), &m.camera, fog()).is_none());
        // Every band in front of the camera gives a valid mesh.
        let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let meshes = d.meshes(rect, &m.camera, 800.0, 600.0);
        assert!(!meshes.is_empty() && meshes.iter().all(|mesh| !mesh.indices.is_empty() && mesh.is_valid()));
        // Zooming in past the drape's detail draws it again, off the UI thread: the last one
        // shows meanwhile.
        m.camera.res /= 3.0;
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.2));
        assert!(d.busy() && d.drape.as_ref().map(|d| d.built) == built, "the last drape stays while the next is drawn");
        wait(&mut d, &ctx, &mut m, key, 0.2);
        assert_ne!(d.drape.as_ref().map(|d| d.built), built);
        // So does panning far away.
        let built = d.drape.as_ref().map(|d| d.built);
        m.camera.pan_px(5_000.0, 0.0);
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.3));
        wait(&mut d, &ctx, &mut m, key, 0.3);
        assert_ne!(d.drape.as_ref().map(|d| d.built), built);
        assert!(covers(&m.camera, 800.0, 600.0, d.drape.as_ref().map(|d| &d.bands[..]).unwrap_or_default()));
        // A change to the map draws its drape at once.
        let built = d.drape.as_ref().map(|d| d.built);
        let changed = DrapeKey { rev: 1, sources: 0 };
        assert!(d.prepare(&ctx, &mut m, changed, 0, 800.0, 600.0, 1.0, 0.4));
        assert!(d.drape.as_ref().is_some_and(|d| d.key == changed && Some(d.built) != built));
    }

    /// A flat map turning is laid over by one north-up band round the view: turned all the way
    /// round, the view stays on it, and nothing is drawn again.
    #[test]
    fn a_turning_flat_map_keeps_its_drape() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(0.0);
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.0));
        let built = d.drape.as_ref().map(|d| d.built);
        let band = d.drape.as_ref().and_then(|d| d.bands.first()).map(|(b, _)| *b).unwrap();
        assert_eq!(d.drape.as_ref().map(|d| d.bands.len()), Some(1));
        assert_eq!(band.camera.bearing, 0.0, "drawn facing north");
        let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        for step in 0..72 {
            m.camera.bearing = f64::from(step) * 5.0 + 0.5;
            assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.01 * f64::from(step)));
            assert_eq!(d.drape.as_ref().map(|d| d.built), built, "{}°", m.camera.bearing);
            assert!(covers(&m.camera, 800.0, 600.0, d.drape.as_ref().map(|d| &d.bands[..]).unwrap_or_default()));
            let meshes = d.meshes(rect, &m.camera, 800.0, 600.0);
            assert!(meshes.len() == 1 && meshes.iter().all(|mesh| !mesh.indices.is_empty() && mesh.is_valid()));
            // The band's middle is under the view's.
            let mid = meshes[0].vertices[meshes[0].vertices.len() / 2].pos;
            assert!((mid.x - 400.0).abs() < 1.0 && (mid.y - 300.0).abs() < 1.0, "{mid:?}");
        }
    }

    /// A marker beyond a wide view's top, which turning it a quarter brings in, stands on the
    /// turning map's drape.
    #[test]
    fn a_turning_flat_map_keeps_the_markers_it_turns_to() {
        use rustgis_engine::data::{Dataset, Geometry, GeometryType};
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let (w, h) = (1600.0, 400.0);
        let mut m = tilted(0.0);
        m.camera.bearing = 0.5;
        let north = rustgis_engine::geom::merc_to_lonlat(Coord::new(m.camera.center.x, m.camera.center.y + 700.0 * m.camera.res));
        let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(north)), vec![]);
        let id = m.add_dataset(ds);
        if let Some(l) = m.layer_mut(id) {
            l.selection = vec![0];
        }
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, w, h, 1.0, 0.0));
        let built = d.drape.as_ref().map(|d| d.built);
        let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(w as f32, h as f32));
        m.camera.bearing = 90.0;
        assert!(d.prepare(&ctx, &mut m, key, 0, w, h, 1.0, 0.1));
        assert_eq!(d.drape.as_ref().map(|d| d.built), built, "turned on the same drape");
        let mesh = d.markers(rect, &m.camera, fog()).expect("the marker turned into the view stands on it");
        assert!(mesh.is_valid() && !mesh.indices.is_empty());
    }

    #[test]
    fn a_drape_that_cant_be_drawn_off_the_ui_thread_is_drawn_on_it() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(60.0);
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.0));
        assert!(d.start_with(&ctx, key, || panic!("the renderer failed")));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while d.busy() {
            assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.1));
            assert!(std::time::Instant::now() < end, "the worker never ended");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(d.failed == Some(key));
        // A job for an earlier map change isn't waited for: the next prepare lets it go.
        let other = DrapeKey { rev: 7, sources: 0 };
        let (release, held) = std::sync::mpsc::channel::<()>();
        assert!(d.start_with(&ctx, other, move || {
            let _ = held.recv();
            panic!("stale")
        }));
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.15));
        assert!(!d.job.as_ref().is_some_and(|(k, _)| *k == other), "the stale job is let go");
        // Its worker still draws: none other starts beside it, the last drape stays meanwhile.
        d.failed = None;
        m.camera.pan_px(5_000.0, 0.0);
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.16));
        assert!(d.job.is_none() && d.let_go.is_some(), "one worker at a time");
        drop(release);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while d.let_go_running() {
            assert!(std::time::Instant::now() < end, "the let-go worker never ended");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        d.failed = Some(key);
        // Panning far away draws the next drape at once, covering the view.
        m.camera.pan_px(5_000.0, 0.0);
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.2));
        assert!(!d.busy());
        assert!(covers(&m.camera, 800.0, 600.0, d.drape.as_ref().map(|d| &d.bands[..]).unwrap_or_default()));
    }

    /// The frame a worker's panic wakes finds the job over (its map change marked failed): the
    /// worker closes the channel before it wakes the frame, so no wake-up is lost. (The callback
    /// holds the worker until the test has looked.)
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_frame_a_panic_wakes_finds_the_job_over() {
        let ctx = egui::Context::default();
        let (woken, wake) = std::sync::mpsc::channel::<()>();
        let (open, gate) = std::sync::mpsc::channel::<()>();
        let (woken, gate) = (std::sync::Mutex::new(woken), std::sync::Mutex::new(gate));
        ctx.set_request_repaint_callback(move |_| {
            if let Ok(w) = woken.lock() {
                let _ = w.send(());
            }
            if let Ok(g) = gate.lock() {
                let _ = g.recv_timeout(std::time::Duration::from_secs(30));
            }
        });
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(60.0);
        let mut d = TiltDrape::default();
        assert!(d.start_with(&ctx, key, || panic!("the renderer failed")));
        assert!(wake.recv_timeout(std::time::Duration::from_secs(30)).is_ok(), "never woken");
        assert!(d.take(&mut m).is_none());
        assert!(!d.busy() && d.failed == Some(key), "the woken frame found the job still running");
        drop(open);
    }

    /// A drape that can't be drawn on the UI thread either keeps the last one: the panic doesn't
    /// take the app down, and it isn't tried again until the map changes.
    #[test]
    fn a_drape_that_cant_be_drawn_at_all_keeps_the_last() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(60.0);
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.0));
        d.failed = Some(key);
        d.broken = Some(key);
        m.camera.pan_px(5_000.0, 0.0);
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.1), "the last drape stays");
        assert!(!d.busy());
        let next = DrapeKey { rev: 1, sources: 0 };
        assert!(d.prepare(&ctx, &mut m, next, 0, 800.0, 600.0, 1.0, 0.2));
        assert!(covers(&m.camera, 800.0, 600.0, d.drape.as_ref().map(|d| &d.bands[..]).unwrap_or_default()), "a change draws again");
    }

    /// An empty drape drawn off the UI thread is for where the view was: the one for where it is
    /// is drawn at once.
    #[test]
    fn an_empty_drape_from_the_worker_is_drawn_again_for_the_view() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(60.0);
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.0));
        let (copy, cam) = (m.clone(), m.camera);
        assert!(d.start_with(&ctx, key, move || (nothing(key, cam), copy)));
        wait(&mut d, &ctx, &mut m, key, 0.1);
        assert!(d.failed.is_none());
        assert!(covers(&m.camera, 800.0, 600.0, d.drape.as_ref().map(|d| &d.bands[..]).unwrap_or_default()));
    }

    /// A resting map whose worker found nothing to drape isn't drawn again, frame after frame,
    /// until the view moves.
    #[test]
    fn a_resting_map_with_nothing_to_drape_isnt_drawn_again() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(60.0);
        let mut d = TiltDrape::default();
        let (copy, cam) = (m.clone(), m.camera);
        assert!(d.start_with(&ctx, key, move || (nothing(key, cam), copy)));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while d.busy() {
            assert!(std::time::Instant::now() < end, "the worker never ended");
            std::thread::sleep(std::time::Duration::from_millis(5));
            d.warm(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.1);
        }
        assert!(d.drape.is_none());
        d.warm(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.2);
        assert!(!d.busy(), "the same view isn't drawn again");
        m.camera.pan_px(5_000.0, 0.0);
        d.warm(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.3);
        assert!(d.busy(), "a view moved on is");
        wait(&mut d, &ctx, &mut m, key, 0.4);
    }

    /// A window larger than a texture: the band, drawn coarser to fit, still covers the view at
    /// every bearing (its pixels rounded down, it isn't let fall short). At 0.75 pixels a point
    /// its 2048 pixels are 2730.7 points' worth, rounded down to 2730: at 3.5 times coarser, over
    /// two points of ground, past the band's spare.
    #[test]
    fn a_large_turning_view_stays_on_its_coarser_drape() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let (w, h, ppp) = (8000.0, 5000.0, 0.75);
        let mut m = tilted(0.0);
        m.camera.bearing = 0.5;
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, w, h, ppp, 0.0));
        let built = d.drape.as_ref().map(|d| d.built);
        for step in 0..=360 {
            m.camera.bearing = f64::from(step) * 0.25 + 0.5;
            assert!(d.prepare(&ctx, &mut m, key, 0, w, h, ppp, 0.01));
            assert_eq!(d.drape.as_ref().map(|d| d.built), built, "{}°", m.camera.bearing);
            assert!(covers(&m.camera, w, h, d.drape.as_ref().map(|d| &d.bands[..]).unwrap_or_default()), "{}°", m.camera.bearing);
        }
    }

    /// A worker's drape begun before the drape shown was drawn (on the UI thread) isn't shown over
    /// it.
    #[test]
    fn an_older_drape_from_the_worker_doesnt_replace_a_newer_one() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(60.0);
        let mut d = TiltDrape::default();
        assert!(d.prepare(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 1.0));
        assert_eq!(d.drape.as_ref().map(|d| d.built), Some(1.0));
        let mut copy = m.clone();
        let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera: m.camera, sky: true };
        assert!(d.start_with(&ctx, key, move || (draw(&mut copy, &view, 1024, key, 0, 0.0), copy)));
        wait(&mut d, &ctx, &mut m, key, 1.1);
        assert_eq!(d.drape.as_ref().map(|d| d.built), Some(1.0), "the newer drape stays");
    }

    /// An empty drape for a view the map has since left doesn't hold up the view it is at: the
    /// resting map is draped there.
    #[test]
    fn an_empty_drape_for_a_view_left_since_doesnt_hold_up_the_next() {
        let ctx = egui::Context::default();
        let key = DrapeKey { rev: 0, sources: 0 };
        let mut m = tilted(60.0);
        let mut d = TiltDrape::default();
        let (copy, cam) = (m.clone(), m.camera);
        assert!(d.start_with(&ctx, key, move || (nothing(key, cam), copy)));
        m.camera.pan_px(5_000.0, 0.0);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while d.drape.is_none() {
            assert!(std::time::Instant::now() < end, "the view moved to was never draped");
            std::thread::sleep(std::time::Duration::from_millis(5));
            d.warm(&ctx, &mut m, key, 0, 800.0, 600.0, 1.0, 0.1);
        }
    }

    /// A drape with nothing in it, as a worker hands back for a view over no ground.
    fn nothing(key: DrapeKey, camera: Camera) -> Drawn {
        Drawn { key, ppp: 1.0, camera, tiles: 0, built: 0.0, patches: Vec::new(), relief: None, shapes: Vec::new(), markers: Vec::new() }
    }

    /// Prepares `d` until the drape being drawn is shown.
    fn wait(d: &mut TiltDrape, ctx: &egui::Context, m: &mut Map, key: DrapeKey, time: f64) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while d.busy() {
            assert!(std::time::Instant::now() < end, "the drape never came");
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert!(d.prepare(ctx, m, key, 0, 800.0, 600.0, 1.0, time));
        }
    }
}
