//! A layer its service draws on request (a WMS, a map service's export) asked for one picture
//! of the view instead of tiles (#1162): the service lays out its symbols and labels over the
//! whole view, where each tile would cut them at its edges, and is asked once a view instead of
//! for every tile.
//!
//! The picture reaches past the view, its middle on a grid and its detail rounded down to a
//! quarter of a power of two, so a small pan or zoom asks for nothing new; one on its way is
//! stood in for by the layer's last picture. Its size is in the map's logical pixels, as tiles
//! are asked for; on a high-density screen with sharp tiles on, in the screen's own pixels up to
//! twice those, and a WMS picture then says the resolution it is for, so the services that read it
//! keep their symbols' size (#2390).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rustgis_data::ogc::warp::{Grid, System, Warp};
use rustgis_data::raster::Transformer;
use rustgis_geom::Coord;
use rustgis_map::TileLayer;
use rustgis_map::projection::Flat;
use rustgis_tiles::cache::{self, Payload, State, Want};
use tiny_skia::Pixmap;

use crate::View;

/// Most pixels a side of a picture: what map servers usually draw at most.
const MAX_SIDE: u32 = 4096;
/// Most pixels a side once a service has refused a larger picture.
const SMALL_SIDE: u32 = 2048;
/// How much wider and taller than the view the picture is: room for the grid its middle is on
/// and for zooming out by a step before a new one is asked for.
const MARGIN: f64 = 1.8;
/// Steps a doubling of the map's detail is split into: the picture's units per pixel are the
/// map's, rounded down to one.
const STEPS: f64 = 4.0;
/// Pixels a picture's sides are rounded up to (so a view's size changing a little keeps it).
const ROUND: f64 = 256.0;
/// Room past the view on each side, a share of its size, that the layer's last picture must
/// reach beyond the view to be drawn instead of a new one.
const ROOM: f64 = 0.15;
/// Most layers whose last picture (and refused size) is kept.
const MAX_KEPT: usize = 256;

/// How a layer is asked for a picture: its address with the box, size and system to fill in
/// (`{nbbox}`, `{nw}`, `{nh}`, `{ncrs}`), and the system the picture is in.
pub(crate) struct Request {
    base: String,
    system: System,
    /// From the system to longitude/latitude and back; `None` for Web Mercator on a Web
    /// Mercator map (the map's own units).
    transformer: Option<Arc<Transformer>>,
    /// In another system than the map's: warped into place.
    warp: bool,
}

/// How `t` is asked for one picture on a flat map in `epsg` (3857 for Web Mercator): in the map's
/// system when its service offers it (an ArcGIS export takes any), else in one it does, warped
/// into place. `None` when it isn't to be, or can't be: it is drawn in tiles then.
pub(crate) fn of(t: &TileLayer, epsg: u16) -> Option<Request> {
    if !t.single_image || !t.on_request() {
        return None;
    }
    let mercator = || System { epsg: 3857, name: "EPSG:3857".into(), lat_first: false };
    // In `system`, on a map in `epsg`: as it comes on a map in it, else through longitude and
    // latitude (and warped into place).
    let request = |base: String, system: System| -> Option<Request> {
        let transformer = match (system.epsg, epsg) {
            (3857, 3857) => None,
            // Web Mercator, on a map in another system (`usable` leaves it out: maps are in it).
            (3857, _) => Some(rustgis_data::ogc::warp::transformer(3857)?),
            (own, _) => Some(rustgis_tiles::warp::transformer(own)?),
        };
        Some(Request { warp: system.epsg != epsg, base, system, transformer })
    };
    if !t.url.contains(rustgis_data::ogc::warp::MARK) {
        // Asked for in Web Mercator by its box.
        let base = rustgis_map::tiles::sized(&t.url, "{nw}", "{nh}")
            .replace("{bbox-epsg-3857}", "{nbbox}")
            .replace("{width}", "{nw}")
            .replace("{height}", "{nh}");
        // A map service's export in the map's own system: the service projects it.
        if epsg != 3857
            && let Some(r) =
                export_in(&base, epsg).and_then(|own| request(own, System { epsg, name: format!("EPSG:{epsg}"), lat_first: false }))
        {
            return Some(r);
        }
        return request(base, mercator());
    }
    // A layer of a system of its own: asked for in the map's when its service offers it.
    let filled = t.url.replacen("{z}/{x}/{y}", "0/0/0", 1);
    let (base, _, w) = Warp::parse(&filled)?;
    if !matches!(w.grid, Grid::Map { .. }) {
        return None;
    }
    // Else in the one it is drawn in: Web Mercator for one drawn in it as it comes (a WMS that
    // offers other systems too, marked `crs=3857`, whose other systems are listed, not that one).
    let base = Warp::map_template(base);
    let own = if w.epsg == 3857 { Some(mercator()) } else { w.system(w.epsg).cloned() };
    w.system(epsg).cloned().and_then(|s| request(base.clone(), s)).or_else(|| request(base, own?))
}

/// A map service export's address (`…/export` or `…/exportImage`, its box and picture in Web
/// Mercator: `bboxSR` and `imageSR` 3857) asked for in `epsg` instead; `None` for any other.
fn export_in(base: &str, epsg: u16) -> Option<String> {
    let (path, query) = base.split_once('?')?;
    let lower = path.to_ascii_lowercase();
    if !(lower.ends_with("/export") || lower.ends_with("/exportimage")) {
        return None;
    }
    // Each of the two once, both Web Mercator: the box's and the picture's system.
    let (mut bbox, mut image, mut other) = (0, 0, false);
    let parts: Vec<String> = query
        .split('&')
        .map(|part| match part.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("bboxSR") || k.eq_ignore_ascii_case("imageSR") => {
                *if k.eq_ignore_ascii_case("bboxSR") { &mut bbox } else { &mut image } += 1;
                other |= !matches!(v, "3857" | "102100");
                format!("{k}={epsg}")
            }
            _ => part.to_string(),
        })
        .collect();
    (bbox == 1 && image == 1 && !other).then(|| format!("{path}?{}", parts.join("&")))
}

