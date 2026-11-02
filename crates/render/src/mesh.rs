//! Lyon tessellation of layer geometry into GPU meshes, matching the tiny-skia CPU path's
//! rules: EvenOdd holes, round joins and caps, the 0.35 physical-pixel decimation of
//! `Ctx::ring_path`, and screen-constant stroke widths — the vertex shader displaces along
//! lyon's unit normal by `stroke_px * map_per_px / 2`, so one tessellation serves every zoom
//! the camera fast path shows it at (strokes are tessellated at the widest of those, see
//! [`tessellation_width`]).

use lyon::path::Path;
use lyon::tessellation::{
    BuffersBuilder, FillGeometryBuilder, FillOptions, FillRule, FillTessellator, FillVertex, GeometryBuilder, LineCap, LineJoin,
    StrokeGeometryBuilder, StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers, VertexId,
};
use rustgis_data::Geometry;
use rustgis_geom::{Coord, Extent};
use rustgis_map::{Layer, Marker, Rgba, Symbol};

use crate::gpu_scene::{DrawCmd, MeshVertex, PointInstance, f32_holds};
use crate::{GPU_CULL_MARGIN, SELECTION};

/// Why a mesh wasn't made; the caller falls the layer (or the outline) back to the CPU path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TessError {
    /// Lyon refused something tiny-skia would draw.
    Refused,
    /// The mesh would pass its vertex limit ([`MAX_LAYER_VERTICES`]).
    TooDense,
    /// Zoomed in too far for f32: vertices this far from the mesh's origin would land pixels
    /// from where they belong (#902). The CPU path clips in f64 and draws them where they are.
    Imprecise,
}

/// The origin for the features in `ext` (its centre), or `Err(Imprecise)` when f32 can't hold
/// them at `map_per_px` map units per physical pixel.
pub(crate) fn origin_for(ext: &Extent, map_per_px: f64) -> Result<[f64; 2], TessError> {
    if !f32_holds(ext.width().max(ext.height()) / 2.0, map_per_px) {
        return Err(TessError::Imprecise);
    }
    Ok([ext.center().x, ext.center().y])
}

/// A lyon geometry builder that refuses vertices past `room`, so a tessellation stops at the
/// limit instead of finishing a mesh that would be thrown away.
struct Capped<B> {
    inner: B,
    room: usize,
    /// Set once a vertex was refused for want of room (lyon then reports a builder error).
    full: bool,
}

impl<B> Capped<B> {
    fn new(inner: B, room: usize) -> Self {
        Capped { inner, room, full: false }
    }

    fn take(&mut self) -> Result<(), lyon::tessellation::GeometryBuilderError> {
        match self.room.checked_sub(1) {
            Some(left) => {
                self.room = left;
                Ok(())
            }
            None => {
                self.full = true;
                Err(lyon::tessellation::GeometryBuilderError::TooManyVertices)
            }
        }
    }

    /// The error for a failed tessellation: the limit when this builder ran out of room.
    fn error(&self) -> TessError {
        if self.full { TessError::TooDense } else { TessError::Refused }
    }
}

impl<B: GeometryBuilder> GeometryBuilder for Capped<B> {
    fn begin_geometry(&mut self) {
        self.inner.begin_geometry();
    }
    fn end_geometry(&mut self) {
        self.inner.end_geometry();
    }
    fn add_triangle(&mut self, a: VertexId, b: VertexId, c: VertexId) {
        self.inner.add_triangle(a, b, c);
    }
    fn abort_geometry(&mut self) {
        self.inner.abort_geometry();
    }
}

impl<B: FillGeometryBuilder> FillGeometryBuilder for Capped<B> {
    fn add_fill_vertex(&mut self, vertex: FillVertex) -> Result<VertexId, lyon::tessellation::GeometryBuilderError> {
        self.take()?;
        self.inner.add_fill_vertex(vertex)
    }
}

impl<B: StrokeGeometryBuilder> StrokeGeometryBuilder for Capped<B> {
    fn add_stroke_vertex(&mut self, vertex: StrokeVertex) -> Result<VertexId, lyon::tessellation::GeometryBuilderError> {
        self.take()?;
        self.inner.add_stroke_vertex(vertex)
    }
}

/// Premultiplied 0..1 RGBA for the GPU (tiny-skia stores premultiplied; wgpu blends the same way).
pub(crate) fn premul(c: Rgba) -> [f32; 4] {
    let a = c.a() as f32 / 255.0;
    [c.r() as f32 / 255.0 * a, c.g() as f32 / 255.0 * a, c.b() as f32 / 255.0 * a, a]
}

/// Origin-relative f32 position (the f64 subtraction happens here, where the precision is).
fn rel(c: Coord, origin: [f64; 2]) -> [f32; 2] {
    [(c.x - origin[0]) as f32, (c.y - origin[1]) as f32]
}

