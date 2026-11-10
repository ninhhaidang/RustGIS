//! The map's web tile layers, drawn over the basemap, each at its place among the map's layers
//! (see `Map::stack`).
//!
//! On the flat Web Mercator map tiles draw straight into the map. In any other projection, and
//! on the globe, they draw into a Web Mercator mosaic of the area in view, which is then warped
//! into place pixel by pixel.

use rustgis_geom::{Coord, Extent, MAX_LAT, lonlat_to_merc};
use rustgis_map::effects::Blend;
use rustgis_map::layout::TextAlign;
use rustgis_map::projection::Flat;
use rustgis_map::{Map, Rgba, TileKind, TileLayer};
use rustgis_tiles::{Frame, Kind, Spec};
use tiny_skia::{Pixmap, PixmapPaint, Transform};

use crate::gpu_scene::{DrawCmd, MeshTile, TileQuad};
use crate::raster::Place;
use crate::{PlacedIcon, PlacedLabel, View};

/// The largest side of the mosaic warped onto the globe or another projection.
const MOSAIC: f64 = 2048.0;

/// What drawing the tile layers found.
#[derive(Default)]
pub(crate) struct TileOutput {
    pub labels: Vec<(PlacedLabel, f64)>,
    pub attribution: Vec<String>,
    pub pending: bool,
    pub errors: Vec<String>,
    /// Raster layers read in place drawn from less than the view needs (a coarser level, tiles
    /// they couldn't read), by id: shown, with the error among `errors`; an export refuses them.
    pub partial: Vec<u32>,
    /// Raster layers read in place whose tiles are still on their way, by id.
    pub pending_in_place: Vec<u32>,
}

impl TileOutput {
    /// Adds what another draw of tile layers gave (those at another place in the drawing order).
    pub fn add(&mut self, o: TileOutput) {
        self.labels.extend(o.labels);
        for a in o.attribution {
            if !self.attribution.contains(&a) {
                self.attribution.push(a);
            }
        }
        self.pending |= o.pending;
        self.errors.extend(o.errors);
    }
}

/// A tile style's label as the map places it (in its logical points), with its priority:
/// sized, wrapped and anchored as MapLibre lays it out. `None` when it has nothing to show.
pub(crate) fn tile_label(l: rustgis_tiles::Label) -> Option<(PlacedLabel, f64)> {
    // Styles often label in two scripts ("Syria\nسوريا"): only the lines the font can
    // draw are kept, so a name never shows as boxes, and a label left with neither text
    // nor an icon is left out.
    let lines: Vec<&str> = l.text.lines().map(str::trim).filter(|s| !s.is_empty() && crate::text::covers(s, l.bold)).collect();
    if lines.is_empty() && l.icon.is_none() {
        return None;
    }
    // text-size is in CSS pixels, the map's logical points; a label's size is in type
    // points (its font is 1.33 × that many pixels tall).
    let size = l.size / 1.33;
    // Letter spacing in points, as a text graphic's.
    let spacing = l.letter_spacing * size;
    // Wrapped at spaces to text-max-width (in ems) as MapLibre wraps a point's label,
    // its own line breaks kept.
    let max_w = l.max_width * l.size;
    let text = if max_w > 0.0 {
        lines.iter().flat_map(|line| crate::text::wrap(line, size, l.bold, spacing, max_w)).collect::<Vec<_>>().join("\n")
    } else {
        lines.join(" ")
    };
    // The text sits against the point as the style anchors and offsets it (beside its
    // icon, for instance); without either it is centred on the point. Across, it is
    // placed by the edge it is anchored at, so each renderer lines it up with its own
    // measure of the width.
    let (th, em) = (crate::text::block_height(&text, size), l.size);
    let x = l.x + l.offset[0] * em;
    let y = l.y + l.anchor[1] * th + l.offset[1] * em;
    let icon =
        l.icon.map(|i| PlacedIcon { image: i.image, x: l.x + i.dx, y: l.y + i.dy, w: i.width, h: i.height, angle: i.angle, alpha: 1.0 });
    let line_align = match l.justify {
        j if j < -0.25 => TextAlign::Left,
        j if j > 0.25 => TextAlign::Right,
        _ => TextAlign::Center,
    };
    Some((
        PlacedLabel {
            text,
            x,
            y,
            size,
            color: Rgba(l.color),
            halo: l.halo.map(Rgba),
            halo_width: None,
            bold: l.bold,
            icon,
            align: -l.anchor[0],
            framed: false,
            italic: l.italic,
            spacing,
            font: None,
            line_align,
            angle: l.angle,
            collide: l.collide,
            glyphs: Vec::new(),
            background: None,
        },
        l.priority,
    ))
}

/// A colour with its alpha times `k`.
fn faded(c: Rgba, k: f32) -> Rgba {
    Rgba([c.0[0], c.0[1], c.0[2], (f32::from(c.0[3]) * k).round() as u8])
}

/// A tile layer's label at the layer's opacity: its text, halo and icon fainter by it.
fn fade(mut l: PlacedLabel, opacity: f32) -> PlacedLabel {
    if !opacity.is_finite() || opacity >= 1.0 {
        return l;
    }
    let k = opacity.max(0.0);
    l.color = faded(l.color, k);
    l.halo = l.halo.map(|c| faded(c, k));
    if let Some(i) = &mut l.icon {
        i.alpha *= k;
    }
    l
}

/// How the tiles crate sees a tile layer.
pub fn spec(t: &TileLayer) -> Spec {
    Spec {
        kind: match t.kind {
            TileKind::Raster => Kind::Raster,
            TileKind::Vector => Kind::Vector,
            TileKind::Style => Kind::Style,
            TileKind::PmTiles => Kind::PmTiles,
            TileKind::MbTiles => Kind::MbTiles,
            TileKind::Cog => Kind::Cog,
        },
        url: t.url.clone(),
        subdomains: t.subdomains.clone(),
        min_zoom: t.min_zoom,
        max_zoom: t.max_zoom,
        tile_size: t.tile_size,
        tms: t.tms,
        attribution: t.attribution.clone(),
        layers: t.layers.clone(),
        style_layers: t.style_layers.clone(),
        hidden_layers: t.hidden_layers.clone(),
    }
}

/// A vector PMTiles or MBTiles archive's source layers (`vector_layers`), each once in its
/// order, as [`source_layers::of_metadata`](rustgis_data::source_layers::of_metadata) keeps
/// them. `None` for other tile layers, and while the archive's header is on its way.
pub fn source_layers(t: &TileLayer) -> Option<Vec<String>> {
    let kind = match t.kind {
        TileKind::PmTiles => Kind::PmTiles,
        TileKind::MbTiles => Kind::MbTiles,
        _ => return None,
    };
    rustgis_tiles::source::source_layers(kind, &t.url)
}

/// Where a MapLibre style layer's root properties place the map; `Ok(None)` while the style is
/// still loading (asking starts loading it). Other tile layers have no view of their own.
pub fn style_view(t: &TileLayer) -> Result<Option<rustgis_tiles::style::StyleView>, String> {
    if t.kind != TileKind::Style {
        return Ok(Some(Default::default()));
    }
    match rustgis_tiles::source::resolve(&spec(t)) {
        rustgis_tiles::source::Resolve::Ready(r) => Ok(Some(r.style.view)),
        rustgis_tiles::source::Resolve::Pending => Ok(None),
        rustgis_tiles::source::Resolve::Failed(e) => Err(e),
    }
}

/// A tile last drawn as a mesh: its mesh held weakly (the mesh cache decides how long it
/// stays), where it is and the square it was drawn in, and its level.
type LastTile = (std::sync::Weak<crate::tile_mesh::TileMesh>, [f64; 2], [f32; 4], u8);

/// The tiles of each style last drawn as meshes (by the resolved style's address), which stand in
/// for tiles whose meshes are still to be made. Any map's do: a mesh is the same wherever it is
/// drawn, and only its square's part of it is.
type Last = std::collections::HashMap<usize, (std::sync::Weak<rustgis_tiles::source::Resolved>, Vec<LastTile>)>;
static LAST: std::sync::OnceLock<std::sync::Mutex<Last>> = std::sync::OnceLock::new();