/// The picture to ask for: its area (west, south, east, north in the system) and size in
/// pixels, for a view showing `area` at `res` units a pixel of the picture (a logical pixel's, or
/// finer on a HiDPI display), `max_side` pixels a side at most. `None` for a view with no size.
fn picture(area: [f64; 4], res: f64, max_side: u32) -> Option<([f64; 4], u32, u32)> {
    // The view's box in the picture's pixels: the same as the map zooms (a window's size).
    let (bw, bh) = ((area[2] - area[0]) / res, (area[3] - area[1]) / res);
    if !(bw.is_finite() && bh.is_finite() && bw > 0.0 && bh > 0.0 && res.is_finite() && res > 0.0) {
        return None;
    }
    let need = |b: f64| (b * MARGIN / ROUND).ceil() * ROUND;
    let (nw, nh) = (need(bw), need(bh));
    // Past the most a service draws: coarser, by whole steps.
    let over = (nw.max(nh) / f64::from(max_side)).max(1.0);
    let coarser = (over.log2() * STEPS).ceil();
    let shrink = 2f64.powf(coarser / STEPS);
    let level = (res.log2() * STEPS).floor() + coarser;
    let r = 2f64.powf(level / STEPS);
    let (w, h) = ((nw / shrink).ceil().min(f64::from(max_side)), (nh / shrink).ceil().min(f64::from(max_side)));
    // The middle on a grid of a sixth of the picture: the view stays inside it until it has
    // moved that far.
    let (sx, sy) = (w * r / 6.0, h * r / 6.0);
    let (cx, cy) = (((area[0] + area[2]) / 2.0 / sx).round() * sx, ((area[1] + area[3]) / 2.0 / sy).round() * sy);
    let (hw, hh) = (w * r / 2.0, h * r / 2.0);
    let out = [cx - hw, cy - hh, cx + hw, cy + hh];
    out.iter().all(|v| v.is_finite()).then_some((out, w as u32, h as u32))
}

/// The resolution of the OGC standard pixel (0.28 mm), which GeoServer draws at.
const OGC_DPI: f64 = 25.4 / 0.28;

