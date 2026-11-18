//! Map layers whose service draws in another coordinate system than Web Mercator (#769): a WMS
//! layer offered only in a national system (EPSG:2056, EPSG:31287) or in longitude/latitude, a
//! WMTS layer whose tile matrix set is in one. The map still shows Web Mercator tiles: each is
//! made from what the service gives for its area in the layer's own system — one GetMap picture,
//! or the tiles of the matrix nearest its detail — warped into place (`rustgis_tiles::warp`).
//!
//! A WMS layer also keeps every system its service offers it in ([`System`]): on a map drawn in
//! one of them (EPSG:2056 chosen as the map's projection) it is asked for in that system and
//! drawn as it comes, and on a Web Mercator map in Web Mercator when it is offered so.
//!
//! Such a layer's address is the service's own, its placeholders for the layer's system
//! (`{ncrs}`, `{nbbox}`, `{nw}`, `{nh}`; `{nm}`, `{ncol}`, `{nrow}`), followed by a fragment that
//! never reaches the server: [`MARK`], the map tile (`{z}/{x}/{y}`), and how the layer is laid
//! out.

use rustgis_geom::{Coord, merc_to_lonlat};

use super::encode;
use crate::raster::{Crs, Transformer};

/// Marks a layer address drawn in another coordinate system (see the module docs).
pub const MARK: &str = "#rustgis-warp=";

/// An address drawn on request with its pictures' size, `w` × `h` (numbers or placeholders): a
/// map service export's `size` (an ArcGIS `…/export` or `…/exportImage` address), else a WMS's
/// `WIDTH` and `HEIGHT` (keys in any letter case). Values that are placeholders already
/// (`{nw}`) and anything after a `#` are kept.
pub fn sized(url: &str, w: &str, h: &str) -> String {
    let (main, fragment) = match url.split_once('#') {
        Some((m, f)) => (m, Some(f)),
        None => (url, None),
    };
    let Some((path, query)) = main.split_once('?') else { return url.to_string() };
    let lower = path.to_ascii_lowercase();
    let export = lower.ends_with("/export") || lower.ends_with("/exportimage");
    let parts: Vec<String> = query
        .split('&')
        .map(|part| match part.split_once('=') {
            Some((k, v)) if v.contains('{') => format!("{k}={v}"),
            Some((k, _)) if export && k.eq_ignore_ascii_case("size") => format!("{k}={w},{h}"),
            Some((k, _)) if !export && k.eq_ignore_ascii_case("width") => format!("{k}={w}"),
            Some((k, _)) if !export && k.eq_ignore_ascii_case("height") => format!("{k}={h}"),
            _ => part.to_string(),
        })
        .collect();
    let mut out = format!("{path}?{}", parts.join("&"));
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Points along each side of a map tile at which its place in the layer's system is worked out;
/// pixels between them are interpolated.
pub const GRID: usize = 16;

/// Most pictures one map tile is made from of a matrix with as much detail as it, or a coarser
/// one; see [`MAX_COARSEST_PARTS`] for a set whose coarsest matrix is finer.
pub const MAX_PARTS: usize = 16;

/// Most a map tile is made from of a matrix set's coarsest matrix, when even that is finer than
/// the tile's detail (a set starting at 1 km pixels, over a continent), nearest its middle. The
/// rest of such a tile is left clear: a request budget for a view far coarser than the set (as
/// the layer is zoomed in, its tiles need fewer and are whole).
pub const MAX_COARSEST_PARTS: usize = 64;

/// Most tile matrices kept of a WMTS tile matrix set: the coarsest half and the finest half.
pub const MAX_MATRICES: usize = 40;

/// Metres in a degree of the equator, for a tile matrix in degrees (OGC's scale denominators
/// count a degree as this long).
const METRES_PER_DEGREE: f64 = 2.0 * std::f64::consts::PI * 6_378_137.0 / 360.0;

/// Metres in a unit of the system `epsg`: [`METRES_PER_DEGREE`] in a geographic one, else as its
/// definition gives it (a US survey foot's 1200/3937 in a State Plane system), else 1.
pub fn metres_per_unit(epsg: u32) -> f64 {
    let Some(crs) = Crs::from_epsg(epsg) else { return 1.0 };
    if crs.is_geographic() {
        return METRES_PER_DEGREE;
    }
    let def = u16::try_from(epsg).ok().and_then(crate::raster::crs::epsg_proj4).unwrap_or_default();
    def.split_whitespace()
        .find_map(|p| match p.split_once('=')? {
            ("+to_meter", v) => v.parse::<f64>().ok().filter(|m| m.is_finite() && *m > 0.0),
            ("+units", "us-ft") => Some(1200.0 / 3937.0),
            ("+units", "ft") => Some(0.3048),
            ("+units", "link") => Some(0.201_168),
            _ => None,
        })
        .unwrap_or(1.0)
}

/// One matrix of a WMTS tile matrix set, in the layer's system.
#[derive(Clone, Debug, PartialEq)]
pub struct Matrix {
    /// Its identifier (the address's `{nm}`).
    pub id: String,
    /// The top left corner of its first tile, east and north.
    pub left: f64,
    pub top: f64,
    /// A pixel's side, in the system's units.
    pub cell: f64,
    pub tile_w: u32,
    pub tile_h: u32,
    pub cols: u32,
    pub rows: u32,
}

impl Matrix {
    /// A matrix as its capabilities give it: its scale denominator (OGC's 0.28 mm pixels) in a
    /// system whose unit is `unit` metres (see [`metres_per_unit`]).
    pub fn from_scale(id: String, scale: f64, unit: f64, corner: (f64, f64), tile: (u32, u32), size: (u32, u32)) -> Option<Matrix> {
        let cell = scale * 0.000_28 / unit;
        let ok = cell.is_finite() && cell > 0.0 && corner.0.is_finite() && corner.1.is_finite();
        let fits = (1..=4096).contains(&tile.0) && (1..=4096).contains(&tile.1) && size.0 > 0 && size.1 > 0;
        (ok && fits && !id.is_empty() && id.len() <= 64).then_some(Matrix {
            id,
            left: corner.0,
            top: corner.1,
            cell,
            tile_w: tile.0,
            tile_h: tile.1,
            cols: size.0,
            rows: size.1,
        })
    }
}

/// How the service gives a layer's pictures.
#[derive(Clone, Debug, PartialEq)]
pub enum Grid {
    /// A WMS GetMap for any area (`{nbbox}`, `{nw}`, `{nh}`); `lat_first` when its bounding box
    /// is written north first (WMS 1.3.0 in a system whose axes run that way).
    Map { lat_first: bool },
    /// A WMTS tile matrix set, coarsest first (`{nm}`, `{ncol}`, `{nrow}`).
    Tiles(Vec<Matrix>),
}

/// A system a WMS service offers a layer in: its EPSG code (4326 for CRS:84 too), its name as
/// the service writes it (the address's `{ncrs}`), and whether a bounding box in it is written
/// north first.
#[derive(Clone, Debug, PartialEq)]
pub struct System {
    pub epsg: u16,
    pub name: String,
    pub lat_first: bool,
}

/// A layer drawn in a coordinate system of its own: the EPSG code of the one a Web Mercator map
/// takes its pictures in (3857 itself for a WMS layer offered so: then they are used as they
/// come), how its pictures are laid out, its area (west, south, east, north) outside which nothing
/// is asked for, and for a WMS layer every system it is offered in.
#[derive(Clone, Debug, PartialEq)]
pub struct Warp {
    pub epsg: u16,
    pub grid: Grid,
    pub area: Option<[f64; 4]>,
    pub systems: Vec<System>,
}

/// Most systems kept of a WMS layer.
pub const MAX_SYSTEMS: usize = 64;

/// The longest name of a system kept in a layer's address (`EPSG:2056`, an OGC URN or URL).
pub const MAX_SYSTEM_NAME: usize = 100;

/// One picture a map tile is made from: its address and the area it shows in the layer's system
/// (west, south, east, north); for a tile of a matrix, its column and row too.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub url: String,
    pub area: [f64; 4],
    pub tile: Option<(u32, u32)>,
}

