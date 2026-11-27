//! Map frames: their shape, border, extent indicator, camera and scale.

use super::*;

/// The largest a map frame's scale can be: 2:1, larger than life for finds drawings (#902). Even
/// there its camera ([`frame_camera`]) stays above the map's closest zoom
/// ([`crate::camera::MIN_RES`]) anywhere on Web Mercator.
pub const MIN_FRAME_SCALE: f64 = 0.5;
/// The smallest: the whole world many times over.
pub const MAX_FRAME_SCALE: f64 = 1e10;

/// The outline of a map frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum FrameShape {
    #[default]
    Rectangle,
    Circle,
    Ellipse,
    /// A polygon in the frame's unit square (0..1 from its top-left), drawn by Polygon or Lasso.
    Polygon(Vec<(f64, f64)>),
}

impl FrameShape {
    /// The outline as points in the frame's unit square.
    pub fn outline(&self) -> Vec<(f64, f64)> {
        match self {
            FrameShape::Rectangle => vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            FrameShape::Circle | FrameShape::Ellipse => (0..96)
                .map(|i| {
                    let t = i as f64 / 96.0 * std::f64::consts::TAU;
                    (0.5 + 0.5 * t.cos(), 0.5 + 0.5 * t.sin())
                })
                .collect(),
            FrameShape::Polygon(p) => p.clone(),
        }
    }
}

/// The extent indicator's colour and width (points) until they are set.
pub const INDICATOR_COLOR: Rgba = Rgba::rgb(0xD0, 0x1C, 0x1C);
pub const INDICATOR_WIDTH: f64 = 2.0;
/// The narrowest and widest an extent indicator's outline may be (points); to hide it, remove it.
pub const MIN_INDICATOR_WIDTH: f64 = 0.1;
pub const MAX_INDICATOR_WIDTH: f64 = 20.0;

/// A map frame's border until it is set (#2557): dark grey, 0.8 points.
pub const FRAME_BORDER_COLOR: Rgba = Rgba::rgb(0x30, 0x30, 0x30);
pub const FRAME_BORDER_WIDTH: f64 = 0.8;
/// The widest a map frame's border may be (points); 0 draws none.
pub const MAX_FRAME_BORDER_WIDTH: f64 = MAX_INDICATOR_WIDTH;

pub(super) fn frame_border_color() -> Rgba {
    FRAME_BORDER_COLOR
}

pub(super) fn is_frame_border_color(c: &Rgba) -> bool {
    *c == FRAME_BORDER_COLOR
}

pub(super) fn frame_border_width() -> f64 {
    FRAME_BORDER_WIDTH
}

pub(super) fn is_frame_border_width(w: &f64) -> bool {
    *w == FRAME_BORDER_WIDTH
}

pub(super) fn indicator_color() -> Rgba {
    INDICATOR_COLOR
}

pub(super) fn is_indicator_color(c: &Rgba) -> bool {
    *c == INDICATOR_COLOR
}

pub(super) fn indicator_width() -> f64 {
    INDICATOR_WIDTH
}

pub(super) fn is_indicator_width(w: &f64) -> bool {
    *w == INDICATOR_WIDTH
}

/// The camera that shows a frame's centre at its paper scale in `map`'s projection, turned to
/// `bearing` and tilted to `pitch` (degrees). The globe takes its camera in Web Mercator, whose
/// ground scale at the centre it matches, so it is handled like the flat map, and turns and
/// tilts like it. The frame's own bearing and pitch apply, whichever way the map in view is
/// turned or tilted.
pub fn frame_camera(map: &Map, center: Coord, scale: f64, bearing: f64, pitch: f64) -> Camera {
    let mut cam = Camera { bearing: crate::camera::normalize_bearing(bearing), pitch: crate::camera::normalize_pitch(pitch), ..map.camera };
    // The centre where the map's projection places it: a pole itself on a polar map. Where it
    // can't (a pole on Web Mercator, at infinity), 85° of latitude, as frames always were (#2684).
    // A longitude past a view's reach (±270° on Web Mercator) is the same meridian a turn of the
    // globe nearer: the frame shows the same place, not one moved to the view's edge (#2711).
    let center = if center.x.abs() > 270.0 { Coord::new((center.x + 180.0).rem_euclid(360.0) - 180.0, center.y) } else { center };
    let at = if frame_center_placed(map, center) { center } else { Coord::new(center.x, center.y.clamp(-85.0, 85.0)) };
    if let Some(c) = map.to_map(at) {
        cam.center = c;
    }
    let per = map.ground_per_unit(cam.center).max(1e-9);
    cam.res = scale * PAPER_M_PER_PT / per;
    cam.clamp();
    cam
}

/// Whether a frame centred on `center` (lon/lat) is drawn centred there on `map`: its projection
/// places the point, and reads it back at the same latitude. A pole on Web Mercator isn't: its
/// projection takes a pole to its edge at 85.05°, which reads back as that ([`frame_camera`]
/// draws the frame at 85° instead); on a polar map it is (#2684). The longitude isn't compared:
/// it may run past ±180°, and at a pole every longitude is the same point.
pub fn frame_center_placed(map: &Map, center: Coord) -> bool {
    map.to_map(center).and_then(|m| map.to_lonlat(m)).is_some_and(|back| (back.y - center.y).abs() < 1e-4)
}

/// The scale that fits a lon/lat extent into a frame `w_mm`×`h_mm` with a margin, and its centre.
pub fn fit_scale(map: &Map, e: &rustgis_geom::Extent, w_mm: f64, h_mm: f64) -> Option<(Coord, f64)> {
    if e.is_empty() || w_mm <= 0.0 || h_mm <= 0.0 {
        return None;
    }
    let flat = map.flat();
    let mut ext = rustgis_geom::Extent::EMPTY;
    for c in
        [Coord::new(e.min_x, e.min_y), Coord::new(e.max_x, e.min_y), Coord::new(e.max_x, e.max_y), Coord::new(e.min_x, e.max_y), e.center()]
    {
        if let Some(p) = flat.forward_checked(c) {
            ext.expand(p);
        }
    }
    if ext.is_empty() {
        return None;
    }
    let center = flat.inverse(ext.center())?;
    let per = (flat.ground_per_unit(ext.center()) * map.body().ratio()).max(1e-9);
    // A lone point gets a metre around it; anything larger fits as it is, down to a find a
    // millimetre across (#902). Ground metres, whatever the projection's units.
    let floor_m = if ext.width() > 0.0 || ext.height() > 0.0 { 1e-3 } else { 1.0 };
    let (gw, gh) = ((ext.width() * per).max(floor_m) * 1.1, (ext.height() * per).max(floor_m) * 1.1);
    let scale = (gw / (w_mm / 1000.0)).max(gh / (h_mm / 1000.0));
    Some((center, scale.clamp(MIN_FRAME_SCALE, MAX_FRAME_SCALE)))
}
