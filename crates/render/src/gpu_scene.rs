//! GPU-ready scene types: a `GpuScene` is what `render_backend(.., Backend::Gpu)` returns —
//! an ordered list of draw commands (painter's algorithm, no depth) that `ui-egui`'s `mapgpu`
//! module uploads to wgpu buffers and draws inside egui's render pass.
//!
//! World positions are f32 offsets from each command's `origin` (map units): the f64
//! subtraction happens on the CPU so city-zoom precision survives the f32 upload.

use std::sync::Arc;

use crate::PlacedLabel;
use rustgis_map::Camera;
use rustgis_tiles::cache::Payload;

/// How many physical pixels from its origin a vertex may lie: an f32 holds 24 bits, and
/// rounding the vertex and the shader's sum each cost up to half a unit in the last place, so
/// within 2^23 px the error stays under a pixel. Zoomed in further, the renderer draws such
/// features on the CPU (#902), and a scene's meshes are not moved past it ([`reach`]).
pub const F32_REACH_PX: f64 = 8_388_608.0;

/// Whether vertices up to `reach` map units from their origin land within a pixel of where they
/// belong at `map_per_px` map units per physical pixel.
pub fn f32_holds(reach: f64, map_per_px: f64) -> bool {
    reach / map_per_px <= F32_REACH_PX
}

/// The farthest any vertex of `cmds` lies from its command's origin, in map units: how far in
/// the scene can be zoomed before f32 moves its vertices (see [`f32_holds`]). Raster and
/// vector tiles are left out: each sits on its own tile's corner.
pub fn reach(cmds: &[DrawCmd]) -> f64 {
    let far = |p: &[f32]| p.iter().fold(0f64, |m, v| m.max(f64::from(v.abs())));
    cmds.iter()
        .map(|c| match c {
            DrawCmd::Mesh { vertices, .. } => vertices.iter().map(|v| far(&v.position)).fold(0.0, f64::max),
            DrawCmd::Points { instances, .. } => instances.iter().map(|p| far(&p.position)).fold(0.0, f64::max),
            DrawCmd::Extrusion { vertices, .. } => vertices.iter().map(|v| far(&v.position[..2])).fold(0.0, f64::max),
            _ => 0.0,
        })
        .fold(0.0, f64::max)
}

/// A finished GPU scene: draw commands in bottom-first order plus the same side channels
/// (labels, attribution, pending flags) the CPU render returns in [`crate::Rendered`].
pub struct GpuScene {
    /// The basemap's water colour (premultiplied): the GPU target's clear colour, so a scene
    /// without any CPU content is still a complete frame — and pure-vector scenes carry no
    /// Image command at all (that is what lets the camera-uniform fast path engage).
    pub clear: [f32; 4],
    pub cmds: Vec<DrawCmd>,
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
    /// A turned or tilted scene: the camera it was drawn for, which the vertex shader's full
    /// perspective takes, and a tilted one's sky and fog. `None` for a flat, north-up scene.
    pub tilt: Option<SceneTilt>,
    /// Per command (by index; missing ones aren't clipped): which band of a tilted view's
    /// ground it draws, and whether its markers stand upright.
    pub clips: Vec<CmdClip>,
}

/// What a turned or tilted scene was drawn for.
#[derive(Clone, Copy, Debug)]
pub struct SceneTilt {
    pub camera: Camera,
    /// The sky at the top and the fog the ground fades into toward the view's reach
    /// (premultiplied).
    pub sky: [f32; 4],
    pub fog: [f32; 4],
}

/// How one command of a tilted scene is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CmdClip {
    /// The ground it draws, as nearest and farthest depths (multiples of the scene camera's
    /// distance from the view's centre): a band of the view, drawn flat at its own scale, shows
    /// only its own stretch of ground, as the CPU path's bands do.
    pub depth: [f32; 2],
    /// Point markers standing upright, sized by their distance (see `render::upright_markers`).
    pub upright: bool,
}