/// The meshes of `v`'s tiles, and whether a sprite they use is on its way. A map
/// drawn for the screen at a pace (see `rustgis_tiles::Pace`) makes those missing a frame's worth
/// at a time, nearest `centre` first, the tiles drawn last standing in for the rest meanwhile,
/// clipped to their squares.
fn mesh_tiles(v: &rustgis_tiles::shapes::VectorTiles, centre: [f64; 2]) -> (Vec<MeshTile>, bool) {
    let pace = rustgis_tiles::pace();
    let (meshes, mut waiting) = crate::tile_mesh::meshes(&v.style, &v.source, &v.tiles, v.level, pace.map(|(_, until)| (until, centre)));
    let lock = || LAST.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let key = std::sync::Arc::as_ptr(&v.style) as usize;
    let last: Vec<MeshTile> = lock()
        .get(&key)
        // The same style still, not a new one at a freed address.
        .filter(|(s, _)| s.upgrade().is_some_and(|s| std::sync::Arc::ptr_eq(&s, &v.style)))
        .map(|(_, t)| {
            t.iter()
                .filter_map(|(m, origin, clip, level)| Some(MeshTile { mesh: m.upgrade()?, origin: *origin, clip: *clip, level: *level }))
                .collect()
        })
        .unwrap_or_default();
    let (mut stood_in, mut tiles) = (Vec::new(), Vec::new());
    for (d, m) in v.tiles.iter().zip(meshes) {
        let o = [d.at.min_x, d.at.max_y];
        match m {
            Some(mesh) => {
                waiting |= mesh.waiting;
                if !mesh.indices.is_empty() {
                    let c = &d.clip;
                    let clip = [(c.min_x - o[0]) as f32, (c.min_y - o[1]) as f32, (c.max_x - o[0]) as f32, (c.max_y - o[1]) as f32];
                    tiles.push(MeshTile { mesh, origin: o, clip, level: v.level });
                }
            }
            None if matches!(&*d.payload, rustgis_tiles::cache::Payload::Vector(_)) => {
                // Still to be made: what was drawn over its square last stands in.
                let before = stood_in.len();
                let s = &d.clip;
                for t in &last {
                    let c = [
                        (t.origin[0] + f64::from(t.clip[0])).max(s.min_x),
                        (t.origin[1] + f64::from(t.clip[1])).max(s.min_y),
                        (t.origin[0] + f64::from(t.clip[2])).min(s.max_x),
                        (t.origin[1] + f64::from(t.clip[3])).min(s.max_y),
                    ];
                    if c[0] < c[2] && c[1] < c[3] {
                        let clip = [
                            (c[0] - t.origin[0]) as f32,
                            (c[1] - t.origin[1]) as f32,
                            (c[2] - t.origin[0]) as f32,
                            (c[3] - t.origin[1]) as f32,
                        ];
                        stood_in.push(MeshTile { clip, ..t.clone() });
                    }
                }
                let covered = stood_in.len() > before;
                rustgis_tiles::unfinished(match pace {
                    Some((rustgis_tiles::Pace::Moving, _)) if covered => rustgis_tiles::Unfinished::Later,
                    _ => rustgis_tiles::Unfinished::Next,
                });
            }
            None => {}
        }
    }
    // Under the tiles drawn now.
    stood_in.extend(tiles);
    let mut l = lock();
    l.retain(|_, (s, _)| s.strong_count() > 0);
    let kept = stood_in.iter().map(|t| (std::sync::Arc::downgrade(&t.mesh), t.origin, t.clip, t.level)).collect();
    l.insert(key, (std::sync::Arc::downgrade(&v.style), kept));
    (stood_in, waiting)
}

/// Where a GPU scene's tile layers go: raster tile layers as [`DrawCmd::Tiles`] in `cmds`,
/// anything else drawn into the accumulator pixmap (`dirty` then set), which is flushed to `cmds`
/// first so the layers keep their order.
pub(crate) struct Gpu<'a> {
    pub cmds: &'a mut Vec<DrawCmd>,
    pub dirty: &'a mut bool,
    /// Vector tile styles may be drawn as meshes ([`DrawCmd::TileMeshes`]).
    pub meshes: bool,
    /// Raster tile layers are left for the GPU map to place for each frame ([`DrawCmd::LiveTiles`]).
    pub live: bool,
}

/// How far past the view a live raster tile layer's tiles are fetched, a share of its size.
const LIVE_FETCH_MARGIN: f64 = 0.25;

/// A live raster tile layer's tiles for one frame (see [`live`]).
pub struct Live {
    /// Bottom first, from the camera's centre.
    pub quads: Vec<TileQuad>,
    /// Tiles in view are still on their way.
    pub pending: bool,
    /// What resolving or placing these tiles left out, reported just as for a full drawing.
    pub error: Option<String>,
}

/// A live raster tile layer's tiles for `view` ([`DrawCmd::LiveTiles`]): over the view, fetching
/// those missing, and out to the margin a moving camera reaches, from the cache only, with
/// parents and children standing in for tiles on their way, placed from the camera's centre.
/// `None` when the layer draws anything but raster tiles (its style changed since the scene was
/// drawn). Cheap: cache lookups.
pub fn live(spec: &Spec, opacity: f32, view: &View) -> Option<Live> {
    let archive = matches!(spec.kind, Kind::PmTiles | Kind::MbTiles).then_some(spec.url.as_str());
    let revision = rustgis_tiles::cache::archive_revision(archive);
    let output = live_inner(spec, opacity, view);
    (revision == rustgis_tiles::cache::archive_revision(archive)).then_some(output).flatten()
}

fn live_inner(spec: &Spec, opacity: f32, view: &View) -> Option<Live> {
    let res = view.camera.res;
    let ppp = view.pixels_per_point as f32;
    let frame = Frame {
        left: view.camera.center.x - view.width / 2.0 * res,
        top: view.camera.center.y + view.height / 2.0 * res,
        m_per_px: res / view.pixels_per_point.max(0.01),
        ppp,
    };
    // Fetched a quarter of the view past each side, so a pan finds its tiles waiting, and held
    // out to the reach of a moving camera.
    let fetch = crate::cull_extent(view, LIVE_FETCH_MARGIN);
    let reach = crate::cull_extent(view, crate::GPU_CULL_MARGIN);
    let p = rustgis_tiles::place_raster(spec, opacity, &frame, &fetch, &reach)?;
    // The view's tiles three levels up (a tile or two) are fetched too: a coarse picture of
    // anywhere the camera goes next stands in until its own tiles arrive, so the map shows no
    // holes while they load (as the root tile is kept in forge3d's tile streaming).
    let coarse = Frame { m_per_px: frame.m_per_px * 8.0, ..frame };
    let _ = rustgis_tiles::place_raster(spec, opacity, &coarse, &fetch, &fetch);
    let origin = [view.camera.center.x, view.camera.center.y];
    Some(Live { quads: p.tiles.iter().map(|q| quad(q, origin)).collect(), pending: p.pending, error: p.error })
}

/// Draws the map's visible tile layers at `which` (indices, top first), bottom first. With
/// `gpu`, on the flat Web Mercator map, raster tile layers are placed as tiles for the GPU
/// instead.
pub(crate) fn draw(
    pm: &mut Pixmap,
    map: &Map,
    which: &[usize],
    view: &View,
    place: Place,
    labels: bool,
    mut gpu: Option<Gpu<'_>>,
) -> TileOutput {
    let mut out = TileOutput::default();
    let layers: Vec<&TileLayer> = which.iter().rev().filter_map(|i| map.tiles.get(*i)).filter(|t| t.draws() && t.opacity > 0.0).collect();
    if layers.is_empty() {
        return out;
    }
    let ppp = view.pixels_per_point as f32;
    if let Place::Flat(Flat::Mercator, _) = place {
        let res = view.camera.res;
        let frame = Frame {
            left: view.camera.center.x - view.width / 2.0 * res,
            top: view.camera.center.y + view.height / 2.0 * res,
            m_per_px: res / view.pixels_per_point.max(0.01),
            ppp,
        };
        let mut scratch = None;
        // The tiles fetched (the view and the CPU path's margin), and those held for a moving
        // camera (the GPU meshes' margin), from the cache only.
        let fetch = crate::cull_extent(view, crate::CULL_MARGIN);
        let reach = crate::cull_extent(view, crate::GPU_CULL_MARGIN);
        let origin = [view.camera.center.x, view.camera.center.y];
        let north_up = view.camera.bearing == 0.0 && view.camera.pitch == 0.0;
        for t in layers {
            // One picture of the view, where the layer is asked for so (#1162).
            if view.camera.pitch == 0.0
                && let Some(req) = crate::single_image::of(t, 3857)
                && let Some((pending, error)) = crate::single_image::draw(pm, &req, t, view, &Flat::Mercator)
            {
                if let Some(g) = gpu.as_mut() {
                    *g.dirty = true;
                }
                let attribution = if t.attribution.trim().is_empty() { Vec::new() } else { vec![t.attribution.clone()] };
                collect(rustgis_tiles::Drawn { labels: Vec::new(), attribution, pending, error }, t, &mut out);
                continue;
            }
            // A vector tile style as meshes, made once a tile and level and drawn at any zoom
            // (see `tile_mesh`); a see-through or blended layer, as pictures.
            if let Some(g) = gpu.as_mut()
                && north_up
                && g.meshes
                && t.blend == Blend::Normal
                && t.opacity >= 0.999
                && let Some(v) =
                    rustgis_tiles::shapes::vector_tiles(&spec(t), &frame, &fetch, Some(&reach), labels.then_some((pm.width(), pm.height())))
                && crate::flush_acc(pm, g.cmds, g.dirty).is_some()
            {
                let (tiles, waiting) = mesh_tiles(&v, origin);
                if !tiles.is_empty() {
                    g.cmds.push(DrawCmd::TileMeshes { tiles });
                }
                let d =
                    rustgis_tiles::Drawn { labels: v.labels, attribution: v.attribution, pending: v.pending || waiting, error: v.error };
                collect(d, t, &mut out);
                continue;
            }
            // A raster tile layer placed for every frame by the GPU map: the scene holds the layer.
            // Placed here too for what the scene reports (credits, tiles on their way, errors),
            // and to start fetching them.
            let s = spec(t);
            if let Some(g) = gpu.as_mut()
                && g.live
                && north_up
                && t.blend == Blend::Normal
                && let Some(p) = rustgis_tiles::place_raster(&s, t.opacity, &frame, &fetch, &reach)
                && crate::flush_acc(pm, g.cmds, g.dirty).is_some()
            {
                let archive_revision = if matches!(s.kind, Kind::PmTiles | Kind::MbTiles) {
                    rustgis_tiles::cache::archive_revision([s.url.as_str()])
                } else {
                    0
                };
                g.cmds.push(DrawCmd::LiveTiles { name: t.name.clone(), spec: s, opacity: t.opacity, archive_revision });
                let d = rustgis_tiles::Drawn { labels: Vec::new(), attribution: p.attribution, pending: p.pending, error: p.error };
                collect(d, t, &mut out);
                continue;
            }
            if let Some(g) = gpu.as_mut()
                && t.blend == Blend::Normal
                && let Some(p) =
                    rustgis_tiles::place(&spec(t), t.opacity, &frame, &fetch, &reach, labels.then_some((pm.width(), pm.height())))
            {
                // What the accumulator holds goes first, under the tiles; with no room for a new
                // accumulator the layer is drawn into this one instead.
                let flushed = p.tiles.is_empty() || crate::flush_acc(pm, g.cmds, g.dirty).is_some();
                if flushed {
                    if !p.tiles.is_empty() {
                        g.cmds.push(DrawCmd::Tiles { origin, tiles: p.tiles.iter().map(|q| quad(q, origin)).collect() });
                    }
                    let d = rustgis_tiles::Drawn { labels: p.labels, attribution: p.attribution, pending: p.pending, error: p.error };
                    collect(d, t, &mut out);
                    continue;
                }
            }
            let d = draw_layer(pm, t, &frame, labels, &mut scratch);
            if let Some(g) = gpu.as_mut() {
                *g.dirty = true;
            }
            collect(d, t, &mut out);
        }
        return out;
    }
    if let Some(g) = gpu {
        *g.dirty = true;
    }
    // Elsewhere: a layer offered in the map's own system drawn in it as its service gives it
    // (#769); runs of the others, a mosaic in Web Mercator warped into place. (A tilted map's
    // pictures aren't flat on screen: it warps them all, pixel by pixel.)
    let native_to = match place {
        Place::Flat(flat @ Flat::Custom(_), code) if view.camera.pitch == 0.0 => {
            rustgis_data::ogc::warp::epsg_of(code).and_then(|c| u16::try_from(c).ok()).map(|e| (flat, e))
        }
        _ => None,
    };
    // A layer blended with what is beneath it (multiply…) is drawn in the mosaic with the tile
    // layers beneath it: those are mosaicked too, not drawn natively.
    let blended = layers.iter().rposition(|t| t.blend != Blend::Normal);
    let mut run: Vec<&TileLayer> = Vec::new();
    for (i, t) in layers.into_iter().enumerate() {
        // One picture of the view, asked for in the map's system (#1162).
        if let Some((flat, epsg)) = native_to.filter(|_| blended.is_none_or(|b| i > b))
            && let Some(req) = crate::single_image::of(t, epsg)
        {
            // Those beneath first.
            mosaic(pm, &run, view, place, &mut out);
            run.clear();
            if let Some((pending, error)) = crate::single_image::draw(pm, &req, t, view, flat) {
                let attribution = if t.attribution.trim().is_empty() { Vec::new() } else { vec![t.attribution.clone()] };
                collect(rustgis_tiles::Drawn { labels: Vec::new(), attribution, pending, error }, t, &mut out);
                continue;
            }
        }
        let native = native_to
            .filter(|_| blended.is_none_or(|b| i > b))
            .and_then(|(flat, epsg)| crate::native_tiles::of(t, epsg).map(|n| (flat, n)));
        let Some((flat, n)) = native else {
            run.push(t);
            continue;
        };
        mosaic(pm, &run, view, place, &mut out);
        run.clear();
        let (pending, error) = crate::native_tiles::draw(pm, &n, t, view, flat);
        let attribution = if t.attribution.trim().is_empty() { Vec::new() } else { vec![t.attribution.clone()] };
        collect(rustgis_tiles::Drawn { labels: Vec::new(), attribution, pending, error }, t, &mut out);
    }
    mosaic(pm, &run, view, place, &mut out);
    out
}

