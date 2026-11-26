//! The globe: the Earth as a sphere seen from space (a near-side perspective projection).
//!
//! The camera sits [`GLOBE_DISTANCE`] Earth radii from the centre, looking at the map camera's
//! centre point. The sphere's radius on screen follows the camera resolution so that the ground
//! scale at the centre matches the flat map's: switching projections keeps the scale.
//!
//! Points are handled as unit vectors in the *view frame*: `x` east and `y` north at the centre
//! of view, `z` towards the viewer. A point is visible when `z` exceeds the horizon value
//! `1 / GLOBE_DISTANCE`. Turned to a bearing, `x` and `y` turn with the view; tilted, the camera
//! swings back about the centre of view (keeping its distance from it) and `z` follows it, so the
//! visible side is still where `z` exceeds the horizon value (one over the camera's distance).

use rustgis_geom::{Coord, Extent, MERCATOR_R};
use serde::{Deserialize, Serialize};

use crate::Camera;

/// Camera distance from the Earth's centre, in Earth radii.
pub const GLOBE_DISTANCE: f64 = 6.0;

/// How a map is drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Projection {
    /// The flat Web Mercator map.
    #[default]
    WebMercator,
    /// A 3D globe.
    Globe,
    /// Another flat projection: a preset id (see [`crate::projection::PRESETS`]) with any
    /// parameters (`robinson +lon_0=150`), `EPSG:<code>`, `ESRI:<code>` or `local:lon,lat`.
    Custom(String),
}

impl Projection {
    pub fn code(&self) -> String {
        match self {
            Projection::WebMercator => "mercator".into(),
            Projection::Globe => "globe".into(),
            Projection::Custom(c) => c.clone(),
        }
    }

    /// A display name: `Web Mercator`, `Globe`, a preset's name or `EPSG:3035`.
    pub fn label(&self) -> String {
        match self {
            Projection::WebMercator => "Web Mercator".into(),
            Projection::Globe => "Globe".into(),
            Projection::Custom(c) => crate::projection::lookup(c).map(|(_, l)| l).unwrap_or_else(|_| c.clone()),
        }
    }

    pub fn is_globe(&self) -> bool {
        matches!(self, Projection::Globe)
    }

    /// A projection by code: `mercator`, `globe`, a preset id or name, or an EPSG or ESRI code
    /// (which must name a projected coordinate system the map can draw).
    pub fn from_code(s: &str) -> Result<Projection, String> {
        match s.trim().to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
            // EPSG:900913, Web Mercator's unofficial code, is in no table.
            "mercator" | "webmercator" | "2d" | "map" | "epsg:900913" => return Ok(Projection::WebMercator),
            "globe" | "3d" | "scene" | "globalscene" => return Ok(Projection::Globe),
            _ => {}
        }
        let s = s.trim();
        // Web Mercator's codes are the default map, however they were typed (`EPSG 3857`, `esri:0102100`).
        let named = crate::projection::lookup(s).map(|(_, label)| label);
        if named.as_deref().is_ok_and(|l| ["EPSG:3857", "ESRI:102100", "ESRI:102113"].contains(&l)) {
            return Ok(Projection::WebMercator);
        }
        // Build it, not just look it up: a known code whose method has no implementation would
        // otherwise be accepted and drawn in Web Mercator under its own name (opengeos/RustGIS#208).
        crate::projection::Projector::new(s)?;
        // A local view keeps its centre.
        if s.starts_with("local:") {
            return Ok(Projection::Custom(s.to_string()));
        }
        // Canonical form: the preset id, or the code as `lookup` names it (`EPSG:<n>`, `ESRI:<n>`,
        // whatever the spacing and leading zeros typed).
        let canonical = match crate::projection::parse_preset(s)? {
            Some((id, params)) => crate::projection::preset_code(id, &params),
            None => named?,
        };
        Ok(Projection::Custom(canonical))
    }
}

/// A unit vector.
pub type V3 = [f64; 3];

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Normalises a vector (unchanged if it is zero).
pub fn normalize(v: V3) -> V3 {
    let n = dot(v, v).sqrt();
    if n > 0.0 && n.is_finite() { [v[0] / n, v[1] / n, v[2] / n] } else { v }
}