/// Drops consecutive points closer than 0.35 physical px, keeping the first and always the
/// last — the exact rule of `Ctx::ring_path` (the dropped point doesn't become the anchor) —
/// and yields one run per maximal finite segment: a point the projection can't show breaks
/// the ring there rather than being bridged (also `ring_path`; lyon's path builder panics on
/// NaN, so non-finite coordinates never reach it).
/// `map_per_px` is map units per physical pixel (`camera.res / pixels_per_point` on the flat
/// map, where `to_screen` is affine).
fn decimate_runs(ring: &[Coord], map_per_px: f64) -> Vec<Vec<Coord>> {
    let threshold = 0.35 * map_per_px;
    let n = ring.len();
    let mut runs = Vec::new();
    let mut out: Vec<Coord> = Vec::new();
    let mut last: Option<Coord> = None;
    for (i, c) in ring.iter().enumerate() {
        if !c.is_finite() {
            if !out.is_empty() {
                runs.push(std::mem::take(&mut out));
            }
            last = None;
            continue;
        }
        let keep = !matches!(last, Some(l) if (c.x - l.x).abs() + (c.y - l.y).abs() < threshold && i + 1 != n);
        if keep {
            out.push(*c);
            last = Some(*c);
        }
    }
    if !out.is_empty() {
        runs.push(out);
    }
    runs
}

/// One closed subpath per decimated finite run of every ring. `None` when nothing survives
/// (the caller's `?` then skips or falls the layer back).
fn path_of(rings: &[Vec<Coord>], origin: [f64; 2], map_per_px: f64, min_points: usize) -> Option<Path> {
    let mut b = Path::builder();
    let mut any = false;
    for ring in rings {
        for pts in decimate_runs(ring, map_per_px) {
            if pts.len() < min_points {
                continue;
            }
            any = true;
            let p0 = rel(pts[0], origin);
            b.begin(lyon::math::point(p0[0], p0[1]));
            for c in &pts[1..] {
                let p = rel(*c, origin);
                b.line_to(lyon::math::point(p[0], p[1]));
            }
            b.close();
        }
    }
    any.then(|| b.build())
}

/// Tessellates polygon fills (all rings in one EvenOdd path, like the CPU's `Ctx::polygon`).
/// `Ok(None)` when nothing would draw; `Err` when lyon fails (the caller falls the layer back
/// to the CPU pixmap — tiny-skia still draws geometry lyon refuses). Non-finite coordinates
/// never reach lyon: `decimate` drops them. `Err(TooDense)` once the vertices would pass `limit`:
/// tessellation stops at the vertex that doesn't fit ([`MAX_LAYER_VERTICES`]).
pub(crate) fn fill_polygon(
    polys: &[Vec<Vec<Coord>>],
    origin: [f64; 2],
    color: [f32; 4],
    map_per_px: f64,
    limit: usize,
) -> Result<Option<(Vec<MeshVertex>, Vec<u32>)>, TessError> {
    let mut out: VertexBuffers<MeshVertex, u32> = VertexBuffers::new();
    let mut tess = FillTessellator::new();
    for rings in polys {
        let Some(path) = path_of(rings, origin, map_per_px, 3) else { continue };
        let room = limit.saturating_sub(out.vertices.len());
        let mut builder = Capped::new(
            BuffersBuilder::new(&mut out, |v: FillVertex| {
                let p = v.position();
                MeshVertex { position: [p.x, p.y], normal: [0.0, 0.0], color, stroke_px: 0.0, _pad: 0.0 }
            }),
            room,
        );
        if tess.tessellate_path(&path, &FillOptions::default().with_fill_rule(FillRule::EvenOdd), &mut builder).is_err() {
            return Err(builder.error());
        }
    }
    Ok((!out.indices.is_empty()).then_some((out.vertices, out.indices)))
}

/// The width (map units) a stroke `width_px` physical pixels wide is tessellated at: the widest
/// it is drawn at before the scene is made again, `map_per_px` zoomed out as far as the camera
/// fast path moves a mesh (the extent it was collected for, padded by [`GPU_CULL_MARGIN`] on
/// each side). Lyon places a join's inner vertex where the two offset edges meet unless that
/// point would pass the far ends of both edges, and then folds the join; that test needs the
/// width drawn. Tessellated at a fixed 1.0, a near-hairpin kept its inner vertex, many widths
/// out along the bisector, and the vertex shader's width stretched it into a spike reaching
/// far past both edges (#1295). Tessellated wider than drawn only folds a few sharp joins
/// lyon would not have. 1.0 when the product isn't a usable width (the tests' `map_per_px` of
/// 0).
fn tessellation_width(width_px: f32, map_per_px: f64) -> f32 {
    let w = (f64::from(width_px) * map_per_px * (1.0 + 2.0 * GPU_CULL_MARGIN)) as f32;
    // The round joins' tolerance is a fraction of it: that must be a positive normal float too.
    if w > 0.0 && (w * STROKE_TOLERANCE).is_normal() { w } else { 1.0 }
}

/// Round caps and joins, as a fraction of the tessellation width: about 31 segments per full
/// circle — lyon's default (0.1 at a width of 1) gives about 5, and the vertex shader scales the
/// normals up to any stroke_px, which would read as polygonal next to tiny-skia's analytic
/// curves.
const STROKE_TOLERANCE: f32 = 0.005;