/// What a tile layer drew, into what the map's tile layers drew: tiles on their way, credits,
/// errors and labels.
fn collect(d: rustgis_tiles::Drawn, t: &TileLayer, out: &mut TileOutput) {
    let name = &t.name;
    out.pending |= d.pending;
    for a in d.attribution {
        if !out.attribution.contains(&a) {
            out.attribution.push(a);
        }
    }
    if let Some(e) = d.error {
        out.errors.push(format!("{name}: {e}"));
    }
    // The layer's opacity fades its text and icons as it fades the rest of what it draws (#1358).
    out.labels.extend(d.labels.into_iter().filter_map(tile_label).map(|(l, p)| (fade(l, t.opacity), p)));
}

/// Tile layers (bottom first) on a map in another projection than Web Mercator, or the globe:
/// drawn together into a mosaic in Web Mercator, warped into place over `pm`.
fn mosaic(pm: &mut Pixmap, layers: &[&TileLayer], view: &View, place: Place, out: &mut TileOutput) {
    if layers.is_empty() {
        return;
    }
    let ppp = view.pixels_per_point as f32;
    let (w, h) = (pm.width(), pm.height());
    let k = view.pixels_per_point.max(0.01);
    let to_lonlat = |dx: f64, dy: f64| -> Option<Coord> {
        let (sx, sy) = (dx / k, dy / k);
        let c = match place {
            Place::Globe(g) => g.inverse(sx, sy)?,
            Place::Flat(flat, _) => flat.inverse(view.camera.to_map(sx, sy, view.width, view.height))?,
        };
        (c.is_finite() && c.y.abs() <= 90.0).then_some(c)
    };
    // Longitudes are unwrapped around the centre's, so a view across the antimeridian is one
    // stretch of an unwrapped world (the tiles past 180° are world copies) rather than two ends
    // of it with the strip between the last points sampled on either side left out.
    let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let lon0 = to_lonlat(cx, cy).map_or(0.0, |c| c.x);
    let to_merc = |c: Coord| {
        let lon = c.x + 360.0 * ((lon0 - c.x) / 360.0).round();
        lonlat_to_merc(Coord::new(lon, c.y.clamp(-MAX_LAT, MAX_LAT)))
    };
    // The area in view, from a grid of points, and the detail needed at its centre.
    let n = 24;
    let grid: Vec<Vec<Option<Coord>>> = (0..=n)
        .map(|j| {
            (0..=n)
                .map(|i| to_lonlat(f64::from(w) * f64::from(i) / f64::from(n), f64::from(h) * f64::from(j) / f64::from(n)).map(to_merc))
                .collect()
        })
        .collect();
    let mut e = Extent::EMPTY;
    // The most a grid cell spans: the area in view reaches up to that past its outermost points
    // (the limb, the Web Mercator latitude limit, the meridian behind a pole).
    let (mut step_x, mut step_y) = (0.0f64, 0.0f64);
    let mut step = |a: Option<&Option<Coord>>, b: &Coord| {
        if let Some(Some(a)) = a {
            step_x = step_x.max((a.x - b.x).abs());
            step_y = step_y.max((a.y - b.y).abs());
        }
    };
    for (j, row) in grid.iter().enumerate() {
        for (i, c) in row.iter().enumerate() {
            let Some(c) = c else { continue };
            e.expand(*c);
            step(row.get(i + 1), c);
            step(grid.get(j + 1).and_then(|r| r.get(i)), c);
        }
    }
    if e.is_empty() {
        return;
    }
    let x0 = lonlat_to_merc(Coord::new(lon0, 0.0)).x;
    // A pole in sight brings the latitudes up to it into view, whether or not the grid reached
    // them, and every longitude when it is in sight on the far side too (on the globe a pole is
    // a point; a world projection draws it as a line, which can run out of the view).
    let (vw, vh) = (f64::from(w) / k, f64::from(h) / k);
    let in_view = |lon: f64, lat: f64| {
        let pole = Coord::new(lon, lat);
        let at = match place {
            Place::Globe(g) => g.forward(pole),
            Place::Flat(flat, _) => flat.forward_checked(pole).map(|m| view.camera.to_screen(m, view.width, view.height)),
        };
        at.is_some_and(|(x, y)| x >= 0.0 && y >= 0.0 && x <= vw && y <= vh)
    };
    let (north, south) = (in_view(lon0, 90.0), in_view(lon0, -90.0));
    let all_lons = (north && in_view(lon0 + 180.0, 90.0)) || (south && in_view(lon0 + 180.0, -90.0));
    let min_y = if south { -HALF } else { (e.min_y - step_y).max(-HALF) };
    let max_y = if north { HALF } else { (e.max_y + step_y).min(HALF) };
    let e = if all_lons || e.width() > HALF {
        // Over HALF the world across: every longitude.
        Extent::new(x0 - HALF, min_y, x0 + HALF, max_y)
    } else {
        Extent::new((e.min_x - step_x).max(x0 - HALF), min_y, (e.max_x + step_x).min(x0 + HALF), max_y)
    };
    let detail = match (to_lonlat(cx, cy), to_lonlat(cx + 4.0, cy)) {
        (Some(a), Some(b)) => (to_merc(a).x - to_merc(b).x).hypot(to_merc(a).y - to_merc(b).y) / 4.0,
        _ => 0.0,
    };
    let m_per_px = detail.max(e.width() / MOSAIC).max(e.height() / MOSAIC).max(0.01);
    let (mw, mh) = ((e.width() / m_per_px).ceil().clamp(1.0, MOSAIC) as u32, (e.height() / m_per_px).ceil().clamp(1.0, MOSAIC) as u32);
    let Some(mut mosaic) = Pixmap::new(mw, mh) else { return };
    // Pixels per point in the mosaic: line widths and the style's zoom then match the view's
    // centre once the mosaic is warped there.
    let mosaic_ppp = if detail > 0.0 { (detail * k / m_per_px) as f32 } else { ppp };
    let frame = Frame { left: e.min_x, top: e.max_y, m_per_px, ppp: mosaic_ppp.clamp(0.05, 8.0) };
    let mut scratch = None;
    for t in layers {
        let d = draw_layer(&mut mosaic, t, &frame, false, &mut scratch);
        collect(d, t, out);
    }
    warp(pm, &mosaic, &frame, &to_lonlat, &to_merc);
}