impl CmdClip {
    /// Every depth, markers as drawn.
    pub const ALL: CmdClip = CmdClip { depth: [0.0, f32::MAX], upright: false };
}

/// One draw command. `Image` is a CPU-rasterized run (basemap, tiles, rasters, and layers
/// that need tiny-skia: masks, blend modes, transparency, aggregation, extrusion) at its
/// exact z-position; `Mesh`/`Points` are lyon-tessellated vector layers.
pub enum DrawCmd {
    /// Screen-sized premultiplied RGBA, drawn covering the whole map view. The pixmap is
    /// moved in, not copied — flushing the CPU accumulator doesn't pay a full-screen `to_vec`.
    Image { pixmap: tiny_skia::Pixmap },
    /// Tessellated polygon fills and line/ring strokes of one layer run.
    Mesh { origin: [f64; 2], vertices: Vec<MeshVertex>, indices: Vec<u32> },
    /// Point markers: instances of a small set of unit-space shape templates.
    Points { origin: [f64; 2], instances: Vec<PointInstance> },
    /// Extruded features standing up in a tilted view: roofs and walls, lit and drawn with a
    /// depth buffer (as MapLibre's fill extrusions), or, with `shadows`, the shadows they cast
    /// on the ground, drawn under every feature.
    Extrusion { origin: [f64; 2], vertices: Vec<ExtrudeVertex>, indices: Vec<u32>, shadows: bool },
    /// One raster tile layer's tiles, each its own picture placed on the map: the GPU keeps a
    /// texture per picture across frames and moves the tiles with the camera as it moves the
    /// meshes, so a map with a basemap stays sharp while it pans and zooms.
    Tiles { origin: [f64; 2], tiles: Vec<TileQuad> },
    /// One raster tile layer the GPU map places again for every frame it shows, from the tile
    /// cache, for the camera of that frame (see [`crate::tiles::live`]): the scene holds only
    /// which layer, so panning and zooming never draw the scene again for its tiles (#635).
    /// The archive revision binds later live tile acquisition to this scene's credits.
    LiveTiles { name: String, spec: rustgis_tiles::Spec, opacity: f32, archive_revision: u64 },
    /// One vector tile layer's tiles as meshes (see `tile_mesh`): the GPU keeps each mesh's
    /// buffers across frames, shows the layers of its runs that show at the camera's zoom and
    /// clips each tile to its square, as MapLibre draws its tiles.
    TileMeshes { tiles: Vec<MeshTile> },
}

/// One tile of a [`DrawCmd::TileMeshes`].
#[derive(Clone)]
pub struct MeshTile {
    pub mesh: Arc<crate::tile_mesh::TileMesh>,
    /// The tile's north-west corner, map units: the mesh's origin.
    pub origin: [f64; 2],
    /// The square it is drawn in, from `origin`: least x and y, greatest x and y.
    pub clip: [f32; 4],
    /// The level the mesh was made for.
    pub level: u8,
}

/// One tile of a [`DrawCmd::Tiles`].
#[derive(Clone)]
pub struct TileQuad {
    /// The tile's picture, shared with the tile cache (a [`Payload::Image`]).
    pub picture: Arc<Payload>,
    /// The square drawn: least x and y, greatest x and y, origin-relative map units.
    pub rect: [f32; 4],
    /// The part of the picture in that square: left, top, right, bottom (0..1, top down).
    pub uv: [f32; 4],
    pub opacity: f32,
}

impl TileQuad {
    /// The picture's pixels (premultiplied RGBA).
    pub fn pixmap(&self) -> Option<&tiny_skia::Pixmap> {
        match &*self.picture {
            Payload::Image(pm) => Some(pm),
            _ => None,
        }
    }
}

/// What part of an extruded feature an [`ExtrudeVertex`] belongs to.
pub const EXTRUDE_ROOF: f32 = 0.0;
pub const EXTRUDE_WALL: f32 = 1.0;
pub const EXTRUDE_SHADOW: f32 = 2.0;