/// Strokes rings (closed, polygon outlines) or line parts (open) at [`tessellation_width`]; the
/// vertex shader applies the real width. Ring outlines take no width floor (`Ctx::polygon`),
/// line parts floor at half a physical pixel (`Ctx::line`). `Err(TooDense)` once the vertices
/// would pass `limit`, as soon as one doesn't fit.
#[allow(clippy::too_many_arguments)]
fn stroke(
    rings: &[Vec<Coord>],
    origin: [f64; 2],
    color: [f32; 4],
    width_px: f32,
    map_per_px: f64,
    closed: bool,
    min_points: usize,
    limit: usize,
) -> Result<Option<(Vec<MeshVertex>, Vec<u32>)>, TessError> {
    let mut out: VertexBuffers<MeshVertex, u32> = VertexBuffers::new();
    let mut tess = StrokeTessellator::new();
    let width = tessellation_width(width_px, map_per_px);
    let options = StrokeOptions::default()
        .with_line_width(width)
        .with_tolerance(width * STROKE_TOLERANCE)
        .with_line_cap(LineCap::Round)
        .with_line_join(LineJoin::Round);
    for ring in rings {
        for pts in decimate_runs(ring, map_per_px) {
            if pts.len() < min_points {
                continue;
            }
            let mut b = Path::builder();
            let p0 = rel(pts[0], origin);
            b.begin(lyon::math::point(p0[0], p0[1]));
            for c in &pts[1..] {
                let p = rel(*c, origin);
                b.line_to(lyon::math::point(p[0], p[1]));
            }
            b.end(closed);
            let room = limit.saturating_sub(out.vertices.len());
            let mut builder = Capped::new(
                BuffersBuilder::new(&mut out, |v: StrokeVertex| {
                    let (p, n) = (v.position_on_path(), v.normal());
                    MeshVertex { position: [p.x, p.y], normal: [n.x, n.y], color, stroke_px: width_px, _pad: 0.0 }
                }),
                room,
            );
            if tess.tessellate_path(&b.build(), &options, &mut builder).is_err() {
                return Err(builder.error());
            }
        }
    }
    Ok((!out.indices.is_empty()).then_some((out.vertices, out.indices)))
}

/// Polygon outlines: every ring stroked (the CPU strokes exterior and holes alike).
pub(crate) fn stroke_rings(
    polys: &[Vec<Vec<Coord>>],
    origin: [f64; 2],
    color: [f32; 4],
    width_px: f32,
    map_per_px: f64,
    limit: usize,
) -> Result<Option<(Vec<MeshVertex>, Vec<u32>)>, TessError> {
    let rings: Vec<Vec<Coord>> = polys.iter().flatten().cloned().collect();
    stroke(&rings, origin, color, width_px, map_per_px, true, 3, limit)
}

/// Line features (round caps, round joins, 0.5 px minimum width).
pub(crate) fn stroke_lines(
    parts: &[Vec<Coord>],
    origin: [f64; 2],
    color: [f32; 4],
    width_px: f32,
    map_per_px: f64,
    limit: usize,
) -> Result<Option<(Vec<MeshVertex>, Vec<u32>)>, TessError> {
    stroke(parts, origin, color, width_px.max(0.5), map_per_px, false, 2, limit)
}

/// One marker as instances of a unit-space shape template: the selection halo first (a
/// larger filled shape, matching the CPU's 3.5 px halo stroke drawn under the fill), then the
/// outline (a larger fill in the stroke colour), then the fill SHRUNK by half the stroke
/// width — the CPU strokes the outline centered on the shape's edge (half over the fill), so
/// stroke at r + w/2 under fill at r − w/2 reproduces the exact band (`Ctx::marker`).
pub(crate) fn marker_instances(c: Coord, origin: [f64; 2], s: &Symbol, ppp: f32, halo: Option<Rgba>) -> Vec<PointInstance> {
    let r = (s.size.max(1.0) * ppp) / 2.0;
    let shape = match s.marker {
        Marker::Circle | Marker::Char(_) => 0,
        Marker::Square => 1,
        Marker::Triangle => 2,
        Marker::Diamond => 3,
        Marker::Star => 4,
    };
    let mut out = Vec::with_capacity(3);
    if let Some(h) = halo {
        out.push(PointInstance { position: rel(c, origin), color: premul(h), radius_px: r + 1.75 * ppp, shape });
    }
    let stroke_active = s.stroke_width > 0.0 && s.stroke.a() > 0 && (s.stroke_width * ppp).is_finite();
    if stroke_active {
        out.push(PointInstance { position: rel(c, origin), color: premul(s.stroke), radius_px: r + s.stroke_width * ppp / 2.0, shape });
    }
    let fill_r = if stroke_active { (r - s.stroke_width * ppp / 2.0).max(0.0) } else { r };
    out.push(PointInstance { position: rel(c, origin), color: premul(s.fill), radius_px: fill_r, shape });
    out
}

/// Appends a tessellated part with its indices rebased onto the shared vertex list. `Err` when
/// lyon failed the part or the result would overflow the u32 index space.
fn extend_mesh(
    v: &mut Vec<MeshVertex>,
    i: &mut Vec<u32>,
    part: Result<Option<(Vec<MeshVertex>, Vec<u32>)>, TessError>,
) -> Result<(), TessError> {
    if let Some((pv, pi)) = part? {
        if v.len() + pv.len() > u32::MAX as usize {
            return Err(TessError::TooDense); // a mesh this big can't index itself (u32): the layer falls back to the CPU
        }
        let base = v.len() as u32;
        v.extend(pv);
        i.extend(pi.into_iter().map(|x| x + base));
    }
    Ok(())
}