/// The globe as seen through a camera in a `w × h` view (logical pixels).
#[derive(Clone, Copy, Debug)]
pub struct Globe {
    /// Earth-frame axes of the view frame: `x` and `y` across the line of sight to the camera,
    /// `z` toward the camera (east, north and up at the centre of view when it faces north and
    /// looks straight down).
    x: V3,
    y: V3,
    z: V3,
    /// The centre of view (Earth frame).
    target: V3,
    /// The camera's distance from the Earth's centre (Earth radii), on the view frame's `z`.
    dist: f64,
    /// The camera's right, up and forward directions (view frame).
    right: V3,
    up: V3,
    forward: V3,
    /// Pixels per unit of image plane at unit distance (the focal length), and pixels per Earth
    /// radius at the centre of view.
    focal: f64,
    radius: f64,
    cx: f64,
    cy: f64,
    /// The visible disc (the Earth's outline): its centre row, and its radii across and up and
    /// down (an ellipse when tilted), worked out once.
    disc: (f64, f64, f64),
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// Where the camera of a globe tilted `pitch` radians is, in the plane of the view's centre
/// (`(0, 1)`, the Earth's centre at the origin) and its up direction (`x`): behind the centre of
/// view (down the screen) and above it, as far from it as when looking straight down. So far
/// out (five Earth radii), the whole side in sight stays in front of it at any pitch.
fn globe_eye(pitch: f64) -> (f64, f64) {
    let d = GLOBE_DISTANCE - 1.0;
    (-d * pitch.sin(), 1.0 + d * pitch.cos())
}

impl Globe {
    pub fn new(camera: &Camera, w: f64, h: f64) -> Globe {
        let c = camera.center_lonlat();
        let (lon, lat) = (c.x.to_radians(), c.y.clamp(-89.0, 89.0).to_radians());
        let (sl, cl) = lon.sin_cos();
        let (sp, cp) = lat.sin_cos();
        // Ground metres per pixel at the centre match the flat map's (res × cos lat).
        let ground = (camera.res * cp.max(1e-6)).max(1e-9);
        let radius = MERCATOR_R / ground;
        let (east, north, toward) = ([-sl, cl, 0.0], [-sp * cl, -sp * sl, cp], [cp * cl, cp * sl, sp]);
        // Turned to the bearing: the top of the view faces it.
        let b = crate::camera::normalize_bearing(camera.bearing).to_radians();
        let (sb, cb) = b.sin_cos();
        let right = add(scale(east, cb), scale(north, -sb));
        let upward = add(scale(north, cb), scale(east, sb));
        let pitch = crate::camera::normalize_pitch(camera.pitch);
        let (y, z, dist, r, up, forward) = if pitch == 0.0 {
            // Straight down, exactly: the camera on the line through the centre of view.
            (upward, toward, GLOBE_DISTANCE, [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0])
        } else {
            // Tilted: the camera swings back (down the view) about the centre of view.
            let (eu, ez) = globe_eye(pitch.to_radians());
            let eye = add(scale(upward, eu), scale(toward, ez));
            let dist = dot(eye, eye).sqrt();
            let z = scale(eye, 1.0 / dist);
            let y = cross(z, right);
            // The camera looks at the centre of view, with the view's right across.
            let ahead = normalize(add(toward, scale(eye, -1.0)));
            let in_view = |v: V3| [dot(v, right), dot(v, y), dot(v, z)];
            let (r, forward) = (in_view(right), in_view(ahead));
            (y, z, dist, r, cross(r, forward), forward)
        };
        let mut g = Globe {
            x: right,
            y,
            z,
            target: toward,
            dist,
            right: r,
            up,
            forward,
            focal: radius * (GLOBE_DISTANCE - 1.0),
            radius,
            cx: w / 2.0,
            cy: h / 2.0,
            disc: (h / 2.0, 0.0, 0.0),
        };
        let (row, half) = g.disc_rows();
        g.disc = (row, g.disc_across(), half);
        g
    }

    /// The horizon: points with a view-frame `z` above this are visible.
    pub fn horizon(&self) -> f64 {
        1.0 / self.dist
    }

    /// The centre of the visible disc (the outline of the Earth) on screen: the middle of the
    /// view when the globe looks straight down, lower when it is tilted.
    pub fn center(&self) -> (f64, f64) {
        (self.cx, self.disc.0)
    }

    /// The visible disc's radius across the screen; up and down it is [`Globe::disc_height`]
    /// (the same unless the globe is tilted, when the disc is an ellipse).
    pub fn disc_radius(&self) -> f64 {
        self.disc.1
    }

    /// The visible disc's radius across the screen, worked out.
    fn disc_across(&self) -> f64 {
        if self.forward == [0.0, 0.0, -1.0] {
            return self.radius * ((self.dist - 1.0) / (self.dist + 1.0)).sqrt();
        }
        // Across, the Earth's outline is widest at the disc's centre row: the outline's edge
        // direction there, sampled.
        let (s_a, c_a) = (1.0 / self.dist, (1.0 - 1.0 / (self.dist * self.dist)).max(0.0).sqrt());
        let to_centre = [0.0, 0.0, -1.0];
        let side = normalize(cross(to_centre, self.right));
        (0..=180)
            .map(|i| {
                let t = std::f64::consts::PI * f64::from(i) / 180.0;
                let d = add(scale(to_centre, c_a), add(scale(self.right, s_a * t.cos()), scale(side, s_a * t.sin())));
                let ahead = dot(d, self.forward);
                if ahead > 1e-9 { (self.focal * dot(d, self.right) / ahead).abs() } else { 0.0 }
            })
            .fold(0.0, f64::max)
    }

    /// The visible disc's radius up and down the screen.
    pub fn disc_height(&self) -> f64 {
        self.disc.2
    }

    /// The screen row of the visible disc's centre, and its half height.
    fn disc_rows(&self) -> (f64, f64) {
        // The Earth's outline seen from the camera: a cone `edge` wide about the direction to
        // the Earth's centre, which lies `toward` up from straight ahead.
        let edge = (1.0 / self.dist).clamp(-1.0, 1.0).asin();
        let to_centre = [0.0, 0.0, -1.0];
        let toward = dot(to_centre, self.up).atan2(dot(to_centre, self.forward));
        let limit = std::f64::consts::FRAC_PI_2 - 1e-3;
        let (lo, hi) = ((toward - edge).clamp(-limit, limit).tan(), (toward + edge).clamp(-limit, limit).tan());
        (self.cy - self.focal * (lo + hi) / 2.0, self.focal * (hi - lo) / 2.0)
    }

    /// Pixels per Earth radius at the centre of view.
    pub fn radius(&self) -> f64 {
        self.radius
    }

    /// A longitude/latitude point as a view-frame unit vector.
    pub fn to_view(&self, c: Coord) -> V3 {
        let (lon, lat) = (c.x.to_radians(), c.y.to_radians());
        let e = [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()];
        [dot(e, self.x), dot(e, self.y), dot(e, self.z)]
    }