/// Vertex of an extruded feature. Position is origin-relative map units, with its height above
/// the ground (map units) third.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ExtrudeVertex {
    pub position: [f32; 3],
    /// A wall's outward normal (map units, unit length); zero elsewhere.
    pub normal: [f32; 2],
    /// Premultiplied RGBA, 0..1.
    pub color: [f32; 4],
    /// Up a wall (0 at its foot, 1 at its top), the part ([`EXTRUDE_ROOF`], [`EXTRUDE_WALL`],
    /// [`EXTRUDE_SHADOW`]), 1 when the wall is lit, and how far a shadow's corner falls from its
    /// feature (the feature's height, map units; 0 for a corner at the feature's foot).
    pub info: [f32; 4],
    pub _pad: f32,
}

/// Vertex for polygon fills and line strokes. Position is origin-relative map units.
/// Fill vertices carry `normal = [0, 0]` and `stroke_px = 0`; stroke vertices carry the
/// centerline position, the tessellator's unit normal, and the stroke width in physical
/// pixels, so the vertex shader keeps stroke width screen-constant at every zoom.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 2],
    pub normal: [f32; 2],
    /// Premultiplied RGBA, 0..1.
    pub color: [f32; 4],
    /// Stroke width in physical pixels (0 for fill vertices).
    pub stroke_px: f32,
    pub _pad: f32,
}

/// Vertex of a vector tile's mesh (see `tile_mesh`), drawn at any zoom of its level: what
/// changes with the zoom is given at the level and at the level above, for the vertex shader to
/// blend between. A fill's vertex has a zero `normal` and `width`; a stroke's lies on its line,
/// moved along `normal` by half its width; a circle's lies at its centre, moved along `normal`
/// by half its `width` (a disc's diameter, or across a ring's edge).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TileMeshVertex {
    /// Map units from the tile's north-west corner (x east, y north).
    pub position: [f32; 2],
    pub normal: [f32; 2],
    /// Premultiplied RGBA, 0..255, at the level and at the level above.
    pub color: [u8; 4],
    pub color_next: [u8; 4],
    /// Points, at the level and at the level above.
    pub width: [f32; 2],
    /// Map units along its line from where the line starts (dashes are laid along it; the
    /// shader takes it to pixels, as it takes `dashes` from widths).
    pub along: f32,
    /// A dashed line's dash and gap lengths, in widths; all zero for a solid one.
    pub dashes: [f32; 4],
}

/// One marker draw: an instance of a unit-space shape template scaled to `radius_px` screen
/// pixels by the vertex shader. A CPU marker with an outline becomes two instances (stroke
/// under, fill over; the outer half of a centered stroke is what shows) and a selection halo
/// is a third, largest, first — painter's order within the layer.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PointInstance {
    /// Origin-relative map units.
    pub position: [f32; 2],
    /// Premultiplied 0..1.
    pub color: [f32; 4],
    /// Shape radius in physical pixels.
    pub radius_px: f32,
    /// `rustgis_map::Marker` as u32 (0 Circle, 1 Square, 2 Triangle, 3 Diamond, 4 Star).
    pub shape: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout a vertex buffer of tile meshes reads (see `ui-egui`'s `mapgpu`).
    #[test]
    fn tile_vertices_are_laid_out_for_the_gpu() {
        assert_eq!(std::mem::size_of::<TileMeshVertex>(), 52);
        let v = TileMeshVertex::default();
        let at = |f: *const u8| f as usize - (&v as *const TileMeshVertex as usize);
        assert_eq!(at(v.normal.as_ptr().cast()), 8);
        assert_eq!(at(v.color.as_ptr()), 16);
        assert_eq!(at(v.color_next.as_ptr()), 20);
        assert_eq!(at(v.width.as_ptr().cast()), 24);
        assert_eq!(at((&v.along as *const f32).cast()), 32);
        assert_eq!(at(v.dashes.as_ptr().cast()), 36);
    }
}