/// Appends one feature's GPU geometry, in the CPU path's per-feature order (fill, then
/// outline — `Ctx::feature`). Transparent fills/strokes are skipped: tiny-skia draws them as
/// no-ops, and not emitting them is identical output for less work. `Err` when lyon refuses
/// the geometry, or once the mesh passes `limit` vertices — the layer then falls back to the
/// CPU path entirely.
#[allow(clippy::too_many_arguments)]
fn append_geometry(
    v: &mut Vec<MeshVertex>,
    i: &mut Vec<u32>,
    pts: &mut Vec<PointInstance>,
    g: &Geometry,
    s: &Symbol,
    origin: [f64; 2],
    wireframe: bool,
    ppp: f32,
    map_per_px: f64,
    limit: usize,
) -> Result<(), TessError> {
    // What the layer's mesh has left: tessellation stops once a part passes it.
    let room = |v: &Vec<MeshVertex>| limit.saturating_sub(v.len());
    // A character has no shape template: its layer draws on the CPU.
    if matches!(s.marker, Marker::Char(_)) && matches!(g, Geometry::Point(_) | Geometry::MultiPoint(_)) {
        return Err(TessError::Refused);
    }
    match g {
        Geometry::Point(c) => pts.extend(marker_instances(*c, origin, s, ppp, None)),
        Geometry::MultiPoint(cs) => cs.iter().for_each(|c| pts.extend(marker_instances(*c, origin, s, ppp, None))),
        Geometry::Line(parts) => {
            // A non-finite width (hostile symbol state) would put NaN vertices on the GPU.
            if s.stroke.a() > 0 && (s.stroke_width * ppp).is_finite() {
                extend_mesh(v, i, stroke_lines(parts, origin, premul(s.stroke), s.stroke_width * ppp, map_per_px, room(v)))?;
            }
        }
        Geometry::Polygon(polys) if wireframe => {
            let width = s.stroke_width.max(1.0) * ppp;
            extend_mesh(v, i, stroke_rings(polys, origin, premul(s.stroke.with_alpha(255)), width, map_per_px, room(v)))?;
        }
        Geometry::Polygon(polys) => {
            if s.fill.a() > 0 {
                extend_mesh(v, i, fill_polygon(polys, origin, premul(s.fill), map_per_px, room(v)))?;
            }
            if s.stroke_width > 0.0 && s.stroke.a() > 0 {
                extend_mesh(v, i, stroke_rings(polys, origin, premul(s.stroke), s.stroke_width * ppp, map_per_px, room(v)))?;
            }
        }
    }
    Ok(())
}

/// The most vertices one layer's mesh may take (40 bytes each: 80 MB, well inside the 256 MiB
/// `max_buffer_size` devices and WebGL offer). A layer of a million tiny polygons (3D buildings
/// as faces) would tessellate to tens of millions — seconds per frame, for a mesh the device
/// then refuses — while the CPU path splats its sub-pixel features in milliseconds.
pub(crate) const MAX_LAYER_VERTICES: usize = 2_000_000;

/// The vertex limit of meshes that aren't a layer's (highlights, the basemap, the graticule,
/// extrusion roofs): small or bounded by their own data.
pub(crate) const NO_LIMIT: usize = usize::MAX;

/// How many mesh vertices `append_geometry` makes of a geometry, roughly: a fill one per
/// point; a round-joined stroke two per point, a join's arc for each turn (up to 16, a half
/// circle at the stroke tolerance, for a hairpin) and two round caps on an open line. Points
/// draw as instances and count nothing. It is an early out, not a bound: lyon adds vertices
/// where an EvenOdd ring crosses itself, and decimation drops some, so the tessellation itself
/// stops at the limit ([`Capped`]).
fn estimate_vertices(g: &Geometry, s: &Symbol, wireframe: bool) -> usize {
    fn stroked<'a>(rings: impl Iterator<Item = &'a Vec<Coord>>, closed: bool) -> usize {
        rings.map(|r| 2 * r.len() + joins(r, closed) + if closed { 0 } else { 32 }).sum()
    }
    // sin(θ/2) ≥ θ/π for a turn θ in 0..π, so 16·sin(θ/2) arc segments covers lyon's ~16θ/π —
    // without an atan2 per point.
    fn joins(r: &[Coord], closed: bool) -> usize {
        let n = r.len();
        let at = |k: usize| if closed { r.get(k % n) } else { r.get(k) };
        let ends = if closed { 0..n } else { 1..n.saturating_sub(1) };
        ends.filter_map(|k| {
            let (a, b, c) = (at(if closed { k + n - 1 } else { k.checked_sub(1)? })?, at(k)?, at(k + 1)?);
            let (u, v) = ((b.x - a.x, b.y - a.y), (c.x - b.x, c.y - b.y));
            let lens = ((u.0 * u.0 + u.1 * u.1) * (v.0 * v.0 + v.1 * v.1)).sqrt();
            let cos = ((u.0 * v.0 + u.1 * v.1) / lens).clamp(-1.0, 1.0);
            // A repeated or non-finite point turns nowhere.
            (lens > 0.0 && cos.is_finite()).then(|| (16.0 * ((1.0 - cos) / 2.0).sqrt()).ceil() as usize + 1)
        })
        .sum()
    }
    match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => 0,
        Geometry::Line(parts) if s.stroke.a() > 0 => stroked(parts.iter(), false),
        Geometry::Line(_) => 0,
        Geometry::Polygon(polys) if wireframe => stroked(polys.iter().flatten(), true),
        Geometry::Polygon(polys) => {
            let fill = if s.fill.a() > 0 { polys.iter().flatten().map(Vec::len).sum() } else { 0 };
            let outline = if s.stroke_width > 0.0 && s.stroke.a() > 0 { stroked(polys.iter().flatten(), true) } else { 0 };
            fill + outline
        }
    }
}