/// A placed tile as the GPU draws it: its clipped square about `origin`, and the part of its
/// picture in that square.
fn quad(p: &rustgis_tiles::Placement, origin: [f64; 2]) -> TileQuad {
    let (w, h) = (p.at.width().max(1e-9), p.at.height().max(1e-9));
    let c = &p.clip;
    TileQuad {
        picture: p.picture.clone(),
        rect: [(c.min_x - origin[0]) as f32, (c.min_y - origin[1]) as f32, (c.max_x - origin[0]) as f32, (c.max_y - origin[1]) as f32],
        uv: [
            ((c.min_x - p.at.min_x) / w) as f32,
            ((p.at.max_y - c.max_y) / h) as f32,
            ((c.max_x - p.at.min_x) / w) as f32,
            ((p.at.max_y - c.min_y) / h) as f32,
        ],
        opacity: p.opacity,
    }
}

/// Draws one tile layer into `pm`: straight in, or (blended) on a layer of its own composited
/// with its blend mode over what's beneath. `scratch` holds that layer between calls, so the
/// blended layers of one draw share one allocation.
fn draw_layer(pm: &mut Pixmap, t: &TileLayer, frame: &Frame, labels: bool, scratch: &mut Option<Pixmap>) -> rustgis_tiles::Drawn {
    if t.blend == Blend::Normal {
        return rustgis_tiles::draw(pm, &spec(t), t.opacity, frame, labels);
    }
    let fits = scratch.as_ref().is_some_and(|s| s.width() == pm.width() && s.height() == pm.height());
    if fits {
        if let Some(s) = scratch.as_mut() {
            s.fill(tiny_skia::Color::TRANSPARENT);
        }
    } else {
        *scratch = Pixmap::new(pm.width(), pm.height());
    }
    let Some(own) = scratch.as_mut() else {
        log::debug!("no room to blend tile layer '{}' ({}x{}): drawing it unblended", t.name, pm.width(), pm.height());
        return rustgis_tiles::draw(pm, &spec(t), t.opacity, frame, labels);
    };
    let d = rustgis_tiles::draw(own, &spec(t), t.opacity, frame, labels);
    let paint = PixmapPaint { blend_mode: crate::blend_mode(t.blend), ..PixmapPaint::default() };
    pm.draw_pixmap(0, 0, own.as_ref(), &paint, Transform::identity(), None);
    d
}

/// Half the width (and height) of the Web Mercator world, metres.
const HALF: f64 = std::f64::consts::PI * rustgis_geom::MERCATOR_R;

/// The side of the cells the warp works out exactly at their corners and interpolates inside.
const WARP_CELL: usize = 8;

/// How far (mosaic pixels) the interpolated middle of a cell may be from its exact place before
/// the cell is worked out pixel by pixel.
const WARP_TOLERANCE: f64 = 0.25;