    /// Screen position of a view-frame vector (whether or not it is visible).
    pub fn project(&self, v: V3) -> (f64, f64) {
        let rel = [v[0], v[1], v[2] - self.dist];
        let ahead = dot(rel, self.forward).max(1e-9);
        (self.cx + self.focal * dot(rel, self.right) / ahead, self.cy - self.focal * dot(rel, self.up) / ahead)
    }

    /// Screen position of a longitude/latitude point, or `None` behind the horizon.
    pub fn forward(&self, c: Coord) -> Option<(f64, f64)> {
        let v = self.to_view(c);
        (v[2] > self.horizon() && c.is_finite()).then(|| self.project(v))
    }

    /// The longitude/latitude under a screen position, or `None` off the globe.
    pub fn inverse(&self, sx: f64, sy: f64) -> Option<Coord> {
        // The ray from the camera (0, 0, dist) through the screen point.
        let (a, b) = ((sx - self.cx) / self.focal, (self.cy - sy) / self.focal);
        let d = add(add(scale(self.right, a), scale(self.up, b)), self.forward);
        let eye = [0.0, 0.0, self.dist];
        let qa = dot(d, d);
        let qb = 2.0 * dot(eye, d);
        let qc = self.dist * self.dist - 1.0;
        let disc = qb * qb - 4.0 * qa * qc;
        if disc < 0.0 || !disc.is_finite() || qa <= 0.0 {
            return None;
        }
        let t = (-qb - disc.sqrt()) / (2.0 * qa);
        if t <= 0.0 {
            return None;
        }
        let v = add(eye, scale(d, t));
        let e = add(add(scale(self.x, v[0]), scale(self.y, v[1])), scale(self.z, v[2]));
        let c = Coord::new(e[1].atan2(e[0]).to_degrees(), e[2].clamp(-1.0, 1.0).asin().to_degrees());
        c.is_finite().then_some(c)
    }

    /// The point where the segment from visible `a` to hidden `b` (or back) meets the horizon.
    pub fn horizon_crossing(&self, a: V3, b: V3) -> V3 {
        let h = self.horizon();
        let dz = b[2] - a[2];
        // Points level with each other never straddle the horizon; meet halfway if they do.
        let t = if dz.abs() > 1e-15 { ((h - a[2]) / dz).clamp(0.0, 1.0) } else { 0.5 };
        let v = normalize([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]);
        // Pull the point exactly onto the horizon circle.
        let r = (1.0 - h * h).sqrt();
        let xy = (v[0] * v[0] + v[1] * v[1]).sqrt().max(1e-12);
        [v[0] / xy * r, v[1] / xy * r, h]
    }

    /// Points along the horizon circle from `a` to `b` (both on it), the shorter way round.
    pub fn horizon_arc(&self, a: V3, b: V3) -> Vec<V3> {
        let (h, r) = (self.horizon(), (1.0 - self.horizon() * self.horizon()).sqrt());
        let t0 = a[1].atan2(a[0]);
        let mut dt = b[1].atan2(b[0]) - t0;
        while dt > std::f64::consts::PI {
            dt -= std::f64::consts::TAU;
        }
        while dt < -std::f64::consts::PI {
            dt += std::f64::consts::TAU;
        }
        let n = ((dt.abs() / 0.05).ceil() as usize).clamp(1, 128);
        (1..n)
            .map(|i| {
                let t = t0 + dt * i as f64 / n as f64;
                [r * t.cos(), r * t.sin(), h]
            })
            .collect()
    }

    /// Rotates the camera as if the globe were dragged by (`dx`, `dy`) pixels.
    /// The globe keeps its size: the camera resolution is Web Mercator metres per pixel, whose
    /// ground scale shrinks with cos(latitude), so it is rescaled as the centre moves north or
    /// south. The centre stays within ±85° (the flat map's limit); longitude turns are capped near
    /// the poles, where the exact rate grows without bound.
    pub fn drag(camera: &mut Camera, dx: f64, dy: f64, w: f64, h: f64) {
        if !(dx.is_finite() && dy.is_finite()) {
            return;
        }
        let g = Globe::new(camera, w, h);
        let c = camera.center_lonlat();
        let cos0 = c.y.clamp(-89.0, 89.0).to_radians().cos();
        // The screen's motion east and south, on a globe turned to a bearing.
        let (sb, cb) = crate::camera::normalize_bearing(camera.bearing).to_radians().sin_cos();
        // Tilted, the ground up and down the view is foreshortened at its centre (as the flat
        // map's `Camera::pan_px` allows for).
        let dy = dy / crate::camera::normalize_pitch(camera.pitch).to_radians().cos().max(0.05);
        let (dx, dy) = (dx * cb - dy * sb, dx * sb + dy * cb);
        let lon = c.x - (dx / g.radius).to_degrees() / cos0.max(0.2);
        let lat = (c.y + (dy / g.radius).to_degrees()).clamp(-85.0, 85.0);
        let lon = (lon + 540.0).rem_euclid(360.0) - 180.0;
        camera.center = rustgis_geom::lonlat_to_merc(Coord::new(lon, lat));
        let cos1 = lat.to_radians().cos();
        if cos1 > 0.0 && cos0 > 0.0 {
            camera.res *= cos0 / cos1;
        }
        camera.clamp();
    }

    /// Turns the globe so the point that was under `from` is under `to` (screen positions), as
    /// if grabbed; falls back to a plain drag when `from` is off the globe.
    pub fn drag_point(camera: &mut Camera, from: (f64, f64), to: (f64, f64), w: f64, h: f64) {
        let Some(target) = Globe::new(camera, w, h).inverse(from.0, from.1) else {
            Globe::drag(camera, to.0 - from.0, to.1 - from.1, w, h);
            return;
        };
        let (mut dx, mut dy) = (to.0 - from.0, to.1 - from.1);
        for _ in 0..4 {
            Globe::drag(camera, dx, dy, w, h);
            let Some((x, y)) = Globe::new(camera, w, h).forward(target) else { break };
            (dx, dy) = (to.0 - x, to.1 - y);
            if dx.abs() + dy.abs() < 0.25 {
                break;
            }
        }
    }

