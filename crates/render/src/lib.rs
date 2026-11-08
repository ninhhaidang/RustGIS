//! L3: the CPU map renderer.
//!
//! [`render`] draws a [`Map`] (basemap, graticule, layers, selection) into a premultiplied RGBA
//! `tiny_skia::Pixmap` and places labels without collisions. The desktop app shows the pixmap
//! as a texture and draws the placed labels with its own text engine; [`render_png`] rasterises
//! the labels too, for export and the command line.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod canvas;
mod diagrams;
mod effects;
pub mod families;
pub mod fonts;
mod glyphs;
pub mod gpu_scene;
mod gradient_fill;
mod label_places;
pub mod layout;
mod line_labels;
mod line_markers;
pub mod logo;
mod mesh;
mod native_tiles;
mod pattern_fill;
pub mod pdf;
mod picture;
mod point_marks;
pub mod post;
mod raster;
pub mod raster_export;
pub mod report;
mod single_image;
mod stroke_paths;
pub mod svg;
pub mod terrain;
mod text;
pub use text::CALLOUT_WIDTH;
pub mod tile_mesh;
mod tiles;
pub mod vector;
pub use tiles::{Live, live as live_tiles};

pub use rustgis_tiles;
pub use tiles::{source_layers as tile_source_layers, spec as tile_spec, style_view};

/// Natural Earth 1:110m sample data (public domain), fetched by `build.rs` from rustgis-assets.
pub mod sample_data {
    pub const COUNTRIES: &str = include_str!(concat!(env!("OUT_DIR"), "/countries.geojson"));
    /// The countries as China sees its borders (Natural Earth's China point of view).
    pub const COUNTRIES_CHN: &str = include_str!(concat!(env!("OUT_DIR"), "/countries_chn.geojson"));
    pub const CITIES: &str = include_str!(concat!(env!("OUT_DIR"), "/cities.geojson"));
    pub const RIVERS: &str = include_str!(concat!(env!("OUT_DIR"), "/rivers.geojson"));
    pub const LAKES: &str = include_str!(concat!(env!("OUT_DIR"), "/lakes.geojson"));

    use std::sync::atomic::{AtomicU8, Ordering};

    /// Whose view of disputed borders the built-in countries and world outline show.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum Worldview {
        /// Natural Earth's de facto borders.
        #[default]
        Default,
        /// China's view (required for maps shown in China).
        China,
    }

    impl Worldview {
        pub fn code(self) -> &'static str {
            match self {
                Worldview::Default => "default",
                Worldview::China => "china",
            }
        }

        pub fn from_code(code: &str) -> Option<Worldview> {
            match code.trim().to_ascii_lowercase().as_str() {
                "default" => Some(Worldview::Default),
                "china" | "chn" | "cn" => Some(Worldview::China),
                _ => None,
            }
        }

        /// The worldview for a system in time zone `tz` (IANA, such as `Asia/Shanghai`) whose
        /// preferred locales are `locales` (such as `zh-CN`, `zh_CN.UTF-8`): China when the time
        /// zone is one of mainland China's or the first locale is mainland Chinese.
        pub fn detect<'a>(tz: Option<&str>, locales: impl IntoIterator<Item = &'a str>) -> Worldview {
            let china_tz = tz.is_some_and(|tz| {
                matches!(
                    tz.trim(),
                    "Asia/Shanghai" | "Asia/Chongqing" | "Asia/Chungking" | "Asia/Harbin" | "Asia/Urumqi" | "Asia/Kashgar" | "PRC"
                )
            });
            let china_locale = locales.into_iter().next().is_some_and(|tag| {
                let base = tag.split(['.', '@']).next().unwrap_or("").replace('_', "-").to_ascii_lowercase();
                let mut parts = base.split('-');
                parts.next() == Some("zh") && parts.any(|p| p == "cn")
            });
            if china_tz || china_locale { Worldview::China } else { Worldview::Default }
        }
    }

    static WORLDVIEW: AtomicU8 = AtomicU8::new(0);

    /// Sets the worldview of the countries sample and the basemap's world outline.
    pub fn set_worldview(w: Worldview) {
        WORLDVIEW.store(w as u8, Ordering::Relaxed);
    }

    pub fn worldview() -> Worldview {
        if WORLDVIEW.load(Ordering::Relaxed) == Worldview::China as u8 { Worldview::China } else { Worldview::Default }
    }

    /// The countries GeoJSON in the current worldview.
    pub fn countries() -> &'static str {
        match worldview() {
            Worldview::Default => COUNTRIES,
            Worldview::China => COUNTRIES_CHN,
        }
    }
}

use std::sync::OnceLock;

pub(crate) use canvas::Canvas;
use rustgis_data::{Dataset, Geometry, GeometryType};
use rustgis_geom::{Coord, Extent, extent_to_merc, lonlat_to_merc};
use rustgis_map::globe::{V3, densify};
use rustgis_map::layout::TextAlign;
use rustgis_map::{Camera, Globe, LabelPlacement, LineStroke, Map, Marker, Rgba, Symbol};
pub use text::halo_reach;
/// The image type pages and maps are drawn into.
pub use tiny_skia::Pixmap;
use tiny_skia::{
    FillRule, GradientStop, LineCap, LineJoin, Paint, PathBuilder, PixmapPaint, Point, RadialGradient, SpreadMode, Stroke, Transform,
};
use unicode_segmentation::UnicodeSegmentation;

/// Selection highlight (cyan, as desktop GIS users expect).
pub const SELECTION: Rgba = Rgba::rgb(0x00, 0xE5, 0xFF);

/// Largest raster rendered (pixels per side).
pub const MAX_SIDE: u32 = 8192;

/// How far past each side of the view (a fraction of its size) features are still drawn.
pub const CULL_MARGIN: f64 = 0.05;

/// The wider margin the GPU backend tessellates vector layers out to: while the map moves,
/// its meshes are moved rather than rebuilt, so they hold the ground a pan of half the view
/// or zooming out to twice the extent reveals (as MapLibre keeps the tiles around the view).
pub const GPU_CULL_MARGIN: f64 = 0.5;

/// The map extent a frame drawn for `view` holds features for: the view grown by `margin`
/// ([`CULL_MARGIN`], or [`GPU_CULL_MARGIN`] for GPU meshes) on every side.
pub fn cull_extent(view: &View, margin: f64) -> Extent {
    view.camera.extent(view.width, view.height).padded(margin, 0.0)
}

/// What to draw and at which size.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Size in logical points.
    pub width: f64,
    pub height: f64,
    /// Physical pixels per logical point.
    pub pixels_per_point: f64,
    pub camera: Camera,
    /// The interactive view: the globe sits in dark, starry space. Exports and layouts leave it
    /// off and get a light space that suits paper (a dark one only on the Midnight basemap).
    pub sky: bool,
}

impl View {
    /// Display error budget in map units, with one device pixel independent of device density.
    fn lod_tolerance(&self) -> f64 {
        self.camera.res / self.pixels_per_point.max(0.01) * rustgis_map::lod::THIN_PX
    }

    pub fn pixel_size(&self) -> (u32, u32) {
        let w = (self.width * self.pixels_per_point).round().clamp(1.0, MAX_SIDE as f64) as u32;
        let h = (self.height * self.pixels_per_point).round().clamp(1.0, MAX_SIDE as f64) as u32;
        (w, h)
    }
    /// Logical screen position of a Web Mercator point.
    pub fn to_screen(&self, p: Coord) -> (f64, f64) {
        self.camera.to_screen(p, self.width, self.height)
    }
}

/// A placed label in logical points (centre of the text).
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedLabel {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub color: Rgba,
    pub halo: Option<Rgba>,
    /// How far the halo reaches beyond the letters (points), as a label class sets it; `None` is
    /// the thin halo every label had before.
    pub halo_width: Option<f32>,
    pub bold: bool,
    /// An icon placed with the label (a tile style's `icon-image`), drawn into the map.
    pub icon: Option<PlacedIcon>,
    /// Which part of the text is at `x`: 0 its centre, -0.5 its left edge, 0.5 its right edge.
    /// Each renderer lines the text up with its own measure of the width, so text set beside an
    /// icon keeps clear of it whatever the font.
    pub align: f32,
    /// Framed in the selection colour: a selected text graphic. Each renderer frames the text as
    /// it lays it out.
    pub framed: bool,
    /// Slanted, and the letter spacing in points: a text graphic's (#535); other labels are
    /// upright with no spacing.
    pub italic: bool,
    pub spacing: f32,
    /// The installed family it is drawn in, by its place in [`families::installed`] (a label
    /// class's font); `None` is the label font.
    pub font: Option<u16>,
    /// How the lines line up in their block (a text graphic's, #535); the block itself sits where
    /// `align` puts it.
    pub line_align: TextAlign,
    /// Turned with the map (a tile label along a street): the text's angle in radians,
    /// clockwise, about its centre (`align` is then 0), kept upright. `None` is upright on the
    /// screen.
    pub angle: Option<f32>,
    /// How it collides with other labels (padding, optional text or icon, overlaps).
    pub collide: rustgis_tiles::Collide,
    /// Curved along a line: its characters one by one, each placed and turned where the line
    /// takes it (`text` is all of them). Empty for a label drawn as one block.
    pub glyphs: Vec<LabelGlyph>,
    /// A rectangle behind the text (a label class's background, its sizes in points as the
    /// class's grown by a reference scale); none draws the text alone. A curved label has none.
    pub background: Option<rustgis_map::LabelBackground>,
}

/// One character of a label curved along a line: its text, and its centre and angle (radians,
/// clockwise, kept upright) relative to the label's `x`, `y`.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelGlyph {
    pub text: String,
    pub dx: f32,
    pub dy: f32,
    pub angle: f32,
}

impl PlacedLabel {
    /// The centre of the text, by this crate's measure of its width.
    pub fn center_x(&self) -> f32 {
        self.x - self.align * text::label_width(self)
    }
}

/// An icon in logical points: its picture and centre.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedIcon {
    pub image: std::sync::Arc<Pixmap>,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Turned with the map (an arrow along a street), in radians clockwise; `None` is upright.
    pub angle: Option<f32>,
    /// How much of it shows, 0 to 1: a tile layer's opacity fades its icons as it fades its
    /// fills and lines (#1358). 1 draws it as it is.
    pub alpha: f32,
}

pub struct Rendered {
    /// The frame's pixels; 1 × 1 and empty when it was kept as vectors (`recorded`).
    pub pixmap: Pixmap,
    pub labels: Vec<PlacedLabel>,
    /// Labels that didn't fit, when the map shows unplaced labels (drawn in red).
    pub unplaced: Vec<PlacedLabel>,
    /// The credits the map's tile layers ask for.
    pub attribution: Vec<String>,
    /// Tiles are still on their way: drawing again later shows more.
    pub tiles_pending: bool,
    /// Streamed layers are still reading the view: drawing again later shows them.
    pub data_pending: bool,
    /// Tile layers that couldn't be drawn, and why.
    pub tile_errors: Vec<String>,
    /// Raster layers read in place drawn from less than the view needs (a coarser level, tiles
    /// they couldn't read), by id, their errors among `tile_errors`: an export refuses them.
    pub partial: Vec<u32>,
    /// Raster layers read in place whose tiles are still on their way, by id: drawn again as
    /// they land; an export that can't wait refuses them.
    pub pending_in_place: Vec<u32>,
    /// The frame kept as vectors, when it was recorded ([`render_complete_recorded`]).
    pub(crate) recorded: Option<Canvas>,
}

/// Which rasteriser [`render_backend`] targets: the tiny-skia CPU pixmap (the default, and the
/// only path for exports, the globe and effect-heavy frames) or a lyon-tessellated GPU scene
/// drawn by the UI inside egui's wgpu pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Cpu,
    Gpu,
    /// A GPU scene whose raster tile layers, on a flat north-up map, are left for the GPU map to
    /// place for every frame it shows ([`gpu_scene::DrawCmd::LiveTiles`], #635).
    GpuLive,
}

impl Backend {
    /// Whether this is a GPU scene.
    pub fn is_gpu(self) -> bool {
        matches!(self, Backend::Gpu | Backend::GpuLive)
    }
}

/// A finished render in either backend.
pub enum RenderOutput {
    Cpu(Rendered),
    Gpu(gpu_scene::GpuScene),
}

/// Renders the map for the screen in the chosen backend. `Backend::Cpu` is exactly
/// [`render`]; `Backend::Gpu` draws what it can as tessellated meshes and everything else
/// (basemap, tiles, rasters, masked/blended/transparent layers) as CPU-rasterized images in
/// the same z-order.
pub fn render_backend(map: &mut Map, view: &View, backend: Backend) -> Option<RenderOutput> {
    render_with_backend(map, view, false, backend)
}

/// Decodes PNG bytes into a pixmap.
pub fn decode_png(png: &[u8]) -> Option<Pixmap> {
    Pixmap::decode_png(png).ok()
}

/// The pixel size of a base64 PNG or JPEG picture, `None` when it can't be decoded.
pub fn picture_size(b64: &str) -> Option<(u32, u32)> {
    picture::size(b64)
}

/// The colour unplaced labels are drawn in.
pub const UNPLACED: Rgba = Rgba::rgb(0xD0, 0x1C, 0x1C);

/// The distance (metres) a viewer of a flat map at a ground resolution would see it from: the
/// view's height seen through a 60° field of view.
pub fn viewing_distance(ground_res: f64, view_height: f64) -> f64 {
    ground_res * view_height * 0.866
}

/// The built-in world outline used by every basemap (Natural Earth 1:110m, public domain), in the
/// current worldview.
fn world() -> &'static Vec<Geometry> {
    static W: [OnceLock<Vec<Geometry>>; 2] = [OnceLock::new(), OnceLock::new()];
    let wv = sample_data::worldview();
    let cell = &W[wv as usize];
    cell.get_or_init(|| match rustgis_data::io::read_geojson(sample_data::countries(), "world") {
        Ok((ds, _)) => ds.features.into_iter().filter_map(|f| f.geometry).map(|g| g.map_coords(&lonlat_to_merc)).collect(),
        Err(e) => {
            log::warn!("built-in world outline unreadable: {e}");
            Vec::new()
        }
    })
}

/// The built-in world outline in longitude/latitude, for the globe.
fn world_lonlat() -> &'static Vec<Geometry> {
    static W: [OnceLock<Vec<Geometry>>; 2] = [OnceLock::new(), OnceLock::new()];
    let cell = &W[sample_data::worldview() as usize];
    cell.get_or_init(|| world().iter().map(|g| g.map_coords(&rustgis_geom::merc_to_lonlat)).collect())
}

thread_local! {
    /// The world outline in each custom projection used so far.
    static WORLD_IN: std::cell::RefCell<std::collections::HashMap<String, std::rc::Rc<Vec<Geometry>>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The built-in world outline in a custom projection's coordinates (kept per projection).
fn world_in(flat: &rustgis_map::projection::Flat, key: &str) -> std::rc::Rc<Vec<Geometry>> {
    WORLD_IN.with(|cell| {
        cell.borrow_mut()
            .entry(format!("{}:{key}", sample_data::worldview().code()))
            .or_insert_with(|| std::rc::Rc::new(world_lonlat().iter().map(|g| flat.project(g)).collect()))
            .clone()
    })
}

/// Largest angle between globe vertices before edges are bent along great circles (2°).
const GLOBE_STEP: f64 = 0.035;

/// The built-in world dataset (lon/lat), for the sample project.
pub fn world_dataset(name: &str, text: &str) -> Option<Dataset> {
    rustgis_data::io::read_geojson(text, name).ok().map(|(d, _)| d)
}

thread_local! {
    /// Antialiasing for the map being drawn (off in the fast display type).
    static ANTIALIAS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Sets antialiasing for one render and puts it back on when dropped, however the render ends.
struct Antialias;

impl Antialias {
    fn set(on: bool) -> Antialias {
        ANTIALIAS.with(|a| a.set(on));
        Antialias
    }
}

impl Drop for Antialias {
    fn drop(&mut self) {
        ANTIALIAS.with(|a| a.set(true));
    }
}

/// Whether the map being drawn is antialiased.
fn antialiased() -> bool {
    ANTIALIAS.with(|a| a.get())
}

fn paint(c: Rgba, opacity: f32) -> Paint<'static> {
    let mut p = Paint::default();
    let a = (c.a() as f32 * opacity).round().clamp(0.0, 255.0) as u8;
    p.set_color_rgba8(c.r(), c.g(), c.b(), a);
    p.anti_alias = ANTIALIAS.with(|a| a.get());
    p
}

/// The size, in device pixels across, up to which a filled polygon's outline is mitred
/// (`Ctx::fill_outline`).
const SMALL_POLYGON_PX: f32 = 4.0;

/// The widest outline, in device pixels, mitred on a small polygon: the default 0.7 pt outline on
/// a 2× screen (1.4 px), not one at print resolution, whose corners would show.
const THIN_OUTLINE_PX: f32 = 2.0;

/// How far, in outline widths, a mitred corner may reach (`Ctx::outline_px`). A limit of 2 bevels
/// corners sharper than 60°, whose mitres would stand out as spikes on a polygon a few pixels
/// across; a 90° corner, the commonest, reaches 0.7 widths past its vertex and stays mitred.
const MITRE_LIMIT: f32 = 2.0;

/// The most dashes a dashed stroke is cut into along one path (`Ctx::dash`).
const MAX_DASHES: f32 = 100_000.0;

struct Ctx<'a> {
    view: &'a View,
    ppp: f32,
    /// Polygons drawn as outlines only.
    wireframe: bool,
    /// Visible Mercator extent with a margin, for culling.
    cull: Extent,
    /// Set when drawing the globe; coordinates are then longitude/latitude, not Web Mercator.
    globe: Option<Globe>,
    /// Point marker sprites drawn so far, by symbol and quarter-pixel offset.
    sprites: std::cell::RefCell<std::collections::HashMap<SpriteKey, Option<Pixmap>>>,
    /// The pixels the frame's shapeburst fills have worked on (`gradient_fill::FRAME_BURST_PX`),
    /// shared by a layer's scaled context (a reference scale's) with the frame's.
    bursts: std::rc::Rc<std::cell::Cell<f32>>,
}

/// A marker sprite's symbol and quarter-pixel offset.
type SpriteKey = (Rgba, Rgba, u32, u32, Marker, u8, u8);

impl Ctx<'_> {
    /// Logical screen position of a coordinate, `None` when it is behind the globe.
    fn screen(&self, c: Coord) -> Option<(f64, f64)> {
        match &self.globe {
            Some(g) => g.forward(c),
            None => Some(self.view.to_screen(c)),
        }
    }

    fn px(&self, c: Coord) -> (f32, f32) {
        match self.screen(c) {
            Some((x, y)) => ((x * self.view.pixels_per_point) as f32, (y * self.view.pixels_per_point) as f32),
            None => (f32::NAN, f32::NAN),
        }
    }

    fn vpx(&self, g: &Globe, v: V3) -> (f32, f32) {
        let (x, y) = g.project(v);
        ((x * self.view.pixels_per_point) as f32, (y * self.view.pixels_per_point) as f32)
    }

    /// View vectors of a ring or line, with long edges bent along great circles.
    fn globe_vectors(g: &Globe, pts: &[Coord]) -> Vec<V3> {
        let mut out: Vec<V3> = Vec::with_capacity(pts.len());
        for c in pts {
            if !c.is_finite() {
                continue;
            }
            let v = g.to_view(*c);
            match out.last().copied() {
                Some(prev) => out.extend(densify(prev, v, GLOBE_STEP)),
                None => out.push(v),
            }
        }
        out
    }

    /// A polygon ring on the globe. Hidden vertices are moved radially onto the horizon circle at
    /// their own bearing, so the hidden part of the ring runs along the limb in its true order and
    /// encloses no area; a ring that is wholly hidden is skipped.
    fn globe_ring(&self, g: &Globe, pb: &mut PathBuilder, ring: &[Coord]) {
        let vs = Self::globe_vectors(g, ring);
        let h = g.horizon();
        if vs.len() < 3 || vs.iter().all(|v| v[2] <= h) {
            return;
        }
        let r = (1.0 - h * h).sqrt();
        let limb = |v: V3| {
            let a = v[1].atan2(v[0]);
            [r * a.cos(), r * a.sin(), h]
        };
        let mut out: Vec<V3> = Vec::with_capacity(vs.len() + 8);
        let push_limb = |out: &mut Vec<V3>, p: V3| {
            // Follow the limb between hidden points that are far apart in bearing.
            if let Some(last) = out.last().copied()
                && last[2] <= h + 1e-9
            {
                out.extend(g.horizon_arc(last, p));
            }
            out.push(p);
        };
        let mut prev: Option<V3> = None;
        for v in vs {
            let vis = v[2] > h;
            if let Some(p) = prev
                && (p[2] > h) != vis
            {
                push_limb(&mut out, g.horizon_crossing(p, v));
            }
            if vis {
                out.push(v)
            } else {
                push_limb(&mut out, limb(v))
            }
            prev = Some(v);
        }
        // A ring that ends and starts behind the horizon closes along the limb, not across it.
        if let (Some(first), Some(last)) = (out.first().copied(), out.last().copied())
            && first[2] <= h + 1e-9
            && last[2] <= h + 1e-9
        {
            out.extend(g.horizon_arc(last, first));
        }
        for (i, v) in out.iter().enumerate() {
            let (x, y) = self.vpx(g, *v);
            if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
        }
        if !out.is_empty() {
            pb.close();
        }
    }

    /// A line on the globe, broken where it goes behind the horizon.
    fn globe_line(&self, g: &Globe, pb: &mut PathBuilder, part: &[Coord]) {
        let vs = Self::globe_vectors(g, part);
        let h = g.horizon();
        let mut prev: Option<V3> = None;
        for v in vs {
            let vis = v[2] > h;
            match prev {
                Some(p) if p[2] > h && vis => {
                    let (x, y) = self.vpx(g, v);
                    pb.line_to(x, y);
                }
                Some(p) if p[2] > h => {
                    let (x, y) = self.vpx(g, g.horizon_crossing(p, v));
                    pb.line_to(x, y);
                }
                Some(p) if vis => {
                    let (x, y) = self.vpx(g, g.horizon_crossing(p, v));
                    pb.move_to(x, y);
                    let (x, y) = self.vpx(g, v);
                    pb.line_to(x, y);
                }
                None if vis => {
                    let (x, y) = self.vpx(g, v);
                    pb.move_to(x, y);
                }
                _ => {}
            }
            prev = Some(v);
        }
    }

    fn ring_path(&self, pb: &mut PathBuilder, ring: &[Coord], close: bool) {
        self.ring_path_at(pb, ring, close, &mut DashStarts::default());
    }

    /// [`Self::ring_path`], noting in `starts` how far along its line each piece it adds begins,
    /// so a dash pattern can carry on across a cut.
    fn ring_path_at(&self, pb: &mut PathBuilder, ring: &[Coord], close: bool, starts: &mut DashStarts) {
        if let Some(g) = &self.globe {
            if close {
                self.globe_ring(g, pb, ring)
            } else {
                self.globe_line(g, pb, ring)
            }
            return;
        }
        let ppp = self.view.pixels_per_point;
        let dev = |c: &Coord| {
            let (x, y) = self.view.to_screen(*c);
            (x * ppp, y * ppp)
        };
        // Each run of points the projection can show; a point it can't breaks the line rather
        // than being bridged.
        let clip = self.clip_box();
        for run in ring.split(|c| !c.is_finite()) {
            let mut b = Extent::EMPTY;
            for c in run {
                let (x, y) = dev(c);
                if x.is_finite() && y.is_finite() {
                    b.expand(Coord::new(x, y));
                }
            }
            if b.is_empty() || !b.intersects(&clip) {
                continue; // wholly off the image
            }
            if clip.contains(Coord::new(b.min_x, b.min_y)) && clip.contains(Coord::new(b.max_x, b.max_y)) {
                emit_run(pb, run.iter().map(dev), close);
                starts.push(0.0);
            } else if close {
                // Zoomed in on a big polygon: its far vertices would be millions of pixels away,
                // which the rasteriser can't take (and needn't walk). Clipping each ring to a box
                // well beyond the image keeps the even-odd fill the same inside it.
                let pts: Vec<(f64, f64)> = run.iter().map(dev).collect();
                emit_run(pb, clip_ring(&pts, &clip).into_iter(), true);
                starts.push(0.0);
            } else {
                let pts: Vec<(f64, f64)> = run.iter().map(dev).collect();
                for (start, part) in clip_line(&pts, &clip) {
                    emit_run(pb, part.into_iter(), false);
                    starts.push(start as f32);
                }
            }
        }
    }

    /// The box, in device pixels, paths are clipped to: the image with a wide margin, so a cut
    /// edge and its stroke stay out of sight.
    fn clip_box(&self) -> Extent {
        let (w, h) = self.view.pixel_size();
        let (w, h) = (w as f64, h as f64);
        let m = w.max(h) / 2.0 + 64.0;
        Extent { min_x: -m, min_y: -m, max_x: w + m, max_y: h + m }
    }

    /// Draws polygons' rings, filled even-odd with `fill`, outlined with `stroke` (its paint and
    /// width in points).
    fn polygon(&self, pm: &mut Canvas, polys: &[Vec<Vec<Coord>>], fill: Option<Paint>, stroke: Option<(Paint, f32)>) {
        let Some(path) = self.polygon_path(polys) else { return };
        if let Some(f) = fill {
            pm.fill_path(&path, &f, FillRule::EvenOdd, Transform::identity(), None);
        }
        if let Some((s, w)) = stroke {
            self.outline(pm, &path, &s, w);
        }
    }

    /// A polygon's rings as one path (its holes cut out by the even-odd rule); `None` for one
    /// with nothing to draw.
    pub(crate) fn polygon_path(&self, polys: &[Vec<Vec<Coord>>]) -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        for rings in polys {
            for r in rings {
                self.ring_path(&mut pb, r, true);
            }
        }
        pb.finish()
    }

    /// A polygon's outline `w` points wide (round joins); none for no width.
    pub(crate) fn outline(&self, pm: &mut Canvas, path: &tiny_skia::Path, p: &Paint, w: f32) {
        self.outline_px(pm, path, p, w * self.ppp, LineJoin::Round);
    }

    /// A polygon's outline `w` device pixels wide, with the given `line_join`; none for no width.
    /// Mitred corners sharper than 60° are bevelled ([`MITRE_LIMIT`]), so they don't grow spikes.
    fn outline_px(&self, pm: &mut Canvas, path: &tiny_skia::Path, p: &Paint, w: f32, line_join: LineJoin) {
        if w > 0.0 {
            // Only mitred joins read the limit.
            let st = Stroke { width: w, line_join, miter_limit: MITRE_LIMIT, ..Stroke::default() };
            pm.stroke_path(path, p, &st, Transform::identity(), None);
        }
    }

    /// A filled polygon's outline `w` points wide: with mitred joins (#1064) when the polygon is at
    /// most [`SMALL_POLYGON_PX`] across and the outline at most [`THIN_OUTLINE_PX`] wide, round
    /// otherwise. Round joins are curves the stroker builds at every vertex, which takes most of
    /// the time a layer of many small polygons draws in; at that size a mitred corner covers nearly
    /// the same pixels (a corner pixel may differ a little; the layer's tone doesn't). A wider
    /// outline (an export at print resolution, a thick line) or a larger polygon keeps its round
    /// corners.
    fn fill_outline(&self, pm: &mut Canvas, path: &tiny_skia::Path, p: &Paint, w: f32) {
        let px = w * self.ppp;
        let b = path.bounds();
        let small = b.width().max(b.height()) <= SMALL_POLYGON_PX && px <= THIN_OUTLINE_PX;
        self.outline_px(pm, path, p, px, if small { LineJoin::Miter } else { LineJoin::Round });
    }

    /// Strokes a line's parts `w` points wide (half a pixel at least), with round caps and joins.
    fn line(&self, pm: &mut Canvas, parts: &[Vec<Coord>], p: &Paint, w: f32) {
        let mut pb = PathBuilder::new();
        for part in parts {
            self.ring_path(&mut pb, part, false);
        }
        let Some(path) = pb.finish() else { return };
        let st = Stroke { width: (w * self.ppp).max(0.5), line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
        pm.stroke_path(&path, p, &st, Transform::identity(), None);
    }

    /// A line drawn as its stacked strokes (`Layer::strokes`), bottom first, each
    /// shifted sideways by its offset.
    fn stacked_line(&self, pm: &mut Canvas, parts: &[Vec<Coord>], s: &Symbol, strokes: &[LineStroke]) {
        let (mut pb, mut starts) = (PathBuilder::new(), DashStarts::default());
        for part in parts {
            self.ring_path_at(&mut pb, part, false, &mut starts);
        }
        let Some(path) = pb.finish() else { return };
        self.stacked_path(pm, &path, s, strokes, &starts, parts);
    }

    /// A stroke's dash pattern in pixels for `path`, or `None` to draw it solid: a pattern under a
    /// pixel a round, or one that would cut the path into more than [`MAX_DASHES`] dashes (each
    /// is a path of its own), would only cost time. The whole path counts, on screen or not: the
    /// dashing doesn't skip what's off screen.
    fn dash(&self, d: &rustgis_map::symbology::Dash, path: &tiny_skia::Path) -> Option<Vec<f32>> {
        let steps: Vec<f32> = d.even_steps().iter().map(|v| v * self.ppp).collect();
        let period: f32 = steps.iter().sum();
        // Each round of the pattern draws one dash per pair of steps.
        let dashes = path_length(path) / period * (steps.len() / 2) as f32;
        if !(period >= 1.0 && dashes <= MAX_DASHES) {
            return None;
        }
        Some(steps)
    }

    /// `starts`: where along its line each of `path`'s pieces begins ([`Self::ring_path_at`]), so a
    /// line cut at the clip box keeps its dashes where they'd be uncut. An offset stroke takes the
    /// same starts: exact on a straight line, a little off after bends, where the shifted line is
    /// longer or shorter than the line.
    ///
    /// `parts` is the line `path` was made from: markers along it are placed along the line as a
    /// whole ([`line_markers`]).
    fn stacked_path(
        &self,
        pm: &mut Canvas,
        path: &tiny_skia::Path,
        s: &Symbol,
        strokes: &[LineStroke],
        starts: &DashStarts,
        parts: &[Vec<Coord>],
    ) {
        // Shifted once per offset: a casing and its core usually share one.
        let mut shifted: Vec<(f32, Option<tiny_skia::Path>)> = Vec::new();
        // The line in device pixels, made for the first stroke of markers.
        let mut runs: Option<Vec<line_markers::Run>> = None;
        for st in strokes {
            let (c, w) = st.resolve(s);
            if let Some(m) = &st.markers {
                let runs = runs.get_or_insert_with(|| self.marker_runs(parts));
                line_markers::draw(pm, runs, m, c, if w.is_nan() { 0.0 } else { w }, st.offset, self.ppp);
                continue;
            }
            if w.is_nan() || w <= 0.0 || c.a() == 0 {
                continue;
            }
            if st.offset != 0.0 && !shifted.iter().any(|(o, _)| *o == st.offset) {
                shifted.push((st.offset, offset_path(path, st.offset * self.ppp)));
            }
            let drawn = match st.offset {
                0.0 => Some(path),
                o => shifted.iter().find(|(x, _)| *x == o).and_then(|(_, p)| p.as_ref()),
            };
            let Some(drawn) = drawn else { continue };
            let stroke = Stroke {
                width: (w * self.ppp).max(0.5),
                line_cap: match st.cap {
                    rustgis_map::LineCap::Round => LineCap::Round,
                    rustgis_map::LineCap::Butt => LineCap::Butt,
                    rustgis_map::LineCap::Square => LineCap::Square,
                },
                line_join: match st.join {
                    rustgis_map::LineJoin::Round => LineJoin::Round,
                    rustgis_map::LineJoin::Miter => LineJoin::Miter,
                    rustgis_map::LineJoin::Bevel => LineJoin::Bevel,
                },
                ..Stroke::default()
            };
            let steps = st.dash.and_then(|d| self.dash(&d, drawn));
            let period: f32 = steps.iter().flatten().sum();
            // A dashed line cut into pieces: each piece starts its pattern where its line is. Only
            // while every piece has its start: one off would shift every later piece's dashes.
            let pieces: Vec<(tiny_skia::Path, f32)> = match (&steps, starts.of(drawn)) {
                (Some(_), Some(at)) => {
                    let c = contours(drawn);
                    if c.len() == at.len() { c.into_iter().zip(at.iter().copied()).collect() } else { Vec::new() }
                }
                _ => Vec::new(),
            };
            match steps {
                Some(steps) if !pieces.is_empty() => {
                    for (piece, at) in &pieces {
                        let dash = tiny_skia::StrokeDash::new(steps.clone(), at.rem_euclid(period));
                        pm.stroke_path(piece, &paint(c, 1.0), &Stroke { dash, ..stroke.clone() }, Transform::identity(), None);
                    }
                }
                steps => {
                    let dash = steps.and_then(|steps| tiny_skia::StrokeDash::new(steps, 0.0));
                    pm.stroke_path(drawn, &paint(c, 1.0), &Stroke { dash, ..stroke }, Transform::identity(), None);
                }
            }
        }
    }

    /// A line's parts in device pixels, for markers along it: each broken where the projection
    /// can't show a point, as the line is drawn.
    fn marker_runs(&self, parts: &[Vec<Coord>]) -> Vec<line_markers::Run> {
        let ppp = self.view.pixels_per_point;
        let mut runs = Vec::new();
        if let Some(g) = &self.globe {
            // On the globe, along the same bent, horizon-cut points as `globe_line` draws.
            let h = g.horizon();
            let px = |v: V3| {
                let (x, y) = g.project(v);
                (x * ppp, y * ppp)
            };
            for part in parts {
                let mut run: Vec<(f64, f64)> = Vec::new();
                let mut prev: Option<V3> = None;
                for v in Self::globe_vectors(g, part) {
                    let vis = v[2] > h;
                    match prev {
                        Some(p) if p[2] > h && !vis => {
                            run.push(px(g.horizon_crossing(p, v)));
                            runs.extend(line_markers::Run::new(std::mem::take(&mut run)));
                        }
                        Some(p) if p[2] <= h && vis => run.extend([px(g.horizon_crossing(p, v)), px(v)]),
                        _ if vis => run.push(px(v)),
                        _ => {}
                    }
                    prev = Some(v);
                }
                runs.extend(line_markers::Run::new(run));
            }
            return runs;
        }
        for part in parts {
            let pts: Vec<Option<(f64, f64)>> = part.iter().map(|c| self.screen(*c).map(|(x, y)| (x * ppp, y * ppp))).collect();
            for run in pts.split(|p| p.is_none_or(|(x, y)| !x.is_finite() || !y.is_finite())) {
                runs.extend(line_markers::Run::new(run.iter().flatten().copied()));
            }
        }
        runs
    }

    /// Draws `s`'s marker at `c`, at `opacity`, ringed by `halo` (a selection); nothing off the image.
    fn marker(&self, pm: &mut Canvas, c: Coord, s: &Symbol, opacity: f32, halo: Option<Rgba>) {
        let (x, y) = self.px(c);
        if !x.is_finite() || !y.is_finite() {
            return; // behind the globe
        }
        // Off the image by more than any marker's reach: nothing to draw.
        let reach = (s.size.max(1.0) * 0.6 + s.stroke_width.max(0.0) + 4.0) * self.ppp;
        if x < -reach || y < -reach || x > pm.width() as f32 + reach || y > pm.height() as f32 + reach {
            return;
        }
        draw_marker(pm, x, y, s, opacity, halo, self.ppp);
    }

    /// A layer's point marker, stamped from a sprite drawn once per symbol and quarter-pixel
    /// offset: a dense point layer otherwise fills and strokes a path for every point.
    fn stamp(&self, pm: &mut Canvas, c: Coord, s: &Symbol) {
        let (x, y) = self.px(c);
        if !x.is_finite() || !y.is_finite() {
            return; // behind the globe
        }
        let (fx, fy) = (x.floor(), y.floor());
        let (qx, qy) = (((x - fx) * 4.0).round(), ((y - fy) * 4.0).round());
        let (ix, iy) = (fx as i64 + (qx >= 4.0) as i64, fy as i64 + (qy >= 4.0) as i64);
        let (qx, qy) = ((qx as u8) % 4, (qy as u8) % 4);
        let reach = ((s.size.max(1.0) * marker_reach(s.marker) + s.stroke_width.max(0.0)) * self.ppp).ceil() as i64 + 3;
        if ix < -reach || iy < -reach || ix > pm.width() as i64 + reach || iy > pm.height() as i64 + reach {
            return;
        }
        // A recording keeps the marker itself, not a sprite's pixels.
        if pm.is_recording() {
            draw_marker(pm, x, y, s, 1.0, None, self.ppp);
            return;
        }
        let key = (s.fill, s.stroke, s.stroke_width.to_bits(), s.size.to_bits(), s.marker, qx, qy);
        let mut sprites = self.sprites.borrow_mut();
        if !sprites.contains_key(&key) {
            // Many symbols (sizes by value, say) would each make a sprite used once: draw directly.
            if sprites.len() >= MAX_SPRITES || reach > MAX_SPRITE_REACH {
                drop(sprites);
                draw_marker(pm, x, y, s, 1.0, None, self.ppp);
                return;
            }
            let side = (2 * reach + 1) as u32;
            let sprite = Canvas::new(side, side).map(|mut sp| {
                let c = reach as f32 + qx as f32 / 4.0;
                let d = reach as f32 + qy as f32 / 4.0;
                draw_marker(&mut sp, c, d, s, 1.0, None, self.ppp);
                sp.into_pixmap()
            });
            sprites.insert(key, sprite);
        }
        if let Some(Some(sp)) = sprites.get(&key) {
            blend_over(pm.pixels(), sp, ix - reach, iy - reach);
        }
    }

    /// Draws a geometry with a plain symbol: points stamped, lines stroked, polygons filled and
    /// outlined (outlined only in a wireframe view).
    fn feature(&self, pm: &mut Canvas, g: &Geometry, s: &Symbol) {
        match g {
            Geometry::Point(c) => self.stamp(pm, *c, s),
            Geometry::MultiPoint(v) => v.iter().for_each(|c| self.stamp(pm, *c, s)),
            Geometry::Line(parts) => self.line(pm, parts, &paint(s.stroke, 1.0), s.stroke_width),
            Geometry::Polygon(polys) if self.wireframe => {
                self.polygon(pm, polys, None, Some((paint(s.stroke.with_alpha(255), 1.0), s.stroke_width.max(1.0))))
            }
            // A small filled polygon's outline may be mitred; one without a fill is its outline
            // alone, which keeps its round joins.
            Geometry::Polygon(polys) if s.fill.a() > 0 => {
                let Some(path) = self.polygon_path(polys) else { return };
                pm.fill_path(&path, &paint(s.fill, 1.0), FillRule::EvenOdd, Transform::identity(), None);
                self.fill_outline(pm, &path, &paint(s.stroke, 1.0), s.stroke_width);
            }
            Geometry::Polygon(polys) => self.polygon(pm, polys, None, Some((paint(s.stroke, 1.0), s.stroke_width))),
        }
    }

    /// Draws a line or polygon smaller than a device pixel across as the coverage its fill and
    /// outline would have, spread over the pixels around its centre. `false` (nothing drawn) for
    /// a bigger feature or a point.
    fn splat(&self, pm: &mut Canvas, e: &Extent, g: &Geometry, s: &Symbol) -> bool {
        // A recording keeps even the smallest feature as its shape.
        if pm.is_recording() {
            return false;
        }
        let k = self.view.pixels_per_point / self.view.camera.res.max(1e-12);
        let (sx, sy) = ((e.width() * k) as f32, (e.height() * k) as f32);
        if !(sx.is_finite() && sy.is_finite()) || sx > 1.0 || sy > 1.0 || e.is_empty() {
            return false;
        }
        let (stroke, width, fill) = match g {
            Geometry::Point(_) | Geometry::MultiPoint(_) => return false,
            Geometry::Line(_) => (s.stroke, (s.stroke_width * self.ppp).max(0.5), None),
            Geometry::Polygon(_) if self.wireframe => (s.stroke.with_alpha(255), s.stroke_width.max(1.0) * self.ppp, None),
            Geometry::Polygon(_) => (s.stroke, if s.stroke.a() > 0 { s.stroke_width.max(0.0) * self.ppp } else { 0.0 }, Some(s.fill)),
        };
        // With its outline, still about a pixel: a thick outline drawn as a dot would lose its ink.
        if sx + width > 2.0 || sy + width > 2.0 {
            return false;
        }
        let (cx, cy) = self.px(Coord::new((e.min_x + e.max_x) / 2.0, (e.min_y + e.max_y) / 2.0));
        // The outline covers the shape grown by half its width all round (round joins); the
        // fill shows inside it.
        let inner = (sx - width).max(0.0) * (sy - width).max(0.0);
        let outer = if width > 0.0 {
            let hw = width / 2.0;
            match g {
                Geometry::Line(_) => (sx.hypot(sy) + width) * width,
                _ => (sx + width) * (sy + width) - (4.0 - std::f32::consts::PI) * hw * hw,
            }
        } else {
            0.0
        };
        if let Some(f) = fill {
            let area = if width > 0.0 { inner } else { sx * sy };
            splat_color(pm.pixels(), cx, cy, f, area);
        }
        if width > 0.0 {
            splat_color(pm.pixels(), cx, cy, stroke, outer - inner);
        }
        true
    }

    /// Draw a near-rectangular filled polygon up to 1.5 px (a rectangle up to 2 px) by coverage.
    /// Other shapes keep their paths: an extent is not a substitute for a hole or an inset.
    fn small_polygon(&self, pm: &mut Canvas, e: &Extent, g: &Geometry, s: &Symbol) -> bool {
        let Geometry::Polygon(polys) = g else { return false };
        if pm.is_recording()
            || self.wireframe
            || self.globe.is_some()
            || self.view.camera.bearing != 0.0
            || self.view.camera.pitch != 0.0
            || s.fill.a() != 255
            || s.stroke.a() != 255
            || !self.ppp.is_finite()
            || self.ppp <= 0.0
            || !self.view.camera.res.is_finite()
            || self.view.camera.res <= 0.0
            || e.is_empty()
        {
            return false;
        }
        let k = self.view.pixels_per_point / self.view.camera.res.max(1e-12);
        let (sx, sy) = (e.width() * k, e.height() * k);
        let width = s.stroke_width.max(0.0) * self.ppp;
        // Hairlines use a different rasterizer; unoutlined shapes already draw cheaply.
        if !(sx.is_finite() && sy.is_finite() && width.is_finite())
            // `emit_run` can drop an edge this short; use the real path in that case.
            || sx <= 0.35
            || sy <= 0.35
            || sx > 2.0
            || sy > 2.0
            || width <= 1.0
            || width > 2.0
        {
            return false;
        }
        if !antialiased() {
            return false;
        }
        let [rings] = polys.as_slice() else { return false };
        let [ring] = rings.as_slice() else { return false };
        let Some(first) = ring.first().copied() else { return false };
        if ring.len() < 4 || ring.last() != Some(&first) {
            return false;
        }
        // Work relative to one vertex: small rings in projected coordinates can be far from zero.
        let (mut twice_area, mut perimeter, mut on_box) = (0.0, 0.0, true);
        let (mut left, mut right, mut bottom, mut top) = (false, false, false, false);
        // Each segment must stay near one box edge, including short diagonals from thinning.
        let margin = e.width().min(e.height()) * 0.15;
        let inside = |p: Coord| p.x >= e.min_x && p.x <= e.max_x && p.y >= e.min_y && p.y <= e.max_y;
        for pair in ring.windows(2) {
            let [a, b] = pair else { continue };
            if !a.is_finite() || !b.is_finite() {
                return false;
            }
            let shares_edge = (a.x - e.min_x <= margin && b.x - e.min_x <= margin)
                || (e.max_x - a.x <= margin && e.max_x - b.x <= margin)
                || (a.y - e.min_y <= margin && b.y - e.min_y <= margin)
                || (e.max_y - a.y <= margin && e.max_y - b.y <= margin);
            if !inside(*a) || !inside(*b) || !shares_edge {
                return false;
            }
            let (l, r, bot, t) = (a.x == e.min_x, a.x == e.max_x, a.y == e.min_y, a.y == e.max_y);
            left |= l;
            right |= r;
            bottom |= bot;
            top |= t;
            on_box &= l || r || bot || t;
            twice_area += (a.x - first.x) * (b.y - first.y) - (b.x - first.x) * (a.y - first.y);
            perimeter += (a.x - b.x).abs() + (a.y - b.y).abs();
        }
        let box_area = e.width() * e.height();
        let share = twice_area.abs() / (2.0 * box_area);
        // Extents come from the unthinned geometry; don't draw a box bigger than this ring.
        if !(left && right && bottom && top) {
            return false;
        }
        // Keep at least 80% of the box's area and at most 20% extra perimeter.
        if !share.is_finite() || !(0.8..=1.0).contains(&share) || perimeter > 2.4 * (e.width() + e.height()) {
            return false;
        }
        // At the larger size, an inset in an outline can be visible: use a full box only.
        if (sx > 1.5 || sy > 1.5) && (!on_box || share < 1.0 - 1e-10) {
            return false;
        }
        let (x0, y0) = self.px(Coord::new(e.min_x, e.max_y));
        let (x1, y1) = self.px(Coord::new(e.max_x, e.min_y));
        if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
            return false;
        }
        small_polygon_pixels(pm.pixels(), [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)], width, s.fill, s.stroke, share as f32);
        true
    }

    /// A point's marks, or its marker turned ([`point_marks::draw`]), unless it lies off the picture.
    fn marks(&self, pm: &mut Canvas, c: Coord, s: &Symbol, strokes: &[LineStroke], turn: f32, halo: Option<Rgba>) {
        let (x, y) = self.px(c);
        let reach = (point_marks::reach(s, strokes) + 4.0) * self.ppp;
        if !(x.is_finite() && y.is_finite()) || x < -reach || y < -reach || x > pm.width() as f32 + reach || y > pm.height() as f32 + reach
        {
            return;
        }
        point_marks::draw(pm, x, y, s, strokes, turn, halo, self.ppp);
    }

    /// Draws a selected feature in the selection colour: a halo under its strokes or marks, or
    /// its outline.
    fn highlight(&self, pm: &mut Canvas, g: &Geometry, s: &Symbol, strokes: &[LineStroke], turn: f32) {
        let cyan = paint(SELECTION, 1.0);
        match g {
            Geometry::Line(parts) if !strokes.is_empty() => {
                // A halo around each stroke that shows, under them all.
                let halo: Vec<LineStroke> = strokes
                    .iter()
                    .filter(|st| {
                        let (c, w) = st.resolve(s);
                        // A hollow marker shows by its outline alone.
                        let outlined = w > 0.0
                            && st
                                .markers
                                .is_some_and(|m| !matches!(m.mark, rustgis_map::LineMark::Hash) && m.outline.is_some_and(|o| o.a() > 0));
                        (w > 0.0 || st.markers.is_some_and(|m| !matches!(m.mark, rustgis_map::LineMark::Hash))) && (c.a() > 0 || outlined)
                    })
                    .map(|st| LineStroke {
                        // A hollow marker's halo is its outline alone, so it stays hollow.
                        color: Some(if st.markers.is_some() && st.resolve(s).0.a() == 0 { Rgba::new(0, 0, 0, 0) } else { SELECTION }),
                        width: Some(st.resolve(s).1.max(0.0) + 3.0),
                        // A marker's halo is its outline.
                        markers: st.markers.map(|m| rustgis_map::LineMarkers { outline: Some(SELECTION), ..m }),
                        ..*st
                    })
                    .collect();
                self.stacked_line(pm, parts, s, &halo);
                self.stacked_line(pm, parts, s, strokes);
            }
            Geometry::Point(c) if !point_marks::plain(strokes, turn) => self.marks(pm, *c, s, strokes, turn, Some(SELECTION)),
            Geometry::MultiPoint(v) if !point_marks::plain(strokes, turn) => {
                v.iter().for_each(|c| self.marks(pm, *c, s, strokes, turn, Some(SELECTION)))
            }
            Geometry::Point(c) => self.marker(pm, *c, s, 1.0, Some(SELECTION)),
            Geometry::MultiPoint(v) => v.iter().for_each(|c| self.marker(pm, *c, s, 1.0, Some(SELECTION))),
            Geometry::Line(parts) => {
                self.line(pm, parts, &cyan, s.stroke_width + 3.0);
                self.line(pm, parts, &paint(s.stroke, 1.0), s.stroke_width);
            }
            Geometry::Polygon(polys) => self.polygon(pm, polys, Some(paint(SELECTION.with_alpha(40), 1.0)), Some((cyan, 2.0))),
        }
    }
}

/// The length of `path` in pixels, at most a little over: a curve counts as its control polygon,
/// which is never shorter than the curve.
fn path_length(path: &tiny_skia::Path) -> f32 {
    let (mut len, mut last, mut start) = (0.0, None, None);
    for seg in path.segments() {
        let pts = match seg {
            tiny_skia::PathSegment::MoveTo(p) => {
                (last, start) = (Some(p), Some(p));
                continue;
            }
            tiny_skia::PathSegment::LineTo(p) => [p, p, p],
            tiny_skia::PathSegment::QuadTo(a, p) => [a, p, p],
            tiny_skia::PathSegment::CubicTo(a, b, p) => [a, b, p],
            tiny_skia::PathSegment::Close => match start {
                Some(p) => [p, p, p],
                None => continue,
            },
        };
        if let Some(mut q) = last {
            for p in pts {
                len += q.distance(p);
                q = p;
            }
        }
        last = Some(pts[2]);
    }
    len
}

/// `path`'s polylines shifted `d` pixels to the right of their direction (screen y down).
fn offset_path(path: &tiny_skia::Path, d: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let mut run: Vec<(f32, f32)> = Vec::new();
    let mut flush = |run: &mut Vec<(f32, f32)>| {
        let pts = offset_polyline(run, d);
        if let Some((first, rest)) = pts.split_first() {
            pb.move_to(first.0, first.1);
            for p in rest {
                pb.line_to(p.0, p.1);
            }
        }
        run.clear();
    };
    for seg in path.segments() {
        match seg {
            tiny_skia::PathSegment::MoveTo(p) => {
                flush(&mut run);
                run.push((p.x, p.y));
            }
            tiny_skia::PathSegment::LineTo(p) | tiny_skia::PathSegment::QuadTo(_, p) | tiny_skia::PathSegment::CubicTo(_, _, p) => {
                run.push((p.x, p.y))
            }
            tiny_skia::PathSegment::Close => {}
        }
    }
    flush(&mut run);
    pb.finish()
}

/// A polyline shifted `d` to the right of its direction (screen y down; negative `d` shifts
/// it left). Corners are mitred, and cut back to a bevel where the mitre would reach beyond
/// four times the offset (a sharp turn). Repeated points are skipped; fewer than two distinct
/// points give nothing.
/// Like any offset that moves each vertex on its own, an offset larger than the segments on the
/// inside of a bend can fold back into a small loop there.
fn offset_polyline(pts: &[(f32, f32)], d: f32) -> Vec<(f32, f32)> {
    let mut p: Vec<(f32, f32)> = Vec::with_capacity(pts.len());
    for &q in pts.iter().filter(|q| q.0.is_finite() && q.1.is_finite()) {
        if p.last().is_none_or(|l: &(f32, f32)| (l.0 - q.0).hypot(l.1 - q.1) > 1e-3) {
            p.push(q);
        }
    }
    // The unit normal to the right of a → b.
    let normal = |a: (f32, f32), b: (f32, f32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = dx.hypot(dy);
        (-dy / l, dx / l)
    };
    let mut out = Vec::with_capacity(p.len() + 4);
    let (Some(&[a, b]), Some(&z)) = (p.first_chunk::<2>(), p.last()) else { return out };
    // Carry the previous segment normal forward; each segment is evaluated once.
    let mut n = normal(a, b);
    out.push((a.0 + n.0 * d, a.1 + n.1 * d));
    for w in p.windows(3) {
        let [v, b] = [w.get(1), w.get(2)].map(|x| x.copied().unwrap_or_default());
        let (na, nb) = (n, normal(v, b));
        n = nb;
        let (sx, sy) = (na.0 + nb.0, na.1 + nb.1);
        let s2 = sx * sx + sy * sy;
        if s2 >= 0.25 {
            // The mitre: along the bisector, 2d / |na + nb|² times na + nb.
            let k = 2.0 * d / s2;
            out.push((v.0 + sx * k, v.1 + sy * k));
        } else {
            out.push((v.0 + na.0 * d, v.1 + na.1 * d));
            out.push((v.0 + nb.0 * d, v.1 + nb.1 * d));
        }
    }
    out.push((z.0 + n.0 * d, z.1 + n.1 * d));
    out
}

/// Adds one run of device-pixel points to a path, dropping points closer than ~0.35 px to the
/// last one kept (a big speed-up zoomed out), but always the last.
fn emit_run(pb: &mut PathBuilder, pts: impl ExactSizeIterator<Item = (f64, f64)>, close: bool) {
    let n = pts.len();
    let mut last: Option<(f32, f32)> = None;
    for (i, (x, y)) in pts.enumerate() {
        let (x, y) = (x as f32, y as f32);
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        match last {
            None => pb.move_to(x, y),
            Some((lx, ly)) => {
                if (x - lx).abs() + (y - ly).abs() < 0.35 && i + 1 != n {
                    continue;
                }
                pb.line_to(x, y)
            }
        }
        last = Some((x, y));
    }
    if close && last.is_some() {
        pb.close();
    }
}

/// A ring clipped to a box (Sutherland–Hodgman): the part inside it, closed along its edges.
fn clip_ring(pts: &[(f64, f64)], b: &Extent) -> Vec<(f64, f64)> {
    let mut cur: Vec<(f64, f64)> = pts.to_vec();
    // Each edge: is a point inside, and where a segment crosses it.
    for edge in 0..4 {
        if cur.is_empty() {
            break;
        }
        let inside = |p: (f64, f64)| match edge {
            0 => p.0 >= b.min_x,
            1 => p.0 <= b.max_x,
            2 => p.1 >= b.min_y,
            _ => p.1 <= b.max_y,
        };
        let cross = |p: (f64, f64), q: (f64, f64)| {
            let (v, t) = match edge {
                0 => (b.min_x, (b.min_x - p.0) / (q.0 - p.0)),
                1 => (b.max_x, (b.max_x - p.0) / (q.0 - p.0)),
                2 => (b.min_y, (b.min_y - p.1) / (q.1 - p.1)),
                _ => (b.max_y, (b.max_y - p.1) / (q.1 - p.1)),
            };
            if edge < 2 { (v, p.1 + (q.1 - p.1) * t) } else { (p.0 + (q.0 - p.0) * t, v) }
        };
        let mut out = Vec::with_capacity(cur.len() + 4);
        let mut prev = cur.last().copied().unwrap_or((0.0, 0.0));
        for &p in &cur {
            match (inside(prev), inside(p)) {
                (true, true) => out.push(p),
                (true, false) => out.push(cross(prev, p)),
                (false, true) => {
                    out.push(cross(prev, p));
                    out.push(p);
                }
                (false, false) => {}
            }
            prev = p;
        }
        cur = out;
    }
    if cur.len() < 3 { Vec::new() } else { cur }
}

/// The parts of a line inside a box (each segment clipped by Liang–Barsky), each with how far
/// along the line it starts.
fn clip_line(pts: &[(f64, f64)], b: &Extent) -> Vec<(f64, Vec<(f64, f64)>)> {
    let mut parts: Vec<(f64, Vec<(f64, f64)>)> = Vec::new();
    let mut cur: Vec<(f64, f64)> = Vec::new();
    // Length of the line before this segment, and where the part being built starts.
    let (mut along, mut start) = (0.0f64, 0.0f64);
    for w in pts.windows(2) {
        let (Some(&p), Some(&q)) = (w.first(), w.get(1)) else { continue };
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        let len = dx.hypot(dy);
        let before = along;
        along += len;
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let mut keep = true;
        for (num, den) in [(p.0 - b.min_x, -dx), (b.max_x - p.0, dx), (p.1 - b.min_y, -dy), (b.max_y - p.1, dy)] {
            if den == 0.0 {
                if num < 0.0 {
                    keep = false;
                    break;
                }
            } else {
                let t = num / den;
                if den < 0.0 {
                    t0 = t0.max(t);
                } else {
                    t1 = t1.min(t);
                }
            }
        }
        if !keep || t0 > t1 {
            if cur.len() > 1 {
                parts.push((start, std::mem::take(&mut cur)));
            }
            cur.clear();
            continue;
        }
        let a = (p.0 + dx * t0, p.1 + dy * t0);
        let z = (p.0 + dx * t1, p.1 + dy * t1);
        if cur.is_empty() || t0 > 0.0 {
            if cur.len() > 1 {
                parts.push((start, std::mem::take(&mut cur)));
            }
            cur = vec![a];
            start = before + len * t0;
        }
        cur.push(z);
        if t1 < 1.0 {
            parts.push((start, std::mem::take(&mut cur)));
        }
    }
    if cur.len() > 1 {
        parts.push((start, cur));
    }
    parts
}

/// Where along its line each piece of a path begins, in device pixels, as [`Ctx::ring_path_at`]
/// adds them. Nothing is kept while every piece begins at its line's start (nothing was cut):
/// pieces starting at 0, 0 keep `at` empty; a third starting at 41 makes it [0, 0, 41]. `pieces`
/// counts every piece pushed, so the zeros before the first cut can be filled in then.
#[derive(Default)]
pub(crate) struct DashStarts {
    pieces: usize,
    at: Vec<f32>,
}

impl DashStarts {
    fn push(&mut self, at: f32) {
        if self.at.is_empty() && at != 0.0 {
            self.at.resize(self.pieces, 0.0);
        }
        if !self.at.is_empty() || at != 0.0 {
            self.at.push(at);
        }
        self.pieces += 1;
    }

    /// The starts for `path`'s pieces, when some piece doesn't begin at its line's start and they
    /// match the path's pieces one to one.
    fn of(&self, path: &tiny_skia::Path) -> Option<&[f32]> {
        let n = path.segments().filter(|s| matches!(s, tiny_skia::PathSegment::MoveTo(_))).count();
        (!self.at.is_empty() && self.at.len() == n).then_some(self.at.as_slice())
    }
}

/// A path's pieces (from each move-to), each a path of its own.
fn contours(path: &tiny_skia::Path) -> Vec<tiny_skia::Path> {
    let mut out = Vec::new();
    let mut pb = PathBuilder::new();
    for seg in path.segments() {
        match seg {
            tiny_skia::PathSegment::MoveTo(p) => {
                if let Some(done) = std::mem::replace(&mut pb, PathBuilder::new()).finish() {
                    out.push(done);
                }
                pb.move_to(p.x, p.y);
            }
            tiny_skia::PathSegment::LineTo(p) => pb.line_to(p.x, p.y),
            tiny_skia::PathSegment::QuadTo(a, p) => pb.quad_to(a.x, a.y, p.x, p.y),
            tiny_skia::PathSegment::CubicTo(a, b, p) => pb.cubic_to(a.x, a.y, b.x, b.y, p.x, p.y),
            tiny_skia::PathSegment::Close => pb.close(),
        }
    }
    if let Some(done) = pb.finish() {
        out.push(done);
    }
    out
}

/// Blends `color` at `coverage` (in pixels, spread bilinearly) around (`cx`, `cy`) device pixels.
fn splat_color(pm: &mut Pixmap, cx: f32, cy: f32, color: Rgba, coverage: f32) {
    if color.a() == 0 || coverage.is_nan() || coverage <= 0.0 || !cx.is_finite() || !cy.is_finite() {
        return;
    }
    let (x, y) = (cx - 0.5, cy - 0.5);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let (ix, iy) = (x.floor() as i64, y.floor() as i64);
    let (w, h) = (pm.width() as i64, pm.height() as i64);
    let data = pm.data_mut();
    for (dx, dy, wt) in [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)] {
        let (px, py) = (ix + dx, iy + dy);
        if px < 0 || py < 0 || px >= w || py >= h {
            continue;
        }
        let a = (color.a() as f32 * (coverage * wt).min(1.0)).round() as u16;
        if a == 0 {
            continue;
        }
        let i = ((py * w + px) * 4) as usize;
        let Some(d) = data.get_mut(i..i + 4) else { continue };
        let inv = 255 - a;
        let src = [color.r(), color.g(), color.b()];
        for (dc, sc) in d.iter_mut().zip(src) {
            *dc = ((sc as u16 * a + *dc as u16 * inv + 127) / 255) as u8;
        }
        d[3] = (a + (d[3] as u16 * inv + 127) / 255).min(255) as u8;
    }
}

/// Coverage of a tiny box and its mitred outline in each touched device pixel.
fn small_polygon_pixels(pm: &mut Pixmap, b: [f32; 4], width: f32, fill: Rgba, stroke: Rgba, share: f32) {
    if b[2] + width < 0.0 || b[3] + width < 0.0 || b[0] - width >= pm.width() as f32 || b[1] - width >= pm.height() as f32 {
        return;
    }
    // tiny-skia's path rasterizer samples at quarter-pixel intervals.
    fn snap(b: [f32; 4]) -> [f32; 4] {
        b.map(|n| (n * 4.0).round() / 4.0)
    }
    fn coverage(b: [f32; 4], x: f32, y: f32) -> f32 {
        (b[2].min(x + 1.0) - b[0].max(x)).max(0.0) * (b[3].min(y + 1.0) - b[1].max(y)).max(0.0)
    }
    fn blend(d: &mut [u8], color: Rgba, coverage: f32) {
        let a = (color.a() as f32 * coverage.clamp(0.0, 1.0)).round() as u16;
        let inv = 255 - a;
        for (dc, sc) in d.iter_mut().zip([color.r(), color.g(), color.b()]) {
            *dc = ((sc as u16 * a + *dc as u16 * inv + 127) / 255) as u8;
        }
        d[3] = (a + (d[3] as u16 * inv + 127) / 255).min(255) as u8;
    }
    let hw = width / 2.0;
    let outer = snap([b[0] - hw, b[1] - hw, b[2] + hw, b[3] + hw]);
    let inner = snap([b[0] + hw, b[1] + hw, b[2] - hw, b[3] - hw]);
    let b = snap(b);
    let (w, h) = (pm.width() as i64, pm.height() as i64);
    let (x0, y0) = ((outer[0].floor() as i64).clamp(0, w), (outer[1].floor() as i64).clamp(0, h));
    let (x1, y1) = ((outer[2].ceil() as i64).clamp(0, w), (outer[3].ceil() as i64).clamp(0, h));
    let data = pm.data_mut();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            let Some(d) = data.get_mut(i..i + 4) else { continue };
            let (x, y) = (x as f32, y as f32);
            blend(d, fill, coverage(b, x, y) * share);
            blend(d, stroke, coverage(outer, x, y) - coverage(inner, x, y));
        }
    }
}

/// Most marker sprites one render keeps.
const MAX_SPRITES: usize = 1024;

/// Largest marker reach (device pixels from its centre) drawn from a sprite: a bigger marker
/// gains little from one. With [`MAX_SPRITES`], the cache stays under about 10 MB.
const MAX_SPRITE_REACH: i64 = 24;

/// A marker's outline at (`x`, `y`), `r` its radius in pixels.
pub(crate) fn marker_path(x: f32, y: f32, r: f32, marker: Marker) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    match marker {
        Marker::Circle => pb.push_circle(x, y, r),
        Marker::Square => {
            if let Some(rect) = tiny_skia::Rect::from_xywh(x - r * 0.9, y - r * 0.9, r * 1.8, r * 1.8) {
                pb.push_rect(rect)
            }
        }
        Marker::Triangle => {
            pb.move_to(x, y - r * 1.1);
            pb.line_to(x + r, y + r * 0.75);
            pb.line_to(x - r, y + r * 0.75);
            pb.close();
        }
        Marker::Diamond => {
            pb.move_to(x, y - r * 1.15);
            pb.line_to(x + r * 0.9, y);
            pb.line_to(x, y + r * 1.15);
            pb.line_to(x - r * 0.9, y);
            pb.close();
        }
        Marker::Star => {
            for i in 0..10 {
                let a = std::f32::consts::PI * (i as f32) / 5.0 - std::f32::consts::FRAC_PI_2;
                let rr = if i % 2 == 0 { r * 1.2 } else { r * 0.5 };
                let (px, py) = (x + rr * a.cos(), y + rr * a.sin());
                if i == 0 { pb.move_to(px, py) } else { pb.line_to(px, py) }
            }
            pb.close();
        }
        Marker::Char(g) => match glyphs::shape(g) {
            Some(s) => {
                let em = 2.0 * r;
                return s.path.clone().transform(Transform::from_row(em, 0.0, 0.0, em, x, y));
            }
            // A space draws nothing; a character no font here has, a circle of its size, so the
            // point still shows.
            None if g.ch.is_whitespace() => return None,
            None => pb.push_circle(x, y, r),
        },
    }
    pb.finish()
}

/// How far a `marker` reaches from its centre, in its sizes: a shape (a star's points) 0.6, a
/// character as far as its outline does.
pub(crate) fn marker_reach(marker: Marker) -> f32 {
    match marker {
        Marker::Char(g) => glyphs::shape(g).map_or(0.6, |s| s.reach),
        _ => 0.6,
    }
}

/// A marker centred at (`x`, `y`) device pixels.
fn draw_marker(pm: &mut Canvas, x: f32, y: f32, s: &Symbol, opacity: f32, halo: Option<Rgba>, ppp: f32) {
    let r = (s.size.max(1.0) * ppp) / 2.0;
    let Some(path) = marker_path(x, y, r, s.marker) else { return };
    if let Some(h) = halo {
        let st = Stroke { width: 3.5 * ppp, line_join: LineJoin::Round, ..Stroke::default() };
        pm.stroke_path(&path, &paint(h, 1.0), &st, Transform::identity(), None);
    }
    pm.fill_path(&path, &paint(s.fill, opacity), FillRule::Winding, Transform::identity(), None);
    if s.stroke_width > 0.0 && s.stroke.a() > 0 {
        let st = Stroke { width: s.stroke_width * ppp, line_join: LineJoin::Round, ..Stroke::default() };
        pm.stroke_path(&path, &paint(s.stroke, opacity), &st, Transform::identity(), None);
    }
}

/// Composites premultiplied `src` over `dst` with its top left corner at (`x`, `y`).
fn blend_over(dst: &mut Pixmap, src: &Pixmap, x: i64, y: i64) {
    let (dw, dh) = (dst.width() as i64, dst.height() as i64);
    let (sw, sh) = (src.width() as i64, src.height() as i64);
    let (x0, x1) = (x.max(0), (x + sw).min(dw));
    let (y0, y1) = (y.max(0), (y + sh).min(dh));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (s, d) = (src.data(), dst.data_mut());
    let n = ((x1 - x0) * 4) as usize;
    for row in y0..y1 {
        let si = (((row - y) * sw + (x0 - x)) * 4) as usize;
        let di = ((row * dw + x0) * 4) as usize;
        let (Some(sr), Some(dr)) = (s.get(si..si + n), d.get_mut(di..di + n)) else { continue };
        for (sp, dp) in sr.as_chunks::<4>().0.iter().zip(dr.as_chunks_mut::<4>().0.iter_mut()) {
            let sa = sp[3];
            if sa == 0 {
                continue;
            }
            if sa == 255 {
                dp.copy_from_slice(sp);
                continue;
            }
            let inv = 255 - sa as u16;
            for (dc, sc) in dp.iter_mut().zip(sp) {
                *dc = (*sc as u16 + (*dc as u16 * inv + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// The longitude/latitude extent a view shows (the whole world on the globe, or when the
/// projection can't say).
pub fn view_lonlat(map: &Map, view: &View) -> Extent {
    if map.projection.is_globe() {
        return Extent::WORLD_LONLAT;
    }
    let flat = map.flat();
    let e = view.camera.extent(view.width, view.height);
    let mut out = Extent::EMPTY;
    const N: usize = 8;
    for i in 0..=N {
        let t = i as f64 / N as f64;
        let (x, y) = (e.min_x + t * e.width(), e.min_y + t * e.height());
        for c in [Coord::new(x, e.min_y), Coord::new(x, e.max_y), Coord::new(e.min_x, y), Coord::new(e.max_x, y)] {
            match flat.inverse(c) {
                Some(p) if p.is_finite() => out.expand(p),
                _ => return Extent::WORLD_LONLAT,
            }
        }
    }
    // A pole inside the view lies beyond every edge sample (a polar projection): reach it.
    for pole in [Coord::new(0.0, 90.0), Coord::new(0.0, -90.0)] {
        if let Some(p) = flat.forward(pole)
            && p.is_finite()
            && e.contains(p)
        {
            out = Extent::new(-180.0, out.min_y.min(pole.y), 180.0, out.max_y.max(pole.y));
        }
    }
    if out.is_empty() { Extent::WORLD_LONLAT } else { out }
}

/// Draws the map for the screen. Layers are drawn bottom-up (the last layer in the list first).
/// On high-density screens rasters are computed at logical-pixel resolution, for speed.
pub fn render(map: &mut Map, view: &View) -> Option<Rendered> {
    render_with(map, view, false)
}

/// [`render_with_backend`] on the CPU, as pixels.
fn render_with(map: &mut Map, view: &View, full_res_rasters: bool) -> Option<Rendered> {
    render_cpu(map, view, full_res_rasters, false)
}

/// [`render_with`] on the CPU, kept as vectors where it can be (`record`, see
/// [`render_frame`]).
fn render_cpu(map: &mut Map, view: &View, full_res_rasters: bool, record: bool) -> Option<Rendered> {
    match render_any(map, view, full_res_rasters, Backend::Cpu, record) {
        Some(RenderOutput::Cpu(r)) => Some(r),
        // The Cpu backend never produces RenderOutput::Gpu.
        Some(RenderOutput::Gpu(_)) | None => None,
    }
}

/// Flushes the GPU pass's CPU accumulator pixmap as an Image draw command and resets it to
/// transparent. Basemap, tiles, rasters and tiny-skia-only layers draw into the accumulator
/// between GPU mesh commands, preserving the exact painter's-algorithm order.
pub(crate) fn flush_acc(pm: &mut Pixmap, cmds: &mut Vec<gpu_scene::DrawCmd>, dirty: &mut bool) -> Option<()> {
    if *dirty {
        // Move the pixmap into the command (no full-screen copy) and start a fresh transparent
        // accumulator.
        let old = std::mem::replace(pm, Pixmap::new(pm.width(), pm.height())?);
        cmds.push(gpu_scene::DrawCmd::Image { pixmap: old });
        *dirty = false;
    }
    Some(())
}

/// The shared render body behind [`render`], [`render_backend`] and the export paths.
/// `full_res_rasters` renders rasters at full resolution (exports); `backend` selects the CPU
/// pixmap or a GPU scene (see [`Backend`]).
fn render_with_backend(map: &mut Map, view: &View, full_res_rasters: bool, backend: Backend) -> Option<RenderOutput> {
    render_any(map, view, full_res_rasters, backend, false)
}

/// [`render_with_backend`], `record` keeping a flat, north-up frame as vectors where it can
/// ([`render_frame`]).
fn render_any(map: &mut Map, view: &View, full_res_rasters: bool, backend: Backend, record: bool) -> Option<RenderOutput> {
    // A bearing or pitch set directly on the camera may be unnormalised (or no number, which
    // faces north and looks straight down).
    let mut camera = view.camera;
    camera.bearing = rustgis_map::camera::normalize_bearing(camera.bearing);
    camera.pitch = rustgis_map::camera::normalize_pitch(camera.pitch);
    // A persisted Shapefile has no chunk handle until it opens. Preflight required local
    // sources in every view so an open failure cannot become a successful incomplete export.
    let complete_layers: std::collections::HashSet<u32> = {
        let mut scales = vec![map.ground_res_at(&camera) / rustgis_geom::PIXEL_M];
        if camera.pitch != 0.0 && !map.projection.is_globe() {
            scales.extend(camera.tilt_bands(view.width, view.height).iter().map(|b| map.ground_res_at(&b.camera) / rustgis_geom::PIXEL_M));
        }
        map.layers
            .iter()
            .filter(|l| scales.iter().any(|scale| map.draws_at(l, *scale)))
            .flat_map(|l| {
                let mask = l
                    .mask
                    .as_ref()
                    .and_then(|name| {
                        map.layers.iter().find(|m| m.name.eq_ignore_ascii_case(name) && m.geometry_type() == GeometryType::Polygon)
                    })
                    .map(|m| m.id);
                std::iter::once(l.id).chain(mask)
            })
            .collect()
    };
    if !complete_layers.is_empty() {
        for layer in &mut map.layers {
            if !complete_layers.contains(&layer.id) {
                continue;
            }
            if let Some(source) = &mut layer.source
                && !source.is_open()
                && is_local_shapefile(source)
            {
                if let Err(e) = source.open() {
                    source.error = Some(e);
                    return None;
                }
                if layer.dataset.fields.is_empty()
                    && let Some(info) = source.info()
                {
                    layer.dataset.fields = info.fields.clone();
                }
            }
        }
    }
    let depth_scale = map.ground_res_at(&camera) / rustgis_geom::PIXEL_M;
    // Perspective markers and extrusions have a global depth pass. Preserve that complete
    // upstream path using an isolated full-source snapshot until it has a chunk-aware sorter.
    // The owning interaction window, selection and editing provider are never replaced.
    if camera.pitch != 0.0
        && !map.projection.is_globe()
        && map.layers.iter().any(|l| needs_tilted_snapshot(map, l, &complete_layers, depth_scale))
    {
        // Keep asynchronous windows and pending work on their owner, never a disposable copy.
        let bands = camera.tilt_bands(view.width, view.height);
        let data_pending = sync_tilted(map, &View { camera, ..*view }, &bands, full_res_rasters);
        let mut snapshot = map.clone();
        let mut snapshot_budget = SnapshotBudget::new(complete_snapshot_limit());
        for layer in &mut snapshot.layers {
            if !complete_layers.contains(&layer.id) || layer.source.as_ref().and_then(|s| s.chunk_source()).is_none() {
                layer.source = None;
                continue;
            }
            if !needs_tilted_snapshot(map, layer, &complete_layers, depth_scale) {
                // Plain fills/lines keep their complete per-band traversal, outside the
                // global depth-order snapshot and its feature-storage budget.
                continue;
            }
            let Some(source) = layer.source.as_ref().and_then(|s| s.chunk_source()) else { continue };
            let ids: std::collections::HashSet<u64> =
                layer.selection.iter().filter_map(|r| layer.dataset.features.get(*r).map(|f| f.id)).collect();
            let mut features = Vec::new();
            let result = visit_complete(source.as_ref(), &mut |chunk| snapshot_budget.append(&mut features, chunk));
            if let Err(e) = result {
                if let Some(s) = map.layer_mut(layer.id).and_then(|l| l.source.as_mut()) {
                    s.error = Some(e.to_string());
                }
                return None;
            }
            if let Some(source) = map.layer_mut(layer.id).and_then(|l| l.source.as_mut()) {
                source.error = None;
            }
            layer.selection = features.iter().enumerate().filter_map(|(r, f)| ids.contains(&f.id).then_some(r)).collect();
            layer.source = None;
            layer.dataset.features = features.into();
            layer.clear_cache();
            layer.touch_geometry();
        }
        let view = View { camera, ..*view };
        // Owner synchronization has already run. Repeating it on the copy could consume a
        // shared pending result before its owner installs it, even for a plain chunk source.
        let mut output = if backend.is_gpu() {
            gpu_turned(&mut snapshot, &view, full_res_rasters, true)
                .map(RenderOutput::Gpu)
                .or_else(|| render_tilted(&mut snapshot, &view, full_res_rasters, true).map(RenderOutput::Cpu))
        } else {
            render_tilted(&mut snapshot, &view, full_res_rasters, true).map(RenderOutput::Cpu)
        };
        map.adopt_caches(&snapshot);
        if let Some(output) = &mut output {
            match output {
                RenderOutput::Cpu(rendered) => rendered.data_pending |= data_pending,
                RenderOutput::Gpu(scene) => scene.data_pending |= data_pending,
            }
        }
        return output;
    }
    // The globe turns and tilts itself (`Globe`): one frame.
    if map.projection.is_globe() {
        let view = View { camera, ..*view };
        return render_frame(map, &view, full_res_rasters, backend, Labels::Own, false);
    }
    {
        let view = View { camera, ..*view };
        if backend.is_gpu()
            && (camera.pitch != 0.0 || camera.bearing != 0.0)
            && let Some(scene) = gpu_turned(map, &view, full_res_rasters, false)
        {
            return Some(RenderOutput::Gpu(scene));
        }
        if camera.pitch != 0.0 {
            return render_tilted(map, &view, full_res_rasters, false).map(RenderOutput::Cpu);
        }
        if camera.bearing != 0.0 {
            return render_turned(map, &view, full_res_rasters, record).map(RenderOutput::Cpu);
        }
    }
    render_frame(map, view, full_res_rasters, backend, Labels::Own, record)
}

/// Persisted local Shapefiles can require complete traversal before a handle is opened.
/// Converted, embedded, remote and database sources retain their own provider behavior.
pub(crate) fn is_local_shapefile(source: &rustgis_map::source::SourceLayer) -> bool {
    source.embedded.is_none()
        && source.gdal.is_none()
        && source.duckdb.is_none()
        && source.postgis.is_none()
        && source.mysql.is_none()
        && source.sqlserver.is_none()
        && source.path.as_ref().is_some_and(|p| {
            !rustgis_data::source::is_remote_name(p) && std::path::Path::new(p).extension().is_some_and(|e| e.eq_ignore_ascii_case("shp"))
        })
}

/// Layers whose perspective painter needs complete globally depth-ordered features.
/// Ordinary fills/lines and their masks stay on the complete per-band chunk path.
fn needs_tilted_snapshot(map: &Map, layer: &rustgis_map::Layer, complete_layers: &std::collections::HashSet<u32>, scale: f64) -> bool {
    complete_layers.contains(&layer.id)
        && map.draws_at(layer, scale)
        && layer.source.as_ref().and_then(|s| s.chunk_source()).is_some()
        && (stands_up(map, layer) || (layer.geometry_type() == GeometryType::Polygon && layer.extrusion.is_some()))
}

// Separate from the interaction budget: this limits retained complete-source feature storage
// across the perspective snapshot. Raster targets, derived caches and decoding scratch are extra.
const COMPLETE_SNAPSHOT_BYTES: usize = rustgis_map::source::VIEW_BYTES * 4;
/// Owned-storage target for a complete traversal; distinct from interaction and depth budgets.
const COMPLETE_CHUNK_BYTES: usize = 8 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    static SNAPSHOT_TEST_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn complete_snapshot_limit() -> usize {
    #[cfg(test)]
    if let Some(limit) = SNAPSHOT_TEST_LIMIT.with(|s| s.get()) {
        return limit;
    }
    COMPLETE_SNAPSHOT_BYTES
}

/// Account owned rows and actual vector capacity across all materialized chunk sources.
/// Exceeding the budget fails the entire frame; no source is sampled or truncated.
struct SnapshotBudget {
    limit: usize,
    used: usize,
}

impl SnapshotBudget {
    fn new(limit: usize) -> Self {
        Self { limit, used: 0 }
    }

    fn append(&mut self, features: &mut Vec<rustgis_data::Feature>, mut chunk: Vec<rustgis_data::Feature>) -> rustgis_data::Result<()> {
        let bad =
            || rustgis_data::DataError::Format(format!("complete perspective snapshot exceeds its {} byte feature budget", self.limit));
        let row_size = std::mem::size_of::<rustgis_data::Feature>();
        let heap = chunk.iter().fold(0usize, |n, f| n.saturating_add(rustgis_data::source::chunk_memory::feature_bytes(f)));
        let needed = features.len().checked_add(chunk.len()).ok_or_else(bad)?;
        let old_capacity = features.capacity();
        let extra = needed.saturating_sub(old_capacity).saturating_mul(row_size);
        if self.used.saturating_add(heap).saturating_add(extra) > self.limit {
            return Err(bad());
        }
        features
            .try_reserve_exact(chunk.len())
            .map_err(|e| rustgis_data::DataError::Format(format!("complete perspective snapshot: {e}")))?;
        let extra = features.capacity().saturating_sub(old_capacity).saturating_mul(row_size);
        let used = self.used.saturating_add(heap).saturating_add(extra);
        if used > self.limit {
            return Err(bad());
        }
        self.used = used;
        features.append(&mut chunk);
        Ok(())
    }
}

/// A capable source must explicitly finish the traversal; an incomplete summary must never
/// publish an apparently successful frame, even if it already delivered some chunks.
fn visit_complete(
    source: &dyn rustgis_data::source::FeatureSource,
    visit: &mut dyn FnMut(Vec<rustgis_data::Feature>) -> rustgis_data::Result<()>,
) -> rustgis_data::Result<rustgis_data::source::Batch> {
    let result = source.visit_chunks(None, COMPLETE_CHUNK_BYTES, visit)?;
    if !result.complete {
        return Err(rustgis_data::DataError::Format("source traversal was incomplete".into()));
    }
    Ok(result)
}

/// A label waiting to be placed: the label, its priority and its key, and where else it may go
/// when its own place is taken: position, angle and curved glyphs, tried in order. Whatever
/// moves the label on its way to the screen moves each place's geometry too.
#[derive(Clone, Debug)]
struct Candidate {
    label: PlacedLabel,
    priority: f64,
    key: rustgis_map::LabelKey,
    // Ready slots and deferred curve positions are mutually exclusive. Slot 0 is the
    // own place; alternatives keep slots 1.. even when invalid (locks name these slots).
    alternatives: Vec<LabelPlace>,
    curves: Option<label_places::Curves>,
    line: Option<LinePlaces>,
    /// Offered row hint (a streamed chunk's row), verified against the feature id before use;
    /// `usize::MAX` when no row is known.
    row: usize,
}

/// Alternatives from one line stretch; band siblings retain one top-level candidate slot.
#[derive(Clone, Debug, Default)]
struct LinePlaces {
    anchors: Vec<Option<Coord>>,
    deduplicate: bool,
    // Other camera bands for the same key, each retaining its own placement slots.
    fallbacks: Vec<Candidate>,
}

/// Geometry of a possible text placement; style and the stationary icon belong to the candidate.
#[derive(Clone, Debug, PartialEq)]
struct LabelPlace {
    x: f32,
    y: f32,
    angle: Option<f32>,
    glyphs: Vec<LabelGlyph>,
}

/// Point places only: these labels stay upright and never carry an angle or curved glyphs.
impl From<(f32, f32)> for LabelPlace {
    fn from((x, y): (f32, f32)) -> Self {
        Self { x, y, angle: None, glyphs: Vec::new() }
    }
}

impl LabelPlace {
    #[cfg(test)]
    fn of(l: &PlacedLabel) -> Self {
        Self { x: l.x, y: l.y, angle: l.angle, glyphs: l.glyphs.clone() }
    }

    /// Move glyph ownership out; the label has no glyphs until a place is swapped back in.
    fn take(l: &mut PlacedLabel) -> Self {
        Self { x: l.x, y: l.y, angle: l.angle, glyphs: std::mem::take(&mut l.glyphs) }
    }

    fn swap(&mut self, l: &mut PlacedLabel) {
        std::mem::swap(&mut self.x, &mut l.x);
        std::mem::swap(&mut self.y, &mut l.y);
        std::mem::swap(&mut self.angle, &mut l.angle);
        std::mem::swap(&mut self.glyphs, &mut l.glyphs);
    }

    /// Transform this place using the candidate's font metrics, without moving its icon.
    fn transform(&mut self, l: &mut PlacedLabel, at: &impl Fn(f32, f32) -> Option<(f32, f32)>) -> bool {
        let Some((x, y)) = at(self.x, self.y) else { return false };
        if !x.is_finite() || !y.is_finite() {
            return false;
        }
        // A character left in the source view would be drawn at a stale offset. Reject the
        // whole place, including a missing baseline direction, before mutating the template.
        if self
            .glyphs
            .iter()
            .any(|g| at(self.x + g.dx, self.y + g.dy).is_none() || turned_angle(self.x + g.dx, self.y + g.dy, g.angle, at).is_none())
        {
            return false;
        }
        let icon = l.icon.take();
        self.swap(l);
        turn_with(l, at);
        (l.x, l.y) = (x, y);
        self.swap(l);
        l.icon = icon;
        true
    }
}

/// A label with no alternatives.
impl From<(PlacedLabel, f64, rustgis_map::LabelKey)> for Candidate {
    fn from((label, priority, key): (PlacedLabel, f64, rustgis_map::LabelKey)) -> Candidate {
        Candidate { label, priority, key, alternatives: Vec::new(), curves: None, line: None, row: usize::MAX }
    }
}

/// Keep exactly the candidates the upstream placement limit can consider, with stable ties.
fn compact_label_candidates(candidates: &mut Vec<Candidate>) {
    if candidates.len() > 40_000 {
        let rank = |p: f64| if p.is_nan() { f64::NEG_INFINITY } else { p };
        candidates.sort_by(|a, b| rank(b.priority).total_cmp(&rank(a.priority)));
        candidates.truncate(20_000);
    }
}

/// Where a frame's labels go.
enum Labels<'a> {
    /// Placed in the frame.
    Own,
    /// Placed in the turned view the north-up frame stands in for ([`render_turned`]).
    Turned(&'a View),
    /// Handed back unplaced, at their places in a tilted view ([`render_tilted`]), whose
    /// bands share one read of the streamed layers.
    Tilted(&'a View, &'a mut Vec<Candidate>),
    /// Placed in the frame, a band of a tilted view's drape ([`render_tilt_drape`]), which
    /// shares the view's read of the streamed layers.
    Shared,
}

/// Labels farther than this (as a fraction of the centre's scale) are left out of a tilted view.
const LABEL_MIN_DEPTH_SCALE: f64 = 0.3;

/// A tilted view, on the CPU: the ground it shows is drawn flat in bands
/// ([`Camera::tilt_bands`]), sharp near and coarser toward the horizon (so far tiles come at
/// lower zoom), then each screen pixel takes its colour from the band under it, fading into fog
/// and the sky past the view's reach. Labels are placed in the tilted view, upright.
fn render_tilted(map: &mut Map, view: &View, full_res_rasters: bool, sources_ready: bool) -> Option<Rendered> {
    let bands_of = view.camera.tilt_bands(view.width, view.height);
    let data_pending = !sources_ready && sync_tilted(map, view, &bands_of, full_res_rasters);
    // Labels are placed in the tilted view: a lock on a flat view's labels doesn't apply.
    let mut candidates = Vec::new();
    let (w, h) = view.pixel_size();
    let mut pm = Canvas::new(w, h)?;
    let lock = map.label_lock.take();
    let mut warp = Warp::new(view, map);
    // On terrain the bands are kept, to lay over the ground's relief once all are drawn: every
    // triangle of every band is then ordered far to near together. That holds all the bands at
    // once (at most eight, `Camera::tilt_bands`, each capped at `MAX_BAND_PX` a side) where the flat warp holds
    // one at a time: the cost of hiding what slopes stand in front of.
    let ground = terrain::Ground::of(map, &view.camera);
    let mut kept: Vec<(View, (f64, f64), Pixmap)> = Vec::new();
    let last = bands_of.len().saturating_sub(1);
    let (mut attribution, mut tiles_pending, mut tile_errors) = (Vec::<String>::new(), false, Vec::<String>::new());
    let (mut partial, mut pending_in_place) = (Vec::<u32>::new(), Vec::<u32>::new());
    for (i, b) in bands_of.into_iter().enumerate() {
        let v = band_view(view, &b);
        let Some(RenderOutput::Cpu(r)) =
            render_frame(map, &v, full_res_rasters, Backend::Cpu, Labels::Tilted(view, &mut candidates), false)
        else {
            let scale = map.ground_res_at(&v.camera) / rustgis_geom::PIXEL_M;
            let complete = sources_ready
                || map.layers.iter().filter(|l| map.draws_at(l, scale)).any(|l| {
                    l.source.as_ref().and_then(|s| s.chunk_source()).is_some()
                        || l.mask
                            .as_deref()
                            .and_then(|name| {
                                map.layers.iter().find(|m| m.name.eq_ignore_ascii_case(name) && m.geometry_type() == GeometryType::Polygon)
                            })
                            .and_then(|m| m.source.as_ref())
                            .and_then(|s| s.chunk_source())
                            .is_some()
                });
            if complete {
                map.label_lock = lock;
                return None;
            }
            // Preserve the upstream best-effort band path for ordinary providers.
            continue;
        };
        for a in r.attribution {
            if !attribution.contains(&a) {
                attribution.push(a);
            }
        }
        for e in r.tile_errors {
            if !tile_errors.contains(&e) {
                tile_errors.push(e);
            }
        }
        for id in r.partial {
            if !partial.contains(&id) {
                partial.push(id);
            }
        }
        for id in r.pending_in_place {
            if !pending_in_place.contains(&id) {
                pending_in_place.push(id);
            }
        }
        tiles_pending |= r.tiles_pending;
        if ground.is_some() {
            // The nearest band reaches the bottom of the view, the farthest the sky.
            let depth = (if i == 0 { 0.0 } else { b.depth.0 }, if i == last { f64::INFINITY } else { b.depth.1 });
            kept.push((v, depth, r.pixmap));
        } else {
            // Into the view at once, nearest band first, so only one band is held at a time.
            warp.band(pm.pixels(), &v, &r.pixmap, view.camera.depth_y(b.depth.1, view.height), view.camera.depth_y(b.depth.0, view.height));
        }
    }
    map.label_lock = lock;
    match &ground {
        Some(g) => {
            drape_terrain(pm.pixels(), view, map, &kept, g);
            tiles_pending |= g.pending();
        }
        None => warp.finish(pm.pixels()),
    }
    drop(kept);
    extrusions_tilted(map, view, pm.pixels());
    markers_tilted(map, view, &mut pm);
    let placed = place_tilted_labels(map, view, candidates, ground.as_ref());
    for l in &placed.labels {
        if let Some(icon) = &l.icon {
            draw_icon(&mut pm, icon, view.pixels_per_point as f32);
        }
    }
    let unplaced = if map.label_display.show_unplaced { placed.unplaced } else { Vec::new() };
    Some(Rendered {
        pixmap: pm.into_pixmap(),
        labels: placed.labels,
        unplaced,
        attribution,
        tiles_pending,
        data_pending,
        tile_errors,
        partial,
        pending_in_place,
        recorded: None,
    })
}

/// Lays a tilted view's bands, drawn flat, over the ground's relief: each band as a grid of
/// triangles raised to the ground's height and mapped from its picture, every band keeping to
/// its own stretch of ground, all drawn far to near so nearer slopes hide what lies behind
/// them; fog with distance, and the sky behind. (Without terrain, [`Warp`] does it per pixel.)
fn drape_terrain(pm: &mut Pixmap, view: &View, map: &Map, bands: &[(View, (f64, f64), Pixmap)], ground: &terrain::Ground) {
    let (sky, fog) = tilt_sky(map);
    let cam = view.camera;
    let (w, h) = (view.width, view.height);
    let k = f64::from(pm.width()) / w.max(1e-9);
    let far = rustgis_map::camera::FAR;
    // The sky shading into the fog at the reach's row; fog below it, where no slope stands.
    let sky_y = (cam.sky_y(h).unwrap_or(0.0).max(1.0) * k) as f32;
    let c = |c: Rgba| tiny_skia::Color::from_rgba8(c.r(), c.g(), c.b(), 255);
    match tiny_skia::LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(0.0, sky_y),
        vec![GradientStop::new(0.0, c(sky)), GradientStop::new(1.0, c(fog))],
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        Some(shader) => {
            if let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32) {
                pm.fill_rect(r, &Paint { shader, ..Paint::default() }, Transform::identity(), None);
            }
        }
        None => pm.fill(c(fog)),
    }
    // Every band's triangles: (depth, band, screen and picture corners).
    type Tri = (f64, usize, [(f64, f64); 3], [(f64, f64); 3]);
    let mut tris: Vec<Tri> = Vec::new();
    let n = terrain::CELLS;
    for (bi, (v, (d0, d1), pic)) in bands.iter().enumerate() {
        let kb = f64::from(pic.width()) / v.width.max(1e-9);
        // A band's grid: screen point, picture point and depth of each vertex.
        let grid: Vec<Option<((f64, f64), (f64, f64), f64)>> = (0..=n)
            .flat_map(|j| (0..=n).map(move |i| (i, j)))
            .map(|(i, j)| {
                let (lx, ly) = (v.width * i as f64 / n as f64, v.height * j as f64 / n as f64);
                let m = v.camera.to_map(lx, ly, v.width, v.height);
                let depth = 1.0 / cam.depth_scale(m, h)?;
                let (u, gv) = cam.ground_px(m);
                let (sx, sy) = cam.project_raised(u, gv, ground.up_px(m, &cam), w, h)?;
                (sx.is_finite() && sy.is_finite()).then_some(((sx * k, sy * k), (lx * kb, ly * kb), depth))
            })
            .collect();
        // Its own stretch of ground, a little over at the seams.
        let margin = ((d1.min(far) - d0) * 0.02).max(0.0);
        for j in 0..n {
            for i in 0..n {
                let at = |i: usize, j: usize| grid.get(j * (n + 1) + i).copied().flatten();
                let (Some(a), Some(b), Some(c), Some(d)) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1)) else { continue };
                for [p, q, r] in [[a, b, d], [a, d, c]] {
                    let depth = (p.2 + q.2 + r.2) / 3.0;
                    if depth < d0 - margin || depth > d1 + margin || p.2.min(q.2).min(r.2) > far {
                        continue;
                    }
                    // Wholly off one side of the picture: nothing to draw.
                    let (pw, ph) = (f64::from(pm.width()), f64::from(pm.height()));
                    let xs = [p.0.0, q.0.0, r.0.0];
                    let ys = [p.0.1, q.0.1, r.0.1];
                    if xs.iter().all(|x| *x < 0.0)
                        || xs.iter().all(|x| *x > pw)
                        || ys.iter().all(|y| *y < 0.0)
                        || ys.iter().all(|y| *y > ph)
                    {
                        continue;
                    }
                    tris.push((depth, bi, [p.0, q.0, r.0], [p.1, q.1, r.1]));
                }
            }
        }
    }
    tris.sort_by(|a, b| b.0.total_cmp(&a.0));
    let fog_paint = |t: f64| paint(fog, t as f32);
    for (depth, bi, dst, src) in tris {
        let Some((_, _, pic)) = bands.get(bi) else { continue };
        let Some(t) = affine(src, dst) else { continue };
        // A hair larger all round, so neighbours leave no seam.
        let (cx, cy) = ((dst[0].0 + dst[1].0 + dst[2].0) / 3.0, (dst[0].1 + dst[1].1 + dst[2].1) / 3.0);
        let mut pb = PathBuilder::new();
        for (i, (x, y)) in dst.iter().enumerate() {
            let (dx, dy) = (x - cx, y - cy);
            let len = dx.hypot(dy).max(1e-9);
            let (x, y) = ((x + dx / len * 0.6) as f32, (y + dy / len * 0.6) as f32);
            if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
        }
        pb.close();
        let Some(path) = pb.finish() else { continue };
        let shader = tiny_skia::Pattern::new(pic.as_ref(), SpreadMode::Pad, tiny_skia::FilterQuality::Bilinear, 1.0, t);
        pm.fill_path(&path, &Paint { shader, anti_alias: true, ..Paint::default() }, FillRule::Winding, Transform::identity(), None);
        // Fog from halfway to the reach.
        let f = ((depth - far * 0.5) / (far * 0.5)).clamp(0.0, 1.0);
        let f = f * f * (3.0 - 2.0 * f);
        if f > 0.004 {
            pm.fill_path(&path, &fog_paint(f), FillRule::Winding, Transform::identity(), None);
        }
    }
}

/// The affine map taking the triangle `src` onto `dst` (physical pixels); `None` for a
/// triangle with no area.
fn affine(src: [(f64, f64); 3], dst: [(f64, f64); 3]) -> Option<Transform> {
    let (s1, s2) = ((src[1].0 - src[0].0, src[1].1 - src[0].1), (src[2].0 - src[0].0, src[2].1 - src[0].1));
    let (d1, d2) = ((dst[1].0 - dst[0].0, dst[1].1 - dst[0].1), (dst[2].0 - dst[0].0, dst[2].1 - dst[0].1));
    let det = s1.0 * s2.1 - s2.0 * s1.1;
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let a = (d1.0 * s2.1 - d2.0 * s1.1) / det;
    let b = (d2.0 * s1.0 - d1.0 * s2.0) / det;
    let c = (d1.1 * s2.1 - d2.1 * s1.1) / det;
    let d = (d2.1 * s1.0 - d1.1 * s2.0) / det;
    let tx = dst[0].0 - a * src[0].0 - b * src[0].1;
    let ty = dst[0].1 - c * src[0].0 - d * src[0].1;
    [a, b, c, d, tx, ty]
        .iter()
        .all(|v| v.is_finite())
        .then(|| Transform::from_row(a as f32, c as f32, b as f32, d as f32, tx as f32, ty as f32))
}

/// Lifts a tilted view's labels onto the ground's relief: each to the height of the ground
/// under it.
fn lift_labels(view: &View, ground: &terrain::Ground, candidates: &mut [Candidate]) {
    let cam = view.camera;
    let (w, h) = (view.width, view.height);
    let offset = |x: f32, y: f32| {
        let m = cam.try_to_map(f64::from(x), f64::from(y), w, h)?;
        let (u, v) = cam.ground_px(m);
        let (sx, sy) = cam.project_raised(u, v, ground.up_px(m, &cam), w, h)?;
        let (dx, dy) = (sx as f32 - x, sy as f32 - y);
        (dx.is_finite() && dy.is_finite()).then_some((dx, dy))
    };
    for c in candidates.iter_mut() {
        if let Some([ax, ay]) = c.label.collide.callout_anchor {
            let (fx, fy) = (c.label.x + ax, c.label.y + ay);
            if let (Some((fdx, fdy)), Some((dx, dy))) = (offset(fx, fy), offset(c.label.x, c.label.y)) {
                c.label.collide.callout_anchor = Some([ax + fdx - dx, ay + fdy - dy]);
            }
        }
        if let Some((dx, dy)) = offset(c.label.x, c.label.y) {
            c.label.x += dx;
            c.label.y += dy;
            if let Some(icon) = &mut c.label.icon {
                icon.x += dx;
                icon.y += dy;
            }
        }
        for place in &mut c.alternatives {
            if let Some((dx, dy)) = offset(place.x, place.y) {
                place.x += dx;
                place.y += dy;
            }
        }
        if let Some(curves) = &mut c.curves {
            curves.shift_each(&offset);
        }
    }
}

/// A turned or tilted view as a GPU scene, drawn by the vertex shader's full perspective: a
/// turned view's north-up frame, or a tilted view's bands, each clipped to its own ground (as
/// [`render_tilted`] warps them), with point markers upright over them and labels placed in
/// the view. `None` when the view needs pictures drawn on the CPU (tiles, rasters, layers drawn
/// as pictures, icons or extrusions), or the scene isn't plain: the caller draws it on the CPU.
fn gpu_turned(map: &mut Map, view: &View, full_res_rasters: bool, sources_ready: bool) -> Option<gpu_scene::GpuScene> {
    use gpu_scene::{CmdClip, DrawCmd, GpuScene, SceneTilt};
    if !gpu_may_turn(map, view) {
        return None;
    }
    let pictures = |cmds: &[DrawCmd]| cmds.iter().any(|c| matches!(c, DrawCmd::Image { .. }));
    let (sky, fog) = tilt_sky(map);
    let tilt = Some(SceneTilt { camera: view.camera, sky: mesh::premul(sky), fog: mesh::premul(fog) });
    if view.camera.pitch == 0.0 {
        let (bw, bh) = view.camera.rotated_size(view.width, view.height);
        let north = View { width: bw.ceil() + 2.0, height: bh.ceil() + 2.0, camera: Camera { bearing: 0.0, ..view.camera }, ..*view };
        let Some(RenderOutput::Gpu(scene)) = render_frame(map, &north, full_res_rasters, Backend::Gpu, Labels::Turned(view), false) else {
            return None;
        };
        if pictures(&scene.cmds) || scene.labels.iter().any(|l| l.icon.is_some()) {
            return None;
        }
        return Some(GpuScene { tilt, ..scene });
    }
    let bands = view.camera.tilt_bands(view.width, view.height);
    let data_pending = !sources_ready && sync_tilted(map, view, &bands, full_res_rasters);
    let lock = map.label_lock.take();
    let mut candidates = Vec::new();
    let (mut cmds, mut clips) = (Vec::new(), Vec::new());
    let (mut attribution, mut tiles_pending, mut tile_errors, mut clear) = (Vec::<String>::new(), false, Vec::<String>::new(), [0.0; 4]);
    let (mut partial, mut pending_in_place) = (Vec::<u32>::new(), Vec::<u32>::new());
    let mut plain = true;
    let last = bands.len().saturating_sub(1);
    for (i, b) in bands.iter().enumerate() {
        let v = band_view(view, b);
        let Some(RenderOutput::Gpu(scene)) =
            render_frame(map, &v, full_res_rasters, Backend::Gpu, Labels::Tilted(view, &mut candidates), false)
        else {
            plain = false;
            break;
        };
        if pictures(&scene.cmds) {
            plain = false;
            break;
        }
        clear = scene.clear;
        // A long band is drawn at a lower pixel density (`band_view`): its strokes and markers,
        // sized in its pixels, are brought to the view's.
        let k = (view.pixels_per_point / v.pixels_per_point.max(1e-9)) as f32;
        let mut scene = scene;
        if (k - 1.0).abs() > 1e-6 {
            for c in &mut scene.cmds {
                match c {
                    DrawCmd::Mesh { vertices, .. } => vertices.iter_mut().for_each(|v| v.stroke_px *= k),
                    DrawCmd::Points { instances, .. } => instances.iter_mut().for_each(|p| p.radius_px *= k),
                    DrawCmd::Image { .. }
                    | DrawCmd::Extrusion { .. }
                    | DrawCmd::Tiles { .. }
                    | DrawCmd::LiveTiles { .. }
                    | DrawCmd::TileMeshes { .. } => {}
                }
            }
        }
        for a in scene.attribution {
            if !attribution.contains(&a) {
                attribution.push(a);
            }
        }
        for e in scene.tile_errors {
            if !tile_errors.contains(&e) {
                tile_errors.push(e);
            }
        }
        for id in scene.partial {
            if !partial.contains(&id) {
                partial.push(id);
            }
        }
        for id in scene.pending_in_place {
            if !pending_in_place.contains(&id) {
                pending_in_place.push(id);
            }
        }
        tiles_pending |= scene.tiles_pending;
        // The nearest band reaches the bottom of the view and the farthest the sky, so no ground
        // falls between them.
        let near = if i == 0 { 0.0 } else { b.depth.0 as f32 };
        let far = if i == last { f32::MAX } else { b.depth.1 as f32 };
        let clip = CmdClip { depth: [near, far], upright: false };
        clips.extend(std::iter::repeat_n(clip, scene.cmds.len()));
        cmds.extend(scene.cmds);
    }
    map.label_lock = lock;
    if !plain {
        return None;
    }
    let placed = place_tilted_labels(map, view, candidates, None);
    if placed.labels.iter().any(|l| l.icon.is_some()) {
        return None;
    }
    let origin = [view.camera.center.x, view.camera.center.y];
    // Extruded features stand up over the ground, their shadows under them; more than the GPU
    // budget takes, and the view is drawn on the CPU.
    for cmd in extrusion_cmds(&extruded_shapes(map, view), origin)? {
        cmds.push(cmd);
        clips.push(CmdClip::ALL);
    }
    let ppp = view.pixels_per_point as f32;
    let instances: Vec<gpu_scene::PointInstance> = upright_markers(map, view, 0.0)
        .iter()
        .flat_map(|m| mesh::marker_instances(m.at, origin, &m.symbol, ppp, m.selected.then_some(SELECTION)))
        .collect();
    if !instances.is_empty() {
        cmds.push(DrawCmd::Points { origin, instances });
        clips.push(CmdClip { upright: true, ..CmdClip::ALL });
    }
    let unplaced = if map.label_display.show_unplaced { placed.unplaced } else { Vec::new() };
    Some(GpuScene {
        clear,
        cmds,
        labels: placed.labels,
        unplaced,
        attribution,
        tiles_pending,
        data_pending,
        tile_errors,
        partial,
        pending_in_place,
        tilt,
        clips,
    })
}

/// Places the labels a tilted view's bands offered, nearest band first. Non-repeated line
/// alternatives keep bounded band fallbacks until one fits; other features keep the nearer
/// band's offer. Tile labels (keyed by their place in a band's list) are told apart by their
/// text and where they land, to a few points. Extruded
/// features' labels go up onto their roofs, and on terrain every label onto the ground's relief.
fn place_tilted_labels(map: &Map, view: &View, mut candidates: Vec<Candidate>, ground: Option<&terrain::Ground>) -> Placement {
    let mut seen = std::collections::HashSet::new();
    let mut seen_tile = std::collections::HashSet::new();
    let mut seen_repeat = std::collections::HashSet::new();
    let near = |c: &Candidate| ((c.label.x / 4.0).round() as i64, (c.label.y / 4.0).round() as i64);
    candidates.retain(|c| match c.key {
        (u32::MAX, ..) => seen_tile.insert((c.label.text.clone(), near(c))),
        // A line's repeats: each band numbers those on its own stretch, so they are told apart by
        // where they land (the same repeat in two bands lands in one place).
        (l, r, k, i) if i > 0 => seen_repeat.insert((l, r, k, near(c))),
        // Non-repeated line alternatives claim the key only once a band is placed.
        _ if c.line.is_some() => true,
        key => seen.insert(key),
    });
    raise_labels(map, view, &mut candidates);
    if let Some(g) = ground {
        lift_labels(view, g, &mut candidates);
    }
    // Group bands under one candidate slot, retaining their near-first order. A nearer
    // band's failed places must not suppress a later band's usable ones.
    let mut groups: Vec<Candidate> = Vec::new();
    let mut line_groups = std::collections::HashMap::new();
    for mut candidate in candidates {
        if let Some(line) = &mut candidate.line {
            line.deduplicate = true;
            if let Some(&index) = line_groups.get(&candidate.key)
                && let Some(first) = groups.get_mut(index).and_then(|c: &mut Candidate| c.line.as_mut())
            {
                if first.fallbacks.len() < rustgis_map::camera::MAX_TILT_BANDS.saturating_sub(1) {
                    first.fallbacks.push(candidate);
                }
                continue;
            }
            line_groups.insert(candidate.key, groups.len());
        }
        groups.push(candidate);
    }
    place_labels_with_previews(groups, Vec::new(), view.width as f32, view.height as f32, false, map.label_display.show_unplaced)
}

/// Whether a turned or tilted `view` of `map` may be drawn on the GPU ([`gpu_turned`]): only
/// what the GPU draws itself shows (no tiles, rasters, see-through, masked, swiped or blended
/// layers, aggregations, graphics or stacked line strokes; no view effects; extrusions only
/// stand up in a tilted one; nor characters standing up, which the GPU has no template for). A cheap check made before any drawing; labels with icons can still
/// decline the scene once it is drawn.
pub fn gpu_may_turn(map: &Map, view: &View) -> bool {
    if map.projection.is_globe()
        || !map.view.effects.is_none()
        || map.view.color_vision != rustgis_map::view::ColorVision::Normal
        || map.tiles.iter().any(|t| t.draws())
        // The GPU draws the ground flat: a view over terrain stays on the CPU.
        || terrain::Ground::of(map, &view.camera).is_some()
    {
        return false;
    }
    let scale = map.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M;
    let held = map.held_by_group(scale);
    // Layers in a see-through group (none, mostly: then nothing is collected).
    let faded = map.group_opacities();
    map.layers.iter().filter(|l| l.visible && l.in_scale_range(scale) && !held.contains(&l.id)).all(|l| {
        l.raster.is_none()
            && l.opacity() >= 0.999
            && !faded.contains_key(&l.id)
            && l.mask.is_none()
            && l.blend == rustgis_map::effects::Blend::Normal
            && map.swipe.is_none_or(|(id, _)| id != l.id)
            && l.aggregation.is_none()
            && !l.graphics
            && !l.has_strokes()
            && !l.has_fills()
            && l.marker_rotation.is_none()
            && l.symbol_by.is_empty()
            && l.renderer.expression().is_none()
            && (l.extrusion.is_none() || view.camera.pitch != 0.0)
            && !(view.camera.pitch != 0.0 && l.renderer.has_char_markers() && stands_up(map, l))
    })
}

/// The colours of a tilted view's sky (at the top) and of the fog it fades into at the
/// horizon, from the basemap. A body without the Earth's atmosphere has neither: the ground
/// fades into the black of space (#223).
pub fn tilt_sky(map: &Map) -> (Rgba, Rgba) {
    if !map.body().is_earth() {
        let space = globe_space(true);
        return (space, space);
    }
    let (water, land, _, _) = map.palette();
    let white = Rgba::rgb(0xFF, 0xFF, 0xFF);
    let sky = water.lerp(white, 0.35);
    (sky, sky.lerp(land, 0.5).lerp(white, 0.3))
}

/// Lifts the labels of extruded features onto their roofs in a tilted view (as MapLibre
/// does), where the extrusions drawn over the ground would otherwise hide them.
fn raise_labels(map: &Map, view: &View, candidates: &mut [Candidate]) {
    let flat = map.flat();
    let cam = view.camera;
    let (w, h) = (view.width, view.height);
    // Each extruded layer's rows by feature id, the label keys naming features by id (#1046).
    let mut rows: std::collections::HashMap<u32, std::collections::HashMap<u64, usize>> = std::collections::HashMap::new();
    for c in candidates.iter_mut() {
        let (id, fid, _, _) = c.key;
        let offered = c.row;
        let l = &mut c.label;
        let Some(layer) = map.layer(id).filter(|l| l.geometry_type() == GeometryType::Polygon) else { continue };
        let Some(x) = &layer.extrusion else { continue };
        let features = &layer.dataset.features;
        // The row it was offered from while its feature is there, else looked up by id; one not
        // among them, as a streamed layer's, rises by the height no field gives.
        let row = Some(offered)
            .filter(|&r| features.get(r).is_some_and(|f| f.id == fid))
            .or_else(|| {
                let by_id = rows.entry(id).or_insert_with(|| features.iter().enumerate().map(|(r, f)| (f.id, r)).collect());
                by_id.get(&fid).copied()
            })
            .unwrap_or(usize::MAX);
        let metres = effects::extrusion_metres(&layer.dataset, x, row);
        let offset = |x: f32, y: f32| {
            let m = cam.try_to_map(f64::from(x), f64::from(y), w, h)?;
            let up = (metres / (flat.ground_per_unit(m) * map.body().ratio()).max(1e-9) / cam.res.max(1e-9)).clamp(0.0, 1e7);
            let (u, v) = cam.ground_px(m);
            let (sx, sy) = cam.project_raised(u, v, up, w, h)?;
            let delta = (sx as f32 - x, sy as f32 - y);
            (delta.0.is_finite() && delta.1.is_finite()).then_some(delta)
        };
        if let Some([ax, ay]) = l.collide.callout_anchor {
            let (fx, fy) = (l.x + ax, l.y + ay);
            if let (Some((fdx, fdy)), Some((dx, dy))) = (offset(fx, fy), offset(l.x, l.y)) {
                l.collide.callout_anchor = Some([ax + fdx - dx, ay + fdy - dy]);
            }
        }
        if let Some((dx, dy)) = offset(l.x, l.y) {
            l.x += dx;
            l.y += dy;
            if let Some(icon) = &mut l.icon {
                icon.x += dx;
                icon.y += dy;
            }
        }
        for p in &mut c.alternatives {
            if let Some((dx, dy)) = offset(p.x, p.y) {
                p.x += dx;
                p.y += dy;
            }
        }
        if let Some(curves) = &mut c.curves {
            curves.shift_each(&offset);
        }
    }
}

/// An extruded feature tessellated once, for a moving tilted view to stand up in perspective
/// every frame (as MapLibre's fill extrusions are built once and only re-projected).
pub struct ExtrudedShape {
    /// The point (the camera's coordinates) the roof's triangles are relative to.
    pub origin: Coord,
    /// The middle of its outline (the camera's coordinates): where it is, for ordering and fog.
    pub center: Coord,
    /// The roof's triangles: vertices relative to `origin` (map units) and their indices.
    pub roof: Vec<[f32; 2]>,
    pub roof_indices: Vec<u32>,
    /// The outer rings the walls stand on, and the holes' (the camera's coordinates).
    pub rings: Vec<Vec<Coord>>,
    pub holes: Vec<Vec<Coord>>,
    /// Its height: in metres, and in the camera's units at its place (Web Mercator stretches the
    /// ground toward the poles).
    pub metres: f64,
    pub height: f64,
    pub fill: Rgba,
    pub stroke: Rgba,
    /// The layer's extrusion settings: walls facing away culled, walls lit, shadows cast.
    pub face_culling: bool,
    pub lighting: bool,
    pub shadows: bool,
}

/// The extruded features of a tilted `view` of `map`, tessellated for [`ExtrudedShape`]s, from
/// geometry thinned for the view's scale, bottom layer first.
pub fn extruded_shapes(map: &mut Map, view: &View) -> Vec<ExtrudedShape> {
    let scale = map.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M;
    // Layers a hidden or out-of-scale group holds aren't drawn, as in `render_frame`.
    let held = map.held_by_group(scale);
    // Only the features on the ground the view shows (grown by a moving drape's margin).
    let cull = view.camera.extent(view.width, view.height).padded(0.3, 0.0);
    let (flat, key) = (map.flat(), map.projection.code());
    // Extrusion heights are metres on the map's body.
    let body = map.body().ratio();
    // Preserve the tighter footprint tolerance used for extruded walls and roofs.
    let tol = view.camera.res * rustgis_map::lod::EXTRUSION_THIN_PX;
    let mut order: Vec<usize> = (0..map.layers.len()).rev().collect();
    order.sort_by_key(|i| map.layers.get(*i).map_or(0, |l| l.priority));
    let mut out = Vec::new();
    for li in order {
        let Some(layer) = map.layers.get_mut(li) else { continue };
        if map.projection.is_globe()
            || !layer.visible
            || !layer.in_scale_range(scale)
            || held.contains(&layer.id)
            || layer.raster.is_some()
            || layer.geometry_type() != GeometryType::Polygon
        {
            continue;
        }
        let Some(x) = layer.extrusion.clone() else { continue };
        let rows = layer.drawn_rows(scale);
        layer.thinned_in(&flat, &key, tol);
        let layer: &rustgis_map::Layer = layer;
        let styler = layer.styler(Some(scale));
        let extents = layer.projected_extents();
        for (row, g) in layer.thinned_cache(tol).iter().enumerate() {
            let Some(Geometry::Polygon(polys)) = g else { continue };
            if rows.get(row) != Some(&true) {
                continue;
            }
            let Some(origin) = polys.iter().flatten().flatten().find(|c| c.is_finite()).copied() else { continue };
            let metres = effects::extrusion_metres(&layer.dataset, &x, row);
            let height = metres / (flat.ground_per_unit(origin) * body).max(1e-9);
            // Off the ground the view shows, even by its height: a tall building just off screen
            // still stands up into view.
            let reach = if height.is_finite() { height.clamp(0.0, 1e9) } else { 0.0 };
            if extents.get(row).is_some_and(|e| !e.padded(0.0, reach).intersects(&cull)) {
                continue;
            }
            let roof = mesh::fill_polygon(polys, [origin.x, origin.y], [0.0; 4], view.camera.res * 0.5, mesh::NO_LIMIT);
            let Ok(Some((verts, indices))) = roof else { continue };
            let sym = styler.symbol(row);
            let pts: Vec<Coord> = polys.iter().filter_map(|p| p.first()).flatten().copied().filter(|c| c.is_finite()).collect();
            let n = pts.len().max(1) as f64;
            let center = Coord::new(pts.iter().map(|c| c.x).sum::<f64>() / n, pts.iter().map(|c| c.y).sum::<f64>() / n);
            out.push(ExtrudedShape {
                origin,
                center,
                roof: verts.iter().map(|v| v.position).collect(),
                roof_indices: indices,
                rings: polys.iter().filter_map(|p| p.first()).filter(|r| r.len() >= 3).cloned().collect(),
                holes: polys.iter().flat_map(|p| p.iter().skip(1)).filter(|r| r.len() >= 3).cloned().collect(),
                metres,
                height,
                fill: sym.fill.with_alpha(255),
                stroke: sym.stroke,
                face_culling: x.face_culling,
                lighting: x.lighting,
                shadows: x.shadows,
            });
        }
    }
    out
}

pub use effects::LIGHT as EXTRUSION_LIGHT;
pub use effects::{TiltedWall, clip_near, shadow_shift, tilted_shadow_quads, tilted_walls};

/// Extruded features as GPU commands, relative to `origin`: the shadows they cast (when any
/// do), then their roofs and walls, which the depth buffer sorts out. Walls carry their outward
/// normals, lit in the shader so they stay lit right as the map turns.
fn extrusion_cmds(shapes: &[ExtrudedShape], origin: [f64; 2]) -> Option<Vec<gpu_scene::DrawCmd>> {
    // The most vertices the roofs, walls and shadows take together (about 220 MB), well before
    // a device's buffer limit or u32 indices could be reached: past it, `None` (the view is
    // drawn on the CPU rather than with features missing).
    const MAX_VERTICES: usize = 4_000_000;
    use gpu_scene::{EXTRUDE_ROOF, EXTRUDE_SHADOW, EXTRUDE_WALL, ExtrudeVertex};
    let rel = |c: Coord, z: f64| [(c.x - origin[0]) as f32, (c.y - origin[1]) as f32, z as f32];
    let vertex =
        |position: [f32; 3], normal: [f32; 2], color: [f32; 4], info: [f32; 4]| ExtrudeVertex { position, normal, color, info, _pad: 0.0 };
    let (mut solid, mut solid_i) = (Vec::new(), Vec::<u32>::new());
    let (mut shade, mut shade_i) = (Vec::new(), Vec::<u32>::new());
    let shadow = mesh::premul(Rgba::new(0, 0, 0, 60));
    for s in shapes {
        let more =
            s.roof.len().saturating_mul(2).saturating_add(s.rings.iter().chain(&s.holes).map(Vec::len).sum::<usize>().saturating_mul(8));
        if solid.len().saturating_add(shade.len()).saturating_add(more) > MAX_VERTICES {
            return None;
        }
        // A height that is no number (hostile attributes) stands no higher than the ground.
        let h = if s.height.is_finite() { s.height.clamp(0.0, 1e9) } else { 0.0 };
        // Roof indices beyond its vertices (never from the tessellator) would draw garbage.
        if s.roof_indices.iter().any(|i| *i as usize >= s.roof.len()) {
            continue;
        }
        let color = mesh::premul(s.fill);
        let lit = if s.lighting { 1.0 } else { 0.0 };
        let roof_at = |p: &[f32; 2]| Coord::new(s.origin.x + f64::from(p[0]), s.origin.y + f64::from(p[1]));
        // The roof.
        let base = solid.len() as u32;
        solid.extend(s.roof.iter().map(|p| vertex(rel(roof_at(p), h), [0.0; 2], color, [1.0, EXTRUDE_ROOF, lit, 0.0])));
        solid_i.extend(s.roof_indices.iter().map(|i| base + i));
        if s.shadows {
            // The footprint moved away from the light (in the shader).
            let base = shade.len() as u32;
            shade.extend(s.roof.iter().map(|p| vertex(rel(roof_at(p), 0.0), [0.0; 2], shadow, [0.0, EXTRUDE_SHADOW, 0.0, h as f32])));
            shade_i.extend(s.roof_indices.iter().map(|i| base + i));
        }
        // The walls; a hole's face into it (a courtyard).
        for (r, hole) in s.rings.iter().map(|r| (r, false)).chain(s.holes.iter().map(|r| (r, true))) {
            let edges = || (0..r.len()).filter_map(|i| Some((*r.get(i)?, *r.get((i + 1) % r.len())?)));
            let area2: f64 = edges().map(|(a, b)| a.x * b.y - b.x * a.y).sum();
            for (a, b) in edges() {
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = dx.hypot(dy);
                if !(len > 0.0 && len.is_finite()) {
                    continue;
                }
                let n =
                    if (area2 > 0.0) != hole { [(dy / len) as f32, (-dx / len) as f32] } else { [(-dy / len) as f32, (dx / len) as f32] };
                let k = solid.len() as u32;
                for (c, z, t) in [(a, 0.0, 0.0), (b, 0.0, 0.0), (b, h, 1.0), (a, h, 1.0)] {
                    solid.push(vertex(rel(c, z), n, color, [t, EXTRUDE_WALL, lit, 0.0]));
                }
                solid_i.extend([k, k + 1, k + 2, k, k + 2, k + 3]);
                if s.shadows && !hole {
                    let k = shade.len() as u32;
                    for (c, fall) in [(a, 0.0), (b, 0.0), (b, h), (a, h)] {
                        shade.push(vertex(rel(c, 0.0), [0.0; 2], shadow, [0.0, EXTRUDE_SHADOW, 0.0, fall as f32]));
                    }
                    shade_i.extend([k, k + 1, k + 2, k, k + 2, k + 3]);
                }
            }
        }
    }
    let mut out = Vec::new();
    if !shade_i.is_empty() {
        out.push(gpu_scene::DrawCmd::Extrusion { origin, vertices: shade, indices: shade_i, shadows: true });
    }
    if !solid_i.is_empty() {
        out.push(gpu_scene::DrawCmd::Extrusion { origin, vertices: solid, indices: solid_i, shadows: false });
    }
    Some(out)
}

/// Point layers with at most this many features stand their markers up in a tilted view; a
/// bigger one keeps them drawn on the ground.
const MAX_UPRIGHT_POINTS: usize = 50_000;

/// How much bigger or smaller than its symbol an upright marker is drawn, at the most, as it
/// nears or recedes from the camera.
const UPRIGHT_SCALE: (f64, f64) = (0.25, 2.0);

/// Whether a tilted view of `map` stands `layer`'s point markers up, facing the viewer (as
/// MapLibre's circles do), rather than drawing them flat on the ground. A masked, swiped or
/// blended layer, an aggregation or graphics keep theirs on the ground.
fn stands_up(map: &Map, layer: &rustgis_map::Layer) -> bool {
    // Complete sources must use their total, never the pan-dependent interaction sample.
    // An unknown total conservatively uses complete ground-marker traversal.
    let rows = match layer.source.as_ref().and_then(|s| s.chunk_source()) {
        Some(source) => source.info().count,
        None => Some(layer.dataset.features.len() as u64),
    };
    layer.raster.is_none()
        && layer.geometry_type() == GeometryType::Point
        && layer.aggregation.is_none()
        && !layer.graphics
        && layer.mask.is_none()
        && layer.blend == rustgis_map::effects::Blend::Normal
        && map.swipe.is_none_or(|(id, _)| id != layer.id)
        // Marks and turned markers stay on the ground, drawn as the flat map draws them.
        && !layer.has_strokes()
        && layer.marker_rotation.is_none()
        && layer.symbol_by.angle.is_none()
        && rows.is_some_and(|n| n <= MAX_UPRIGHT_POINTS as u64)
}

/// A point marker standing upright in a tilted view, facing the viewer.
#[derive(Clone, Debug)]
pub struct UprightMarker {
    /// Where it stands (the camera's coordinates).
    pub at: Coord,
    /// The ground's height under it (map units), on terrain; 0 without.
    pub height: f64,
    /// Its symbol, with the layer's transparency in its colours.
    pub symbol: Symbol,
    pub selected: bool,
}

impl UprightMarker {
    /// Where it stands in a tilted `w × h` view of `cam` (logical px), how much bigger or
    /// smaller than its symbol it is drawn there (above or below 1), and how far it has faded
    /// into the fog (0 to 1); `None` behind the camera or past the view's reach.
    pub fn place(&self, cam: &Camera, w: f64, h: f64) -> Option<(f64, f64, f64, f64)> {
        let k = cam.depth_scale(self.at, h).filter(|k| *k * rustgis_map::camera::FAR >= 1.0)?;
        let (u, v) = cam.ground_px(self.at);
        let up = (self.height / cam.res.max(1e-9)).clamp(-1e7, 1e7);
        let (x, y) = cam.project_raised(u, v, up, w, h).filter(|(x, y)| x.is_finite() && y.is_finite())?;
        // Fog from halfway to the reach, as on the ground.
        let far = rustgis_map::camera::FAR;
        let t = ((1.0 / k - far * 0.5) / (far * 0.5)).clamp(0.0, 1.0);
        Some((x, y, k.clamp(UPRIGHT_SCALE.0, UPRIGHT_SCALE.1), t * t * (3.0 - 2.0 * t)))
    }

    /// Its symbol drawn `k` times its size, faded `t` of the way into `fog`.
    pub fn look(&self, k: f64, fog: Rgba, t: f64) -> Symbol {
        let s = &self.symbol;
        let fade = |c: Rgba| c.lerp(fog.with_alpha(c.a()), t);
        Symbol {
            fill: fade(s.fill),
            stroke: fade(s.stroke),
            stroke_width: s.stroke_width * k as f32,
            size: s.size * k as f32,
            marker: s.marker,
        }
    }
}

/// The outline of a `marker` of radius `r` centred at (`x`, `y`), as [`draw_marker`] draws it
/// (a circle as a polygon): for drawing upright markers as meshes. Every outline is seen whole
/// from its centre, so a fan from the centre fills it. A character, which no fan fills, as a circle
/// of its size (while a tilted view moves; the settled view draws the character).
pub fn marker_outline(marker: Marker, x: f32, y: f32, r: f32) -> Vec<(f32, f32)> {
    match marker {
        Marker::Circle | Marker::Char(_) => (0..24)
            .map(|i| {
                let a = std::f32::consts::TAU * i as f32 / 24.0;
                (x + r * a.cos(), y + r * a.sin())
            })
            .collect(),
        Marker::Square => {
            vec![(x - r * 0.9, y - r * 0.9), (x + r * 0.9, y - r * 0.9), (x + r * 0.9, y + r * 0.9), (x - r * 0.9, y + r * 0.9)]
        }
        Marker::Triangle => vec![(x, y - r * 1.1), (x + r, y + r * 0.75), (x - r, y + r * 0.75)],
        Marker::Diamond => vec![(x, y - r * 1.15), (x + r * 0.9, y), (x, y + r * 1.15), (x - r * 0.9, y)],
        Marker::Star => (0..10)
            .map(|i| {
                let a = std::f32::consts::PI * (i as f32) / 5.0 - std::f32::consts::FRAC_PI_2;
                let rr = if i % 2 == 0 { r * 1.2 } else { r * 0.5 };
                (x + rr * a.cos(), y + rr * a.sin())
            })
            .collect(),
    }
}

/// The point markers a tilted `view` of `map` stands upright ([`stands_up`]), bottom layer
/// first and each layer's far to near, so nearer markers draw over farther ones. Those on the
/// ground it shows, grown by `margin` of its size on every side (for a view that moves).
pub fn upright_markers(map: &mut Map, view: &View, margin: f64) -> Vec<UprightMarker> {
    if map.projection.is_globe() {
        return Vec::new();
    }
    let scale = map.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M;
    let (flat, key) = (map.flat(), map.projection.code());
    let cull = view.camera.extent(view.width, view.height).padded(margin.clamp(CULL_MARGIN, 4.0), 0.0);
    let clip = map.view.clip_distance.map(|d| (view.camera.center_lonlat(), d));
    let held = map.held_by_group(scale);
    let group_opacity = map.group_opacities();
    let upright: std::collections::HashSet<u32> = map.layers.iter().filter(|l| stands_up(map, l)).map(|l| l.id).collect();
    let mut order: Vec<usize> = (0..map.layers.len()).rev().collect();
    order.sort_by_key(|i| map.layers.get(*i).map_or(0, |l| l.priority));
    let (cam, h) = (view.camera, view.height);
    let ground = terrain::Ground::of(map, &view.camera);
    let mut out = Vec::new();
    for li in order {
        let Some(layer) = map.layers.get_mut(li) else { continue };
        if !layer.visible || !layer.in_scale_range(scale) || held.contains(&layer.id) || !upright.contains(&layer.id) {
            continue;
        }
        let opacity = layer.opacity() * group_opacity.get(&layer.id).copied().unwrap_or(1.0);
        // Markers sized for a reference scale grow and shrink with the map.
        let size_factor = layer.size_factor(scale);
        let rows = layer.drawn_rows(scale);
        // Refreshes the projected copy for this projection.
        layer.projected_in(&flat, &key);
        let layer: &rustgis_map::Layer = layer;
        let styler = layer.styler(Some(scale));
        let mut here: Vec<(f64, UprightMarker)> = Vec::new();
        for (row, g) in layer.projected_cache().iter().enumerate() {
            if rows.get(row) != Some(&true) {
                continue;
            }
            let pts: &[Coord] = match g {
                Some(Geometry::Point(c)) => std::slice::from_ref(c),
                Some(Geometry::MultiPoint(v)) => v,
                _ => continue,
            };
            let lonlat = clip.and_then(|_| layer.dataset.features.get(row).and_then(|f| f.geometry.as_ref()).map(|g| g.extent()));
            if let (Some((centre, d)), Some(e)) = (clip, lonlat) {
                let near = Coord::new(centre.x.clamp(e.min_x, e.max_x), centre.y.clamp(e.min_y, e.max_y));
                if e.is_empty() || rustgis_geom::haversine_m(centre, near) > d {
                    continue;
                }
            }
            let mut symbol = styler.symbol(row);
            // Sized for its reference scale, as on the ground.
            symbol.size *= size_factor;
            symbol.stroke_width *= size_factor;
            let see = |c: Rgba| c.with_alpha((f32::from(c.a()) * opacity).round().clamp(0.0, 255.0) as u8);
            (symbol.fill, symbol.stroke) = (see(symbol.fill), see(symbol.stroke));
            let selected = layer.selection.contains(&row);
            for c in pts.iter().filter(|c| c.is_finite() && cull.contains(**c)) {
                let Some(k) = cam.depth_scale(*c, h) else { continue };
                // On terrain, standing on the ground (its height in map units there).
                let height = ground.as_ref().and_then(|g| g.height(*c).map(|m| (m / g.per_unit(*c)).clamp(-1e9, 1e9))).unwrap_or(0.0);
                here.push((k, UprightMarker { at: *c, height, symbol, selected }));
            }
        }
        here.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.extend(here.into_iter().map(|(_, m)| m));
    }
    out
}

/// The point markers of a tilted view, stood up over its ground and extrusions.
fn markers_tilted(map: &mut Map, view: &View, pm: &mut Canvas) {
    let (_, fog) = tilt_sky(map);
    let ppp = (f64::from(pm.width()) / view.width.max(1e-9)) as f32;
    let (pw, ph) = (pm.width() as f32, pm.height() as f32);
    for m in upright_markers(map, view, 0.0) {
        let Some((x, y, k, t)) = m.place(&view.camera, view.width, view.height) else { continue };
        let s = m.look(k, fog, t);
        let (x, y) = (x as f32 * ppp, y as f32 * ppp);
        let reach = (s.size.max(1.0) * 0.6 + s.stroke_width.max(0.0) + 4.0) * ppp;
        if x < -reach || y < -reach || x > pw + reach || y > ph + reach {
            continue;
        }
        draw_marker(pm, x, y, &s, 1.0, m.selected.then_some(SELECTION), ppp);
    }
}

/// The extruded layers of a tilted view, stood up in perspective over its ground, bottom layer
/// first (a drape of the moving view shows them flat).
fn extrusions_tilted(map: &mut Map, view: &View, pm: &mut Pixmap) {
    let scale = map.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M;
    // Layers a hidden or out-of-scale group holds aren't drawn, as in `render_frame`.
    let held = map.held_by_group(scale);
    let (flat, key) = (map.flat(), map.projection.code());
    // Heights are metres on the map's body.
    let body = map.body().ratio();
    let (_, fog) = tilt_sky(map);
    let mut order: Vec<usize> = (0..map.layers.len()).rev().collect();
    order.sort_by_key(|i| map.layers.get(*i).map_or(0, |l| l.priority));
    for li in order {
        let Some(layer) = map.layers.get_mut(li) else { continue };
        if !layer.visible
            || !layer.in_scale_range(scale)
            || held.contains(&layer.id)
            || layer.raster.is_some()
            || layer.geometry_type() != GeometryType::Polygon
        {
            continue;
        }
        let Some(x) = layer.extrusion.clone() else { continue };
        let rows = layer.drawn_rows(scale);
        // Refreshes the projected copy for this projection.
        layer.projected_in(&flat, &key);
        let layer: &rustgis_map::Layer = layer;
        let styler = layer.styler(Some(scale));
        let geoms: Vec<Option<&Geometry>> = layer.projected_cache().iter().map(Option::as_ref).collect();
        effects::extrude_tilted(pm, view, &layer.dataset, &geoms, &rows, &x, |c| flat.ground_per_unit(c) * body, fog, |r| styler.symbol(r));
    }
}

/// The most physical pixels a side of a tilted view's band is drawn with: past it, at a lower
/// pixel density. (`Camera::tilt_bands` limits a band's logical size the same way.)
const MAX_BAND_PX: f64 = 4096.0;

/// The view a band of a tilted `view` is drawn with: at the view's pixel density, or coarser
/// for a long band.
fn band_view(view: &View, b: &rustgis_map::camera::Band) -> View {
    let ppp = view.pixels_per_point.min(MAX_BAND_PX / b.width.max(b.height).max(1.0));
    View { width: b.width, height: b.height, camera: b.camera, pixels_per_point: ppp, ..*view }
}

/// Reads what a tilted view shows of the map's streamed layers, without waiting, as drawing it
/// does: a copy of the map drawn off the UI thread then finds the data already in place, and the
/// map itself holds what the view shows. True while reads are still on their way.
pub fn sync_tilted_view(map: &mut Map, view: &View) -> bool {
    let bands = view.camera.tilt_bands(view.width, view.height);
    sync_tilted(map, view, &bands, false)
}

/// Reads the streamed layers once for a tilted `view`: every layer drawn in one of its `bands`
/// (each drawn at its own scale, coarser far away). Reads are kept per extent, so a layer drawn
/// in two bands asks for the same extent twice and reads once. Returns whether a read is still
/// running.
fn sync_tilted(map: &mut Map, view: &View, bands: &[rustgis_map::camera::Band], block: bool) -> bool {
    let lonlat = view_lonlat(map, view);
    let mut pending = false;
    for b in bands {
        let scale = map.ground_res_at(&b.camera) / rustgis_geom::PIXEL_M;
        pending |= map.sync_sources(&lonlat, scale, block);
    }
    pending
}

/// Fills a tilted view's picture band by band: each pixel takes its colour from the nearest
/// band under it, fading into the fog toward the view's reach; then the sky above the horizon
/// shades from the fog to the sky colour at the top.
struct Warp {
    look: rustgis_map::camera::GroundLookup,
    /// Physical pixels per logical point, across and down.
    k: (f64, f64),
    sky_y: f64,
    sky: [f32; 4],
    fog: [f32; 4],
    /// The pixels a band has given a colour.
    filled: Vec<bool>,
}

impl Warp {
    fn new(view: &View, map: &Map) -> Warp {
        let (pw, ph) = view.pixel_size();
        let (sky, fog) = tilt_sky(map);
        let rgba = |c: Rgba| [f32::from(c.r()), f32::from(c.g()), f32::from(c.b()), 255.0];
        Warp {
            look: view.camera.ground_lookup(view.width, view.height),
            k: (f64::from(pw) / view.width.max(1e-9), f64::from(ph) / view.height.max(1e-9)),
            sky_y: view.camera.sky_y(view.height).unwrap_or(0.0).max(1e-9),
            sky: rgba(sky),
            fog: rgba(fog),
            filled: vec![false; pw as usize * ph as usize],
        }
    }

    /// The logical point at the centre of pixel (px, py).
    fn point(&self, px: usize, py: usize) -> (f64, f64) {
        ((px as f64 + 0.5) / self.k.0, (py as f64 + 0.5) / self.k.1)
    }

    /// Colours the pixels still empty that band picture `pic` (drawn for `v`) has under them,
    /// on the screen rows between `top` and `bottom` (logical points) where its ground lies.
    fn band(&mut self, pm: &mut Pixmap, v: &View, pic: &Pixmap, top: f64, bottom: f64) {
        let (pw, ph) = (pm.width() as usize, pm.height() as usize);
        let far = rustgis_map::camera::FAR;
        // A row's worth of slack each way for sampling at the edges.
        let row = |y: f64| ((y * self.k.1).floor().max(0.0) as usize).min(ph);
        let (r0, r1) = (row(top - 1.0 / self.k.1), (row(bottom + 2.0 / self.k.1)).min(ph));
        let out = pm.pixels_mut();
        let span = r0 * pw..r1.max(r0) * pw;
        for (i, filled) in self.filled.iter_mut().enumerate().take(span.end).skip(span.start) {
            if *filled {
                continue;
            }
            let (x, y) = ((i % pw) as f64 + 0.5, (i / pw) as f64 + 0.5);
            let Some((m, depth)) = self.look.at(x / self.k.0, y / self.k.1) else { continue };
            let Some(ground) = sample_band(v, pic, m) else { continue };
            // Fog from halfway to the reach.
            let t = ((depth - far * 0.5) / (far * 0.5)).clamp(0.0, 1.0) as f32;
            if let Some(dst) = out.get_mut(i) {
                *dst = premultiplied(mix(ground, self.fog, t * t * (3.0 - 2.0 * t)));
            }
            *filled = true;
        }
    }

    /// Fills what no band covered: the sky, and fog on ground past every band.
    fn finish(&self, pm: &mut Pixmap) {
        let pw = pm.width() as usize;
        let out = pm.pixels_mut();
        for (i, dst) in out.iter_mut().enumerate() {
            if self.filled.get(i).copied().unwrap_or(true) {
                continue;
            }
            let (x, y) = self.point(i % pw.max(1), i / pw.max(1));
            *dst = premultiplied(match self.look.at(x, y) {
                Some(_) => self.fog,
                None => mix(self.sky, self.fog, (y / self.sky_y).clamp(0.0, 1.0) as f32),
            });
        }
    }
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// A premultiplied pixel from 0–255 premultiplied channels.
fn premultiplied(c: [f32; 4]) -> tiny_skia::PremultipliedColorU8 {
    let a = c[3].clamp(0.0, 255.0);
    let ch = |v: f32| v.clamp(0.0, a).round() as u8;
    tiny_skia::PremultipliedColorU8::from_rgba(ch(c[0]), ch(c[1]), ch(c[2]), a.round() as u8)
        .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT)
}

/// The colour (premultiplied, 0–255) of band picture `pm`, drawn for `v`, at Mercator point
/// `m`, sampled bilinearly; `None` outside it.
fn sample_band(v: &View, pm: &Pixmap, m: Coord) -> Option<[f32; 4]> {
    let (bw, bh) = (pm.width() as f64, pm.height() as f64);
    let (sx, sy) = v.camera.to_screen(m, v.width, v.height);
    let (fx, fy) = (sx * bw / v.width.max(1e-9) - 0.5, sy * bh / v.height.max(1e-9) - 0.5);
    if !(fx >= -0.5 && fy >= -0.5 && fx <= bw - 0.5 && fy <= bh - 0.5) {
        return None;
    }
    let (x0, y0) = (fx.floor().max(0.0), fy.floor().max(0.0));
    let (tx, ty) = ((fx - x0).clamp(0.0, 1.0) as f32, (fy - y0).clamp(0.0, 1.0) as f32);
    let (x0, y0) = (x0 as usize, y0 as usize);
    let (x1, y1) = ((x0 + 1).min(pm.width() as usize - 1), (y0 + 1).min(pm.height() as usize - 1));
    let px = pm.pixels();
    let at = |x: usize, y: usize| px.get(y * pm.width() as usize + x).map(|p| [p.red(), p.green(), p.blue(), p.alpha()]);
    let (a, b, c, d) = (at(x0, y0)?, at(x1, y0)?, at(x0, y1)?, at(x1, y1)?);
    Some(std::array::from_fn(|i| {
        let top = f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * tx;
        let bottom = f32::from(c[i]) + (f32::from(d[i]) - f32::from(c[i])) * tx;
        top + (bottom - top) * ty
    }))
}

/// A view turned to a bearing, on the CPU: the map is drawn facing north over the box around
/// the turned view, then turned into place. Labels are placed in the turned view and drawn
/// upright, as MapLibre keeps them.
fn render_turned(map: &mut Map, view: &View, full_res_rasters: bool, record: bool) -> Option<Rendered> {
    let (bw, bh) = view.camera.rotated_size(view.width, view.height);
    let north = View { width: bw.ceil() + 2.0, height: bh.ceil() + 2.0, camera: Camera { bearing: 0.0, ..view.camera }, ..*view };
    let Some(RenderOutput::Cpu(mut r)) = render_frame(map, &north, full_res_rasters, Backend::Cpu, Labels::Turned(view), record) else {
        return None;
    };
    // A recorded north-up frame is placed turned as it is: its vectors turn with it.
    let recorded = r.recorded.take();
    let (pw, ph) = recorded.as_ref().map_or((r.pixmap.width(), r.pixmap.height()), |c| (c.width(), c.height()));
    let (w, h) = view.pixel_size();
    let mut pm = if recorded.is_some() { Canvas::recording(w, h)? } else { Canvas::new(w, h)? };
    // Physical pixels per logical point of each frame (the north-up one may have been capped
    // at MAX_SIDE).
    let k_out = w as f64 / view.width.max(1e-9);
    let k_in = pw as f64 / north.width.max(1e-9);
    let s = k_out / k_in.max(1e-9);
    let b = view.camera.bearing.to_radians();
    let (c, n) = (b.cos() * s, b.sin() * s);
    let (iw, ih) = (pw as f64 / 2.0, ph as f64 / 2.0);
    let (tx, ty) = (w as f64 / 2.0 - (c * iw + n * ih), h as f64 / 2.0 - (c * ih - n * iw));
    let t = Transform::from_row(c as f32, -n as f32, n as f32, c as f32, tx as f32, ty as f32);
    match recorded {
        Some(child) => pm.place(child, t, 1.0, rustgis_map::effects::Blend::Normal, None),
        None => {
            let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() };
            pm.draw_pixmap(0, 0, r.pixmap.as_ref(), &paint, t, None);
        }
    }
    for l in &r.labels {
        if let Some(icon) = &l.icon {
            draw_icon(&mut pm, icon, view.pixels_per_point as f32);
        }
    }
    if pm.is_recording() {
        return Some(Rendered { pixmap: Pixmap::new(1, 1)?, recorded: Some(pm), ..r });
    }
    Some(Rendered { pixmap: pm.into_pixmap(), ..r })
}

/// Draws one north-up frame. `labels_to` says where its labels go: a turned or tilted view the
/// frame stands in for places them itself (and draws their icons). `full_res_rasters` marks an
/// export, which draws everything as it will stay: rasters at full resolution, and labels in
/// their installed font families, waiting for those to be indexed (`families::find`). A frame on
/// screen passes `false`, and never waits for the index (`families::find_ready`).
fn render_frame(
    map: &mut Map,
    view: &View,
    full_res_rasters: bool,
    backend: Backend,
    mut labels_to: Labels<'_>,
    record: bool,
) -> Option<RenderOutput> {
    let (w, h) = view.pixel_size();
    // Kept as vectors (`record`) on the CPU, for a flat frame without view effects or a
    // colour-vision simulation it applies (pixel filters over the whole frame; an export,
    // `full_res_rasters`, never simulates colour vision); anything else draws its pixels.
    let record = record
        && !backend.is_gpu()
        && !map.projection.is_globe()
        && map.view.effects.is_none()
        && (full_res_rasters || map.view.color_vision == rustgis_map::view::ColorVision::Normal);
    let mut pm = if record { Canvas::recording(w, h)? } else { Canvas::new(w, h)? };
    // Streamed layers read the view: in the background for the screen, in place for an export
    // (the full-resolution render), which waits for them; only the layers drawn at the view's
    // scale read. A tilted view's bands share its read.
    let scale = map.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M;
    // The labels a tilted view's bands hand back stand in that view, beside its upright markers:
    // sized, for a reference scale, at its scale rather than the band's.
    let label_scale = match &labels_to {
        Labels::Tilted(to, _) => map.ground_res_at(&to.camera) / rustgis_geom::PIXEL_M,
        _ => scale,
    };
    let data_pending = match labels_to {
        Labels::Tilted(..) | Labels::Shared => false,
        _ => map.sync_sources(&view_lonlat(map, view), scale, full_res_rasters),
    };
    let globe = map.projection.is_globe().then(|| Globe::new(&view.camera, view.width, view.height));
    let (flat, key) = (map.flat(), map.projection.code());
    let custom = matches!(flat, rustgis_map::projection::Flat::Custom(_)) && globe.is_none();
    let _antialias = Antialias::set(!map.view.fast);
    let ctx = Ctx {
        view,
        ppp: view.pixels_per_point as f32,
        wireframe: map.view.wireframe,
        cull: cull_extent(view, CULL_MARGIN),
        globe,
        sprites: Default::default(),
        bursts: Default::default(),
    };
    // The Gpu backend draws flat, effect-free frames; anything else stays on the CPU pixmap.
    let gpu = backend.is_gpu()
        && globe.is_none()
        && map.view.effects.is_none()
        && map.view.color_vision == rustgis_map::view::ColorVision::Normal;
    // The effective backend: the Gpu arm at the end only runs when the frame is compatible.
    let backend = if gpu { backend } else { Backend::Cpu };
    // GPU draw commands; between them, tiny-skia-only content accumulates in `pm` and is
    // flushed as ordered Image commands (`flush_acc`, gated by `cpu_dirty`).
    let mut cmds: Vec<gpu_scene::DrawCmd> = Vec::new();
    // Map units per physical pixel: the decimation threshold scale (`Ctx::ring_path`'s 0.35 px
    // rule is applied in screen space) and the stroke shader's width scale.
    let map_per_px = view.camera.res / view.pixels_per_point;
    // On the globe nothing is culled by the flat extent: the horizon clips instead.
    let visible = |e: &Extent| match &globe {
        Some(g) => g.may_see(e),
        None => e.intersects(&ctx.cull),
    };
    // GPU meshes reach further (`GPU_CULL_MARGIN`), so the camera fast path can move them.
    let mesh_cull = cull_extent(view, GPU_CULL_MARGIN);
    let mesh_visible = |e: &Extent| match &globe {
        Some(g) => g.may_see(e),
        None => e.intersects(&mesh_cull),
    };

    // Basemap.
    let (water, land, outline, grat) = map.palette();
    match &globe {
        Some(g) => globe_backdrop(pm.pixels(), g, water, space(view, map), view.pixels_per_point as f32),
        None if !gpu => pm.fill(tiny_skia::Color::from_rgba8(water.r(), water.g(), water.b(), 255)),
        // The Gpu scene carries the water backdrop as its clear colour instead — an
        // always-full-screen Image here would poison `has_images` and kill the camera
        // fast path.
        None => {}
    }
    // Set when tiny-skia actually rasterizes into the accumulator.
    let mut cpu_dirty = false;
    if map.draws_world_outline() {
        let fill = paint(land, 1.0);
        let stroke = (outline.a() > 0).then(|| (paint(outline, 1.0), 0.6));
        let custom_world = custom.then(|| world_in(&flat, &key));
        let outlines: &Vec<Geometry> = match (&globe, &custom_world) {
            (Some(_), _) => world_lonlat(),
            (None, Some(w)) => w,
            (None, None) => world(),
        };
        for g in outlines {
            if let Geometry::Polygon(polys) = g
                && visible(&g.extent())
            {
                let mut tessellated = false;
                if gpu {
                    flush_acc(pm.pixels(), &mut cmds, &mut cpu_dirty)?;
                    tessellated = mesh::collect_basemap(&mut cmds, polys, &g.extent(), land, outline, ctx.ppp, map_per_px).is_ok();
                }
                if gpu && !tessellated {
                    // Lyon refused, or it's too large for f32 this far in: tiny-skia still draws it.
                    ctx.polygon(&mut pm, polys, Some(fill.clone()), stroke.clone());
                    cpu_dirty = true;
                } else if !gpu {
                    ctx.polygon(&mut pm, polys, Some(fill.clone()), stroke.clone());
                }
            }
        }
    }
    if map.graticule && grat.a() > 0 {
        let p = paint(grat, 1.0);
        let step = if view.camera.res > 8000.0 {
            30.0
        } else if view.camera.res > 2000.0 {
            15.0
        } else if view.camera.res > 400.0 {
            5.0
        } else {
            1.0
        };
        let (lat_max, lat_min) = if globe.is_some() { (90.0, -90.0) } else { (85.0, -85.0) };
        let mut lines = Vec::new();
        let mut lon = -180.0;
        while lon <= 180.0 {
            lines.push((0..=36).map(|i| Coord::new(lon, lat_min + (lat_max - lat_min) * i as f64 / 36.0)).collect::<Vec<_>>());
            lon += step;
        }
        let mut lat = -90.0 + step;
        while lat < 90.0 {
            lines.push((0..=72).map(|i| Coord::new(-180.0 + i as f64 * 5.0, lat)).collect::<Vec<_>>());
            lat += step;
        }
        let lines = if globe.is_some() {
            lines
        } else if custom {
            // Parallels are cut at the map's edge like any line (#209).
            match flat.project(&Geometry::Line(lines)) {
                Geometry::Line(parts) => parts,
                _ => Vec::new(),
            }
        } else {
            lines.into_iter().map(|l| l.into_iter().map(lonlat_to_merc).collect()).collect()
        };
        if gpu {
            let mut e = Extent::EMPTY;
            for l in &lines {
                for c in l {
                    e.expand(*c);
                }
            }
            flush_acc(pm.pixels(), &mut cmds, &mut cpu_dirty)?;
            let tessellated = !e.is_empty() && mesh::collect_graticule(&mut cmds, &lines, &e, grat, ctx.ppp, map_per_px).is_ok();
            if !tessellated {
                ctx.line(&mut pm, &lines, &p, 0.6);
                cpu_dirty = true;
            }
        } else {
            ctx.line(&mut pm, &lines, &p, 0.6);
        }
    }

    // Web tile layers, at their places in the drawing order (#808): those under all the map's
    // layers here, over the built-in basemap and its graticule; the others over their layer.
    let tile_labels = !map.label_display.paused && map.label_lock.is_none();
    let mut tile_out = tiles::TileOutput::default();
    // Each tile layer's place, worked out once a frame.
    let tile_slots = map.tile_slots();
    macro_rules! draw_tiles {
        ($over:expr) => {{
            let over: Option<u32> = $over;
            let which: Vec<usize> = tile_slots.iter().enumerate().filter(|(_, s)| **s == over).map(|(i, _)| i).collect();
            if !which.is_empty() {
                let tile_place = match &globe {
                    Some(g) => raster::Place::Globe(g),
                    None => raster::Place::Flat(&flat, &key),
                };
                let drawn = |t: &&rustgis_map::TileLayer| t.draws();
                let shown = which.iter().filter_map(|i| map.tiles.get(*i)).filter(drawn).any(|t| t.opacity > 0.0);
                // Hidden tile layers draw nothing: marking dirty anyway would emit an empty
                // full-screen Image command (a pointless upload, and it poisons the camera fast
                // path's `has_images`). On the GPU raster tile layers are placed as tiles, which
                // move with the camera.
                let tile_gpu = (gpu && shown).then_some(tiles::Gpu {
                    cmds: &mut cmds,
                    dirty: &mut cpu_dirty,
                    // A tilted view's bands are drawn at their own densities: their tiles as pictures.
                    meshes: !matches!(labels_to, Labels::Tilted(..)),
                    // Only a frame for the view itself (not a turned or tilted view's north-up stand-in).
                    live: backend == Backend::GpuLive && matches!(labels_to, Labels::Own),
                });
                let blended = |i: &usize| map.tiles.get(*i).is_some_and(|t| t.blend != rustgis_map::effects::Blend::Normal);
                if pm.is_recording() && which.iter().any(blended) {
                    // A recording's pixels hold nothing of the vectors under them, so a blended
                    // tile layer is drawn alone (over nothing its blend leaves it as it is) and
                    // placed with its blend; the others go on the pixels, each in its turn.
                    for i in which.iter().rev() {
                        let one = std::slice::from_ref(i);
                        if blended(i) {
                            let Some(mut own) = Pixmap::new(w, h) else { continue };
                            tile_out.add(tiles::draw(&mut own, map, one, view, tile_place, tile_labels, None));
                            let blend = map.tiles.get(*i).map_or(tiny_skia::BlendMode::SourceOver, |t| blend_mode(t.blend));
                            pm.draw_pixmap(
                                0,
                                0,
                                own.as_ref(),
                                &PixmapPaint { blend_mode: blend, ..PixmapPaint::default() },
                                Transform::identity(),
                                None,
                            );
                        } else {
                            tile_out.add(tiles::draw(pm.pixels(), map, one, view, tile_place, tile_labels, None));
                        }
                    }
                } else {
                    tile_out.add(tiles::draw(pm.pixels(), map, &which, view, tile_place, tile_labels, tile_gpu));
                }
                if !gpu {
                    cpu_dirty |= which.iter().filter_map(|i| map.tiles.get(*i)).any(|t| t.draws());
                }
            }
        }};
    }
    draw_tiles!(None);

    // Layers, bottom first.
    let mut label_candidates: Vec<Candidate> = Vec::new();
    let display = map.label_display;
    if !display.locked {
        map.label_lock = None;
        map.label_lock_line_anchors.clear();
    }
    let lock: Option<std::collections::HashMap<rustgis_map::LabelKey, rustgis_map::LockedPlace>> =
        map.label_lock.as_ref().map(|v| v.iter().copied().collect());
    // The classes of features with a label locked (any of its repeats), to pass the rest over early.
    let locked_classes: Option<std::collections::HashSet<(u32, u64, usize)>> =
        lock.as_ref().map(|k| k.keys().map(|&(l, r, c, _)| (l, r, c)).collect());
    let distance = viewing_distance(map.ground_res_at(&view.camera), view.height);
    // Graphics text: always drawn, and other labels keep clear of it.
    let mut fixed_labels: Vec<PlacedLabel> = Vec::new();
    let tokens = rustgis_map::graphics::TokenValues::of(map, scale);
    let n_layers = map.layers.len();
    // Masks: layers shown only inside another layer's polygons, or left of the swipe divider.
    let masks = layer_masks(map, &ctx, w, h, &flat, &key, scale)?;
    // Bottom first; a higher depth priority draws later (on top) whatever the order.
    let mut order: Vec<usize> = (0..n_layers).rev().collect();
    order.sort_by_key(|i| map.layers.get(*i).map_or(0, |l| l.priority));
    let clip = map.view.clip_distance.map(|d| (view.camera.center_lonlat(), d));
    // Each layer group's visibility, scale range and transparency apply to its layers, its
    // subgroups' too (#413, #836).
    let held = map.held_by_group(scale);
    let group_opacity = map.group_opacities();
    // The point layers whose markers stand up over a tilted view (`upright_markers`) rather than
    // lie in its bands.
    let upright: std::collections::HashSet<u32> = match labels_to {
        Labels::Tilted(..) | Labels::Shared => map.layers.iter().filter(|l| stands_up(map, l)).map(|l| l.id).collect(),
        _ => Default::default(),
    };
    // Extrusion heights are metres on the map's body.
    let body = map.body().ratio();
    // The layer drawn last, whose tile layers go over it before the next.
    let mut below: Option<u32> = None;
    for li in order {
        if let Some(id) = below.take() {
            draw_tiles!(Some(id));
        }
        below = map.layers.get(li).map(|l| l.id);
        let Some(layer) = map.layers.get_mut(li) else { continue };
        if !layer.visible || !layer.in_scale_range(scale) || held.contains(&layer.id) {
            continue;
        }
        let opacity = layer.opacity() * group_opacity.get(&layer.id).copied().unwrap_or(1.0);
        let mask = masks.iter().find(|(id, _)| *id == layer.id).map(|(_, m)| m);
        let (blend, layer_blend) = (blend_mode(layer.blend), layer.blend);
        // Drawn on its own and composited when it is see-through, masked or blended.
        let offscreen = opacity < 0.999 || mask.is_some() || layer.blend != rustgis_map::effects::Blend::Normal;
        let composite = |pm: &mut Canvas, target: &Pixmap| {
            // A recording keeps the masked layer as a picture cut by its mask, placed with its
            // opacity and blend over the vectors beneath.
            if pm.is_recording()
                && let Some(m) = mask
            {
                let mut cut = target.clone();
                cut.apply_mask(m);
                let paint = PixmapPaint { opacity, blend_mode: blend, ..PixmapPaint::default() };
                pm.draw_pixmap(0, 0, cut.as_ref(), &paint, Transform::identity(), None);
                return;
            }
            pm.draw_pixmap(
                0,
                0,
                target.as_ref(),
                &PixmapPaint { opacity, blend_mode: blend, ..PixmapPaint::default() },
                Transform::identity(),
                mask,
            );
        };
        if let Some(rl) = &layer.raster {
            if let Some(data) = &rl.data {
                let place = match &globe {
                    Some(g) => raster::Place::Globe(g),
                    None => raster::Place::Flat(&flat, &key),
                };
                let stats = rl.masked_stats();
                let stats = stats.as_ref().map(|s| s.as_slice());
                // Read in place: the part in view, at the level the view needs.
                let address = rl.path.as_deref().filter(|_| rl.in_place);
                let mut draw_it = |pm: &mut Pixmap, opacity: f32| match address {
                    // Drawn from less than the view needs (a coarser level, tiles that couldn't be
                    // read): shown, and said with the tile errors.
                    Some(a) => match raster::draw_in_place(pm, a, data, &rl.style, stats, view, opacity, full_res_rasters, place) {
                        Ok(pending) => {
                            tile_out.pending |= pending;
                            if pending && !tile_out.pending_in_place.contains(&layer.id) {
                                tile_out.pending_in_place.push(layer.id);
                            }
                        }
                        Err(e) => {
                            let e = format!("{}: {e}", layer.name);
                            if !tile_out.errors.contains(&e) {
                                tile_out.errors.push(e);
                            }
                            if !tile_out.partial.contains(&layer.id) {
                                tile_out.partial.push(layer.id);
                            }
                        }
                    },
                    None => raster::draw(pm, data, &rl.style, stats, view, opacity, full_res_rasters, place),
                };
                if offscreen {
                    let mut target = Pixmap::new(w, h)?;
                    draw_it(&mut target, 1.0);
                    composite(&mut pm, &target);
                } else {
                    draw_it(pm.pixels(), opacity);
                }
                cpu_dirty = true;
            }
            continue;
        }
        let source = layer.source.as_ref().and_then(|s| s.chunk_source());
        let complete_source = source.is_some();
        // LOD is chosen for the entire layer, not separately for each chunk. Both the
        // activation threshold and the required saving must match an ordinary whole read.
        let thin_tol = view.lod_tolerance();
        let mut thin_complete = false;
        if globe.is_none()
            && layer.geometry_type() != GeometryType::Point
            && let (Some(owner), Some(source), Some(level)) = (&layer.source, &source, rustgis_map::lod::level(thin_tol))
        {
            let result = owner.cached_thinning(&key, level, || {
                let (mut eligible, mut all, mut kept) = (0usize, 0usize, 0usize);
                visit_complete(source.as_ref(), &mut |features| {
                    for feature in features {
                        if let Some(g) = feature.geometry {
                            let tolerance = rustgis_map::lod::tolerance(level);
                            let (counted, thinned) = if matches!(flat, rustgis_map::projection::Flat::Mercator) {
                                let thinned = rustgis_map::lod::project_thinned(&g, &flat, tolerance);
                                (g, thinned)
                            } else {
                                let projected = flat.project(&g);
                                let thinned = rustgis_map::lod::thin(&projected, tolerance);
                                (projected, thinned)
                            };
                            eligible = eligible.saturating_add(rustgis_map::lod::thinnable(&counted));
                            all = all.saturating_add(rustgis_map::lod::vertices(&counted));
                            kept = kept.saturating_add(rustgis_map::lod::vertices(&thinned));
                        }
                    }
                    Ok(())
                })?;
                Ok(eligible > rustgis_map::lod::THIN_ABOVE && kept as f64 <= all as f64 * (1.0 - rustgis_map::lod::MIN_SAVING))
            });
            match result {
                Ok(decision) => thin_complete = decision,
                Err(e) => {
                    if let Some(s) = &mut layer.source {
                        s.error = Some(e.to_string());
                    }
                    return None;
                }
            }
        }

        // A recording keeps a see-through or blended layer as a group of its own; a masked one
        // (its mask is pixels) is drawn as pixels and composited as a picture.
        let mut target = match (offscreen, pm.is_recording() && mask.is_none()) {
            (true, true) => pm.sub(w, h)?,
            (true, false) => Canvas::new(w, h)?,
            (false, _) => Canvas::new(1, 1)?,
        };
        let direct = !offscreen;
        let mut bins = effects::AggregationBins::default();
        let mut aggregate_symbol = None;
        // Only selected geometry is retained; paint directly after the whole layer to avoid
        // a second full-size canvas and transparent-intermediate rounding differences.
        let selected_order: std::collections::HashMap<u64, usize> = if complete_source {
            layer.selection.iter().enumerate().filter_map(|(rank, row)| layer.dataset.features.get(*row).map(|f| (f.id, rank))).collect()
        } else {
            Default::default()
        };
        let mut highlights: Vec<(usize, Geometry, Symbol, Vec<LineStroke>, f32)> = Vec::new();
        // The GPU mesh vertices the layer has left: a streamed layer's batches share them, so
        // one too dense to mesh goes to the CPU however it is split.
        let mut mesh_room = mesh::MAX_LAYER_VERTICES;
        let mut extrusions = if complete_source
            && layer.geometry_type() == GeometryType::Polygon
            && layer.extrusion.is_some()
            && globe.is_none()
            && !matches!(labels_to, Labels::Tilted(..) | Labels::Shared)
        {
            match effects::ExtrusionSpool::new() {
                Ok(spool) => Some(spool),
                Err(e) => {
                    if let Some(source) = &mut layer.source {
                        source.error = Some(e.to_string());
                    }
                    return None;
                }
            }
        } else {
            None
        };
        // A layer with a reference scale draws its symbols (and labels) grown or shrunk
        // with the map (built once for all of a streamed layer's batches): its points are wider or narrower in pixels.
        let size_factor = layer.size_factor(scale);
        let scaled_ctx;
        let ctx = if size_factor == 1.0 {
            &ctx
        } else {
            // Its own sprite cache: the sprites are drawn at the scaled size.
            scaled_ctx = Ctx { ppp: ctx.ppp * size_factor, sprites: Default::default(), bursts: std::rc::Rc::clone(&ctx.bursts), ..ctx };
            &scaled_ctx
        };
        let mut draw_chunk = |layer: &mut rustgis_map::Layer| -> rustgis_data::Result<()> {
            // The definition query and the display filters that apply at this scale (a feature draws
            // only if it matches all of them).
            let mut rows = layer.drawn_rows(scale);
            // View clipping: features entirely beyond the distance from the view's centre are left
            // out; one reaching into it (its box's nearest point within the distance) still draws.
            if let Some((centre, d)) = clip {
                for (r, keep) in rows.iter_mut().enumerate() {
                    if *keep {
                        let e =
                            layer.dataset.features.get(r).and_then(|f| f.geometry.as_ref()).map(|g| g.extent()).filter(|e| !e.is_empty());
                        *keep = e.is_some_and(|e| {
                            let near = Coord::new(centre.x.clamp(e.min_x, e.max_x), centre.y.clamp(e.min_y, e.max_y));
                            rustgis_geom::haversine_m(centre, near) <= d
                        });
                    }
                }
            }
            // Exact positions are needed by labels, selection and effects. Plain flat layers can
            // project only their retained display vertices, leaving interaction geometry lazy.
            let display_only = globe.is_none()
                && (!layer.labels.enabled || display.paused)
                && layer.selection.is_empty()
                && !layer.graphics
                && layer.aggregation.is_none()
                && layer.extrusion.is_none()
                && (!complete_source || thin_complete)
                && layer.diagram.is_none()
                && layer.prepare_display(&flat, thin_tol, complete_source);
            if globe.is_none() && !display_only {
                if complete_source {
                    layer.projected_in(&flat, &key);
                } else {
                    layer.thinned_in(&flat, &key, thin_tol);
                }
            }
            #[cfg(test)]
            if complete_source && globe.is_none() {
                let (display, exact) = stream_tests::LOD_PATHS.get();
                stream_tests::LOD_PATHS
                    .set((display + usize::from(display_only), exact + usize::from(!layer.projected_cache().is_empty())));
            }
            // Everything below only reads the layer: borrow rather than copy its data each frame.
            let layer: &rustgis_map::Layer = layer;
            // Its symbol by expression, compiled once for the drawing (none: the layer's own).
            let styler = layer.styler(Some(scale));
            // A label is named by its feature's id, not its row: a lock holds through edits that
            // move rows (#1046).
            let label_id = |row: usize| layer.dataset.features.get(row).map_or(row as u64, |f| f.id);
            let (labels, selection, ds_ptr) = (&layer.labels, &layer.selection, &layer.dataset);
            // The globe draws the lon/lat geometry itself; the flat map its Web Mercator copy.
            let geoms: Vec<Option<&Geometry>> = if globe.is_some() {
                ds_ptr.features.iter().map(|f| f.geometry.as_ref()).collect()
            } else if display_only {
                layer.display_cache().map_or(&[][..], |(g, _)| g).iter().map(Option::as_ref).collect()
            } else {
                layer.projected_cache().iter().map(Option::as_ref).collect()
            };
            // What the features draw from, and their extents for culling (on the flat map, cached).
            // Only the fills and strokes below draw thinned: the selection, labels, aggregation and
            // extrusion keep `geoms`, the full geometry, for their exact positions.
            let chunk_thinned: Vec<Option<Geometry>> = if thin_complete && !display_only {
                let tolerance = rustgis_map::lod::level(thin_tol).map_or(thin_tol, rustgis_map::lod::tolerance);
                if matches!(flat, rustgis_map::projection::Flat::Mercator) {
                    ds_ptr
                        .features
                        .iter()
                        .map(|f| f.geometry.as_ref().map(|g| rustgis_map::lod::project_thinned(g, &flat, tolerance)))
                        .collect()
                } else {
                    geoms.iter().map(|g| g.map(|g| rustgis_map::lod::thin(g, tolerance))).collect()
                }
            } else {
                Vec::new()
            };
            let drawn: Vec<Option<&Geometry>> = if globe.is_some() || display_only {
                geoms.clone()
            } else if complete_source {
                if thin_complete { chunk_thinned.iter().map(Option::as_ref).collect() } else { geoms.clone() }
            } else {
                layer.thinned_cache(thin_tol).iter().map(Option::as_ref).collect()
            };
            let extents = if globe.is_some() {
                &[][..]
            } else if display_only {
                layer.display_cache().map_or(&[][..], |(_, e)| e)
            } else {
                layer.projected_extents()
            };
            // GPU-eligible: a plain vector layer drawn at full opacity with no mask, blend,
            // aggregation, extrusion or per-row graphics (an inert aggregation/extrusion on the
            // "wrong" geometry type still falls back — conservative and correct).
            // Stacked line strokes are offset in screen space, and stacked fills draw patterns:
            // the GPU meshes do neither.
            let gpu_layer = gpu
                && !offscreen
                && layer.aggregation.is_none()
                && layer.extrusion.is_none()
                && !layer.graphics
                && !layer.has_strokes()
                && !layer.has_fills()
                && layer.marker_rotation.is_none()
                && layer.diagram.is_none()
                // Symbols and classes by expression are styled per feature on the CPU.
                && layer.symbol_by.is_empty()
                && layer.renderer.expression().is_none()
                && !upright.contains(&layer.id);
            // Lyon refuses degenerate geometry tiny-skia draws fine, a layer too dense to mesh
            // (`MAX_LAYER_VERTICES`) is quicker splatted, and zoomed in far, features reaching too
            // far for f32 are drawn where they are by the CPU (#902): collect_layer reports all
            // three, and the layer runs the CPU branch below like any ineligible one. It meshes the
            // thinned copy the CPU draws. A turned or tilted view takes no CPU pictures
            // (`gpu_turned`), so there such a layer sends the whole view to the CPU renderer, not
            // just itself.
            let mut gpu_done = false;
            if gpu_layer {
                flush_acc(pm.pixels(), &mut cmds, &mut cpu_dirty)
                    .ok_or_else(|| rustgis_data::DataError::Format("cannot allocate render target".into()))?;
                let collected = mesh::collect_layer(
                    &mut cmds,
                    layer,
                    &drawn,
                    extents,
                    &rows,
                    &mesh_visible,
                    ctx.wireframe,
                    ctx.ppp,
                    map_per_px,
                    mesh_room,
                );
                // Once a batch is too dense, the layer's later ones fall back too (they get no
                // room); one lyon refused leaves the room to the batches after it.
                mesh_room = match collected {
                    Ok(used) => mesh_room.saturating_sub(used),
                    Err(mesh::TessError::TooDense) => 0,
                    Err(mesh::TessError::Refused | mesh::TessError::Imprecise) => mesh_room,
                };
                gpu_done = collected.is_ok();
            }
            // False for a layer drawn elsewhere: nothing goes into the picture for it.
            let mut drawn_here = true;
            if !gpu_done {
                let dest = if direct { &mut pm } else { &mut target };
                let aggregate = layer.aggregation.filter(|_| layer.geometry_type() == GeometryType::Point);
                let extrude = layer.extrusion.as_ref().filter(|_| layer.geometry_type() == GeometryType::Polygon && globe.is_none());
                if let Some(a) = aggregate {
                    bins.add((w, h), ctx, &geoms, &rows, a);
                    aggregate_symbol.get_or_insert_with(|| layer.symbol(0));
                } else if extrude.is_some() && matches!(labels_to, Labels::Tilted(..)) {
                    // Stood up in perspective over the tilted view (`render_tilted`); its labels still
                    // come from here.
                    drawn_here = false;
                } else if upright.contains(&layer.id) {
                    // Stood up facing the viewer over the tilted view (`upright_markers`); its labels
                    // still come from here.
                    drawn_here = false;
                } else if let Some(x) = extrude.filter(|_| !matches!(labels_to, Labels::Shared)) {
                    let px_per_m = view.pixels_per_point / (map_ground_res(view, &flat) * body);
                    if let Some(spool) = &mut extrusions {
                        spool.add(ctx, ds_ptr, &geoms, &rows, x, px_per_m, |r| styler.symbol(r))?;
                    } else {
                        effects::extrude(dest, ctx, ds_ptr, &geoms, &rows, x, px_per_m, |r| styler.symbol(r));
                    }
                } else {
                    let mut stroke_paths = stroke_paths::Paths::default();
                    // Symbol levels: lines with stacked strokes wait, and go down a level at a time
                    // after the rest (each batch of a streamed layer on its own).
                    let levels = layer.stroke_levels && layer.has_strokes();
                    // Polygons with stacked fills (patterns) draw them in place of their single fill.
                    let has_fills = layer.has_fills();
                    let mut stacked: Vec<(&[Vec<Coord>], Symbol, &[LineStroke])> = Vec::new();
                    for (row, g) in drawn.iter().enumerate() {
                        let Some(g) = g else { continue };
                        let e = extents.get(row).copied().unwrap_or_else(|| g.extent());
                        // A point's marks, shifted from it, may show while the point is off the view.
                        let marked_in = || {
                            if globe.is_some() || !matches!(g, Geometry::Point(_) | Geometry::MultiPoint(_)) {
                                return false;
                            }
                            let (sym, strokes) = styler.style(row);
                            let marks = styler.marks(row, strokes);
                            if marks.is_empty() {
                                return false;
                            }
                            // In the layer's pixels (`ctx` is scaled by its size factor).
                            let r = f64::from(point_marks::reach(&sym, &marks) * ctx.ppp) * map_per_px;
                            Extent::new(e.min_x - r, e.min_y - r, e.max_x + r, e.max_y + r).intersects(&ctx.cull)
                        };
                        if rows.get(row) != Some(&true) || !(visible(&e) || marked_in()) {
                            continue;
                        }
                        // A picture draws in its row's turn, so graphics keep their order. (On the globe
                        // the geometry and `px` are both longitude/latitude, so the corners land right.)
                        if layer.graphics && rustgis_map::graphics::kind(ds_ptr, row) == "picture" {
                            let e = g.extent();
                            let (x0, y0) = ctx.px(Coord::new(e.min_x, e.max_y));
                            let (x1, y1) = ctx.px(Coord::new(e.max_x, e.min_y));
                            let (pw, ph) = (dest.width() as f32, dest.height() as f32);
                            if x0.max(x1) >= 0.0 && x0.min(x1) <= pw && y0.max(y1) >= 0.0 && y0.min(y1) <= ph {
                                picture::draw(dest, ds_ptr, row, (x0, y0, x1, y1), if direct { opacity } else { 1.0 });
                            }
                            continue;
                        }
                        // Its symbol, and the layer's stacked strokes or the feature's class's own; a
                        // polygon's stacked fills too (none in wireframe, which draws outlines only),
                        // the feature classed once for all.
                        let (sym, strokes, fills) = if has_fills && !ctx.wireframe && matches!(g, Geometry::Polygon(_)) {
                            styler.style_and_fills(row)
                        } else {
                            let (sym, strokes) = styler.style(row);
                            (sym, strokes, &[] as &[rustgis_map::fills::FillLayer])
                        };
                        // A feature under a pixel across: blend its coverage into the pixels it touches
                        // (a dense layer seen whole is mostly these, and a path each is slow). Not
                        // stacked strokes: a splat would draw them unshifted, in one colour. Stacked
                        // fills show as the colour they make together at that size.
                        let splat_sym = if fills.is_empty() { sym } else { Symbol { fill: pattern_fill::tint(fills, sym.fill), ..sym } };
                        if globe.is_none()
                            && !layer.graphics
                            && strokes.is_empty()
                            && let Some(e) = extents.get(row)
                            && (ctx.splat(dest, e, g, &splat_sym)
                                || (!gpu && direct && fills.is_empty() && ctx.small_polygon(dest, e, g, &sym)))
                        {
                            continue;
                        }
                        // A point's marks (sized with an expression's size), or its marker turned by a
                        // field or an expression.
                        let turn = styler.marker_turn(row);
                        let marks = match g {
                            Geometry::Point(_) | Geometry::MultiPoint(_) => styler.marks(row, strokes),
                            _ => std::borrow::Cow::Borrowed(strokes),
                        };
                        match g {
                            Geometry::Point(c) if !point_marks::plain(&marks, turn) => ctx.marks(dest, *c, &sym, &marks, turn, None),
                            Geometry::MultiPoint(v) if !point_marks::plain(&marks, turn) => {
                                v.iter().for_each(|c| ctx.marks(dest, *c, &sym, &marks, turn, None))
                            }
                            Geometry::Line(parts) if !strokes.is_empty() && levels => stacked.push((parts, sym, strokes)),
                            Geometry::Line(parts) if !strokes.is_empty() => stroke_paths.draw(ctx, dest, parts, &sym, strokes),
                            Geometry::Polygon(polys) if !fills.is_empty() => ctx.filled(dest, polys, &sym, fills),
                            _ => ctx.feature(dest, g, &sym),
                        }
                    }
                    // A turn per feature and level it has strokes at, by level and then the features'
                    // order: a feature is visited only for its own levels, however many the layer has.
                    let mut turns: Vec<(u8, usize)> = stacked
                        .iter()
                        .enumerate()
                        .flat_map(|(f, (.., st))| st.iter().enumerate().map(move |(i, s)| (s.level_at(i), f)))
                        .collect();
                    turns.sort_unstable();
                    turns.dedup();
                    let mut one: Vec<LineStroke> = Vec::with_capacity(rustgis_map::symbology::MAX_STROKES);
                    for (level, f) in turns {
                        let Some((parts, sym, strokes)) = stacked.get(f) else { continue };
                        one.clear();
                        one.extend(strokes.iter().enumerate().filter(|(i, s)| s.level_at(*i) == level).map(|(_, s)| *s));
                        stroke_paths.draw(ctx, dest, parts, sym, &one);
                    }
                    // Diagrams: a small chart on each feature from several fields, over the
                    // renderer's own symbol (pie or bars, as ArcGIS Pro's Chart symbology draws).
                    if let Some(d) = layer.diagram.as_ref().filter(|_| !layer.graphics) {
                        let fields = d.field_indices(ds_ptr);
                        for (row, g) in geoms.iter().enumerate() {
                            let Some(g) = g else { continue };
                            if rows.get(row) != Some(&true) {
                                continue;
                            }
                            let e = extents.get(row).copied().unwrap_or_else(|| g.extent());
                            // Grown by the largest diagram, as a point's marks are (in map units).
                            let r = f64::from(diagrams::reach(d) * ctx.ppp) * map_per_px;
                            if !visible(&Extent::new(e.min_x - r, e.min_y - r, e.max_x + r, e.max_y + r)) {
                                continue;
                            }
                            let Some(c) = diagrams::anchor(g) else { continue };
                            let (x, y) = ctx.px(c);
                            let Some((values, total)) = d.values_at(ds_ptr, row, &fields) else { continue };
                            diagrams::draw(dest, x, y, d, &values, total, if direct { opacity } else { 1.0 }, ctx.ppp, display.paused);
                        }
                    }
                }
                cpu_dirty |= drawn_here;
            }
            if !complete_source {
                if let (Some(a), Some(base)) = (layer.aggregation, aggregate_symbol) {
                    bins.draw(if direct { &mut pm } else { &mut target }, ctx, a, &base);
                }
                if !direct {
                    if target.is_recording() {
                        if let Some(empty) = Canvas::new(1, 1) {
                            pm.place(std::mem::replace(&mut target, empty), Transform::identity(), opacity, layer_blend, None);
                        }
                    } else {
                        composite(&mut pm, target.pixmap());
                    }
                }
            }
            // An upright marker shows its own selection.
            for row in selection.iter().filter(|_| !upright.contains(&layer.id)) {
                if let Some(Some(g)) = geoms.get(*row)
                    && rows.get(*row) == Some(&true)
                {
                    // A text graphic's point is invisible: its label carries the frame (`framed`),
                    // drawn with the text wherever that lands (turned, tilted or on the globe).
                    if layer.graphics && rustgis_map::graphics::label(ds_ptr, *row).is_some() {
                        continue;
                    }
                    // Follow the layer's own backend: when it fell back to the CPU pixmap, the
                    // highlight must too — tessellated here, the layer's later-flushed Image would
                    // draw over it (the CPU path draws highlights over the layer).
                    if complete_source {
                        let rank = ds_ptr.features.get(*row).and_then(|f| selected_order.get(&f.id)).copied().unwrap_or(usize::MAX);
                        let (symbol, strokes) = styler.style(*row);
                        highlights.push((rank, (**g).clone(), symbol, styler.marks(*row, strokes).to_vec(), styler.marker_turn(*row)));
                    } else if gpu_done && mesh::collect_highlight(&mut cmds, g, &styler.symbol(*row), ctx.ppp, map_per_px).is_ok() {
                        // highlight tessellated into the scene, right after its layer
                    } else {
                        let (symbol, strokes) = styler.style(*row);
                        ctx.highlight(&mut pm, g, &symbol, &styler.marks(*row, strokes), styler.marker_turn(*row));
                        cpu_dirty = true;
                    }
                }
            }
            if layer.graphics {
                for (row, g) in geoms.iter().enumerate() {
                    let Some(g) = g else { continue };
                    if rows.get(row) != Some(&true) {
                        continue;
                    }
                    if let (Some(t), Some(c)) = (rustgis_map::graphics::label(ds_ptr, row), g.label_point())
                        && opacity > 0.0
                        && let Some((x, y)) = ctx.screen(c)
                    {
                        let base = rustgis_map::graphics::text_color(ds_ptr, row);
                        let color = base.with_alpha((base.a() as f32 * opacity).round() as u8);
                        fixed_labels.push(PlacedLabel {
                            text: rustgis_map::graphics::expand(t, &tokens),
                            x: x as f32,
                            y: y as f32,
                            size: rustgis_map::graphics::font_size(ds_ptr, row),
                            color,
                            halo: Some(halo_for(color)),
                            halo_width: None,
                            bold: rustgis_map::graphics::bold(ds_ptr, row),
                            icon: None,
                            align: 0.0,
                            framed: selection.contains(&row),
                            italic: rustgis_map::graphics::italic(ds_ptr, row),
                            spacing: rustgis_map::graphics::letter_spacing(ds_ptr, row),
                            font: None,
                            line_align: rustgis_map::graphics::align(ds_ptr, row),
                            angle: None,
                            collide: Default::default(),
                            glyphs: Vec::new(),
                            background: None,
                        });
                    }
                }
            }
            // Each label class labels every feature its clause takes, as in ArcGIS Pro (#732), when
            // shown, in range and on a field the layer has (#618) or by a text expression. Its
            // expressions are compiled once for the drawing.
            let label_exprs: Vec<Option<rustgis_map::label_by::LabelExprs>> = if labels.enabled && !display.paused {
                labels.in_order().map(|c| rustgis_map::label_by::LabelExprs::new(&c.by, ds_ptr, Some(label_scale))).collect()
            } else {
                Vec::new()
            };
            // For each class: `None` when it labels nothing here, else its field (if it has one).
            let label_fields: Vec<Option<Option<usize>>> = if labels.enabled && !display.paused {
                labels
                    .in_order()
                    .zip(&label_exprs)
                    .map(|(c, e)| {
                        let field = ds_ptr.field_index(&c.field);
                        let by_text = e.as_ref().is_some_and(|e| e.has_text());
                        (c.visible && c.in_range(scale, distance) && (field.is_some() || by_text)).then_some(field)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if label_fields.iter().any(Option::is_some) {
                let label_factor = layer.size_factor(label_scale);
                // Each class's font, where it is installed. An export (`full_res_rasters`: see
                // `render_frame`'s contract) waits for the font index; a frame on screen doesn't.
                let find = if full_res_rasters { families::find } else { families::find_ready };
                let class_fonts: Vec<Option<u16>> = labels.in_order().map(|c| c.font.as_deref().and_then(find)).collect();
                let classes = rustgis_map::LabelMatcher::new(labels, ds_ptr, Some(label_scale));
                let lid = layer.id;
                // Whether a class labels lines along them: those take the stretch of a line in
                // view, wherever its label point is.
                let along_line = |placement: LabelPlacement, g: &Geometry| placement.along_line() && matches!(g, Geometry::Line(_));
                let any_along = labels.in_order().zip(&label_fields).any(|(c, f)| f.is_some() && c.placement.along_line());
                // How far a class moves its labels at most (points, as placed below): a point that
                // far out of view may still have its label in it.
                let offset_reach = labels
                    .in_order()
                    .zip(&label_fields)
                    .filter(|(_, f)| f.is_some())
                    .map(|(c, _)| c.offset[0].abs().max(c.offset[1].abs()) * 1.33 * label_factor)
                    .filter(|r| r.is_finite())
                    .fold(0.0, f32::max);
                let locked_reach = lock
                    .as_ref()
                    .into_iter()
                    .flat_map(|locks| locks.iter())
                    .filter(|((layer, ..), _)| *layer == lid)
                    .filter_map(|(_, place)| place.callout)
                    .map(|[x, y]| x.abs().max(y.abs()))
                    .filter(|r| r.is_finite())
                    .fold(0.0, f32::max);
                let offset_reach = f64::from(offset_reach.max(locked_reach));
                let (vw, vh) = (ctx.view.width, ctx.view.height);
                // Labels moved by hand (the Move Label tool): any class's, its feature known by
                // its id. Where one was put is in view or not whatever its feature's place.
                let any_moved = labels.in_order().zip(&label_fields).any(|(c, f)| f.is_some() && !c.moved.is_empty());
                // (Kept as longitude and latitude: the flat map draws its projection's coordinates,
                // as its features' `geoms` are; the globe draws longitude and latitude.)
                let moved_to = |class: &rustgis_map::LabelClass, fid: Option<u64>| {
                    fid.and_then(|id| class.moved_to(id))
                        .map(|[x, y]| Coord::new(x, y))
                        .and_then(|c| if globe.is_some() { Some(c) } else { flat.forward_checked(c) })
                        .and_then(|c| ctx.screen(c))
                };
                for (row, g) in geoms.iter().enumerate() {
                    let Some(g) = g else { continue };
                    if rows.get(row) != Some(&true) {
                        continue;
                    }
                    let Some(c) = g.label_point() else { continue };
                    // Where the label point is on the screen, `None` when it is out of view (or
                    // behind the globe): then only a line labelled along it can be, on its
                    // stretch in view.
                    let point = if globe.is_some() || ctx.cull.contains(c) {
                        ctx.screen(c)
                    } else if offset_reach > 0.0 {
                        let r = offset_reach;
                        ctx.screen(c).filter(|&(x, y)| x >= -r && y >= -r && x <= vw + r && y <= vh + r)
                    } else {
                        None
                    };
                    let line_in_view = || globe.is_some() || ctx.cull.intersects(&extents.get(row).copied().unwrap_or_else(|| g.extent()));
                    let fid = layer.dataset.features.get(row).map(|f| f.id);
                    // (Its class's clause read only for a feature with a label moved.)
                    let moved_here = || {
                        any_moved
                            && fid.is_some_and(|id| labels.in_order().any(|c| c.moved_to(id).is_some()))
                            && classes.classes_of(ds_ptr, row).any(|(_, c)| moved_to(c, fid).is_some())
                    };
                    if point.is_none() && !(any_along && matches!(g, Geometry::Line(_)) && line_in_view()) && !moved_here() {
                        continue;
                    }
                    let is_point = g.kind() == GeometryType::Point;
                    // A line's longest part on the screen, for the classes placed along it: worked
                    // out once (its path each way too), for the first that asks.
                    let mut screen_line: Option<Option<std::sync::Arc<line_labels::Line>>> = None;
                    let marker = if is_point { styler.symbol(row).size } else { 0.0 };
                    // Bigger polygons get priority.
                    let area = match g {
                        Geometry::Polygon(_) => {
                            let e = g.extent();
                            // (An extent of no number counts as none.)
                            let a = (e.width() * e.height()).sqrt();
                            if a.is_finite() { a.clamp(0.0, 1e8) } else { 0.0 }
                        }
                        _ => 0.0,
                    };
                    // The clauses are read for the features in view only.
                    for (ci, class) in classes.classes_of(ds_ptr, row) {
                        // Locked, only the labels placed then: one class's, not every class's (and
                        // along a line, only its repeats placed then, below).
                        if locked_classes.as_ref().is_some_and(|k| !k.contains(&(lid, label_id(row), ci))) {
                            continue;
                        }
                        // Moved by hand: one label, upright, where it was put.
                        let moved = if any_moved { moved_to(class, fid) } else { None };
                        if point.is_none() && moved.is_none() && !along_line(class.placement, g) {
                            continue;
                        }
                        // Its feature's place, for a callout's line back to it: off the screen too
                        // when the label was moved into view.
                        let anchor = point.or_else(|| moved.and_then(|_| ctx.screen(c)));
                        // (Along a line, the label is put on its stretch in view below.)
                        let (x, y) = point.unwrap_or_default();
                        let Some(fi) = label_fields.get(ci).copied().flatten() else { continue };
                        let exprs = label_exprs.get(ci).and_then(Option::as_ref);
                        // Its text by expression, else from its field; none: no label.
                        let raw = match exprs.and_then(|e| e.text(ds_ptr, row)) {
                            Some(Some(t)) => t,
                            Some(None) => continue,
                            None => {
                                let Some(v) = fi.map(|fi| ds_ptr.value(row, fi)).filter(|v| !v.is_null()) else { continue };
                                v.to_string()
                            }
                        };
                        // Its size and colour by expression, else the class's.
                        let csize = exprs.and_then(|e| e.size(ds_ptr, row)).unwrap_or(class.size);
                        let ccolor = exprs.and_then(|e| e.color(ds_ptr, row)).unwrap_or(class.color);
                        // Wrapped into lines as its class says; not a label curved character by
                        // character along a line (one line of characters, as QGIS curves it).
                        // (One too long to curve goes straight, and is wrapped as a straight one.)
                        let curving = class.placement == LabelPlacement::Curved
                            && matches!(g, Geometry::Line(_))
                            && raw.graphemes(true).nth(line_labels::MAX_CURVED_CHARS).is_none();
                        let mut text =
                            if curving || (class.wrap_char.is_none() && class.wrap_length == 0) { raw } else { class.wrap(&raw) };
                        // Placed as at its own size, then that much farther out: the gap between
                        // the label and its feature grows with it too.
                        let size = csize * label_factor;
                        let font = class_fonts.get(ci).copied().flatten();
                        let face = text::Face { bold: class.bold, italic: class.italic, serif: false, family: font };
                        let spacing = class.spacing * label_factor;
                        let (bw, bh) = (text::spaced_block_width(&text, csize, face, class.spacing), text::block_height(&text, csize));
                        // Where a placement puts it, moved as its class says (points, as the
                        // text's size), unless along a line.
                        let place = |p: LabelPlacement| {
                            let (dx, dy) = p.offset(bw, bh, marker, is_point);
                            let (dx, dy) = ((dx + class.offset[0] * 1.33) * label_factor, (dy + class.offset[1] * 1.33) * label_factor);
                            (x as f32 + dx, y as f32 + dy)
                        };
                        // Where it goes, each with its repeat's index: by its feature (0), or along
                        // a line at its middle and, repeated, every so far either side.
                        let mut places: Vec<(usize, f32, f32, Option<f32>, Vec<LabelGlyph>)> = Vec::new();
                        let mut curves = None;
                        let mut line_alternatives = Vec::new();
                        let mut line_places = None;
                        if let Some((mx, my)) = moved {
                            places.push((0, mx as f32, my as f32, None, Vec::new()));
                        } else if let (LabelPlacement::Parallel | LabelPlacement::Curved, Geometry::Line(parts)) = (class.placement, g) {
                            let key = (lid, label_id(row), ci, 0);
                            let repeat = class.repeat.map(|r| r * 1.33 * label_factor);
                            let try_others = class.other_positions && repeat.is_none();
                            let saved_anchor = lock.as_ref().and(map.label_lock_line_anchors.get(&key)).copied().filter(|_| try_others);
                            // A lock naming this label without its saved anchor (a failed back-projection) has
                            // no place to hold: sliding it to whatever stretch shows now would move it.
                            if try_others && saved_anchor.is_none() && lock.as_ref().is_some_and(|k| k.contains_key(&key)) {
                                continue;
                            }
                            let (line, locked_at) = if let Some(anchor) = saved_anchor {
                                let target = match &globe {
                                    Some(globe) => globe.forward(anchor),
                                    None => flat.forward(anchor).map(|p| view.to_screen(p)),
                                };
                                let Some((x, y)) = target else { continue };
                                let Some((line, at)) = line_near_anchor(ctx, parts, (x as f32, y as f32), size) else { continue };
                                (std::sync::Arc::new(line), Some(at))
                            } else {
                                let line = screen_line.get_or_insert_with(|| {
                                    longest_on_screen(ctx, parts).and_then(|l| line_labels::Line::new(&l)).map(std::sync::Arc::new)
                                });
                                let Some(line) = line.as_ref() else { continue };
                                (line.clone(), None)
                            };
                            let overrun = size;
                            let positions = |width| match locked_at {
                                Some(at) => {
                                    let won = lock.as_ref().and_then(|keys| keys.get(&key)).map_or(0, |won| won.place);
                                    vec![at; (usize::from(won) + 1).min(8)]
                                }
                                None => line_labels::positions(line.length(), width, overrun),
                            };
                            let anchors = |positions: &[f32]| {
                                // Back-project only while locking is requested, never on ordinary frames or replay.
                                if !display.locked || lock.is_some() || matches!(labels_to, Labels::Tilted(..)) {
                                    return Vec::new();
                                }
                                positions
                                    .iter()
                                    .map(|d| {
                                        let (x, y) = line.anchor(*d)?;
                                        match &globe {
                                            Some(globe) => globe.inverse(f64::from(x), f64::from(y)),
                                            None => flat.inverse(view.camera.to_map(f64::from(x), f64::from(y), view.width, view.height)),
                                        }
                                    })
                                    .collect()
                            };
                            if curving {
                                let chars: Vec<String> = text.graphemes(true).map(String::from).collect();
                                let advances: Vec<f32> = chars.iter().map(|c| text::measure_in(c, size, face)).collect();
                                if try_others {
                                    let width = line_labels::curved_len(&advances, spacing * 1.33);
                                    let positions = positions(width);
                                    // A saved slot the curve cannot take has no truthful preview either (as for a
                                    // straight line): never draw one at the anchor or the middle.
                                    if lock.as_ref().and_then(|k| k.get(&key)).is_some_and(|won| usize::from(won.place) >= positions.len())
                                    {
                                        continue;
                                    }
                                    line_places = Some(LinePlaces { anchors: anchors(&positions), ..Default::default() });
                                    let Some((cx, cy)) = line.anchor(locked_at.unwrap_or(line.length() / 2.0)) else { continue };
                                    let angle = line.parallel(width, overrun, locked_at).map(|(_, _, a)| a);
                                    // The preview is also View Unplaced's middle when every curve fails.
                                    places.push((0, cx, cy, angle, Vec::new()));
                                    let locked = lock.as_ref().and_then(|k| k.get(&key)).map(|won| usize::from(won.place));
                                    curves = Some(label_places::Curves::new(
                                        std::sync::Arc::new(label_places::CurveSpec {
                                            line: line.clone(),
                                            chars,
                                            advances,
                                            gap: spacing * 1.33,
                                            overrun,
                                        }),
                                        positions,
                                        locked,
                                    ));
                                } else {
                                    for (i, at) in line_labels::repeats(line.length(), repeat) {
                                        let Some(((cx, cy), turns)) = line.curved(&advances, spacing * 1.33, overrun, Some(at)) else {
                                            continue;
                                        };
                                        let glyphs = chars
                                            .iter()
                                            .zip(turns)
                                            .map(|(text, (dx, dy, angle))| LabelGlyph { text: text.clone(), dx, dy, angle })
                                            .collect();
                                        places.push((i, cx, cy, None, glyphs));
                                    }
                                }
                            } else {
                                // A text too long to curve character by character also goes straight.
                                let width = text::spaced_block_width(&text, size, face, spacing);
                                let along = if try_others {
                                    let positions = positions(width);
                                    line_places = Some(LinePlaces { anchors: anchors(&positions), ..Default::default() });
                                    positions.into_iter().enumerate().collect()
                                } else {
                                    line_labels::repeats(line.length(), repeat)
                                };
                                // Alternatives share one width/overrun and stay inside upright's
                                // range: if the middle cannot fit, none can. Keep it as place 0
                                // so a lock's alternative indices do not change.
                                for (i, at) in along {
                                    if let Some((x, y, angle)) = line.parallel(width, overrun, Some(at)) {
                                        if try_others && i > 0 {
                                            line_alternatives.push(LabelPlace { x, y, angle: Some(angle), glyphs: Vec::new() });
                                        } else {
                                            places.push((i, x, y, Some(angle), Vec::new()));
                                        }
                                    } else if try_others && i > 0 {
                                        // Keep position/anchor indices aligned even when this slot cannot fit.
                                        line_alternatives.push(LabelPlace { x: f32::NAN, y: f32::NAN, angle: None, glyphs: Vec::new() });
                                    }
                                }
                            }
                        } else {
                            let (lx, ly) = place(class.placement);
                            // Turned by its angle expression (degrees clockwise), about its centre,
                            // kept upright as labels turned with a turned map are.
                            let turned = exprs
                                .and_then(|e| e.angle(ds_ptr, row))
                                .filter(|a| *a != 0.0)
                                .map(|a| rustgis_tiles::symbols::upright(a.to_radians()))
                                .filter(|a| *a != 0.0);
                            places.push((0, lx, ly, turned, Vec::new()));
                        }
                        // Layers above first, then the classes listed first (#732), then bigger
                        // polygons: a class (ci ≤ MAX_LABEL_CLASSES, 1e9 each) never outweighs a
                        // layer (1e12), nor an area (≤ 1e8) a class. A line's repeats come after
                        // its label at the middle, nearest first (lines have no area). Labels moved
                        // by hand come before all of them (1e15), so the others keep off them.
                        let pri = (n_layers - li) as f64 * 1e12 - ci as f64 * 1e9 + area + if moved.is_some() { 1e15 } else { 0.0 };
                        let last = places.len().saturating_sub(1);
                        for (n, (i, mut lx, mut ly, mut angle, mut glyphs)) in places.into_iter().enumerate() {
                            let key = (lid, label_id(row), ci, i);
                            // Locked, only the repeats placed then.
                            if lock.as_ref().is_some_and(|k| !k.contains_key(&key)) {
                                continue;
                            }
                            // A point's label may go round it when its own place is taken (#1011);
                            // locked, it goes where it went then, and nowhere else.
                            let others = || {
                                if is_point {
                                    class
                                        .placement
                                        .alternatives()
                                        .filter(|_| class.other_positions && moved.is_none())
                                        .map(place)
                                        .map(LabelPlace::from)
                                        .collect::<Vec<_>>()
                                } else {
                                    line_alternatives.clone()
                                }
                            };
                            let alternatives = match lock.as_ref().and_then(|k| k.get(&key)) {
                                Some(&won) => {
                                    if let Some(at) = usize::from(won.place).checked_sub(1).and_then(|i| others().into_iter().nth(i)) {
                                        (lx, ly, angle, glyphs) = (at.x, at.y, at.angle, at.glyphs);
                                    } else if won.place > 0 && !is_point && curves.is_none() {
                                        // Do not slide a locked line to its middle when its saved place
                                        // cannot be represented. Without a valid saved place there is also
                                        // no truthful View Unplaced preview; never invent one at the middle.
                                        // Point alternatives have a fixed shape.
                                        continue;
                                    }
                                    Vec::new()
                                }
                                None => others(),
                            };
                            // (The text copied for each repeat but the last, which takes it.)
                            let text = if n == last { std::mem::take(&mut text) } else { text.clone() };
                            label_candidates.push(Candidate {
                                label: PlacedLabel {
                                    text,
                                    x: lx,
                                    y: ly,
                                    size,
                                    color: ccolor,
                                    halo: class.halo.then_some(class.halo_color_for(ccolor)),
                                    halo_width: class.halo_width.map(|w| w * label_factor),
                                    bold: class.bold,
                                    icon: None,
                                    align: 0.0,
                                    framed: false,
                                    italic: class.italic,
                                    spacing,
                                    font,
                                    line_align: class.line_align,
                                    angle,
                                    collide: rustgis_tiles::Collide {
                                        // Shown all: over the other labels where they crowd.
                                        text_allow_overlap: class.show_all,
                                        // Callouts: a label set by its feature (not along a line,
                                        // rotated or not) may move off a crowded spot, a line drawn
                                        // back to it; one moved by hand, a line's too, keeps its line.
                                        callout_reach: if moved.is_some() || (!along_line(class.placement, g) && glyphs.is_empty()) {
                                            class.callout.unwrap_or(0.0)
                                        } else {
                                            0.0
                                        },
                                        callout_anchor: anchor.map(|(ax, ay)| [ax as f32 - lx, ay as f32 - ly]),
                                        callout_offset: lock.as_ref().and_then(|k| k.get(&key)).and_then(|won| won.callout),
                                        pinned: moved.is_some(),
                                        source: fid.map(|id| (lid, id, ci as u32)),
                                        ..Default::default()
                                    },
                                    // (Not behind characters curved one by one.)
                                    background: class
                                        .background
                                        .as_ref()
                                        .filter(|b| b.draws() && glyphs.is_empty() && curves.is_none())
                                        .map(|b| b.scaled(label_factor)),
                                    glyphs,
                                },
                                priority: pri - i as f64,
                                key,
                                alternatives,
                                curves: curves.take(),
                                line: line_places.take(),
                                row,
                            });
                        }
                    }
                }
            }
            if complete_source && matches!(labels_to, Labels::Own | Labels::Turned(_)) {
                compact_label_candidates(&mut label_candidates);
            }
            Ok(())
        };
        if let Some(source) = source {
            let mut chunk = layer.clone();
            chunk.source = None;
            chunk.clear_cache();
            chunk.dataset.features = Vec::new().into();
            // All coordinates are read here: filtering only to the viewport can change labels,
            // shadows and screen-space effects whose geometry lies beyond its boundary.
            let result = visit_complete(source.as_ref(), &mut |features| {
                chunk.clear_cache();
                chunk.selection =
                    features.iter().enumerate().filter_map(|(r, f)| selected_order.contains_key(&f.id).then_some(r)).collect();
                chunk.dataset.features = features.into();
                chunk.touch_geometry();
                draw_chunk(&mut chunk)?;
                chunk.clear_cache();
                chunk.dataset.features = Vec::new().into();
                Ok(())
            });
            if let Err(e) = result {
                if let Some(s) = &mut layer.source {
                    s.error = Some(e.to_string());
                }
                return None;
            }
            if let Some(source) = &mut layer.source {
                source.error = None;
            }
        } else if let Err(e) = draw_chunk(layer) {
            log::warn!("cannot render layer '{}': {e}", layer.name);
            return None;
        }
        if let (Some(spool), Some(x)) = (&mut extrusions, &layer.extrusion)
            && let Err(e) = spool.draw(if direct { &mut pm } else { &mut target }, ctx, x)
        {
            if let Some(source) = &mut layer.source {
                source.error = Some(e.to_string());
            }
            return None;
        }
        if complete_source {
            if let (Some(a), Some(base)) = (layer.aggregation, aggregate_symbol) {
                bins.draw(if direct { &mut pm } else { &mut target }, ctx, a, &base);
            }
            if !direct {
                if target.is_recording() {
                    pm.place(target, Transform::identity(), opacity, layer_blend, None);
                } else {
                    composite(&mut pm, target.pixmap());
                }
            }
        }
        highlights.sort_by_key(|(rank, ..)| *rank);
        for (_, geometry, symbol, strokes, turn) in highlights {
            ctx.highlight(&mut pm, &geometry, &symbol, &strokes, turn);
            cpu_dirty = true;
        }
    }
    if let Some(id) = below {
        draw_tiles!(Some(id));
    }
    if let Some(g) = &globe {
        globe_shading(pm.pixels(), g, water, space(view, map), view.pixels_per_point as f32);
    }
    // Tile labels, after every layer's own.
    if lock.is_none() {
        for (i, (l, pri)) in tile_out.labels.into_iter().enumerate() {
            label_candidates.push((l, pri - 1e13, (u32::MAX, i as u64, 0, 0)).into());
        }
    }
    if let Labels::Tilted(to, out) = &mut labels_to {
        // At their places in the tilted view; those behind the camera, under the sky or too far
        // to read are left out.
        let (from, to) = (*view, **to);
        let (tw, th) = (to.width, to.height);
        let sky_y = to.camera.sky_y(th).unwrap_or(0.0) as f32;
        let at = move |x: f32, y: f32| -> Option<(f32, f32)> {
            let m = from.camera.to_map(f64::from(x), f64::from(y), from.width, from.height);
            let near = to.camera.depth_scale(m, th).is_some_and(|k| k >= LABEL_MIN_DEPTH_SCALE);
            let (sx, sy) = to.camera.try_to_screen(m, tw, th).filter(|_| near)?;
            (sy as f32 > sky_y).then_some((sx as f32, sy as f32))
        };
        let fixed = fixed_labels.into_iter().map(|l| Candidate::from((l, f64::MAX, (u32::MAX, u64::MAX, 0, 0))));
        for c in fixed.chain(label_candidates) {
            out.extend(tilt_candidate(c, &at));
        }
        label_candidates = Vec::new();
        fixed_labels = Vec::new();
    }
    let turned = match &labels_to {
        Labels::Own | Labels::Shared => None,
        Labels::Turned(to) | Labels::Tilted(to, _) => Some(*to),
    };
    let (lw, lh) = match turned {
        Some(to) => {
            // Into the turned view's screen, where point labels stay upright.
            let (from, target) = (*view, *to);
            let at = move |x: f32, y: f32| {
                let (sx, sy) = target.to_screen(from.camera.to_map(f64::from(x), f64::from(y), from.width, from.height));
                Some((sx as f32, sy as f32))
            };
            for c in &mut label_candidates {
                turn_candidate(c, at);
            }
            for l in &mut fixed_labels {
                turn_with(l, &at);
                if let Some((x, y)) = at(l.x, l.y) {
                    (l.x, l.y) = (x, y);
                }
                if let Some(i) = &mut l.icon
                    && let Some((x, y)) = at(i.x, i.y)
                {
                    (i.x, i.y) = (x, y);
                }
            }
            (to.width, to.height)
        }
        None => (view.width, view.height),
    };
    let placed = place_labels_with_previews(label_candidates, fixed_labels, lw as f32, lh as f32, lock.is_some(), display.show_unplaced);
    // (The lock is never taken from a tilted view: its labels' places out of sight are dropped,
    // so the numbers of the places they take aren't those of their class's.)
    if display.locked && map.label_lock.is_none() && !matches!(labels_to, Labels::Tilted(..)) {
        map.label_lock_line_anchors = placed.line_anchors.into_iter().collect();
        map.label_lock = Some(placed.keys);
    }
    let unplaced = if display.show_unplaced { placed.unplaced } else { Vec::new() };
    // The placed labels' icons go into the map itself; their text is drawn over it later. A
    // turned view draws them once it is turned.
    for l in placed.labels.iter().filter(|_| turned.is_none()) {
        if let Some(icon) = &l.icon {
            draw_icon(&mut pm, icon, view.pixels_per_point as f32);
            cpu_dirty = true;
        }
    }
    // Effects, and (on screen only) the colour vision simulation.
    let vision = if full_res_rasters { rustgis_map::view::ColorVision::Normal } else { map.view.color_vision };
    if !map.view.effects.is_none() || vision != rustgis_map::view::ColorVision::Normal {
        let ppp = view.pixels_per_point;
        let lonlat = |x: f64, y: f64| -> Option<Coord> {
            let (sx, sy) = (x / ppp, y / ppp);
            match &globe {
                Some(g) => g.inverse(sx, sy),
                None => flat.inverse(view.camera.to_map(sx, sy, view.width, view.height)),
            }
        };
        post::apply(pm.pixels(), &map.view.effects, vision, &lonlat);
    }
    let labels = placed.labels;
    let attribution = tile_out.attribution;
    let tiles_pending = tile_out.pending;
    let tile_errors = tile_out.errors;
    let partial = tile_out.partial;
    let pending_in_place = tile_out.pending_in_place;
    match backend {
        Backend::Cpu => {
            let (pixmap, recorded) = if pm.is_recording() { (Pixmap::new(1, 1)?, Some(pm)) } else { (pm.into_pixmap(), None) };
            Some(RenderOutput::Cpu(Rendered {
                pixmap,
                labels,
                unplaced,
                attribution,
                tiles_pending,
                data_pending,
                tile_errors,
                partial,
                pending_in_place,
                recorded,
            }))
        }
        Backend::Gpu | Backend::GpuLive => {
            flush_acc(pm.pixels(), &mut cmds, &mut cpu_dirty)?;
            // Pathological CPU/GPU alternation (rasters, masks, blends between every vector
            // layer) costs more full-screen uploads than a plain CPU rasterization.
            let images = cmds.iter().filter(|c| matches!(c, gpu_scene::DrawCmd::Image { .. })).count();
            if images > 4 {
                return render_frame(map, view, full_res_rasters, Backend::Cpu, labels_to, record);
            }
            Some(RenderOutput::Gpu(gpu_scene::GpuScene {
                clear: mesh::premul(water),
                cmds,
                labels,
                unplaced,
                attribution,
                tiles_pending,
                data_pending,
                tile_errors,
                partial,
                pending_in_place,
                tilt: None,
                clips: Vec::new(),
            }))
        }
    }
}

/// Draws an icon, scaled to its size in logical points.
fn draw_icon(pm: &mut Canvas, icon: &PlacedIcon, ppp: f32) {
    let (iw, ih) = (icon.image.width().max(1) as f32, icon.image.height().max(1) as f32);
    let (sx, sy) = (icon.w * ppp / iw, icon.h * ppp / ih);
    let (cx, cy) = (icon.x * ppp, icon.y * ppp);
    if !(sx.is_finite() && sy.is_finite() && cx.is_finite() && cy.is_finite()) {
        return;
    }
    // Scaled about its centre, turned there, then put in place.
    let turn = icon.angle.filter(|a| a.is_finite()).map_or(0.0, f32::to_degrees);
    let t = Transform::from_translate(-iw / 2.0, -ih / 2.0).post_scale(sx, sy).post_rotate(turn).post_translate(cx, cy);
    let opacity = if icon.alpha.is_finite() { icon.alpha.clamp(0.0, 1.0) } else { 1.0 };
    let paint = PixmapPaint { opacity, quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() };
    pm.draw_pixmap(0, 0, Pixmap::as_ref(&icon.image), &paint, t, None);
}

/// Metres on the ground per logical point at the view's centre.
fn map_ground_res(view: &View, flat: &rustgis_map::projection::Flat) -> f64 {
    (view.camera.res * flat.ground_per_unit(view.camera.center)).max(1e-9)
}

pub(crate) fn blend_mode(b: rustgis_map::effects::Blend) -> tiny_skia::BlendMode {
    use rustgis_map::effects::Blend;
    match b {
        Blend::Normal => tiny_skia::BlendMode::SourceOver,
        Blend::Multiply => tiny_skia::BlendMode::Multiply,
        Blend::Screen => tiny_skia::BlendMode::Screen,
        Blend::Overlay => tiny_skia::BlendMode::Overlay,
        Blend::Darken => tiny_skia::BlendMode::Darken,
        Blend::Lighten => tiny_skia::BlendMode::Lighten,
        Blend::Difference => tiny_skia::BlendMode::Difference,
    }
}

/// The mask of every masked or swiped layer: inside its mask layer's polygons and left of the
/// swipe divider.
fn layer_masks(
    map: &mut Map,
    ctx: &Ctx,
    w: u32,
    h: u32,
    flat: &rustgis_map::projection::Flat,
    key: &str,
    scale: f64,
) -> Option<Vec<(u32, tiny_skia::Mask)>> {
    let wanted: Vec<(u32, Option<String>, Option<f64>)> = map
        .layers
        .iter()
        .filter(|l| map.draws_at(l, scale))
        .filter_map(|l| {
            let swipe = map.swipe.filter(|(id, _)| *id == l.id).map(|(_, x)| x.clamp(0.0, 1.0));
            (l.mask.is_some() || swipe.is_some()).then(|| (l.id, l.mask.clone(), swipe))
        })
        .collect();
    let mut out = Vec::new();
    let mut paths = std::collections::HashMap::new();
    for (id, by, swipe) in wanted {
        let Some(mut m) = tiny_skia::Mask::new(w, h) else { continue };
        match by
            .as_deref()
            .and_then(|n| map.layers.iter().position(|l| l.name.eq_ignore_ascii_case(n) && l.geometry_type() == GeometryType::Polygon))
        {
            Some(mi) => {
                if let std::collections::hash_map::Entry::Vacant(entry) = paths.entry(mi) {
                    let mut pb = PathBuilder::new();
                    // The mask's shapes go straight into the path, without copying them.
                    let mut add = |g: &Geometry| {
                        if let Geometry::Polygon(polys) = g {
                            for r in polys.iter().flatten() {
                                ctx.ring_path(&mut pb, r, true);
                            }
                        }
                    };
                    if let Some(source) = map.layers.get(mi).and_then(|l| l.source.as_ref()).and_then(|s| s.chunk_source()) {
                        let result = visit_complete(source.as_ref(), &mut |features| {
                            for feature in features {
                                if let Some(g) = feature.geometry {
                                    let g = if ctx.globe.is_some() { g } else { flat.project(&g) };
                                    add(&g);
                                }
                            }
                            Ok(())
                        });
                        if let Err(e) = result {
                            if let Some(s) = map.layers.get_mut(mi).and_then(|l| l.source.as_mut()) {
                                s.error = Some(e.to_string());
                            }
                            return None;
                        }
                    } else if let Some(ml) = map.layers.get_mut(mi) {
                        if ctx.globe.is_some() {
                            ml.dataset.features.iter().filter_map(|f| f.geometry.as_ref()).for_each(&mut add);
                        } else {
                            ml.projected_in(flat, key).iter().flatten().for_each(&mut add);
                        }
                    }
                    entry.insert(pb.finish());
                }
                if let Some(p) = paths.get(&mi).and_then(Option::as_ref) {
                    m.fill_path(p, FillRule::EvenOdd, true, Transform::identity());
                }
            }
            // No (or no usable) mask layer: everything shows.
            None => {
                if let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32) {
                    m.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, false, Transform::identity());
                }
            }
        }
        if let Some(x) = swipe
            && let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, (w as f64 * x) as f32, h as f32)
        {
            m.intersect_path(&PathBuilder::from_rect(r), FillRule::Winding, false, Transform::identity());
        }
        out.push((id, m));
    }
    Some(out)
}

/// The colour of the space around the globe: the dark sky, or the light space of exports.
pub fn globe_space(dark: bool) -> Rgba {
    if dark { Rgba::rgb(0x08, 0x0B, 0x10) } else { Rgba::rgb(0xEC, 0xEF, 0xF3) }
}

/// How the space around the globe looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Space {
    dark: bool,
    stars: bool,
    /// The glow of an atmosphere around the disc: the Earth's (#223).
    atmosphere: bool,
}

/// The space around the globe for `view`: the interactive view's starry sky, or for exports a
/// light space (dark on the Midnight basemap) without stars.
fn space(view: &View, map: &Map) -> Space {
    let dark = view.sky || map.basemap == rustgis_map::Basemap::Midnight;
    Space { dark, stars: view.sky && map.view.effects.stars, atmosphere: map.body().is_earth() }
}

/// Side of the starfield tile in logical points: the stars repeat this far apart.
pub const STAR_TILE: f32 = 512.0;

/// The stars of the space around the globe: a square `STAR_TILE` points a side at `ppp` pixels
/// per point, transparent between the stars, to be laid edge to edge from the view's top left
/// corner. The sky is the same every time, and the stars stay put while the globe turns.
pub fn starfield(ppp: f32) -> Option<Pixmap> {
    let ppp = if ppp.is_finite() { ppp.clamp(0.25, 4.0) } else { 1.0 };
    let side = (STAR_TILE * ppp).round().max(1.0);
    let mut pm = Pixmap::new(side as u32, side as u32)?;
    let ppp = side / STAR_TILE;
    // A small fixed-seed generator (SplitMix64), so every view gets the same sky.
    let mut seed: u64 = 0x5EED_57A2_F1E1_D000;
    let mut next = || {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    };
    // Bluish, white and warm stars; most of them faint.
    let tints = [(0xCF, 0xDD, 0xFF), (0xFF, 0xFF, 0xFF), (0xFF, 0xEA, 0xCC)];
    for _ in 0..180 {
        let (x, y) = (2.0 + next() * (STAR_TILE - 4.0), 2.0 + next() * (STAR_TILE - 4.0));
        let bright = next().powi(3);
        let tint = tints.get((next() * 3.0) as usize).copied().unwrap_or((0xFF, 0xFF, 0xFF));
        let (cx, cy) = (x * ppp, y * ppp);
        let r = (0.3 + 0.8 * bright) * ppp;
        if bright > 0.6
            && let Some(path) = PathBuilder::from_circle(cx, cy, r * 2.5)
        {
            // A faint halo around the brightest.
            let halo = paint(Rgba::rgb(tint.0, tint.1, tint.2), 0.12);
            pm.fill_path(&path, &halo, FillRule::Winding, Transform::identity(), None);
        }
        if let Some(path) = PathBuilder::from_circle(cx, cy, r.max(0.35)) {
            let p = paint(Rgba::rgb(tint.0, tint.1, tint.2), 0.3 + 0.7 * bright);
            pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        }
    }
    Some(pm)
}

thread_local! {
    /// The starfield tile last drawn, and its pixels per point.
    static STARS: std::cell::RefCell<Option<(u32, Pixmap)>> = const { std::cell::RefCell::new(None) };
}

/// Fills `pm` with the starfield, laid edge to edge from its top left corner.
fn draw_stars(pm: &mut Pixmap, ppp: f32) {
    STARS.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.as_ref().is_none_or(|c| c.0 != ppp.to_bits()) {
            *cache = starfield(ppp).map(|t| (ppp.to_bits(), t));
        }
        let Some((_, tile)) = cache.as_ref() else { return };
        let shader =
            tiny_skia::Pattern::new(tile.as_ref(), SpreadMode::Repeat, tiny_skia::FilterQuality::Nearest, 1.0, Transform::identity());
        let p = Paint { shader, ..Paint::default() };
        if let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32) {
            pm.fill_rect(r, &p, Transform::identity(), None);
        }
    });
}

/// The globe's backdrop and lighting depend only on the view size, zoom, ocean and stars, not on
/// the rotation, so they are drawn once and reused while the globe turns.
struct GlobeLayers {
    key: (u32, u32, u32, u32, u32, u32, u32, [u8; 4], Space),
    backdrop: Pixmap,
    shading: Pixmap,
}

thread_local! {
    static GLOBE_LAYERS: std::cell::RefCell<Option<GlobeLayers>> = const { std::cell::RefCell::new(None) };
}

fn with_globe_layers(pm: &mut Pixmap, g: &Globe, water: Rgba, space: Space, ppp: f32, f: impl FnOnce(&mut Pixmap, &GlobeLayers)) {
    let (cx, cy) = g.center();
    let key = (
        pm.width(),
        pm.height(),
        ppp.to_bits(),
        (cx as f32).to_bits(),
        (cy as f32).to_bits(),
        (g.disc_radius() as f32).to_bits(),
        (g.disc_height() as f32).to_bits(),
        water.0,
        space,
    );
    GLOBE_LAYERS.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.as_ref().is_none_or(|c| c.key != key) {
            let (Some(mut backdrop), Some(mut shading)) = (Pixmap::new(pm.width(), pm.height()), Pixmap::new(pm.width(), pm.height()))
            else {
                return;
            };
            draw_globe_backdrop(&mut backdrop, g, water, space, ppp);
            draw_globe_shading(&mut shading, g, ppp);
            *cache = Some(GlobeLayers { key, backdrop, shading });
        }
        if let Some(c) = cache.as_ref() {
            f(pm, c);
        }
    });
}

fn globe_backdrop(pm: &mut Pixmap, g: &Globe, water: Rgba, space: Space, ppp: f32) {
    with_globe_layers(pm, g, water, space, ppp, |pm, c| pm.data_mut().copy_from_slice(c.backdrop.data()));
}

fn globe_shading(pm: &mut Pixmap, g: &Globe, water: Rgba, space: Space, ppp: f32) {
    with_globe_layers(pm, g, water, space, ppp, |pm, c| {
        pm.draw_pixmap(0, 0, c.shading.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    });
}

/// The globe's surroundings and ocean: space (dark or light, starry when it says so), an
/// atmosphere glow, and the ocean disc.
fn draw_globe_backdrop(pm: &mut Pixmap, g: &Globe, water: Rgba, space: Space, ppp: f32) {
    let bg = globe_space(space.dark);
    pm.fill(tiny_skia::Color::from_rgba8(bg.r(), bg.g(), bg.b(), 255));
    if space.stars {
        draw_stars(pm, ppp);
    }
    let (cx, cy) = g.center();
    let disc = (cx as f32 * ppp, cy as f32 * ppp, g.disc_radius() as f32 * ppp);
    draw_globe_disc(pm, disc, disc_stretch(g), water, space.dark, space.atmosphere);
}

/// How much taller than wide a globe's disc is (1 unless it is tilted).
fn disc_stretch(g: &Globe) -> f32 {
    let k = (g.disc_height() / g.disc_radius().max(1e-9)) as f32;
    if k.is_finite() && k > 0.0 { k } else { 1.0 }
}

/// Stretches drawing up and down by `k` about the row `cy`: a disc into a tilted globe's ellipse.
fn stretch_about(cy: f32, k: f32) -> Transform {
    Transform::from_row(1.0, 0.0, 0.0, k, 0.0, cy * (1.0 - k))
}

/// The atmosphere glow (with `atmosphere`) and the ocean disc, for a disc of radius `r` pixels
/// at (`cx`, `cy`), stretched `k` times up and down (a tilted globe's ellipse).
fn draw_globe_disc(pm: &mut Pixmap, (cx, cy, r): (f32, f32, f32), k: f32, water: Rgba, dark: bool, atmosphere: bool) {
    if !(r.is_finite() && r > 0.5 && cx.is_finite() && cy.is_finite()) {
        return;
    }
    let t = stretch_about(cy, k);
    // Atmosphere: a glow fading out just beyond the limb.
    let glow_r = r * 1.07;
    // A softer, paler glow on light space.
    let glow = if dark { (0x5A, 0x9B, 0xF0, 150) } else { (0x7F, 0xB4, 0xE8, 120) };
    if atmosphere
        && let Some(shader) = RadialGradient::new(
            Point::from_xy(cx, cy),
            0.0,
            Point::from_xy(cx, cy),
            glow_r,
            vec![
                GradientStop::new(0.0, tiny_skia::Color::from_rgba8(glow.0, glow.1, glow.2, glow.3)),
                GradientStop::new(r / glow_r, tiny_skia::Color::from_rgba8(glow.0, glow.1, glow.2, glow.3)),
                GradientStop::new(1.0, tiny_skia::Color::from_rgba8(glow.0, glow.1, glow.2, 0)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        )
        && let Some(path) = PathBuilder::from_circle(cx, cy, glow_r)
    {
        let p = Paint { shader, anti_alias: true, ..Paint::default() };
        pm.fill_path(&path, &p, FillRule::Winding, t, None);
    }
    if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
        pm.fill_path(&path, &paint(water, 1.0), FillRule::Winding, t, None);
    }
}

/// Light from the upper left: a soft highlight and a darker limb, so the globe reads as a sphere.
fn draw_globe_shading(pm: &mut Pixmap, g: &Globe, ppp: f32) {
    let (cx, cy) = g.center();
    draw_globe_light(pm, cx as f32 * ppp, cy as f32 * ppp, g.disc_radius() as f32 * ppp, disc_stretch(g));
}

/// The lighting of a disc of radius `r` pixels at (`cx`, `cy`), stretched `k` times up and down.
fn draw_globe_light(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, k: f32) {
    if !(r.is_finite() && r > 0.5 && cx.is_finite() && cy.is_finite()) {
        return;
    }
    let t = stretch_about(cy, k);
    let light = Point::from_xy(cx - r * 0.35, cy - r * 0.4);
    if let Some(shader) = RadialGradient::new(
        light,
        0.0,
        Point::from_xy(cx, cy),
        r * 1.02,
        vec![
            GradientStop::new(0.0, tiny_skia::Color::from_rgba8(255, 255, 255, 46)),
            GradientStop::new(0.45, tiny_skia::Color::from_rgba8(255, 255, 255, 0)),
            GradientStop::new(0.8, tiny_skia::Color::from_rgba8(0, 0, 0, 18)),
            GradientStop::new(1.0, tiny_skia::Color::from_rgba8(0, 0, 0, 70)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    ) && let Some(path) = PathBuilder::from_circle(cx, cy, r)
    {
        let p = Paint { shader, anti_alias: true, ..Paint::default() };
        pm.fill_path(&path, &p, FillRule::Winding, t, None);
    }
}

/// How far the atmosphere glow reaches, in disc radii.
pub const GLOBE_GLOW: f32 = 1.07;

/// The globe's surroundings as two square sprites for a disc of radius `r` pixels, centred in
/// them (side `2 × GLOBE_GLOW × r`): the atmosphere (with `atmosphere`) and the `water` disc drawn
/// under the map, and the lighting drawn over it. Neither changes as the globe turns, so a view
/// can scale them while it moves instead of drawing them again.
pub fn globe_sprites(water: Rgba, atmosphere: bool, r: f32) -> Option<(Pixmap, Pixmap)> {
    if !(r.is_finite() && r >= 1.0) {
        return None;
    }
    // A whole number of pixels a side, with the disc exactly in proportion to it.
    let side = (2.0 * GLOBE_GLOW * r.min(2048.0)).round().max(2.0);
    let (r, c) = (side / (2.0 * GLOBE_GLOW), side / 2.0);
    let (mut under, mut over) = (Pixmap::new(side as u32, side as u32)?, Pixmap::new(side as u32, side as u32)?);
    draw_globe_disc(&mut under, (c, c, r), 1.0, water, true, atmosphere);
    draw_globe_light(&mut over, c, c, r, 1.0);
    Some((under, over))
}

/// A part of the map drawn flat, in Web Mercator, to lay over the globe while it turns.
pub struct DrapePatch {
    pub pixmap: Pixmap,
    /// The Web Mercator box the picture shows.
    pub merc: Extent,
}

/// Draws the longitude/latitude `boxes` (from `Globe::drape_extents`) of a globe map flat, at
/// the view's scale, each picture at most `max_side` pixels a side (fewer pixels per point
/// beyond that). The globe view then drapes them over the sphere
/// ([`rustgis_map::Globe::drape_mesh`]) while it turns, with no drawing per frame. Labels are
/// left out (the view hides them while it moves).
pub fn render_globe_drape(map: &mut Map, view: &View, boxes: &[Extent], max_side: u32) -> Vec<DrapePatch> {
    // A coarse drape may have far fewer pixels than points: it shows the map blurrier, but at
    // the view's scale.
    let ppp = if view.pixels_per_point.is_finite() { view.pixels_per_point.clamp(1.0 / 64.0, 4.0) } else { 1.0 };
    let res = view.camera.res;
    if !(res.is_finite() && res > 0.0) {
        return Vec::new();
    }
    let max_side = f64::from(max_side.clamp(64, 8192));
    let projection = std::mem::replace(&mut map.projection, rustgis_map::Projection::WebMercator);
    let (display, lock) = (map.label_display, map.label_lock.take());
    let anchors = std::mem::take(&mut map.label_lock_line_anchors);
    map.label_display.paused = true;
    let mut out = Vec::new();
    for b in boxes {
        let merc = extent_to_merc(b);
        if merc.is_empty() || !(merc.width() > 0.0 && merc.height() > 0.0) {
            continue;
        }
        // Whole points at the view's scale. Fitting the size limit lowers the pixels per point,
        // never the scale: symbols and strokes are sized in points, so a coarser scale would
        // draw them larger on the ground than the settled frame does.
        let (w, h) = ((merc.width() / res).floor().max(1.0), (merc.height() / res).floor().max(1.0));
        let ppp = ppp.min(max_side / w.max(h));
        if w * ppp < 2.0 || h * ppp < 2.0 {
            continue;
        }
        let camera = Camera { center: merc.center(), res, bearing: 0.0, pitch: 0.0 };
        let v = View { width: w, height: h, pixels_per_point: ppp, camera, sky: false };
        // The picture is given the box asked for, though rounding to whole points made it show
        // up to a point less: stretched by under a point, its edges stay exactly where asked, so
        // one at the Web Mercator limit is still stretched on to the pole (drape_mesh).
        if let Some(r) = render(map, &v) {
            out.push(DrapePatch { pixmap: r.pixmap, merc });
        }
    }
    map.projection = projection;
    map.label_display = display;
    map.label_lock = lock;
    map.label_lock_line_anchors = anchors;
    out
}

/// A band of a tilted view drawn flat, to lay over the tilted ground while the view moves.
pub struct TiltPatch {
    pub pixmap: Pixmap,
    /// The flat, north-up camera and size (logical points) it was drawn for.
    pub band: rustgis_map::camera::Band,
}

/// Draws the bands of a tilted view ([`Camera::tilt_bands`]) flat, each grown by `margin` (a
/// fraction of its size) on every side so the view can move a little before they no longer
/// cover it, and at most `max_side` pixels a side (coarser beyond that). The map view lays them
/// over the tilted ground as meshes while it moves, with no drawing per frame. Labels are left
/// out (the view keeps the last frame's while it moves). A flat view turned to a bearing is one
/// band, square, over the circle round the view and without the margin: it is laid over the view
/// the same way while it turns.
pub fn render_tilt_drape(map: &mut Map, view: &View, margin: f64, max_side: u32) -> Vec<TiltPatch> {
    let ppp = if view.pixels_per_point.is_finite() { view.pixels_per_point.clamp(0.25, 4.0) } else { 1.0 };
    let max_side = f64::from(max_side.clamp(64, 8192));
    let cam = view.camera;
    let turned = turned_band(view);
    // A flat turned view moves on its band only as it turns: no room for panning, whose picture
    // a service drawing one picture of the view would be asked for again, larger.
    let margin = if margin.is_finite() && turned.is_none() { margin.clamp(0.0, 2.0) } else { 0.0 };
    let bands = match turned {
        Some(band) => vec![band],
        None => cam.tilt_bands(view.width, view.height),
    };
    sync_tilted(map, view, &bands, false);
    let (display, lock) = (map.label_display, map.label_lock.take());
    let anchors = std::mem::take(&mut map.label_lock_line_anchors);
    map.label_display.paused = true;
    let mut out = Vec::new();
    for b in bands {
        let (mut w, mut h) = (b.width * (1.0 + 2.0 * margin), b.height * (1.0 + 2.0 * margin));
        let mut camera = b.camera;
        // Coarser to fit the size limit, its pixels whole: as coarse again as the rounding takes,
        // so it still reaches as far (a turned band has a point to spare).
        let k = (w.max(h) * ppp / max_side).max(1.0);
        let (fw, fh) = ((w / k).floor().max(1.0), (h / k).floor().max(1.0));
        camera.res *= (w / fw).max(h / fh);
        (w, h) = (fw, fh);
        let v = View { width: w, height: h, pixels_per_point: ppp, camera, sky: false };
        // The bands share the read above.
        if let Some(RenderOutput::Cpu(r)) = render_frame(map, &v, false, Backend::Cpu, Labels::Shared, false) {
            out.push(TiltPatch { pixmap: r.pixmap, band: rustgis_map::camera::Band { camera, width: w, height: h, depth: b.depth } });
        }
    }
    map.label_display = display;
    map.label_lock = lock;
    map.label_lock_line_anchors = anchors;
    out
}

/// A line's longest stretch in view, in points (`None` when none has two points in sight): the
/// one a label along the line follows. Each part is cut at the view's edges and where it goes
/// out of sight (behind the globe), so a long part mostly off screen counts by what is on it.
fn longest_on_screen(ctx: &Ctx<'_>, parts: &[Vec<Coord>]) -> Option<Vec<(f32, f32)>> {
    let (w, h) = (ctx.view.width as f32, ctx.view.height as f32);
    let length = |pts: &[(f32, f32)]| pts.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum::<f32>();
    // A part whose extent is out of view has no stretch in it: not projected point by point.
    let near = |part: &[Coord]| {
        ctx.globe.is_some() || {
            let mut e = Extent::EMPTY;
            part.iter().for_each(|c| e.expand(*c));
            ctx.cull.intersects(&e)
        }
    };
    parts
        .iter()
        .filter(|part| near(part))
        .flat_map(|part| line_labels::in_view(part.iter().map(|c| ctx.screen(*c).map(|(x, y)| (x as f32, y as f32))), w, h))
        .max_by(|a, b| length(a).total_cmp(&length(b)))
}

/// Full projected runs nearest a saved geographic anchor, independent of viewport clipping.
fn line_near_anchor(ctx: &Ctx<'_>, parts: &[Vec<Coord>], anchor: (f32, f32), overrun: f32) -> Option<(line_labels::Line, f32)> {
    let mut best = None;
    let mut distance = f32::INFINITY;
    for part in parts {
        let points: Vec<_> = part.iter().map(|p| ctx.screen(*p).map(|(x, y)| (x as f32, y as f32))).collect();
        for run in points.split(|p| p.is_none_or(|(x, y)| !x.is_finite() || !y.is_finite())) {
            let run: Vec<_> = run.iter().flatten().copied().collect();
            if let Some(line) = line_labels::Line::new(&run)
                && let Some((d, at)) = line.nearest(anchor, overrun)
                && d < distance
            {
                distance = d;
                best = Some((line, at));
            }
        }
    }
    best
}

/// A label in a tilted view, at its place there (`at`: a place's in the view, if in sight). Its
/// own place, or else the first of its alternatives in sight, is where it goes; the rest in sight
/// are left to try. Left out with none in sight, or its icon out of sight.
fn tilt_candidate(mut c: Candidate, at: &(impl Fn(f32, f32) -> Option<(f32, f32)> + Clone + Send + Sync + 'static)) -> Option<Candidate> {
    let feature = c.label.collide.callout_anchor.and_then(|[ax, ay]| at(c.label.x + ax, c.label.y + ay));
    if let Some(curves) = &mut c.curves {
        let mut preview = curves.preview_in(&mut c.label, at)?;
        preview.swap(&mut c.label);
        curves.project(at.clone());
    } else {
        let mut first = None;
        let mut alternatives = Vec::new();
        for mut p in std::iter::once(LabelPlace::take(&mut c.label)).chain(std::mem::take(&mut c.alternatives)) {
            if p.transform(&mut c.label, at) {
                if first.is_none() {
                    first = Some(p);
                } else {
                    alternatives.push(p);
                }
            }
        }
        first?.swap(&mut c.label);
        c.alternatives = alternatives;
    }
    if let Some(i) = &mut c.label.icon {
        if let Some(a) = i.angle {
            i.angle = Some(turned_angle(i.x, i.y, a, at).unwrap_or(a));
        }
        (i.x, i.y) = at(i.x, i.y)?;
    }
    c.label.collide.callout_anchor = feature.map(|(x, y)| [x - c.label.x, y - c.label.y]);
    Some(c)
}

/// Project every possible text place, retaining deferred curved layouts until placement.
/// `at` must succeed for the main label and its icon (the turned-view caller is infallible).
/// Alternative glyph positions may fail; their slots stay invalid rather than being renumbered.
fn turn_candidate(c: &mut Candidate, at: impl Fn(f32, f32) -> Option<(f32, f32)> + Clone + Send + Sync + 'static) {
    let feature = c.label.collide.callout_anchor.and_then(|[ax, ay]| at(c.label.x + ax, c.label.y + ay));
    turn_with(&mut c.label, &at);
    if let Some((x, y)) = at(c.label.x, c.label.y) {
        (c.label.x, c.label.y) = (x, y);
    }
    if let Some(i) = &mut c.label.icon
        && let Some((x, y)) = at(i.x, i.y)
    {
        (i.x, i.y) = (x, y);
    }
    c.label.collide.callout_anchor = feature.map(|(x, y)| [x - c.label.x, y - c.label.y]);
    for p in &mut c.alternatives {
        if !p.transform(&mut c.label, &at) {
            // Preserve the slot number for locks, but never place source-view geometry.
            (p.x, p.y) = (f32::NAN, f32::NAN);
        }
    }
    if let Some(curves) = &mut c.curves {
        curves.project(at);
    }
}

/// The one band a flat view turned to a bearing is draped on: facing north, square over the
/// circle round the view, so turning it stays on the band (its tiles and pictures the same at
/// every bearing). `None` for a view north up or tilted.
pub fn turned_band(view: &View) -> Option<rustgis_map::camera::Band> {
    let cam = view.camera;
    (cam.pitch == 0.0 && cam.bearing != 0.0).then(|| {
        let side = view.width.hypot(view.height) + 2.0;
        rustgis_map::camera::Band { camera: Camera { bearing: 0.0, ..cam }, width: side, height: side, depth: (1.0, 1.0) }
    })
}

/// Turns `l` as `turn_with` does, for a label drawn in one view shown in another (a moving
/// map's labels laid over its drape): its place is left as it is.
pub fn turn_label(l: &mut PlacedLabel, at: &impl Fn(f32, f32) -> Option<(f32, f32)>) {
    turn_with(l, at);
}

/// Direction of a short step after projection into another view.
fn turned_angle(x: f32, y: f32, a: f32, at: &impl Fn(f32, f32) -> Option<(f32, f32)>) -> Option<f32> {
    let ((x0, y0), (x1, y1)) = (at(x, y)?, at(x + 4.0 * a.cos(), y + 4.0 * a.sin())?);
    let b = (y1 - y0).atan2(x1 - x0);
    b.is_finite().then_some(b)
}

/// Turns a label turned with the map (and its icon) as `at` moves it into another view (a
/// turned or tilted one): its angle there is that of a short step along it, the text kept
/// upright. Called before the label itself is moved.
fn turn_with(l: &mut PlacedLabel, at: &impl Fn(f32, f32) -> Option<(f32, f32)>) {
    let turned = |x, y, a| turned_angle(x, y, a, at);
    if let Some(a) = l.angle {
        let x = l.center_x();
        l.angle = Some(turned(x, l.y, a).map_or(a, rustgis_tiles::symbols::upright));
    }
    // A curved label's characters: each moved and turned, about where its centre goes.
    if !l.glyphs.is_empty()
        && let Some((cx, cy)) = at(l.x, l.y)
    {
        for g in &mut l.glyphs {
            let (gx, gy) = (l.x + g.dx, l.y + g.dy);
            if let Some((x, y)) = at(gx, gy) {
                (g.dx, g.dy) = (x - cx, y - cy);
            }
            g.angle = turned(gx, gy, g.angle).unwrap_or(g.angle);
        }
        // Turned upside down (a map turned past a quarter): read from the other end.
        let back = l.glyphs.iter().map(|g| g.angle.cos()).sum::<f32>() < 0.0;
        if back {
            read_back(l);
        }
    }
    if let Some(i) = &mut l.icon
        && let Some(a) = i.angle
    {
        i.angle = Some(turned(i.x, i.y, a).unwrap_or(a));
    }
}

/// A curved label's characters read from the line's other end, turned half round: each placed
/// as far from that end as it was from the start, by its own width rather than that of the
/// character whose place it takes, a step along the nearest one's angle from it.
fn read_back(l: &mut PlacedLabel) {
    let widths: Vec<f32> = l.glyphs.iter().map(|g| text::measure_in(&g.text, l.size, text::label_face(l))).collect();
    let total: f32 = widths.iter().sum();
    // How far along the text each character's centre was.
    let mut centres = Vec::with_capacity(widths.len());
    let mut d = 0.0;
    for w in &widths {
        centres.push(d + w / 2.0);
        d += w;
    }
    let before = l.glyphs.clone();
    for (g, c) in l.glyphs.iter_mut().zip(&centres) {
        let to = total - c;
        let nearest = centres.iter().zip(&before).min_by(|a, b| (a.0 - to).abs().total_cmp(&(b.0 - to).abs()));
        let Some((at, slot)) = nearest else { continue };
        let step = to - at;
        g.dx = slot.dx + step * slot.angle.cos();
        g.dy = slot.dy + step * slot.angle.sin();
        g.angle = slot.angle + std::f32::consts::PI;
    }
}

/// A halo that contrasts with the text colour.
fn halo_for(text: Rgba) -> Rgba {
    rustgis_map::symbology::auto_halo(text)
}

struct Placement {
    labels: Vec<PlacedLabel>,
    unplaced: Vec<PlacedLabel>,
    /// The key of each placed label, and the place it took.
    keys: Vec<(rustgis_map::LabelKey, rustgis_map::LockedPlace)>,
    line_anchors: Vec<(rustgis_map::LabelKey, Coord)>,
}

/// Labels a placement looks for a callout's spot for, at most (each looks at up to a few thousand
/// spots).
const MAX_CALLOUT_SEARCHES: usize = 500;

/// Spots a placement tries for its callouts at most, all its searches together.
const MAX_CALLOUT_SPOTS: usize = 200_000;

/// A label's text's extent (points, `[x0, y0, x1, y1]`), as placement keeps it clear of the
/// others: what the Move Label tool picks it by.
pub fn label_extent(l: &PlacedLabel) -> Option<[f32; 4]> {
    boxes_extent(&label_boxes(l).0)
}

/// The extent of boxes (`[x0, y0, x1, y1]` each); none for none.
fn boxes_extent(boxes: &[[f32; 4]]) -> Option<[f32; 4]> {
    let e = boxes
        .iter()
        .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |e, b| [e[0].min(b[0]), e[1].min(b[1]), e[2].max(b[2]), e[3].max(b[3])]);
    (e[0] <= e[2] && e[1] <= e[3]).then_some(e)
}

/// The callout's line a label would have where it is now, as offsets from its `x`, `y`: from its
/// feature (`collide.callout_anchor`) to its text's nearest edge, when its class has callouts.
/// For showing a label being moved by hand.
pub fn callout_to(l: &PlacedLabel) -> Option<[f32; 4]> {
    let [ax, ay] = l.collide.callout_anchor.filter(|_| l.collide.callout_reach > 0.0)?;
    let line = callout_line(&label_boxes(l).0, [l.x + ax, l.y + ay])?;
    Some([line[0] - l.x, line[1] - l.y, line[2] - l.x, line[3] - l.y])
}

/// A callout's line, from its feature at `anchor` to the nearest edge of the text's boxes; none
/// when the text covers the feature.
fn callout_line(text: &[[f32; 4]], anchor: [f32; 2]) -> Option<[f32; 4]> {
    let e = boxes_extent(text)?;
    if !anchor.iter().all(|v| v.is_finite()) {
        return None;
    }
    let to = [anchor[0].clamp(e[0], e[2]), anchor[1].clamp(e[1], e[3])];
    ((to[0] - anchor[0]).hypot(to[1] - anchor[1]) >= 1.0).then_some([anchor[0], anchor[1], to[0], to[1]])
}

/// Where a crowded label with a callout goes: the nearest free spot for its text's boxes, inside
/// the view, whose line from its feature at `anchor` to the nearest edge of its text is at most
/// `reach` long (points), and which doesn't cover the feature. The offset that moves it there and
/// that line (from, to), or none.
fn callout_spot(
    text: &[[f32; 4]],
    anchor: [f32; 2],
    reach: f32,
    free: impl Fn(&[f32; 4]) -> bool,
    w: f32,
    h: f32,
    spots: &mut usize,
) -> Option<(f32, f32, [f32; 4])> {
    let ext = boxes_extent(text)?;
    if !(anchor[0].is_finite() && anchor[1].is_finite() && reach.is_finite()) {
        return None;
    }
    // Out in rings half the text's height apart, as far as its line may reach past its size: at
    // most 60 of them, farther apart for a long reach, so the last still gets that far.
    let far = reach + (ext[2] - ext[0]).max(ext[3] - ext[1]);
    let step = ((ext[3] - ext[1]) * 0.5).clamp(2.0, 20.0).max(far / 60.0);
    let rings = ((far / step).ceil() as usize).clamp(1, 60);
    for ring in 1..=rings {
        let d = ring as f32 * step;
        // More directions farther out, so the spots stay about as close together.
        let n = ((d * 0.75) as usize).clamp(8, 64);
        let mut best: Option<(f32, f32, f32, [f32; 4])> = None;
        for k in 0..n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            let (dx, dy) = (d * a.cos(), d * a.sin());
            let moved = |b: &[f32; 4]| [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy];
            let e = moved(&ext);
            if e[0] < 0.0 || e[1] < 0.0 || e[2] > w || e[3] > h {
                continue;
            }
            // The line ends at the text's nearest edge; text over the feature hides it.
            let to = [anchor[0].clamp(e[0], e[2]), anchor[1].clamp(e[1], e[3])];
            let len = (to[0] - anchor[0]).hypot(to[1] - anchor[1]);
            if len > reach || len < 1.0 || best.is_some_and(|b| len >= b.0) {
                continue;
            }
            if *spots == 0 {
                return best.map(|(_, dx, dy, line)| (dx, dy, line));
            }
            *spots -= 1;
            if !text.iter().all(|b| free(&moved(b))) {
                continue;
            }
            best = Some((len, dx, dy, [anchor[0], anchor[1], to[0], to[1]]));
        }
        if let Some((_, dx, dy, line)) = best {
            return Some((dx, dy, line));
        }
    }
    None
}

/// Greedy collision-free placement, highest priority first. Locked labels are all placed (they
/// were placed without collisions when the lock was taken).
///
/// As MapLibre places its symbols: a label's text and its icon each have a box (a turned label's
/// text a chain of them along it) kept `collide`'s padding clear of the others; a label is placed
/// when they fit, or the part that is optional is dropped when only the other fits, and a part
/// allowed to overlap is shown whatever is under it. A part ignoring placement is shown without
/// keeping the others off it. Labels reaching past the view's edge are placed (only those wholly
/// outside it are left out), and the boxes are kept in a grid so a crowded view stays quick.
///
/// A label with alternatives (#1011) tries its own place, then each of them in turn, and takes
/// the first where all of it shows; where none does, its own place shows what fits there.
#[cfg(test)]
fn place_labels(cands: impl IntoIterator<Item = impl Into<Candidate>>, fixed: Vec<PlacedLabel>, w: f32, h: f32, locked: bool) -> Placement {
    place_labels_with_previews(cands, fixed, w, h, locked, true)
}

/// Places labels while retaining failed previews only when View Unplaced needs them.
fn place_labels_with_previews(
    cands: impl IntoIterator<Item = impl Into<Candidate>>,
    fixed: Vec<PlacedLabel>,
    w: f32,
    h: f32,
    locked: bool,
    show_unplaced: bool,
) -> Placement {
    let mut cands: Vec<Candidate> = cands.into_iter().map(Into::into).collect();
    // A priority of no number places last, not first (`total_cmp` puts NaN above everything).
    let rank = |p: f64| if p.is_nan() { f64::NEG_INFINITY } else { p };
    cands.sort_by(|a, b| rank(b.priority).total_cmp(&rank(a.priority)));
    let mut grid = LabelGrid::new(w, h);
    for l in &fixed {
        let (text, icon) = label_boxes(l);
        text.iter().chain(&icon).for_each(|b| grid.insert(*b));
    }
    let mut out = Placement { labels: fixed, unplaced: Vec::new(), keys: Vec::new(), line_anchors: Vec::new() };
    let outside = |b: &[f32; 4]| b[2] < 0.0 || b[3] < 0.0 || b[0] > w || b[1] > h;
    let (mut searches, mut spots) = (MAX_CALLOUT_SEARCHES, MAX_CALLOUT_SPOTS);
    // Same work bound as 20,000 heads with eight positions, shared across all bands.
    let mut layouts_left: usize = 20_000 * 8;
    let mut placed_groups = std::collections::HashSet::new();
    let mut unplaced = Vec::new();
    for mut head in cands.into_iter().take(20_000) {
        let fallbacks = head.line.as_mut().map(|line| std::mem::take(&mut line.fallbacks)).unwrap_or_default();
        // Siblings are contiguous: hold just one temporary preview until the group finishes.
        let mut pending = None;
        for Candidate { label: mut l, key, alternatives, curves, line, .. } in std::iter::once(head).chain(fallbacks) {
            let c = l.collide;
            let feature = c.callout_anchor.map(|[ax, ay]| [l.x + ax, l.y + ay]);
            let grouped = line.as_ref().is_some_and(|line| line.deduplicate);
            if grouped && placed_groups.contains(&key) {
                continue;
            }
            let collide = l.collide;
            let (has_text, has_icon) = (!l.text.is_empty(), l.icon.is_some());
            // Its own place, else the first alternative where all of it shows (the text moves there;
            // an icon stays).
            let mut at = None;
            let count = curves.as_ref().map_or(1 + alternatives.len(), |curves| curves.positions.len());
            let original = LabelPlace::take(&mut l);
            // A failed curve at the middle still appears there once in View Unplaced.
            if curves.is_some() {
                let (text, icon) = label_boxes(&l);
                if !text.iter().chain(&icon).all(outside) {
                    at = Some((0, original.clone(), text, icon, false, false));
                }
            }
            let mut ready = std::iter::once(original).chain(alternatives);
            for i in 0..count {
                let place = match &curves {
                    Some(curves) => {
                        if curves.locked.is_some_and(|won| won != i) {
                            continue;
                        }
                        // The budget only limits alternatives: a label's own place is always tried,
                        // so exhausting it never silently drops later curved labels.
                        if layouts_left == 0 && i > 0 {
                            break;
                        }
                        layouts_left = layouts_left.saturating_sub(1);
                        curves.place(i, &mut l)
                    }
                    None => ready.next(),
                };
                let Some(mut place) = place else { continue };
                if !place.x.is_finite() || !place.y.is_finite() {
                    continue;
                }
                // Locking: a place whose anchor cannot be back-projected cannot be replayed, so never
                // take it (its key would be recorded without an anchor and the label would vanish later).
                if line.as_ref().is_some_and(|line| !line.anchors.is_empty() && line.anchors.get(i).copied().flatten().is_none()) {
                    continue;
                }
                place.swap(&mut l);
                if locked
                    && !l.collide.pinned
                    && let Some([dx, dy]) = l.collide.callout_offset
                {
                    l.x += dx;
                    l.y += dy;
                }
                let (text, icon) = label_boxes(&l);
                if text.iter().chain(&icon).all(outside) {
                    continue;
                }
                let (show_text, show_icon) = shows(&l, &text, &icon, &grid, locked);
                let whole = show_text == has_text && show_icon == has_icon;
                if i == 0 || whole {
                    at = Some((i, LabelPlace::take(&mut l), text, icon, show_text, show_icon));
                }
                if whole {
                    break;
                }
            }
            let Some((i, mut place, mut text, icon, mut show_text, show_icon)) = at else { continue };
            place.swap(&mut l);
            l.collide.callout = None;
            let relative = |l: &PlacedLabel, line: [f32; 4]| [line[0] - l.x, line[1] - l.y, line[2] - l.x, line[3] - l.y];
            let mut moved_by = None;
            if let (true, Some([dx, dy])) = (locked && !collide.pinned, collide.callout_offset) {
                // Locked after a callout moved it: there again, with its line.
                moved_by = Some([dx, dy]);
                if let Some(f) = feature.filter(|_| collide.callout_reach > 0.0)
                    && let Some(line) = callout_line(&text, f)
                {
                    l.collide.callout = Some(relative(&l, line));
                }
            } else if collide.pinned {
                // Moved by hand: where it was put, a callout's line back to its feature when its class
                // has callouts (however long: it was put there).
                if let Some(f) = feature.filter(|_| collide.callout_reach > 0.0)
                    && let Some(line) = callout_line(&text, f)
                {
                    l.collide.callout = Some(relative(&l, line));
                }
            } else if let Some(f) = feature
                && collide.callout_reach > 0.0
                && !locked
                && !has_icon
                && has_text
                && text.iter().any(|b| grid.hits(b))
                && searches > 0
            {
                // A callout: crowded where it is and at its alternatives, the label moves to the
                // nearest free spot within its reach, a line drawn back to its feature. With no such
                // spot it stays, and is left out, or shown over the others when its class shows all.
                searches -= 1;
                if let Some((dx, dy, line)) = callout_spot(&text, f, collide.callout_reach, |b| !grid.hits(b), w, h, &mut spots) {
                    moved_by = Some([dx, dy]);
                    l.x += dx;
                    l.y += dy;
                    for b in &mut text {
                        *b = [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy];
                    }
                    show_text = true;
                    l.collide.callout = Some(relative(&l, line));
                }
            }
            if !show_text && !show_icon {
                if show_unplaced && has_text && unplaced.len() < 2000 && pending.is_none() {
                    pending =
                        Some((grouped.then_some(key), PlacedLabel { color: UNPLACED, halo: Some(halo_for(UNPLACED)), icon: None, ..l }));
                }
                continue;
            }
            if show_text && !collide.text_ignore_placement {
                text.iter().for_each(|b| grid.insert(*b));
            }
            if show_icon && !collide.icon_ignore_placement {
                icon.iter().for_each(|b| grid.insert(*b));
            }
            if !show_text {
                l.text.clear();
            }
            if !show_icon {
                l.icon = None;
            }
            if grouped {
                placed_groups.insert(key);
            }
            if let Some(anchor) = line.as_ref().and_then(|line| line.anchors.get(i)).copied().flatten() {
                out.line_anchors.push((key, anchor));
            }
            // Its feature stays where it is: the anchor is its offset from the label as placed
            // (an alternative place, a callout or a lock may have moved it).
            if let Some(f) = feature {
                l.collide.callout_anchor = Some([f[0] - l.x, f[1] - l.y]);
            }
            out.labels.push(l);
            out.keys.push((key, rustgis_map::LockedPlace { place: u8::try_from(i).unwrap_or(u8::MAX), callout: moved_by }));
        }
        if let Some((key, preview)) = pending
            && key.is_none_or(|k| !placed_groups.contains(&k))
        {
            unplaced.push(preview);
        }
    }
    out.unplaced = unplaced;
    out
}

/// Which of a label's text and icon show with their boxes `text` and `icon` where they are, as
/// MapLibre has it: each where it fits, or may overlap, and a part that isn't optional only with
/// the other.
fn shows(l: &PlacedLabel, text: &[[f32; 4]], icon: &[[f32; 4]], grid: &LabelGrid, locked: bool) -> (bool, bool) {
    let c = l.collide;
    let (has_text, has_icon) = (!text.is_empty(), !icon.is_empty());
    // A label moved by hand shows where it was put, whatever is under it.
    let fits = |boxes: &[[f32; 4]], overlap: bool| locked || overlap || c.pinned || !boxes.iter().any(|b| grid.hits(b));
    let mut show_text = has_text && fits(text, c.text_allow_overlap);
    let mut show_icon = has_icon && fits(icon, c.icon_allow_overlap);
    // Which part may show without the other (MapLibre's rules).
    let icon_alone = c.text_optional || !has_text;
    let text_alone = c.icon_optional || !has_icon;
    if !icon_alone && !text_alone {
        let both = show_text && show_icon;
        (show_text, show_icon) = (both, both);
    } else if !text_alone {
        show_text = show_text && show_icon;
    } else if !icon_alone {
        show_icon = show_icon && show_text;
    }
    (show_text, show_icon)
}

/// A label's collision boxes, [left, top, right, bottom] in points, padded: its text's (one box,
/// or a chain of squares along a turned label) and its icon's.
fn label_boxes(l: &PlacedLabel) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
    let mut text = Vec::new();
    if !l.text.is_empty() {
        let tw = text::label_width(l);
        let th = text::block_height(&l.text, l.size);
        // A halo drawn at a width of its own reaches past the letters (points × 1.33, as the text's
        // size and the drawing have it): another label's text must not fall under it. A width
        // kept with the halo turned off reserves nothing. A background reaches as far as its
        // padding across, and up and down.
        let halo = text::drawn_halo_reach(l);
        let [across, up_down] = text::background_reach(l).map(|r| l.collide.text_padding.clamp(0.0, 100.0) + halo.max(r));
        let p = across.max(up_down);
        let cx = l.center_x();
        if !l.glyphs.is_empty() {
            // A curved label: a square round each character, as tall as the text.
            let r = th / 2.0 + p;
            for g in l.glyphs.iter().take(500) {
                let (x, y) = (l.x + g.dx, l.y + g.dy);
                text.push([x - r, y - r, x + r, y + r]);
            }
            return (text, Vec::new());
        }
        match l.angle.filter(|a| a.is_finite() && a.abs() > 1e-3) {
            Some(a) => {
                // Squares as tall as the text, end to end along it.
                // (A width of no number is one square, so the label still collides.)
                let n = (tw / th.max(1.0)).ceil();
                let n = if n.is_finite() { n.clamp(1.0, 200.0) } else { 1.0 };
                let step = tw / n;
                let r = step.max(th) / 2.0 + p;
                let (dx, dy) = (a.cos(), a.sin());
                for i in 0..n as usize {
                    let t = (i as f32 + 0.5) * step - tw / 2.0;
                    let (x, y) = (cx + dx * t, l.y + dy * t);
                    text.push([x - r, y - r, x + r, y + r]);
                }
            }
            None => text.push([cx - tw / 2.0 - across, l.y - th / 2.0 - up_down, cx + tw / 2.0 + across, l.y + th / 2.0 + up_down]),
        }
    }
    let icon = l
        .icon
        .iter()
        .map(|i| {
            let p = l.collide.icon_padding.clamp(0.0, 100.0);
            // A turned icon: the box round it as turned.
            let (c, s) = i.angle.map_or((1.0, 0.0), |a| (a.cos().abs(), a.sin().abs()));
            let (hw, hh) = ((i.w * c + i.h * s) / 2.0, (i.w * s + i.h * c) / 2.0);
            [i.x - hw - p, i.y - hh - p, i.x + hw + p, i.y + hh + p]
        })
        .collect();
    (text, icon)
}

/// Placed labels' boxes in a grid of cells over the view (and a margin round it), so a box is
/// tested against those near it only.
struct LabelGrid {
    boxes: Vec<[f32; 4]>,
    cells: Vec<Vec<u32>>,
    cols: usize,
    rows: usize,
    /// The grid's top-left corner and the side of a cell, in points.
    x0: f32,
    y0: f32,
    cell: f32,
}

impl LabelGrid {
    const MARGIN: f32 = 512.0;

    fn new(w: f32, h: f32) -> LabelGrid {
        let (w, h) = (if w.is_finite() { w.clamp(1.0, 1e5) } else { 1.0 }, if h.is_finite() { h.clamp(1.0, 1e5) } else { 1.0 });
        let cell = ((w + 2.0 * Self::MARGIN).max(h + 2.0 * Self::MARGIN) / 128.0).max(32.0);
        let cols = ((w + 2.0 * Self::MARGIN) / cell).ceil() as usize + 1;
        let rows = ((h + 2.0 * Self::MARGIN) / cell).ceil() as usize + 1;
        LabelGrid { boxes: Vec::new(), cells: vec![Vec::new(); cols * rows], cols, rows, x0: -Self::MARGIN, y0: -Self::MARGIN, cell }
    }

    /// The cells a box falls in (those past the grid's edge clamped to it): columns, rows.
    fn span(&self, b: &[f32; 4]) -> Option<(std::ops::RangeInclusive<usize>, std::ops::RangeInclusive<usize>)> {
        if !b.iter().all(|v| v.is_finite()) {
            return None;
        }
        let at = |v: f32, o: f32, n: usize| (((v - o) / self.cell).floor().max(0.0) as usize).min(n.saturating_sub(1));
        Some((at(b[0], self.x0, self.cols)..=at(b[2], self.x0, self.cols), at(b[1], self.y0, self.rows)..=at(b[3], self.y0, self.rows)))
    }

    fn insert(&mut self, b: [f32; 4]) {
        let Some((xs, ys)) = self.span(&b) else { return };
        let i = self.boxes.len() as u32;
        self.boxes.push(b);
        for y in ys {
            for x in xs.clone() {
                if let Some(c) = self.cells.get_mut(y * self.cols + x) {
                    c.push(i);
                }
            }
        }
    }

    /// Whether a box overlaps one already placed.
    fn hits(&self, b: &[f32; 4]) -> bool {
        let Some((xs, ys)) = self.span(b) else { return true };
        ys.into_iter().any(|y| {
            xs.clone().any(|x| {
                self.cells.get(y * self.cols + x).is_some_and(|c| {
                    c.iter().any(|&i| self.boxes.get(i as usize).is_some_and(|o| b[0] < o[2] && o[0] < b[2] && b[1] < o[3] && o[1] < b[3]))
                })
            })
        })
    }
}

/// Render passes [`render_complete`] makes at most with nothing come since the last (the wait ran
/// out, or nothing was under way): passes that draw what came are bound by its minute alone.
const IDLE_PASSES: usize = 6;

/// Renders at full resolution once the map's tiles have arrived: each pass fetches what it is
/// missing (a style, then its sources, then their tiles, or a layer's tiles after its picture
/// failed) and waits for news of it, at most a minute in all. In the browser, which can't wait,
/// it draws what has arrived.
pub(crate) fn render_complete(map: &mut Map, view: &View) -> Option<Rendered> {
    complete(map, view, false)
}

/// [`render_complete`] kept as vectors where the frame can be (flat, north up, without view
/// effects): its `recorded` canvas; anything else as pixels.
pub(crate) fn render_complete_recorded(map: &mut Map, view: &View) -> Option<Rendered> {
    complete(map, view, true)
}

/// [`render_complete`]'s passes, each recorded when `record`.
fn complete(map: &mut Map, view: &View, record: bool) -> Option<Rendered> {
    // What had come when it drew: what lands meanwhile is news to the wait.
    let mut drawn_at = rustgis_tiles::cache::generation();
    let mut r = render_cpu(map, view, true, record)?;
    let start = web_time::Instant::now();
    let budget = web_time::Duration::from_secs(60);
    let mut idle = 0;
    // An export cancelled meanwhile (see `rustgis_data::progress`) stops waiting: it writes nothing.
    let cancelled = || rustgis_data::progress::check().is_err();
    while r.tiles_pending && idle < IDLE_PASSES && start.elapsed() < budget && cfg!(not(target_arch = "wasm32")) && !cancelled() {
        // Each wait within what is left of the minute, until something has come: a fetch of
        // another's (a map open beside it, a slow server) doesn't hold it up.
        let left = budget.saturating_sub(start.elapsed());
        rustgis_tiles::cache::wait_for_news_unless(drawn_at, left.min(web_time::Duration::from_secs(30)), cancelled);
        if cancelled() {
            break;
        }
        let now = rustgis_tiles::cache::generation();
        if now == drawn_at {
            idle += 1;
        }
        drawn_at = now;
        r = render_cpu(map, view, true, record)?;
    }
    Some(r)
}

/// [`render_png`]'s map kept as vectors (for a vector PDF) where it can be: flat, north up,
/// without view effects; anything else, and what is pixels by nature, as pictures.
pub fn render_drawing(map: &mut Map, view: &View) -> Option<vector::Drawing> {
    let r = render_complete_recorded(map, view)?;
    let mut pm = match r.recorded {
        Some(c) => c,
        None => {
            // Not recorded: its pixels, placed on a recording.
            let mut c = Canvas::recording(r.pixmap.width(), r.pixmap.height())?;
            c.draw_pixmap(0, 0, r.pixmap.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            c
        }
    };
    for l in r.unplaced.iter().chain(&r.labels) {
        text::draw(&mut pm, l, view.pixels_per_point as f32);
    }
    text::scale_bar(&mut pm, view, map.ground_res_at(&view.camera));
    draw_attribution(&mut pm, &r.attribution, view.pixels_per_point as f32);
    pm.into_drawing()
}

/// Renders a map with its labels, a scale bar and a north arrow to PNG bytes.
pub fn render_png(map: &mut Map, view: &View) -> Result<Vec<u8>, String> {
    let r = render_exported(map, view)?;
    let mut pm = Canvas::from_pixmap(r.pixmap);
    for l in r.unplaced.iter().chain(&r.labels) {
        text::draw(&mut pm, l, view.pixels_per_point as f32);
    }
    text::scale_bar(&mut pm, view, map.ground_res_at(&view.camera));
    draw_attribution(&mut pm, &r.attribution, view.pixels_per_point as f32);
    pm.into_pixmap().encode_png().map_err(|e| e.to_string())
}

/// A map drawn as data ([`render_placed`]), with what it holds besides its pixels.
pub struct Placed {
    /// The pixels, premultiplied as tiny-skia keeps them.
    pub pixmap: Pixmap,
    /// The credits the map's tile layers ask for, for the file to keep.
    pub credits: Vec<String>,
    /// Why parts of it may be empty: tiles or streamed data still on their way when the wait
    /// ran out, tile layers that couldn't be drawn. In a picture placed on the ground a gap looks
    /// like data, so an export says these.
    pub gaps: Vec<String>,
}

/// A map drawn as data, for an image placed on the ground (a GeoTIFF of the view): its layers
/// and labels (the unplaced ones too, when the map shows them, as [`render_png`] draws them),
/// without the scale bar and the credits, which sit on the screen, not the ground. Refuses what
/// [`render_png`] refuses.
pub fn render_placed(map: &mut Map, view: &View) -> Result<Placed, String> {
    let r = render_exported(map, view)?;
    let mut gaps = Vec::new();
    if r.tiles_pending {
        gaps.push("some tiles were still on their way when the export stopped waiting, so parts of tile layers are empty: export again once the map has drawn them".to_string());
    }
    if r.data_pending {
        gaps.push(
            "streamed layers were still reading the view, so some features are missing: export again once the map has drawn them"
                .to_string(),
        );
    }
    if let Some(first) = r.tile_errors.first() {
        let n = r.tile_errors.len();
        gaps.push(if n == 1 {
            format!("a tile layer couldn't be drawn, so its part is empty: {first}")
        } else {
            format!("{n} tile layers couldn't be drawn, so their parts are empty; the first: {first}")
        });
    }
    let mut pm = Canvas::from_pixmap(r.pixmap);
    for l in r.unplaced.iter().chain(&r.labels) {
        text::draw(&mut pm, l, view.pixels_per_point as f32);
    }
    Ok(Placed { pixmap: pm.into_pixmap(), credits: r.attribution, gaps })
}

/// The map drawn whole for an export, or why it can't be written: a raster read in place whose
/// tiles haven't come, or that is drawn from less than the view needs.
fn render_exported(map: &mut Map, view: &View) -> Result<Rendered, String> {
    let r = render_complete(map, view).ok_or("could not allocate the image")?;
    // A raster read in place drawn from less than the view needs isn't written: its tiles still
    // on their way (the browser can't wait; natively the wait ran out), or a coarser level or
    // tiles it couldn't read (`partial`).
    // Other layers' tiles pending export what has drawn, as before.
    if let Some(l) = r.pending_in_place.first().and_then(|id| map.layers.iter().find(|l| l.id == *id)) {
        return Err(format!("{}: its tiles are still on their way: export again once the map has drawn it", l.name));
    }
    if let Some(l) = r.partial.first().and_then(|id| map.layers.iter().find(|l| l.id == *id)) {
        // Its own error, named by the layer, says why; else the general one.
        let why = r.tile_errors.iter().find(|e| e.starts_with(&format!("{}: ", l.name))).cloned();
        let why = why.unwrap_or_else(|| {
            format!("{}: drawn from less than the view needs (a coarser level, or tiles that couldn't be read)", l.name)
        });
        return Err(format!("{why}, so the image isn't written"));
    }
    Ok(r)
}

/// The tile layers' credits, small, in the bottom-right corner.
pub(crate) fn draw_attribution(pm: &mut Canvas, attribution: &[String], ppp: f32) {
    if attribution.is_empty() {
        return;
    }
    let text = attribution.join(" | ");
    let size = 9.0;
    let (w, h) = (pm.width() as f32 / ppp, pm.height() as f32 / ppp);
    let tw = text::measure(&text, size, false).min(w);
    let label = PlacedLabel {
        text,
        x: w - tw / 2.0 - 4.0,
        y: h - size * 0.9,
        size,
        color: Rgba::rgb(0x33, 0x33, 0x33),
        halo: Some(Rgba::rgb(0xFF, 0xFF, 0xFF)),
        halo_width: None,
        bold: false,
        icon: None,
        align: 0.0,
        framed: false,
        italic: false,
        spacing: 0.0,
        font: None,
        line_align: TextAlign::Left,
        angle: None,
        collide: Default::default(),
        glyphs: Vec::new(),
        background: None,
    };
    text::draw(pm, &label, ppp);
}

#[cfg(test)]
mod tests {

    pub(super) fn label(text: &str, x: f32, icon: Option<PlacedIcon>, align: f32) -> PlacedLabel {
        PlacedLabel {
            text: text.into(),
            x,
            y: 20.0,
            size: 10.0,
            color: Rgba::rgb(0, 0, 0),
            halo: None,
            halo_width: None,
            bold: false,
            icon,
            align,
            framed: false,
            italic: false,
            spacing: 0.0,
            font: None,
            line_align: TextAlign::Left,
            angle: None,
            collide: Default::default(),
            glyphs: Vec::new(),
            background: None,
        }
    }

    /// While a label is dragged, its callout's line is worked out where it is: from its feature,
    /// which stays put, to its text's nearest edge; none when its class has no callouts, or its
    /// text covers its feature. Its extent is what picks it.
    #[test]
    fn a_dragged_label_s_callout_follows_it() {
        use rustgis_tiles::Collide;
        let l = PlacedLabel {
            y: 50.0,
            collide: Collide { callout_reach: 40.0, callout_anchor: Some([-60.0, 30.0]), ..Default::default() },
            ..label("Moved", 150.0, None, 0.0)
        };
        let e = label_extent(&l).unwrap();
        assert!(e[0] < 150.0 && 150.0 < e[2] && e[1] < 50.0 && 50.0 < e[3], "{e:?}");
        let [x0, y0, x1, y1] = callout_to(&l).expect("a line");
        assert_eq!((x0, y0), (-60.0, 30.0), "from the feature");
        // To the text's bottom-left corner: the feature is below and to the left of it.
        assert!(((l.x + x1) - e[0]).abs() < 1e-3 && ((l.y + y1) - e[3]).abs() < 1e-3);
        let none = PlacedLabel { collide: Collide { callout_reach: 0.0, ..l.collide }, ..l.clone() };
        assert!(callout_to(&none).is_none(), "no callouts");
        let over = PlacedLabel { collide: Collide { callout_anchor: Some([0.0, 0.0]), ..l.collide }, ..l.clone() };
        assert!(callout_to(&over).is_none(), "the text over its feature");
    }

    /// A label moved by hand is placed where it was put, over another and first, so the others keep
    /// off it; with callouts its line reaches back to its feature, however far it went.
    #[test]
    fn labels_moved_by_hand_are_placed_where_they_were_put() {
        use rustgis_tiles::Collide;
        // Its feature 80 points left of where it was put.
        let pinned = |callout: f32| PlacedLabel {
            y: 50.0,
            collide: Collide { pinned: true, callout_reach: callout, callout_anchor: Some([-80.0, 0.0]), ..Default::default() },
            ..label("Moved", 150.0, None, 0.0)
        };
        let other = PlacedLabel { y: 50.0, ..label("Other", 150.0, None, 0.0) };
        // The other first by priority, the moved one placed first anyway: it keeps its spot.
        let p = place_labels(
            vec![(other.clone(), 9.0, (u32::MAX, 0, 0, 0)), (pinned(0.0), 1e15, (u32::MAX, 1, 0, 0))],
            Vec::new(),
            300.0,
            120.0,
            false,
        );
        assert_eq!(p.labels.first().map(|l| (l.text.as_str(), l.x, l.y)), Some(("Moved", 150.0, 50.0)));
        assert_eq!(p.labels.len(), 1, "the other keeps off it");
        assert!(p.labels[0].collide.callout.is_none(), "no callouts: no line");
        // With callouts: a line from the feature to its text, past any callout's usual reach.
        let p = place_labels(vec![(pinned(5.0), 1e15, (u32::MAX, 1, 0, 0))], Vec::new(), 300.0, 120.0, false);
        let l = &p.labels[0];
        let [x0, y0, x1, y1] = l.collide.callout.expect("a line back to its feature");
        assert!(((l.x + x0) - 70.0).abs() < 1e-3 && ((l.y + y0) - 50.0).abs() < 1e-3);
        assert!((x1 - x0).hypot(y1 - y0) > 5.0);
        // And over whatever is there when it's placed after: a fixed label under it stays too.
        let fixed = vec![other];
        let p = place_labels(vec![(pinned(0.0), 1e15, (u32::MAX, 1, 0, 0))], fixed, 300.0, 120.0, false);
        assert_eq!(p.labels.len(), 2);
    }

    /// A label crowded off its spot moves, with a callout, to the nearest free spot whose line back
    /// to its feature is no longer than its reach, and keeps that line; with every label shown it
    /// stays, over the other; with neither it's left out.
    #[test]
    fn callouts_move_crowded_labels_and_show_all_overlaps_them() {
        use rustgis_tiles::Collide;
        // Two labels on one spot, each by its feature 10 points below the text's centre.
        let at = |text: &str, collide: Collide| PlacedLabel {
            y: 50.0,
            collide: Collide { callout_anchor: Some([0.0, 10.0]), ..collide },
            ..label(text, 100.0, None, 0.0)
        };
        let place = |second: Collide| {
            let cands = vec![(at("First", Collide::default()), 2.0, (u32::MAX, 0, 0, 0)), (at("Second", second), 1.0, (u32::MAX, 1, 0, 0))];
            place_labels(cands, Vec::new(), 300.0, 120.0, false)
        };
        let overlap = |a: &[f32; 4], b: &[f32; 4]| a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
        // Neither: the second is left out.
        let p = place(Collide::default());
        assert_eq!((p.labels.len(), p.unplaced.len()), (1, 1));
        // A callout: moved clear of the first, a line from its feature to its text's edge.
        let p = place(Collide { callout_reach: 40.0, ..Default::default() });
        assert_eq!(p.labels.len(), 2);
        let (first, second) = (&p.labels[0], &p.labels[1]);
        assert!(first.collide.callout.is_none(), "in its own spot, no line");
        let [x0, y0, x1, y1] = second.collide.callout.expect("a callout's line");
        assert!(((second.x + x0) - 100.0).abs() < 1e-3 && ((second.y + y0) - 60.0).abs() < 1e-3, "it starts at the feature");
        // Its anchor names the feature from where it was moved to, as a drag's preview reads it.
        let [ax, ay] = second.collide.callout_anchor.expect("an anchor");
        assert!(((second.x + ax) - 100.0).abs() < 1e-3 && ((second.y + ay) - 60.0).abs() < 1e-3, "anchor follows the move");
        assert_eq!(callout_to(second), second.collide.callout, "the preview's line is the placed one");
        let len = (x1 - x0).hypot(y1 - y0);
        assert!((1.0..=40.0).contains(&len), "{len} points");
        let (a, b) = (label_boxes(first).0, label_boxes(second).0);
        assert!(!a.iter().any(|x| b.iter().any(|y| overlap(x, y))), "clear of the first");
        // No free spot within so short a reach: left out, as without one.
        let p = place(Collide { callout_reach: 1.0, ..Default::default() });
        assert_eq!(p.labels.len(), 1);
        // Every label shown: the second stays in its spot, over the first, with no line.
        let p = place(Collide { text_allow_overlap: true, ..Default::default() });
        assert_eq!(p.labels.len(), 2);
        assert_eq!((p.labels[1].x, p.labels[1].y), (100.0, 50.0));
        assert!(p.labels[1].collide.callout.is_none());
        // Both: a free spot nearby comes before overlapping.
        let p = place(Collide { callout_reach: 40.0, text_allow_overlap: true, ..Default::default() });
        assert!(p.labels[1].collide.callout.is_some());
    }

    /// A callout's line is drawn from its feature to the text, in the label's colour.
    #[test]
    fn a_callout_line_is_drawn() {
        use rustgis_tiles::Collide;
        let mut pm = Canvas::new(200, 100).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let l = PlacedLabel {
            y: 20.0,
            color: Rgba::rgb(200, 0, 0),
            collide: Collide { callout: Some([-60.0, 60.0, -10.0, 10.0]), ..Default::default() },
            ..label("Callout", 150.0, None, 0.0)
        };
        text::draw(&mut pm, &l, 1.0);
        // Halfway along the line (x + y = 170), well clear of the text: red. (The pixel whose
        // centre is on it: the line passes through (115, 55)'s corner.)
        let mid = pm.pixels().pixel(115, 54).unwrap();
        assert!(mid.red() > 120 && mid.green() < 80 && mid.blue() < 80, "{mid:?}");
        assert!(pm.pixels().pixel(115, 20).is_some_and(|p| p.green() > 200), "nothing above the line");
    }

    /// A halo of its own width reaches past the letters, so its label's collision box grows by it:
    /// another label's text never falls under it. The thin halo and none leave the box as it was.
    #[test]
    fn a_wide_halo_keeps_other_labels_out_of_its_reach() {
        let plain = label("Rhein", 50.0, None, 0.0);
        let mut haloed = plain.clone();
        (haloed.halo, haloed.halo_width) = (Some(Rgba::rgb(255, 255, 255)), Some(5.0));
        // As far as the halo is drawn: 5 points, at the 1.33 the text's size is drawn at.
        let reach = 5.0 * 1.33;
        let (a, b) = (label_boxes(&plain).0[0], label_boxes(&haloed).0[0]);
        assert!((a[0] - b[0] - reach).abs() < 1e-4 && (b[2] - a[2] - reach).abs() < 1e-4, "{a:?} {b:?}");
        assert!((a[1] - b[1] - reach).abs() < 1e-4 && (b[3] - a[3] - reach).abs() < 1e-4, "{a:?} {b:?}");
        haloed.halo_width = None;
        assert_eq!(label_boxes(&haloed).0[0], a);
        // A width kept with the halo turned off, or a fully see-through halo: nothing drawn,
        // nothing reserved.
        let off = PlacedLabel { halo: None, halo_width: Some(5.0), ..plain.clone() };
        assert_eq!(label_boxes(&off).0[0], a);
        let clear = PlacedLabel { halo: Some(Rgba::new(255, 255, 255, 0)), halo_width: Some(5.0), ..plain.clone() };
        assert_eq!(label_boxes(&clear).0[0], a);
        // A halo grown at a reference scale reserves all it reaches: labels are placed once for
        // every pixel density, and only drawing holds a halo to MAX_HALO_PX pixels.
        let grown = PlacedLabel { halo_width: Some(300.0), ..haloed.clone() };
        let g = label_boxes(&grown).0[0];
        assert!((a[0] - g[0] - 300.0 * 1.33).abs() < 1e-2, "{g:?}");
        // Turned and curved labels' squares grow by it too.
        haloed.halo_width = Some(5.0);
        let turned = |l: &PlacedLabel| PlacedLabel { angle: Some(0.5), ..l.clone() };
        let (a, b) = (label_boxes(&turned(&plain)).0[0], label_boxes(&turned(&haloed)).0[0]);
        assert!(((b[2] - b[0]) - (a[2] - a[0]) - 2.0 * reach).abs() < 1e-4, "{a:?} {b:?}");
    }

    #[test]
    fn places_icons_with_their_labels() {
        let icon = |x: f32| {
            let mut p = Pixmap::new(4, 4).unwrap();
            p.fill(tiny_skia::Color::from_rgba8(0, 0, 255, 255));
            PlacedIcon { image: std::sync::Arc::new(p), x, y: 20.0, w: 10.0, h: 10.0, angle: None, alpha: 1.0 }
        };
        // Two icons on the same spot collide even with no text; one far away doesn't.
        let cands = vec![
            (label("", 50.0, Some(icon(50.0)), 0.0), 2.0, (u32::MAX, 0, 0, 0)),
            (label("", 52.0, Some(icon(52.0)), 0.0), 1.0, (u32::MAX, 1, 0, 0)),
            (label("", 150.0, Some(icon(150.0)), 0.0), 0.0, (u32::MAX, 2, 0, 0)),
        ];
        let placed = place_labels(cands, Vec::new(), 200.0, 40.0, false);
        assert_eq!(placed.labels.iter().map(|l| l.x).collect::<Vec<_>>(), vec![50.0, 150.0]);
        // Left-aligned text starts at x.
        let l = label("Station", 100.0, None, -0.5);
        assert!((l.center_x() - (100.0 + text::measure("Station", 10.0, false) / 2.0)).abs() < 1e-3);
        // An icon is drawn into the map at its size.
        let mut pm = Pixmap::new(200, 40).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| draw_icon(c, &icon(50.0), 1.0));
        let px = pm.pixel(50, 20).unwrap();
        assert_eq!((px.blue(), px.alpha()), (255, 255));
        assert_eq!(pm.pixel(60, 20).unwrap().alpha(), 0);
    }

    /// An icon's `alpha` fades it (a tile layer's opacity, #1358); out-of-range or non-finite values
    /// never panic or draw outside 0 to 1.
    #[test]
    fn an_icon_is_drawn_at_its_alpha() {
        let alpha = |a: f32| {
            let mut pm = Pixmap::new(20, 20).unwrap();
            let icon = PlacedIcon { alpha: a, ..icon_at(10.0, 10.0, 10.0, 10.0, None) };
            crate::canvas::with_canvas(&mut pm, |c| draw_icon(c, &icon, 1.0));
            pm.pixel(10, 10).unwrap().alpha()
        };
        assert_eq!(alpha(1.0), 255);
        assert!((i32::from(alpha(0.5)) - 128).abs() <= 2, "{}", alpha(0.5));
        assert_eq!(alpha(0.0), 0);
        assert_eq!(alpha(7.0), 255);
        assert_eq!(alpha(-1.0), 0);
        assert_eq!(alpha(f32::NAN), 255);
    }

    fn icon_at(x: f32, y: f32, w: f32, h: f32, angle: Option<f32>) -> PlacedIcon {
        let mut p = Pixmap::new(4, 4).unwrap();
        p.fill(tiny_skia::Color::from_rgba8(0, 0, 255, 255));
        PlacedIcon { image: std::sync::Arc::new(p), x, y, w, h, angle, alpha: 1.0 }
    }

    fn texts(p: &Placement) -> Vec<&str> {
        p.labels.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn labels_keep_their_padding_above_and_below() {
        // Two labels a point apart vertically: their padding (2 on every side) keeps them apart.
        let a = PlacedLabel { y: 20.0, ..label("Upper", 100.0, None, 0.0) };
        let gap = text::block_height("Upper", 10.0) + 1.0;
        let b = PlacedLabel { y: 20.0 + gap, ..label("Lower", 100.0, None, 0.0) };
        let placed = place_labels(
            vec![(a.clone(), 2.0, (u32::MAX, 0, 0, 0)), (b.clone(), 1.0, (u32::MAX, 1, 0, 0))],
            Vec::new(),
            200.0,
            80.0,
            false,
        );
        assert_eq!(texts(&placed), ["Upper"]);
        // Without padding they fit.
        let none = rustgis_tiles::Collide { text_padding: 0.0, ..Default::default() };
        let (a, b) = (PlacedLabel { collide: none, ..a }, PlacedLabel { collide: none, ..b });
        let placed = place_labels(vec![(a, 2.0, (u32::MAX, 0, 0, 0)), (b, 1.0, (u32::MAX, 1, 0, 0))], Vec::new(), 200.0, 80.0, false);
        assert_eq!(texts(&placed), ["Upper", "Lower"]);
    }

    #[test]
    fn optional_text_and_icons_drop_alone() {
        use rustgis_tiles::Collide;
        // A word in the way of a station's name, not of its icon (to the left of it).
        let blocker = || (label("Blocker", 100.0, None, 0.0), 9.0, (u32::MAX, 0, 0, 0));
        let station =
            |collide: Collide| PlacedLabel { collide, ..label("Station", 100.0, Some(icon_at(40.0, 20.0, 10.0, 10.0, None)), 0.0) };
        let run =
            |collide: Collide| place_labels(vec![blocker(), (station(collide), 1.0, (u32::MAX, 1, 0, 0))], Vec::new(), 200.0, 40.0, false);
        // Neither optional: the station isn't shown at all.
        let p = run(Collide::default());
        assert_eq!(p.labels.len(), 1);
        // Its text optional: the icon shows on its own.
        let p = run(Collide { text_optional: true, ..Default::default() });
        assert_eq!(p.labels.len(), 2);
        assert!(p.labels[1].text.is_empty() && p.labels[1].icon.is_some());
        // Its icon optional, and the icon in the way instead: the text shows on its own.
        let icon_blocked = vec![
            (label("", 40.0, Some(icon_at(40.0, 20.0, 10.0, 10.0, None)), 0.0), 9.0, (u32::MAX, 0, 0, 0)),
            (station(Collide { icon_optional: true, ..Default::default() }), 1.0, (u32::MAX, 1, 0, 0)),
        ];
        let p = place_labels(icon_blocked, Vec::new(), 200.0, 40.0, false);
        assert_eq!(p.labels.len(), 2);
        assert!(p.labels[1].text == "Station" && p.labels[1].icon.is_none());
        // Allowed to overlap, the text shows over the blocker; ignoring placement, a later label
        // goes over it.
        let p = run(Collide { text_allow_overlap: true, ..Default::default() });
        assert_eq!(p.labels.len(), 2);
        assert!(p.labels[1].text == "Station" && p.labels[1].icon.is_some());
        let ghost =
            PlacedLabel { collide: Collide { text_ignore_placement: true, ..Default::default() }, ..label("Ghost", 100.0, None, 0.0) };
        let p = place_labels(
            vec![(ghost, 9.0, (u32::MAX, 0, 0, 0)), (label("Real", 100.0, None, 0.0), 1.0, (u32::MAX, 1, 0, 0))],
            Vec::new(),
            200.0,
            40.0,
            false,
        );
        assert_eq!(texts(&p), ["Ghost", "Real"]);
    }

    #[test]
    fn labels_reaching_past_the_edge_are_placed() {
        let half_out = label("Edge of the map", 0.0, None, 0.0);
        let gone = label("Gone", -500.0, None, 0.0);
        let p = place_labels(vec![(half_out, 1.0, (u32::MAX, 0, 0, 0)), (gone, 0.0, (u32::MAX, 1, 0, 0))], Vec::new(), 200.0, 40.0, false);
        assert_eq!(texts(&p), ["Edge of the map"]);
        assert!(p.unplaced.is_empty(), "a label wholly off the view isn't unplaced either");
    }

    #[test]
    fn turned_labels_collide_along_their_text() {
        // A long label at 45° and a short one in the corner of its bounding box, clear of the text.
        let long = "A long street name along the line";
        let w = text::measure(long, 10.0, false);
        let turned = PlacedLabel { x: 100.0, y: 100.0, angle: Some(std::f32::consts::FRAC_PI_4), ..label(long, 100.0, None, 0.0) };
        let corner = PlacedLabel { x: 100.0 + w * 0.3, y: 100.0 - w * 0.3, ..label("Hi", 0.0, None, 0.0) };
        let across = PlacedLabel { x: 100.0 + w * 0.3, y: 100.0 + w * 0.3, ..label("Hi", 0.0, None, 0.0) };
        let p = place_labels(
            vec![(turned, 3.0, (u32::MAX, 0, 0, 0)), (corner, 2.0, (u32::MAX, 1, 0, 0)), (across, 1.0, (u32::MAX, 2, 0, 0))],
            Vec::new(),
            400.0,
            400.0,
            false,
        );
        assert_eq!(p.labels.len(), 2, "the corner one fits, the one on the text doesn't");
        assert!(p.labels[1].y < 100.0);
    }

    /// A label whose own place is taken tries its alternatives in turn and takes the first free
    /// one (#1011); where none is, it is unplaced once, at its own place.
    #[test]
    fn labels_try_their_alternatives() {
        let blocker = || Candidate::from((label("Blocker", 100.0, None, 0.0), 9.0, (u32::MAX, 0, 0, 0)));
        let taken = |alternatives: Vec<(f32, f32)>| Candidate {
            label: label("Taken", 100.0, None, 0.0),
            priority: 1.0,
            key: (1, 0, 0, 0),
            alternatives: alternatives.into_iter().map(LabelPlace::from).collect(),
            curves: None,
            line: None,
            row: 0,
        };
        let at = |p: &Placement| p.labels.iter().map(|l| (l.text.clone(), l.x, l.y)).collect::<Vec<_>>();
        // None: left out, as before.
        let p = place_labels(vec![blocker(), taken(Vec::new())], Vec::new(), 400.0, 200.0, false);
        assert_eq!(at(&p), [("Blocker".to_string(), 100.0, 20.0)]);
        assert_eq!(p.unplaced.len(), 1);
        // The first free one, past one that is taken too.
        let p = place_labels(vec![blocker(), taken(vec![(104.0, 20.0), (300.0, 20.0), (300.0, 100.0)])], Vec::new(), 400.0, 200.0, false);
        assert_eq!(at(&p)[1], ("Taken".to_string(), 300.0, 20.0));
        let place = |place| rustgis_map::LockedPlace { place, callout: None };
        assert_eq!(p.keys, [((u32::MAX, 0, 0, 0), place(0)), ((1, 0, 0, 0), place(2))], "the place it took, for a lock");
        assert!(p.unplaced.is_empty());
        // All taken: unplaced once, where it would have gone.
        let p = place_labels(vec![blocker(), taken(vec![(104.0, 20.0), (96.0, 22.0)])], Vec::new(), 400.0, 200.0, false);
        assert_eq!(p.labels.len(), 1);
        assert_eq!(p.unplaced.iter().map(|l| (l.x, l.y)).collect::<Vec<_>>(), [(100.0, 20.0)]);
        // Locked, its own place is kept whatever is under it.
        let p = place_labels(vec![blocker(), taken(vec![(300.0, 20.0)])], Vec::new(), 400.0, 200.0, true);
        assert_eq!(at(&p)[1], ("Taken".to_string(), 100.0, 20.0));
    }

    #[test]
    fn alternatives_keep_their_angle_and_curved_glyphs() {
        let base = label("ab", 100.0, None, 0.0);
        let first = LabelPlace::of(&base);
        let other = LabelPlace {
            x: 300.0,
            y: 80.0,
            angle: Some(0.4),
            glyphs: vec![
                LabelGlyph { text: "a".into(), dx: -5.0, dy: 1.0, angle: 0.2 },
                LabelGlyph { text: "b".into(), dx: 5.0, dy: -1.0, angle: 0.4 },
            ],
        };
        let candidate = Candidate {
            label: base.clone(),
            priority: 1.0,
            key: (1, 0, 0, 0),
            curves: None,
            line: None,
            row: 0,
            alternatives: vec![other.clone()],
        };
        let blocker = label("Blocker", 100.0, None, 0.0);
        let p = place_labels(vec![candidate.clone()], vec![blocker], 400.0, 200.0, false);
        assert_eq!(LabelPlace::of(&p.labels[1]), other);
        assert_eq!(p.keys, [((1, 0, 0, 0), rustgis_map::LockedPlace { place: 1, callout: None })]);
        // If every place collides, View Unplaced retains the original geometry, once.
        let p = place_labels(
            vec![candidate],
            vec![base, PlacedLabel { x: other.x, y: other.y, ..label("Blocker", 0.0, None, 0.0) }],
            400.0,
            200.0,
            false,
        );
        assert_eq!(p.unplaced.len(), 1);
        assert_eq!(LabelPlace::of(&p.unplaced[0]), first);
    }

    #[test]
    fn tilted_alternatives_transform_their_own_glyph_geometry() {
        let base = label("ab", 100.0, None, 0.0);
        let alternative = LabelPlace {
            x: 100.0,
            y: 80.0,
            angle: Some(0.4),
            glyphs: vec![
                LabelGlyph { text: "a".into(), dx: -5.0, dy: 1.0, angle: 0.2 },
                LabelGlyph { text: "b".into(), dx: 5.0, dy: -1.0, angle: 0.4 },
            ],
        };
        let c = Candidate {
            label: base,
            priority: 1.0,
            key: (1, 0, 0, 0),
            curves: None,
            line: None,
            row: 0,
            alternatives: vec![alternative.clone()],
        };
        let projected = tilt_candidate(c, &|x, y| (y > 50.0).then_some((2.0 * x + 10.0, 2.0 * y))).unwrap();
        assert!(projected.alternatives.is_empty());
        assert_eq!((projected.label.x, projected.label.y), (210.0, 160.0));
        assert!((projected.label.angle.unwrap() - 0.4).abs() < 1e-5);
        for (got, original) in projected.label.glyphs.iter().zip(&alternative.glyphs) {
            assert_eq!((got.dx, got.dy), (original.dx * 2.0, original.dy * 2.0));
            assert!((got.angle - original.angle).abs() < 1e-5);
        }
    }

    /// In a tilted view a label whose own place is out of sight (in the sky, say) goes to the
    /// first of its alternatives in sight, and keeps the others in sight to try.
    #[test]
    fn tilted_labels_fall_back_on_an_alternative_in_sight() {
        // In sight below y = 100 only, and moved 1000 right there.
        let at = |x: f32, y: f32| (y > 100.0).then_some((x + 1000.0, y));
        let offer = |(x, y): (f32, f32), alternatives: Vec<(f32, f32)>| Candidate {
            label: PlacedLabel { x, y, ..label("Paris", 0.0, None, 0.0) },
            priority: 1.0,
            key: (1, 0, 0, 0),
            alternatives: alternatives.into_iter().map(LabelPlace::from).collect(),
            curves: None,
            line: None,
            row: 0,
        };
        let c = tilt_candidate(offer((10.0, 50.0), vec![(10.0, 60.0), (10.0, 150.0), (20.0, 160.0)]), &at).unwrap();
        assert_eq!((c.label.x, c.label.y), (1010.0, 150.0));
        assert_eq!(c.alternatives, [LabelPlace::from((1020.0, 160.0))]);
        // Its own place in sight stays its own.
        let c = tilt_candidate(offer((10.0, 120.0), vec![(10.0, 50.0), (10.0, 150.0)]), &at).unwrap();
        assert_eq!((c.label.x, c.label.y), (1010.0, 120.0));
        assert_eq!(c.alternatives, [LabelPlace::from((1010.0, 150.0))]);
        // None in sight: left out.
        assert!(tilt_candidate(offer((10.0, 50.0), vec![(10.0, 60.0)]), &at).is_none());
    }

    /// In a tilted view, a candidate the bands repeat (the same layer, row and class) is placed
    /// once, but two classes' labels of one feature are two labels (#732).
    #[test]
    fn tilted_views_keep_each_class_label() {
        let map = Map::new("Map");
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 5000.0, bearing: 0.0, pitch: 30.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let (a, b) = (label("Capitals", 100.0, None, 0.0), label("Default", 300.0, None, 0.0));
        let texts = |c: Vec<(PlacedLabel, f64, rustgis_map::LabelKey)>| -> Vec<String> {
            place_tilted_labels(&map, &view, c.into_iter().map(Candidate::from).collect(), None)
                .labels
                .into_iter()
                .map(|l| l.text)
                .collect()
        };
        assert_eq!(texts(vec![(a.clone(), 2.0, (1, 0, 0, 0)), (b.clone(), 1.0, (1, 0, 1, 0))]), ["Capitals", "Default"]);
        assert_eq!(texts(vec![(a, 2.0, (1, 0, 0, 0)), (b, 1.0, (1, 0, 0, 0))]), ["Capitals"], "a band's repeat");
        // A line's repeats along it: each band numbers those on its stretch, so two bands' repeat 1
        // at different places are both kept, and the same repeat offered twice in one place once.
        let (near, far) = (label("Töss", 100.0, None, 0.0), label("Töss", 300.0, None, 0.0));
        assert_eq!(texts(vec![(near.clone(), 2.0, (1, 0, 0, 1)), (far, 1.0, (1, 0, 0, 1))]).len(), 2, "repeats of two bands");
        assert_eq!(texts(vec![(near.clone(), 2.0, (1, 0, 0, 1)), (near, 1.0, (1, 0, 0, 1))]).len(), 1, "one repeat twice");
    }

    #[test]
    fn places_many_labels_quickly() {
        // A grid of 20,000 candidates, a third of them fitting: placed without testing each
        // against every other, also when each has seven places round it to try (#1011).
        let round = [(-6.0, -6.0), (6.0, -6.0), (-6.0, 6.0), (6.0, 6.0), (-8.0, 0.0), (8.0, 0.0), (0.0, 8.0)];
        let cands = |others: bool| -> Vec<Candidate> {
            (0..20_000)
                .map(|i| {
                    let (x, y) = ((i % 200) as f32 * 10.0, (i / 200) as f32 * 10.0);
                    let l = PlacedLabel { x, y, ..label("ab", x, None, 0.0) };
                    let alternatives = if others { round.map(|(dx, dy)| LabelPlace::from((x + dx, y + dy))).to_vec() } else { Vec::new() };
                    Candidate {
                        label: l,
                        priority: -f64::from(i),
                        key: (u32::MAX, i as u64, 0, 0),
                        alternatives,
                        curves: None,
                        line: None,
                        row: usize::MAX,
                    }
                })
                .collect()
        };
        for others in [false, true] {
            let start = web_time::Instant::now();
            let p = place_labels(cands(others), Vec::new(), 2000.0, 1000.0, false);
            assert!(!p.labels.is_empty() && p.labels.len() < 20_000);
            assert!(start.elapsed() < web_time::Duration::from_secs(5), "with places round them: {others}");
        }
    }

    #[test]
    fn tile_labels_are_sized_and_wrapped_as_maplibre() {
        let l = rustgis_tiles::Label {
            text: "West 42nd Street & Avenue of the Americas\nOuest".into(),
            x: 100.0,
            y: 100.0,
            size: 12.0,
            color: [0, 0, 0, 255],
            halo: None,
            bold: false,
            italic: true,
            letter_spacing: 0.1,
            max_width: 9.0,
            justify: 0.0,
            priority: 7.5,
            anchor: [0.0, 0.0],
            offset: [0.0, 0.0],
            angle: None,
            icon: None,
            collide: Default::default(),
        };
        let (p, pri) = tiles::tile_label(l.clone()).unwrap();
        // text-size 12 is a 12-pixel font.
        assert!((p.size * 1.33 - 12.0).abs() < 1e-3);
        assert_eq!(pri, 7.5);
        assert!(p.italic && (p.spacing - 0.1 * p.size).abs() < 1e-4 && p.line_align == TextAlign::Center);
        // Wrapped to about 9 ems (108 pixels), the line break of its own kept.
        let lines: Vec<&str> = text::lines(&p.text).collect();
        assert!(lines.len() >= 3, "{lines:?}");
        assert_eq!(lines.last(), Some(&"Ouest"));
        for line in &lines {
            assert!(text::measure(line, p.size, false) < 108.0 * 1.4, "{line} is too long");
        }
        // Along a line, never wrapped; turned with it.
        let along =
            rustgis_tiles::Label { max_width: 0.0, angle: Some(0.5), text: "West 42nd Street & Avenue of the Americas".into(), ..l };
        let (p, _) = tiles::tile_label(along).unwrap();
        assert_eq!((text::line_count(&p.text), p.angle), (1, Some(0.5)));
    }

    #[test]
    fn wraps_balanced_at_spaces_and_dashes() {
        let width = |s: &str| text::measure(s, 9.0, false);
        let w = width("Times Square–42nd Street");
        let lines = text::wrap("Times Square–42nd Street", 9.0, false, 0.0, w * 0.6);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.join(" ").replace("– ", "–") == "Times Square–42nd Street", "{lines:?}");
        // Balanced: neither line much longer than the other.
        let (a, b) = (width(&lines[0]), width(&lines[1]));
        assert!(a.max(b) / a.min(b) < 2.0, "{lines:?}");
        // A space after an "&" is kept.
        let lines = text::wrap("West 42nd Street & 8th Avenue", 9.0, false, 0.0, width("West 42nd Street"));
        assert!(lines.iter().all(|l| !l.contains("&8")), "{lines:?}");
        // What fits, or has nowhere to break, is left alone; junk is safe.
        assert_eq!(text::wrap("Short", 9.0, false, 0.0, 1000.0), ["Short"]);
        assert_eq!(text::wrap("Unbreakable", 9.0, false, 0.0, 1.0), ["Unbreakable"]);
        assert!(!text::wrap("a b c", f32::NAN, false, f32::INFINITY, -1.0).is_empty());
    }

    #[test]
    fn turned_text_and_icons_are_drawn_turned() {
        // Ink from a long label at 90° runs down the pixmap, not across it.
        let ink = |pm: &Pixmap| {
            let (mut x0, mut x1, mut y0, mut y1) = (u32::MAX, 0, u32::MAX, 0);
            for y in 0..pm.height() {
                for x in 0..pm.width() {
                    if pm.pixel(x, y).is_some_and(|p| p.alpha() > 0) {
                        (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
                    }
                }
            }
            (x1.saturating_sub(x0), y1.saturating_sub(y0))
        };
        let l = PlacedLabel { x: 100.0, y: 100.0, size: 12.0, ..label("Broadway Broadway", 100.0, None, 0.0) };
        let mut flat = Pixmap::new(200, 200).unwrap();
        crate::canvas::with_canvas(&mut flat, |c| text::draw(c, &l, 1.0));
        let mut turned = Pixmap::new(200, 200).unwrap();
        crate::canvas::with_canvas(&mut turned, |c| text::draw(c, &PlacedLabel { angle: Some(std::f32::consts::FRAC_PI_2), ..l }, 1.0));
        let ((fw, fh), (tw, th)) = (ink(&flat), ink(&turned));
        if fw > 0 {
            assert!(fw > fh && th > tw, "flat {fw}×{fh}, turned {tw}×{th}");
            assert!(th.abs_diff(fw) <= 3, "the same length either way");
        }
        // An arrow 20 long and 4 wide, turned to point down.
        let mut pm = Pixmap::new(40, 40).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| draw_icon(c, &icon_at(20.0, 20.0, 20.0, 4.0, Some(std::f32::consts::FRAC_PI_2)), 1.0));
        assert!(pm.pixel(20, 28).unwrap().alpha() > 0 && pm.pixel(28, 20).unwrap().alpha() == 0);
    }

    #[test]
    fn labels_turn_with_a_turned_view() {
        // A view turned by 30°: a label along a line turns with it, kept upright; one upright on
        // the screen stays so.
        let a = 30f32.to_radians();
        let at = |x: f32, y: f32| Some((x * a.cos() - y * a.sin(), x * a.sin() + y * a.cos()));
        let mut along =
            PlacedLabel { angle: Some(0.0), icon: Some(icon_at(0.0, 0.0, 4.0, 4.0, Some(3.0))), ..label("Road", 50.0, None, 0.0) };
        turn_with(&mut along, &at);
        assert!(along.angle.is_some_and(|b| (b - a).abs() < 1e-3));
        assert!(
            along
                .icon
                .as_ref()
                .and_then(|i| i.angle)
                .is_some_and(
                    |b| (b - 3.0 - a).rem_euclid(std::f32::consts::TAU).min((3.0 + a - b).rem_euclid(std::f32::consts::TAU)) < 1e-3
                )
        );
        let mut flipped = PlacedLabel { angle: Some(80f32.to_radians()), ..label("Road", 50.0, None, 0.0) };
        turn_with(&mut flipped, &at);
        assert!(flipped.angle.is_some_and(|b| (b - (110f32 - 180.0).to_radians()).abs() < 1e-3), "{:?}", flipped.angle);
        let mut upright = label("Town", 50.0, None, 0.0);
        turn_with(&mut upright, &at);
        assert_eq!(upright.angle, None);
    }

    use super::*;
    use rustgis_data::{Field, FieldType, Value};

    #[test]
    fn worldview_detection_and_data() {
        use sample_data::Worldview::{China, Default};
        let detect = |tz: Option<&str>, locales: &[&str]| sample_data::Worldview::detect(tz, locales.iter().copied());
        assert_eq!(detect(Some("Asia/Shanghai"), &["en-US"]), China);
        assert_eq!(detect(Some("Asia/Urumqi"), &[]), China);
        assert_eq!(detect(Some("Europe/Berlin"), &["zh-CN", "en"]), China);
        assert_eq!(detect(None, &["zh_CN.UTF-8"]), China);
        assert_eq!(detect(None, &["zh-Hans-CN"]), China);
        assert_eq!(detect(Some("Asia/Taipei"), &["zh-TW"]), Default);
        assert_eq!(detect(Some("Asia/Hong_Kong"), &["zh-HK"]), Default);
        assert_eq!(detect(Some("America/New_York"), &["en-US", "zh-CN"]), Default);
        assert_eq!(detect(None, &[]), Default);
        assert_eq!(sample_data::Worldview::from_code(" China "), Some(China));
        assert_eq!(sample_data::Worldview::from_code("default"), Some(Default));
        assert_eq!(sample_data::Worldview::from_code("auto"), None);
        // Both files have the same fields; in China's view Taiwan is part of China.
        let read = |text| rustgis_data::io::read_geojson(text, "Countries").unwrap().0;
        let (def, chn) = (read(sample_data::COUNTRIES), read(sample_data::COUNTRIES_CHN));
        let fields = |d: &Dataset| d.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(fields(&def), fields(&chn));
        let names = |d: &Dataset| {
            let i = d.fields.iter().position(|f| f.name == "name").unwrap();
            d.features
                .iter()
                .filter_map(|f| if let Some(Value::Text(t)) = f.attrs.get(i) { Some(t.clone()) } else { None })
                .collect::<Vec<_>>()
        };
        assert!(names(&def).iter().any(|n| n == "Taiwan"));
        assert!(!names(&chn).iter().any(|n| n == "Taiwan"));
        assert!(names(&chn).iter().any(|n| n == "China"));
    }

    #[test]
    fn the_globe_drape_is_the_flat_map_of_its_box() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into())]);
        m.add_dataset(d);
        m.projection = rustgis_map::Projection::Globe;
        let paris = lonlat_to_merc(Coord::new(2.35, 48.85));
        let camera = Camera { center: paris, res: 2_000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let boxes = Globe::new(&camera, 400.0, 300.0).drape_extents(400.0, 300.0, 0.25);
        let patches = render_globe_drape(&mut m, &view, &boxes, 4096);
        assert_eq!(patches.len(), 1);
        // The map is left as it was.
        assert!(m.projection.is_globe() && m.label_lock.is_none() && !m.label_display.paused);
        let p = &patches[0];
        assert!(p.merc.contains(paris) && p.merc.contains(lonlat_to_merc(boxes[0].center())));
        // Paris's marker is where the box puts it, as the flat map draws it.
        let px = |pm: &Pixmap, x: f64, y: f64| pm.pixel(x as u32, y as u32).map(|c| (c.red(), c.green(), c.blue()));
        let res = p.merc.width() / f64::from(p.pixmap.width());
        let at = px(&p.pixmap, (paris.x - p.merc.min_x) / res, (p.merc.max_y - paris.y) / res);
        m.projection = rustgis_map::Projection::WebMercator;
        let flat = render(&mut m, &View { camera: Camera { center: paris, res, bearing: 0.0, pitch: 0.0 }, ..view }).unwrap();
        assert_eq!(at, px(&flat.pixmap, 200.0, 150.0));
        // Too big a box is drawn coarser, within the limit; junk draws nothing.
        let big = render_globe_drape(&mut m, &view, &[Extent::new(-180.0, -80.0, 180.0, 80.0)], 512);
        assert!(big.iter().all(|p| p.pixmap.width() <= 512 && p.pixmap.height() <= 512));
        // A box at the Web Mercator limit stays exactly there however coarse the picture, so its
        // mesh still reaches the pole.
        let lim = rustgis_geom::MAX_LAT;
        // (30 km points leave a remainder: the picture is 1335 points tall, 0.8 of a point short.)
        let coarse = View { camera: Camera { center: paris, res: 30_000.0, bearing: 0.0, pitch: 0.0 }, ..view };
        let world = render_globe_drape(&mut m, &coarse, &[Extent::new(-180.0, -lim, 180.0, lim)], 4096);
        let edge = std::f64::consts::PI * rustgis_geom::MERCATOR_R * 0.9999;
        assert!(
            world.iter().all(|p| p.merc.max_y >= edge && p.merc.min_y <= -edge),
            "{:?}",
            world.iter().map(|p| p.merc).collect::<Vec<_>>()
        );
        assert!(
            render_globe_drape(
                &mut m,
                &View { camera: Camera { center: paris, res: f64::NAN, bearing: 0.0, pitch: 0.0 }, ..view },
                &boxes,
                4096
            )
            .is_empty()
        );
        assert!(render_globe_drape(&mut m, &view, &[Extent::EMPTY], 4096).is_empty());
        // The sprites: a square around the disc, or nothing for a junk radius.
        let (under, over) = globe_sprites(rustgis_map::Basemap::Paper.palette().0, true, 100.0).unwrap();
        assert_eq!((under.width(), over.height()), (214, 214));
        assert_eq!(under.pixel(107, 107).map(|c| c.alpha()), Some(255));
        // Off the Earth no atmosphere glows past the limb (#223).
        assert!(under.pixel(107 + 103, 107).is_some_and(|c| c.alpha() > 0));
        let (airless, _) = globe_sprites(rustgis_map::Basemap::Paper.palette().0, false, 100.0).unwrap();
        assert_eq!(airless.pixel(107 + 103, 107).map(|c| c.alpha()), Some(0));
        assert_eq!(airless.pixel(107, 107).map(|c| c.alpha()), Some(255));
        assert!(globe_sprites(rustgis_map::Basemap::Paper.palette().0, true, f32::NAN).is_none());
    }

    #[test]
    fn the_starfield_is_the_same_sky_every_time() {
        let a = starfield(2.0).unwrap();
        assert_eq!((a.width(), a.height()), (1024, 1024));
        assert_eq!(a.data(), starfield(2.0).unwrap().data());
        // Stars on a transparent sky.
        let lit = a.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(lit > 100 && lit < a.pixels().len() / 20, "{lit}");
        // Junk scales still give a tile.
        assert!(starfield(f32::NAN).is_some_and(|t| t.width() == 512));
        assert!(starfield(1e9).is_some_and(|t| t.width() == 2048));
    }

    /// A label moved by hand is drawn where it was put, on the flat map: its place is kept as
    /// longitude and latitude, and drawn through the map's projection as its feature is.
    /// (Taken for the projection's own coordinates, it was drawn off by Null Island instead.)
    #[test]
    fn a_label_moved_by_hand_is_drawn_where_it_was_put() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into())]);
        d.push(Some(Geometry::Point(Coord::new(4.83, 45.76))), vec![Value::Text("Lyon".into())]);
        let id = m.add_dataset(d);
        let paris = m.layer(id).unwrap().dataset.features.first().unwrap().id;
        if let Some(l) = m.layer_mut(id) {
            l.labels.enabled = true;
            l.labels.default.callout = Some(40.0);
            assert!(l.labels.default.move_label(paris, Some([5.0, 49.5])));
        }
        let camera = Camera { center: lonlat_to_merc(Coord::new(3.6, 47.3)), res: 5000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false };
        let on_screen = |m: &Map, lon: f64, lat: f64| view.to_screen(m.to_map(Coord::new(lon, lat)).unwrap());
        let labels = render(&mut m, &view).unwrap().labels;
        let l = labels.iter().find(|l| l.text == "Paris").expect("Paris's label is drawn");
        let (ex, ey) = on_screen(&m, 5.0, 49.5);
        assert!((f64::from(l.x) - ex).abs() < 0.5 && (f64::from(l.y) - ey).abs() < 0.5, "at ({}, {}), not ({ex}, {ey})", l.x, l.y);
        assert!(l.collide.pinned);
        assert_eq!(l.collide.source, Some((id, paris, 0)), "what the Move Label tool grabs it by");
        // Its callout's line starts at Paris.
        let [x0, y0, ..] = l.collide.callout.expect("a line back to Paris");
        let (px, py) = on_screen(&m, 2.35, 48.85);
        assert!((f64::from(l.x + x0) - px).abs() < 0.5 && (f64::from(l.y + y0) - py).abs() < 0.5);
        // Lyon's, not moved, is where its class puts it, with no line.
        let lyon = labels.iter().find(|l| l.text == "Lyon").unwrap();
        assert!(!lyon.collide.pinned && lyon.collide.callout.is_none());
    }

    /// A label moved by hand into view is drawn whatever its feature's place: a point's, or a
    /// line's labelled along it, out of view, its callout's line reaching back off the screen.
    #[test]
    fn a_label_moved_into_view_is_drawn_with_its_feature_out_of_it() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into())]);
        let pts = m.add_dataset(d);
        let mut d = Dataset::new("lines", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(1.0, 44.0), Coord::new(3.0, 44.0)]])), vec![Value::Text("Garonne".into())]);
        let lines = m.add_dataset(d);
        for (id, to) in [(pts, [20.0, 50.0]), (lines, [20.0, 47.0])] {
            let fid = m.layer(id).unwrap().dataset.features.first().unwrap().id;
            let l = m.layer_mut(id).unwrap();
            l.labels.enabled = true;
            l.labels.default.callout = Some(40.0);
            if id == lines {
                l.labels.default.placement = LabelPlacement::Parallel;
            }
            assert!(l.labels.default.move_label(fid, Some(to)));
        }
        // Over Central Europe: France is off the left of the view.
        let camera = Camera { center: lonlat_to_merc(Coord::new(20.0, 48.5)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false };
        let on_screen = |m: &Map, lon: f64, lat: f64| view.to_screen(m.to_map(Coord::new(lon, lat)).unwrap());
        assert!(on_screen(&m, 2.35, 48.85).0 < 0.0 && on_screen(&m, 3.0, 44.0).0 < 0.0, "the features are out of view");
        let labels = render(&mut m, &view).unwrap().labels;
        for (text, at) in [("Paris", (20.0, 50.0)), ("Garonne", (20.0, 47.0))] {
            let l = labels.iter().find(|l| l.text == text).unwrap_or_else(|| panic!("{text}'s label is drawn"));
            let (ex, ey) = on_screen(&m, at.0, at.1);
            assert!((f64::from(l.x) - ex).abs() < 0.5 && (f64::from(l.y) - ey).abs() < 0.5, "{text}");
            assert!(l.collide.pinned && l.angle.is_none() && l.glyphs.is_empty(), "{text}: upright where it was put");
            let [x0, ..] = l.collide.callout.unwrap_or_else(|| panic!("{text}: a line back to its feature"));
            assert!(l.x + x0 < 0.0, "{text}: its line starts off the screen");
        }
    }

    /// A long callout is looked for as far as it may reach (the search's rings spaced out to get
    /// there), not only near the label.
    #[test]
    fn a_long_callout_reaches_a_far_free_spot() {
        // A small text by its feature at (100, 100); only spots 200 points right of it are free.
        let text = [[95.0, 98.0, 105.0, 102.0]];
        let free = |b: &[f32; 4]| b[0] > 300.0;
        let (dx, _, [x0, y0, x1, y1]) = callout_spot(&text, [100.0, 100.0], 300.0, free, 1000.0, 200.0, &mut 100_000).expect("a spot");
        assert!(dx > 195.0);
        let len = (x1 - x0).hypot(y1 - y0);
        assert!((150.0..=300.0).contains(&len), "{len} points");
        // Beyond its reach, none.
        assert!(callout_spot(&text, [100.0, 100.0], 100.0, free, 1000.0, 200.0, &mut 100_000).is_none());
        // The placement's spots used up: it stops looking, and says how many it tried.
        let mut spots = 5;
        assert!(callout_spot(&text, [100.0, 100.0], 300.0, free, 1000.0, 200.0, &mut spots).is_none());
        assert_eq!(spots, 0);
    }

    /// A point label turned by its angle expression may still move off by a callout: only labels
    /// along lines are left out of callouts (it was left out, silently, for any angle).
    #[test]
    fn a_rotated_point_label_moves_off_by_a_callout() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("First place".into())]);
        d.push(Some(Geometry::Point(Coord::new(0.01, 0.0))), vec![Value::Text("Second place".into())]);
        let id = m.add_dataset(d);
        let l = m.layer_mut(id).unwrap();
        l.labels.enabled = true;
        l.labels.default.callout = Some(60.0);
        l.labels.default.by.angle = Some("30".into());
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false };
        let labels = render(&mut m, &view).unwrap().labels;
        assert_eq!(labels.len(), 2, "both drawn");
        assert!(labels.iter().all(|l| l.angle.is_some()), "turned");
        assert!(labels.iter().any(|l| l.collide.callout.is_some()), "one moved off by a callout");
    }

    /// Two points close together: the second label moves off by a callout. Locked, it keeps that
    /// place and its line, as the map moves too (it went back onto the first, without its line).
    #[test]
    fn a_lock_keeps_a_label_where_its_callout_moved_it() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("First place".into())]);
        d.push(Some(Geometry::Point(Coord::new(0.01, 0.0))), vec![Value::Text("Second place".into())]);
        let id = m.add_dataset(d);
        let l = m.layer_mut(id).unwrap();
        l.labels.enabled = true;
        l.labels.default.callout = Some(60.0);
        let at = |x: f64| {
            let camera = Camera { center: lonlat_to_merc(Coord::new(x, 0.0)), res: 500.0, bearing: 0.0, pitch: 0.0 };
            View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false }
        };
        let placed = |m: &mut Map, view: &View| {
            let mut labels = render(m, view).unwrap().labels;
            labels.sort_by(|a, b| a.text.cmp(&b.text));
            labels
        };
        let free = placed(&mut m, &at(0.0));
        assert_eq!(free.len(), 2);
        let line = free.iter().filter_map(|l| l.collide.callout).next().expect("one moved off by a callout");
        // Locked: the same places and line, frame after frame.
        m.label_display.locked = true;
        for _ in 0..2 {
            let locked = placed(&mut m, &at(0.0));
            assert_eq!(locked.len(), 2);
            for (a, b) in free.iter().zip(&locked) {
                assert!((a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01, "{}: ({}, {}) to ({}, {})", a.text, a.x, a.y, b.x, b.y);
            }
            assert_eq!(locked.iter().filter_map(|l| l.collide.callout).next(), Some(line));
        }
        // The map panned: the two labels stay as far apart, the line the same.
        let panned = placed(&mut m, &at(0.02));
        assert_eq!(panned.len(), 2);
        let apart = |l: &[PlacedLabel]| (l[1].x - l[0].x, l[1].y - l[0].y);
        let (a, b) = (apart(&free), apart(&panned));
        assert!((a.0 - b.0).abs() < 0.5 && (a.1 - b.1).abs() < 0.5, "{a:?} {b:?}");
        assert_eq!(panned.iter().filter_map(|l| l.collide.callout).next(), Some(line));
    }

    /// On a turned or tilted map a callout's line starts at its feature there too, not where the
    /// north-up frame had it.
    #[test]
    fn a_callout_starts_at_its_feature_on_a_turned_map() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into())]);
        let id = m.add_dataset(d);
        let fid = m.layer(id).unwrap().dataset.features.first().unwrap().id;
        let l = m.layer_mut(id).unwrap();
        l.labels.enabled = true;
        l.labels.default.callout = Some(40.0);
        assert!(l.labels.default.move_label(fid, Some([3.5, 49.5])));
        for (bearing, pitch) in [(40.0, 0.0), (0.0, 40.0), (40.0, 30.0)] {
            let camera = Camera { center: lonlat_to_merc(Coord::new(2.9, 49.1)), res: 1000.0, bearing, pitch };
            let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false };
            let labels = render(&mut m, &view).unwrap().labels;
            let l = labels.iter().find(|l| l.text == "Paris").expect("drawn");
            let [x0, y0, ..] = l.collide.callout.expect("a line back to Paris");
            let (px, py) = view.to_screen(m.to_map(Coord::new(2.35, 48.85)).unwrap());
            let off = (f64::from(l.x + x0) - px).hypot(f64::from(l.y + y0) - py);
            assert!(off < 1.0, "bearing {bearing}, pitch {pitch}: {off} points off");
        }
    }

    /// Each label class labels every feature its clause takes, as in ArcGIS Pro (#732): a feature
    /// two classes take gets a label from each, the class listed first placed first. A hidden
    /// class labels none, and a clause that no longer reads takes none (#618).
    #[test]
    fn label_classes_label_their_features() {
        let mut m = Map::new("Map");
        let fields = vec![Field::new("name", FieldType::Text), Field::new("capital", FieldType::Integer)];
        let mut d = Dataset::new("pts", GeometryType::Point, fields);
        d.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into()), Value::Int(1)]);
        d.push(Some(Geometry::Point(Coord::new(4.83, 45.76))), vec![Value::Text("Lyon".into()), Value::Int(0)]);
        let id = m.add_dataset(d);
        let capitals = |l: &mut rustgis_map::Layer, w: &str| {
            let base = l.labels.default.clone();
            l.labels.classes = vec![rustgis_map::LabelClass { name: "Capitals".into(), where_: Some(w.into()), size: 20.0, ..base }];
        };
        if let Some(l) = m.layer_mut(id) {
            l.labels.enabled = true;
            capitals(l, "capital = 1");
        }
        let camera = Camera { center: lonlat_to_merc(Coord::new(3.6, 47.3)), res: 5000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false };
        let sizes = |m: &mut Map| {
            let mut v: Vec<(String, f32)> = render(m, &view).unwrap().labels.into_iter().map(|l| (l.text, l.size)).collect();
            v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
            v
        };
        let default = m.layer(id).unwrap().labels.default.size;
        assert!(default < 20.0, "so Paris's labels sort Default first");
        let both = [("Lyon".to_string(), default), ("Paris".to_string(), default)];
        // Default takes Paris too, but its label sits where Capitals' does: Capitals, listed
        // first, is placed and Default's collides.
        assert_eq!(sizes(&mut m), [("Lyon".to_string(), default), ("Paris".to_string(), 20.0)], "Paris is a capital");
        // Default below, Capitals above: both of Paris's labels fit.
        m.layer_mut(id).unwrap().labels.default.placement = rustgis_map::LabelPlacement::Below;
        m.layer_mut(id).unwrap().labels.classes[0].placement = rustgis_map::LabelPlacement::Above;
        assert_eq!(
            sizes(&mut m),
            [("Lyon".to_string(), default), ("Paris".to_string(), default), ("Paris".to_string(), 20.0)],
            "a feature two classes take gets a label from each"
        );
        // Locked with one of Paris's labels placed, it keeps that one only.
        m.layer_mut(id).unwrap().labels.default.placement = rustgis_map::LabelPlacement::Above;
        m.label_display.locked = true;
        let placed = sizes(&mut m);
        assert_eq!(m.label_lock.as_ref().map(Vec::len), Some(2));
        m.layer_mut(id).unwrap().labels.default.placement = rustgis_map::LabelPlacement::Below;
        assert_eq!(sizes(&mut m), placed, "Default's label of Paris wasn't placed when locked");
        m.label_display.locked = false;
        m.label_lock = None;
        // Hidden, the class labels nothing; Default still labels Paris.
        m.layer_mut(id).unwrap().labels.classes[0].visible = false;
        assert_eq!(sizes(&mut m), both);
        // A clause that doesn't read takes no feature.
        capitals(m.layer_mut(id).unwrap(), "gone = 1");
        assert_eq!(sizes(&mut m), both);
    }

    /// A class's text, size, colour and angle by expressions, per feature; null text: no label.
    #[test]
    fn label_expressions_set_each_labels_text_size_colour_and_angle() {
        let mut m = Map::new("Map");
        let fields = vec![Field::new("name", FieldType::Text), Field::new("pop", FieldType::Double)];
        let mut d = Dataset::new("pts", GeometryType::Point, fields);
        d.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into()), Value::Float(2.1e6)]);
        d.push(Some(Geometry::Point(Coord::new(4.83, 45.76))), vec![Value::Text("Lyon".into()), Value::Float(5.2e5)]);
        d.push(Some(Geometry::Point(Coord::new(5.37, 43.3))), vec![Value::Null, Value::Float(8.7e5)]);
        let id = m.add_dataset(d);
        m.layer_mut(id).unwrap().labels.enabled = true;
        let camera = Camera { center: lonlat_to_merc(Coord::new(3.6, 46.3)), res: 5000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false };
        let labels = |m: &mut Map| {
            let mut v: Vec<(String, f32, [u8; 4], Option<f32>)> =
                render(m, &view).unwrap().labels.into_iter().map(|l| (l.text, l.size, l.color.0, l.angle)).collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        let class = m.layer(id).unwrap().labels.default.clone();
        let plain = labels(&mut m);
        assert_eq!(plain.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["Lyon", "Paris"], "from its field");
        assert!(plain.iter().all(|l| l.1 == class.size && l.2 == class.color.0 && l.3.is_none()));
        m.layer_mut(id).unwrap().labels.default.by = rustgis_map::label_by::LabelBy {
            text: Some("upper(name) || ' ' || round(pop / 1e6, 1)".into()),
            size: Some("scale_linear(pop, 0, 2e6, 8, 20)".into()),
            color: Some("CASE WHEN pop > 1e6 THEN '#ff0000' END".into()),
            angle: Some("if(pop > 1e6, 90, 0)".into()),
        };
        let by = labels(&mut m);
        // Marseille's name is null: so is its text, and it isn't labelled.
        assert_eq!(by.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["LYON 0.5", "PARIS 2.1"]);
        let (lyon, paris) = (&by[0], &by[1]);
        assert_eq!((paris.1, paris.2), (20.0, [255, 0, 0, 255]));
        assert!(paris.3.is_some_and(|a| (a - 90f32.to_radians()).abs() < 1e-4), "{:?}", paris.3);
        assert!((lyon.1 - 11.12).abs() < 1e-3, "{}", lyon.1);
        // No colour or a zero angle: the class's colour, upright.
        assert_eq!((lyon.2, lyon.3), (class.color.0, None));
        // Past 90°, a label is kept upright: 135° reads as -45°.
        m.layer_mut(id).unwrap().labels.default.by.angle = Some("135".into());
        let turned = labels(&mut m);
        assert!(turned.iter().all(|l| l.3.is_some_and(|a| (a + 45f32.to_radians()).abs() < 1e-4)), "{turned:?}");
        // A text expression labels a layer without the class's field; one that no longer reads
        // leaves the text to it.
        m.layer_mut(id).unwrap().labels.default.field = "gone".into();
        assert_eq!(labels(&mut m).len(), 2);
        m.layer_mut(id).unwrap().labels.default.by.text = Some("gone || 'x'".into());
        assert!(labels(&mut m).is_empty(), "neither the expression nor the field reads");
    }

    /// A lock keeps source ids wider than a wasm32 pointer distinct, including when a label
    /// that collided at lock time later has room to draw (#1135).
    #[test]
    fn locked_labels_keep_full_width_feature_ids() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        for (name, lon) in [("Alpha", -0.1), ("Beta", 0.1)] {
            d.push(Some(Geometry::Point(Coord::new(lon, 0.0))), vec![Value::Text(name.into())]);
        }
        d.features[0].id = 1;
        d.features[1].id = (1_u64 << 32) + 1;
        let id = m.add_dataset(d);
        m.layer_mut(id).unwrap().labels.enabled = true;
        m.label_display.locked = true;
        let mut view = View {
            width: 800.0,
            height: 600.0,
            pixels_per_point: 1.0,
            camera: Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 5000.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let texts = |m: &mut Map, view: &View| render(m, view).unwrap().labels.into_iter().map(|l| l.text).collect::<Vec<_>>();
        assert_eq!(texts(&mut m, &view), ["Alpha"]);
        view.camera.res /= 16.0;
        assert_eq!(texts(&mut m, &view), ["Alpha"], "Beta was not placed when the lock was taken");
        m.label_display.locked = false;
        assert_eq!(texts(&mut m, &view), ["Alpha", "Beta"]);
        m.label_display.locked = true;
        assert_eq!(texts(&mut m, &view), ["Alpha", "Beta"]);
        let mut ids: Vec<u64> = m.label_lock.as_ref().unwrap().iter().map(|((_, f, ..), _)| *f).collect();
        ids.sort_unstable();
        assert_eq!(ids, [1, (1_u64 << 32) + 1]);
    }

    /// A point's label whose place another takes goes round the point when its class says so
    /// (#1011): off, it is left out as before; locked, it stays where it went as the map moves;
    /// turned and tilted views place it too.
    #[test]
    fn point_labels_go_round_their_point() {
        let mut m = Map::new("Map");
        let paris = Coord::new(2.35, 48.85);
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(paris)), vec![Value::Text("Paris".into())]);
        let id = m.add_dataset(d);
        // Capitals, listed first, takes the place above the point that Default wants too. A big
        // marker keeps the labels round it well apart: a tilted view draws their distance from
        // the point on the ground, shortened up and down, while the labels stay their size.
        if let Some(l) = m.layer_mut(id) {
            l.renderer = rustgis_map::Renderer::Single(Symbol { size: 20.0, ..l.symbol(0) });
            l.labels.enabled = true;
            l.labels.default.placement = rustgis_map::LabelPlacement::Above;
            let base = l.labels.default.clone();
            l.labels.classes = vec![rustgis_map::LabelClass { name: "Capitals".into(), size: 20.0, ..base }];
        }
        let view = |pitch: f64, bearing: f64, center: Coord| {
            let camera = Camera { center: lonlat_to_merc(center), res: 2000.0, bearing, pitch };
            View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false }
        };
        // Each label's size and place, Capitals' first.
        let labels = |m: &mut Map, v: &View| {
            let mut l: Vec<(f32, f32, f32)> = render(m, v).unwrap().labels.into_iter().map(|l| (l.size, l.x, l.y)).collect();
            l.sort_by(|a, b| b.0.total_cmp(&a.0));
            l
        };
        let flat = view(0.0, 0.0, paris);
        assert_eq!(labels(&mut m, &flat).len(), 1, "off, Default's label is left out");
        m.layer_mut(id).unwrap().labels.default.other_positions = true;
        let placed = labels(&mut m, &flat);
        assert_eq!(placed.len(), 2, "on, it goes round the point: {placed:?}");
        assert!(placed[1].2 > placed[0].2, "below Capitals' label: {placed:?}");
        // Locked, it stays there as the map moves.
        m.label_display.locked = true;
        assert_eq!(labels(&mut m, &flat), placed);
        assert!(m.label_lock.as_ref().is_some_and(|k| k.iter().any(|(_, i)| i.place > 0)), "{:?}", m.label_lock);
        let moved = labels(&mut m, &view(0.0, 0.0, Coord::new(2.6, 48.85)));
        let apart = |l: &[(f32, f32, f32)]| (l[1].1 - l[0].1, l[1].2 - l[0].2);
        assert_eq!(moved.len(), 2);
        let (a, b) = (apart(&placed), apart(&moved));
        assert!((a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3, "{placed:?} {moved:?}");
        m.label_display.locked = false;
        m.label_lock = None;
        for v in [view(0.0, 30.0, paris), view(30.0, 0.0, paris)] {
            assert_eq!(labels(&mut m, &v).len(), 2, "turned {}°, tilted {}°", v.camera.bearing, v.camera.pitch);
        }
    }

    #[test]
    fn renders_world_and_layers() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into())]);
        let id = m.add_dataset(d);
        if let Some(l) = m.layer_mut(id) {
            l.labels.enabled = true;
            l.selection = vec![0];
        }
        let camera = Camera { center: lonlat_to_merc(Coord::new(2.35, 48.85)), res: 5000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let r = render(&mut m, &view).unwrap();
        assert_eq!(r.labels.len(), 1);
        assert_eq!(r.labels[0].text, "Paris");
        let png = render_png(&mut m, &view).unwrap();
        assert!(png.len() > 1000);
        assert!(!world().is_empty());
    }

    #[test]
    fn a_selected_text_graphic_is_framed_with_its_label() {
        let mut m = Map::new("Map");
        let id = m.add_graphics("Graphics");
        let at = Coord::new(2.35, 48.85);
        if let Some(l) = m.layer_mut(id) {
            l.dataset.push(Some(Geometry::Point(at)), vec![Value::Text("text".into()), Value::Text("Selected text".into())]);
        }
        let camera = Camera { center: lonlat_to_merc(at), res: 5000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let framed = |m: &mut Map| render(m, &view).unwrap().labels.iter().find(|l| l.text == "Selected text").map(|l| l.framed);
        assert_eq!(framed(&mut m), Some(false));
        if let Some(l) = m.layer_mut(id) {
            l.selection = vec![0];
        }
        assert_eq!(framed(&mut m), Some(true));
        // Drawn into an image (an export), the frame goes around the text in the selection colour.
        let r = render(&mut m, &view).unwrap();
        let mut pm = Pixmap::new(400, 300).unwrap();
        for l in &r.labels {
            crate::canvas::with_canvas(&mut pm, |c| text::draw(c, l, 1.0));
        }
        let edge = (200.0 - text::measure("Selected text", 14.0, false) / 2.0 - 3.0).round() as u32;
        let cyan = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|c| c.alpha() > 200 && c.red() < 60 && c.green() > 180 && c.blue() > 200);
        assert!((0..300).any(|y| (edge - 1..=edge + 1).any(|x| cyan(x, y))), "no frame at x = {edge}");
    }

    /// A text of several lines is drawn in an image (an export) on as many lines, as the map
    /// view shows it, not run together on one (#577); its box and frame hold every line.
    #[test]
    fn a_text_of_several_lines_is_drawn_on_as_many() {
        let at = |text: &str, framed: bool| PlacedLabel { y: 150.0, size: 20.0, framed, ..label(text, 200.0, None, 0.0) };
        // The rows and columns with ink.
        let ink = |l: &PlacedLabel| {
            let mut pm = Pixmap::new(400, 300).unwrap();
            crate::canvas::with_canvas(&mut pm, |c| text::draw(c, l, 1.0));
            let dark = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|c| c.alpha() > 128 && c.red() < 60);
            let rows: Vec<u32> = (0..300).filter(|y| (0..400).any(|x| dark(x, *y))).collect();
            let cols: Vec<u32> = (0..400).filter(|x| (0..300).any(|y| dark(*x, y))).collect();
            (rows, cols)
        };
        let (rows, cols) = ink(&at("Atlantic\nOcean", false));
        let (one_rows, one_cols) = ink(&at("AtlanticOcean", false));
        // Two bands of ink with a gap between, each the height of one line.
        let bands = rows.windows(2).filter(|w| w[1] > w[0] + 1).count() + 1;
        assert_eq!(bands, 2, "rows with ink: {rows:?}");
        let span = |v: &[u32]| v.last().copied().unwrap_or(0).saturating_sub(v.first().copied().unwrap_or(0));
        assert!(span(&rows) > span(&one_rows) * 3 / 2, "taller than one line");
        assert!(span(&cols) < span(&one_cols) * 3 / 4, "narrower than the lines run together");
        // Lines start at the block's left, the block centred on the point.
        let (_, alone) = ink(&at("Atlantic", false));
        assert!(cols.first().zip(alone.first()).is_some_and(|(a, b)| a.abs_diff(*b) <= 2), "the longer line keeps its place");
        // As wide as the widest line, as tall as its lines; a trailing newline is an empty line.
        assert_eq!(text::spaced_block_width("ab\ncdef", 10.0, text::Face::default(), 0.0), text::measure("cdef", 10.0, false));
        // Text drawn on one line (a table cell, a chart's name) is still measured whole, so it is
        // cut to its column as it is drawn.
        assert_eq!(text::measure("ab\ncdef", 10.0, false), text::measure("abcdef", 10.0, false));
        assert_eq!((text::line_count("a"), text::line_count("a\nb"), text::line_count("a\n")), (1, 2, 2));
        assert_eq!(text::line_count(&"\n".repeat(10_000)), 200, "lines are bounded");
        // The collision box spaces its lines as they are drawn, a line pitch apart, not one
        // line's height (1.25 × size) each: a label whose top is just past where two such
        // heights would end, but still over the drawn second line, is left out.
        assert!((text::block_height("a\nb\nc", 20.0) - text::block_height("a", 20.0) - 2.0 * text::line_pitch(20.0)).abs() < 1e-3);
        let one_line = 20.0 * 1.25;
        let (old_bottom, gap) = (150.0 + one_line, 2.0);
        let below = PlacedLabel { y: old_bottom + gap + one_line / 2.0, ..at("Below", false) };
        let p = place_labels(vec![(below, 0.0, (1, 0, 0, 0))], vec![at("Atlantic\nOcean", false)], 400.0, 300.0, false);
        assert!(p.labels.iter().all(|l| l.text != "Below"), "placed over the second line");
        // The selection frame goes round both lines.
        let mut pm = Pixmap::new(400, 300).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| text::draw(c, &at("Atlantic\nOcean", true), 1.0));
        let cyan = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|c| c.alpha() > 200 && c.red() < 60 && c.green() > 180 && c.blue() > 200);
        let framed: Vec<u32> = (0..300).filter(|y| (0..400).any(|x| cyan(x, *y))).collect();
        assert!(framed.first().zip(rows.first()).is_some_and(|(f, r)| f < r), "frame above the first line");
        assert!(framed.last().zip(rows.last()).is_some_and(|(f, r)| f > r), "frame below the last line");
    }

    /// A text graphic's lines are aligned in their block, slanted and letter-spaced as it says
    /// (#535), in an image as on screen; plain labels are drawn as before.
    #[test]
    fn a_text_graphic_is_drawn_aligned_slanted_and_spaced() {
        let at = |text: &str| PlacedLabel { y: 150.0, size: 20.0, ..label(text, 200.0, None, 0.0) };
        // Each band of ink (a line) as (top, bottom, left, right), top to bottom.
        let bands = |l: &PlacedLabel| {
            let mut pm = Pixmap::new(400, 300).unwrap();
            crate::canvas::with_canvas(&mut pm, |c| text::draw(c, l, 1.0));
            let dark = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|c| c.alpha() > 128 && c.red() < 60);
            let rows: Vec<u32> = (0..300).filter(|y| (0..400).any(|x| dark(x, *y))).collect();
            let mut out: Vec<(u32, u32, u32, u32)> = Vec::new();
            for y in rows {
                let cols: Vec<u32> = (0..400).filter(|x| dark(*x, y)).collect();
                let (lo, hi) = (cols.first().copied().unwrap_or(0), cols.last().copied().unwrap_or(0));
                match out.last_mut() {
                    Some(b) if y == b.1 + 1 => *b = (b.0, y, b.2.min(lo), b.3.max(hi)),
                    _ => out.push((y, y, lo, hi)),
                }
            }
            out
        };
        // Lines of clearly different widths, so where the short one sits shows its alignment.
        let two = "North Atlantic\nOcean";
        // Left, as labels have always been: both lines start at the block's left.
        let left = bands(&at(two));
        assert_eq!(left.len(), 2);
        assert!(left[0].2.abs_diff(left[1].2) <= 2, "left: {left:?}");
        // Centred: the short line sits in the middle of the long one; right: their right edges meet.
        let centred = bands(&PlacedLabel { line_align: TextAlign::Center, ..at(two) });
        let (a, b) = (centred[0], centred[1]);
        let gap = |x: u32, y: u32| i64::from(x) - i64::from(y);
        assert!((gap(b.2, a.2) - gap(a.3, b.3)).abs() <= 3, "centred: {centred:?}");
        assert!(b.2 > a.2 + 20, "the short line moved in: {centred:?}");
        let right = bands(&PlacedLabel { line_align: TextAlign::Right, ..at(two) });
        assert!(right[0].3.abs_diff(right[1].3) <= 2, "right: {right:?}");
        // The block stays centred on its point whichever way its lines are aligned.
        for b in [&left, &centred, &right] {
            let (l, r) = (b.iter().map(|x| x.2).min().unwrap_or(0), b.iter().map(|x| x.3).max().unwrap_or(0));
            assert!(((l + r) as f32 / 2.0 - 200.0).abs() <= 3.0, "block centred at {}", (l + r) / 2);
        }
        // Letter-spaced: wider by the spacing between each letter, still centred; measured so.
        let plain = bands(&at("Spaced"))[0];
        let spaced = PlacedLabel { spacing: 6.0, ..at("Spaced") };
        let wide = bands(&spaced)[0];
        let grew = (gap(wide.3, wide.2) - gap(plain.3, plain.2)) as f32;
        assert!((grew - 5.0 * 6.0 * 1.33).abs() <= 3.0, "grew {grew}");
        // Placed as the map view lays it out: its advance and the spacing between its letters (5
        // gaps in "Spaced"), not its ink; a slant doesn't widen it either.
        let added = text::label_width(&spaced) - text::label_width(&at("Spaced"));
        assert!((added - 5.0 * 6.0 * 1.33).abs() < 0.01, "added {added}");
        assert_eq!(text::label_width(&PlacedLabel { italic: true, ..at("Slant") }), text::label_width(&at("Slant")));
        // Spaced tighter than its letters are wide, it is still no less than nothing.
        assert!(text::label_width(&PlacedLabel { spacing: -5.0, size: 4.0, ..at("iiiiii") }) >= 0.0);
        // Slanted: an upright stem leans right at the top.
        let stem = |italic: bool| {
            let mut pm = Pixmap::new(400, 300).unwrap();
            crate::canvas::with_canvas(&mut pm, |c| text::draw(c, &PlacedLabel { italic, size: 60.0, ..at("l") }, 1.0));
            let dark = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|c| c.alpha() > 128);
            let rows: Vec<u32> = (0..300).filter(|y| (0..400).any(|x| dark(x, *y))).collect();
            let mid = |y: u32| {
                let c: Vec<u32> = (0..400).filter(|x| dark(*x, y)).collect();
                (c.first().copied().unwrap_or(0) + c.last().copied().unwrap_or(0)) as i64 / 2
            };
            mid(rows.first().copied().unwrap_or(0) + 2) - mid(rows.last().copied().unwrap_or(0).saturating_sub(2))
        };
        assert!(stem(false).abs() <= 1 && stem(true) >= 6, "lean: upright {}, italic {}", stem(false), stem(true));
        // A text graphic's fields reach the label; a plain one's are neutral.
        let mut m = Map::new("Map");
        let id = m.add_graphics("Graphics");
        let p = Coord::new(2.35, 48.85);
        if let Some(l) = m.layer_mut(id) {
            let field = |name: &str| l.dataset.field_index(name).unwrap();
            let (fi, fa, fs) = (field("italic"), field("align"), field("letter_spacing"));
            let mut attrs = vec![Value::Null; l.dataset.fields.len()];
            attrs[0] = Value::Text("text".into());
            attrs[1] = Value::Text("Styled".into());
            (attrs[fi], attrs[fa], attrs[fs]) = (Value::Bool(true), Value::Text("right".into()), Value::Float(4.0));
            l.dataset.push(Some(Geometry::Point(p)), attrs);
            l.dataset.push(Some(Geometry::Point(Coord::new(2.45, 48.80))), vec![Value::Text("text".into()), Value::Text("Plain".into())]);
        }
        let camera = Camera { center: lonlat_to_merc(p), res: 5000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let r = render(&mut m, &view).unwrap();
        let get = |t: &str| r.labels.iter().find(|l| l.text == t).map(|l| (l.italic, l.line_align, l.spacing));
        assert_eq!(get("Styled"), Some((true, TextAlign::Right, 4.0)));
        assert_eq!(get("Plain"), Some((false, TextAlign::Left, 0.0)));
    }

    #[test]
    fn degenerate_views_do_not_panic() {
        let mut m = Map::new("Map");
        for (w, h) in [(0.0, 0.0), (1e9, 1e9), (f64::NAN, 3.0)] {
            let view = View { width: w, height: h, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
            let _ = render(&mut m, &view);
        }
    }

    /// The world sample the golden pixmap test renders.
    fn sample_world() -> Map {
        let mut m = Map::new("Map");
        for (name, text) in [
            ("Countries", sample_data::COUNTRIES),
            ("Rivers", sample_data::RIVERS),
            ("Lakes", sample_data::LAKES),
            ("Cities", sample_data::CITIES),
        ] {
            if let Some(d) = world_dataset(name, text) {
                m.add_dataset(d);
            }
        }
        m
    }

    fn fnv1a(data: &[u8]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for b in data {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Golden hash of the sample world render: a byte-lock proving the GPU-backend refactor
    /// (Backend/RenderOutput plumbing) never changed the CPU path's output. tiny-skia is
    /// deterministic IEEE f32 with no platform fast-math, so the hash should hold everywhere;
    /// if a platform or a tiny-skia bump ever disagrees (CI runs Linux/macOS/Windows), convert
    /// this to a handful of sampled pixels rather than deleting the lock.
    #[test]
    fn world_atlas_pixmap_is_byte_stable() {
        let mut m = sample_world();
        let view = View { width: 800.0, height: 500.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let r = render(&mut m, &view).expect("render");
        assert_eq!(r.pixmap.data().len(), 800 * 500 * 4);
        let h = fnv1a(r.pixmap.data());
        // Point markers are stamped from sprites at quarter-pixel offsets (the cities' anti-aliased
        // edges moved by up to an eighth of a pixel); nothing else changed this frame.
        assert_eq!(h, 0x89e2_f6ec_5589_a46e, "golden pixmap hash changed: {h:#x}");
    }

    fn square_layer(m: &mut Map, name: &str, x: f64, y: f64, s: f64) -> u32 {
        let mut d = Dataset::new(name, GeometryType::Polygon, vec![]);
        d.push(
            Some(Geometry::Polygon(vec![vec![vec![
                Coord::new(x, y),
                Coord::new(x + s, y),
                Coord::new(x + s, y + s),
                Coord::new(x, y + s),
            ]]])),
            vec![],
        );
        m.add_dataset(d)
    }

    fn cmd_kinds(scene: &gpu_scene::GpuScene) -> Vec<&'static str> {
        scene
            .cmds
            .iter()
            .map(|c| match c {
                gpu_scene::DrawCmd::Mesh { .. } => "mesh",
                gpu_scene::DrawCmd::Points { .. } => "points",
                gpu_scene::DrawCmd::Extrusion { .. } => "extrusion",
                gpu_scene::DrawCmd::Image { .. } => "image",
                gpu_scene::DrawCmd::Tiles { .. } => "tiles",
                gpu_scene::DrawCmd::LiveTiles { .. } => "live tiles",
                gpu_scene::DrawCmd::TileMeshes { .. } => "tile meshes",
            })
            .collect()
    }

    #[test]
    fn gpu_backend_builds_scene_for_flat_world() {
        let mut m = sample_world();
        if let Some(l) = m.layers.iter_mut().find(|l| l.name == "Cities") {
            l.labels.enabled = true;
        }
        let view = View { width: 800.0, height: 500.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        assert!(kinds.iter().filter(|k| **k == "mesh").count() >= 2, "countries/lakes/rivers as meshes: {kinds:?}");
        assert!(kinds.contains(&"points"), "cities as marker instances: {kinds:?}");
        // The label channel is identical to the CPU render's.
        let mut m2 = sample_world();
        if let Some(l) = m2.layers.iter_mut().find(|l| l.name == "Cities") {
            l.labels.enabled = true;
        }
        let cpu = render(&mut m2, &view).expect("cpu");
        assert_eq!(scene.labels.len(), cpu.labels.len());
        assert_eq!(scene.labels.first().map(|l| l.text.as_str()), cpu.labels.first().map(|l| l.text.as_str()));
    }

    #[test]
    fn gpu_backend_draws_a_layer_too_dense_to_mesh_on_the_cpu() {
        // 72,900 triangles about a pixel across: tessellated with outlines they'd pass
        // MAX_LAYER_VERTICES, so that layer alone is drawn on the CPU (splatted), never meshed.
        let mut m = Map::new("Map");
        square_layer(&mut m, "sparse", 0.0, -2.0, 1.0);
        let mut d = Dataset::new("dense", GeometryType::Polygon, vec![]);
        for i in 0..270 {
            for j in 0..270 {
                let (x, y) = (i as f64 * 0.01, j as f64 * 0.01);
                let ring = vec![Coord::new(x, y), Coord::new(x + 0.008, y), Coord::new(x, y + 0.008)];
                d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
            }
        }
        m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(1.35, 0.5)), res: 1000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        assert!(kinds.contains(&"image"), "the dense layer as a CPU picture: {kinds:?}");
        assert!(kinds.contains(&"mesh"), "the sparse layer stays a mesh: {kinds:?}");
        for c in &scene.cmds {
            if let gpu_scene::DrawCmd::Mesh { vertices, .. } = c {
                assert!(vertices.len() <= mesh::MAX_LAYER_VERTICES, "{} vertices", vertices.len());
            }
        }
    }

    #[test]
    fn gpu_backend_interleaves_cpu_image_for_blended_layer() {
        let mut m = Map::new("Map");
        // Layers draw bottom-up: the last one in the list first.
        square_layer(&mut m, "a", 0.0, 0.0, 10.0);
        let b = square_layer(&mut m, "b", 20.0, 0.0, 10.0);
        square_layer(&mut m, "c", 40.0, 0.0, 10.0);
        if let Some(l) = m.layer_mut(b) {
            l.blend = rustgis_map::effects::Blend::Multiply;
        }
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        let last_image = kinds.iter().rposition(|k| *k == "image").expect("the blended layer's cpu image");
        assert!(kinds[..last_image].contains(&"mesh"), "a mesh below the blended layer: {kinds:?}");
        assert!(kinds[last_image + 1..].contains(&"mesh"), "a mesh above the blended layer: {kinds:?}");
    }

    /// On the GPU too a tile layer draws at its place among the map's layers (#808).
    #[test]
    fn gpu_backend_draws_tile_layers_at_their_place() {
        let mut m = Map::new("Map");
        m.graticule = false;
        let below = square_layer(&mut m, "below", 0.0, 0.0, 10.0);
        square_layer(&mut m, "above", 20.0, 0.0, 10.0);
        let mut t = rustgis_map::TileLayer::new("tiles", rustgis_map::TileKind::Raster, "https://example.invalid/{z}/{x}/{y}.png");
        t.over = Some(below);
        m.tiles.push(t);
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::GpuLive) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        let tiles = kinds.iter().position(|k| *k == "live tiles").expect("the tile layer");
        assert!(kinds[..tiles].contains(&"mesh") && kinds[tiles + 1..].contains(&"mesh"), "between the two layers: {kinds:?}");
    }

    #[test]
    fn gpu_backend_skips_the_spurious_image_for_hidden_tile_layers() {
        let mut m = sample_world();
        m.tiles.push(rustgis_map::TileLayer::new("hidden", rustgis_map::TileKind::Raster, "https://example.invalid/{z}/{x}/{y}.png"));
        for t in &mut m.tiles {
            t.visible = false;
        }
        let view = View { width: 800.0, height: 500.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        assert!(!scene.cmds.iter().any(|c| matches!(c, gpu_scene::DrawCmd::Image { .. })), "hidden tiles must not emit an empty image");
    }

    #[test]
    fn gpu_backend_caps_image_commands_to_a_cpu_frame() {
        // More than four alternating CPU-only layers costs more full-screen uploads than a
        // plain CPU rasterization: the whole frame goes to the CPU backend.
        let build = |blends: usize| {
            let mut m = Map::new("Map");
            for i in 0..(blends * 2 + 1) {
                let id = square_layer(&mut m, &format!("l{i}"), i as f64 * 20.0, 0.0, 10.0);
                if i % 2 == 1
                    && let Some(l) = m.layer_mut(id)
                {
                    l.transparency = 50.0; // offscreen-composited → an image command each
                }
            }
            m
        };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let mut three = build(3);
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut three, &view, Backend::Gpu) else { panic!("3 blends stays gpu") };
        assert_eq!(scene.cmds.iter().filter(|c| matches!(c, gpu_scene::DrawCmd::Image { .. })).count(), 3);
        let mut five = build(5);
        assert!(
            matches!(render_backend(&mut five, &view, Backend::Gpu), Some(RenderOutput::Cpu(_))),
            "5 blends falls back to the cpu frame"
        );
    }

    #[test]
    fn gpu_backend_appends_selection_highlight() {
        let mut m = Map::new("Map");
        let id = square_layer(&mut m, "s", 0.0, 0.0, 10.0);
        if let Some(l) = m.layer_mut(id) {
            l.selection = vec![0];
        }
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        assert_eq!(kinds.last(), Some(&"mesh"), "the highlight mesh follows the layer: {kinds:?}");
        assert!(kinds.iter().filter(|k| **k == "mesh").count() >= 2, "layer mesh + highlight mesh: {kinds:?}");
    }

    #[test]
    fn zoomed_in_past_f32_large_features_are_drawn_on_the_cpu_where_they_are() {
        // A 10° square's west edge at the deepest zoom (#902): its vertices lie hundreds of km
        // from the mesh's origin, which f32 can't hold to a pixel at a tenth of a millimetre per
        // pixel, so the layer is drawn on the CPU, with the edge through the middle of the view.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        square_layer(&mut m, "s", 0.0, 40.0, 10.0);
        let mut camera = Camera::world();
        camera.center = rustgis_geom::lonlat_to_merc(Coord::new(0.0, 45.0));
        camera.res = rustgis_map::camera::MIN_RES;
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        assert!(!kinds.contains(&"mesh"), "no mesh f32 would misplace: {kinds:?}");
        let Some(pixmap) = scene.cmds.iter().find_map(|c| match c {
            gpu_scene::DrawCmd::Image { pixmap } => Some(pixmap),
            _ => None,
        }) else {
            panic!("the square is drawn on the CPU: {kinds:?}")
        };
        let alpha = |x: u32| pixmap.pixel(x, 150).map_or(0, |p| p.alpha());
        assert_eq!(alpha(195), 0, "west of the edge is outside the square");
        assert!(alpha(205) > 0, "east of the edge is inside it");

        // Zoomed out, the same square is a mesh, and the scene says how far in it holds.
        let mut camera = view.camera;
        camera.res = 1_000.0;
        let view = View { camera, ..view };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        assert!(cmd_kinds(&scene).contains(&"mesh"));
        let half = (rustgis_geom::lonlat_to_merc(Coord::new(10.0, 50.0)).y - rustgis_geom::lonlat_to_merc(Coord::new(0.0, 40.0)).y) / 2.0;
        assert!((gpu_scene::reach(&scene.cmds) - half).abs() < 1.0, "reach {} vs {half}", gpu_scene::reach(&scene.cmds));
    }

    #[test]
    fn gpu_scene_carries_backdrop_as_clear_color_and_no_image_for_pure_vectors() {
        let mut m = sample_world();
        let view = View { width: 800.0, height: 500.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        assert!(
            !scene.cmds.iter().any(|c| matches!(c, gpu_scene::DrawCmd::Image { .. })),
            "a pure-vector scene must carry no CPU image (it would poison the camera fast path): {} cmds",
            scene.cmds.len()
        );
        let (water, ..) = m.basemap.palette();
        assert_eq!(scene.clear, mesh::premul(water), "the clear colour is the basemap's water");
    }

    #[test]
    fn a_coarse_globe_drape_keeps_point_symbols_their_size() {
        // A drape over its size limit gets fewer pixels, not a coarser scale: a marker sized in
        // points covers as much ground as in a full-density drape, so it doesn't grow on the
        // globe while it moves.
        let mut m = Map::new("Map");
        m.projection = rustgis_map::Projection::Globe;
        let mut d = Dataset::new("pts", GeometryType::Point, vec![]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        let id = m.add_dataset(d);
        if let Some(l) = m.layer_mut(id) {
            l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol {
                fill: Rgba::rgb(0, 255, 0),
                stroke: Rgba::rgb(0, 255, 0),
                stroke_width: 0.0,
                size: 20.0,
                marker: rustgis_map::Marker::Circle,
            });
        }
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 20_000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: true };
        let boxes = [Extent::new(-60.0, -60.0, 60.0, 60.0)];
        // The marker's width in metres along the equator.
        let mut width = |max_side: u32| {
            let p = render_globe_drape(&mut m, &view, &boxes, max_side);
            let p = p.first().expect("a picture");
            let row = p.pixmap.height() / 2;
            let green = (0..p.pixmap.width()).filter(|&x| p.pixmap.pixel(x, row).is_some_and(|c| c.green() > 200 && c.red() < 100)).count();
            (green as f64 * p.merc.width() / f64::from(p.pixmap.width()), p.merc.width() / f64::from(p.pixmap.width()))
        };
        let (sharp, _) = width(4096);
        let (coarse, px) = width(128);
        assert!(px > 4.0 * 20_000.0, "the size limit should coarsen the picture ({px} m per pixel)");
        assert!(sharp > 0.0 && (coarse - sharp).abs() <= 2.0 * px, "a coarse drape drew the marker {coarse} m wide, not {sharp} m");
        assert_eq!(m.projection, rustgis_map::Projection::Globe);
    }

    #[test]
    fn gpu_backend_skips_non_finite_stroke_widths() {
        // A hostile symbol state (NaN stroke width) must not put NaN vertices on the GPU:
        // the line's stroke is skipped, the rest of the scene is unaffected.
        let mut m = Map::new("Map");
        let mut d = Dataset::new("l", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-50.0, 0.0), Coord::new(50.0, 0.0)]])), vec![]);
        let id = m.add_dataset(d);
        if let Some(l) = m.layer_mut(id) {
            l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol {
                fill: rustgis_map::Rgba::rgb(0, 0, 200),
                stroke: rustgis_map::Rgba::rgb(0, 0, 200),
                stroke_width: f32::NAN,
                size: 0.0,
                marker: rustgis_map::Marker::Circle,
            });
        }
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        for c in &scene.cmds {
            if let gpu_scene::DrawCmd::Mesh { vertices, .. } = c {
                assert!(vertices.iter().all(|v| v.position[0].is_finite() && v.position[1].is_finite() && v.stroke_px.is_finite()));
            }
        }
    }

    #[test]
    fn gpu_backend_handles_non_finite_geometry_without_panicking() {
        // A polygon with a NaN vertex (hostile file): the GPU drops the point (lyon's path
        // builder panics on NaN — never feed it), the healthy layer still tessellates, and
        // the frame completes like the CPU's.
        let build = || {
            let mut m = Map::new("Map");
            let mut d = Dataset::new("bad", GeometryType::Polygon, vec![]);
            d.push(
                Some(Geometry::Polygon(vec![vec![vec![
                    Coord::new(0.0, 0.0),
                    Coord::new(10.0, f64::NAN),
                    Coord::new(10.0, 10.0),
                    Coord::new(0.0, 10.0),
                ]]])),
                vec![],
            );
            m.add_dataset(d);
            square_layer(&mut m, "good", 30.0, 0.0, 10.0);
            m
        };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        let mut gm = build();
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut gm, &view, Backend::Gpu) else { panic!("gpu scene") };
        assert!(cmd_kinds(&scene).contains(&"mesh"), "the healthy layer tessellated");
        let mut cm = build();
        assert!(render(&mut cm, &view).is_some(), "cpu frame completes too");
    }

    #[test]
    fn gpu_backend_falls_back_for_globe_and_effects() {
        let mut m = sample_world();
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: Camera::world(), sky: false };
        m.projection = rustgis_map::Projection::Globe;
        assert!(matches!(render_backend(&mut m, &view, Backend::Gpu), Some(RenderOutput::Cpu(_))), "globe stays on the CPU renderer");
        m.projection = rustgis_map::Projection::WebMercator;
        m.view.effects.vignette = true;
        assert!(matches!(render_backend(&mut m, &view, Backend::Gpu), Some(RenderOutput::Cpu(_))), "view effects stay on the CPU renderer");
    }

    #[test]
    fn deep_zoom_on_the_coast_does_not_panic() {
        // Zoomed in to millimetres on a coastline, the land polygon's far vertices were billions
        // of pixels away, and the rasteriser indexed past its buffer.
        let mut m = Map::new("Map");
        for res in [0.05, 0.008, 0.001, 1e-5] {
            let camera = Camera { center: lonlat_to_merc(Coord::new(-122.2585, 37.8719)), res, bearing: 0.0, pitch: 0.0 };
            let view = View { width: 1600.0, height: 1000.0, pixels_per_point: 1.0, camera, sky: false };
            assert!(render(&mut m, &view).is_some(), "{res}");
        }
    }

    #[test]
    fn clips_rings_and_lines_to_a_box() {
        let b = Extent { min_x: 0.0, min_y: 0.0, max_x: 10.0, max_y: 10.0 };
        // A square far bigger than the box clips to the box.
        let big = [(-1e9, -1e9), (1e9, -1e9), (1e9, 1e9), (-1e9, 1e9)];
        let c = clip_ring(&big, &b);
        assert_eq!(c.len(), 4);
        assert!(c.iter().all(|p| (p.0 == 0.0 || p.0 == 10.0) && (p.1 == 0.0 || p.1 == 10.0)), "{c:?}");
        // One wholly outside clips away; one inside is kept as it is.
        assert!(clip_ring(&[(20.0, 20.0), (30.0, 20.0), (30.0, 30.0)], &b).is_empty());
        let inside = [(1.0, 1.0), (5.0, 1.0), (5.0, 5.0)];
        assert_eq!(clip_ring(&inside, &b), inside.to_vec());
        // A line in and out of the box comes back as its two parts inside.
        let parts = clip_line(&[(-5.0, 2.0), (5.0, 2.0), (5.0, 20.0), (8.0, 20.0), (8.0, 5.0), (15.0, 5.0)], &b);
        assert_eq!(parts, vec![(5.0, vec![(0.0, 2.0), (5.0, 2.0), (5.0, 10.0)]), (41.0, vec![(8.0, 10.0), (8.0, 5.0), (10.0, 5.0)])]);
        assert!(clip_line(&[(-5.0, -5.0), (-1.0, 20.0)], &b).is_empty());
    }

    #[test]
    fn stamped_markers_match_drawn_ones() {
        let view = View {
            width: 120.0,
            height: 80.0,
            pixels_per_point: 1.0,
            camera: Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let ctx = Ctx {
            view: &view,
            ppp: 1.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        for marker in Marker::ALL {
            let s = Symbol { fill: Rgba::new(200, 60, 30, 200), stroke: Rgba::rgb(20, 20, 20), stroke_width: 1.2, size: 9.0, marker };
            let (mut stamped, mut drawn) = (Pixmap::new(120, 80).unwrap(), Pixmap::new(120, 80).unwrap());
            // Points on and off the quarter-pixel grid, and over the image's edge.
            for c in
                [Coord::new(-30.0, 10.0), Coord::new(5.37, -12.81), Coord::new(25.25, 3.5), Coord::new(-61.0, 0.0), Coord::new(40.0, 41.0)]
            {
                crate::canvas::with_canvas(&mut stamped, |cv| ctx.stamp(cv, c, &s));
                crate::canvas::with_canvas(&mut stamped, |cv| ctx.stamp(cv, c, &s)); // the second comes from the cache
                crate::canvas::with_canvas(&mut drawn, |cv| ctx.marker(cv, c, &s, 1.0, None));
                crate::canvas::with_canvas(&mut drawn, |cv| ctx.marker(cv, c, &s, 1.0, None));
            }
            let worst = stamped.data().iter().zip(drawn.data()).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(worst < 120, "{marker:?}: {worst}");
            let total: u64 = stamped.data().iter().zip(drawn.data()).map(|(a, b)| a.abs_diff(*b) as u64).sum();
            let ink: u64 = drawn.data().iter().map(|v| *v as u64).sum();
            assert!(total * 20 < ink, "{marker:?}: {total} of {ink}");
        }
        assert!(ctx.sprites.borrow().len() <= 5 * 16);
    }

    #[test]
    fn a_turned_view_turns_the_map_and_keeps_labels_upright() {
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        // 80 px east of the centre.
        d.push(Some(Geometry::Point(Coord::new(80.0 * 1000.0 / 111_319.49, 0.0))), vec![Value::Text("East".into())]);
        let id = m.add_dataset(d);
        if let Some(l) = m.layer_mut(id) {
            l.labels.enabled = true;
        }
        // Where the point is drawn: the middle of the pixels unlike the background.
        let ink = |pm: &Pixmap| {
            let bg = pm.pixel(0, 0).unwrap();
            let (mut n, mut sx, mut sy) = (0.0, 0.0, 0.0);
            for (k, p) in pm.pixels().iter().enumerate() {
                if *p != bg {
                    n += 1.0;
                    sx += (k as u32 % pm.width()) as f64;
                    sy += (k as u32 / pm.width()) as f64;
                }
            }
            assert!(n > 0.0, "nothing drawn");
            (sx / n, sy / n)
        };
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 1000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 2.0, camera, sky: false };
        let north = render(&mut m, &view).unwrap();
        let (x, y) = ink(&north.pixmap);
        assert!((x - 560.0).abs() < 4.0 && (y - 300.0).abs() < 4.0, "north up: {x},{y}");
        // Facing east, east is up.
        let turned = View { camera: Camera { bearing: 90.0, ..camera }, ..view };
        let r = render(&mut m, &turned).unwrap();
        assert_eq!((r.pixmap.width(), r.pixmap.height()), (800, 600));
        let (x, y) = ink(&r.pixmap);
        assert!((x - 400.0).abs() < 4.0 && (y - 140.0).abs() < 4.0, "facing east: {x},{y}");
        // The label sits by the point in the turned view (its text is drawn upright over it).
        let l = r.labels.iter().find(|l| l.text == "East").expect("the label is placed");
        assert!((l.center_x() - 200.0).abs() < 40.0 && (l.y - 70.0).abs() < 20.0, "label at {},{}", l.x, l.y);
        // Any bearing covers the whole view: no corner is left empty.
        let r = render(&mut m, &View { camera: Camera { bearing: -33.0, ..camera }, ..view }).unwrap();
        assert!(r.pixmap.pixels().iter().all(|p| p.alpha() == 255));
        // A bearing set straight on the camera is normalised: no number faces north, 450 is 90.
        let nan = render(&mut m, &View { camera: Camera { bearing: f64::NAN, ..camera }, ..view }).unwrap();
        assert_eq!(nan.pixmap.data(), north.pixmap.data());
        let (x, y) = ink(&render(&mut m, &View { camera: Camera { bearing: 450.0, ..camera }, ..view }).unwrap().pixmap);
        assert!((x - 400.0).abs() < 4.0 && (y - 140.0).abs() < 4.0, "450°: {x},{y}");
    }

    /// A hill of a DEM 3000 m high in the middle of a 2° square around (0, 0).
    pub(super) fn hill_map() -> Map {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let n = 64usize;
        let heights: Vec<f32> = (0..n * n)
            .map(|i| {
                let (x, y) = ((i % n) as f32 / n as f32 - 0.5, (i / n) as f32 / n as f32 - 0.5);
                3000.0 * (-(x * x + y * y) * 30.0).exp()
            })
            .collect();
        let gt = GeoTransform { origin_x: -1.0, origin_y: 1.0, pixel_width: 2.0 / n as f64, pixel_height: -2.0 / n as f64 };
        let dem = Raster::new("dem", n as u32, n as u32, vec![heights], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let id = m.add_raster("dem", rustgis_map::RasterLayer::from_path(dem, "dem.tif"));
        if let Some(l) = m.layer_mut(id) {
            l.visible = false;
        }
        let mut d = Dataset::new("peak", GeometryType::Point, vec![]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        m.add_dataset(d);
        m
    }

    #[test]
    fn a_raster_at_the_closest_zoom_is_drawn_where_it_is() {
        // At the deepest zoom (#902) the raster's far corners lie billions of pixels from the
        // view: its west edge still runs through the middle, with the raster east of it.
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let n = 16usize;
        let values: Vec<f32> = (0..n * n).map(|i| (i % n) as f32).collect();
        let gt = GeoTransform { origin_x: -1.0, origin_y: 1.0, pixel_width: 2.0 / n as f64, pixel_height: -2.0 / n as f64 };
        let r = Raster::new("r", n as u32, n as u32, vec![values], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        m.add_raster("r", rustgis_map::RasterLayer::from_path(r, "r.tif"));
        let camera = Camera { center: lonlat_to_merc(Coord::new(-1.0, 0.0)), res: rustgis_map::camera::MIN_RES, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let Some(out) = render(&mut m, &view) else { panic!("cpu frame") };
        let px = |x: u32| out.pixmap.pixel(x, 150).map(|p| (p.red(), p.green(), p.blue(), p.alpha()));
        let (west, east) = (px(195), px(205));
        assert!(west.is_some() && east.is_some());
        assert_ne!(west, east, "the raster starts at the middle of the view");
        assert_eq!(px(5), west, "west of the edge is all background");
        assert_eq!(px(395), east, "east of it, one raster cell");
    }

    #[test]
    fn a_tilted_view_of_characters_standing_up_stays_on_the_cpu() {
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, -0.6)), res: 600.0, bearing: 0.0, pitch: 65.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let mut map = hill_map();
        assert!(gpu_may_turn(&map, &view));
        for l in &mut map.layers {
            l.renderer.map_symbols(|s| s.marker = Marker::Char(rustgis_map::Glyph::new('L', "")));
        }
        // The GPU has no template for a character: standing up, it would be a circle.
        assert!(!gpu_may_turn(&map, &view));
        // Turned, not tilted, the map is drawn flat, where the layer is a CPU picture.
        let turned = View { camera: Camera { bearing: 30.0, pitch: 0.0, ..camera }, ..view };
        assert!(gpu_may_turn(&map, &turned));
    }

    #[test]
    fn a_tilted_view_drapes_the_map_over_its_terrain() {
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, -0.6)), res: 600.0, bearing: 0.0, pitch: 65.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let mut flat = hill_map();
        let mut hilly = hill_map();
        hilly.view.terrain =
            Some(rustgis_map::view::Terrain { source: rustgis_map::view::TerrainSource::Layer("dem".into()), exaggeration: 2.0 });
        // The ground's height is read from the DEM (exaggerated), and nothing off it.
        let g = terrain::Ground::of(&hilly, &camera).expect("terrain");
        let top = g.height(lonlat_to_merc(Coord::new(0.0, 0.0))).unwrap();
        assert!((top - 6000.0).abs() < 400.0, "{top}");
        assert!(g.height(lonlat_to_merc(Coord::new(5.0, 5.0))).is_none());
        assert!(terrain::Ground::of(&flat, &camera).is_none());
        // The marker on the peak stands on it: higher up the view than on flat ground.
        let place = |m: &mut Map| {
            let up = upright_markers(m, &view, 0.0);
            let k = up.first().expect("the peak's marker");
            k.place(&camera, view.width, view.height).expect("in view").1
        };
        let (y_flat, y_hill) = (place(&mut flat), place(&mut hilly));
        // The GPU draws the ground flat: a view over terrain stays on the CPU.
        assert!(gpu_may_turn(&flat, &view) && !gpu_may_turn(&hilly, &view));
        assert!(y_hill < y_flat - 5.0, "{y_hill} vs {y_flat}");
        // The frame shows the relief: it differs from the flat one, and the sky still shows.
        let a = render(&mut flat, &view).unwrap();
        let b = render(&mut hilly, &view).unwrap();
        assert_ne!(a.pixmap.data(), b.pixmap.data());
        assert!(b.pixmap.pixels().iter().all(|p| p.alpha() == 255));
        // A DEM layer gone (renamed or removed) leaves the ground flat.
        hilly.view.terrain =
            Some(rustgis_map::view::Terrain { source: rustgis_map::view::TerrainSource::Layer("none".into()), exaggeration: 1.0 });
        assert!(terrain::Ground::of(&hilly, &camera).is_none());
        assert!(render(&mut hilly, &view).is_some());
    }

    #[test]
    fn a_tilted_view_draws_the_ground_in_perspective_under_a_sky() {
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        // 100 px north of the centre at its scale.
        d.push(Some(Geometry::Point(Coord::new(0.0, 100.0 * 1000.0 / 111_319.49))), vec![Value::Text("Ahead".into())]);
        let id = m.add_dataset(d);
        if let Some(l) = m.layer_mut(id) {
            l.labels.enabled = true;
        }
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 1000.0, bearing: 0.0, pitch: 60.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 2.0, camera, sky: false };
        let r = render(&mut m, &view).unwrap();
        assert_eq!((r.pixmap.width(), r.pixmap.height()), (800, 600));
        // The point is drawn where the camera puts it: ahead, foreshortened toward the centre.
        let (_, want_y) = camera.to_screen(lonlat_to_merc(Coord::new(0.0, 100.0 * 1000.0 / 111_319.49)), 400.0, 300.0);
        assert!(want_y < 150.0 - 20.0 && want_y > 150.0 - 100.0, "{want_y}");
        let bg = r.pixmap.pixel(400, 590).unwrap();
        let (mut n, mut sy) = (0.0, 0.0);
        for (k, p) in r.pixmap.pixels().iter().enumerate() {
            let (x, y) = (k as u32 % 800, k as u32 / 800);
            if (390..410).contains(&x) && y > 150 && *p != bg {
                n += 1.0;
                sy += f64::from(y);
            }
        }
        assert!(n > 0.0 && (sy / n / 2.0 - want_y).abs() < 4.0, "drawn at {}, want {want_y}", sy / n / 2.0);
        // Its label is placed there, upright.
        let l = r.labels.iter().find(|l| l.text == "Ahead").expect("placed");
        assert!((f64::from(l.y) - want_y).abs() < 20.0, "{}", l.y);
        // Steeper, the sky shows at the top in the sky colour, and the ground at the bottom.
        let steep = View { camera: Camera { pitch: 80.0, ..camera }, ..view };
        let r = render(&mut m, &steep).unwrap();
        let (sky, _) = tilt_sky(&m);
        let top = r.pixmap.pixel(400, 0).unwrap();
        assert!((i32::from(top.red()) - i32::from(sky.r())).abs() <= 2 && (i32::from(top.blue()) - i32::from(sky.b())).abs() <= 2);
        assert!(r.pixmap.pixels().iter().all(|p| p.alpha() == 255));
        // A pitch past the limit or no number is kept to the limit or looks straight down.
        assert!(render(&mut m, &View { camera: Camera { pitch: 400.0, ..camera }, ..view }).is_some());
        let flat = render(&mut m, &View { camera: Camera { pitch: 0.0, ..camera }, ..view }).unwrap();
        let nan = render(&mut m, &View { camera: Camera { pitch: f64::NAN, ..camera }, ..view }).unwrap();
        assert_eq!(nan.pixmap.data(), flat.pixmap.data());
    }

    #[test]
    fn point_markers_stand_upright_in_a_tilted_view() {
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("pts", GeometryType::Point, vec![]);
        // At the centre, and 150 px ahead of it at its scale.
        let ahead = 150.0 * 1000.0 / 111_319.49;
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        d.push(Some(Geometry::Point(Coord::new(0.0, ahead))), vec![]);
        let id = m.add_dataset(d);
        let red = Rgba::rgb(255, 0, 0);
        if let Some(l) = m.layer_mut(id) {
            l.renderer =
                rustgis_map::Renderer::Single(Symbol { fill: red, stroke: red, stroke_width: 0.0, size: 20.0, marker: Marker::Circle });
            l.selection = vec![1];
        }
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 1000.0, bearing: 30.0, pitch: 70.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let r = render(&mut m, &view).unwrap();
        // The red pixels' box in the rows `ys` (logical px).
        let bbox = |ys: std::ops::Range<u32>| {
            let (mut x0, mut x1, mut y0, mut y1) = (u32::MAX, 0, u32::MAX, 0);
            for (k, p) in r.pixmap.pixels().iter().enumerate() {
                let (x, y) = (k as u32 % 400, k as u32 / 400);
                if ys.contains(&y) && p.red() > 200 && p.green() < 60 && p.blue() < 60 {
                    (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
                }
            }
            (x1.saturating_sub(x0) + 1, y1.saturating_sub(y0) + 1)
        };
        // The marker at the centre faces the viewer: as tall as it is wide, its symbol's size,
        // where on the ground it would be squashed to a third as tall.
        let (w, h) = bbox(135..166);
        assert!((18..=22).contains(&w) && w.abs_diff(h) <= 1, "{w} × {h}");
        // The one ahead is farther: smaller, still round, and selected (the selection colour
        // rings it).
        let (_, y) = camera.to_screen(lonlat_to_merc(Coord::new(0.0, ahead)), 400.0, 300.0);
        let y = y.round() as u32;
        let (fw, fh) = bbox(y.saturating_sub(12)..y + 12);
        assert!(fw < w && fw > 4 && fw.abs_diff(fh) <= 1, "{fw} × {fh}");
        let ring = r.pixmap.pixels().iter().any(|p| p.red() < 40 && p.green() > 200 && p.blue() > 230);
        assert!(ring, "the selected marker is ringed");
        // Both stand in the drape's list, far first.
        let up = upright_markers(&mut m, &view, 0.3);
        assert_eq!(up.len(), 2);
        assert!(up.first().is_some_and(|u| u.selected) && up.get(1).is_some_and(|u| !u.selected));
        // With a reference scale twice the view's, they stand twice the size.
        let size = up.first().map(|u| u.symbol.size).unwrap_or_default();
        let scale = m.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M;
        m.layer_mut(id).unwrap().reference_scale = Some(scale * 2.0);
        let up = upright_markers(&mut m, &view, 0.3);
        assert!(up.iter().all(|u| (u.symbol.size - size * 2.0).abs() < 1e-3), "{size}");
        m.layer_mut(id).unwrap().reference_scale = None;
        // A point layer seen flat draws as before, and an aggregation stays on the ground.
        if let Some(l) = m.layer_mut(id) {
            l.aggregation = Some(rustgis_map::effects::Aggregation { hexagons: true, size: 20.0 });
        }
        assert!(upright_markers(&mut m, &view, 0.3).is_empty());
    }

    #[test]
    fn extrusions_stand_up_in_a_tilted_view() {
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("blocks", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
        // A square about 200 km a side at the centre.
        let s = 0.9;
        let ring = vec![Coord::new(-s, -s), Coord::new(s, -s), Coord::new(s, s), Coord::new(-s, s), Coord::new(-s, -s)];
        d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![Value::Text("Block".into())]);
        let id = m.add_dataset(d);
        if let Some(l) = m.layer_mut(id) {
            l.labels.enabled = true;
            l.extrusion = Some(rustgis_map::effects::Extrusion {
                field: None,
                height: 100_000.0,
                factor: 1.0,
                face_culling: true,
                lighting: true,
                shadows: true,
            });
        }
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 2000.0, bearing: 30.0, pitch: 60.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        // The square on the ground spans these rows; its roof stands well above them.
        let (_, ground_top) = camera.to_screen(lonlat_to_merc(Coord::new(0.0, s)), 400.0, 300.0);
        let r = render(&mut m, &view).unwrap();
        let bg = r.pixmap.pixel(5, 295).unwrap();
        let top_ink = (0..300u32).find(|&y| (150..250).any(|x| r.pixmap.pixel(x, y).is_some_and(|p| p != bg))).unwrap();
        assert!(f64::from(top_ink) < ground_top - 15.0, "roof from row {top_ink}, ground from {ground_top}");
        // Its label is lifted onto the roof, above the ground under it.
        let l = r.labels.iter().find(|l| l.text == "Block").expect("placed");
        assert!(f64::from(l.y) < 150.0 - 15.0, "label at {}", l.y);
        // The moving view's tessellation holds the square, roof and walls, at its height.
        let shapes = extruded_shapes(&mut m, &view);
        assert_eq!(shapes.len(), 1);
        assert!(shapes[0].roof_indices.len() >= 6 && shapes[0].rings.len() == 1 && shapes[0].metres == 100_000.0);
        // Heights follow each feature's own scale, not the view centre's: at the equator a metre
        // is a Web Mercator unit, at 60°N two (#433 review; a view centred near a pole stood
        // features elsewhere up several times too tall).
        assert!((shapes[0].height / 100_000.0 - 1.0).abs() < 0.01, "{}", shapes[0].height);
        // A long block reaching from behind the eye into view stands up where it is in front (it
        // was dropped whole when its middle or a corner was out of reach).
        let long =
            vec![Coord::new(-0.5, -40.0), Coord::new(0.5, -40.0), Coord::new(0.5, 0.5), Coord::new(-0.5, 0.5), Coord::new(-0.5, -40.0)];
        assert!(camera.try_to_screen(lonlat_to_merc(Coord::new(0.0, -40.0)), 400.0, 300.0).is_none(), "behind the eye");
        let mut flat_map = m.clone();
        if let Some(l) = m.layers.first_mut() {
            l.dataset.features.clear();
            l.dataset.push(Some(Geometry::Polygon(vec![vec![long.clone()]])), vec![Value::Text("Long".into())]);
            l.touch_geometry();
        }
        if let Some(l) = flat_map.layers.first_mut() {
            l.dataset.features.clear();
            l.dataset.push(Some(Geometry::Polygon(vec![vec![long]])), vec![Value::Text("Long".into())]);
            l.touch_geometry();
            l.extrusion = None;
        }
        let up = render(&mut m, &view).unwrap();
        let down = render(&mut flat_map, &view).unwrap();
        let changed = up.pixmap.pixels().iter().zip(down.pixmap.pixels()).filter(|(a, b)| a != b).count();
        assert!(changed > 2000, "the long block stands up: {changed} px");
        if let Some(l) = m.layers.first_mut() {
            let north =
                vec![Coord::new(-1.0, 59.0), Coord::new(1.0, 59.0), Coord::new(1.0, 61.0), Coord::new(-1.0, 61.0), Coord::new(-1.0, 59.0)];
            l.dataset.features.clear();
            l.dataset.push(Some(Geometry::Polygon(vec![vec![north]])), vec![Value::Text("North".into())]);
            l.touch_geometry();
        }
        // Seen from a camera at another latitude, the block's height is still its own latitude's.
        // (A camera so far that the block is off its ground leaves it out: see
        // an_extruded_ring_with_a_hole_is_open_over_it.)
        let south = View { camera: Camera { center: lonlat_to_merc(Coord::new(0.0, 57.0)), ..camera }, ..view };
        let shapes = extruded_shapes(&mut m, &south);
        assert_eq!(shapes.len(), 1);
        assert!((shapes[0].height / 200_000.0 - 1.0).abs() < 0.05, "{}", shapes[0].height);
    }

    /// A label's key names its feature by id (#1046): an extruded feature's label rises by its own
    /// height, also once its row no longer follows its id.
    #[test]
    fn labels_rise_by_their_own_features_height() {
        let mut m = Map::new("Map");
        let mut d = Dataset::new("blocks", GeometryType::Polygon, vec![Field::new("h", FieldType::Double)]);
        let s = 0.1;
        let ring = vec![Coord::new(-s, -s), Coord::new(s, -s), Coord::new(s, s), Coord::new(-s, s), Coord::new(-s, -s)];
        for h in [0.0, 100_000.0, 0.0] {
            d.push(Some(Geometry::Polygon(vec![vec![ring.clone()]])), vec![Value::Float(h)]);
        }
        // The first one deleted: the tall one (id 2) is row 0 and the low one (id 3) row 1.
        d.features.remove(0);
        let id = m.add_dataset(d);
        m.layer_mut(id).unwrap().extrusion = Some(rustgis_map::effects::Extrusion {
            field: Some("h".into()),
            height: 0.0,
            factor: 1.0,
            face_culling: true,
            lighting: true,
            shadows: true,
        });
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 2000.0, bearing: 0.0, pitch: 60.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let at_centre = |text: &str| PlacedLabel { y: 150.0, ..label(text, 200.0, None, 0.0) };
        // The tall one with no row, looked up by id; the low one offered from row 0 before the
        // tall one took it there: the row is a hint the id overrules.
        let low = Candidate { row: 0, ..(at_centre("Low"), 1.0, (id, 3, 0, 0)).into() };
        let mut c: Vec<Candidate> = vec![(at_centre("Tall"), 1.0, (id, 2, 0, 0)).into(), low];
        raise_labels(&m, &view, &mut c);
        assert!(c[0].label.y < 150.0 - 15.0, "the tall one rises onto its roof: {}", c[0].label.y);
        assert!((c[1].label.y - 150.0).abs() < 1.0, "the low one stays on the ground: {}", c[1].label.y);
    }

    /// Extruded features with junk heights or roof indices (hostile data) give finite geometry
    /// or are left out, never garbage.
    #[test]
    fn gpu_extrusions_survive_junk_heights() {
        let ring = vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(10.0, 10.0), Coord::new(0.0, 10.0)];
        let shape = |height: f64, roof_indices: Vec<u32>| ExtrudedShape {
            origin: Coord::new(0.0, 0.0),
            center: Coord::new(5.0, 5.0),
            roof: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            roof_indices,
            rings: vec![ring.clone()],
            holes: vec![],
            metres: height,
            height,
            fill: Rgba::rgb(200, 100, 50),
            stroke: Rgba::rgb(0, 0, 0),
            face_culling: true,
            lighting: true,
            shadows: true,
        };
        let shapes = [shape(f64::NAN, vec![0, 1, 2, 0, 2, 3]), shape(f64::INFINITY, vec![0, 1, 2]), shape(5.0, vec![0, 1, 99])];
        for cmd in extrusion_cmds(&shapes, [0.0, 0.0]).expect("within the budget") {
            let gpu_scene::DrawCmd::Extrusion { vertices, indices, .. } = cmd else { panic!("an extrusion") };
            assert!(vertices.iter().all(|v| v.position.iter().chain(&v.info).all(|x| x.is_finite())));
            assert!(indices.iter().all(|i| (*i as usize) < vertices.len()));
        }
    }

    #[test]
    fn a_globe_turns_and_tilts() {
        let mut m = Map::new("Map");
        m.projection = rustgis_map::Projection::Globe;
        let camera = Camera { center: lonlat_to_merc(Coord::new(10.0, 40.0)), res: 20_000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 300.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let plain = render(&mut m, &view).unwrap();
        let turned = render(&mut m, &View { camera: Camera { bearing: 40.0, ..camera }, ..view }).unwrap();
        let tilted = render(&mut m, &View { camera: Camera { pitch: 60.0, ..camera }, ..view }).unwrap();
        assert_ne!(plain.pixmap.data(), turned.pixmap.data());
        assert_ne!(plain.pixmap.data(), tilted.pixmap.data());
    }

    #[test]
    fn an_extruded_ring_with_a_hole_is_open_over_it() {
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("yard", GeometryType::Polygon, vec![]);
        let outer =
            vec![Coord::new(-2.0, -2.0), Coord::new(2.0, -2.0), Coord::new(2.0, 2.0), Coord::new(-2.0, 2.0), Coord::new(-2.0, -2.0)];
        let hole = vec![Coord::new(-1.2, -1.2), Coord::new(-1.2, 1.2), Coord::new(1.2, 1.2), Coord::new(1.2, -1.2), Coord::new(-1.2, -1.2)];
        d.push(Some(Geometry::Polygon(vec![vec![outer, hole]])), vec![]);
        let id = m.add_dataset(d);
        if let Some(l) = m.layer_mut(id) {
            l.extrusion = Some(rustgis_map::effects::Extrusion {
                field: None,
                height: 20_000.0,
                factor: 1.0,
                face_culling: true,
                lighting: true,
                shadows: false,
            });
        }
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 2_000.0, bearing: 0.0, pitch: 30.0 };
        let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        let r = render(&mut m, &view).unwrap();
        // The middle of the courtyard shows the ground, not a roof over it.
        assert_eq!(r.pixmap.pixel(200, 150), r.pixmap.pixel(5, 295));
        // Its walls stand inside the roof's outline.
        let shapes = extruded_shapes(&mut m, &view);
        assert_eq!((shapes[0].rings.len(), shapes[0].holes.len()), (1, 1));
        // Off the ground the view shows, a feature isn't tessellated at all.
        let far = View { camera: Camera { center: Coord::new(5e6, 5e6), ..camera }, ..view };
        assert!(extruded_shapes(&mut m, &far).is_empty());
        // In a hidden group, the layer stands up nowhere.
        let mut g = rustgis_map::LayerGroup::new("hidden");
        g.visible = false;
        g.children = vec![id];
        m.groups.push(g);
        assert!(extruded_shapes(&mut m, &view).is_empty());
        let hidden = render(&mut m, &view).unwrap();
        assert_eq!(hidden.pixmap.pixel(200, 150), hidden.pixmap.pixel(5, 295));
    }

    #[test]
    fn only_features_still_under_a_pixel_with_their_outline_are_splatted() {
        let view = View {
            width: 100.0,
            height: 100.0,
            pixels_per_point: 2.0,
            camera: Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let ctx = Ctx {
            view: &view,
            ppp: 2.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let ring = vec![Coord::new(0.0, 0.0), Coord::new(0.25, 0.0), Coord::new(0.25, 0.25), Coord::new(0.0, 0.25), Coord::new(0.0, 0.0)];
        let g = Geometry::Polygon(vec![vec![ring]]);
        let mut pm = Pixmap::new(200, 200).unwrap();
        let thin = Symbol { fill: Rgba::rgb(200, 0, 0), stroke: Rgba::rgb(0, 0, 0), stroke_width: 0.5, size: 0.0, marker: Marker::Circle };
        assert!(crate::canvas::with_canvas(&mut pm, |c| ctx.splat(c, &g.extent(), &g, &thin)), "0.5 px across with a 1 px outline");
        // A 3 pt outline at 2× is 6 px wide: drawn as a ring, not a dot.
        let thick = Symbol { stroke_width: 3.0, ..thin };
        assert!(!crate::canvas::with_canvas(&mut pm, |c| ctx.splat(c, &g.extent(), &g, &thick)));
    }

    #[test]
    fn small_polygons_preserve_rectangular_coverage_and_refuse_complex_shapes() {
        let view = View {
            width: 40.0,
            height: 40.0,
            pixels_per_point: 1.0,
            camera: Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let ctx = Ctx {
            view: &view,
            ppp: 1.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let polygon = |x: f64, y: f64, width: f64, height: f64| {
            Geometry::Polygon(vec![vec![vec![
                Coord::new(x, y),
                Coord::new(x + width, y),
                Coord::new(x + width, y + height),
                Coord::new(x, y + height),
                Coord::new(x, y),
            ]]])
        };
        for (width_px, height_px) in [(1.1, 1.1), (1.5, 1.5), (2.0, 2.0), (0.4, 1.9), (1.9, 0.4)] {
            for offset in [0.0, 0.13, 0.49, 0.87] {
                for (fill, stroke, width) in
                    [(Rgba::rgb(200, 120, 0), Rgba::rgb(30, 30, 30), 1.4), (Rgba::rgb(200, 120, 0), Rgba::rgb(30, 30, 30), 2.0)]
                {
                    let g = polygon(offset, offset, width_px, height_px);
                    let sym = Symbol { fill, stroke, stroke_width: width, size: 0.0, marker: Marker::Circle };
                    for white in [false, true] {
                        let mut actual = Pixmap::new(40, 40).unwrap();
                        if white {
                            actual.fill(tiny_skia::Color::WHITE);
                        }
                        let mut expected = actual.clone();
                        crate::canvas::with_canvas(&mut expected, |c| ctx.feature(c, &g, &sym));
                        assert!(crate::canvas::with_canvas(&mut actual, |c| ctx.small_polygon(c, &g.extent(), &g, &sym)));
                        let max = actual.data().iter().zip(expected.data()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                        assert!(
                            max <= 8,
                            "size {width_px}x{height_px}, offset {offset}, width {width}, white {white}: largest difference {max}"
                        );
                    }
                }
            }
        }
        let sym =
            Symbol { fill: Rgba::rgb(200, 120, 0), stroke: Rgba::rgb(30, 30, 30), stroke_width: 1.4, size: 0.0, marker: Marker::Circle };
        let mut pm = Pixmap::new(40, 40).unwrap();
        let square = polygon(0.0, 0.0, 1.5, 1.5);
        let hole = Geometry::Polygon(vec![vec![
            vec![Coord::new(0.0, 0.0), Coord::new(1.5, 0.0), Coord::new(1.5, 1.5), Coord::new(0.0, 1.5), Coord::new(0.0, 0.0)],
            vec![Coord::new(0.4, 0.4), Coord::new(1.1, 0.4), Coord::new(1.1, 1.1), Coord::new(0.4, 1.1), Coord::new(0.4, 0.4)],
        ]]);
        let triangle =
            Geometry::Polygon(vec![vec![vec![Coord::new(0.0, 0.0), Coord::new(1.5, 0.0), Coord::new(0.0, 1.5), Coord::new(0.0, 0.0)]]]);
        let multi = Geometry::Polygon(vec![
            vec![vec![
                Coord::new(0.0, 0.0),
                Coord::new(1.5, 0.0),
                Coord::new(1.5, 1.5),
                Coord::new(0.0, 1.5),
                Coord::new(0.0, 0.0),
            ]];
            2
        ]);
        let line = Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.5, 1.5)]]);
        let inset = Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(0.8, 0.0),
            Coord::new(0.8, 0.05),
            Coord::new(1.2, 0.05),
            Coord::new(1.2, 0.0),
            Coord::new(1.75, 0.0),
            Coord::new(1.75, 1.75),
            Coord::new(0.0, 1.75),
            Coord::new(0.0, 0.0),
        ]]]);
        let deep_inset = Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(0.4, 0.0),
            Coord::new(0.4, 0.45),
            Coord::new(1.1, 0.45),
            Coord::new(1.1, 0.0),
            Coord::new(1.5, 0.0),
            Coord::new(1.5, 1.5),
            Coord::new(0.0, 1.5),
            Coord::new(0.0, 0.0),
        ]]]);
        let chamfer = Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(0.8, 0.0),
            Coord::new(1.5, 0.7),
            Coord::new(1.5, 1.5),
            Coord::new(0.0, 1.5),
            Coord::new(0.0, 0.0),
        ]]]);
        for g in [hole, triangle, multi, line, inset, deep_inset, chamfer, polygon(0.0, 0.0, 2.01, 2.01), polygon(0.0, 0.0, 0.2, 1.5)] {
            assert!(!crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(c, &g.extent(), &g, &sym)));
        }
        assert!(!crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(
            c,
            &square.extent(),
            &square,
            &Symbol { stroke_width: 2.1, ..sym }
        )));
        assert!(
            !crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(c, &square.extent(), &square, &Symbol { stroke_width: 0.0, ..sym })),
            "unoutlined fills keep their paths"
        );
        assert!(!crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(
            c,
            &square.extent(),
            &square,
            &Symbol { stroke: Rgba::new(0, 0, 0, 0), ..sym }
        )));
        assert!(!crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(
            c,
            &square.extent(),
            &square,
            &Symbol { fill: Rgba::new(200, 120, 0, 120), ..sym }
        )));
        assert!(!crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(
            c,
            &square.extent(),
            &square,
            &Symbol { stroke: Rgba::new(30, 30, 30, 170), ..sym }
        )));
        assert!(
            !crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(c, &square.extent(), &square, &Symbol { stroke_width: 0.7, ..sym })),
            "hairlines keep their rasterizer"
        );
        assert!(
            !crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(c, &square.extent(), &square, &Symbol { stroke_width: 1.0, ..sym })),
            "one pixel is still a hairline"
        );
        let smaller = polygon(0.0, 0.0, 1.1, 1.1);
        let original_extent = Extent::new(-0.02, -0.02, 1.1, 1.1);
        assert!(
            !crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(c, &original_extent, &smaller, &sym)),
            "a thinned ring must not fill its old extent"
        );
        assert!(!crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(
            c,
            &square.extent(),
            &square,
            &Symbol { fill: Rgba::new(0, 0, 0, 0), ..sym }
        )));
        let turned = View { camera: Camera { bearing: 15.0, ..view.camera }, ..view };
        let turned_ctx = Ctx {
            view: &turned,
            ppp: 1.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        assert!(!crate::canvas::with_canvas(&mut pm, |c| turned_ctx.small_polygon(c, &square.extent(), &square, &sym)));
        let wire = Ctx {
            view: &view,
            ppp: 1.0,
            wireframe: true,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        assert!(!crate::canvas::with_canvas(&mut pm, |c| wire.small_polygon(c, &square.extent(), &square, &sym)));
        let invalid = View { camera: Camera { res: -1.0, ..view.camera }, ..view };
        let invalid_ctx = Ctx {
            view: &invalid,
            ppp: 1.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        assert!(!crate::canvas::with_canvas(&mut pm, |c| invalid_ctx.small_polygon(c, &square.extent(), &square, &sym)));
        let far = View { camera: Camera { center: Coord::new(10_000_000.0, 10_000_000.0), ..view.camera }, ..view };
        let far_ctx = Ctx {
            view: &far,
            ppp: 1.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let far_square = polygon(10_000_000.13, 10_000_000.13, 1.5, 1.5);
        let mut actual = Pixmap::new(40, 40).unwrap();
        actual.fill(tiny_skia::Color::WHITE);
        let mut expected = actual.clone();
        crate::canvas::with_canvas(&mut expected, |c| far_ctx.feature(c, &far_square, &sym));
        assert!(crate::canvas::with_canvas(&mut actual, |c| far_ctx.small_polygon(c, &far_square.extent(), &far_square, &sym)));
        assert!(actual.data().iter().zip(expected.data()).all(|(a, b)| a.abs_diff(*b) <= 8));
        let hidpi = View { pixels_per_point: 2.0, ..view };
        let hidpi_ctx = Ctx {
            view: &hidpi,
            ppp: 2.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let small = polygon(0.13, 0.13, 0.75, 0.75);
        let hidpi_sym = Symbol { stroke_width: 0.7, ..sym };
        let mut actual = Pixmap::new(80, 80).unwrap();
        let mut expected = actual.clone();
        crate::canvas::with_canvas(&mut expected, |c| hidpi_ctx.feature(c, &small, &hidpi_sym));
        assert!(crate::canvas::with_canvas(&mut actual, |c| hidpi_ctx.small_polygon(c, &small.extent(), &small, &hidpi_sym)));
        assert!(actual.data().iter().zip(expected.data()).all(|(a, b)| a.abs_diff(*b) <= 8));
        let edge = polygon(19.6, 0.13, 1.4, 1.4);
        let mut actual = Pixmap::new(40, 40).unwrap();
        let mut expected = actual.clone();
        crate::canvas::with_canvas(&mut expected, |c| ctx.feature(c, &edge, &sym));
        assert!(crate::canvas::with_canvas(&mut actual, |c| ctx.small_polygon(c, &edge.extent(), &edge, &sym)));
        assert!(actual.data().iter().zip(expected.data()).all(|(a, b)| a.abs_diff(*b) <= 8));
        let _antialias = Antialias::set(false);
        assert!(!crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(c, &square.extent(), &square, &sym)));
    }

    #[test]
    fn small_polygon_shortcut_matches_land_cover_tone() {
        let view = View {
            width: 100.0,
            height: 100.0,
            pixels_per_point: 1.0,
            camera: Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let ctx = Ctx {
            view: &view,
            ppp: 1.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let colors = [Rgba::rgb(49, 130, 72), Rgba::rgb(219, 177, 80), Rgba::rgb(65, 120, 180), Rgba::rgb(200, 90, 55)];
        for (size, gap, stroke_width, thinned, alpha) in [
            (1.1, 0.18, 0.0, false, 255),
            (1.1, 0.18, 1.4, false, 255),
            (1.5, 0.2, 0.0, false, 255),
            (1.5, 0.2, 1.4, false, 255),
            (1.1, 0.18, 1.4, true, 255),
            (1.5, 0.2, 1.4, true, 255),
            (1.1, 0.18, 1.4, true, 128),
            (1.5, 0.2, 1.4, true, 128),
        ] {
            let features: Vec<_> = (0..40)
                .flat_map(|i| (0..40).map(move |j| (i, j)))
                .map(|(i, j)| {
                    let (x, y, step) = (-44.0 + i as f64 * (size + gap), -44.0 + j as f64 * (size + gap), size / 7.0);
                    let inset = |t: usize| if (i * 31 + j * 17 + t).is_multiple_of(3) { step * 0.3 } else { 0.0 };
                    let mut ring = Vec::new();
                    for t in 0..7 {
                        ring.push(Coord::new(x + t as f64 * step, y + inset(t)));
                        let end = if t == 6 { x + size } else { x + (t + 1) as f64 * step };
                        ring.push(Coord::new(end, y + inset(t)));
                    }
                    ring.push(Coord::new(x + size, y + size));
                    for t in (1..=7).rev() {
                        let start = if t == 7 { x + size } else { x + t as f64 * step };
                        ring.push(Coord::new(start, y + size - inset(t + 7)));
                        ring.push(Coord::new(x + (t - 1) as f64 * step, y + size - inset(t + 7)));
                    }
                    ring.push(Coord::new(x, y + inset(0)));
                    let symbol = Symbol {
                        fill: colors[(i * 31 + j * 17) % colors.len()].with_alpha(alpha),
                        stroke: Rgba::rgb(30, 30, 30).with_alpha(alpha),
                        stroke_width,
                        size: 0.0,
                        marker: Marker::Circle,
                    };
                    let original = Geometry::Polygon(vec![vec![ring]]);
                    let extent = original.extent();
                    let geometry = if thinned { rustgis_map::lod::thin(&original, 0.25) } else { original };
                    (geometry, extent, symbol)
                })
                .collect();
            let draw = |fast: bool| {
                let mut pm = Pixmap::new(100, 100).unwrap();
                if alpha == 255 {
                    pm.fill(tiny_skia::Color::WHITE);
                }
                let mut used = 0;
                for (g, extent, sym) in &features {
                    if fast && crate::canvas::with_canvas(&mut pm, |c| ctx.small_polygon(c, extent, g, sym)) {
                        used += 1;
                    } else {
                        crate::canvas::with_canvas(&mut pm, |c| ctx.feature(c, g, sym));
                    }
                }
                (pm, used)
            };
            let ((reference, _), (fast, used)) = (draw(false), draw(true));
            let difference =
                reference.data().iter().zip(fast.data()).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>() / reference.data().len() as f64;
            let maximum = reference.data().iter().zip(fast.data()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(
                difference < 0.6 && maximum <= 100,
                "{size} px, stroke {stroke_width}, thinned {thinned}, alpha {alpha}: mean {difference}, max {maximum}"
            );
            if stroke_width > 0.0 && alpha == 255 {
                assert!(used >= features.len() * 95 / 100, "{size} px, thinned {thinned}: only {used} eligible");
            } else {
                assert_eq!(used, 0, "unoutlined or translucent styles keep their paths");
            }
        }
    }

    #[test]
    fn small_polygons_draw_like_the_path_in_a_thinned_map() {
        let mut ds = Dataset::new(
            "land cover",
            GeometryType::Polygon,
            vec![Field::new("fill", FieldType::Text), Field::new("outline", FieldType::Text), Field::new("width", FieldType::Double)],
        );
        let size = 0.0002;
        let step = size / 7.0;
        for i in 0usize..100 {
            for j in 0usize..80 {
                let (x, y) = (i as f64 * size * 1.1, j as f64 * size * 1.1);
                let inset = |t: usize| if (i * 31 + j * 17 + t).is_multiple_of(3) { step * 0.3 } else { 0.0 };
                let mut ring = Vec::new();
                for t in 0..7 {
                    ring.push(Coord::new(x + t as f64 * step, y + inset(t)));
                    let end = if t == 6 { x + size } else { x + (t + 1) as f64 * step };
                    ring.push(Coord::new(end, y + inset(t)));
                }
                ring.push(Coord::new(x + size, y + size));
                for t in (1..=7).rev() {
                    let start = if t == 7 { x + size } else { x + t as f64 * step };
                    ring.push(Coord::new(start, y + size - inset(t + 7)));
                    ring.push(Coord::new(x + (t - 1) as f64 * step, y + size - inset(t + 7)));
                }
                ring.push(Coord::new(x, y + inset(0)));
                ds.push_next(
                    Some(Geometry::Polygon(vec![vec![ring]])),
                    vec![Value::Text("#4A90D9".into()), Value::Text("#1F4E8C".into()), Value::Float(0.7)],
                );
            }
        }
        let mut map = Map::new("Map");
        map.basemap = rustgis_map::Basemap::Blank;
        map.graticule = false;
        let id = map.add_dataset(ds);
        let sym = Symbol {
            fill: Rgba::rgb(0x4A, 0x90, 0xD9),
            stroke: Rgba::rgb(0x1F, 0x4E, 0x8C),
            stroke_width: 0.7,
            size: 10.0,
            marker: Marker::Circle,
        };
        map.layer_mut(id).unwrap().renderer = rustgis_map::Renderer::Single(sym);
        let mut baseline = map.clone();
        baseline.layer_mut(id).unwrap().graphics = true; // The same symbol, but no splat shortcut.
        assert_eq!(baseline.layer(id).unwrap().symbol(0), sym);
        // About 1.5 device pixels per cell: thinning removes detail while enough rings remain
        // near-rectangular for the coverage shortcut (at 1 px, thinning can cut their corners).
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.011, 0.0088)), res: 30.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 100.0, height: 100.0, pixels_per_point: 2.0, camera, sky: false };
        let before = render(&mut baseline, &view).unwrap().pixmap;
        let after = render(&mut map, &view).unwrap().pixmap;
        let difference =
            before.data().iter().zip(after.data()).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>() / before.data().len() as f64;
        let maximum = before.data().iter().zip(after.data()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(difference < 0.6 && maximum <= 100, "map: mean {difference}, max {maximum}");
        let layer = map.layer(id).unwrap();
        // Plain layers draw from this cache and leave the full projection unallocated.
        let (drawn, extents) = layer.display_cache().unwrap();
        let vertices = |geometries: &[Option<Geometry>]| geometries.iter().flatten().map(rustgis_map::lod::vertices).sum::<usize>();
        let original_vertices =
            layer.dataset.features.iter().filter_map(|f| f.geometry.as_ref()).map(rustgis_map::lod::vertices).sum::<usize>();
        assert!(vertices(drawn) < original_vertices, "the map should use simplified geometry");
        let ctx = Ctx {
            view: &view,
            ppp: 2.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let mut scratch = Pixmap::new(200, 200).unwrap();
        let eligible = drawn
            .iter()
            .zip(extents)
            .filter(|(g, e)| g.as_ref().is_some_and(|g| crate::canvas::with_canvas(&mut scratch, |c| ctx.small_polygon(c, e, g, &sym))))
            .count();
        assert!(eligible >= 2_400, "only {eligible} of {} projected features use the shortcut", drawn.len());
        map.layer_mut(id).unwrap().transparency = 50.0;
        baseline.layer_mut(id).unwrap().transparency = 50.0;
        let before = render(&mut baseline, &view).unwrap().pixmap;
        let after = render(&mut map, &view).unwrap().pixmap;
        assert_eq!(before.data(), after.data(), "transparent layers keep the original rendering path");
    }

    /// A filled polygon a few pixels across has its 1.4 px outline mitred, which covers nearly the
    /// same pixels as round joins at that size; a larger polygon, an outline-only one and a wide
    /// outline keep round joins (#1064).
    #[test]
    fn small_filled_polygons_have_mitred_outlines_that_look_the_same() {
        let view = View {
            width: 100.0,
            height: 100.0,
            pixels_per_point: 2.0,
            camera: Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let ctx = Ctx {
            view: &view,
            ppp: 2.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let sym =
            Symbol { fill: Rgba::rgb(200, 120, 0), stroke: Rgba::rgb(30, 30, 30), stroke_width: 0.7, size: 0.0, marker: Marker::Circle };
        // Polygons `cell` map units across (twice that in device px), tiled as land cover is, each
        // a staircase of 7 steps a side, as land cover traced from a raster is.
        let grid = |cell: f64| -> Vec<Geometry> {
            let n = (90.0 / cell) as usize;
            (0..n)
                .flat_map(|i| (0..n).map(move |j| (i, j)))
                .map(|(i, j)| {
                    let (x, y, s) = (-45.0 + i as f64 * cell, -45.0 + j as f64 * cell, cell / 7.0);
                    let step = |t: usize| if (i * 31 + j * 17 + t).is_multiple_of(3) { s * 0.3 } else { 0.0 };
                    let mut ring = Vec::new();
                    for t in 0..7 {
                        ring.push(Coord::new(x + t as f64 * s, y + step(t)));
                        ring.push(Coord::new(x + (t + 1) as f64 * s, y + step(t)));
                    }
                    ring.push(Coord::new(x + cell, y + cell));
                    for t in (1..=7).rev() {
                        ring.push(Coord::new(x + t as f64 * s, y + cell - step(t + 7)));
                        ring.push(Coord::new(x + (t - 1) as f64 * s, y + cell - step(t + 7)));
                    }
                    ring.push(Coord::new(x, y + step(0)));
                    Geometry::Polygon(vec![vec![ring]])
                })
                .collect()
        };
        let draw = |gs: &[Geometry], s: &Symbol, round: bool| {
            let mut pm = Pixmap::new(200, 200).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            for g in gs {
                match g {
                    Geometry::Polygon(polys) if round => crate::canvas::with_canvas(&mut pm, |c| {
                        ctx.polygon(c, polys, Some(paint(s.fill, 1.0)), Some((paint(s.stroke, 1.0), s.stroke_width)))
                    }),
                    _ => crate::canvas::with_canvas(&mut pm, |c| ctx.feature(c, g, s)),
                }
            }
            pm
        };
        // 1.5 and 1.9 points across: 3 and 3.8 device px.
        for cell in [1.5, 1.9] {
            let small = grid(cell);
            let (mitred, round) = (draw(&small, &sym, false), draw(&small, &sym, true));
            assert_ne!(mitred.data(), round.data(), "{cell}: mitred");
            let d = mitred.data().iter().zip(round.data()).map(|(x, y)| x.abs_diff(*y) as f64).sum::<f64>() / round.data().len() as f64;
            assert!(d < 0.5, "{cell}: a mean difference of {d} per channel");
        }
        // Larger, without a fill or with a wide outline: as before, pixel for pixel.
        let large = grid(2.5);
        assert_eq!(draw(&large, &sym, false).data(), draw(&large, &sym, true).data());
        let small = grid(1.9);
        let hollow = Symbol { fill: Rgba::new(0, 0, 0, 0), ..sym };
        assert_eq!(draw(&small, &hollow, false).data(), draw(&small, &hollow, true).data());
        let wide = Symbol { stroke_width: 1.5, ..sym };
        assert_eq!(draw(&small, &wide, false).data(), draw(&small, &wide, true).data());
        // The limits themselves are mitred: a polygon 4 device px across, and a 2 px outline.
        let four = grid(2.0);
        assert_ne!(draw(&four, &sym, false).data(), draw(&four, &sym, true).data(), "4 px across");
        let two = Symbol { stroke_width: 1.0, ..sym };
        assert_ne!(draw(&small, &two, false).data(), draw(&small, &two, true).data(), "a 2 px outline");
        // Triangles with corners from 10° to 80°: the sharp ones are bevelled, so no corner grows
        // a spike. A corner pixel differs as a bevelled one does (98 levels at most here); with
        // tiny-skia's own limit of 4, mitres reach 141.
        let triangles: Vec<Geometry> = (0..64)
            .map(|k| {
                let (x, y) = (-44.0 + (k % 8) as f64 * 11.0, -44.0 + (k / 8) as f64 * 11.0);
                let a = (10.0 + k as f64 * 70.0 / 63.0).to_radians();
                let ring =
                    vec![Coord::new(x, y), Coord::new(x + 1.9, y), Coord::new(x + 1.9 * a.cos(), y + 1.9 * a.sin()), Coord::new(x, y)];
                Geometry::Polygon(vec![vec![ring]])
            })
            .collect();
        let (mitred, round) = (draw(&triangles, &sym, false), draw(&triangles, &sym, true));
        let max = mitred.data().iter().zip(round.data()).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(max <= 100, "a pixel {max} levels off");
        let d = mitred.data().iter().zip(round.data()).map(|(x, y)| x.abs_diff(*y) as f64).sum::<f64>() / round.data().len() as f64;
        assert!(d < 0.5, "triangles: a mean difference of {d} per channel");
    }

    #[test]
    fn offset_polylines_shift_right_of_their_direction() {
        // Screen y points down: right of an eastward line is below it.
        assert_eq!(offset_polyline(&[(0.0, 0.0), (10.0, 0.0)], 3.0), vec![(0.0, 3.0), (10.0, 3.0)]);
        assert_eq!(offset_polyline(&[(10.0, 0.0), (0.0, 0.0)], 3.0), vec![(10.0, -3.0), (0.0, -3.0)]);
        assert_eq!(offset_polyline(&[(0.0, 0.0), (10.0, 0.0)], -3.0), vec![(0.0, -3.0), (10.0, -3.0)]);
        // A right-angle corner is mitred: east then south, shifted right (inside the turn).
        let p = offset_polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], 2.0);
        assert_eq!(p.len(), 3);
        assert!((p[1].0 - 8.0).abs() < 1e-4 && (p[1].1 - 2.0).abs() < 1e-4, "{p:?}");
        // Doubling back is bevelled rather than mitred far away.
        let p = offset_polyline(&[(0.0, 0.0), (10.0, 0.0), (0.0, 0.1)], 2.0);
        assert!(p.iter().all(|q| q.0.abs() < 20.0 && q.1.abs() < 20.0), "{p:?}");
        // Repeated, non-finite and lone points give nothing to draw, and no NaN.
        assert!(offset_polyline(&[(1.0, 1.0), (1.0, 1.0)], 2.0).is_empty());
        assert!(offset_polyline(&[(f32::NAN, 0.0), (1.0, 1.0)], 2.0).is_empty());
        let p = offset_polyline(&[(0.0, 0.0), (0.0, 0.0), (5.0, 0.0), (5.0, 0.0)], 1.0);
        assert_eq!(p, vec![(0.0, 1.0), (5.0, 1.0)]);
    }

    #[test]
    fn stacked_fills_hatch_and_dot_polygons_anchored_to_the_map() {
        use rustgis_map::fills::{FillLayer, FillPattern};
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("fields", GeometryType::Polygon, vec![Field::new("kind", FieldType::Text)]);
        let square = |x0: f64, x1: f64| {
            Geometry::Polygon(vec![vec![vec![
                Coord::new(x0, -0.5),
                Coord::new(x1, -0.5),
                Coord::new(x1, 0.5),
                Coord::new(x0, 0.5),
                Coord::new(x0, -0.5),
            ]]])
        };
        d.push(Some(square(-0.6, 0.6)), vec![Value::Text("a".into())]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let (red, blue) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 0, 255));
        let rgb = |r: &Rendered, x: u32, y: u32| r.pixmap.pixel(x, y).map(|p| (p.red(), p.green(), p.blue())).unwrap();
        let before = render(&mut m, &view).unwrap();
        let plain = rgb(&before, 100, 100);
        // Hatched: red lines 6 points apart across the square's light fill, nothing outside it.
        m.layer_mut(id).unwrap().fill_layers = vec![
            FillLayer { color: Some(Rgba::rgb(240, 240, 240)), pattern: FillPattern::Solid },
            FillLayer { color: Some(red), pattern: FillPattern::Hatch { angle: 0.0, spacing: 6.0, width: 1.5, offset: 0.0 } },
        ];
        let r = render(&mut m, &view).unwrap();
        let rows: Vec<u32> = (60..140).filter(|&y| rgb(&r, 100, y).0 > 200 && rgb(&r, 100, y).1 < 120).collect();
        assert!(rows.len() >= 10, "hatch rows: {rows:?}");
        let starts: Vec<u32> = rows.iter().copied().filter(|y| !rows.contains(&(y - 1))).collect();
        assert!(starts.windows(2).all(|w| w[1] - w[0] == 6), "6 points apart: {starts:?}");
        assert_eq!(rgb(&r, 100, 5), rgb(&before, 100, 5), "nothing outside the polygon");
        assert!((60..140).any(|y| rgb(&r, 100, y) == (240, 240, 240)), "the solid fill under the lines");
        // Anchored to the map: panned 3 points, the lines move 3 points with it.
        let panned = View { camera: Camera { center: Coord::new(camera.center.x, camera.center.y + 3.0 * camera.res), ..camera }, ..view };
        let r2 = render(&mut m, &panned).unwrap();
        let moved: Vec<u32> = (60..140).filter(|&y| rgb(&r2, 100, y).0 > 200 && rgb(&r2, 100, y).1 < 120).collect();
        assert!(moved.iter().all(|y| rows.contains(&(y - 3)) || *y < 63 || *y > 137), "{rows:?} → {moved:?}");
        // Turned 90°: the lines run up and down.
        if let FillPattern::Hatch { angle, .. } = &mut m.layer_mut(id).unwrap().fill_layers[1].pattern {
            *angle = 90.0;
        }
        let r = render(&mut m, &view).unwrap();
        let red_at = |x: u32, y: u32| rgb(&r, x, y).0 > 200 && rgb(&r, x, y).1 < 120;
        let cols = (60..140).filter(|&x| red_at(x, 100)).count();
        // Between the lines, a column with no red at all.
        let clear = (60..140).any(|x| (60..140).all(|y| !red_at(x, y)));
        assert!(cols >= 10 && clear, "vertical: {cols} columns");
        // Dots: blue markers on a grid, in the feature's colour when the fill has none.
        let dots = FillPattern::Dots {
            marker: rustgis_map::Marker::Circle,
            size: 3.0,
            outline: None,
            outline_width: 0.0,
            spacing: [10.0, 10.0],
            displacement: [5.0, 0.0],
            offset: [0.0, 0.0],
        };
        m.layer_mut(id).unwrap().fill_layers = vec![FillLayer { color: Some(blue), pattern: dots }];
        let r = render(&mut m, &view).unwrap();
        let bluish = (60..140).flat_map(|y| (60..140).map(move |x| (x, y))).filter(|&(x, y)| rgb(&r, x, y) == (0, 0, 255)).count();
        assert!(bluish > 20, "dots: {bluish}");
        // A class's own fills over the layer's (Unique Values).
        let base = rustgis_map::Symbol::default_for(GeometryType::Polygon, id);
        m.layer_mut(id).unwrap().renderer =
            rustgis_map::Renderer::unique(&m.layer(id).unwrap().dataset, &[], "kind", rustgis_map::Ramp::default(), base).unwrap();
        if let rustgis_map::Renderer::Unique { class_fills, .. } = &mut m.layer_mut(id).unwrap().renderer {
            class_fills.insert("a".into(), vec![FillLayer { color: Some(red), pattern: FillPattern::Solid }]);
        }
        let r = render(&mut m, &view).unwrap();
        assert_eq!(rgb(&r, 100, 100), (255, 0, 0), "its class's fill");
        // None: the symbol's fill, as before.
        m.layer_mut(id).unwrap().renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol::default_for(GeometryType::Polygon, id));
        m.layer_mut(id).unwrap().fill_layers.clear();
        assert_eq!(rgb(&render(&mut m, &view).unwrap(), 100, 100), plain);
    }

    /// Gradient and shapeburst fills, as QGIS draws them (sampled from QGIS 4.2's renders): a
    /// gradient from its first colour at its first point (the box's top middle) to its second,
    /// turned anticlockwise, radial out to its second point, conical anticlockwise from it; a
    /// shapeburst from its first colour at the edge to its second deepest inside (or as deep as
    /// its distance), holes' edges shading too unless left out.
    #[test]
    fn gradient_and_shapeburst_fills_shade_as_qgis_does() {
        use rustgis_map::fills::{FillLayer, FillPattern, GradientPoint, GradientShape, Spread, Stops};
        let ring = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)]
        };
        // A map of one polygon of `parts` (each its rings), unoutlined.
        let map_of = |parts: Vec<Vec<Vec<Coord>>>| {
            let mut m = Map::new("Map");
            m.basemap = rustgis_map::Basemap::Blank;
            let mut d = Dataset::new("fields", GeometryType::Polygon, vec![]);
            d.push(Some(Geometry::Polygon(parts)), vec![]);
            let id = m.add_dataset(d);
            m.layer_mut(id).unwrap().renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol {
                stroke: Rgba::new(0, 0, 0, 0),
                ..rustgis_map::Symbol::default_for(GeometryType::Polygon, id)
            });
            (m, id)
        };
        let (mut m, id) = map_of(vec![vec![ring(-0.5, -0.5, 0.5, 0.5)]]);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let (red, blue) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 0, 255));
        // The square's edges on screen, and its red at a pixel (blue is the rest of it).
        let r0 = render(&mut m, &view).unwrap();
        let inside: Vec<u32> = (0..200).filter(|&x| r0.pixmap.pixel(x, 100).is_some_and(|p| p.red() < 250)).collect();
        let (lo, hi) = (*inside.first().unwrap(), *inside.last().unwrap());
        let (mid, span) = ((lo + hi) / 2, (hi - lo) as f32);
        let set = |m: &mut Map, pattern: FillPattern| {
            m.layer_mut(id).unwrap().fill_layers = vec![FillLayer { color: Some(red), pattern }];
            render(m, &view).unwrap()
        };
        let redness = |r: &Rendered, x: u32, y: u32| {
            let p = r.pixmap.pixel(x, y).unwrap();
            assert!(p.green() < 10, "red to blue at ({x}, {y}): {p:?}");
            f32::from(p.red()) / 255.0
        };
        let near = |got: f32, want: f32, what: &str| assert!((got - want).abs() < 0.08, "{what}: {got} not {want}");
        let gradient = |shape, from, to, angle| FillPattern::Gradient {
            shape,
            color2: blue,
            stops: Stops::default(),
            discrete: false,
            from,
            to,
            spread: Spread::Pad,
            angle,
            in_view: false,
        };
        let top = GradientPoint::At([0.5, 0.0]);
        let bottom = GradientPoint::At([0.5, 1.0]);
        // Linear, top to bottom.
        let r = set(&mut m, gradient(GradientShape::Linear, top, bottom, 0.0));
        near(redness(&r, mid, lo + 2), 1.0, "top");
        near(redness(&r, mid, mid), 0.5, "middle");
        near(redness(&r, mid, hi - 2), 0.0, "bottom");
        near(redness(&r, lo + 2, mid), redness(&r, hi - 2, mid), "level across");
        // Turned 90° anticlockwise: left to right.
        let r = set(&mut m, gradient(GradientShape::Linear, top, bottom, 90.0));
        near(redness(&r, lo + 2, mid), 1.0, "turned: left");
        near(redness(&r, hi - 2, mid), 0.0, "turned: right");
        // Radial from the middle out to the right edge's middle.
        let centre = GradientPoint::At([0.5, 0.5]);
        let right = GradientPoint::At([1.0, 0.5]);
        let r = set(&mut m, gradient(GradientShape::Radial, centre, right, 0.0));
        near(redness(&r, mid, mid), 1.0, "radial: middle");
        near(redness(&r, mid + (span / 4.0) as u32, mid), 0.5, "radial: halfway out");
        near(redness(&r, mid, lo + 1), 0.0, "radial: top edge as far out");
        // Conical: anticlockwise from the right, a quarter of the way round at the top.
        let r = set(&mut m, gradient(GradientShape::Conical, centre, right, 0.0));
        near(redness(&r, mid, lo + 10), 0.75, "conical: top");
        near(redness(&r, lo + 10, mid), 0.5, "conical: left");
        near(redness(&r, mid, hi - 10), 0.25, "conical: bottom");
        // The centroid as its first point.
        let r = set(&mut m, gradient(GradientShape::Linear, GradientPoint::CENTROID, bottom, 0.0));
        near(redness(&r, mid, lo + 5), 1.0, "from the centroid: red above it");
        near(redness(&r, mid, mid + (span / 4.0) as u32), 0.5, "from the centroid: halfway down");
        // Reflected past its second point.
        let quarter = GradientPoint::At([0.5, 0.25]);
        let mut reflected = gradient(GradientShape::Linear, top, quarter, 0.0);
        if let FillPattern::Gradient { spread, .. } = &mut reflected {
            *spread = Spread::Reflect;
        }
        let r = set(&mut m, reflected);
        near(redness(&r, mid, lo + (span * 0.375) as u32), 0.5, "reflected: back halfway");
        near(redness(&r, mid, mid), 1.0, "reflected: back at the first colour");
        // A shapeburst over the whole shape: red at the edge, blue at the middle, a quarter in half.
        let burst = |distance, ignore_rings| FillPattern::Shapeburst {
            color2: blue,
            stops: Stops::default(),
            discrete: false,
            distance,
            blur: 0,
            ignore_rings,
        };
        let r = set(&mut m, burst(None, false));
        near(redness(&r, lo + 1, mid), 1.0, "shapeburst: edge");
        near(redness(&r, lo + (span / 4.0) as u32, mid), 0.5, "shapeburst: a quarter in");
        near(redness(&r, mid, mid), 0.0, "shapeburst: middle");
        // 10 points deep: blue past it.
        let r = set(&mut m, burst(Some(10.0), false));
        near(redness(&r, lo + 4, mid), 0.5, "10 points deep: 5 in (the edge a pixel out)");
        near(redness(&r, lo + 15, mid), 0.0, "10 points deep: past it");
        // A hole: shaded from its edge too, unless rings are left out.
        let (mut m, _) = map_of(vec![vec![ring(-0.5, -0.5, 0.5, 0.5), ring(-0.1, -0.1, 0.1, 0.1)]]);
        let hole = (span * 0.4) as u32;
        let r = set(&mut m, burst(None, false));
        assert!(redness(&r, lo + hole - 2, mid) > 0.7, "next to the hole: its edge");
        let r = set(&mut m, burst(None, true));
        assert!(redness(&r, lo + hole - 2, mid) < 0.4, "rings left out: deep inside");
        assert_eq!(r.pixmap.pixel(mid, mid).map(|p| p.alpha()), r0.pixmap.pixel(0, 0).map(|p| p.alpha()), "the hole stays empty");
        // Each part of a multi-part feature in its own box: two squares side by side, each red at
        // its own top.
        let (mut m, _) = map_of(vec![vec![ring(-0.5, -0.5, -0.1, 0.5)], vec![ring(0.1, -0.2, 0.5, 0.2)]]);
        let r = set(&mut m, gradient(GradientShape::Linear, top, bottom, 0.0));
        let top_of = |x: u32| (0..200).find(|&y| r.pixmap.pixel(x, y).is_some_and(|p| p.red() < 250 || p.blue() < 250)).unwrap();
        let right = mid + (span * 0.3) as u32;
        near(redness(&r, right, top_of(right) + 1), 1.0, "the smaller part red at its own top");
        near(redness(&r, lo + 5, lo + 2), 1.0, "the taller part red at its top");
        // Held, not blended: each colour up to the next stop.
        let mut held = gradient(GradientShape::Linear, top, bottom, 0.0);
        if let FillPattern::Gradient { stops, discrete, .. } = &mut held {
            *stops = Stops::new([rustgis_map::fills::Stop { at: 0.5, color: Rgba::rgb(0, 255, 0) }]);
            *discrete = true;
        }
        let (mut m, _) = map_of(vec![vec![ring(-0.5, -0.5, 0.5, 0.5)]]);
        let r = set(&mut m, held);
        let at = |y: u32| r.pixmap.pixel(mid, y).map(|p| (p.red(), p.green(), p.blue())).unwrap();
        assert_eq!((at(lo + 5), at(mid - 3), at(mid + 3)), ((255, 0, 0), (255, 0, 0), (0, 255, 0)), "red, then green from halfway");
        // In the view's box: the view's top red, its middle half way, whatever the feature's box.
        let mut in_view = gradient(GradientShape::Linear, top, bottom, 0.0);
        if let FillPattern::Gradient { in_view: v, .. } = &mut in_view {
            *v = true;
        }
        let r = set(&mut m, in_view);
        near(redness(&r, mid, lo + 2), 1.0 - (lo + 2) as f32 / 200.0, "in the view: by the view's height");
        near(redness(&r, mid, 100), 0.5, "in the view: half way down it");
        // A shapeburst larger than it works on at full resolution (scaled down) keeps its edges:
        // a square near 3,000 pixels across, red at its edge and blue at its middle.
        let big = View { width: 3000.0, height: 3000.0, camera: Camera { res: 37.0, ..camera }, ..view };
        let (mut m, id) = map_of(vec![vec![ring(-0.5, -0.5, 0.5, 0.5)]]);
        m.layer_mut(id).unwrap().fill_layers = vec![FillLayer { color: Some(red), pattern: burst(None, false) }];
        let r = render(&mut m, &big).unwrap();
        let inside: Vec<u32> = (0..3000).filter(|&x| r.pixmap.pixel(x, 1500).is_some_and(|p| p.alpha() > 0 && (p.green() < 250))).collect();
        let (blo, bhi) = (*inside.first().unwrap(), *inside.last().unwrap());
        assert!(bhi - blo > 2500, "{blo}..{bhi}");
        near(redness(&r, blo + 3, 1500), 1.0, "large: its edge");
        near(redness(&r, (blo + bhi) / 2, 1500), 0.0, "large: its middle");
        near(redness(&r, blo + (bhi - blo) / 4, 1500), 0.5, "large: a quarter in");
        // Zoomed into the middle of a polygon shaded over its whole shape, no edge in sight: its
        // middle still the far colour (from the whole polygon's depth), not the edge's.
        let inside = View { camera: Camera { res: 25.0, ..camera }, ..view };
        let r = render(&mut m, &inside).unwrap();
        assert!(redness(&r, 100, 100) < 0.1, "zoomed into the middle: {}", redness(&r, 100, 100));
        // A radial gradient from the centroid of a polygon far larger than the view keeps its
        // place on the map as it pans: a map point the same colour before and after.
        let (mut m, id) = map_of(vec![vec![ring(-0.5, -0.5, 0.5, 0.5)]]);
        let far_out = GradientPoint::At([1.0, 0.5]);
        m.layer_mut(id).unwrap().fill_layers =
            vec![FillLayer { color: Some(red), pattern: gradient(GradientShape::Radial, GradientPoint::CENTROID, far_out, 0.0) }];
        let near_corner = Camera { center: lonlat_to_merc(Coord::new(0.3, 0.3)), res: 25.0, ..camera };
        let a = render(&mut m, &View { camera: near_corner, ..view }).unwrap();
        let panned = Camera { center: Coord::new(near_corner.center.x + 20.0 * 25.0, near_corner.center.y), ..near_corner };
        let b = render(&mut m, &View { camera: panned, ..view }).unwrap();
        near(redness(&b, 80, 100), redness(&a, 100, 100), "the same map point, panned 20 pixels");
        assert!(redness(&a, 100, 100) < 0.9 && redness(&a, 100, 100) > 0.1, "part way out: {}", redness(&a, 100, 100));
        // A thin diagonal strip (about 6 pixels wide) across a large export, too thin for the
        // coarse grid: red at its edges, blue down its middle, not one flat colour.
        let (mut m, id) = map_of(vec![vec![vec![
            Coord::new(-0.45, -0.45),
            Coord::new(-0.447, -0.45),
            Coord::new(0.45, 0.447),
            Coord::new(0.45, 0.45),
            Coord::new(-0.45, -0.45),
        ]]]);
        m.layer_mut(id).unwrap().fill_layers = vec![FillLayer { color: Some(red), pattern: burst(None, false) }];
        let r = render(&mut m, &big).unwrap();
        let row: Vec<u32> = (0..3000).filter(|&x| r.pixmap.pixel(x, 1500).is_some_and(|p| p.alpha() > 0 && p.green() < 250)).collect();
        assert!(row.len() >= 5 && row.len() < 20, "the strip across the middle row: {row:?}");
        let (first, last) = (row[0], row[row.len() - 1]);
        // (A pixel in from the edge: the edge pixel is partly covered.)
        assert!(redness(&r, first + 1, 1500) > 0.5, "its edge: {}", redness(&r, first + 1, 1500));
        assert!(redness(&r, (first + last) / 2, 1500) < 0.4, "its middle: {}", redness(&r, (first + last) / 2, 1500));
    }

    #[test]
    fn a_polygon_under_a_pixel_is_drawn_with_its_stacked_fills() {
        // Smaller than a pixel (about 0.7 across), it isn't blended in the symbol's fill colour:
        // its own solid fill shows.
        use rustgis_map::fills::{FillLayer, FillPattern};
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("specks", GeometryType::Polygon, vec![]);
        let (x0, x1, y0, y1) = (-0.0045, 0.0045, -0.0045, 0.0045);
        let ring = vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)];
        d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 20.0, height: 20.0, pixels_per_point: 1.0, camera, sky: false };
        let symbol = rustgis_map::Symbol {
            fill: Rgba::rgb(0, 0, 255),
            stroke_width: 0.0,
            ..rustgis_map::Symbol::default_for(GeometryType::Polygon, id)
        };
        m.layer_mut(id).unwrap().renderer = rustgis_map::Renderer::Single(symbol);
        let ink = |m: &mut Map| {
            let r = render(m, &view).unwrap();
            r.pixmap
                .pixels()
                .iter()
                .filter(|p| p.red() < 250 || p.green() < 250 || p.blue() < 250)
                .map(|p| (p.red(), p.blue()))
                .collect::<Vec<_>>()
        };
        let plain = ink(&mut m);
        assert!(!plain.is_empty() && plain.iter().all(|(r, b)| b >= r), "the symbol's blue: {plain:?}");
        m.layer_mut(id).unwrap().fill_layers = vec![FillLayer { color: Some(Rgba::rgb(255, 0, 0)), pattern: FillPattern::Solid }];
        let filled = ink(&mut m);
        assert!(!filled.is_empty() && filled.iter().all(|(r, b)| r > b), "its red fill: {filled:?}");
        // Far smaller (0.15 across), still blended in, not lost as a path of no area.
        let mut small = Map::new("Map");
        small.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("specks", GeometryType::Polygon, vec![]);
        let t = 0.0045 * 0.15 / 0.67;
        let ring = vec![Coord::new(-t, -t), Coord::new(t, -t), Coord::new(t, t), Coord::new(-t, t), Coord::new(-t, -t)];
        d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let tiny = small.add_dataset(d);
        let l = small.layer_mut(tiny).unwrap();
        l.renderer = rustgis_map::Renderer::Single(symbol);
        l.fill_layers = vec![FillLayer { color: Some(Rgba::rgb(255, 0, 0)), pattern: FillPattern::Solid }];
        // (A faint speck: about 2 % of a pixel's area.)
        let r = render(&mut small, &view).unwrap();
        let specks: Vec<_> = r.pixmap.pixels().iter().filter(|p| p.green() < 255).map(|p| (p.red(), p.green(), p.blue())).collect();
        assert!(!specks.is_empty() && specks.iter().all(|(r, g, b)| r > g && g == b), "a speck of red: {specks:?}");
    }

    #[test]
    fn reference_scaled_hatch_stays_on_the_map_as_it_pans() {
        // With a reference scale twice the map's, a hatch 6 points apart is drawn 12 apart; panned
        // 3 points, its lines move 3 points with the polygons, not 6.
        use rustgis_map::fills::{FillLayer, FillPattern};
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("fields", GeometryType::Polygon, vec![]);
        let ring = vec![Coord::new(-0.6, -0.5), Coord::new(0.6, -0.5), Coord::new(0.6, 0.5), Coord::new(-0.6, 0.5), Coord::new(-0.6, -0.5)];
        d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let scale = m.ground_res_at(&camera) / rustgis_geom::PIXEL_M;
        let red = Rgba::rgb(255, 0, 0);
        let l = m.layer_mut(id).unwrap();
        l.reference_scale = Some(scale * 2.0);
        l.fill_layers =
            vec![FillLayer { color: Some(red), pattern: FillPattern::Hatch { angle: 0.0, spacing: 6.0, width: 1.5, offset: 0.0 } }];
        let starts = |r: &Rendered| -> Vec<u32> {
            let red_at = |y: u32| r.pixmap.pixel(100, y).is_some_and(|p| p.red() > 200 && p.green() < 120);
            (68..132).filter(|&y| red_at(y) && !red_at(y - 1)).collect()
        };
        let before = starts(&render(&mut m, &view).unwrap());
        assert!(before.len() >= 4 && before.windows(2).all(|w| w[1] - w[0] == 12), "12 points apart: {before:?}");
        let panned = View { camera: Camera { center: Coord::new(camera.center.x, camera.center.y + 3.0 * camera.res), ..camera }, ..view };
        let after = starts(&render(&mut m, &panned).unwrap());
        assert!(after.iter().filter(|y| (71..129).contains(*y)).all(|y| before.contains(&(y - 3))), "{before:?} → {after:?}");
    }

    #[test]
    fn stacked_line_strokes_draw_offset_by_direction() {
        // Two copies of one line in opposite directions (#478): with a stroke offset to the right
        // they land on opposite sides, and nothing draws on the shared centreline.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("edges", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.0), Coord::new(1.0, 0.0)]])), vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(1.0, 0.0), Coord::new(-1.0, 0.0)]])), vec![]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let red = Rgba::rgb(255, 0, 0);
        let is_red = |r: &Rendered, y: u32| {
            let p = r.pixmap.pixel(100, y).unwrap();
            (p.red(), p.green(), p.blue()) == (255, 0, 0)
        };
        let r = render(&mut m, &view).unwrap();
        let centre = r.pixmap.pixel(100, 100).unwrap();
        let blank = r.pixmap.pixel(100, 60).unwrap();
        assert_ne!(centre, blank, "the single stroke draws on the centreline");
        m.layer_mut(id).unwrap().line_strokes = vec![
            rustgis_map::LineStroke { color: Some(Rgba::rgb(0, 0, 0)), width: Some(5.0), offset: 10.0, ..Default::default() },
            rustgis_map::LineStroke { color: Some(red), width: Some(2.0), offset: 10.0, ..Default::default() },
        ];
        let r = render(&mut m, &view).unwrap();
        assert!(is_red(&r, 110) && is_red(&r, 90), "both copies, one on each side");
        assert_eq!(r.pixmap.pixel(100, 100).unwrap(), blank, "nothing on the centreline");
        // The casing shows around the core.
        let casing = r.pixmap.pixel(100, 111).unwrap();
        assert_eq!((casing.red(), casing.green(), casing.blue()), (0, 0, 0));
        // Selected, it still draws offset, with its halo; a hidden stroke gets no halo.
        m.layer_mut(id).unwrap().line_strokes.push(rustgis_map::LineStroke { width: Some(0.0), offset: -20.0, ..Default::default() });
        m.layer_mut(id).unwrap().selection = vec![0];
        let r = render(&mut m, &view).unwrap();
        assert!(is_red(&r, 110));
        assert_eq!(r.pixmap.pixel(100, 100).unwrap(), blank);
        assert_eq!(r.pixmap.pixel(100, 80).unwrap(), blank, "no halo for the hidden stroke");
        // A line under a pixel across still draws shifted, not splatted on its centreline.
        let mut d = Dataset::new("short", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(0.0005, 0.0)]])), vec![]);
        let short = m.add_dataset(d);
        m.layer_mut(id).unwrap().visible = false;
        m.layer_mut(short).unwrap().line_strokes =
            vec![rustgis_map::LineStroke { color: Some(red), width: Some(3.0), offset: 10.0, ..Default::default() }];
        let r = render(&mut m, &view).unwrap();
        assert_ne!(r.pixmap.pixel(100, 110).unwrap(), blank, "drawn 10 pt to the right");
        assert_eq!(r.pixmap.pixel(100, 100).unwrap(), blank, "nothing on the centreline");
    }

    #[test]
    fn a_points_marks_and_rotation_draw_on_the_cpu_and_old_layers_stay_on_the_gpu() {
        use rustgis_map::{LineMark, LineMarkers, LineStroke, MarkerRotation};
        // One point at the centre of a 200 px view, its angle 90 in field "a".
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("signs", GeometryType::Point, vec![rustgis_data::Field::new("a", rustgis_data::FieldType::Double)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![rustgis_data::Value::Float(90.0)]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        // A plain point stays a GPU instance, as before.
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        assert!(cmd_kinds(&scene).contains(&"points"));
        // A red square 30 pt right of the point, turned with the field to 30 pt below it.
        let red = Rgba::rgb(255, 0, 0);
        let square = LineMarkers { mark: LineMark::Marker(Marker::Square), size: 10.0, offset_along: 30.0, ..LineMarkers::default() };
        let l = m.layer_mut(id).unwrap();
        l.line_strokes = vec![LineStroke { color: Some(red), width: Some(0.0), markers: Some(square), ..LineStroke::default() }];
        let is_red = |r: &Rendered, x: u32, y: u32| r.pixmap.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.alpha()) == (255, 0, 255));
        let r = render(&mut m, &view).unwrap();
        assert!(is_red(&r, 130, 100) && !is_red(&r, 100, 100), "right of the point, the symbol's own marker gone");
        // The point 20 px left of the view, past its cull margin: its mark still shows, 10 px in.
        let left = View { camera: Camera { center: Coord::new(camera.center.x + 120.0 * camera.res, camera.center.y), ..camera }, ..view };
        let r = render(&mut m, &left).unwrap();
        assert!(is_red(&r, 10, 100), "a mark shifted into the view");
        // At twice its reference scale's size: the point 60 px left, its mark 60 px right of it.
        let far = View { camera: Camera { center: Coord::new(camera.center.x + 160.0 * camera.res, camera.center.y), ..camera }, ..view };
        let scale = m.ground_res_at(&far.camera) / rustgis_geom::PIXEL_M;
        m.layer_mut(id).unwrap().reference_scale = Some(2.0 * scale);
        let r = render(&mut m, &far).unwrap();
        assert!(is_red(&r, 4, 100), "an enlarged mark shifted into the view");
        m.layer_mut(id).unwrap().reference_scale = None;
        m.layer_mut(id).unwrap().marker_rotation = Some(MarkerRotation { field: "a".into(), counterclockwise: false });
        let r = render(&mut m, &view).unwrap();
        assert!(is_red(&r, 100, 130) && !is_red(&r, 130, 100), "turned to below it");
        // Selected: a halo round it, where it is.
        m.layer_mut(id).unwrap().selection = vec![0];
        let r = render(&mut m, &view).unwrap();
        let p = r.pixmap.pixel(100, 136).unwrap();
        assert!((p.red(), p.green(), p.blue()) != (255, 255, 255) && p.alpha() > 0, "{p:?}");
        m.layer_mut(id).unwrap().selection.clear();
        // Turned or marked, the layer draws on the CPU.
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        assert!(kinds.contains(&"image") && !kinds.contains(&"points"), "{kinds:?}");
        m.layer_mut(id).unwrap().line_strokes.clear();
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        assert!(!cmd_kinds(&scene).contains(&"points"), "turned alone, too");
    }

    #[test]
    fn a_marker_sized_and_coloured_by_expression_draws_on_the_cpu() {
        use rustgis_map::symbol_by::SymbolBy;
        // One point at the centre of a 200 px view, 40 in field "a".
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("p", GeometryType::Point, vec![rustgis_data::Field::new("a", rustgis_data::FieldType::Double)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![rustgis_data::Value::Float(40.0)]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let is_green = |r: &Rendered, x: u32, y: u32| r.pixmap.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == (0, 255, 0));
        let r = render(&mut m, &view).unwrap();
        assert!(!is_green(&r, 115, 100), "a 7 pt marker");
        let scale = m.ground_res_at(&camera) / rustgis_geom::PIXEL_M;
        m.layer_mut(id).unwrap().symbol_by = SymbolBy {
            size: Some("a".into()),
            fill: Some(format!("CASE WHEN @map_scale BETWEEN {} AND {} THEN '#00ff00' END", scale * 0.9, scale * 1.1)),
            ..Default::default()
        };
        let r = render(&mut m, &view).unwrap();
        assert!(is_green(&r, 115, 100) && is_green(&r, 100, 85), "40 pt across, green at this scale");
        // Zoomed out the fill is the symbol's own.
        let out = View { camera: Camera { res: camera.res * 4.0, ..camera }, ..view };
        let r = render(&mut m, &out).unwrap();
        assert!(!is_green(&r, 110, 100) && r.pixmap.pixel(110, 100).is_some_and(|p| p.alpha() == 255), "still 40 pt, not green");
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        assert!(kinds.contains(&"image") && !kinds.contains(&"points"), "{kinds:?}");
    }

    #[test]
    fn a_character_marker_draws_its_character_where_qgis_does_and_on_the_cpu() {
        use ab_glyph::Font;
        use rustgis_map::Glyph;
        // A 60 pt "L" at the centre of a 200 px view: its advance centred, its baseline half the
        // font's ascent below the point.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("signs", GeometryType::Point, vec![]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        let id = m.add_dataset(d);
        let black = Rgba::rgb(0, 0, 0);
        let none = Rgba::new(0, 0, 0, 0);
        m.layer_mut(id).unwrap().renderer = rustgis_map::Renderer::Single(Symbol {
            fill: black,
            stroke: none,
            stroke_width: 0.0,
            size: 60.0,
            marker: Marker::Char(Glyph::new('L', "")),
        });
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let r = render(&mut m, &view).unwrap();
        let ink = |x: u32, y: u32| r.pixmap.pixel(x, y).is_some_and(|p| p.red() < 100 && p.alpha() > 200);
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..200 {
            for x in 0..200 {
                if ink(x, y) {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
        }
        let f = crate::text::label_font().unwrap();
        let upm = f.units_per_em().unwrap();
        let baseline = 100.0 + 60.0 * f.ascent_unscaled() / 2.0 / upm;
        assert!((y1 as f32 + 1.0 - baseline).abs() < 2.0, "foot on the baseline: {y1} vs {baseline}");
        assert!(y1 - y0 > x1 - x0 && x0 < 100 && x1 > 100, "an L, about the point: {x0},{y0} {x1},{y1}");
        assert!(ink(x0 + 1, y0 + 1) && !ink(x1 - 1, y0 + 1) && ink(x1 - 1, y1 - 1), "its stem and foot, not a blob");
        // The GPU has no character template: the layer is a CPU picture there.
        let Some(RenderOutput::Gpu(scene)) = render_backend(&mut m, &view, Backend::Gpu) else { panic!("gpu scene") };
        let kinds = cmd_kinds(&scene);
        assert!(kinds.contains(&"image") && !kinds.contains(&"points"), "{kinds:?}");
        // A character no font here has still shows the point: a circle of its size.
        if let rustgis_map::Renderer::Single(sym) = &mut m.layer_mut(id).unwrap().renderer {
            sym.marker = Marker::Char(Glyph::new('\u{10FFFD}', ""));
        }
        let r = render(&mut m, &view).unwrap();
        let ink = |x: u32, y: u32| r.pixmap.pixel(x, y).is_some_and(|p| p.red() < 100 && p.alpha() > 200);
        assert!(ink(100, 100) && ink(100, 75) && !ink(100, 65), "a 60 pt circle");
        // Along a line too, turned with it.
        let mut d = Dataset::new("rail", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.5), Coord::new(1.0, 0.5)]])), vec![]);
        let line = m.add_dataset(d);
        let marks = rustgis_map::LineMarkers {
            mark: rustgis_map::LineMark::Marker(Marker::Char(Glyph::new('L', ""))),
            size: 20.0,
            interval: 40.0,
            ..Default::default()
        };
        m.layer_mut(line).unwrap().line_strokes =
            vec![rustgis_map::LineStroke { color: Some(black), markers: Some(marks), ..Default::default() }];
        m.layer_mut(id).unwrap().visible = false;
        let r = render(&mut m, &view).unwrap();
        let drawn = (0..200)
            .flat_map(|y| (0..200).map(move |x| (x, y)))
            .filter(|(x, y)| r.pixmap.pixel(*x, *y).is_some_and(|p| p.alpha() > 200 && p.red() < 100))
            .count();
        assert!(drawn > 40, "{drawn} px of characters along the line");
    }

    #[test]
    fn markers_and_hash_marks_draw_along_a_line() {
        use rustgis_map::{LineMark, LineMarkers, LineStroke, Placements};
        // A line from lon -1 to 1 at 1,500 m a pixel: x 25.8 to 174.2, along y 100.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("rail", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.0), Coord::new(1.0, 0.0)]])), vec![]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let red = Rgba::rgb(255, 0, 0);
        let draw = |m: &mut Map, markers: LineMarkers, offset: f32| {
            m.layer_mut(id).unwrap().line_strokes =
                vec![LineStroke { color: Some(red), width: Some(2.0), offset, markers: Some(markers), ..Default::default() }];
            render(m, &view).unwrap()
        };
        let is_red = |r: &Rendered, x: u32, y: u32| {
            let p = r.pixmap.pixel(x, y).unwrap();
            (p.red(), p.green(), p.blue()) == (255, 0, 0)
        };
        let squares = LineMarkers { mark: LineMark::Marker(Marker::Square), size: 8.0, interval: 30.0, ..LineMarkers::default() };
        // Every 30 pt from the start (x 25.8, 55.8, 85.8 …), nothing between, no line.
        let r = draw(&mut m, squares, 0.0);
        assert!([26, 56, 86, 116, 146].iter().all(|x| is_red(&r, *x, 100)));
        assert!([41, 71, 101, 131, 161].iter().all(|x| !is_red(&r, *x, 100)), "nothing between");
        // At the ends and the middle only.
        let places = LineMarkers { at: Placements::FIRST.with(Placements::LAST).with(Placements::MIDDLE), ..squares };
        let r = draw(&mut m, places, 0.0);
        assert!(is_red(&r, 26, 100) && is_red(&r, 100, 100) && is_red(&r, 174, 100));
        assert!(!is_red(&r, 56, 100) && !is_red(&r, 146, 100));
        // Shifted 10 pt to the right of the line's direction (down the screen, going east).
        let r = draw(&mut m, squares, 10.0);
        assert!(is_red(&r, 56, 110) && !is_red(&r, 56, 100));
        // Hash marks: across the line, 10 pt long, 2 pt wide.
        let hash = LineMarkers { mark: LineMark::Hash, size: 10.0, ..squares };
        let r = draw(&mut m, hash, 0.0);
        assert!(is_red(&r, 55, 96) && is_red(&r, 55, 104) && !is_red(&r, 60, 100) && !is_red(&r, 55, 108));
        // Selected: a halo around each marker.
        m.layer_mut(id).unwrap().selection = vec![0];
        let r = draw(&mut m, squares, 0.0);
        let p = r.pixmap.pixel(61, 100).unwrap();
        assert!(is_red(&r, 56, 100) && (p.red(), p.green(), p.blue()) != (255, 255, 255), "{p:?}");
        m.layer_mut(id).unwrap().selection.clear();
        // Zoomed far in on a line round the world, a mark every half point: only those that show
        // are visited (this would be 80 billion marks), and they draw.
        let mut d = Dataset::new("long", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-170.0, 0.0), Coord::new(170.0, 0.0)]])), vec![]);
        let long = m.add_dataset(d);
        m.layer_mut(id).unwrap().visible = false;
        let tiny = LineMarkers { size: 0.4, interval: 0.5, ..squares };
        m.layer_mut(long).unwrap().line_strokes = vec![LineStroke { color: Some(red), markers: Some(tiny), ..Default::default() }];
        let near = View { camera: Camera { res: 0.001, ..camera }, ..view };
        let r = render(&mut m, &near).unwrap();
        assert!((0..200).any(|x| r.pixmap.pixel(x, 100).unwrap().red() > 0), "marks show");
    }

    #[test]
    fn markers_on_the_globe_follow_the_line_up_to_the_horizon() {
        use rustgis_map::{LineMarkers, LineStroke};
        // A line from lon -120 over lon 10 to lon 120, seen over lon 10: both its ends are behind
        // the globe, its middle vertex alone shows, yet the parts up to the horizon on either side
        // show too, so their marks do.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        m.projection = rustgis_map::Projection::Globe;
        let mut d = Dataset::new("rail", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-120.0, 0.0), Coord::new(10.0, 0.0), Coord::new(120.0, 0.0)]])), vec![]);
        let id = m.add_dataset(d);
        let red = Rgba::rgb(255, 0, 0);
        let squares =
            LineMarkers { mark: rustgis_map::LineMark::Marker(Marker::Square), size: 6.0, interval: 20.0, ..LineMarkers::default() };
        m.layer_mut(id).unwrap().line_strokes = vec![LineStroke { color: Some(red), markers: Some(squares), ..Default::default() }];
        let camera = Camera { center: lonlat_to_merc(Coord::new(10.0, 0.0)), res: 40_000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 400.0, pixels_per_point: 1.0, camera, sky: false };
        let r = render(&mut m, &view).unwrap();
        let reds: Vec<u32> =
            (0..400).filter(|x| (0..400).any(|y| r.pixmap.pixel(*x, y).is_some_and(|p| p.red() > 200 && p.green() < 60))).collect();
        assert!(reds.len() > 20, "marks along the visible part: {reds:?}");
        // On both sides of the middle, from the horizon in.
        assert!(reds.iter().any(|x| *x < 150) && reds.iter().any(|x| *x > 250), "{reds:?}");
    }

    #[test]
    fn a_class_draws_its_own_stacked_strokes() {
        // Two lines of two Unique Values classes: "road" has a black casing and a red core of its
        // own, "path" takes the layer's single green stroke.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("lines", GeometryType::Polyline, vec![Field::new("kind", FieldType::Text)]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.3), Coord::new(1.0, 0.3)]])), vec![Value::Text("road".into())]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, -0.3), Coord::new(1.0, -0.3)]])), vec![Value::Text("path".into())]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let colour = |r: &Rendered, y: u32| {
            let p = r.pixmap.pixel(100, y).unwrap();
            (p.red(), p.green(), p.blue())
        };
        let (black, red, green) = (Rgba::rgb(0, 0, 0), Rgba::rgb(255, 0, 0), Rgba::rgb(0, 160, 0));
        let stroke = |c: Rgba, w: f32| rustgis_map::LineStroke { color: Some(c), width: Some(w), ..Default::default() };
        let l = m.layer_mut(id).unwrap();
        let s = Symbol::default_for(GeometryType::Polyline, 0);
        l.renderer = rustgis_map::Renderer::Unique {
            field: "kind".into(),
            classes: vec![("road".into(), s), ("path".into(), s)],
            other: s,
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: [("road".to_string(), vec![stroke(black, 9.0), stroke(red, 3.0)])].into_iter().collect(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        l.line_strokes = vec![stroke(green, 3.0)];
        // 0.3° is about 22 px from the centre at this resolution.
        let r = render(&mut m, &view).unwrap();
        let (road, path) = ((0..100).find(|y| colour(&r, *y) == (255, 0, 0)), (100..200).find(|y| colour(&r, *y) == (0, 160, 0)));
        let (Some(road), Some(path)) = (road, path) else { panic!("the road's red core and the path's green stroke draw") };
        assert_eq!(colour(&r, road - 2), (0, 0, 0), "the road's casing shows around its core");
        assert!((path - 1..=path + 3).all(|y| colour(&r, y) != (0, 0, 0)), "the path has no casing");
        // Selected, the road keeps its own stack under the halo.
        m.layer_mut(id).unwrap().selection = vec![0];
        let r = render(&mut m, &view).unwrap();
        assert_eq!(colour(&r, road + 1), (255, 0, 0));
        // Without the layer's strokes, the path draws its symbol's single line; the road keeps its own.
        m.layer_mut(id).unwrap().line_strokes.clear();
        m.layer_mut(id).unwrap().selection.clear();
        let r = render(&mut m, &view).unwrap();
        assert_eq!(colour(&r, road + 1), (255, 0, 0));
        assert_eq!(colour(&r, road - 2), (0, 0, 0));
        assert!((100..200).all(|y| colour(&r, y) != (0, 160, 0)), "no green left");
    }

    #[test]
    fn stroke_levels_join_casings_under_the_cores() {
        // One road in two segments meeting at the centre, a black casing under a red core. Drawn
        // a feature at a time, the second segment's round casing covers the end of the first's
        // core; in levels, every casing goes down first and the core runs through the joint.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("road", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.0), Coord::new(0.0, 0.0)]])), vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)]])), vec![]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let colour = |r: &Rendered, x: u32| {
            let p = r.pixmap.pixel(x, 100).unwrap();
            (p.red(), p.green(), p.blue())
        };
        let stroke = |c: Rgba, w: f32| rustgis_map::LineStroke { color: Some(c), width: Some(w), ..Default::default() };
        m.layer_mut(id).unwrap().line_strokes = vec![stroke(Rgba::rgb(0, 0, 0), 12.0), stroke(Rgba::rgb(255, 0, 0), 4.0)];
        let r = render(&mut m, &view).unwrap();
        assert_eq!(colour(&r, 96), (0, 0, 0), "the second casing cuts across the first core");
        assert_eq!(colour(&r, 60), (255, 0, 0));
        m.layer_mut(id).unwrap().stroke_levels = true;
        let r = render(&mut m, &view).unwrap();
        assert!((60..140).all(|x| colour(&r, x) == (255, 0, 0)), "the core runs through the joint");
        // A level set by hand: the core at level 0 and the casing at 1 bury the cores.
        let l = m.layer_mut(id).unwrap();
        l.line_strokes[0].level = Some(1);
        l.line_strokes[1].level = Some(0);
        let r = render(&mut m, &view).unwrap();
        assert_eq!(colour(&r, 60), (0, 0, 0));
    }

    #[test]
    fn a_line_cut_at_the_clip_box_keeps_its_dashes_where_they_were() {
        // A dashed line from far left of the image (beyond the clip box, where it's cut) across it:
        // drawn cut, its dashes lie where the whole line, drawn uncut, has them. So do an offset
        // stroke's, shifted beside it (straight, so the shifted line is as long as the line).
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 100.0, height: 20.0, pixels_per_point: 1.0, camera, sky: false };
        let ctx = Ctx {
            view: &view,
            ppp: 1.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        // 1,003.5 px of line before the image, not a whole number of the pattern's 7 px rounds.
        let line = [Coord::new(-1053.5, 0.0), Coord::new(60.0, 0.0)];
        let (mut pb, mut starts) = (PathBuilder::new(), DashStarts::default());
        ctx.ring_path_at(&mut pb, &line, false, &mut starts);
        let cut = pb.finish().unwrap();
        assert!(cut.bounds().left() > -200.0, "cut at the clip box: {:?}", cut.bounds());
        for offset in [0.0, 3.0] {
            let stroke = LineStroke {
                color: Some(Rgba::rgb(0, 0, 0)),
                width: Some(2.0),
                offset,
                cap: rustgis_map::LineCap::Butt,
                dash: rustgis_map::symbology::Dash::new(&[4.0, 3.0]),
                ..Default::default()
            };
            let s = Symbol::default_for(GeometryType::Polyline, 0);
            let mut drawn = Pixmap::new(100, 20).unwrap();
            crate::canvas::with_canvas(&mut drawn, |c| ctx.stacked_path(c, &cut, &s, &[stroke], &starts, &[]));
            let mut whole = Pixmap::new(100, 20).unwrap();
            let mut pb = PathBuilder::new();
            let (x0, y) = view.to_screen(line[0]);
            let (x1, _) = view.to_screen(line[1]);
            // Right of the line's direction: left to right, that's down the screen.
            let y = y as f32 + offset;
            pb.move_to(x0 as f32, y);
            pb.line_to(x1 as f32, y);
            let st =
                Stroke { width: 2.0, line_cap: LineCap::Butt, dash: tiny_skia::StrokeDash::new(vec![4.0, 3.0], 0.0), ..Stroke::default() };
            whole.stroke_path(&pb.finish().unwrap(), &paint(Rgba::rgb(0, 0, 0), 1.0), &st, Transform::identity(), None);
            let row = |pm: &Pixmap| (0..100).map(|x| pm.pixel(x, (y as u32).min(19)).unwrap().alpha() > 128).collect::<Vec<bool>>();
            assert!(row(&whole).iter().any(|d| *d) && row(&whole).iter().any(|d| !*d), "dashed");
            let differ = row(&drawn).iter().zip(row(&whole)).filter(|(a, b)| **a != *b).count();
            assert!(differ <= 2, "offset {offset}: {differ} pixels differ:\n{:?}\n{:?}", row(&drawn), row(&whole));
        }
    }

    #[test]
    fn a_winding_path_counts_its_length_against_the_dash_limit() {
        // A zigzag inside a 10-pixel square, 20,000 times across it: 200,000 pixels of line in
        // bounds whose span is 40.
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, 0.0);
        for i in 0..20_000 {
            pb.line_to(if i % 2 == 0 { 10.0 } else { 0.0 }, 0.0);
        }
        let zigzag = pb.finish().unwrap();
        assert!((path_length(&zigzag) - 200_000.0).abs() < 1.0);
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, 0.0);
        pb.line_to(30.0, 0.0);
        pb.line_to(30.0, 40.0);
        pb.close();
        assert!((path_length(&pb.finish().unwrap()) - 120.0).abs() < 1e-3);
        let view = View {
            width: 100.0,
            height: 100.0,
            pixels_per_point: 1.0,
            camera: Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let ctx = Ctx {
            view: &view,
            ppp: 1.0,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        // A 1-pixel period would cut it into 200,000 dashes: drawn solid. A 4-pixel one is fine.
        assert!(ctx.dash(&rustgis_map::symbology::Dash::new(&[0.5, 0.5]).unwrap(), &zigzag).is_none());
        assert!(ctx.dash(&rustgis_map::symbology::Dash::new(&[2.0, 2.0]).unwrap(), &zigzag).is_some());
        // Dashes count, not rounds of the pattern: four dashes a 4-pixel round is 200,000 too.
        let four = rustgis_map::symbology::Dash::new(&[0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]).unwrap();
        assert!(ctx.dash(&four, &zigzag).is_none());
    }

    #[test]
    fn a_dashed_stroke_leaves_gaps() {
        // A line across the view, one 4-point black stroke: solid it covers the middle of the row;
        // dashed 10 on, 10 off (flat caps), about half of it, in runs about 10 pixels long.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("road", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.0), Coord::new(1.0, 0.0)]])), vec![]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let stroke = rustgis_map::LineStroke {
            color: Some(Rgba::rgb(0, 0, 0)),
            width: Some(4.0),
            cap: rustgis_map::LineCap::Butt,
            ..Default::default()
        };
        let dark = |m: &mut Map| {
            let r = render(m, &view).unwrap();
            (40..160).map(|x| r.pixmap.pixel(x, 100).unwrap().red() < 128).collect::<Vec<bool>>()
        };
        m.layer_mut(id).unwrap().line_strokes = vec![stroke];
        assert!(dark(&mut m).iter().all(|d| *d), "solid");
        let dash = rustgis_map::symbology::Dash::new(&[10.0, 10.0]);
        m.layer_mut(id).unwrap().line_strokes = vec![rustgis_map::LineStroke { dash, ..stroke }];
        let row = dark(&mut m);
        let on = row.iter().filter(|d| **d).count();
        assert!((50..=70).contains(&on), "{on} of 120 pixels dark");
        let runs = row.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((10..=13).contains(&runs), "{runs} changes along the row");
        // A pattern under a pixel a round draws solid rather than cut into slivers.
        let dash = rustgis_map::symbology::Dash::new(&[0.2, 0.2]);
        m.layer_mut(id).unwrap().line_strokes = vec![rustgis_map::LineStroke { dash, ..stroke }];
        assert!(dark(&mut m).iter().all(|d| *d), "too fine: solid");
    }

    #[test]
    fn a_reference_scale_scales_lines_with_the_map() {
        // A 4-point black line: as wide as set without a reference scale or at it, twice as wide
        // at half the reference scale (zoomed in), half as wide at twice it.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("road", GeometryType::Polyline, vec![]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.0), Coord::new(1.0, 0.0)]])), vec![]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        m.camera = camera;
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let base = Symbol::default_for(GeometryType::Polyline, 0);
        m.layer_mut(id).unwrap().renderer = rustgis_map::Renderer::Single(Symbol { stroke: Rgba::rgb(0, 0, 0), stroke_width: 4.0, ..base });
        let width = |m: &mut Map| {
            let r = render(m, &view).unwrap();
            (0..200).filter(|&y| r.pixmap.pixel(100, y).unwrap().red() < 128).count()
        };
        let plain = width(&mut m);
        assert!((3..=5).contains(&plain), "{plain}");
        let scale = m.scale();
        for (reference, want) in [(scale, plain), (scale * 2.0, plain * 2), (scale / 2.0, plain / 2)] {
            m.layer_mut(id).unwrap().reference_scale = Some(reference);
            let got = width(&mut m);
            assert!(got.abs_diff(want) <= 1, "reference 1:{reference:.0} at 1:{scale:.0}: {got} px, not about {want}");
        }
        // The GPU path's meshes carry the same widths.
        let gpu_width = |m: &mut Map| {
            let Some(RenderOutput::Gpu(scene)) = render_backend(m, &view, Backend::Gpu) else { panic!("gpu scene") };
            let widths = scene.cmds.iter().flat_map(|c| match c {
                gpu_scene::DrawCmd::Mesh { vertices, .. } => vertices.iter().map(|v| v.stroke_px).collect(),
                _ => Vec::new(),
            });
            widths.fold(0.0f32, f32::max)
        };
        m.layer_mut(id).unwrap().reference_scale = None;
        let plain = gpu_width(&mut m);
        assert!(plain > 0.0, "the line is meshed");
        for (reference, k) in [(scale, 1.0), (scale * 2.0, 2.0), (scale / 2.0, 0.5)] {
            m.layer_mut(id).unwrap().reference_scale = Some(reference);
            assert!((gpu_width(&mut m) - plain * k).abs() < 1e-3, "gpu at reference 1:{reference:.0}");
        }
    }

    #[test]
    fn labels_run_along_lines_straight_or_curved() {
        // A road rising to the right, labelled along it: straight and turned to it, or a
        // character at a time; drawn either way, and a too sharp bend leaves a curved label out.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("roads", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, -1.0), Coord::new(1.0, 1.0)]])), vec![Value::Text("Seestrasse".into())]);
        // A right angle at its middle.
        d.push(
            Some(Geometry::Line(vec![vec![Coord::new(-1.0, -1.6), Coord::new(0.6, -1.6), Coord::new(0.6, 0.0)]])),
            vec![Value::Text("Bahnhofstrasse".into())],
        );
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, -0.3)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 400.0, pixels_per_point: 1.0, camera, sky: false };
        let l = m.layer_mut(id).unwrap();
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        l.labels.default.size = 12.0;
        l.labels.default.placement = rustgis_map::LabelPlacement::Parallel;
        let r = render(&mut m, &view).unwrap();
        let along = r.labels.iter().find(|l| l.text == "Seestrasse").expect("placed");
        // Up and to the right on the screen: −45°, about the middle of the line.
        assert!(along.angle.is_some_and(|a| (a + std::f32::consts::FRAC_PI_4).abs() < 0.02), "{:?}", along.angle);
        assert!(along.glyphs.is_empty());
        m.layer_mut(id).unwrap().labels.default.placement = rustgis_map::LabelPlacement::Curved;
        let r = render(&mut m, &view).unwrap();
        let curved = r.labels.iter().find(|l| l.text == "Seestrasse").expect("placed");
        assert_eq!(curved.glyphs.iter().map(|g| g.text.as_str()).collect::<String>(), "Seestrasse");
        assert!(curved.glyphs.windows(2).all(|w| w[1].dx > w[0].dx && w[1].dy < w[0].dy), "left to right, rising");
        assert!(r.labels.iter().all(|l| l.text != "Bahnhofstrasse"), "the bend is too sharp to curve round");
        // Drawn: dark text along the line, and a square to collide with per character.
        let mut pm = Pixmap::new(400, 400).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| text::draw(c, curved, 1.0));
        let dark = pm.pixels().iter().filter(|p| p.alpha() > 200 && p.red() < 120).count();
        assert!(dark > 30, "{dark} text pixels");
        assert_eq!(label_boxes(curved).0.len(), 10);
    }

    #[test]
    fn a_line_mostly_off_screen_is_labelled_along_its_stretch_in_view() {
        // A river reaching far off to the east, its middle well out of view, and a road whose
        // longer part is out of view too: both labelled on what is on the screen, and the
        // river's accent (a combining one) kept on its letter.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("rivers", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.5, 0.5), Coord::new(30.0, 0.5)]])), vec![Value::Text("Rho\u{302}ne".into())]);
        d.push(
            Some(Geometry::Line(vec![
                vec![Coord::new(-1.5, -1.0), Coord::new(1.5, -1.0)],
                vec![Coord::new(10.0, -1.0), Coord::new(20.0, -1.0)],
            ])),
            vec![Value::Text("Seestrasse".into())],
        );
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, -0.3)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 400.0, pixels_per_point: 1.0, camera, sky: false };
        let l = m.layer_mut(id).unwrap();
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        l.labels.default.size = 12.0;
        l.labels.default.placement = rustgis_map::LabelPlacement::Curved;
        let r = render(&mut m, &view).unwrap();
        for name in ["Rho\u{302}ne", "Seestrasse"] {
            let l = r.labels.iter().find(|l| l.text == name).unwrap_or_else(|| panic!("{name} placed"));
            assert!((0.0..400.0).contains(&l.x) && (0.0..400.0).contains(&l.y), "{name} in view: {} {}", l.x, l.y);
        }
        let river = r.labels.iter().find(|l| l.text.starts_with("Rho")).unwrap();
        let glyphs: Vec<&str> = river.glyphs.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(glyphs, ["R", "h", "o\u{302}", "n", "e"]);
    }

    #[test]
    fn a_curved_label_turned_upside_down_reads_from_its_other_end() {
        // "AB" along a level line, then the map turned half round: the characters swap ends and
        // stay upright.
        let glyph = |text: &str, dx: f32| LabelGlyph { text: text.into(), dx, dy: 0.0, angle: 0.0 };
        let mut l = PlacedLabel {
            text: "AB".into(),
            x: 100.0,
            y: 100.0,
            size: 10.0,
            color: Rgba::rgb(0, 0, 0),
            halo: None,
            halo_width: None,
            bold: false,
            icon: None,
            align: 0.0,
            framed: false,
            italic: false,
            spacing: 0.0,
            font: None,
            line_align: TextAlign::Left,
            angle: None,
            collide: Default::default(),
            glyphs: vec![glyph("A", -5.0), glyph("B", 5.0)],
            background: None,
        };
        turn_with(&mut l, &|x, y| Some((200.0 - x, 200.0 - y)));
        let order: Vec<(&str, f32)> = l.glyphs.iter().map(|g| (g.text.as_str(), g.dx)).collect();
        assert_eq!(order.iter().map(|g| g.0).collect::<Vec<_>>(), ["A", "B"]);
        assert!(order[0].1 < order[1].1, "A on the left, read first: {order:?}");
        assert!(l.glyphs.iter().all(|g| g.angle.cos() > 0.99), "upright");
        // Each placed by its own width: "Wi" turned round keeps "i" beside "W", not a "W" apart.
        let (w, i) = (text::measure("W", 10.0, false), text::measure("i", 10.0, false));
        l.glyphs = vec![glyph("W", -i / 2.0), glyph("i", w / 2.0)];
        turn_with(&mut l, &|x, y| Some((200.0 - x, 200.0 - y)));
        let (gw, gi) = (&l.glyphs[0], &l.glyphs[1]);
        assert_eq!((gw.text.as_str(), gi.text.as_str()), ("W", "i"));
        assert!(((gi.dx - gw.dx) - (w + i) / 2.0).abs() < 1e-3, "{gw:?} {gi:?}");
        assert!((gw.dx + i / 2.0).abs() < 1e-3, "W first, at the left end: {gw:?}");
    }

    #[test]
    fn a_curved_labels_halos_go_under_all_its_letters() {
        // Two letters close together along a line, with a halo: the second one's halo does not
        // cover the first one's strokes, so the letters are as dark as drawn without halos. Large
        // enough that even a thin system font's strokes (Segoe UI, SF) have solid middles.
        let glyph = |dx: f32| LabelGlyph { text: "l".into(), dx, dy: 0.0, angle: 0.0 };
        let label = |halo: Option<Rgba>, glyphs: Vec<LabelGlyph>| PlacedLabel {
            text: "ll".into(),
            x: 30.0,
            y: 40.0,
            size: 30.0,
            color: Rgba::rgb(0, 0, 0),
            halo,
            halo_width: None,
            bold: false,
            icon: None,
            align: 0.0,
            framed: false,
            italic: false,
            spacing: 0.0,
            font: None,
            line_align: TextAlign::Left,
            angle: None,
            collide: Default::default(),
            glyphs,
            background: None,
        };
        let drawn = |halo, glyphs| {
            let mut pm = Pixmap::new(60, 80).unwrap();
            pm.fill(tiny_skia::Color::from_rgba8(128, 128, 128, 255));
            crate::canvas::with_canvas(&mut pm, |c| text::draw(c, &label(halo, glyphs), 1.0));
            pm
        };
        let pair = || vec![glyph(-1.0), glyph(1.0)];
        let (plain, haloed) = (drawn(None, pair()), drawn(Some(Rgba::rgb(255, 255, 255)), pair()));
        // Both letters are drawn: the pair's ink is wider than one letter's.
        let inked = |pm: &Pixmap| (0..60).filter(|&x| (0..80).any(|y| pm.pixel(x, y).is_some_and(|p| p.red() < 40))).count();
        let one = drawn(None, vec![glyph(-1.0)]);
        assert!(inked(&plain) > inked(&one), "two letters, not one: {} vs {}", inked(&plain), inked(&one));
        let mut letters = 0;
        for (a, b) in plain.pixels().iter().zip(haloed.pixels()) {
            if a.red() < 40 {
                letters += 1;
                assert!(b.red() < 80, "a letter's pixel covered by a halo: {} → {}", a.red(), b.red());
            }
        }
        assert!(letters > 5, "the letters are drawn: {letters}");
    }

    #[test]
    fn a_reference_scale_scales_labels_and_their_gap() {
        // A point labelled to its right: at twice the size, the label is twice as large and twice
        // as far from the point, the gap between them included.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("towns", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("Winterthur".into())]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        m.camera = camera;
        let view = View { width: 400.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let l = m.layer_mut(id).unwrap();
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        l.labels.default.placement = rustgis_map::LabelPlacement::Right;
        let label = |m: &mut Map| {
            let r = render(m, &view).unwrap();
            let l = r.labels.first().expect("placed");
            (l.x - 200.0, l.size)
        };
        let (dx, size) = label(&mut m);
        assert!(dx > 0.0, "{dx}");
        let scale = m.scale();
        m.layer_mut(id).unwrap().reference_scale = Some(scale * 2.0);
        let (dx2, size2) = label(&mut m);
        assert!((size2 - size * 2.0).abs() < 1e-3 && (dx2 - dx * 2.0).abs() < 0.5, "{dx} {size} → {dx2} {size2}");
        // A halo's own colour and width, and the letter spacing, grow with the label too.
        let c = &mut m.layer_mut(id).unwrap().labels.default;
        (c.halo_color, c.halo_width, c.spacing) = (Some(Rgba::rgb(255, 0, 0)), Some(1.5), 1.0);
        let r = render(&mut m, &view).unwrap();
        let l = r.labels.first().expect("placed");
        assert_eq!((l.halo, l.halo_width, l.spacing), (Some(Rgba::rgb(255, 0, 0)), Some(3.0), 2.0));
        // Without them, as every label was drawn before: the automatic colour, thin, unspaced.
        let c = &mut m.layer_mut(id).unwrap().labels.default;
        (c.halo_color, c.halo_width, c.spacing) = (None, None, 0.0);
        let r = render(&mut m, &view).unwrap();
        let l = r.labels.first().expect("placed");
        assert_eq!((l.halo, l.halo_width, l.spacing), (Some(halo_for(c_color(&m, id))), None, 0.0));
    }

    fn c_color(m: &Map, id: u32) -> Rgba {
        m.layer(id).unwrap().labels.default.color
    }

    #[test]
    fn a_halo_of_its_own_width_spreads_round_the_letters_in_one_coat() {
        let label = |halo: Rgba, halo_width: Option<f32>| PlacedLabel {
            text: "H".into(),
            x: 50.0,
            y: 50.0,
            size: 20.0,
            color: Rgba::rgb(0, 0, 0),
            halo: Some(halo),
            halo_width,
            bold: false,
            icon: None,
            align: 0.0,
            framed: false,
            italic: false,
            spacing: 0.0,
            font: None,
            line_align: TextAlign::Left,
            angle: None,
            collide: Default::default(),
            glyphs: Vec::new(),
            background: None,
        };
        let drawn = |l: &PlacedLabel| {
            let mut pm = Pixmap::new(100, 100).unwrap();
            pm.fill(tiny_skia::Color::from_rgba8(128, 128, 128, 255));
            crate::canvas::with_canvas(&mut pm, |c| text::draw(c, l, 1.0));
            pm
        };
        // The leftmost column with a pixel matching `f`.
        let left = |pm: &Pixmap, f: &dyn Fn(tiny_skia::PremultipliedColorU8) -> bool| {
            (0..100).find(|&x| (0..100).any(|y| pm.pixel(x, y).is_some_and(f))).unwrap() as i32
        };
        let red = Rgba::new(255, 0, 0, 128);
        let thin = drawn(&label(red, None));
        let wide = drawn(&label(red, Some(3.0)));
        let ink = left(&wide, &|p| p.red() < 40 && p.green() < 40);
        let reach = ink - left(&wide, &|p| p.red() > 160 && p.green() < 100);
        // 3 pt beyond the letters: 4 pixels, as a 20 pt text is 26.6 pixels tall.
        assert!((3..=5).contains(&reach), "{reach}");
        assert!(ink - left(&thin, &|p| p.red() > 160 && p.green() < 100) <= 2, "the thin halo as before");
        // One coat of the see-through red over the grey: nowhere redder than one coat makes.
        let one_coat = (255.0 * 0.5 + 128.0 * 0.5_f32).round() as u8 + 2;
        assert!(wide.pixels().iter().all(|p| p.red() <= one_coat), "a pixel coated twice");
        // A width grown with its label at a reference scale isn't cut back, whatever the text's
        // size (a 10 pt halo on 9 pt text at twice the size: 20 pt); past the most a reference
        // scale grows a 10 pt width, and for no number, it is held.
        assert_eq!(
            (text::halo_reach(20.0), text::halo_reach(1e9), text::halo_reach(f32::NAN), text::halo_reach(-3.0)),
            (20.0, 320.0, 0.0, 0.0)
        );
        // No width, no halo: not even round the letters' edges.
        assert_eq!(drawn(&label(red, Some(0.0))).data(), drawn(&PlacedLabel { halo: None, ..label(red, None) }).data());
        // A spaced label is drawn with its letters farther apart.
        let two = |spacing: f32| drawn(&PlacedLabel { text: "II".into(), spacing, halo: None, ..label(red, None) });
        let span = |pm: &Pixmap| {
            let cols: Vec<u32> = (0..100).filter(|&x| (0..100).any(|y| pm.pixel(x, y).is_some_and(|p| p.red() < 40))).collect();
            cols.last().unwrap() - cols.first().unwrap()
        };
        let (tight, spaced) = (span(&two(0.0)), span(&two(4.0)));
        assert!(spaced >= tight + 4, "{tight} → {spaced}");
    }

    #[test]
    fn a_background_fills_behind_the_text_and_keeps_other_labels_off() {
        let bg = rustgis_map::LabelBackground {
            fill: Some(Rgba::rgb(255, 255, 0)),
            stroke: Some(Rgba::rgb(0, 0, 255)),
            stroke_width: 1.5,
            padding: [6.0, 3.0],
            radius: 0.0,
        };
        let label = |background: Option<rustgis_map::LabelBackground>| PlacedLabel {
            text: "HH".into(),
            x: 60.0,
            y: 50.0,
            size: 12.0,
            color: Rgba::rgb(0, 0, 0),
            halo: None,
            halo_width: None,
            bold: false,
            icon: None,
            align: 0.0,
            framed: false,
            italic: false,
            spacing: 0.0,
            font: None,
            line_align: TextAlign::Left,
            angle: None,
            collide: Default::default(),
            glyphs: Vec::new(),
            background,
        };
        let drawn = |l: &PlacedLabel| {
            let mut pm = Pixmap::new(120, 100).unwrap();
            pm.fill(tiny_skia::Color::from_rgba8(128, 128, 128, 255));
            crate::canvas::with_canvas(&mut pm, |c| text::draw(c, l, 1.0));
            pm
        };
        let yellow = |p: tiny_skia::PremultipliedColorU8| p.red() > 230 && p.green() > 230 && p.blue() < 40;
        let blue = |p: tiny_skia::PremultipliedColorU8| p.blue() > 200 && p.red() < 60;
        let l = label(Some(bg.clone()));
        let [x0, y0, x1, y1] = text::background_box(&l).unwrap();
        // The text's block grown by the padding (points × 1.33, as the text's size).
        let w = text::label_width(&l);
        assert!((x1 - x0 - (w + 2.0 * 6.0 * 1.33)).abs() < 0.01, "{x0} {x1} {w}");
        assert!(y1 - y0 > 12.0 * 1.33 + 2.0 * 3.0 * 1.33);
        let pm = drawn(&l);
        // Filled between the text's end and its edge, outlined at the edge, the text over it.
        let inside = pm.pixel((x1 - 4.0) as u32, 50).unwrap();
        assert!(yellow(inside), "{inside:?}");
        assert!(blue(pm.pixel(x1.round() as u32, 50).unwrap()), "{:?}", pm.pixel(x1.round() as u32, 50));
        assert!(pm.pixels().iter().any(|p| p.red() < 100 && p.green() < 100 && p.blue() < 100), "the text over it");
        // None, and one that draws nothing: the text alone, as before.
        let plain = drawn(&label(None));
        assert!(!plain.pixels().iter().any(|p| yellow(*p)));
        let nothing = rustgis_map::LabelBackground { fill: None, stroke: None, ..bg.clone() };
        assert_eq!(drawn(&label(Some(nothing.clone()))).data(), plain.data());
        assert_eq!(text::background_reach(&label(Some(nothing))), [0.0; 2]);
        // Turned along a line, it turns with the text.
        let turned = drawn(&PlacedLabel { angle: Some(0.6), ..l.clone() });
        // Its middle is the background, the text or the text's edge blended over it (whose glyphs
        // fall there depends on the platform's font): never the grey map.
        assert!(turned.pixel(60, 50).is_some_and(|p| p.blue() < 40), "{:?}", turned.pixel(60, 50));
        assert!(turned.pixels().iter().filter(|p| yellow(**p)).count() > 100);
        // Curved character by character: none.
        let glyph = LabelGlyph { text: "H".into(), dx: 0.0, dy: 0.0, angle: 0.0 };
        let curved = PlacedLabel { glyphs: vec![glyph.clone(), LabelGlyph { dx: 10.0, ..glyph }], ..l.clone() };
        assert!(text::background_box(&curved).is_none());
        assert!(!drawn(&curved).pixels().iter().any(|p| yellow(*p)));
        // Its reach widens the box other labels keep off: a label just past the text but on the
        // background is placed beside the plain one, not beside the one with a background.
        let [reach, up_down] = text::background_reach(&l);
        assert!((reach - (6.0 * 1.33 + 0.75 * 1.33)).abs() < 0.01, "{reach}");
        let (plain_box, _) = label_boxes(&label(None));
        let (bg_box, _) = label_boxes(&l);
        assert!((plain_box[0][0] - bg_box[0][0] - reach).abs() < 0.01, "{plain_box:?} {bg_box:?}");
        // Up and down only as far as it reaches there: less padding than across, less kept off.
        assert!(up_down < reach && (plain_box[0][1] - bg_box[0][1] - up_down).abs() < 0.01, "{up_down} {plain_box:?} {bg_box:?}");
        let beside = |first: PlacedLabel| {
            // A point clear of the two labels' padding (2 points each), not on its edge, where
            // fonts' rounding decides.
            let next = PlacedLabel { x: first.x + w + 5.0, ..label(None) };
            place_labels(vec![(first, 2.0, (0, 0, 0, 0)), (next, 1.0, (0, 1, 0, 0))], Vec::new(), 400.0, 100.0, false).labels.len()
        };
        assert_eq!((beside(label(None)), beside(l.clone())), (2, 1));
    }

    /// A line's label repeated along it: the one at its middle and more either side, each its own
    /// label (in a tilted view too, and under a lock only those placed come back); wrapped text
    /// in lines lined up as its class says, but not curved character by character.
    #[test]
    fn labels_repeat_along_lines_and_wrap_into_lines() {
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("rivers", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-2.0, 0.0), Coord::new(2.0, 0.0)]])), vec![Value::Text("Töss".into())]);
        let line = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        m.camera = camera;
        let view = View { width: 400.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let l = m.layer_mut(line).unwrap();
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        l.labels.default.placement = rustgis_map::LabelPlacement::Parallel;
        let xs = |m: &mut Map, view: &View| {
            let mut xs: Vec<f32> = render(m, view).unwrap().labels.iter().filter(|l| l.text == "Töss").map(|l| l.x).collect();
            xs.sort_by(f32::total_cmp);
            xs
        };
        // Once, at the middle, as always.
        let once = xs(&mut m, &view);
        assert_eq!(once.len(), 1);
        assert!((once[0] - 200.0).abs() < 1.0, "{once:?}");
        // Every 60 points (80 logical): the middle one where it was, and more either side.
        m.layer_mut(line).unwrap().labels.default.repeat = Some(60.0);
        let many = xs(&mut m, &view);
        assert!(many.len() >= 3, "{many:?}");
        assert!(many.iter().any(|x| (x - 200.0).abs() < 1.0), "{many:?}");
        assert!(many.windows(2).all(|w| (w[1] - w[0] - 80.0).abs() < 1.0), "{many:?}");
        // In a tilted view each is a label of its own, not one collapsed with the others.
        let tilted = View { camera: Camera { pitch: 30.0, ..camera }, ..view };
        assert!(xs(&mut m, &tilted).len() >= 2, "{:?}", xs(&mut m, &tilted));
        // Locked with some placed: only those come back (each repeat has its own key).
        m.label_display.locked = true;
        let placed = xs(&mut m, &view);
        let keys = m.label_lock.clone().unwrap();
        assert_eq!(keys.len(), placed.len());
        assert!(keys.iter().map(|(k, _)| k.3).collect::<std::collections::HashSet<_>>().len() == keys.len(), "{keys:?}");
        m.label_lock = Some(keys.into_iter().filter(|(k, _)| k.3 == 0).collect());
        assert_eq!(xs(&mut m, &view).len(), 1, "the middle one only");
        m.label_display.locked = false;
        m.label_lock = None;
        // Wrapped at its spaces, centred; not curved character by character.
        m.layer_mut(line).unwrap().dataset.features = vec![rustgis_data::Feature {
            id: 0,
            geometry: Some(Geometry::Line(vec![vec![Coord::new(-2.0, 0.0), Coord::new(2.0, 0.0)]])),
            attrs: vec![Value::Text("Obere Töss".into())],
        }]
        .into();
        m.layer_mut(line).unwrap().touch_geometry();
        let c = &mut m.layer_mut(line).unwrap().labels.default;
        (c.repeat, c.wrap_char, c.line_align) = (None, Some(" ".into()), TextAlign::Center);
        let r = render(&mut m, &view).unwrap();
        let label = r.labels.iter().find(|l| l.text.contains("Töss")).unwrap();
        assert_eq!((label.text.as_str(), label.line_align), ("Obere\nTöss", TextAlign::Center));
        m.layer_mut(line).unwrap().labels.default.placement = rustgis_map::LabelPlacement::Curved;
        let r = render(&mut m, &view).unwrap();
        assert!(r.labels.iter().any(|l| l.text == "Obere Töss" && !l.glyphs.is_empty()), "curved in one line");
    }

    #[test]
    fn an_offset_moves_a_point_label_but_not_one_along_a_line() {
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("towns", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("Winterthur".into())]);
        let id = m.add_dataset(d);
        let mut d = Dataset::new("rivers", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.5), Coord::new(1.0, 0.5)]])), vec![Value::Text("Töss".into())]);
        let line = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        m.camera = camera;
        let view = View { width: 400.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        for (layer, placement) in [(id, rustgis_map::LabelPlacement::Right), (line, rustgis_map::LabelPlacement::Parallel)] {
            let l = m.layer_mut(layer).unwrap();
            l.labels.enabled = true;
            l.labels.default.field = "name".into();
            l.labels.default.placement = placement;
        }
        let at = |m: &mut Map| {
            let r = render(m, &view).unwrap();
            let find = |t: &str| r.labels.iter().find(|l| l.text == t).map(|l| (l.x, l.y)).unwrap();
            (find("Winterthur"), find("Töss"))
        };
        let (town, river) = at(&mut m);
        for layer in [id, line] {
            m.layer_mut(layer).unwrap().labels.default.offset = [6.0, -3.0];
        }
        let (town2, river2) = at(&mut m);
        // Points × 1.33, as the text's size: right and up.
        assert!((town2.0 - town.0 - 6.0 * 1.33).abs() < 0.01 && (town2.1 - town.1 + 3.0 * 1.33).abs() < 0.01, "{town:?} → {town2:?}");
        assert_eq!(river, river2, "along a line, not moved");
        // A reference scale grows it with the label.
        let scale = m.scale();
        m.layer_mut(id).unwrap().reference_scale = Some(scale * 2.0);
        m.layer_mut(id).unwrap().labels.default.offset = [0.0; 2];
        let (big, _) = at(&mut m);
        m.layer_mut(id).unwrap().labels.default.offset = [6.0, -3.0];
        let (big2, _) = at(&mut m);
        assert!((big2.0 - big.0 - 2.0 * 6.0 * 1.33).abs() < 0.01, "{big:?} → {big2:?}");
        // A point 30 points left of the view (past the culling margin) is labelled when its offset
        // brings the label into it.
        let mut d = Dataset::new("edge", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(-3.1, 0.0))), vec![Value::Text("Elgg".into())]);
        let edge = m.add_dataset(d);
        let l = m.layer_mut(edge).unwrap();
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        let shown = |m: &mut Map| render(m, &view).unwrap().labels.iter().find(|l| l.text == "Elgg").map(|l| l.x);
        assert_eq!(shown(&mut m), None);
        m.layer_mut(edge).unwrap().labels.default.offset = [100.0, 0.0];
        assert!(shown(&mut m).is_some_and(|x| x > 0.0 && x < 400.0), "{:?}", shown(&mut m));
    }

    #[test]
    fn features_under_a_pixel_still_show() {
        // A dense grid of tiny squares seen whole: each is splatted rather than filled as a path,
        // and the area still comes out the colour of their outlines.
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("cells", GeometryType::Polygon, vec![]);
        for i in 0..100 {
            for j in 0..100 {
                let (x, y) = (i as f64 * 0.01, j as f64 * 0.01);
                let ring = vec![
                    Coord::new(x, y),
                    Coord::new(x + 0.005, y),
                    Coord::new(x + 0.005, y + 0.005),
                    Coord::new(x, y + 0.005),
                    Coord::new(x, y),
                ];
                d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
            }
        }
        let id = m.add_dataset(d);
        let sym = m.layer(id).map(|l| l.symbol(0)).unwrap();
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.5, 0.5)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let r = render(&mut m, &view).unwrap();
        let px = |x: u32, y: u32| r.pixmap.pixel(x, y).unwrap();
        let (centre, corner) = (px(100, 100), px(2, 2));
        assert_ne!((centre.red(), centre.green(), centre.blue()), (corner.red(), corner.green(), corner.blue()));
        // Towards the outline's colour (darker than the blank basemap).
        let lum = |p: tiny_skia::PremultipliedColorU8| p.red() as u32 + p.green() as u32 + p.blue() as u32;
        assert!(lum(centre) < lum(corner), "{centre:?} vs {corner:?} ({sym:?})");
    }

    /// The globe and the tilted view on another body: no atmosphere glows past the limb, and the
    /// tilted sky is the black of space (#223).
    #[test]
    fn other_bodies_have_no_atmosphere() {
        let glow_at = |body: &str| {
            let mut m = Map::new("Map");
            m.body = body.into();
            m.graticule = false;
            m.projection = rustgis_map::Projection::Globe;
            let camera = Camera { center: Coord::new(0.0, 0.0), res: 60_000.0, bearing: 0.0, pitch: 0.0 };
            let view = View { width: 400.0, height: 400.0, pixels_per_point: 1.0, camera, sky: true };
            let g = Globe::new(&camera, 400.0, 400.0);
            let (cx, cy) = g.center();
            let pm = render(&mut m, &view).unwrap().pixmap;
            let p = pm.pixel((cx + g.disc_radius() * 1.03) as u32, cy as u32).unwrap();
            (p.red(), p.green(), p.blue())
        };
        let space = globe_space(true);
        assert_ne!(glow_at("earth"), (space.r(), space.g(), space.b()));
        assert_eq!(glow_at("moon"), (space.r(), space.g(), space.b()));
        let mut m = Map::new("Map");
        m.body = "mars".into();
        assert_eq!(tilt_sky(&m), (space, space));
    }
}

#[cfg(test)]
mod stream_tests;

#[cfg(test)]
mod budget_tests;

#[cfg(test)]
mod offset_normal_tests {
    use super::offset_polyline;

    /// Reusing a normal must preserve the previous algorithm's exact output, including signed zero.
    #[test]
    fn reused_normals_match_reference_bits() {
        let mut cases = vec![
            vec![],
            vec![(1.0, 2.0)],
            vec![(0.0, 0.0), (0.0, 0.0)],
            vec![(-0.0, 0.0), (10.0, -0.0)],
            vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 0.0)],
            vec![(0.0, 0.0), (0.0, 0.0), (0.00001, 0.0), (3.0, 4.0)],
            vec![(0.0, 0.0), (0.001, 0.0), (0.00101, 0.0), (3.0, 4.0)],
            vec![(0.0, 0.0), (10.0, 0.0), (0.01, 0.01), (-5.0, 8.0)],
            vec![(f32::NAN, 0.0), (0.0, 0.0), (3.0, 4.0), (f32::INFINITY, 0.0)],
            vec![(40_500_000.0, 3_500_000.0), (40_500_020.0, 3_500_040.0)],
            vec![(-f32::MAX, 0.0), (f32::MAX, 1.0), (0.0, -f32::MAX)],
        ];
        for variant in 0..32 {
            let path: Vec<_> = (0..80)
                .map(|i| {
                    let x = (i as f32 * 0.37 + variant as f32).sin() * 100.0;
                    let y = (i as f32 * 0.13 - variant as f32).cos() * 100.0;
                    (x, y)
                })
                .collect();
            cases.push(path.iter().rev().copied().collect());
            cases.push(path);
        }
        for (case, points) in cases.iter().enumerate() {
            for d in [-12.0, -0.0, 0.0, 0.25, 9.0] {
                let bits = |p: Vec<(f32, f32)>| p.into_iter().map(|(x, y)| (x.to_bits(), y.to_bits())).collect::<Vec<_>>();
                assert_eq!(bits(offset_polyline(points, d)), bits(reference_offset_polyline(points, d)), "case={case} offset={d}");
            }
        }
        println!("normal cases: {} paths x 5 offsets", cases.len());
    }

    /// Frozen pre-optimization oracle; keep independent of the optimized implementation.
    fn reference_offset_polyline(pts: &[(f32, f32)], d: f32) -> Vec<(f32, f32)> {
        let mut p: Vec<(f32, f32)> = Vec::with_capacity(pts.len());
        for &q in pts.iter().filter(|q| q.0.is_finite() && q.1.is_finite()) {
            if p.last().is_none_or(|l: &(f32, f32)| (l.0 - q.0).hypot(l.1 - q.1) > 1e-3) {
                p.push(q);
            }
        }
        // The unit normal to the right of a → b.
        let normal = |a: (f32, f32), b: (f32, f32)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l = dx.hypot(dy);
            (-dy / l, dx / l)
        };
        let mut out = Vec::with_capacity(p.len() + 4);
        let (Some(&[a, b]), Some(&[y, z])) = (p.first_chunk::<2>(), p.last_chunk::<2>()) else { return out };
        let n = normal(a, b);
        out.push((a.0 + n.0 * d, a.1 + n.1 * d));
        for w in p.windows(3) {
            let [a, v, b] = [w.first(), w.get(1), w.get(2)].map(|x| x.copied().unwrap_or_default());
            let (na, nb) = (normal(a, v), normal(v, b));
            let (sx, sy) = (na.0 + nb.0, na.1 + nb.1);
            let s2 = sx * sx + sy * sy;
            if s2 >= 0.25 {
                // The mitre: along the bisector, 2d / |na + nb|² times na + nb.
                let k = 2.0 * d / s2;
                out.push((v.0 + sx * k, v.1 + sy * k));
            } else {
                out.push((v.0 + na.0 * d, v.1 + na.1 * d));
                out.push((v.0 + nb.0 * d, v.1 + nb.1 * d));
            }
        }
        let n = normal(y, z);
        out.push((z.0 + n.0 * d, z.1 + n.1 * d));
        out
    }
}

#[cfg(test)]
mod lod_tests;

#[cfg(test)]
mod line_positions_tests;