/// Collects one GPU-eligible layer, or one streamed batch of it, into Mesh/Points commands.
/// `geoms`, `extents`, `rows` and `visible` are the render loop's own (geometries as the CPU
/// draws them, their cached extents, row filter, extent cull), so the GPU draws exactly the
/// features the CPU would. `limit` is what is left of the layer's [`MAX_LAYER_VERTICES`] (its
/// earlier batches took the rest). `Ok` with the mesh vertices pushed; `Err(Refused)` when
/// lyon refused a feature, `Err(TooDense)` when the mesh would pass `limit`, `Err(Imprecise)` when
/// the features reach too far for f32 at this zoom — the caller then draws the layer
/// (or batch) with tiny-skia instead (nothing is pushed part-way).
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_layer(
    cmds: &mut Vec<DrawCmd>,
    layer: &Layer,
    geoms: &[Option<&Geometry>],
    extents: &[Extent],
    rows: &[bool],
    visible: &dyn Fn(&Extent) -> bool,
    wireframe: bool,
    ppp: f32,
    map_per_px: f64,
    limit: usize,
) -> Result<usize, TessError> {
    // Stable origin: the centre of the drawn features' extent, so the features are listed
    // first — with their symbols and estimates, each worked out once. The same pass gives up on
    // a layer too dense to mesh before any of it is tessellated.
    let mut ext = Extent::EMPTY;
    let mut estimate = 0usize;
    let mut features = Vec::new();
    for (row, g) in geoms.iter().enumerate() {
        let Some(g) = g else { continue };
        // Culled on the extent the CPU path culls on (the full geometry's, cached), so both
        // backends keep the same features; the globe has none cached.
        let e = extents.get(row).copied().unwrap_or_else(|| g.extent());
        if rows.get(row) != Some(&true) || !visible(&e) {
            continue;
        }
        ext = ext.union(&e);
        let s = layer.symbol(row);
        let n = estimate_vertices(g, &s, wireframe);
        estimate = estimate.saturating_add(n);
        if estimate > limit {
            return Err(TessError::TooDense);
        }
        features.push((*g, s, n));
    }
    if ext.is_empty() {
        return Ok(0);
    }
    let origin = origin_for(&ext, map_per_px)?;
    let (mut v, mut i, mut pts) = (Vec::new(), Vec::new(), Vec::new());
    for (g, s, n) in &features {
        // The estimate is rough: a feature estimated not to fit isn't started, and
        // `append_geometry` stops one that overflows at the first vertex past the limit.
        if v.len().saturating_add(*n) > limit {
            return Err(TessError::TooDense);
        }
        append_geometry(&mut v, &mut i, &mut pts, g, s, origin, wireframe, ppp, map_per_px, limit)?;
    }
    let used = v.len();
    if !i.is_empty() {
        cmds.push(DrawCmd::Mesh { origin, vertices: v, indices: i });
    }
    if !pts.is_empty() {
        cmds.push(DrawCmd::Points { origin, instances: pts });
    }
    Ok(used)
}

/// Selection highlight of one feature, after its layer (`Ctx::highlight`): haloed markers,
/// a wider cyan under-stroke for lines, translucent cyan fill + outline for polygons.
pub(crate) fn collect_highlight(cmds: &mut Vec<DrawCmd>, g: &Geometry, s: &Symbol, ppp: f32, map_per_px: f64) -> Result<(), TessError> {
    let origin = origin_for(&g.extent(), map_per_px)?;
    match g {
        Geometry::Point(c) => cmds.push(DrawCmd::Points { origin, instances: marker_instances(*c, origin, s, ppp, Some(SELECTION)) }),
        Geometry::MultiPoint(cs) => {
            let instances: Vec<PointInstance> = cs.iter().flat_map(|c| marker_instances(*c, origin, s, ppp, Some(SELECTION))).collect();
            cmds.push(DrawCmd::Points { origin, instances });
        }
        Geometry::Line(parts) => {
            let (mut v, mut i) = (Vec::new(), Vec::new());
            extend_mesh(
                &mut v,
                &mut i,
                stroke_lines(parts, origin, premul(SELECTION), (s.stroke_width + 3.0) * ppp, map_per_px, NO_LIMIT),
            )?;
            extend_mesh(&mut v, &mut i, stroke_lines(parts, origin, premul(s.stroke), s.stroke_width * ppp, map_per_px, NO_LIMIT))?;
            if !i.is_empty() {
                cmds.push(DrawCmd::Mesh { origin, vertices: v, indices: i });
            }
        }
        Geometry::Polygon(polys) => {
            let (mut v, mut i) = (Vec::new(), Vec::new());
            extend_mesh(&mut v, &mut i, fill_polygon(polys, origin, premul(SELECTION.with_alpha(40)), map_per_px, NO_LIMIT))?;
            extend_mesh(&mut v, &mut i, stroke_rings(polys, origin, premul(SELECTION), 2.0 * ppp, map_per_px, NO_LIMIT))?;
            if !i.is_empty() {
                cmds.push(DrawCmd::Mesh { origin, vertices: v, indices: i });
            }
        }
    }
    Ok(())
}