/// A WMS GetMap address for a picture of `detail` pixels a logical one (as the picture came
/// out, past any size limit), with the resolution it is for in the parameters servers read:
/// `DPI` (QGIS Server, from 96) and `FORMAT_OPTIONS=dpi:` (GeoServer, from the OGC standard
/// pixel's 90.7; added to the options the address has), so their symbols and text keep their size
/// in the larger picture. Any other address, a picture at a logical pixel's detail or less, or
/// one that names a resolution already, is left as it is.
fn with_dpi(url: String, detail: f64) -> String {
    if !(detail > 1.0 && detail.is_finite()) {
        return url;
    }
    let (address, fragment) = url.split_once('#').map_or((url.as_str(), None), |(a, f)| (a, Some(f)));
    let Some((path, query)) = address.split_once('?') else { return url };
    let parts: Vec<(&str, Option<&str>)> =
        query.split('&').map(|part| part.split_once('=').map_or((part, None), |(k, v)| (k, Some(v)))).collect();
    // Keys compared decoded (`FORMAT%5FOPTIONS`), and written back as they came.
    let key = |k: &str, want: &str| rustgis_data::address::percent_decode(k).eq_ignore_ascii_case(want);
    let get_map = parts
        .iter()
        .any(|(k, v)| key(k, "REQUEST") && v.is_some_and(|v| rustgis_data::address::percent_decode(v).eq_ignore_ascii_case("GetMap")));
    // A resolution of its own: `DPI`, `MAP_RESOLUTION`, or a `dpi` among its format options.
    // Looked at decoded (`%64pi%3A300` is `dpi:300`); the address is kept as it is.
    let options_dpi = |v: &str| {
        rustgis_data::address::percent_decode(v)
            .split([';', ','])
            .any(|o| o.split_once(':').is_some_and(|(n, _)| n.trim().eq_ignore_ascii_case("dpi")))
    };
    let own =
        parts.iter().any(|(k, v)| key(k, "DPI") || key(k, "MAP_RESOLUTION") || (key(k, "FORMAT_OPTIONS") && v.is_some_and(options_dpi)));
    if !get_map || own {
        return url;
    }
    let dpi = (96.0 * detail).round();
    let ogc = (OGC_DPI * detail).round();
    let mut options = false;
    let mut query: Vec<String> = parts
        .iter()
        .map(|(k, v)| match v {
            Some(v) if key(k, "FORMAT_OPTIONS") && !options => {
                options = true;
                if v.is_empty() { format!("{k}=dpi:{ogc}") } else { format!("{k}={v};dpi:{ogc}") }
            }
            Some(v) => format!("{k}={v}"),
            // A bare key: the options are the resolution alone.
            None if key(k, "FORMAT_OPTIONS") && !options => {
                options = true;
                format!("{k}=dpi:{ogc}")
            }
            None => (*k).to_string(),
        })
        // An empty part (a trailing `&`) left out, so none is added before the resolution.
        .filter(|part| !part.is_empty())
        .collect();
    query.push(format!("DPI={dpi}"));
    if !options {
        query.push(format!("FORMAT_OPTIONS=dpi:{ogc}"));
    }
    let mut out = format!("{path}?{}", query.join("&"));
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// How long a layer whose large picture failed is asked for smaller ones before a large one is
/// tried again (the failure may have been passing: a timeout, a busy server).
const SMALL_FOR: web_time::Duration = web_time::Duration::from_secs(300);

/// How long a layer whose picture saying its resolution failed is asked for pictures without one.
const PLAIN_FOR: web_time::Duration = SMALL_FOR;

/// What is kept between frames, by the layer's address: its last picture drawn (the address,
/// area and units a pixel), when a large picture of it last failed, when a picture saying its
/// resolution was last refused, and when (and how large) a HiDPI picture of it last refused by
/// the server gave way to a standard one.
#[derive(Default)]
struct Kept {
    last: HashMap<String, (String, [f64; 4], f64)>,
    small: HashMap<String, web_time::Instant>,
    plain: HashMap<String, web_time::Instant>,
    standard: HashMap<String, (web_time::Instant, (u32, u32))>,
}

fn kept() -> std::sync::MutexGuard<'static, Kept> {
    static KEPT: OnceLock<Mutex<Kept>> = OnceLock::new();
    KEPT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Half the width of the Web Mercator world, metres: what a service draws in it.
const HALF_WORLD: f64 = std::f64::consts::PI * rustgis_geom::MERCATOR_R;

/// The world a system's pictures are kept to (west, south, east, north): Web Mercator's square,
/// longitude and latitude's; `None` for a projected system (its service says where it draws).
fn world_of(epsg: u16) -> Option<[f64; 4]> {
    if epsg == 3857 {
        return Some([-HALF_WORLD, -HALF_WORLD, HALF_WORLD, HALF_WORLD]);
    }
    rustgis_data::raster::Crs::from_epsg(u32::from(epsg)).filter(|c| c.is_geographic()).map(|_| [-180.0, -90.0, 180.0, 90.0])
}

/// A picture kept to its system's `world`: `None` where it reaches past the antimeridian (a
/// service draws nothing there: the layer is drawn in tiles, which wrap); past the poles' edge,
/// cut off there, with as many fewer pixels.
fn in_world(at: [f64; 4], w: u32, h: u32, world: [f64; 4]) -> Option<Option<([f64; 4], u32, u32)>> {
    if at[0] < world[0] || at[2] > world[2] {
        return None;
    }
    let r = (at[3] - at[1]) / f64::from(h.max(1));
    let (s, n) = (at[1].max(world[1]), at[3].min(world[3]));
    let rows = ((n - s) / r).round();
    if rows.is_nan() || rows < 1.0 {
        return Some(None);
    }
    Some(Some(([at[0], s, at[2], n], w, rows.min(f64::from(h)) as u32)))
}

/// Web Mercator metres a logical pixel at the middle of the view, whatever the map's projection
/// and its units: what a layer's zooms are worked out from.
fn mercator_res(view: &View, flat: &Flat) -> Option<f64> {
    let (cam, w, h) = (view.camera, view.width, view.height);
    let (a, b) = (cam.to_map(w / 2.0, h / 2.0, w, h), cam.to_map(w / 2.0 + 1.0, h / 2.0, w, h));
    let (a, b) = match flat {
        Flat::Mercator => (a, b),
        Flat::Custom(_) => (rustgis_geom::lonlat_to_merc(flat.inverse(a)?), rustgis_geom::lonlat_to_merc(flat.inverse(b)?)),
    };
    let r = (b.x - a.x).hypot(b.y - a.y);
    (r.is_finite() && r > 0.0).then_some(r)
}

/// Metres a pixel of a `tile_size`-pixel tile at `zoom` (a layer's zooms are its tiles').
fn zoom_res(zoom: u8, tile_size: u32) -> f64 {
    2.0 * HALF_WORLD / (f64::from(tile_size.max(1)) * 2f64.powi(i32::from(zoom)))
}

/// Draws the layer over `pm` (the view's pixels) from one picture: whether it is still on its
/// way, and the error of one that failed. `None` when the view's picture can't be asked for (it
/// crosses the antimeridian, or the service failed to draw it): the layer is drawn in tiles
/// instead.
pub(crate) fn draw(pm: &mut Pixmap, req: &Request, t: &TileLayer, view: &View, flat: &Flat) -> Option<(bool, Option<String>)> {
    let nothing = Some((false, None));
    // Not closer out than its service draws it (as its tiles).
    let Some(merc) = mercator_res(view, flat) else { return nothing };
    if rustgis_tiles::template::zoom_for(merc, t.tile_size, rustgis_map::tiles::MAX_ZOOM) < t.min_zoom {
        return nothing;
    }
    let cam = view.camera;
    let (w, h, k) = (view.width, view.height, view.pixels_per_point.max(0.01));
    // A pixel of the picture for each of the display's own on a HiDPI one, when sharp tiles are
    // on, so the picture isn't stretched (#2390).
    let detail = f64::from(rustgis_tiles::template::detail(k as f32));
    // Closer in than its finest zoom, its picture there is stretched (as its finest tiles are).
    let finer = (zoom_res(t.max_zoom, t.tile_size) * detail / merc).max(1.0);
    let to_system = |m: Coord| -> Option<Coord> {
        match &req.transformer {
            None => Some(m),
            Some(tr) => flat.inverse(m).and_then(|ll| tr.from_lonlat(ll)),
        }
    };
    let place = |c: Coord| -> Option<(f32, f32)> {
        let m = match &req.transformer {
            None => c,
            // Warped, a point the projection folds or wraps isn't placed (a cell across it would
            // be smeared over the map).
            Some(tr) => tr.to_lonlat(c).and_then(|ll| if req.warp { flat.forward_checked(ll) } else { flat.forward(ll) })?,
        };
        let (x, y) = cam.to_screen(m, w, h);
        Some(((x * k) as f32, (y * k) as f32))
    };
    // The view's area in the system, from its edges, and its units a logical pixel.
    let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
    for i in 0..=4 {
        for j in 0..=4 {
            let (sx, sy) = (w * f64::from(i) / 4.0, h * f64::from(j) / 4.0);
            if let Some(p) = to_system(cam.to_map(sx, sy, w, h)) {
                lo = (lo.0.min(p.x), lo.1.min(p.y));
                hi = (hi.0.max(p.x), hi.1.max(p.y));
            }
        }
    }
    let (Some(a), Some(b)) = (to_system(cam.to_map(w / 2.0, h / 2.0, w, h)), to_system(cam.to_map(w / 2.0 + 1.0, h / 2.0, w, h))) else {
        return nothing;
    };
    // The system's units a logical pixel, and a pixel of the picture asked for: coarser where it
    // is stretched past its finest zoom.
    let logical = (b.x - a.x).hypot(b.y - a.y);
    let res = logical * finer / detail;
    if !(lo.0 < hi.0 && lo.1 < hi.1) {
        return nothing;
    }
    let area = [lo.0, lo.1, hi.0, hi.1];
    // Nothing asked for outside the layer's bounds, when known.
    let bounds = t.bounds.and_then(|[bw, bs, be, bn]| match &req.transformer {
        None => {
            let (sw, ne) = (rustgis_geom::lonlat_to_merc(Coord::new(bw, bs)), rustgis_geom::lonlat_to_merc(Coord::new(be, bn)));
            Some([sw.x, sw.y, ne.x, ne.y])
        }
        Some(tr) => rustgis_data::ogc::warp::native_box(tr, [bw, bs, be, bn]),
    });
    if bounds.is_some_and(|l| l[0] > area[2] || l[2] < area[0] || l[1] > area[3] || l[3] < area[1]) {
        return nothing;
    }
    // Kept by the system too: a picture in another is no stand-in.
    let key = format!("{} {}", req.system.epsg, req.base);
    let opacity = t.opacity.clamp(0.0, 1.0);
    let blend = crate::blend_mode(t.blend);
    // In another system than the map's, warped into place piece by piece (one affine placing
    // would leave it off where the systems bend apart).
    let warped = req.warp;
    let paint = |pm: &mut Pixmap, img: &Pixmap, at: [f64; 4]| {
        if warped {
            paint_warped(pm, img, at, opacity, blend, &place);
        } else {
            crate::native_tiles::paint_blended(pm, img, at, at, opacity, blend, &place);
        }
    };
    // The units a pixel of a picture at a logical pixel's detail, as on a standard screen.
    let logical_res = logical * (zoom_res(t.max_zoom, t.tile_size) / merc).max(1.0);
    // What is tried when a picture fails, in turn, each once a frame:
    // 1. the same picture without its resolution (a service may refuse a parameter it doesn't
    //    know); kept for a while only when that one draws and the server refused the first
    //    (a 4xx other than 408 or 429: not a server error, a timeout or a lost connection);
    // 2. one at most 2048 pixels a side, with its resolution again (some services draw no
    //    larger), kept for a while unless the larger one's answer was empty;
    // 3. the picture at a logical pixel's detail, as on a standard screen (a service may draw
    //    no picture as large as a HiDPI one); kept for a while, for HiDPI pictures as large,
    //    when the server refused the HiDPI one and the standard one draws.
    // Past those, its tiles are drawn (and say what fails).
    let (mut without, mut smaller) = (false, false);
    // Kept to a standard screen's pictures while its HiDPI ones are as large as one the server
    // refused: a smaller view's may draw.
    let was_standard = {
        // The HiDPI picture's size as it would be asked: kept to the world, as below.
        let size = |max| {
            let (at, w, h) = picture(area, res, max)?;
            match world_of(req.system.epsg) {
                Some(world) => in_world(at, w, h, world).flatten().map(|(_, w, h)| (w, h)),
                None => Some((w, h)),
            }
        };
        let held = kept();
        let max = if held.small.get(&key).is_some_and(|at| at.elapsed() < SMALL_FOR) { SMALL_SIDE } else { MAX_SIDE };
        held.standard
            .get(&key)
            .is_some_and(|(at, (rw, rh))| at.elapsed() < SMALL_FOR && size(max).is_some_and(|(w, h)| w >= *rw && h >= *rh))
    };
    let mut standard = was_standard;
    // The size of the HiDPI picture the server refused (without its resolution), which keeps the
    // layer to a standard screen's pictures for a while once one draws; `None` for a passing
    // failure (a 5xx, a timeout), which keeps nothing.
    let mut refused_side: Option<(u32, u32)> = None;
    // The address (without its resolution) of the picture the server refused with it.
    let mut refused: Option<String> = None;
    for _ in 0..6 {
        // At most 2048 pixels: kept for a while after a refusal, or this frame after an empty
        // answer (which keeps nothing).
        let small = smaller || kept().small.get(&key).is_some_and(|at| at.elapsed() < SMALL_FOR);
        let sharp = detail > 1.0 && !standard;
        let plain = !sharp || without || kept().plain.get(&key).is_some_and(|at| at.elapsed() < PLAIN_FOR);
        let ask = if sharp { res } else { logical_res };
        let Some((at, pw, ph)) = picture(area, ask, if small { SMALL_SIDE } else { MAX_SIDE }) else { return nothing };
        let (at, pw, ph) = match world_of(req.system.epsg) {
            Some(world) => match in_world(at, pw, ph, world)? {
                Some(p) => p,
                None => return nothing,
            },
            None => (at, pw, ph),
        };
        // The picture's own units a pixel: coarser than asked past the most a service draws.
        let r = (at[2] - at[0]) / f64::from(pw.max(1));
        // The layer's last picture holding the whole view at this detail, with room to move, is
        // drawn instead: a view turned (its box wider or narrower at each angle) or made smaller
        // asks for nothing new. A view moving on asks for its next picture while the last still
        // covers it, standing in.
        let last = kept().last.get(&key).cloned();
        let (gx, gy) = ((area[2] - area[0]) * ROOM, (area[3] - area[1]) * ROOM);
        if let Some((u, last_at, last_r)) = &last
            && (last_r / r - 1.0).abs() < 1e-6
            && last_at[0] <= area[0] - gx
            && last_at[1] <= area[1] - gy
            && last_at[2] >= area[2] + gx
            && last_at[3] >= area[3] + gy
            && let Some(payload) = cache::peek(u)
            && let Payload::Image(img) = &*payload
        {
            paint(pm, img, *last_at);
            if let Some(side) = refused_side.filter(|_| standard && !was_standard) {
                remember_standard(&key, side);
            }
            return nothing;
        }
        let address = Warp::map_address(&req.base, &req.system, at, pw, ph);
        let url = if plain { address.clone() } else { with_dpi(address.clone(), logical / r) };
        // An empty answer to a HiDPI picture is taken as a failure, so the steps below are
        // tried (a service may draw nothing that large), but not as a refusal: nothing is kept
        // for it. To a standard screen's picture it is "nothing here", drawn as such. (An
        // answer that doesn't decode, such as a WMS exception, fails in the cache already.)
        let state = match cache::get(&url, Want::Image) {
            State::Ready(payload) if sharp && !matches!(&*payload, Payload::Image(_)) => State::Failed(EMPTY.into()),
            state => state,
        };
        match state {
            State::Ready(payload) => {
                if let Payload::Image(img) = &*payload {
                    paint(pm, img, at);
                    let mut k = kept();
                    if k.last.len() >= MAX_KEPT {
                        k.last.clear();
                    }
                    // The same picture drew without its resolution where it was refused with
                    // it: the layer is asked without it for a while.
                    if refused.as_deref() == Some(address.as_str()) {
                        if k.plain.len() >= MAX_KEPT {
                            k.plain.clear();
                        }
                        k.plain.insert(key.clone(), web_time::Instant::now());
                    }
                    k.last.insert(key.clone(), (url, at, r));
                    drop(k);
                    // A standard screen's picture drew where the server refused the HiDPI one this
                    // frame: so for a while, then the HiDPI one is tried again.
                    if let Some(side) = refused_side.filter(|_| standard && !was_standard) {
                        remember_standard(&key, side);
                    }
                }
                return nothing;
            }
            State::Pending => {
                // The layer's last picture, where it still is in the cache, stands in; failing
                // that, the same picture without its resolution, where it came from a try
                // before (the failure with it may have been forgotten since).
                let plain_here = (url != address).then(|| (address.clone(), at));
                let stand_in = last.map(|(u, at, _)| (u, at)).into_iter().chain(plain_here).find_map(|(u, at)| match cache::peek(&u) {
                    Some(payload) if matches!(&*payload, Payload::Image(_)) => Some((payload, at)),
                    _ => None,
                });
                if let Some((payload, at)) = stand_in
                    && let Payload::Image(img) = &*payload
                {
                    paint(pm, img, at);
                }
                return Some((true, None));
            }
            State::Failed(e) if url != address => {
                without = true;
                if refusal(&e) {
                    refused = Some(address.clone());
                }
            }
            State::Failed(e) if !small && pw.max(ph) > SMALL_SIDE => {
                if e == EMPTY {
                    smaller = true;
                } else {
                    let mut k = kept();
                    if k.small.len() >= MAX_KEPT {
                        k.small.clear();
                    }
                    k.small.insert(key.clone(), web_time::Instant::now());
                }
                // The smaller picture is asked with its resolution again: the size failed.
                (without, refused) = (false, None);
            }
            State::Failed(e) if sharp => {
                standard = true;
                refused_side = refusal(&e).then_some((pw, ph));
            }
            State::Failed(_) => return None,
        }
    }
    None
}

/// Keeps the layer `key` to a standard screen's pictures for a while, while its HiDPI ones are
/// at least `side` (width, height): the server refused one that large and the standard one drew.
fn remember_standard(key: &str, side: (u32, u32)) {
    let mut k = kept();
    if k.standard.len() >= MAX_KEPT {
        k.standard.clear();
    }
    k.standard.insert(key.to_string(), (web_time::Instant::now(), side));
}

/// What an empty answer to a HiDPI picture fails with (a failure, not a refusal).
const EMPTY: &str = "an empty answer";

/// Whether a fetch's failure is the server refusing what it was asked: a 4xx other than a
/// timeout (408) or too many requests (429), or a document that isn't a picture (a WMS
/// exception). A server error, a timeout, a lost connection or an empty answer says nothing of
/// what was asked.
fn refusal(error: &str) -> bool {
    error.starts_with(cache::NOT_A_PICTURE)
        || matches!(cache::http_status(error), Some(status) if (400..=499).contains(&status) && status != 408 && status != 429)
}

/// Cells a side a picture in another system is warped in: each placed by its own corners, so
/// it is off by at most the bend within a cell.
const WARP_CELLS: u32 = 16;

/// Draws `img`, covering `at` (west, south, east, north in its system), over `pm` through `place`
/// (a point of its system to the view's pixels) cell by cell, each cell two triangles placed by
/// their corners: the picture follows the bend between the systems, continuous across the cells.
/// A cell `place` turns over or stretches far past the others (across a projection's fold or
/// wrap) isn't drawn, rather than smeared over the map.
fn paint_warped(
    pm: &mut Pixmap,
    img: &Pixmap,
    at: [f64; 4],
    opacity: f32,
    blend: tiny_skia::BlendMode,
    place: &dyn Fn(Coord) -> Option<(f32, f32)>,
) {
    use tiny_skia::{FillRule, FilterQuality, Paint, PathBuilder, Pattern, SpreadMode, Transform};
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let n = WARP_CELLS;
    // The grid's points on screen, row by row from the top.
    let grid: Vec<Option<(f32, f32)>> = (0..=n)
        .flat_map(|j| (0..=n).map(move |i| (i, j)))
        .map(|(i, j)| {
            let (fx, fy) = (f64::from(i) / f64::from(n), f64::from(j) / f64::from(n));
            place(Coord::new(at[0] + (at[2] - at[0]) * fx, at[3] - (at[3] - at[1]) * fy))
        })
        .collect();
    let at_grid = |i: u32, j: u32| grid.get((j * (n + 1) + i) as usize).copied().flatten();
    // Each cell's corners (top-left, top-right, bottom-left, bottom-right) and its two
    // triangles' signed areas.
    type P = (f32, f32);
    let cells: Vec<(u32, u32, [P; 4], [f32; 2])> = (0..n)
        .flat_map(|j| (0..n).map(move |i| (i, j)))
        .filter_map(|(i, j)| {
            let c = [at_grid(i, j)?, at_grid(i + 1, j)?, at_grid(i, j + 1)?, at_grid(i + 1, j + 1)?];
            let cross = |a: P, b: P, o: P| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
            Some((i, j, c, [cross(c[1], c[2], c[0]), cross(c[2], c[1], c[3])]))
        })
        .collect();
    // The cells' usual size and turn (most cells': a few huge ones across a wrap don't decide
    // it): a triangle turned over, or a cell many times the size, is across a fold.
    let mut sizes: Vec<f32> = cells.iter().map(|c| (c.3[0] + c.3[1]).abs()).collect();
    sizes.sort_by(f32::total_cmp);
    let usual = sizes.get(sizes.len() / 2).copied().unwrap_or(0.0);
    let positive = cells.iter().filter(|c| c.3[0] + c.3[1] > 0.0).count();
    let turn = if positive * 2 >= cells.len() { 1.0 } else { -1.0 };
    let (cw, ch) = (iw / n as f32, ih / n as f32);
    // A triangle of the picture's pixels `s` drawn at the screen points `d`.
    let mut triangle = |s: [P; 3], d: [P; 3]| {
        // The affine map taking s to d: its columns from two edges.
        let (u, v) = ((s[1].0 - s[0].0, s[1].1 - s[0].1), (s[2].0 - s[0].0, s[2].1 - s[0].1));
        let det = u.0 * v.1 - u.1 * v.0;
        if det.abs() < f32::EPSILON {
            return;
        }
        let (p, q) = ((d[1].0 - d[0].0, d[1].1 - d[0].1), (d[2].0 - d[0].0, d[2].1 - d[0].1));
        let a = (p.0 * v.1 - q.0 * u.1) / det;
        let c = (q.0 * u.0 - p.0 * v.0) / det;
        let b = (p.1 * v.1 - q.1 * u.1) / det;
        let e = (q.1 * u.0 - p.1 * v.0) / det;
        let transform = Transform::from_row(a, b, c, e, d[0].0 - a * s[0].0 - c * s[0].1, d[0].1 - b * s[0].0 - e * s[0].1);
        let mut path = PathBuilder::new();
        path.move_to(d[0].0, d[0].1);
        path.line_to(d[1].0, d[1].1);
        path.line_to(d[2].0, d[2].1);
        path.close();
        let Some(path) = path.finish() else { return };
        let shader = Pattern::new(img.as_ref(), SpreadMode::Pad, FilterQuality::Bilinear, opacity, transform);
        // Without anti-aliasing, neighbouring triangles meet without a seam.
        let paint = Paint { shader, anti_alias: false, blend_mode: blend, ..Paint::default() };
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    };
    for (i, j, [tl, tr, bl, br], [a, b]) in cells {
        if a.signum() != turn || b.signum() != turn || (a + b).abs() > usual * 16.0 {
            continue;
        }
        let (x0, y0) = (i as f32 * cw, j as f32 * ch);
        let (x1, y1) = (x0 + cw, y0 + ch);
        triangle([(x0, y0), (x1, y0), (x0, y1)], [tl, tr, bl]);
        triangle([(x1, y0), (x1, y1), (x0, y1)], [tr, br, bl]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture warped piece by piece lands where `place` puts each of its points: an affine
    /// placing draws as one quad does, and a bent one is followed (a picture's middle where the
    /// bend puts it, not where its corners alone would).
    #[test]
    fn a_picture_in_another_system_is_warped_into_place() {
        let mut img = Pixmap::new(64, 64).unwrap();
        img.fill(tiny_skia::Color::from_rgba8(200, 30, 30, 255));
        // A blue dot in its middle.
        for y in 30..34 {
            for x in 30..34 {
                img.pixels_mut()[y * 64 + x] = tiny_skia::ColorU8::from_rgba(30, 30, 200, 255).premultiply();
            }
        }
        let at = [0.0, 0.0, 64.0, 64.0];
        let shift = |c: Coord| Some(((c.x + 10.0) as f32, (74.0 - c.y) as f32));
        let (mut a, mut b) = (Pixmap::new(100, 100).unwrap(), Pixmap::new(100, 100).unwrap());
        paint_warped(&mut a, &img, at, 1.0, tiny_skia::BlendMode::SourceOver, &shift);
        crate::native_tiles::paint_blended(&mut b, &img, at, at, 1.0, tiny_skia::BlendMode::SourceOver, &shift);
        let differ = a.pixels().iter().zip(b.pixels()).filter(|(p, q)| p != q).count();
        assert!(differ < 64, "{differ} pixels differ from one quad");
        // Bent with a cross term (x pushed right by x × y), so a cell's fourth corner is off any
        // affine piece fitted to the other three (by 16 × 16 / 64 = 4 pixels): each drawn pixel
        // shows the picture's column the bend's inverse says it should, within a pixel. (The
        // picture's red is its column.)
        let mut ramp = Pixmap::new(256, 256).unwrap();
        for (k, p) in ramp.pixels_mut().iter_mut().enumerate() {
            *p = tiny_skia::ColorU8::from_rgba((k % 256) as u8, 0, 0, 255).premultiply();
        }
        let bend = |c: Coord| Some(((c.x + 10.0 + c.x * c.y / 64.0) as f32, (266.0 - c.y) as f32));
        let mut pm = Pixmap::new(1300, 280).unwrap();
        paint_warped(&mut pm, &ramp, [0.0, 0.0, 256.0, 256.0], 1.0, tiny_skia::BlendMode::SourceOver, &bend);
        let (mut worst, mut seen) = (0.0f64, 0);
        for py in 0..pm.height() {
            for px in 0..pm.width() {
                let y = 266.0 - (f64::from(py) + 0.5);
                let x = (f64::from(px) + 0.5 - 10.0) / (1.0 + y / 64.0);
                // Away from the picture's edges, where its own pixels are clamped.
                if !(2.0..254.0).contains(&x) || !(2.0..254.0).contains(&y) {
                    continue;
                }
                let red = f64::from(pm.pixel(px, py).unwrap().red());
                worst = worst.max((red - (x - 0.5)).abs());
                seen += 1;
            }
        }
        assert!(seen > 100_000, "{seen} pixels drawn");
        assert!(worst < 1.5, "a pixel {worst} columns off the bend");
        // Across a wrap (the right of the picture placed a thousand pixels on): the cells
        // straddling it aren't drawn, so nothing is smeared between the two parts.
        let wrap = |c: Coord| Some(((if c.x > 200.0 { c.x + 1000.0 } else { c.x }) as f32, (256.0 - c.y) as f32));
        let mut pm = Pixmap::new(1400, 256).unwrap();
        paint_warped(&mut pm, &ramp, [0.0, 0.0, 256.0, 256.0], 1.0, tiny_skia::BlendMode::SourceOver, &wrap);
        assert!((0..256).all(|y| pm.pixel(600, y).unwrap().alpha() == 0), "smeared across the wrap");
        assert!(pm.pixel(100, 128).unwrap().alpha() == 255 && pm.pixel(1240, 128).unwrap().alpha() == 255);
        // Nothing drawn where `place` can't put the picture.
        let mut none = Pixmap::new(10, 10).unwrap();
        paint_warped(&mut none, &img, at, 1.0, tiny_skia::BlendMode::SourceOver, &|_| None);
        assert!(none.pixels().iter().all(|p| p.alpha() == 0));
    }

    /// A layer's zooms are its tiles': a 512-pixel tile at a zoom has the resolution of a
    /// 256-pixel one a zoom finer, so its picture is held, and its layer drawn, as its tiles are.
    #[test]
    fn zooms_are_in_the_layers_tile_size() {
        for z in 0..20 {
            assert!((zoom_res(z, 512) - zoom_res(z + 1, 256)).abs() < 1e-9 * zoom_res(z + 1, 256));
            assert_eq!(rustgis_tiles::template::zoom_for(zoom_res(z, 512), 512, 22), z);
        }
    }

    #[test]
    fn a_picture_reaches_past_the_view_and_holds_while_it_moves_a_little() {
        // A 1000 × 600 pixel view, 10 m a pixel.
        let res = 10.0;
        let view = |cx: f64, cy: f64, r: f64| [cx - 500.0 * r, cy - 300.0 * r, cx + 500.0 * r, cy + 300.0 * r];
        let (at, w, h) = picture(view(100_000.0, 200_000.0, res), res, MAX_SIDE).unwrap();
        // 1.8 times the view, rounded up to 256 pixels.
        assert_eq!((w, h), (2048, 1280));
        let r = (at[2] - at[0]) / f64::from(w);
        assert!(r <= res && r > res / 2f64.powf(1.0 / STEPS), "{r}");
        let v = view(100_000.0, 200_000.0, res);
        assert!(at[0] <= v[0] && at[1] <= v[1] && at[2] >= v[2] && at[3] >= v[3]);
        // Pans of less than half the grid either way and a small zoom ask for the same picture.
        let (cx, cy) = ((at[0] + at[2]) / 2.0, (at[1] + at[3]) / 2.0);
        let (gx, gy) = ((at[2] - at[0]) / 6.0, (at[3] - at[1]) / 6.0);
        for (dx, dy) in [(0.45, 0.0), (-0.45, 0.3), (0.2, -0.45)] {
            assert_eq!(picture(view(cx + dx * gx, cy + dy * gy, res), res, MAX_SIDE).unwrap().0, at, "{dx} {dy}");
        }
        assert_eq!(picture(view(cx, cy, res * 1.05), res * 1.05, MAX_SIDE).unwrap().0, at);
        // Past it, the next picture along.
        assert_ne!(picture(view(cx + 0.55 * gx, cy, res), res, MAX_SIDE).unwrap().0, at);
        // Panned and zoomed anywhere, the view is inside its picture.
        for i in 0..400 {
            let f = f64::from(i);
            let (cx, cy, r) = (f * 731.7 - 90_000.0, f * -517.3 + 33_000.0, res * 2f64.powf(f / 97.0 - 2.0));
            let v = view(cx, cy, r);
            let (at, w, h) = picture(v, r, MAX_SIDE).unwrap();
            assert!(at[0] <= v[0] && at[1] <= v[1] && at[2] >= v[2] && at[3] >= v[3], "{i}");
            assert!(w <= MAX_SIDE && h <= MAX_SIDE);
        }
    }

    #[test]
    fn a_view_wider_than_a_service_draws_is_asked_coarser() {
        // 3000 pixels wide: 5400 with the margin, past 4096 and 2048.
        let v = [0.0, 0.0, 3000.0, 1000.0];
        for max in [MAX_SIDE, SMALL_SIDE] {
            let (at, w, h) = picture(v, 1.0, max).unwrap();
            assert!(w <= max && h <= max, "{w} × {h}");
            assert!(at[0] <= v[0] && at[2] >= v[2] && at[1] <= v[1] && at[3] >= v[3]);
        }
        assert!(picture([0.0, 0.0, 0.0, 1.0], 1.0, MAX_SIDE).is_none());
        assert!(picture([0.0, 0.0, 1.0, 1.0], f64::NAN, MAX_SIDE).is_none());
    }

    #[test]
    fn a_web_mercator_picture_is_kept_to_the_world() {
        let h = HALF_WORLD;
        // Inside: as it is.
        let world = [-h, -h, h, h];
        assert_eq!(world_of(3857), Some(world));
        assert_eq!(world_of(4326), Some([-180.0, -90.0, 180.0, 90.0]));
        assert_eq!(world_of(2056), None);
        // Longitude and latitude: past the antimeridian, tiles; past a pole, cut there.
        let geo = [-180.0, -90.0, 180.0, 90.0];
        assert_eq!(in_world([170.0, 0.0, 190.0, 10.0], 100, 100, geo), None);
        assert_eq!(in_world([0.0, 80.0, 10.0, 100.0], 100, 200, geo).unwrap().unwrap().0, [0.0, 80.0, 10.0, 90.0]);
        assert_eq!(in_world([-10.0, -20.0, 10.0, 20.0], 100, 200, world), Some(Some(([-10.0, -20.0, 10.0, 20.0], 100, 200))));
        // Past the antimeridian (a Pacific view): tiles, which wrap.
        assert_eq!(in_world([h - 10.0, 0.0, h + 10.0, 20.0], 100, 100, world), None);
        assert_eq!(in_world([-h - 1.0, 0.0, 0.0, 20.0], 100, 100, world), None);
        // Past the top of the world: cut there, with as many fewer rows.
        let (at, w, rows) = in_world([0.0, h - 50.0, 100.0, h + 150.0], 100, 200, world).unwrap().unwrap();
        assert_eq!((at, w, rows), ([0.0, h - 50.0, 100.0, h], 100, 50));
        // Wholly past it: nothing.
        assert_eq!(in_world([0.0, h + 10.0, 100.0, h + 20.0], 100, 10, world), Some(None));
    }

    /// A WMS picture of more pixels than the view's logical ones says the resolution it is for,
    /// in the parameters servers read (#2390); any other is asked as it was.
    #[test]
    fn a_hidpi_wms_picture_says_its_resolution() {
        let wms = "https://m.example.org/wms?SERVICE=WMS&REQUEST=GetMap&LAYERS=a&WIDTH=800&HEIGHT=600";
        // QGIS Server's from 96, GeoServer's from the OGC standard pixel (90.7).
        assert_eq!(with_dpi(wms.into(), 2.0), format!("{wms}&DPI=192&FORMAT_OPTIONS=dpi:181"));
        assert_eq!(with_dpi(wms.into(), 1.5), format!("{wms}&DPI=144&FORMAT_OPTIONS=dpi:136"));
        // At a logical pixel's detail, or stretched past its finest zoom: as it was.
        for detail in [1.0, 0.4, f64::NAN, f64::INFINITY] {
            assert_eq!(with_dpi(wms.into(), detail), wms, "{detail}");
        }
        // A trailing `&`: no empty part before the resolution.
        assert_eq!(with_dpi(format!("{wms}&"), 2.0), format!("{wms}&DPI=192&FORMAT_OPTIONS=dpi:181"));
        // An encoded request name.
        let encoded = "https://m.example.org/wms?SERVICE=WMS&REQUEST=%47etMap&LAYERS=a";
        assert_eq!(with_dpi(encoded.into(), 2.0), format!("{encoded}&DPI=192&FORMAT_OPTIONS=dpi:181"));
        // Lower case keys, and a fragment kept last.
        let lower = "https://m.example.org/wms?service=wms&request=getmap&layers=a#rustgis-x";
        assert_eq!(
            with_dpi(lower.into(), 2.0),
            "https://m.example.org/wms?service=wms&request=getmap&layers=a&DPI=192&FORMAT_OPTIONS=dpi:181#rustgis-x"
        );
        // Format options of its own keep theirs, with the resolution added, its key as it came.
        assert_eq!(
            with_dpi(format!("{wms}&FORMAT%5FOPTIONS=antialias:none"), 2.0),
            format!("{wms}&FORMAT%5FOPTIONS=antialias:none;dpi:181&DPI=192")
        );
        // Format options of its own keep theirs, with the resolution added.
        assert_eq!(
            with_dpi(format!("{wms}&FORMAT_OPTIONS=antialias:none"), 2.0),
            format!("{wms}&FORMAT_OPTIONS=antialias:none;dpi:181&DPI=192")
        );
        // A bare options key takes the resolution, not a second key.
        assert_eq!(with_dpi(format!("{wms}&FORMAT_OPTIONS"), 2.0), format!("{wms}&FORMAT_OPTIONS=dpi:181&DPI=192"));
        // A resolution of its own, a request that isn't GetMap, or no query: as it was.
        for own in [
            format!("{wms}&dpi=300"),
            format!("{wms}&format_options=antialias:full;DPI:300"),
            format!("{wms}&FORMAT_OPTIONS=dpi%3A300"),
            format!("{wms}&FORMAT_OPTIONS=%64pi%3A300"),
            format!("{wms}&%44PI=300"),
            format!("{wms}&map_resolution=72"),
            "https://m.example.org/wms?SERVICE=WMS&REQUEST=GetCapabilities".into(),
            "https://s.example.org/x/MapServer/export?bbox=1,2,3,4&size=800,600&f=image".into(),
            "https://m.example.org/tile.png".into(),
        ] {
            assert_eq!(with_dpi(own.clone(), 2.0), own);
        }
    }

    #[test]
    fn asked_for_by_its_own_address() {
        let wms = "https://m.example.org/wms?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=a&STYLES=&CRS=EPSG:3857&BBOX={bbox-epsg-3857}&WIDTH=256&HEIGHT=256&FORMAT=image%2Fpng&TRANSPARENT=TRUE";
        let mut t = TileLayer::new("a", rustgis_map::TileKind::Raster, wms);
        assert!(of(&t, 3857).is_none(), "tiles unless asked");
        t.single_image = true;
        let r = of(&t, 3857).unwrap();
        assert_eq!(
            Warp::map_address(&r.base, &r.system, [1.0, 2.0, 3.0, 4.0], 800, 600),
            "https://m.example.org/wms?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=a&STYLES=&CRS=EPSG:3857&BBOX=1,2,3,4&WIDTH=800&HEIGHT=600&FORMAT=image%2Fpng&TRANSPARENT=TRUE"
        );
        // On a map in another system: in Web Mercator still (all its service offers), warped.
        let r = of(&t, 2056).unwrap();
        assert!(r.system.epsg == 3857 && r.warp && r.transformer.is_some());
        assert!(!of(&t, 3857).unwrap().warp);
        let export = "https://s.example.org/arcgis/rest/services/x/MapServer/export?bbox={bbox-epsg-3857}&bboxSR=3857&imageSR=3857&size=256,256&format=png32&transparent=true&f=image";
        t.url = export.into();
        let r = of(&t, 3857).unwrap();
        assert_eq!(
            Warp::map_address(&r.base, &r.system, [1.0, 2.0, 3.0, 4.0], 800, 600),
            "https://s.example.org/arcgis/rest/services/x/MapServer/export?bbox=1,2,3,4&bboxSR=3857&imageSR=3857&size=800,600&format=png32&transparent=true&f=image"
        );
        // On a map in another system, asked for in that one: the service projects it.
        let r = of(&t, 2056).unwrap();
        assert!(r.system.epsg == 2056 && !r.warp);
        assert_eq!(
            Warp::map_address(&r.base, &r.system, [1.0, 2.0, 3.0, 4.0], 800, 600),
            "https://s.example.org/arcgis/rest/services/x/MapServer/export?bbox=1,2,3,4&bboxSR=2056&imageSR=2056&size=800,600&format=png32&transparent=true&f=image"
        );
        // Both systems named once each, both Web Mercator; else asked for as it is, and warped.
        let x = "https://s.example.org/arcgis/rest/services/x/MapServer/export?bbox=1,2,3,4";
        assert!(export_in(&format!("{x}&bboxSR=3857&imageSR=102100"), 2056).is_some());
        assert_eq!(export_in(&format!("{x}&bboxSR=3857&bboxSR=3857"), 2056), None);
        assert_eq!(export_in(&format!("{x}&bboxSR=3857&imageSR=4326"), 2056), None);
        assert_eq!(export_in(&format!("{x}&bboxSR=3857&imageSR=3857&imageSR=3857"), 2056), None);
        // A template sized with `{width}` and `{height}`.
        t.url = "https://m.example.org/wms?BBOX={bbox-epsg-3857}&WIDTH={width}&HEIGHT={height}".into();
        let r = of(&t, 3857).unwrap();
        assert_eq!(
            Warp::map_address(&r.base, &r.system, [1.0, 2.0, 3.0, 4.0], 8, 6),
            "https://m.example.org/wms?BBOX=1,2,3,4&WIDTH=8&HEIGHT=6"
        );
        // A WMS that offers Web Mercator and other systems (swisstopo's), marked as drawn in Web
        // Mercator as it comes: one picture in Web Mercator, though only its others are listed.
        t.url = "https://wms.geo.admin.ch/de/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=a&STYLES=&CRS=EPSG:3857&BBOX={bbox-epsg-3857}&WIDTH=256&HEIGHT=256&FORMAT=image%2Fpng&TRANSPARENT=TRUE#rustgis-warp={z}/{x}/{y};crs=3857;in=3.3,44.0,14.2,49.2;sys=2056|e|EPSG:2056~4326|n|EPSG:4326;map=east".into();
        let r = of(&t, 3857).expect("one picture in Web Mercator");
        assert_eq!(
            Warp::map_address(&r.base, &r.system, [1.0, 2.0, 3.0, 4.0], 800, 600),
            "https://wms.geo.admin.ch/de/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=a&STYLES=&CRS=EPSG:3857&BBOX=1,2,3,4&WIDTH=800&HEIGHT=600&FORMAT=image%2Fpng&TRANSPARENT=TRUE"
        );
        // On a map in one of its other systems, in that one; in one it doesn't offer, in Web
        // Mercator, warped.
        assert_eq!(of(&t, 2056).map(|r| (r.system.epsg, r.warp)), Some((2056, false)));
        assert_eq!(of(&t, 3035).map(|r| (r.system.epsg, r.warp)), Some((3857, true)));
        // A WMS offered only in LV95: in it on a Web Mercator map, warped.
        t.url = "https://m.example.org/wms?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=a&STYLES=&CRS={ncrs}&BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}&FORMAT=image%2Fpng#rustgis-warp={z}/{x}/{y};crs=2056;in=5.9,45.8,10.5,47.8;sys=2056|e|EPSG:2056;map=east".into();
        let r = of(&t, 3857).unwrap();
        assert!(r.system.epsg == 2056 && r.warp);
        assert!(Warp::map_address(&r.base, &r.system, [1.0, 2.0, 3.0, 4.0], 8, 6).contains("CRS=EPSG:2056&BBOX=1,2,3,4&WIDTH=8&HEIGHT=6"));
        // Tiles cut from a tile set aren't drawn on request.
        t.url = "https://tile.example.org/{z}/{x}/{y}.png".into();
        assert!(of(&t, 3857).is_none());
    }
}