    /// Whether any of a longitude/latitude extent can be on the visible side: a cap test on its
    /// centre, widened by its angular radius.
    pub fn may_see(&self, e: &rustgis_geom::Extent) -> bool {
        if e.is_empty() || e.width() > 180.0 || e.height() > 90.0 {
            return !e.is_empty();
        }
        let c = self.to_view(e.center());
        let reach = [
            Coord::new(e.min_x, e.min_y),
            Coord::new(e.min_x, e.max_y),
            Coord::new(e.max_x, e.min_y),
            Coord::new(e.max_x, e.max_y),
            Coord::new(e.min_x, e.center().y),
            Coord::new(e.max_x, e.center().y),
        ]
        .iter()
        .map(|p| dot(self.to_view(*p), c).clamp(-1.0, 1.0).acos())
        .fold(0.0_f64, f64::max);
        let limit = self.horizon().acos() + reach + 0.02;
        limit >= std::f64::consts::PI || c[2].clamp(-1.0, 1.0).acos() <= limit
    }
}

impl Globe {
    /// Zooms by `factor`, keeping the point under (`sx`, `sy`) where it is when it's on the globe.
    pub fn zoom_at(camera: &mut Camera, factor: f64, sx: f64, sy: f64, w: f64, h: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let target = Globe::new(camera, w, h).inverse(sx, sy);
        camera.res /= factor;
        camera.clamp();
        if let Some(t) = target {
            // Turn the globe until the point is back under the pointer (converges in a few steps).
            for _ in 0..3 {
                let Some((x, y)) = Globe::new(camera, w, h).forward(t) else { break };
                if (sx - x).abs() + (sy - y).abs() < 0.25 {
                    break;
                }
                Globe::drag(camera, sx - x, sy - y, w, h);
            }
        }
    }

    /// Points the camera at a longitude/latitude extent: centred on it, with the whole globe in
    /// view when the extent spans most of a hemisphere, otherwise zoomed as the flat map would.
    pub fn fit(camera: &mut Camera, e: &rustgis_geom::Extent, w: f64, h: f64) {
        if e.is_empty() || w <= 1.0 || h <= 1.0 {
            return;
        }
        if e.width() > 100.0 || e.height() > 70.0 {
            let c = e.center();
            let lat = c.y.clamp(-60.0, 60.0);
            camera.center = rustgis_geom::lonlat_to_merc(Coord::new(c.x, lat));
            // Disc radius ≈ 44% of the shorter side.
            let disc = 0.44 * w.min(h);
            let radius = disc / ((GLOBE_DISTANCE - 1.0) / (GLOBE_DISTANCE + 1.0)).sqrt();
            camera.res = MERCATOR_R / radius / lat.to_radians().cos().max(0.05);
            camera.clamp();
        } else {
            camera.fit_lonlat(e, w, h);
        }
    }
}

/// A triangle mesh laying a flat Web Mercator picture over the globe: screen positions (logical
/// pixels from the view's top left), texture positions (0–1 across the picture) and triangles.
#[derive(Clone, Debug, Default)]
pub struct DrapeMesh {
    pub pos: Vec<[f32; 2]>,
    pub uv: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// Wraps a longitude difference into ±180°.
fn wrap_lon(d: f64) -> f64 {
    (d + 540.0).rem_euclid(360.0) - 180.0
}

impl Globe {
    /// The longitude of the centre of view.
    fn center_lon(&self) -> f64 {
        self.target[1].atan2(self.target[0]).to_degrees()
    }

    /// Longitude/latitude points spread over what a `w × h` view shows: a grid over the view, the
    /// limb and any pole in sight.
    fn visible_points(&self, w: f64, h: f64) -> Vec<Coord> {
        const N: usize = 32;
        let mut out = Vec::with_capacity((N + 1) * (N + 1) + 64);
        for i in 0..=N {
            for j in 0..=N {
                if let Some(c) = self.inverse(w * i as f64 / N as f64, h * j as f64 / N as f64) {
                    out.push(c);
                }
            }
        }
        let on_screen = |x: f64, y: f64| x >= 0.0 && y >= 0.0 && x <= w && y <= h;
        let (cx, cy) = self.center();
        let (rx, ry) = (self.disc_radius() * 0.999, self.disc_height() * 0.999);
        for k in 0..64 {
            let a = std::f64::consts::TAU * k as f64 / 64.0;
            let (x, y) = (cx + rx * a.cos(), cy + ry * a.sin());
            if on_screen(x, y)
                && let Some(c) = self.inverse(x, y)
            {
                out.push(c);
            }
        }
        for pole in [Coord::new(0.0, 90.0), Coord::new(0.0, -90.0)] {
            if self.pole_in_view(pole, w, h) {
                out.extend((-180..=180).step_by(30).map(|lon| Coord::new(f64::from(lon), pole.y)));
            }
        }
        out
    }

    /// Whether a pole is on the visible side and inside a `w × h` view.
    fn pole_in_view(&self, pole: Coord, w: f64, h: f64) -> bool {
        self.forward(pole).is_some_and(|(x, y)| x >= 0.0 && y >= 0.0 && x <= w && y <= h)
    }