/// The built-in basemap's land polygons (already in map units), fill plus optional outline.
pub(crate) fn collect_basemap(
    cmds: &mut Vec<DrawCmd>,
    polys: &[Vec<Vec<Coord>>],
    ext: &Extent,
    land: Rgba,
    outline: Rgba,
    ppp: f32,
    map_per_px: f64,
) -> Result<(), TessError> {
    let origin = origin_for(ext, map_per_px)?;
    let (mut v, mut i) = (Vec::new(), Vec::new());
    extend_mesh(&mut v, &mut i, fill_polygon(polys, origin, premul(land), map_per_px, NO_LIMIT))?;
    if outline.a() > 0 {
        extend_mesh(&mut v, &mut i, stroke_rings(polys, origin, premul(outline), 0.6 * ppp, map_per_px, NO_LIMIT))?;
    }
    if !i.is_empty() {
        cmds.push(DrawCmd::Mesh { origin, vertices: v, indices: i });
    }
    Ok(())
}

/// The graticule lines (already in map units).
pub(crate) fn collect_graticule(
    cmds: &mut Vec<DrawCmd>,
    lines: &[Vec<Coord>],
    ext: &Extent,
    grat: Rgba,
    ppp: f32,
    map_per_px: f64,
) -> Result<(), TessError> {
    let origin = origin_for(ext, map_per_px)?;
    let (mut v, mut i) = (Vec::new(), Vec::new());
    extend_mesh(&mut v, &mut i, stroke_lines(lines, origin, premul(grat), 0.6 * ppp, map_per_px, NO_LIMIT))?;
    if !i.is_empty() {
        cmds.push(DrawCmd::Mesh { origin, vertices: v, indices: i });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::GeometryType;
    use rustgis_map::Marker;

    fn square(x: f64, y: f64, s: f64) -> Vec<Coord> {
        vec![Coord::new(x, y), Coord::new(x + s, y), Coord::new(x + s, y + s), Coord::new(x, y + s)]
    }

    fn point_covered(verts: &[MeshVertex], idx: &[u32], x: f32, y: f32) -> bool {
        idx.chunks(3).any(|t| {
            let [a, b, c] = [verts[t[0] as usize].position, verts[t[1] as usize].position, verts[t[2] as usize].position];
            let d = |o: [f32; 2], u: [f32; 2], v: [f32; 2]| (u[0] - o[0]) * (v[1] - o[1]) - (v[0] - o[0]) * (u[1] - o[1]);
            let (da, db, dc) = (d(a, b, c), d([x, y], b, c), d([x, y], c, a));
            da != 0.0 && db.signum() == da.signum() && dc.signum() == da.signum() && d([x, y], a, b).signum() == da.signum()
        })
    }

    #[test]
    fn polygon_with_hole_tessellates_evenodd() {
        let polys = vec![vec![square(0.0, 0.0, 100.0), square(30.0, 30.0, 40.0)]];
        let (v, i) = fill_polygon(&polys, [0.0, 0.0], [1.0, 0.0, 0.0, 1.0], 0.0, NO_LIMIT).expect("tessellate").expect("non-empty");
        assert!(i.len() >= 24, "a hole needs interior triangles: {}", i.len());
        assert!(!point_covered(&v, &i, 50.0, 50.0), "hole center must stay uncovered");
        // (10, 5): inside, and not on a tessellation diagonal (edge points read as uncovered).
        assert!(point_covered(&v, &i, 10.0, 5.0), "corner region must be covered");
    }

    #[test]
    fn decimate_matches_ring_path_rule() {
        // map_per_px = 2: threshold 0.7. Points at 0.5 apart drop, 0.8 apart keep.
        let ring = vec![
            Coord::new(0.0, 0.0),
            Coord::new(0.5, 0.0),  // 0.5 from anchor: drop
            Coord::new(1.0, 0.0),  // 1.0 from anchor (not the dropped 0.5): keep
            Coord::new(1.5, 0.0),  // 0.5: drop
            Coord::new(1.7, 0.0),  // 0.7: not < 0.7, keep
            Coord::new(1.71, 0.0), // last point: always kept
        ];
        let runs = decimate_runs(&ring, 2.0);
        assert_eq!(
            runs,
            vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.7, 0.0), Coord::new(1.71, 0.0)]],
            "no NaN: one run, the old rule"
        );
    }

    #[test]
    fn decimate_breaks_at_non_finite_points_like_ring_path() {
        let ring = vec![
            Coord::new(0.0, 0.0),
            Coord::new(10.0, 0.0),
            Coord::new(f64::NAN, 0.0), // the break
            Coord::new(20.0, 0.0),
            Coord::new(20.5, 0.0), // 0.5 < 0.7 from the run's fresh anchor: dropped
            Coord::new(30.0, 0.0),
        ];
        let runs = decimate_runs(&ring, 2.0);
        assert_eq!(runs.len(), 2, "two finite runs, no bridging edge");
        assert_eq!(runs[0], vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0)]);
        assert_eq!(runs[1], vec![Coord::new(20.0, 0.0), Coord::new(30.0, 0.0)], "decimation resets across the break");
    }

    #[test]
    fn stroke_vertices_carry_centerline_and_normal() {
        let parts = vec![vec![Coord::new(0.0, 0.0), Coord::new(100.0, 0.0)]];
        let (v, i) = stroke_lines(&parts, [0.0, 0.0], [0.0, 0.0, 1.0, 1.0], 2.0, 0.0, NO_LIMIT).expect("stroke").expect("non-empty");
        assert!(!i.is_empty());
        for vtx in &v {
            assert!(vtx.position[1].abs() < 1e-4, "centerline stays on the path: y={}", vtx.position[1]);
            let len = (vtx.normal[0] * vtx.normal[0] + vtx.normal[1] * vtx.normal[1]).sqrt();
            assert!((len - 1.0).abs() < 1e-3, "unit normal (round caps point along x): {:?}", vtx.normal);
            assert_eq!(vtx.stroke_px, 2.0);
        }
    }

    #[test]
    fn ring_stroke_has_no_width_floor_and_closes() {
        let polys = vec![vec![square(0.0, 0.0, 10.0)]];
        let (v, i) = stroke_rings(&polys, [0.0, 0.0], [0.0; 4], 0.2, 0.0, NO_LIMIT).expect("stroke").expect("non-empty");
        assert!(!i.is_empty());
        assert!(v.iter().all(|x| x.stroke_px == 0.2), "polygon outlines keep their exact width");
    }

    #[test]
    fn stroke_round_joints_are_finely_subdivided() {
        // A 90° arc stroked at width 1.0: with tolerance 0.005 the join is a real curve
        // (~8 vertices), not the ~1–2 of lyon's default 0.1 tolerance (~74° steps).
        let arc: Vec<Coord> = (0..=9)
            .map(|i| {
                let a = std::f64::consts::FRAC_PI_2 * i as f64 / 9.0;
                Coord::new(a.cos() * 100.0, a.sin() * 100.0)
            })
            .collect();
        let (v, _) = stroke_lines(&[arc], [0.0, 0.0], [0.0; 4], 2.0, 0.0, NO_LIMIT).expect("stroke").expect("non-empty");
        assert!(v.len() >= 40, "round join subdivided finely: {} vertices", v.len());
    }

    #[test]
    fn line_width_floors_at_half_pixel() {
        let parts = vec![vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0)]];
        let (v, _) = stroke_lines(&parts, [0.0, 0.0], [0.0; 4], 0.2, 0.0, NO_LIMIT).expect("stroke").expect("non-empty");
        assert!(v.iter().all(|x| x.stroke_px == 0.5));
    }

    #[test]
    fn marker_halo_and_stroke_emit_under_instances() {
        let s =
            Symbol { fill: Rgba::rgb(255, 0, 0), stroke: Rgba::rgb(255, 255, 255), stroke_width: 1.0, size: 8.0, marker: Marker::Circle };
        let three = marker_instances(Coord::new(5.0, 5.0), [5.0, 5.0], &s, 2.0, Some(Rgba::rgb(0, 255, 255)));
        assert_eq!(three.len(), 3, "halo + stroke + fill");
        assert_eq!(three[0].radius_px, 8.0 + 3.5, "halo = r + 1.75*ppp"); // r = 8*2/2 = 8
        assert_eq!(three[1].radius_px, 9.0, "stroke at r + w/2 (outer band)");
        assert_eq!(three[2].radius_px, 7.0, "fill at r − w/2 (the stroke's inner half covers its band)");
        // Painter's order: halo, stroke, fill.
        assert_eq!(three[0].color, premul(Rgba::rgb(0, 255, 255)));
        assert_eq!(three[1].color, premul(Rgba::rgb(255, 255, 255)));
        assert_eq!(three[2].color, premul(Rgba::rgb(255, 0, 0)));
        let one = marker_instances(Coord::new(5.0, 5.0), [5.0, 5.0], &Symbol { stroke_width: 0.0, ..s }, 2.0, None);
        assert_eq!(one.len(), 1, "fill only");
    }

    #[test]
    fn vertex_estimate_tracks_tessellation() {
        let s = Symbol { fill: Rgba::rgb(255, 0, 0), stroke: Rgba::rgb(0, 0, 0), stroke_width: 1.0, size: 4.0, marker: Marker::Circle };
        let polys = vec![vec![square(0.0, 0.0, 100.0), square(30.0, 30.0, 40.0)]];
        let shapes = [
            Geometry::Polygon(polys),
            Geometry::Polygon(vec![vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(0.0, 1.0)]]]),
            Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(50.0, 0.0), Coord::new(50.0, 50.0), Coord::new(0.0, 60.0)]]),
        ];
        for g in &shapes {
            for wireframe in [false, true] {
                let (mut v, mut i, mut pts) = (Vec::new(), Vec::new(), Vec::new());
                append_geometry(&mut v, &mut i, &mut pts, g, &s, [0.0, 0.0], wireframe, 1.0, 0.0, NO_LIMIT).expect("tessellate");
                let est = estimate_vertices(g, &s, wireframe);
                assert!(est >= v.len() / 2 && est <= v.len() * 2, "{g:?}: estimated {est}, tessellated {}", v.len());
            }
        }
        let point = Geometry::Point(Coord::new(0.0, 0.0));
        assert_eq!(estimate_vertices(&point, &s, false), 0, "markers are instances, not mesh");
        let hollow = Symbol { fill: Rgba::rgb(0, 0, 0).with_alpha(0), ..s };
        let (est_hollow, est_full) = (estimate_vertices(&shapes[1], &hollow, false), estimate_vertices(&shapes[1], &s, false));
        assert_eq!(est_full - est_hollow, 3, "a transparent fill tessellates nothing");
    }

    #[test]
    fn tessellation_stops_past_its_vertex_limit() {
        // Hairpin turns: each round join adds a half circle, which the estimate counts.
        let zigzag: Vec<Coord> = (0..200).map(|k| Coord::new((k % 2) as f64 * 100.0, k as f64)).collect();
        let parts = vec![zigzag.clone(), zigzag];
        let (v, _) = stroke_lines(&parts, [0.0, 0.0], [0.0; 4], 2.0, 0.0, NO_LIMIT).expect("stroke").expect("non-empty");
        let est = estimate_vertices(&Geometry::Line(parts.clone()), &Symbol::default_for(GeometryType::Polyline, 0), false);
        assert!(est >= v.len() && est <= v.len() * 2, "hairpins: estimated {est}, tessellated {}", v.len());
        // The first part already passes the limit: the second is never tessellated.
        assert_eq!(stroke_lines(&parts, [0.0, 0.0], [0.0; 4], 2.0, 0.0, v.len() / 4).err(), Some(TessError::TooDense));
        // One part stops at the vertex past the limit, not after it is finished.
        assert_eq!(stroke_lines(&parts[..1], [0.0, 0.0], [0.0; 4], 2.0, 0.0, 10).err(), Some(TessError::TooDense));
        let polys = vec![vec![square(0.0, 0.0, 10.0)], vec![square(20.0, 0.0, 10.0)]];
        assert!(fill_polygon(&polys, [0.0, 0.0], [0.0; 4], 0.0, 3).is_err(), "past the limit after the first square");
        assert!(fill_polygon(&polys, [0.0, 0.0], [0.0; 4], 0.0, 8).is_ok_and(|m| m.is_some()), "two squares fit in 8");
    }

    #[test]
    fn premultiplies_alpha() {
        assert_eq!(premul(Rgba::rgb(255, 0, 0).with_alpha(128)), [128.0 / 255.0, 0.0, 0.0, 128.0 / 255.0]);
    }

    /// Near-hairpins (slivers like the CIREN soils' in #1295): lyon must fold a join whose two
    /// offset edges meet past both edges. Tessellated narrower than drawn, the inner vertex
    /// stayed and the shader stretched it into a spike up to 80 half-widths long. Every vertex,
    /// drawn at the collected zoom and zoomed out as far as the camera fast path goes, stays
    /// within half the drawn width of the path. The wider tip folds only at the zoomed-out
    /// width: its edges meet 15 half-widths out, short of its 1000-unit edges at the collected
    /// zoom (20 half-widths of 50) and past them zoomed out (10 of 100).
    #[test]
    fn near_hairpin_strokes_stay_within_their_width() {
        let dist = |q: [f64; 2], a: Coord, b: Coord| {
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let t = (((q[0] - a.x) * dx + (q[1] - a.y) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            (q[0] - a.x - t * dx).hypot(q[1] - a.y - t * dy)
        };
        let (width_px, map_per_px) = (2.0, 50.0);
        for tip in [20.0, 134.0] {
            let path = vec![Coord::new(0.0, 0.0), Coord::new(1000.0, 0.0), Coord::new(0.0, tip)];
            let ring = stroke_rings(&[vec![path.clone()]], [0.0, 0.0], [0.0; 4], width_px, map_per_px, NO_LIMIT);
            let line = stroke_lines(std::slice::from_ref(&path), [0.0, 0.0], [0.0; 4], width_px, map_per_px, NO_LIMIT);
            for (what, mesh, closed) in [("polygon outline", ring, true), ("line", line, false)] {
                let (v, _) = mesh.expect("stroke").expect("non-empty");
                let mut segments: Vec<(Coord, Coord)> = path.windows(2).map(|s| (s[0], s[1])).collect();
                if closed {
                    segments.push((path[2], path[0]));
                }
                for zoomed_out in [1.0, 1.0 + 2.0 * GPU_CULL_MARGIN] {
                    // The vertex shader's displacement: normal * stroke_px * map_per_px / 2.
                    let half = f64::from(width_px) * map_per_px * zoomed_out / 2.0;
                    for x in &v {
                        let q = [
                            f64::from(x.position[0]) + f64::from(x.normal[0]) * half,
                            f64::from(x.position[1]) + f64::from(x.normal[1]) * half,
                        ];
                        let d = segments.iter().map(|(a, b)| dist(q, *a, *b)).fold(f64::INFINITY, f64::min);
                        assert!(d <= half * 1.01, "{what}, tip {tip}, x{zoomed_out}: a vertex {:.1} half-widths from the path", d / half);
                    }
                }
            }
        }
    }
}