/// What a map tile is made from: the place in the layer's system of each point of its grid
/// (`GRID + 1` a side, row by row from the top left; `None` where there is none, outside the
/// layer's area or the system's), and the pictures, all in one matrix for a WMTS layer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub points: Vec<Option<(f64, f64)>>,
    pub parts: Vec<Part>,
    /// For a WMTS layer, the matrix the parts are in.
    pub matrix: Option<Matrix>,
}

/// Whether the coordinate system `epsg` can draw a layer: one RustGIS can transform, other than
/// Web Mercator (drawn as it is).
pub fn usable(epsg: u32) -> Option<Crs> {
    let crs = Crs::from_epsg(epsg)?;
    (crs != Crs::WebMercator && transformer(epsg).is_some()).then_some(crs)
}

/// A longitude/latitude box (west, south, east, north) in a system: the bounds of a grid of
/// points over it worked out there (a projected box's corners aren't its extremes, and a world's
/// edges meet at the poles and the antimeridian: its inside says where it is). If a grid point
/// is refused, the box is unknown: surviving points must not exclude imagery inside the area.
pub fn native_box(t: &Transformer, [w, s, e, n]: [f64; 4]) -> Option<[f64; 4]> {
    let mut out: Option<[f64; 4]> = None;
    for i in 0..=8 {
        for j in 0..=8 {
            let (x, y) = (w + (e - w) * f64::from(i) / 8.0, s + (n - s) * f64::from(j) / 8.0);
            let c = t.from_lonlat(Coord::new(x, y)).filter(|c| c.x.is_finite() && c.y.is_finite())?;
            out = Some(out.map_or([c.x, c.y, c.x, c.y], |b| [b[0].min(c.x), b[1].min(c.y), b[2].max(c.x), b[3].max(c.y)]));
        }
    }
    out
}

/// The transformer of a system `epsg` names, made once a code (from two PROJ definitions: a
/// service lists a few codes for each of hundreds of layers, and the map draws many tiles).
pub fn transformer(epsg: u32) -> Option<std::sync::Arc<Transformer>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static MADE: OnceLock<Mutex<HashMap<u32, Option<Arc<Transformer>>>>> = OnceLock::new();
    let mut made = MADE.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner());
    made.entry(epsg).or_insert_with(|| Crs::from_epsg(epsg)?.transformer().ok().map(Arc::new)).clone()
}

/// A tile matrix identifier as it goes into an address: as the capabilities give it, but for
/// what would change the address's path or query (`/`, `?`, `#`, `&`, `%`, spaces), and the
/// braces, so that no `{ncol}` or `{nrow}` in it is filled in later.
pub fn matrix_in_address(id: &str) -> String {
    percent_escape(id, b"/?#&%+{}")
}