/// Draws the mosaic over `pm`: each pixel takes the mosaic pixel under its longitude/latitude.
///
/// The projections are worked out at the corners of [`WARP_CELL`]-pixel cells and interpolated
/// inside them, where the mapping is smooth (checked at each cell's middle): every pixel of the
/// globe took an inverse projection and a Web Mercator one, millions a frame, and the browser
/// draws the globe on its one thread. Cells at the globe's edge or across the antimeridian, where
/// it isn't smooth, are worked out pixel by pixel.
fn warp(pm: &mut Pixmap, mosaic: &Pixmap, frame: &Frame, to_lonlat: &dyn Fn(f64, f64) -> Option<Coord>, to_merc: &dyn Fn(Coord) -> Coord) {
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    let (mw, mh) = (mosaic.width() as usize, mosaic.height() as usize);
    // The latitudes past the Web Mercator limit, clamped to it, land a hair outside the mosaic
    // (the poles' caps), and its last row can hang past the world's south edge, where no tile
    // draws: the row above it is the edge then.
    let (fw, fh) = (mw as f64, (mh as f64).min(((frame.top + HALF) / frame.m_per_px).floor().max(1.0)));
    let src = mosaic.data();
    let dst = pm.data_mut();
    // The mosaic position (pixels) under a pixel's centre.
    let at = |x: f64, y: f64| -> Option<(f64, f64)> {
        let m = to_merc(to_lonlat(x + 0.5, y + 0.5)?);
        Some(((m.x - frame.left) / frame.m_per_px, (frame.top - m.y) / frame.m_per_px))
    };
    let mut put = |x: usize, y: usize, (mx, my): (f64, f64)| {
        // Within a pixel of the mosaic's edge takes the edge (`fw`, `fh`).
        if !(mx > -1.0 && my > -1.0 && mx < fw + 1.0 && my < fh + 1.0) {
            return;
        }
        let (mx, my) = (mx.max(0.0).min(fw - 1.0) as usize, my.max(0.0).min(fh - 1.0) as usize);
        let si = (my * mw + mx) * 4;
        let di = (y * w + x) * 4;
        let (Some(s), Some(d)) = (src.get(si..si + 4), dst.get_mut(di..di + 4)) else { return };
        // Source-over, premultiplied.
        let a = u32::from(s[3]);
        if a == 0 {
            return;
        }
        for i in 0..4 {
            d[i] = (u32::from(s[i]) + u32::from(d[i]) * (255 - a) / 255).min(255) as u8;
        }
    };
    let n = WARP_CELL;
    let (cols, rows) = (w.div_ceil(n), h.div_ceil(n));
    // The corners' positions, a row of them at a time.
    let corners = |j: usize| -> Vec<Option<(f64, f64)>> { (0..=cols).map(|i| at((i * n) as f64, (j * n) as f64)).collect() };
    let mut above = corners(0);
    for j in 0..rows {
        let below = corners(j + 1);
        let (y0, y1) = (j * n, ((j + 1) * n).min(h));
        for i in 0..cols {
            let (x0, x1) = (i * n, ((i + 1) * n).min(w));
            let quad = match (above.get(i), above.get(i + 1), below.get(i), below.get(i + 1)) {
                (Some(&Some(a)), Some(&Some(b)), Some(&Some(c)), Some(&Some(d))) => Some((a, b, c, d)),
                _ => None,
            };
            // Bilinear between the corners, `t` across and `u` down the cell.
            let lerp = |(a, b, c, d): ((f64, f64), (f64, f64), (f64, f64), (f64, f64)), t: f64, u: f64| {
                let top = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                let bottom = (c.0 + (d.0 - c.0) * t, c.1 + (d.1 - c.1) * t);
                (top.0 + (bottom.0 - top.0) * u, top.1 + (bottom.1 - top.1) * u)
            };
            let smooth = quad.filter(|&q| {
                let half = n as f64 / 2.0;
                at((x0 as f64) + half, (y0 as f64) + half).is_some_and(|e| {
                    let g = lerp(q, 0.5, 0.5);
                    (e.0 - g.0).abs() <= WARP_TOLERANCE && (e.1 - g.1).abs() <= WARP_TOLERANCE
                })
            });
            for y in y0..y1 {
                for x in x0..x1 {
                    let m = match smooth {
                        Some(q) => Some(lerp(q, (x - x0) as f64 / n as f64, (y - y0) as f64 / n as f64)),
                        None => at(x as f64, y as f64),
                    };
                    if let Some(m) = m {
                        put(x, y, m);
                    }
                }
            }
        }
        above = below;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use rustgis_geom::{Coord, lonlat_to_merc};
    use rustgis_map::{Camera, Map, Projection, TileKind, TileLayer};

    use crate::View;

    /// A tile layer's opacity fades its labels' text, halo and icon with the rest of the layer
    /// (#1358), and leaves them as they are at full opacity.
    #[test]
    fn a_tile_layers_opacity_fades_its_labels_and_icons() {
        use super::{PlacedIcon, PlacedLabel, Rgba, fade};
        let icon = PlacedIcon {
            image: std::sync::Arc::new(tiny_skia::Pixmap::new(2, 2).unwrap()),
            x: 5.0,
            y: 5.0,
            w: 2.0,
            h: 2.0,
            angle: None,
            alpha: 1.0,
        };
        let label = PlacedLabel {
            color: Rgba([10, 20, 30, 255]),
            halo: Some(Rgba([255, 255, 255, 200])),
            icon: Some(icon),
            ..crate::tests::label("Hoek", 5.0, None, 0.0)
        };
        assert_eq!(fade(label.clone(), 1.0), label);
        let half = fade(label.clone(), 0.5);
        assert_eq!(half.color, Rgba([10, 20, 30, 128]));
        assert_eq!(half.halo, Some(Rgba([255, 255, 255, 100])));
        assert_eq!(half.icon.as_ref().map(|i| i.alpha), Some(0.5));
        // Only the opacity changes: where it sits and what it says.
        assert_eq!((half.x, half.y, &half.text), (label.x, label.y, &label.text));
        // A fade of a fade multiplies; junk opacity never panics.
        assert_eq!(fade(half, 0.5).icon.as_ref().map(|i| i.alpha), Some(0.25));
        assert_eq!(fade(label.clone(), -3.0).color.0[3], 0);
        assert_eq!(fade(label.clone(), f32::NAN), label);
    }

    /// `collect` applies the layer's opacity to what the layer drew (the one place every way of
    /// drawing a tile layer passes through), not only `fade` on its own.
    #[test]
    fn collecting_a_tile_layers_labels_applies_its_opacity() {
        let label = || rustgis_tiles::Label {
            text: "Hoek".into(),
            x: 20.0,
            y: 20.0,
            size: 12.0,
            color: [10, 20, 30, 255],
            halo: Some([255, 255, 255, 200]),
            bold: false,
            italic: false,
            letter_spacing: 0.0,
            max_width: 0.0,
            justify: 0.0,
            priority: 0.0,
            anchor: [0.0, 0.0],
            offset: [0.0, 0.0],
            angle: None,
            icon: Some(rustgis_tiles::LabelIcon {
                image: std::sync::Arc::new(tiny_skia::Pixmap::new(2, 2).unwrap()),
                width: 8.0,
                height: 8.0,
                dx: 0.0,
                dy: 0.0,
                angle: None,
            }),
            collide: Default::default(),
        };
        let collected = |opacity: f32| {
            let mut t = TileLayer::new("Seamap", TileKind::Style, "https://tiles.test/style.json");
            t.opacity = opacity;
            let mut out = super::TileOutput::default();
            let drawn = rustgis_tiles::Drawn { labels: vec![label()], ..Default::default() };
            super::collect(drawn, &t, &mut out);
            assert_eq!(out.labels.len(), 1);
            out.labels.remove(0).0
        };
        let full = collected(1.0);
        assert_eq!((full.color.0[3], full.halo.map(|h| h.0[3]), full.icon.as_ref().map(|i| i.alpha)), (255, Some(200), Some(1.0)));
        let faint = collected(0.5);
        assert_eq!((faint.color.0[3], faint.halo.map(|h| h.0[3]), faint.icon.as_ref().map(|i| i.alpha)), (128, Some(100), Some(0.5)));
    }

    /// The warp interpolating across cells puts every pixel where working it out pixel by pixel
    /// does: on a globe zoomed in, on a whole one with its edge in view, across the antimeridian
    /// (where the mosaic's longitudes jump from one side to the other), and with a pole in sight
    /// (where latitudes are clamped to Web Mercator's limit).
    #[test]
    fn the_warp_matches_the_exact_projection() {
        use rustgis_geom::MERCATOR_R;
        let half = std::f64::consts::PI * MERCATOR_R;
        // A whole-world mosaic whose colour is its position: red across, green down.
        let side = 1024u32;
        let mut mosaic = tiny_skia::Pixmap::new(side, side).unwrap();
        for (i, px) in mosaic.data_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i as u32 % side, i as u32 / side);
            px.copy_from_slice(&[(x / 4) as u8, (y / 4) as u8, 128, 255]);
        }
        let frame = rustgis_tiles::Frame { left: -half, top: half, m_per_px: 2.0 * half / f64::from(side), ppp: 1.0 };
        for (lon, lat, res) in [
            (10.0, 45.0, 2_000.0),
            (10.0, 20.0, 40_000.0),
            (179.0, -30.0, 5_000.0),
            (30.0, 80.0, 15_000.0),
            (-60.0, -84.0, 4_000.0),
            (0.0, 85.0, 800.0),
        ] {
            let camera = Camera { center: lonlat_to_merc(Coord::new(lon, lat)), res, bearing: 0.0, pitch: 0.0 };
            let g = rustgis_map::globe::Globe::new(&camera, 300.0, 200.0);
            let to_lonlat = |x: f64, y: f64| g.inverse(x, y);
            let to_merc = |c: Coord| lonlat_to_merc(Coord::new(c.x, c.y.clamp(-rustgis_geom::MAX_LAT, rustgis_geom::MAX_LAT)));
            let mut fast = tiny_skia::Pixmap::new(300, 200).unwrap();
            super::warp(&mut fast, &mosaic, &frame, &to_lonlat, &to_merc);
            let (mut drawn, mut worst) = (0, 0);
            for y in 0..200u32 {
                for x in 0..300u32 {
                    let got = fast.pixel(x, y).unwrap();
                    let Some(m) = to_lonlat(f64::from(x) + 0.5, f64::from(y) + 0.5).map(to_merc) else {
                        assert_eq!(got.alpha(), 0, "drawn off the globe at {x}, {y}");
                        continue;
                    };
                    let (mx, my) =
                        (((m.x - frame.left) / frame.m_per_px).clamp(0.0, 1023.0), ((frame.top - m.y) / frame.m_per_px).clamp(0.0, 1023.0));
                    let want = ((mx as u32 / 4) as i32, (my as u32 / 4) as i32);
                    assert_eq!(got.alpha(), 255, "left out at {x}, {y}");
                    drawn += 1;
                    worst = worst.max((i32::from(got.red()) - want.0).abs()).max((i32::from(got.green()) - want.1).abs());
                }
            }
            assert!(drawn > 1000, "the globe at {lon}, {lat} is in view");
            assert!(worst <= 1, "a pixel {worst} mosaic steps off at {lon}, {lat}");
        }
    }

    /// What `wms.test` and `wmts.test` were asked for.
    static ASKED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    /// What `one.test` was asked for (a test of its own: tests run at once).
    static ASKED_ONE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    /// Held by the tests reading `ASKED_ONE`, one at a time.
    static ONE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Requests that are never answered: their tiles stay on their way.
    static PARKED: std::sync::Mutex<Vec<rustgis_tiles::cache::Done>> = std::sync::Mutex::new(Vec::new());
    /// Fetches under `stuck.test/`, left unanswered until a test lets them go: another's fetch
    /// still under way.
    static STUCK: std::sync::Mutex<Vec<rustgis_tiles::cache::Done>> = std::sync::Mutex::new(Vec::new());
    /// Fetches under `tiles.test/held/`, kept in [`HELD`] while `HOLD` is set (a layout page drawn
    /// short of its frame's tiles), answered at once after.
    static HOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static HELD: std::sync::Mutex<Vec<rustgis_tiles::cache::Done>> = std::sync::Mutex::new(Vec::new());

    pub(crate) static PRIVATE_SPRITE_REQUESTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Serves tiles and documents for `https://tiles.test/…` without a network. Under `split/`,
    /// zoom 11 is red, zoom 13 blue, and zoom 12 never arrives.
    pub(crate) fn serve() {
        rustgis_tiles::cache::set_fetcher(|url, done| {
            if url.starts_with("http://127.0.0.1/mesh-private-only") {
                PRIVATE_SPRITE_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let color = |r, g, b| {
                let mut pm = tiny_skia::Pixmap::new(256, 256).unwrap();
                pm.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
                pm.encode_png().unwrap()
            };
            if url.starts_with("https://tiles.test/red/") || url.starts_with("https://tiles.test/split/11/") {
                done(Ok(color(220, 20, 20)));
            } else if url.starts_with("https://tiles.test/split/13/") {
                done(Ok(color(20, 20, 220)));
            } else if url.starts_with("https://tiles.test/split/12/") {
                PARKED.lock().unwrap().push(done);
            } else if url.starts_with("https://stuck.test/") {
                STUCK.lock().unwrap().push(done);
            } else if url.starts_with("https://tiles.test/held/") {
                if HOLD.load(std::sync::atomic::Ordering::SeqCst) {
                    HELD.lock().unwrap().push(done);
                } else {
                    done(Ok(color(220, 20, 20)));
                }
            } else if url.starts_with("https://wms.test/") || url.starts_with("https://wmts.test/") {
                // A service drawing in a system of its own (#769): green, and what it was asked.
                ASKED.lock().unwrap().push(url.clone());
                done(Ok(color(20, 200, 20)));
            } else if url.starts_with("https://one.test/") {
                ASKED_ONE.lock().unwrap().push(url.clone());
                // Under `tiles-only/`, a service drawing no picture larger than a tile.
                if url.starts_with("https://one.test/tiles-only/") && !url.contains("&WIDTH=256&HEIGHT=256") {
                    done(Err("400 Bad Request".into()));
                } else {
                    done(Ok(color(20, 200, 20)));
                }
            } else if url == "https://tiles.test/style.json" {
                let style = r##"{"version": 8, "sources": {}, "layers": [{"id": "bg", "type": "background", "paint": {"background-color": "#2050e0"}}]}"##;
                done(Ok(style.as_bytes().to_vec()));
            } else if url == "https://tiles.test/missing-source-style.json" {
                let style = serde_json::json!({"version": 8,
                "sources": {
                    "s": {"type": "raster", "tiles": ["https://tiles.test/red/{z}/{x}/{y}.png"]},
                    "unsupported": {"type": "geojson", "data": {"type": "FeatureCollection", "features": []}}
                },
                "layers": [
                    {"id": "valid", "type": "raster", "source": "s"},
                    {"id": "no-source", "type": "raster"},
                    {"id": "no-definition", "type": "raster", "source": "absent"},
                    {"id": "no-reader", "type": "raster", "source": "unsupported"}
                ]});
                done(Ok(serde_json::to_vec(&style).unwrap()));
            } else {
                done(Err("404".into()));
            }
        });
    }

    fn pixel(map: &mut Map) -> [u8; 4] {
        let camera = Camera { center: lonlat_to_merc(Coord::new(10.0, 50.0)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 100.0, pixels_per_point: 1.0, camera, sky: false };
        let png = crate::render_png(map, &view).unwrap();
        let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let p = pm.pixel(100, 50).unwrap();
        [p.red(), p.green(), p.blue(), p.alpha()]
    }

    /// A WMS layer offered only in Switzerland's LV95 (EPSG:2056) and a WMTS layer whose matrices
    /// are in it (#769): on a Web Mercator map, asked for in LV95 and warped into the map's tiles;
    /// on a map drawn in LV95, asked for in its own tiles (the WMS layer's 256 pixels a side, on a
    /// grid of powers of two; the WMTS layer's of its matrix) and drawn as they come.
    #[test]
    fn layers_in_a_system_of_their_own() {
        use rustgis_data::ogc::warp::{Grid, Matrix, System, Warp};
        serve();
        let area = Some([5.9, 45.8, 10.5, 47.8]);
        let lv95 = System { epsg: 2056, name: "EPSG:2056".into(), lat_first: false };
        let wms = Warp { epsg: 2056, grid: Grid::Map { lat_first: false }, area, systems: vec![lv95] }.address(
            "https://wms.test/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=a&STYLES=&CRS={ncrs}&BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}",
        );
        let matrices: Vec<Matrix> = [20.0, 10.0, 5.0, 2.5]
            .iter()
            .enumerate()
            .map(|(i, cell)| Matrix {
                id: format!("m{i}"),
                left: 2_420_000.0,
                top: 1_350_000.0,
                cell: *cell,
                tile_w: 256,
                tile_h: 256,
                cols: 5000,
                rows: 5000,
            })
            .collect();
        let wmts =
            Warp { epsg: 2056, grid: Grid::Tiles(matrices), area, systems: Vec::new() }.address("https://wmts.test/{nm}/{ncol}/{nrow}.png");
        let bern = Coord::new(7.4474, 46.948);
        for (name, url) in [("WMS", &wms), ("WMTS", &wmts)] {
            let mut m = Map::new("Map");
            m.graticule = false;
            m.tiles.push(TileLayer::new(name, TileKind::Raster, url));
            let at = |m: &mut Map, res: f64| {
                let center = m.to_map(bern).unwrap();
                let view = View {
                    width: 200.0,
                    height: 100.0,
                    pixels_per_point: 1.0,
                    camera: Camera { center, res, bearing: 0.0, pitch: 0.0 },
                    sky: false,
                };
                ASKED.lock().unwrap().clear();
                let png = crate::render_png(m, &view).unwrap();
                let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
                let p = pm.pixel(100, 50).unwrap();
                ([p.red(), p.green(), p.blue(), p.alpha()], std::mem::take(&mut *ASKED.lock().unwrap()))
            };
            // On Web Mercator: in LV95, warped.
            let (p, asked) = at(&mut m, 8.0);
            assert_eq!(p, [20, 200, 20, 255], "{name} on Web Mercator");
            assert!(!asked.is_empty(), "{name}");
            if name == "WMS" {
                assert!(asked.iter().all(|u| u.contains("&CRS=EPSG:2056&BBOX=2")), "{asked:?}");
            } else {
                assert!(asked.iter().all(|u| u.starts_with("https://wmts.test/m")), "{asked:?}");
            }
            // On a map in LV95: its own tiles, drawn as they come. (From another path: those the
            // warp asked for are in the cache.)
            m.set_projection(Projection::Custom("EPSG:2056".into()));
            m.tiles[0].url = m.tiles[0].url.replacen(".test/", ".test/lv95/", 1);
            let (p, asked) = at(&mut m, 5.0);
            assert_eq!(p, [20, 200, 20, 255], "{name} on LV95");
            if name == "WMS" {
                // 4 m pixels (the power of two below 5 m): tiles 1024 m a side on that grid.
                for u in &asked {
                    let bbox = u.split("&BBOX=").nth(1).unwrap().split('&').next().unwrap();
                    let n: Vec<f64> = bbox.split(',').map(|v| v.parse().unwrap()).collect();
                    assert!(u.ends_with("&WIDTH=256&HEIGHT=256") && n[2] - n[0] == 1024.0 && n[0] % 1024.0 == 0.0, "{u}");
                }
            } else {
                // The 5 m matrix's.
                assert!(!asked.is_empty() && asked.iter().all(|u| u.starts_with("https://wmts.test/lv95/m2/")), "{asked:?}");
            }
        }
    }

    /// A WMS layer asked for one picture of the view (#1162): one request for the view, of its
    /// size and more, in Web Mercator and in a system of its own; a small pan asks nothing new;
    /// in tiles, many.
    #[test]
    fn a_wms_layer_as_one_picture_of_the_view() {
        let _one = ONE_TEST.lock().unwrap_or_else(|e| e.into_inner());
        use rustgis_data::ogc::warp::{Grid, System, Warp};
        serve();
        let bern = Coord::new(7.4474, 46.948);
        let at = |m: &mut Map, res: f64, dx: f64| {
            let c = m.to_map(bern).unwrap();
            let center = Coord::new(c.x + dx, c.y);
            let view = View {
                width: 200.0,
                height: 100.0,
                pixels_per_point: 2.0,
                camera: Camera { center, res, bearing: 0.0, pitch: 0.0 },
                sky: false,
            };
            ASKED_ONE.lock().unwrap().clear();
            let png = crate::render_png(m, &view).unwrap();
            let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
            let px = |x, y| pm.pixel(x, y).map(|p| [p.red(), p.green(), p.blue(), p.alpha()]).unwrap();
            ([px(5, 5), px(200, 100), px(394, 194)], std::mem::take(&mut *ASKED_ONE.lock().unwrap()))
        };
        let green = [20, 200, 20, 255];
        // In Web Mercator.
        let mut m = Map::new("Map");
        m.graticule = false;
        let url = "https://one.test/merc/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=a&STYLES=&CRS=EPSG:3857&BBOX={bbox-epsg-3857}&WIDTH=256&HEIGHT=256&FORMAT=image%2Fpng";
        m.tiles.push(TileLayer::new("WMS", TileKind::Raster, url));
        let (_, tiled) = at(&mut m, 30.0, 0.0);
        assert!(tiled.len() > 1 && tiled.iter().all(|u| u.contains("&WIDTH=256&HEIGHT=256&")), "{tiled:?}");
        m.tiles[0].single_image = true;
        let (p, asked) = at(&mut m, 30.0, 0.0);
        assert_eq!(p, [green; 3], "the whole view drawn");
        // One picture, past the view (200 × 100 logical pixels: 512 × 256 with the margin).
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].contains("&CRS=EPSG:3857&BBOX=") && asked[0].contains("&WIDTH=512&HEIGHT=256&"), "{}", asked[0]);
        // A small pan: the same picture, from the cache.
        let (p, asked) = at(&mut m, 30.0, 300.0);
        assert_eq!((p, asked.len()), ([green; 3], 0));
        // Closer in than its finest zoom (10: 153 m a pixel), its picture there stretched, as its
        // finest tiles are: fewer pixels for the same view.
        m.tiles[0].max_zoom = 10;
        let (p, asked) = at(&mut m, 30.0, 0.0);
        assert_eq!((p, asked.len()), ([green; 3], 1));
        assert!(asked[0].contains("&WIDTH=256&HEIGHT=256&"), "{}", asked[0]);
        // Zoomed out past its coarsest zoom: nothing asked, nothing drawn.
        m.tiles[0].max_zoom = 19;
        m.tiles[0].min_zoom = 15;
        let (p, asked) = at(&mut m, 30.0, 0.0);
        assert!(p[1] != green && asked.is_empty(), "{p:?} {asked:?}");
        // A service drawing no picture larger than a tile: its tiles, after its picture failed,
        // and at once though a fetch of another's is still under way.
        let _ = rustgis_tiles::cache::get("https://stuck.test/0.png", rustgis_tiles::cache::Want::Image);
        let mut m = Map::new("Map");
        m.graticule = false;
        let mut t = TileLayer::new("WMS", TileKind::Raster, url.replace("one.test/merc/", "one.test/tiles-only/"));
        t.single_image = true;
        m.tiles.push(t);
        let t0 = std::time::Instant::now();
        let (p, asked) = at(&mut m, 30.0, 0.0);
        let took = t0.elapsed();
        for done in std::mem::take(&mut *STUCK.lock().unwrap()) {
            done(Err("404".into()));
        }
        assert_eq!(p, [green; 3], "drawn in tiles");
        assert!(took < std::time::Duration::from_secs(10), "waited {took:?} for another's fetch");
        assert!(asked.iter().any(|u| u.contains("&WIDTH=512&HEIGHT=256&")), "its picture asked first: {asked:?}");
        // In a system of its own, on a map in it.
        let lv95 = System { epsg: 2056, name: "EPSG:2056".into(), lat_first: false };
        let wms = Warp { epsg: 2056, grid: Grid::Map { lat_first: false }, area: Some([5.9, 45.8, 10.5, 47.8]), systems: vec![lv95] }.address(
            "https://one.test/lv95/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=a&STYLES=&CRS={ncrs}&BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}",
        );
        let mut m = Map::new("Map");
        m.graticule = false;
        m.set_projection(Projection::Custom("EPSG:2056".into()));
        let mut t = TileLayer::new("WMS", TileKind::Raster, wms);
        t.single_image = true;
        m.tiles.push(t);
        let (p, asked) = at(&mut m, 5.0, 0.0);
        assert_eq!(p, [green; 3]);
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].contains("&CRS=EPSG:2056&BBOX=2") && asked[0].ends_with("&WIDTH=512&HEIGHT=256"), "{}", asked[0]);
    }

    /// Turning the map round asks a single picture layer for a picture or two, not one every few
    /// degrees (the turned view's box widens and narrows as it turns): a picture holding the
    /// view at its detail is drawn again.
    #[test]
    fn a_turning_map_asks_for_a_picture_or_two() {
        let _one = ONE_TEST.lock().unwrap_or_else(|e| e.into_inner());
        use rustgis_data::ogc::warp::{Grid, System, Warp};
        serve();
        let bern = Coord::new(7.4474, 46.948);
        let turned = |m: &mut Map, res: f64, bearing: f64| {
            let center = m.to_map(bern).unwrap();
            let view = View {
                width: 200.0,
                height: 100.0,
                pixels_per_point: 2.0,
                camera: Camera { center, res, bearing, pitch: 0.0 },
                sky: false,
            };
            ASKED_ONE.lock().unwrap().clear();
            let png = crate::render_png(m, &view).unwrap();
            let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
            let px = |x, y| pm.pixel(x, y).map(|p| [p.red(), p.green(), p.blue(), p.alpha()]).unwrap();
            ([px(1, 1), px(200, 100), px(398, 198)], std::mem::take(&mut *ASKED_ONE.lock().unwrap()))
        };
        let green = [20, 200, 20, 255];
        let merc = "https://one.test/merc/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=turn&STYLES=&CRS=EPSG:3857&BBOX={bbox-epsg-3857}&WIDTH=256&HEIGHT=256&FORMAT=image%2Fpng";
        let lv95 = System { epsg: 2056, name: "EPSG:2056".into(), lat_first: false };
        let own = Warp { epsg: 2056, grid: Grid::Map { lat_first: false }, area: Some([5.9, 45.8, 10.5, 47.8]), systems: vec![lv95] }.address(
            "https://one.test/lv95/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=turn&STYLES=&CRS={ncrs}&BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}",
        );
        for (projection, url, res) in
            [(Projection::WebMercator, merc.to_string(), 30.0), (Projection::Custom("EPSG:2056".into()), own, 5.0)]
        {
            let mut m = Map::new("Map");
            m.graticule = false;
            m.set_projection(projection);
            let mut t = TileLayer::new("WMS", TileKind::Raster, url);
            t.single_image = true;
            m.tiles.push(t);
            let (p, asked) = turned(&mut m, res, 0.0);
            assert_eq!((p, asked.len()), ([green; 3], 1), "{asked:?}");
            let mut all = Vec::new();
            for step in 0..120 {
                let (p, asked) = turned(&mut m, res, 0.5 + f64::from(step) * 3.0);
                assert_eq!(p, [green; 3], "{step}");
                all.extend(asked);
            }
            assert!(all.len() <= 2, "{all:?}");
        }
    }

    /// A tile layer draws at its place in the drawing order (#808): under all the map's layers
    /// without one (as every tile layer did), over the layer it's placed over, and under a layer
    /// added later.
    #[test]
    fn tile_layers_draw_at_their_place_among_the_layers() {
        use rustgis_data::{Dataset, Geometry, GeometryType};
        serve();
        let mut m = Map::new("Map");
        m.graticule = false;
        let square = |m: &mut Map, name: &str, rgb: (u8, u8, u8)| {
            let mut d = Dataset::new(name, GeometryType::Polygon, vec![]);
            let ring =
                vec![Coord::new(0.0, 40.0), Coord::new(20.0, 40.0), Coord::new(20.0, 60.0), Coord::new(0.0, 60.0), Coord::new(0.0, 40.0)];
            d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
            let id = m.add_dataset(d);
            if let Some(l) = m.layer_mut(id) {
                let mut s = l.renderer.base_symbol();
                s.fill = rustgis_map::Rgba::rgb(rgb.0, rgb.1, rgb.2);
                s.stroke = s.fill;
                l.renderer = rustgis_map::Renderer::Single(s);
            }
            id
        };
        let green = square(&mut m, "Green", (20, 200, 20));
        m.tiles.push(TileLayer::new("Red", TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png"));
        assert_eq!(pixel(&mut m), [20, 200, 20, 255], "under the layers");
        m.tiles[0].over = Some(green);
        assert_eq!(pixel(&mut m), [220, 20, 20, 255], "over the layer it's placed over");
        square(&mut m, "Blue", (20, 20, 200));
        assert_eq!(pixel(&mut m), [20, 20, 200, 255], "under a layer added after it");
        // A layer it names that the map no longer has: under them all.
        m.tiles[0].over = Some(9999);
        assert_eq!(pixel(&mut m), [20, 20, 200, 255]);
    }

    #[test]
    fn draws_raster_tiles_and_styles() {
        serve();
        let mut m = Map::new("Map");
        m.graticule = false;
        let mut t = TileLayer::new("Red", TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png");
        t.attribution = "© Test".into();
        m.tiles.push(t);
        assert_eq!(pixel(&mut m), [220, 20, 20, 255]);
        // Half transparent over the basemap.
        m.tiles[0].opacity = 0.5;
        let p = pixel(&mut m);
        assert!(p[0] > 200 && (100..180).contains(&p[1]), "{p:?}");
        // A style on top, and the globe.
        m.tiles.insert(0, TileLayer::new("Blue", TileKind::Style, "https://tiles.test/style.json"));
        assert_eq!(pixel(&mut m), [0x20, 0x50, 0xE0, 255]);
        m.projection = Projection::Globe;
        let p = pixel(&mut m);
        // Shaded a little on the globe.
        assert!(p[2] > 150 && p[0] < 90, "{p:?}");
        // A layer that can't load draws nothing and says why.
        m.projection = Projection::WebMercator;
        m.tiles = vec![TileLayer::new("Gone", TileKind::Style, "https://tiles.test/missing.json")];
        let camera = Camera { center: lonlat_to_merc(Coord::new(10.0, 50.0)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 100.0, pixels_per_point: 1.0, camera, sky: false };
        let r = crate::render_complete(&mut m, &view).unwrap();
        assert!(!r.tiles_pending);
        assert_eq!(r.tile_errors.len(), 1, "{:?}", r.tile_errors);
    }

    /// Both full drawings and per-frame placement retain the counted missing-source notice.
    #[test]
    fn missing_source_diagnostics_reach_full_and_live_drawings() {
        serve();
        let mut map = Map::new("Map");
        map.tiles.push(TileLayer::new("Broken style", TileKind::Style, "https://tiles.test/missing-source-style.json"));
        let camera = Camera { center: lonlat_to_merc(Coord::new(10.0, 50.0)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 100.0, pixels_per_point: 1.0, camera, sky: false };
        let full = crate::render_complete(&mut map, &view).unwrap();
        let live = super::live(&super::spec(&map.tiles[0]), 1.0, &view).unwrap();
        let note = live.error.unwrap();
        assert!(note.contains("3 style layer(s) skipped"), "{note}");
        for id in ["no-source", "no-definition", "no-reader"] {
            assert!(note.contains(id), "{note}");
        }
        assert_eq!(full.tile_errors, [format!("Broken style: {note}")]);
        assert!(!live.quads.is_empty(), "the valid raster layer still draws");
    }

    /// A view across the antimeridian on the globe draws the tiles on both sides of it: the
    /// mosaic once ran from the westernmost point sampled east of 180° to the easternmost west
    /// of it, and left out the strip between 180° and the nearest sample (#223).
    #[test]
    fn the_globe_draws_tiles_across_the_antimeridian() {
        serve();
        let mut m = Map::new("Map");
        m.graticule = false;
        m.projection = Projection::Globe;
        m.tiles.push(TileLayer::new("Red", TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png"));
        for lon in [175.0, 179.5, -178.0] {
            let camera = Camera { center: lonlat_to_merc(Coord::new(lon, 0.0)), res: 20_000.0, bearing: 0.0, pitch: 0.0 };
            let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
            let pm = tiny_skia::Pixmap::decode_png(&crate::render_png(&mut m, &view).unwrap()).unwrap();
            // The middle of the view, well inside the globe's edge (5° is 28 pixels here).
            for y in (60..140).step_by(10) {
                for x in 40..160 {
                    let p = pm.pixel(x, y).unwrap();
                    assert!(p.red() > 120 && p.green() < 80 && p.blue() < 80, "centred on {lon}: ({x}, {y}) is {p:?}");
                }
            }
        }
    }

    /// With a pole in sight the globe's mosaic is the whole world wide, too many tiles at the
    /// zoom of the view's centre: the coarser zoom that fits is drawn, where the tiles farthest
    /// from the mosaic's centre (the far meridian, the polar rows) were left out (#223).
    #[test]
    fn the_globe_draws_tiles_around_a_pole() {
        serve();
        let mut m = Map::new("Map");
        m.graticule = false;
        m.projection = Projection::Globe;
        m.tiles.push(TileLayer::new("Red", TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png"));
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 80.0)), res: 40_000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 600.0, height: 600.0, pixels_per_point: 2.0, camera, sky: false };
        let pm = tiny_skia::Pixmap::decode_png(&crate::render_png(&mut m, &view).unwrap()).unwrap();
        // The pole and the meridian beyond it, up the middle of the view.
        for y in (20..700).step_by(20) {
            for x in (560..640).step_by(4) {
                let p = pm.pixel(x, y).unwrap();
                assert!(p.red() > 120 && p.green() < 80 && p.blue() < 80, "({x}, {y}) is {p:?}");
            }
        }
        // Around the south pole, and a small globe whose grid of points misses the pole near
        // its edge.
        for (lon, lat, res, pole) in [(-110.0, -55.0, 40_000.0, -89.9), (120.0, 20.0, 150_000.0, 89.9), (120.0, 20.0, 150_000.0, 86.0)] {
            let camera = Camera { center: lonlat_to_merc(Coord::new(lon, lat)), res, bearing: 0.0, pitch: 0.0 };
            let view = View { width: 600.0, height: 600.0, pixels_per_point: 2.0, camera, sky: false };
            let pm = tiny_skia::Pixmap::decode_png(&crate::render_png(&mut m, &view).unwrap()).unwrap();
            let g = rustgis_map::globe::Globe::new(&camera, 600.0, 600.0);
            let (x, y) = g.forward(Coord::new(lon, pole)).unwrap();
            let p = pm.pixel((x * 2.0) as u32, (y * 2.0) as u32).unwrap();
            assert!(p.red() > 120 && p.green() < 80 && p.blue() < 80, "({lon}, {lat}): {pole}° is {p:?}");
        }
    }

    /// On the GPU a raster tile layer is its tiles, placed on the map, rather than a picture of
    /// the view: the tiles then move with the camera and the map stays sharp while it moves (#568).
    #[test]
    fn the_gpu_places_raster_tiles_on_the_map() {
        use crate::gpu_scene::DrawCmd;
        use crate::{Backend, RenderOutput};
        serve();
        let mut m = Map::new("Map");
        m.graticule = false;
        m.basemap = rustgis_map::Basemap::Blank;
        m.tiles.push(TileLayer::new("Red", TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png"));
        let camera = Camera { center: lonlat_to_merc(Coord::new(10.0, 50.0)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 100.0, pixels_per_point: 1.0, camera, sky: false };
        // Fetched first, out to the margin (as a camera that just moved there has): past the
        // view only the tiles in the cache are placed.
        crate::render_complete(&mut m, &View { width: 400.0, height: 200.0, ..view }).unwrap();
        let Some(RenderOutput::Gpu(scene)) = crate::render_backend(&mut m, &view, Backend::Gpu) else { panic!("a gpu scene") };
        assert!(!scene.cmds.iter().any(|c| matches!(c, DrawCmd::Image { .. })), "no picture of the view");
        let tiles: Vec<_> = scene
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Tiles { origin, tiles } => Some((origin, tiles)),
                _ => None,
            })
            .collect();
        assert_eq!(tiles.len(), 1);
        let (origin, quads) = tiles[0];
        for q in quads.iter() {
            let p = q.pixmap().and_then(|pm| pm.pixel(10, 10)).unwrap();
            assert_eq!([p.red(), p.green(), p.blue()], [220, 20, 20]);
            assert!(q.uv.iter().all(|v| (0.0..=1.0).contains(v)) && q.uv[0] < q.uv[2] && q.uv[1] < q.uv[3], "{:?}", q.uv);
            assert!(q.opacity == 1.0);
        }
        // The view and the margin a moving camera reaches are covered by tiles.
        let held = crate::cull_extent(&view, crate::GPU_CULL_MARGIN);
        let covered = |x: f64, y: f64| {
            quads.iter().any(|q| {
                let r = q.rect.map(f64::from);
                x >= origin[0] + r[0] && x <= origin[0] + r[2] && y >= origin[1] + r[1] && y <= origin[1] + r[3]
            })
        };
        for (fx, fy) in [(0.0, 0.0), (0.5, 0.5), (1.0, 1.0), (0.0, 1.0), (1.0, 0.0)] {
            let (x, y) = (held.min_x + fx * held.width(), held.min_y + fy * held.height());
            assert!(covered(x, y), "({fx}, {fy}) of the held extent");
        }
        // See-through, at the layer's opacity.
        m.tiles[0].opacity = 0.5;
        let Some(RenderOutput::Gpu(scene)) = crate::render_backend(&mut m, &view, Backend::Gpu) else { panic!("a gpu scene") };
        assert!(scene.cmds.iter().all(|c| match c {
            DrawCmd::Tiles { tiles, .. } => tiles.iter().all(|q| (q.opacity - 0.5).abs() < 1e-6),
            DrawCmd::Image { .. } => false,
            _ => true,
        }));
        // Blended, or a style that draws more than raster tiles: still a picture.
        m.tiles[0].opacity = 1.0;
        m.tiles[0].blend = rustgis_map::effects::Blend::Multiply;
        let Some(RenderOutput::Gpu(scene)) = crate::render_backend(&mut m, &view, Backend::Gpu) else { panic!("a gpu scene") };
        assert!(scene.cmds.iter().any(|c| matches!(c, DrawCmd::Image { .. })));
        assert!(!scene.cmds.iter().any(|c| matches!(c, DrawCmd::Tiles { .. })));
        m.tiles[0].blend = rustgis_map::effects::Blend::Normal;
        m.tiles.insert(0, TileLayer::new("Blue", TileKind::Style, "https://tiles.test/style.json"));
        crate::render_complete(&mut m, &view).unwrap();
        let Some(RenderOutput::Gpu(scene)) = crate::render_backend(&mut m, &view, Backend::Gpu) else { panic!("a gpu scene") };
        let kinds: Vec<bool> = scene.cmds.iter().map(|c| matches!(c, DrawCmd::Tiles { .. })).collect();
        // The red tiles under the style's picture, in order.
        assert_eq!(kinds, [true, false], "{kinds:?}");
    }

    /// A tile on its way is stood in for by its children already in the cache, which are sharper,
    /// and by its parent only where they are missing: zooming in, the tiles of the zoom passed
    /// through are missing while those of the zoom it ends at are often here (#568).
    #[test]
    fn children_in_the_cache_stand_in_over_the_parent() {
        use crate::gpu_scene::DrawCmd;
        use crate::{Backend, RenderOutput};
        serve();
        let mut m = Map::new("Map");
        m.graticule = false;
        m.basemap = rustgis_map::Basemap::Blank;
        m.tiles.push(TileLayer::new("Split", TileKind::Raster, "https://tiles.test/split/{z}/{x}/{y}.png"));
        // Zoom 12's own resolution, nudged off the tile grid.
        let res = 2.0 * std::f64::consts::PI * rustgis_geom::MERCATOR_R / 256.0 / 4096.0;
        let center = lonlat_to_merc(Coord::new(2.3417, 48.8571));
        let view = |res: f64| View {
            width: 200.0,
            height: 100.0,
            pixels_per_point: 1.0,
            camera: Camera { center, res, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        // The parents and the children, fetched.
        crate::render_png(&mut m, &View { width: 400.0, height: 200.0, ..view(res * 2.0) }).unwrap();
        crate::render_png(&mut m, &View { width: 800.0, height: 400.0, ..view(res / 2.0) }).unwrap();
        let v = view(res);
        let r = crate::render(&mut m, &v).unwrap();
        let p = r.pixmap.pixel(100, 50).unwrap();
        let Some(RenderOutput::Gpu(scene)) = crate::render_backend(&mut m, &v, Backend::Gpu) else { panic!("a gpu scene") };
        let Some(DrawCmd::Tiles { origin, tiles }) = scene.cmds.iter().find(|c| matches!(c, DrawCmd::Tiles { .. })) else {
            panic!("tiles")
        };
        let over_centre: Vec<_> = tiles
            .iter()
            .filter(|q| {
                let (x, y) = (center.x - origin[0], center.y - origin[1]);
                f64::from(q.rect[0]) <= x && x <= f64::from(q.rect[2]) && f64::from(q.rect[1]) <= y && y <= f64::from(q.rect[3])
            })
            .map(|q| q.pixmap().and_then(|pm| pm.pixel(5, 5)).map(|c| c.blue()))
            .collect();
        let pending = r.tiles_pending;
        // Answered now: other tests' renders wait for every fetch under way.
        for done in std::mem::take(&mut *PARKED.lock().unwrap()) {
            done(Err("404".into()));
        }
        assert!(pending, "zoom 12 is on its way");
        assert_eq!([p.red(), p.green(), p.blue()], [20, 20, 220], "the children over the parent");
        assert_eq!(over_centre, [Some(220)], "a child, and not the parent under it");
    }

    #[test]
    fn blends_tile_layers_with_those_beneath() {
        serve();
        let mut m = Map::new("Map");
        m.graticule = false;
        m.tiles.push(TileLayer::new("Red", TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png"));
        let mut blue = TileLayer::new("Blue", TileKind::Style, "https://tiles.test/style.json");
        blue.blend = rustgis_map::effects::Blend::Multiply;
        m.tiles.insert(0, blue);
        // Multiplied: red × blue, not the blue on top.
        let p = pixel(&mut m);
        assert!(p[0] < 40 && p[1] < 15 && p[2] < 30 && p[3] == 255, "{p:?}");
        // Screened: lighter than either.
        m.tiles[0].blend = rustgis_map::effects::Blend::Screen;
        let p = pixel(&mut m);
        assert!(p[0] > 220 && p[1] > 80 && p[2] > 0xE0, "{p:?}");
        // And in the mosaic warped onto another projection (the globe shades it a little).
        m.tiles[0].blend = rustgis_map::effects::Blend::Multiply;
        m.projection = Projection::Globe;
        let p = pixel(&mut m);
        assert!(p[0] < 60 && p[1] < 40 && p[2] < 60, "{p:?}");
    }

    /// A layout page drawn while a map frame's tiles are still on their way says so, for the
    /// browser's layout view to draw it again as they land (#1534); drawn once they have, it
    /// doesn't. The browser doesn't wait for tiles: here the wait is cancelled, which skips it too.
    #[test]
    fn a_layout_page_drawn_short_of_its_frames_tiles_says_so() {
        use rustgis_map::layout::{ElementKind, FrameShape, INDICATOR_COLOR, INDICATOR_WIDTH, Layout};
        use std::sync::atomic::Ordering;
        struct Cancelled;
        impl rustgis_data::progress::Probe for Cancelled {
            fn cancelled(&self) -> bool {
                true
            }
            fn begin(&self, _: u64) {}
            fn advance(&self, _: u64) {}
        }
        serve();
        let mut m = Map::new("Map");
        m.graticule = false;
        m.basemap = rustgis_map::Basemap::Blank;
        m.tiles.push(TileLayer::new("Held", TileKind::Raster, "https://tiles.test/held/{z}/{x}/{y}.png"));
        let mut page = Layout::new("page", 100.0, 80.0);
        page.add(
            "map",
            (5.0, 5.0, 90.0, 70.0),
            ElementKind::MapFrame {
                map: "Map".into(),
                center: Coord::new(4.6, 4.7),
                scale: 500_000.0,
                shape: FrameShape::Rectangle,
                grid: None,
                extent_of: None,
                extent_color: INDICATOR_COLOR,
                extent_width: INDICATOR_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        let info = crate::layout::PageInfo::default();
        let mut maps = [m];
        HOLD.store(true, Ordering::SeqCst);
        let short = {
            let _wait = rustgis_data::progress::install(Some(std::sync::Arc::new(Cancelled)));
            crate::layout::render_pending(&page, &mut maps, 72.0, &info).map(|(_, pending)| pending)
        };
        // Answered now, and those asked for later at once: the next drawing waits for them.
        HOLD.store(false, Ordering::SeqCst);
        let start = std::time::Instant::now();
        while HELD.lock().unwrap().is_empty() && start.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for done in std::mem::take(&mut *HELD.lock().unwrap()) {
            done(Ok(tiny_skia::Pixmap::new(256, 256).unwrap().encode_png().unwrap()));
        }
        assert_eq!(short, Ok(true), "drawn before its tiles landed");
        let whole = crate::layout::render_pending(&page, &mut maps, 72.0, &info).map(|(_, pending)| pending);
        assert_eq!(whole, Ok(false), "drawn once they have");
    }
}