    /// The longitude/latitude boxes a `w × h` view shows, widened by `margin` (a fraction of
    /// their size) so they keep covering the view for a while as it turns. Each lies within
    /// ±180° longitude and the Web Mercator latitudes: a view across the antimeridian gets one
    /// box on either side of it. Empty when the globe is out of sight.
    pub fn drape_extents(&self, w: f64, h: f64, margin: f64) -> Vec<Extent> {
        use rustgis_geom::MAX_LAT;
        let pts = self.visible_points(w, h);
        if pts.is_empty() {
            return Vec::new();
        }
        let c0 = self.center_lon();
        let (mut d0, mut d1, mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for p in &pts {
            let d = wrap_lon(p.x - c0);
            d0 = d0.min(d);
            d1 = d1.max(d);
            y0 = y0.min(p.y);
            y1 = y1.max(p.y);
        }
        let margin = if margin.is_finite() { margin.clamp(0.0, 4.0) } else { 0.0 };
        let (px, py) = ((d1 - d0) * margin + 0.5, (y1 - y0) * margin + 0.5);
        let (y0, y1) = ((y0 - py).max(-MAX_LAT), (y1 + py).min(MAX_LAT));
        let (lo, hi) = (c0 + d0 - px, c0 + d1 + px);
        if !(lo.is_finite() && hi.is_finite() && y0 < y1) {
            return Vec::new();
        }
        // A pole in sight brings every longitude into view.
        let pole = [90.0, -90.0].iter().any(|&lat| self.pole_in_view(Coord::new(0.0, lat), w, h));
        if pole || hi - lo >= 360.0 {
            return vec![Extent::new(-180.0, y0, 180.0, y1)];
        }
        // Shift so the west edge is within ±180°; the east edge may then pass the antimeridian.
        let shift = 360.0 * ((lo + 180.0) / 360.0).floor();
        let (lo, hi) = (lo - shift, hi - shift);
        if hi <= 180.0 {
            vec![Extent::new(lo, y0, hi, y1)]
        } else {
            vec![Extent::new(lo, y0, 180.0, y1), Extent::new(-180.0, y0, hi - 360.0, y1)]
        }
    }

    /// Whether `boxes` (from [`Globe::drape_extents`]) cover everything a `w × h` view shows.
    pub fn drape_covers(&self, w: f64, h: f64, boxes: &[Extent]) -> bool {
        use rustgis_geom::MAX_LAT;
        self.visible_points(w, h).iter().all(|p| {
            let p = Coord::new(p.x, p.y.clamp(-MAX_LAT, MAX_LAT));
            boxes.iter().any(|b| b.contains(p))
        })
    }

    /// The mesh laying a picture of the Web Mercator box `merc` over the globe. Vertices behind
    /// the horizon are pulled onto it, and triangles wholly behind it are left out. A box
    /// reaching the Web Mercator latitude limit is stretched on to the pole, as the globe's
    /// tiles are.
    pub fn drape_mesh(&self, merc: &Extent) -> DrapeMesh {
        use rustgis_geom::merc_to_lonlat;
        let mut out = DrapeMesh::default();
        if !(!merc.is_empty() && merc.width() > 0.0 && merc.height() > 0.0 && merc.width().is_finite() && merc.height().is_finite()) {
            return out;
        }
        let ll =
            Extent::from_points(merc_to_lonlat(Coord::new(merc.min_x, merc.min_y)), merc_to_lonlat(Coord::new(merc.max_x, merc.max_y)));
        // About 2° cells, at least 16 a side (curvature), at most 192 (vertex count).
        let cols = ((ll.width() / 2.0).ceil() as usize).clamp(16, 192);
        let rows = ((ll.height() / 2.0).ceil() as usize).clamp(16, 192);
        // (latitude, v) of each row of vertices, top first.
        let edge = std::f64::consts::PI * MERCATOR_R * 0.9999;
        let mut lats: Vec<(f64, f32)> = Vec::with_capacity(rows + 3);
        if merc.max_y >= edge {
            lats.push((90.0, 0.0));
        }
        for j in 0..=rows {
            let t = j as f64 / rows as f64;
            let y = merc.max_y - merc.height() * t;
            lats.push((merc_to_lonlat(Coord::new(0.0, y)).y, t as f32));
        }
        if merc.min_y <= -edge {
            lats.push((-90.0, 1.0));
        }
        let h = self.horizon();
        let r = (1.0 - h * h).sqrt();
        let n = lats.len() * (cols + 1);
        out.pos.reserve(n);
        out.uv.reserve(n);
        let mut hidden = Vec::with_capacity(n);
        // The grid's longitudes and latitudes each turned into sines and cosines once, not at
        // every vertex: the mesh is made again every frame the globe moves, and in the browser
        // its trigonometry was a good part of each frame. As `to_view`, row by column.
        let lons: Vec<(f64, f64, f32)> = (0..=cols)
            .map(|i| {
                let t = i as f64 / cols as f64;
                let (s, c) = (ll.min_x + ll.width() * t).to_radians().sin_cos();
                (c, s, t as f32)
            })
            .collect();
        for &(lat, v) in &lats {
            let (slat, clat) = lat.to_radians().sin_cos();
            for &(clon, slon, t) in &lons {
                let e = [clat * clon, clat * slon, slat];
                let mut p = [dot(e, self.x), dot(e, self.y), dot(e, self.z)];
                let behind = p[2] <= h;
                if behind {
                    let xy = (p[0] * p[0] + p[1] * p[1]).sqrt().max(1e-12);
                    p = [p[0] / xy * r, p[1] / xy * r, h];
                }
                let (x, y) = self.project(p);
                out.pos.push([x as f32, y as f32]);
                out.uv.push([t, v]);
                hidden.push(behind);
            }
        }
        let stride = cols + 1;
        for j in 0..lats.len().saturating_sub(1) {
            for i in 0..cols {
                let (a, b) = (j * stride + i, j * stride + i + 1);
                let (c, d) = (a + stride, b + stride);
                for tri in [[a, b, c], [b, d, c]] {
                    if tri.iter().any(|&k| hidden.get(k) == Some(&false)) {
                        out.indices.extend(tri.iter().map(|&k| k as u32));
                    }
                }
            }
        }
        out
    }
}

/// Great-circle points between `a` and `b` (view-frame unit vectors), at most `step` radians
/// apart, excluding `a` and including `b`.
pub fn densify(a: V3, b: V3, step: f64) -> Vec<V3> {
    let angle = dot(a, b).clamp(-1.0, 1.0).acos();
    if !angle.is_finite() || angle <= step {
        return vec![b];
    }
    let n = ((angle / step).ceil() as usize).clamp(1, 360);
    (1..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            normalize([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rustgis_geom::lonlat_to_merc;

    use super::*;

    fn at(lon: f64, lat: f64, res: f64) -> Globe {
        Globe::new(&Camera { center: lonlat_to_merc(Coord::new(lon, lat)), res, bearing: 0.0, pitch: 0.0 }, 800.0, 600.0)
    }

    fn turned(lon: f64, lat: f64, bearing: f64, pitch: f64) -> Globe {
        Globe::new(&Camera { center: lonlat_to_merc(Coord::new(lon, lat)), res: 20_000.0, bearing, pitch }, 800.0, 600.0)
    }

    #[test]
    fn a_turned_and_tilted_globe_round_trips() {
        for (bearing, pitch) in [(40.0, 0.0), (-120.0, 30.0), (0.0, 55.0), (75.0, 70.0)] {
            let g = turned(10.0, 45.0, bearing, pitch);
            // The centre of view stays in the middle.
            let (x, y) = g.forward(Coord::new(10.0, 45.0)).unwrap();
            assert!((x - 400.0).abs() < 1e-6 && (y - 300.0).abs() < 1e-6, "{bearing}/{pitch}: {x}, {y}");
            for (lon, lat) in [(11.0, 45.5), (8.0, 43.0), (12.0, 44.0)] {
                let (x, y) = g.forward(Coord::new(lon, lat)).unwrap();
                let back = g.inverse(x, y).unwrap();
                assert!((back.x - lon).abs() < 1e-6 && (back.y - lat).abs() < 1e-6, "{bearing}/{pitch}: {lon},{lat} -> {back:?}");
            }
        }
    }

    #[test]
    fn a_turned_globe_puts_its_bearing_at_the_top() {
        // East at the top: a point east of the centre is above it, one north of it to the left.
        let g = turned(10.0, 0.0, 90.0, 0.0);
        let (x, y) = g.forward(Coord::new(15.0, 0.0)).unwrap();
        assert!(y < 290.0 && (x - 400.0).abs() < 1.0, "{x}, {y}");
        let (x, y) = g.forward(Coord::new(10.0, 5.0)).unwrap();
        assert!(x < 390.0 && (y - 300.0).abs() < 1.0, "{x}, {y}");
        // Dragged right, it brings in what lay to the left: the north.
        let mut cam = Camera { center: lonlat_to_merc(Coord::new(10.0, 0.0)), res: 20_000.0, bearing: 90.0, pitch: 0.0 };
        Globe::drag(&mut cam, 50.0, 0.0, 800.0, 600.0);
        let c = cam.center_lonlat();
        assert!(c.y > 1.0 && (c.x - 10.0).abs() < 1e-6, "{c:?}");
    }

    #[test]
    fn a_tilted_globe_follows_the_pointer() {
        let mut cam = Camera { center: lonlat_to_merc(Coord::new(10.0, 45.0)), res: 20_000.0, bearing: 25.0, pitch: 60.0 };
        let (w, h) = (800.0, 600.0);
        // The point grabbed below the centre stays under the pointer as it moves up.
        let grabbed = Globe::new(&cam, w, h).inverse(420.0, 380.0).unwrap();
        Globe::drag_point(&mut cam, (420.0, 380.0), (430.0, 330.0), w, h);
        let (x, y) = Globe::new(&cam, w, h).forward(grabbed).unwrap();
        assert!((x - 430.0).abs() < 1.0 && (y - 330.0).abs() < 1.0, "{x}, {y}");
        // A plain drag up the view moves the ground at the centre about as far as the pointer.
        let mut cam = Camera { center: lonlat_to_merc(Coord::new(10.0, 45.0)), res: 20_000.0, bearing: 0.0, pitch: 60.0 };
        let at = Globe::new(&cam, w, h).inverse(400.0, 300.0).unwrap();
        Globe::drag(&mut cam, 0.0, -20.0, w, h);
        let (_, y) = Globe::new(&cam, w, h).forward(at).unwrap();
        assert!((y - 280.0).abs() < 4.0, "{y}");
    }

    #[test]
    fn a_tilted_globe_looks_across_the_ground() {
        let flat = turned(10.0, 45.0, 0.0, 0.0);
        let g = turned(10.0, 45.0, 0.0, 50.0);
        // Ground ahead (north) is foreshortened, and the Earth's outline sits lower: seen
        // obliquely, an ellipse a little longer up the view than across.
        let (_, y_flat) = flat.forward(Coord::new(10.0, 50.0)).unwrap();
        let (_, y) = g.forward(Coord::new(10.0, 50.0)).unwrap();
        assert!(y < 300.0 && 300.0 - y < (300.0 - y_flat) * 0.8, "{y} vs {y_flat}");
        let (_, cy) = g.center();
        assert!(cy > 300.0 && g.disc_height() > g.disc_radius(), "{cy} {} {}", g.disc_height(), g.disc_radius());
        assert!((flat.disc_height() - flat.disc_radius()).abs() < 1e-6);
        // Points around the outline project onto it.
        let (cx, cy) = g.center();
        for k in 0..16 {
            let a = std::f64::consts::TAU * f64::from(k) / 16.0;
            let (x, y) = (cx + 0.98 * g.disc_radius() * a.cos(), cy + 0.98 * g.disc_height() * a.sin());
            assert!(g.inverse(x, y).is_some(), "inside the outline at {a}");
            let (x, y) = (cx + 1.03 * g.disc_radius() * a.cos(), cy + 1.03 * g.disc_height() * a.sin());
            assert!(g.inverse(x, y).is_none(), "outside the outline at {a}");
        }
    }

    #[test]
    fn forward_inverse_round_trip() {
        let g = at(10.0, 45.0, 20_000.0);
        assert_eq!(g.forward(Coord::new(10.0, 45.0)), Some((400.0, 300.0)));
        for (lon, lat) in [(10.0, 45.0), (30.0, 60.0), (-20.0, 10.0), (60.0, 30.0)] {
            let (x, y) = g.forward(Coord::new(lon, lat)).unwrap();
            let back = g.inverse(x, y).unwrap();
            assert!((back.x - lon).abs() < 1e-6 && (back.y - lat).abs() < 1e-6, "{lon},{lat} -> {back:?}");
        }
        // The far side is hidden; points off the disc have no position.
        assert_eq!(g.forward(Coord::new(-170.0, -45.0)), None);
        assert_eq!(g.inverse(400.0 + g.disc_radius() + 2.0, 300.0), None);
        assert!(g.inverse(400.0 + g.disc_radius() - 2.0, 300.0).is_some());
    }

    #[test]
    fn north_is_up_and_east_is_right() {
        let g = at(0.0, 0.0, 20_000.0);
        let (x, y) = g.forward(Coord::new(5.0, 5.0)).unwrap();
        assert!(x > 400.0 && y < 300.0);
        // Ground scale at the centre matches the flat map's.
        let (x1, _) = g.forward(Coord::new(0.01, 0.0)).unwrap();
        let metres = 0.01_f64.to_radians() * MERCATOR_R;
        assert!(((x1 - 400.0) - metres / 20_000.0).abs() < 1e-3);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        let mut cam = Camera { center: lonlat_to_merc(Coord::new(10.0, 40.0)), res: 20_000.0, bearing: 0.0, pitch: 0.0 };
        let before = Globe::new(&cam, 800.0, 600.0);
        let target = before.inverse(520.0, 240.0).unwrap();
        Globe::zoom_at(&mut cam, 1.6, 520.0, 240.0, 800.0, 600.0);
        let after = Globe::new(&cam, 800.0, 600.0);
        let (x, y) = after.forward(target).unwrap();
        assert!((x - 520.0).abs() < 1.0 && (y - 240.0).abs() < 1.0, "{x},{y}");
        // The globe grows by exactly the zoom factor, however it turned to keep the point.
        assert!((after.radius() / before.radius() - 1.6).abs() < 1e-6, "{}", after.radius() / before.radius());
        Globe::zoom_at(&mut cam, f64::NAN, 1.0, 1.0, 800.0, 600.0);
        Globe::zoom_at(&mut cam, 2.0, 1.0, 1.0, 800.0, 600.0); // off the globe: plain zoom
    }

    #[test]
    fn dragging_keeps_the_globe_size_and_far_features_are_culled() {
        let mut cam = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 20_000.0, bearing: 0.0, pitch: 0.0 };
        let r0 = Globe::new(&cam, 800.0, 600.0).disc_radius();
        Globe::drag(&mut cam, 0.0, 200.0, 800.0, 600.0);
        assert!(cam.center_lonlat().y > 20.0);
        let r1 = Globe::new(&cam, 800.0, 600.0).disc_radius();
        assert!((r1 - r0).abs() < 1e-6 * r0, "{r0} vs {r1}");
        let g = Globe::new(&Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 20_000.0, bearing: 0.0, pitch: 0.0 }, 800.0, 600.0);
        let near = rustgis_geom::Extent::new(-5.0, -5.0, 5.0, 5.0);
        let far = rustgis_geom::Extent::new(170.0, -5.0, 179.0, 5.0);
        let limb = rustgis_geom::Extent::new(75.0, -5.0, 85.0, 5.0);
        assert!(g.may_see(&near) && !g.may_see(&far) && g.may_see(&limb));
        assert!(g.may_see(&rustgis_geom::Extent::new(-180.0, -90.0, 180.0, 90.0)));
        assert!(!g.may_see(&rustgis_geom::Extent::EMPTY));
    }

    #[test]
    fn a_grabbed_point_follows_the_pointer() {
        let mut cam = Camera { center: lonlat_to_merc(Coord::new(20.0, 60.0)), res: 20_000.0, bearing: 0.0, pitch: 0.0 };
        let target = Globe::new(&cam, 800.0, 600.0).inverse(600.0, 200.0).unwrap();
        Globe::drag_point(&mut cam, (600.0, 200.0), (520.0, 260.0), 800.0, 600.0);
        let (x, y) = Globe::new(&cam, 800.0, 600.0).forward(target).unwrap();
        assert!((x - 520.0).abs() < 1.0 && (y - 260.0).abs() < 1.0, "{x},{y}");
    }

    #[test]
    fn drapes_cover_the_view_until_it_turns_past_them() {
        let (w, h) = (800.0, 600.0);
        let mut cam = Camera { center: lonlat_to_merc(Coord::new(10.0, 45.0)), res: 2_000.0, bearing: 0.0, pitch: 0.0 };
        let g = Globe::new(&cam, w, h);
        let boxes = g.drape_extents(w, h, 0.25);
        assert_eq!(boxes.len(), 1);
        assert!(g.drape_covers(w, h, &boxes));
        assert!(boxes[0].contains(Coord::new(10.0, 45.0)));
        // A small turn stays inside the margin; a big one leaves it.
        Globe::drag(&mut cam, 20.0, 0.0, w, h);
        assert!(Globe::new(&cam, w, h).drape_covers(w, h, &boxes));
        Globe::drag(&mut cam, 2_000.0, 0.0, w, h);
        assert!(!Globe::new(&cam, w, h).drape_covers(w, h, &boxes));
    }

    #[test]
    fn a_drape_across_the_antimeridian_is_split_and_the_whole_globe_is_one() {
        let (w, h) = (800.0, 600.0);
        let g = at(179.8, -18.0, 2_000.0);
        let boxes = g.drape_extents(w, h, 0.25);
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        for b in &boxes {
            assert!(b.min_x >= -180.0 && b.max_x <= 180.0 && b.min_y >= -rustgis_geom::MAX_LAT, "{b:?}");
        }
        assert!(g.drape_covers(w, h, &boxes));
        // A pole in view: the box spans every longitude.
        let g = at(0.0, 70.0, 50_000.0);
        let boxes = g.drape_extents(w, h, 0.25);
        assert_eq!(boxes.len(), 1);
        assert!(boxes[0].width() >= 359.9 && boxes[0].max_y > 85.0, "{boxes:?}");
        assert!(g.drape_covers(w, h, &boxes));
        // Nothing to drape off the globe or for junk.
        assert!(at(0.0, 0.0, 20_000.0).drape_extents(0.0, 0.0, f64::NAN).iter().all(|b| !b.is_empty()));
    }

    #[test]
    fn the_drape_mesh_lands_on_the_globe() {
        let g = at(10.0, 45.0, 20_000.0);
        let world = Extent::new(-180.0, -rustgis_geom::MAX_LAT, 180.0, rustgis_geom::MAX_LAT);
        let m = g.drape_mesh(&rustgis_geom::extent_to_merc(&world));
        assert_eq!(m.pos.len(), m.uv.len());
        assert!(!m.indices.is_empty() && m.indices.len().is_multiple_of(3));
        assert!(m.indices.iter().all(|&i| (i as usize) < m.pos.len()));
        assert!(m.pos.iter().flatten().all(|v| v.is_finite()));
        // Every vertex is on the disc, and the far side's triangles are left out.
        let (cx, cy) = g.center();
        let r = g.disc_radius() as f32 + 0.5;
        assert!(m.pos.iter().all(|p| (p[0] - cx as f32).hypot(p[1] - cy as f32) <= r));
        let cells = (m.pos.len() / 2) * 2;
        assert!(m.indices.len() / 3 < cells, "{} triangles", m.indices.len() / 3);
        // A picture's point lands where the globe puts it: the vertex at a texture position maps
        // to the screen position of its longitude/latitude.
        let merc = rustgis_geom::extent_to_merc(&Extent::new(0.0, 40.0, 20.0, 50.0));
        let m = g.drape_mesh(&merc);
        let k = m.uv.iter().position(|uv| uv[0] == 0.5 && uv[1] == 0.5).unwrap();
        let ll = rustgis_geom::merc_to_lonlat(merc.center());
        let (x, y) = g.forward(ll).unwrap();
        assert!((m.pos[k][0] - x as f32).abs() < 0.01 && (m.pos[k][1] - y as f32).abs() < 0.01);
        // Every vertex on the near side is where `forward` puts its longitude/latitude (the mesh
        // works out its sines and cosines by row and column instead).
        let ll = Extent::from_points(
            rustgis_geom::merc_to_lonlat(Coord::new(merc.min_x, merc.min_y)),
            rustgis_geom::merc_to_lonlat(Coord::new(merc.max_x, merc.max_y)),
        );
        for (p, uv) in m.pos.iter().zip(&m.uv) {
            let lon = ll.min_x + ll.width() * f64::from(uv[0]);
            let y = merc.max_y - merc.height() * f64::from(uv[1]);
            let lat = rustgis_geom::merc_to_lonlat(Coord::new(0.0, y)).y;
            if let Some((x, y)) = g.forward(Coord::new(lon, lat)) {
                assert!((p[0] - x as f32).abs() < 0.01 && (p[1] - y as f32).abs() < 0.01, "{p:?} vs {x}, {y}");
            }
        }
        // Junk boxes give no mesh.
        assert!(g.drape_mesh(&Extent::EMPTY).indices.is_empty());
        assert!(g.drape_mesh(&Extent::new(0.0, 0.0, f64::INFINITY, 1.0)).indices.is_empty());
    }

    #[test]
    fn drag_rotates_and_degenerate_input_is_safe() {
        let mut cam = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 20_000.0, bearing: 0.0, pitch: 0.0 };
        Globe::drag(&mut cam, -100.0, 0.0, 800.0, 600.0);
        assert!(cam.center_lonlat().x > 0.0, "dragging left turns the globe east");
        Globe::drag(&mut cam, f64::NAN, 1e12, 800.0, 600.0);
        let g = Globe::new(&Camera { center: Coord::new(f64::NAN, 0.0), res: 0.0, bearing: 0.0, pitch: 0.0 }, 0.0, 0.0);
        let _ = g.inverse(1.0, 1.0);
        let _ = g.forward(Coord::new(f64::NAN, f64::INFINITY));
        assert_eq!(densify([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], 0.1).len(), 1);
        assert_eq!(densify([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.1).len(), 16);
    }
}