/// `s` with each byte of `reserved`, and each that isn't printable ASCII, percent-escaped.
fn percent_escape(s: &str, reserved: &[u8]) -> String {
    s.bytes().map(|b| if b.is_ascii_graphic() && !reserved.contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The EPSG code a service names a coordinate system with: `EPSG:2056`, `CRS:84` (4326, east
/// first), an OGC URN (`urn:ogc:def:crs:EPSG::2056`, `…:OGC:1.3:CRS84`) or URL (`…/EPSG/0/2056`).
pub fn epsg_of(name: &str) -> Option<u32> {
    let n = name.trim();
    let upper = n.to_ascii_uppercase();
    if upper.ends_with("CRS84") || upper.ends_with("CRS:84") {
        return Some(4326);
    }
    if !upper.contains("EPSG") {
        return None;
    }
    let digits: String = n.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().ok().filter(|c| *c > 0)
}

/// Whether a name is OGC's CRS84, longitude first (as against EPSG:4326, latitude first).
pub fn is_crs84(name: &str) -> bool {
    let upper = name.trim().to_ascii_uppercase();
    upper.ends_with("CRS84") || upper.ends_with("CRS:84")
}

impl Warp {
    /// The layer's address: `base` (the service's address with the placeholders) and its fragment.
    pub fn address(&self, base: &str) -> String {
        let mut s = format!("{base}{MARK}{{z}}/{{x}}/{{y}};crs={}", self.epsg);
        if let Some([w, so, e, n]) = self.area {
            s.push_str(&format!(";in={w},{so},{e},{n}"));
        }
        if !self.systems.is_empty() {
            let list: Vec<String> =
                self.systems.iter().map(|c| format!("{}|{}|{}", c.epsg, if c.lat_first { "n" } else { "e" }, escape(&c.name))).collect();
            s.push_str(&format!(";sys={}", list.join("~")));
        }
        match &self.grid {
            Grid::Map { lat_first } => s.push_str(if *lat_first { ";map=north" } else { ";map=east" }),
            Grid::Tiles(ms) => {
                s.push_str(";tiles=");
                let list: Vec<String> = ms
                    .iter()
                    .map(|m| format!("{}|{}|{}|{}|{}|{}|{}|{}", escape(&m.id), m.left, m.top, m.cell, m.tile_w, m.tile_h, m.cols, m.rows))
                    .collect();
                s.push_str(&list.join("~"));
            }
        }
        s
    }

    /// A map tile's address split into the service's address, the tile (z, x, y) and the layer's
    /// layout; `None` when it isn't such an address.
    pub fn parse(key: &str) -> Option<(&str, (u8, u32, u32), Warp)> {
        let (base, spec) = key.split_once(MARK)?;
        let mut fields = spec.split(';');
        let mut zxy = fields.next()?.split('/');
        let z = zxy.next()?.parse().ok()?;
        let x = zxy.next()?.parse().ok()?;
        let y = zxy.next()?.parse().ok()?;
        let (mut epsg, mut grid, mut area, mut systems) = (None, None, None, Vec::new());
        for f in fields {
            let (k, v) = f.split_once('=')?;
            match k {
                "crs" => epsg = v.parse().ok(),
                "in" => {
                    let n: Vec<f64> = v.split(',').filter_map(|t| t.parse().ok()).collect();
                    area = match n.as_slice() {
                        [w, s, e, n] if [*w, *s, *e, *n].iter().all(|v| v.is_finite()) && w < e && s < n => Some([*w, *s, *e, *n]),
                        _ => None,
                    };
                }
                "map" => grid = Some(Grid::Map { lat_first: v == "north" }),
                "sys" => {
                    systems = v
                        .split('~')
                        .take(MAX_SYSTEMS)
                        .filter_map(|c| {
                            let mut p = c.splitn(3, '|');
                            let epsg = p.next()?.parse().ok()?;
                            let lat_first = p.next()? == "n";
                            let name = crate::address::percent_decode(p.next()?);
                            (!name.is_empty() && name.len() <= MAX_SYSTEM_NAME).then_some(System { epsg, name, lat_first })
                        })
                        .collect();
                }
                "tiles" => {
                    let mut ms = Vec::new();
                    for m in v.split('~').take(MAX_MATRICES) {
                        let p: Vec<&str> = m.split('|').collect();
                        let [id, left, top, cell, tw, th, cols, rows] = p.as_slice() else { return None };
                        let n = |t: &str| t.parse::<f64>().ok().filter(|v| v.is_finite());
                        let u = |t: &str| t.parse::<u32>().ok().filter(|v| *v > 0);
                        let m = Matrix {
                            id: crate::address::percent_decode(id),
                            left: n(left)?,
                            top: n(top)?,
                            cell: n(cell).filter(|c| *c > 0.0)?,
                            tile_w: u(tw).filter(|v| *v <= 4096)?,
                            tile_h: u(th).filter(|v| *v <= 4096)?,
                            cols: u(cols)?,
                            rows: u(rows)?,
                        };
                        ms.push(m);
                    }
                    if ms.is_empty() {
                        return None;
                    }
                    grid = Some(Grid::Tiles(ms));
                }
                _ => {}
            }
        }
        let epsg: u16 = epsg?;
        Some((base, (z, x, y), Warp { epsg, grid: grid?, area, systems }))
    }

    /// The Web Mercator zooms a WMTS layer in another system is drawn at: down to the one about
    /// as fine as its finest matrix, out to the one whose tiles take at most
    /// [`MAX_COARSEST_PARTS`] of its coarsest's. `None` for a WMS layer (its scale hints say).
    pub fn zooms(&self) -> Option<(u8, u8)> {
        let Grid::Tiles(ms) = &self.grid else { return None };
        let (coarsest, finest) = (ms.first()?, ms.last()?);
        let unit = metres_per_unit(u32::from(self.epsg));
        let metres = |cell: f64| cell * unit;
        // Web Mercator's pixel at zoom 0, 256 to a tile.
        let z0 = 2.0 * std::f64::consts::PI * 6_378_137.0 / 256.0;
        let zoom = |res: f64| (z0 / res).log2();
        let max = zoom(metres(finest.cell)).round().clamp(0.0, 24.0) as u8;
        let side = (MAX_COARSEST_PARTS as f64).sqrt();
        let min = (zoom(metres(coarsest.cell) * side) - 0.05).ceil().clamp(0.0, 24.0) as u8;
        Some((min.min(max), max))
    }

    /// The system named `epsg` the layer is offered in (a WMS layer's).
    pub fn system(&self, epsg: u16) -> Option<&System> {
        self.systems.iter().find(|c| c.epsg == epsg)
    }

    /// A WMS picture of `area` (west, south, east, north in `system`'s units), `w` × `h` pixels.
    pub fn map_address(base: &str, system: &System, area: [f64; 4], w: u32, h: u32) -> String {
        let bbox = if system.lat_first {
            format!("{},{},{},{}", area[1], area[0], area[3], area[2])
        } else {
            format!("{},{},{},{}", area[0], area[1], area[2], area[3])
        };
        base.replace("{ncrs}", &encode(&system.name))
            .replace("{nbbox}", &bbox)
            .replace("{nw}", &w.to_string())
            .replace("{nh}", &h.to_string())
    }

    /// The service's address for pictures in any of the layer's systems (`{ncrs}`, `{nbbox}`,
    /// `{nw}`, `{nh}`), from the layer's address before it is filled in for a tile: a WMS layer
    /// drawn in Web Mercator asks with `EPSG:3857` and `{bbox-epsg-3857}`, which become those.
    pub fn map_template(base: &str) -> String {
        // Its pictures' size whatever its tiles' (256 pixels, or another the layer was given).
        let base = base.replace("CRS=EPSG:3857&", "CRS={ncrs}&").replace("SRS=EPSG:3857&", "SRS={ncrs}&");
        if !base.contains("BBOX={bbox-epsg-3857}") {
            return base;
        }
        sized(&base, "{nw}", "{nh}").replace("BBOX={bbox-epsg-3857}", "BBOX={nbbox}")
    }

    /// What the map tile over `merc` (west, south, east, north in Web Mercator metres) is made
    /// of, `size` pixels a side, through `t` (the layer's system to and from longitude/latitude);
    /// `base` is the service's address. An empty plan when the tile is outside the layer.
    pub fn plan(&self, base: &str, merc: [f64; 4], size: u32, t: &Transformer) -> Plan {
        let [w, s, e, n] = merc;
        // A little past the layer's area, so its edge isn't cut by the interpolation.
        let inside = |ll: Coord| {
            self.area.is_none_or(|[aw, as_, ae, an]| {
                let (mx, my) = ((ae - aw) * 0.05 + 0.01, (an - as_) * 0.05 + 0.01);
                ll.x >= aw - mx && ll.x <= ae + mx && ll.y >= as_ - my && ll.y <= an + my
            })
        };
        let grid = |keep: &dyn Fn(Coord) -> bool| {
            let mut points = Vec::with_capacity((GRID + 1) * (GRID + 1));
            let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for j in 0..=GRID {
                for i in 0..=GRID {
                    let m = Coord::new(w + (e - w) * i as f64 / GRID as f64, n - (n - s) * j as f64 / GRID as f64);
                    let ll = merc_to_lonlat(m);
                    let p = keep(ll).then(|| t.from_lonlat(ll)).flatten().filter(|c| c.is_finite()).map(|c| (c.x, c.y));
                    if let Some((x, y)) = p {
                        (lo_x, lo_y, hi_x, hi_y) = (lo_x.min(x), lo_y.min(y), hi_x.max(x), hi_y.max(y));
                    }
                    points.push(p);
                }
            }
            (points, [lo_x, lo_y, hi_x, hi_y])
        };
        let (mut points, [mut lo_x, mut lo_y, mut hi_x, mut hi_y]) = grid(&inside);
        // A layer's area small enough to fall between the grid's points (a town, far out): the
        // tile's points all, when the tile and the area meet.
        if !(lo_x < hi_x && lo_y < hi_y) {
            let (sw, ne) = (merc_to_lonlat(Coord::new(w, s)), merc_to_lonlat(Coord::new(e, n)));
            let meets = self.area.is_some_and(|[aw, as_, ae, an]| aw <= ne.x && ae >= sw.x && as_ <= ne.y && an >= sw.y);
            if !meets {
                return Plan::default();
            }
            (points, [lo_x, lo_y, hi_x, hi_y]) = grid(&|_| true);
            if !(lo_x < hi_x && lo_y < hi_y) {
                return Plan::default();
            }
        }
        // The area each grid cell spans past the outermost points, so the pictures cover them.
        let (pad_x, pad_y) = ((hi_x - lo_x) / GRID as f64, (hi_y - lo_y) / GRID as f64);
        let area = [lo_x - pad_x, lo_y - pad_y, hi_x + pad_x, hi_y + pad_y];
        match &self.grid {
            Grid::Map { lat_first } => {
                // About as many pixels as the map tile has, in the area's own proportions.
                let ratio = ((area[2] - area[0]) / (area[3] - area[1])).clamp(0.25, 4.0);
                let px = f64::from(size.clamp(64, 1024));
                let (nw, nh) = ((px * ratio.sqrt()).round().clamp(32.0, 2048.0), (px / ratio.sqrt()).round().clamp(32.0, 2048.0));
                let system = self.system(self.epsg).cloned().unwrap_or_else(|| System {
                    epsg: self.epsg,
                    name: format!("EPSG:{}", self.epsg),
                    lat_first: *lat_first,
                });
                let url = Warp::map_address(base, &system, area, nw as u32, nh as u32);
                Plan { points, parts: vec![Part { url, area, tile: None }], matrix: None }
            }
            Grid::Tiles(ms) => {
                // The coarsest matrix with as much detail as the map tile, else the finest.
                let needed = (area[2] - area[0]).max(area[3] - area[1]) / f64::from(size.max(1));
                let mut i = ms.iter().position(|m| m.cell <= needed * 1.25).unwrap_or(ms.len().saturating_sub(1));
                loop {
                    let Some(m) = ms.get(i) else { return Plan::default() };
                    let (tw, th) = (m.cell * f64::from(m.tile_w), m.cell * f64::from(m.tile_h));
                    let col = |x: f64| ((x - m.left) / tw).floor();
                    let row = |y: f64| ((m.top - y) / th).floor();
                    let (mut c0, mut c1) = (col(area[0]).max(0.0), col(area[2]).min(f64::from(m.cols) - 1.0));
                    let (mut r0, mut r1) = (row(area[3]).max(0.0), row(area[1]).min(f64::from(m.rows) - 1.0));
                    if c0 > c1 || r0 > r1 {
                        return Plan { points, parts: Vec::new(), matrix: Some(m.clone()) };
                    }
                    let count = (c1 - c0 + 1.0) * (r1 - r0 + 1.0);
                    // Too many for one map tile: a coarser matrix; of the coarsest, more, about the
                    // tile's middle.
                    if count > MAX_PARTS as f64 && i > 0 {
                        i -= 1;
                        continue;
                    }
                    if count > MAX_COARSEST_PARTS as f64 {
                        let half = ((MAX_COARSEST_PARTS as f64).sqrt() / 2.0).floor();
                        let (mc, mr) = (((c0 + c1) / 2.0).floor(), ((r0 + r1) / 2.0).floor());
                        (c0, c1) = ((mc - half).max(c0), (mc + half - 1.0).min(c1));
                        (r0, r1) = ((mr - half).max(r0), (mr + half - 1.0).min(r1));
                    }
                    let id = matrix_in_address(&m.id);
                    let mut parts = Vec::new();
                    for r in (r0 as u32)..=(r1 as u32) {
                        for c in (c0 as u32)..=(c1 as u32) {
                            let x0 = m.left + f64::from(c) * tw;
                            let y1 = m.top - f64::from(r) * th;
                            let url = base.replace("{nm}", &id).replace("{ncol}", &c.to_string()).replace("{nrow}", &r.to_string());
                            parts.push(Part { url, area: [x0, y1 - th, x0 + tw, y1], tile: Some((c, r)) });
                        }
                    }
                    return Plan { points, parts, matrix: Some(m.clone()) };
                }
            }
        }
    }
}

/// A matrix identifier in an address's fragment, its separators escaped.
fn escape(id: &str) -> String {
    percent_escape(id, b"%|~;#{}")
}

/// Whether a layer's box from a capabilities document (a WMS `BoundingBox`: `minx`, `miny`,
/// `maxx`, `maxy` as written) is written north first, for a system whose EPSG axis order RustGIS
/// doesn't know (see [`crate::raster::crs::epsg_north_first`]): the way that overlaps more of `area` (the
/// layer's area worked out in the system: west, south, east, north), so a parent's or a world's
/// box over a small layer is read right too; overlapping as much either way (a box holding the
/// area both ways), the way whose middle is nearer the area's; else as the system's axes usually
/// run (latitude first in a geographic system, east first in a projected one).
pub fn north_first(b: [f64; 4], area: Option<[f64; 4]>, geographic: bool) -> bool {
    let Some([w, s, e, n]) = area else { return geographic };
    let overlap = |x0: f64, y0: f64, x1: f64, y1: f64| {
        let (dx, dy) = (x1.min(e) - x0.max(w), y1.min(n) - y0.max(s));
        if dx > 0.0 && dy > 0.0 { dx * dy } else { 0.0 }
    };
    let (east, north) = (overlap(b[0], b[1], b[2], b[3]), overlap(b[1], b[0], b[3], b[2]));
    if north != east {
        return north > east;
    }
    let off = |x0: f64, y0: f64, x1: f64, y1: f64| ((x0 + x1 - w - e) / 2.0).hypot((y0 + y1 - s - n) / 2.0);
    let (east, north) = (off(b[0], b[1], b[2], b[3]), off(b[1], b[0], b[3], b[2]));
    if north == east { geographic } else { north < east }
}

/// How much of a layer's area in the system (west, south, east, north) a WMTS tile matrix covers
/// with its `TopLeftCorner` read east first and read north first (`span`: the matrix's width and
/// height in the system's units). `None` without an area.
pub fn corner_coverage(pair: (f64, f64), span: (f64, f64), area: Option<[f64; 4]>) -> Option<(f64, f64)> {
    let [w, s, e, n] = area?;
    let covered = |x0: f64, y1: f64| {
        let dx = (x0 + span.0).min(e) - x0.max(w);
        let dy = y1.min(n) - (y1 - span.1).max(s);
        if dx > 0.0 && dy > 0.0 { dx * dy } else { 0.0 }
    };
    Some((covered(pair.0, pair.1), covered(pair.1, pair.0)))
}

/// Whether a WMTS tile matrix's `TopLeftCorner` is written north first, by its geometry, for a
/// system whose EPSG axis order RustGIS doesn't know (see [`crate::raster::crs::epsg_north_first`]): the
/// order whose matrix covers more of the layer (see [`corner_coverage`]). `None` without an area,
/// or covering as much either way. (Only a guess: a layer's box reaching a little past the
/// corner tips it.)
pub fn corner_north_first(pair: (f64, f64), span: (f64, f64), area: Option<[f64; 4]>) -> Option<bool> {
    let (east, north) = corner_coverage(pair, span, area)?;
    if north > east {
        Some(true)
    } else if east > north {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WMS's size is its WIDTH and HEIGHT (a vendor `size` kept), a map service export's its
    /// `size`; placeholders and the warp mark are kept; a WMS address of any tile size is a
    /// template for any picture.
    #[test]
    fn addresses_sized() {
        let wms = "https://m.org/wms?LAYERS=a&width=256&HEIGHT=256&size=big&BBOX={bbox-epsg-3857}#rustgis-warp={z}/{x}/{y};crs=3857";
        assert_eq!(
            sized(wms, "1024", "1024"),
            "https://m.org/wms?LAYERS=a&width=1024&HEIGHT=1024&size=big&BBOX={bbox-epsg-3857}#rustgis-warp={z}/{x}/{y};crs=3857"
        );
        let export = "https://s.org/arcgis/rest/services/x/MapServer/export?bbox={bbox-epsg-3857}&size=256,256&width=9";
        assert_eq!(
            sized(export, "512", "512"),
            "https://s.org/arcgis/rest/services/x/MapServer/export?bbox={bbox-epsg-3857}&size=512,512&width=9"
        );
        assert_eq!(sized("https://m.org/wms?WIDTH={width}", "1", "2"), "https://m.org/wms?WIDTH={width}");
        assert_eq!(sized("https://m.org/tile.png", "1", "2"), "https://m.org/tile.png");
        let big = "https://m.org/wms?CRS=EPSG:3857&BBOX={bbox-epsg-3857}&WIDTH=1024&HEIGHT=1024&FORMAT=image%2Fpng";
        assert_eq!(Warp::map_template(big), "https://m.org/wms?CRS={ncrs}&BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}&FORMAT=image%2Fpng");
    }

    #[test]
    fn names_of_systems() {
        assert_eq!(epsg_of("EPSG:2056"), Some(2056));
        assert_eq!(epsg_of("urn:ogc:def:crs:EPSG::31287"), Some(31287));
        assert_eq!(epsg_of("urn:ogc:def:crs:EPSG:6.18:3:3857"), Some(3857));
        assert_eq!(epsg_of("http://www.opengis.net/def/crs/EPSG/0/4258"), Some(4258));
        assert_eq!(epsg_of("CRS:84"), Some(4326));
        assert_eq!(epsg_of("urn:ogc:def:crs:OGC:1.3:CRS84"), Some(4326));
        assert!(is_crs84("CRS:84") && !is_crs84("EPSG:4326"));
        assert_eq!(epsg_of("AUTO:42001"), None);
        assert!(usable(2056).is_some() && usable(4326).is_some());
        assert!(usable(3857).is_none(), "Web Mercator is drawn as it is");
    }

    /// A partial projected box must not cull valid imagery inside the service's geographic area.
    #[test]
    fn refused_grid_points_do_not_make_a_partial_culling_box() {
        let t = transformer(32617).unwrap();
        let view = [-170.0, 0.0, 10.0, 50.0];
        let place = t.from_lonlat(Coord::new(-170.0, 10.5)).unwrap();
        assert!(t.from_lonlat(Coord::new(-170.0, 0.0)).is_none());
        let bounds = native_box(&t, view);
        assert!(bounds.is_none(), "partial bounds {bounds:?} can cull valid imagery at {place:?}");
    }

    #[test]
    fn addresses_round_trip() {
        let m = Matrix::from_scale("EPSG:2056:20|x".into(), 3571.4285714, 1.0, (2_420_000.0, 1_350_000.0), (256, 256), (938, 625)).unwrap();
        assert!((m.cell - 1.0).abs() < 1e-6, "{}", m.cell);
        let w = Warp { epsg: 2056, grid: Grid::Tiles(vec![m.clone()]), area: Some([5.9, 45.8, 10.5, 47.8]), systems: Vec::new() };
        let address = w.address("https://t.example.org/{nm}/{nrow}/{ncol}.png");
        let filled = address.replace("{z}", "12").replace("{x}", "2138").replace("{y}", "1447");
        let (base, zxy, back) = Warp::parse(&filled).unwrap();
        assert_eq!((base, zxy), ("https://t.example.org/{nm}/{nrow}/{ncol}.png", (12, 2138, 1447)));
        assert_eq!(back, w);
        let wms = Warp {
            epsg: 4326,
            grid: Grid::Map { lat_first: true },
            area: None,
            systems: vec![
                System { epsg: 4326, name: "EPSG:4326".into(), lat_first: true },
                System { epsg: 3857, name: "EPSG:3857".into(), lat_first: false },
            ],
        };
        let filled = wms.address("https://m.example.org/wms?BBOX={nbbox}").replace("{z}/{x}/{y}", "1/0/0");
        let (base, _, back) = Warp::parse(&filled).unwrap();
        assert_eq!((base, back), ("https://m.example.org/wms?BBOX={nbbox}", wms));
        assert!(Warp::parse("https://x.example.org/{z}/{x}/{y}.png").is_none());
        assert!(Warp::parse("https://x.example.org/a#rustgis-warp=1/0/0;crs=2056").is_none(), "no layout");
        assert_eq!(matrix_in_address("EPSG:2056:{ncol}/a b"), "EPSG:2056:%7Bncol%7D%2Fa%20b");
        // A world's box in Europe's LAEA reaches west of its centre (10° E): Iberia is in it.
        let world = native_box(&transformer(3035).unwrap(), [-180.0, -90.0, 180.0, 90.0]).unwrap();
        assert!(world[0] < 3_000_000.0 && world[2] > 5_500_000.0, "{world:?}");
        // A State Plane system in US survey feet: its cells in feet.
        assert!((metres_per_unit(2264) - 1200.0 / 3937.0).abs() < 1e-12, "{}", metres_per_unit(2264));
        assert!((metres_per_unit(2056) - 1.0).abs() < 1e-12 && (metres_per_unit(4326) - METRES_PER_DEGREE).abs() < 1e-6);
    }

    /// A map tile over Bern, from swisstopo's LV95 (EPSG:2056) matrices: the one nearest its
    /// detail, the tiles covering it; and from a WMS, one GetMap of its area.
    #[test]
    fn plans_for_a_map_tile() {
        let t = usable(2056).unwrap().transformer().unwrap();
        // Zoom 14 over Bern (7.44°E, 46.95°N).
        let (z, x, y) = (14u32, 8530u32, 5764u32);
        let span = 2.0 * 20_037_508.342_789_244 / f64::from(1u32 << z);
        let (w, n) = (-20_037_508.342_789_244 + f64::from(x) * span, 20_037_508.342_789_244 - f64::from(y) * span);
        let merc = [w, n - span, w + span, n];
        // Matrices of 4, 2 and 1 m pixels.
        let ms: Vec<Matrix> = [4.0, 2.0, 1.0]
            .iter()
            .enumerate()
            .map(|(i, cell)| Matrix {
                id: i.to_string(),
                left: 2_420_000.0,
                top: 1_350_000.0,
                cell: *cell,
                tile_w: 256,
                tile_h: 256,
                cols: 2000,
                rows: 2000,
            })
            .collect();
        let tiles = Warp { epsg: 2056, grid: Grid::Tiles(ms), area: Some([5.9, 45.8, 10.5, 47.8]), systems: Vec::new() };
        let plan = tiles.plan("https://t.example.org/{nm}/{nrow}/{ncol}.png", merc, 256, &t);
        assert_eq!(plan.points.len(), (GRID + 1) * (GRID + 1));
        assert!(plan.points.iter().all(Option::is_some));
        // The tile is about 1.7 km across at that latitude (6.6 m a pixel): the 4 m matrix, whose
        // tiles are 1 km, a few of them.
        let m = plan.matrix.as_ref().unwrap();
        assert_eq!(m.id, "0");
        assert!((1..=9).contains(&plan.parts.len()), "{}", plan.parts.len());
        assert!(plan.parts[0].url.starts_with("https://t.example.org/0/"), "{}", plan.parts[0].url);
        // Bern's LV95 east and north lie within the parts.
        let (e, nn) = (2_600_000.0, 1_200_000.0);
        assert!(
            plan.parts
                .iter()
                .any(|p| p.area[0] - 3000.0 <= e && e <= p.area[2] + 3000.0 && p.area[1] - 3000.0 <= nn && nn <= p.area[3] + 3000.0)
        );
        // A set whose coarsest matrix is far finer than a zoom 8 tile: at most so many of it,
        // about the tile's middle, not its corner.
        let fine = Warp {
            epsg: 2056,
            grid: Grid::Tiles(vec![Matrix {
                id: "f/1".into(),
                left: 2_420_000.0,
                top: 1_350_000.0,
                cell: 1.0,
                tile_w: 256,
                tile_h: 256,
                cols: 5000,
                rows: 5000,
            }]),
            area: Some([5.9, 45.8, 10.5, 47.8]),
            systems: Vec::new(),
        };
        let z8 = {
            let s8 = 2.0 * 20_037_508.342_789_244 / 256.0;
            let (x, y) = (133.0, 90.0);
            let (w, n) = (-20_037_508.342_789_244 + x * s8, 20_037_508.342_789_244 - y * s8);
            [w, n - s8, w + s8, n]
        };
        let plan = fine.plan("https://t.example.org/{nm}/{nrow}/{ncol}.png", z8, 256, &t);
        assert!(plan.parts.len() <= MAX_COARSEST_PARTS && plan.parts.len() > MAX_PARTS, "{}", plan.parts.len());
        // Its identifier, with a slash, kept to one path segment.
        assert!(plan.parts[0].url.starts_with("https://t.example.org/f%2F1/"), "{}", plan.parts[0].url);
        let (cols, rows): (Vec<u32>, Vec<u32>) = plan.parts.iter().filter_map(|p| p.tile).unzip();
        let mid = |v: &[u32]| (v.iter().min().unwrap() + v.iter().max().unwrap()) / 2;
        let (all_c0, all_c1) = (
            ((plan.points.iter().flatten().map(|p| p.0).fold(f64::MAX, f64::min) - 2_420_000.0) / 256.0) as u32,
            ((plan.points.iter().flatten().map(|p| p.0).fold(f64::MIN, f64::max) - 2_420_000.0) / 256.0) as u32,
        );
        assert!(mid(&cols).abs_diff((all_c0 + all_c1) / 2) <= 2 && !rows.is_empty(), "about the middle");
        // Outside the layer's area: nothing to ask for.
        let far = [0.0, 0.0, span, span];
        assert!(tiles.plan("https://t.example.org/{nm}/{nrow}/{ncol}.png", far, 256, &t).parts.is_empty());
        // A WMS layer: one picture of the tile's area, its box east first.
        let map = Warp { epsg: 2056, grid: Grid::Map { lat_first: false }, area: None, systems: Vec::new() };
        let plan = map.plan("https://m.example.org/wms?CRS={ncrs}&BBOX={nbbox}&WIDTH={nw}&HEIGHT={nh}", merc, 256, &t);
        assert!(plan.parts[0].url.starts_with("https://m.example.org/wms?CRS=EPSG:2056&"), "{}", plan.parts[0].url);
        let [aw, as_, ae, an] = plan.parts[0].area;
        assert!(aw > 2_590_000.0 && ae < 2_610_000.0 && as_ > 1_190_000.0 && an < 1_210_000.0, "{:?}", plan.parts[0].area);
        assert!(plan.parts[0].url.contains(&format!("BBOX={aw},{as_},{ae},{an}&WIDTH=")), "{}", plan.parts[0].url);
        // Latitude first: the box is written south, west, north, east.
        let ll = usable(4326).unwrap().transformer().unwrap();
        let geo = Warp { epsg: 4326, grid: Grid::Map { lat_first: true }, area: None, systems: Vec::new() };
        let plan = geo.plan("https://m.example.org/wms?BBOX={nbbox}", merc, 256, &ll);
        let [aw, as_, ae, an] = plan.parts[0].area;
        assert!(plan.parts[0].url.ends_with(&format!("BBOX={as_},{aw},{an},{ae}")), "{}", plan.parts[0].url);
    }

    /// A WMTS layer in another system is drawn down to about its finest matrix's detail, out
    /// to where its coarsest one's tiles still fill a map tile: swisstopo's LV95 set, 4 km to
    /// 10 cm pixels, is zooms 3 to 21.
    #[test]
    fn zooms_of_a_set_in_another_system() {
        let m = |cell: f64| Matrix {
            id: format!("{cell}"),
            left: 2_420_000.0,
            top: 1_350_000.0,
            cell,
            tile_w: 256,
            tile_h: 256,
            cols: 10,
            rows: 10,
        };
        let lv95 = Warp { epsg: 2056, grid: Grid::Tiles(vec![m(4000.0), m(10.0), m(0.1)]), area: None, systems: Vec::new() };
        assert_eq!(lv95.zooms(), Some((3, 21)));
        let wms = Warp { epsg: 2056, grid: Grid::Map { lat_first: false }, area: None, systems: Vec::new() };
        assert_eq!(wms.zooms(), None);
    }

    /// A layer's address from a project is hostile input: junk fragments are refused, not a
    /// panic.
    #[test]
    fn junk_fragments_are_refused() {
        for junk in [
            "https://x.org/#rustgis-warp=",
            "https://x.org/#rustgis-warp=99999999999/0/0;crs=2056;map=east",
            "https://x.org/#rustgis-warp=1/0/0;crs=99999999;map=east",
            "https://x.org/#rustgis-warp=1/0/0;crs=2056;tiles=a|NaN|1|1|256|256|1|1",
            "https://x.org/#rustgis-warp=1/0/0;crs=2056;tiles=a|0|0|0|256|256|1|1",
            "https://x.org/#rustgis-warp=1/0/0;crs=2056;tiles=a|0|0|1|0|256|1|1",
            "https://x.org/#rustgis-warp=1/0/0;crs=2056;tiles=a|0|0|1|256|256|0|1",
            "https://x.org/#rustgis-warp=1/0/0;crs=2056;tiles=",
            "https://x.org/#rustgis-warp=1/0/0;crs=2056;in=1,2,x;map=east;sys=x|y",
            "https://x.org/#rustgis-warp=a/b/c;crs=2056;map=east",
        ] {
            let parsed = Warp::parse(junk);
            assert!(parsed.as_ref().is_none_or(|(_, _, w)| w.area.is_none() && w.systems.is_empty()), "{junk}: {parsed:?}");
        }
    }

    #[test]
    fn corners_by_axis_order() {
        // A Europe-wide LAEA (EPSG:3035) matrix written north first (its corner 2.0e6 E, 5.5e6 N),
        // over Austria (4.3e6 to 4.9e6 E, 2.6e6 to 2.9e6 N): only north first covers it.
        let austria = Some([4_300_000.0, 2_600_000.0, 4_900_000.0, 2_900_000.0]);
        assert_eq!(corner_north_first((5_500_000.0, 2_000_000.0), (5_000_000.0, 4_000_000.0), austria), Some(true));
        // swisstopo's LV95 matrix, east first, over a box reaching past its corner's west.
        let switzerland = Some([2_380_000.0, 1_060_000.0, 2_870_000.0, 1_310_000.0]);
        assert_eq!(corner_north_first((2_420_000.0, 1_350_000.0), (480_000.0, 320_000.0), switzerland), Some(false));
        assert_eq!(corner_north_first((90.0, -180.0), (360.0, 180.0), None), None);
        // swisstopo's LV95 box, east first; an Austrian one written north first.
        assert!(!north_first([2_480_000.0, 1_070_000.0, 2_840_000.0, 1_300_000.0], switzerland, false));
        assert!(north_first([2_600_000.0, 4_300_000.0, 2_900_000.0, 4_900_000.0], austria, false));
        // A world box latitude first over a small layer (lon 120..130, lat 30..40): the reading
        // nearer its corner would be east first; the one that holds the layer is north first.
        assert!(north_first([-90.0, -180.0, 90.0, 180.0], Some([120.0, 30.0, 130.0, 40.0]), true));
        assert!(!north_first([-180.0, -90.0, 180.0, 90.0], Some([120.0, 30.0, 130.0, 40.0]), true));
        // A Europe-wide LAEA box, north first, holding a German layer either way: its middle says.
        let germany = Some([4_000_000.0, 2_650_000.0, 4_720_000.0, 3_560_000.0]);
        assert!(north_first([1_400_000.0, 2_500_000.0, 5_500_000.0, 7_400_000.0], germany, false));
        assert!(!north_first([2_500_000.0, 1_400_000.0, 7_400_000.0, 5_500_000.0], germany, false));
        assert!(north_first([-90.0, -180.0, 90.0, 180.0], None, true));
        assert!(!north_first([-180.0, -90.0, 180.0, 90.0], None, false));
    }
}
