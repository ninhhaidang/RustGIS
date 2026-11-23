//! DXF: AutoCAD's drawing exchange format (ASCII), as points, lines and polygons layers.
//!
//! The ENTITIES section is read, with blocks inserted where INSERTs place them (scaled, rotated,
//! in arrays, nested to a bounded depth): POINT, LINE, LWPOLYLINE and POLYLINE (bulges drawn as
//! arcs; polyface meshes as polygons), CIRCLE, ARC, ELLIPSE, SPLINE, LEADER, TEXT, MTEXT and
//! ATTRIB (points with their text), HATCH boundaries, SOLID, TRACE and 3DFACE (polygons), and the
//! blocks of DIMENSIONs. Entities in an object's own coordinate system (an extrusion direction)
//! are turned into the drawing's. Each feature keeps its layer, entity type, colour (ACI, BYLAYER
//! and BYBLOCK resolved), linetype, text and elevation.
//!
//! A drawing doesn't say its coordinate system: one given converts the coordinates to
//! longitude/latitude; without one they're kept as they are and the report says so. Its text is
//! UTF-8 (2007 and later), else the ANSI code page its header names (`$DWGCODEPAGE`), else Latin-1,
//! which a report names. Whether the contents of its blocks (图块) are drawn is the caller's
//! choice ([`Options::blocks`]). Binary DXF and DWG aren't read here.
//!
//! The file is hostile: counts are bounded, block nesting is cut off (and cycles refused), and an
//! entity that can't be read is skipped and counted.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;

use rustgis_geom::Coord;

use crate::raster::Crs;
use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

/// DXF file types.
pub const EXTENSIONS: &[&str] = &["dxf"];

/// The most features read from one drawing (blocks inserted many times multiply quickly).
const MAX_FEATURES: usize = if cfg!(target_arch = "wasm32") { 500_000 } else { 2_000_000 };
/// The deepest blocks nest within blocks.
const MAX_DEPTH: usize = 8;
/// The most vertices one entity has.
const MAX_VERTICES: usize = 1_000_000;
/// The most copies one INSERT's array makes.
const MAX_ARRAY: usize = 10_000;
/// The most copies nested arrays make together along one nesting path (an array of arrays
/// multiplies).
const MAX_PATH_COPIES: usize = 100_000;
/// The most entity placements one drawing's expansion visits, features made or not (inserts of
/// empty or filtered blocks still cost work).
const MAX_STEPS: usize = if cfg!(target_arch = "wasm32") { 2_000_000 } else { 8_000_000 };
/// The most pieces (vertices, knots, edges) a count in the file may ask for.
const MAX_COUNT: usize = 1_000_000;
/// The most coordinates one drawing's features hold together (a block of a large polyline inserted
/// in an array multiplies them).
/// (Each costs about 40 bytes while read: 20M is under a gigabyte. Tests use a smaller budget.)
const MAX_TOTAL_VERTICES: usize = if cfg!(test) {
    2_000_000
} else if cfg!(target_arch = "wasm32") {
    5_000_000
} else {
    20_000_000
};
/// Segments of a full circle.
const CIRCLE: f64 = 72.0;

/// What to read: the coordinate system of a drawing (which never says), one DXF layer only,
/// whether the layouts' entities (paper space) are drawn with the model's, and whether the
/// contents of inserted blocks (图块) are drawn.
#[derive(Clone, Copy, Debug)]
pub struct Options<'a> {
    pub crs: Option<Crs>,
    pub layer: Option<&'a str>,
    pub paper_space: bool,
    /// Whether INSERTs and DIMENSIONs draw the contents of the blocks they place. `false` leaves
    /// them out, counted in the report; what the drawing draws without blocks is kept.
    pub blocks: bool,
}

impl Default for Options<'_> {
    fn default() -> Self {
        // A drawing's blocks are part of what it draws: drawn unless asked otherwise.
        Options { crs: None, layer: None, paper_space: false, blocks: true }
    }
}

pub fn is_dxf_name(name: &str) -> bool {
    let ext = std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// Whether bytes are a binary DXF (not read here).
fn is_binary(bytes: &[u8]) -> bool {
    bytes.starts_with(b"AutoCAD Binary DXF")
}

/// The text of a drawing and what had to be assumed to read it: UTF-8 (AutoCAD 2007 and later),
/// else the ANSI code page its header names ([`code_page`], the `$DWGCODEPAGE` of older drawings),
/// else Latin-1.
fn decode(bytes: &[u8]) -> (Cow<'_, str>, Option<String>) {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (Cow::Borrowed(s), None);
    }
    let page = code_page(bytes);
    let Some(encoding) = page.as_deref().and_then(encoding_of) else {
        let said = match page.as_deref() {
            Some(p) => format!(
                "the drawing's text isn't UTF-8 and its code page '{p}' isn't one RustGIS knows: it is read as Latin-1, so non-ASCII text (such as layer names) may be wrong"
            ),
            None => "the drawing's text isn't UTF-8 and it names no code page ($DWGCODEPAGE): it is read as Latin-1, so non-ASCII text (such as layer names) may be wrong".to_string(),
        };
        return (Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect()), Some(said));
    };
    let (text, _) = encoding.decode_without_bom_handling(bytes);
    let lost = text.matches('\u{fffd}').count();
    let said = (lost > 0).then(|| {
        format!(
            "{lost} characters of the drawing's text couldn't be read as {} (its code page): they are drawn as a replacement character",
            encoding.name()
        )
    });
    (text, said)
}

/// The code page a drawing's header gives (`$DWGCODEPAGE`, such as `ANSI_936`), read from the
/// first megabyte: a header is small, and a file that isn't a drawing isn't searched further.
/// Blank code lines are skipped without consuming a value, as in [`records`].
fn code_page(bytes: &[u8]) -> Option<String> {
    let head = bytes.get(..bytes.len().min(1 << 20)).unwrap_or(bytes);
    let mut lines = head.split(|&b| b == b'\n');
    let (mut section, mut header, mut page) = (false, false, false);
    while let Some(code) = lines.next() {
        let code = trim_ascii(code);
        if code.is_empty() {
            continue;
        }
        let value = trim_ascii(lines.next()?);
        let code = std::str::from_utf8(code).ok().and_then(|s| s.parse::<u16>().ok());
        match code {
            Some(999) => continue, // Comments never name a section or header variable.
            Some(0) => {
                section = value.eq_ignore_ascii_case(b"SECTION");
                header = false;
                page = false;
            }
            Some(2) if section => {
                header = value.eq_ignore_ascii_case(b"HEADER");
                section = false;
            }
            Some(9) if header => page = value.eq_ignore_ascii_case(b"$DWGCODEPAGE"),
            Some(3) if header && page => return Some(String::from_utf8_lossy(value).into_owned()),
            _ => {}
        }
    }
    None
}

/// ASCII whitespace off both ends of a line of raw bytes.
fn trim_ascii(mut b: &[u8]) -> &[u8] {
    while let Some((first, rest)) = b.split_first() {
        if !first.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    while let Some((last, rest)) = b.split_last() {
        if !last.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    b
}

/// The encoding a DXF code page names (`ANSI_936` is GBK, `ANSI_950` Big5, …): the ANSI and DOS
/// code pages AutoCAD writes in `$DWGCODEPAGE`.
fn encoding_of(page: &str) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::*;
    Some(match page.trim().to_ascii_uppercase().as_str() {
        "ANSI_874" => WINDOWS_874,
        "ANSI_932" => SHIFT_JIS,
        "ANSI_936" => GBK,
        "ANSI_949" => EUC_KR,
        "ANSI_950" => BIG5,
        "ANSI_1250" => WINDOWS_1250,
        "ANSI_1251" => WINDOWS_1251,
        "ANSI_1252" => WINDOWS_1252,
        "ANSI_1253" => WINDOWS_1253,
        "ANSI_1254" => WINDOWS_1254,
        "ANSI_1255" => WINDOWS_1255,
        "ANSI_1256" => WINDOWS_1256,
        "ANSI_1257" => WINDOWS_1257,
        "ANSI_1258" => WINDOWS_1258,
        "ANSI_65001" => UTF_8,
        "ANSI_866" | "DOS866" => IBM866,
        _ => return None,
    })
}

type Pair<'a> = (i32, &'a str);

/// A record: an entity, table entry or section marker (group code 0) and its groups.
struct Record<'a> {
    kind: &'a str,
    groups: Vec<Pair<'a>>,
}

/// The records of a drawing, in order, up to a line that isn't a group code (a stray line, or a
/// pairing shifted by a missing one): what came before is kept, and the line is said.
fn records(text: &str) -> Result<(Vec<Record<'_>>, Option<String>)> {
    let mut lines = text.lines();
    let mut out: Vec<Record> = Vec::new();
    let mut line = 0usize;
    while let Some(code) = lines.next() {
        crate::progress::step(code.len() + 1)?;
        line += 1;
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let Some(value) = lines.next() else { break };
        line += 1;
        crate::progress::step(value.len() + 1)?;
        let value = value.trim_end_matches('\r');
        let Ok(code) = code.parse::<i32>() else {
            let said = format!("line {}: '{}' isn't a DXF group code: the drawing is read up to it", line - 1, crate::io::cut(code, 40));
            return Ok((out, Some(said)));
        };
        if code == 0 {
            let kind = value.trim();
            if kind == "EOF" {
                break;
            }
            out.push(Record { kind, groups: Vec::new() });
        } else if let Some(r) = out.last_mut() {
            r.groups.push((code, value));
        }
    }
    Ok((out, None))
}

/// A point in three dimensions.
type P3 = [f64; 3];

fn num(v: &str) -> Option<f64> {
    v.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

/// The groups of one record, read by code.
struct G<'r, 'a> {
    p: &'r [Pair<'a>],
}

impl<'r, 'a> G<'r, 'a> {
    fn s(&self, code: i32) -> Option<&'a str> {
        self.p.iter().find(|(c, _)| *c == code).map(|(_, v)| *v)
    }
    fn f(&self, code: i32) -> Option<f64> {
        self.s(code).and_then(num)
    }
    fn i(&self, code: i32) -> Option<i64> {
        self.s(code).and_then(|v| v.trim().parse::<i64>().ok())
    }
    /// The point of group `x` (with `x + 10`, `x + 20`).
    fn pt(&self, x: i32) -> Option<P3> {
        Some([self.f(x)?, self.f(x + 10)?, self.f(x + 20).unwrap_or(0.0)])
    }
    /// The extrusion direction (the object's coordinate system), +Z unless given.
    fn extrusion(&self) -> P3 {
        match self.pt(210) {
            Some(n) if norm(n) > 1e-12 => n,
            _ => [0.0, 0.0, 1.0],
        }
    }
}

fn norm(v: P3) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn unit(v: P3) -> P3 {
    let n = norm(v);
    if n > 0.0 { [v[0] / n, v[1] / n, v[2] / n] } else { [0.0, 0.0, 1.0] }
}

fn cross(a: P3, b: P3) -> P3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// An affine transform of three dimensions: `p' = m · p + t` (the last column is t).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Xf([[f64; 4]; 3]);

impl Xf {
    const ID: Xf = Xf([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]);

    fn apply(&self, p: P3) -> P3 {
        let m = &self.0;
        [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2] + m[0][3],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2] + m[1][3],
            m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2] + m[2][3],
        ]
    }

    /// `self` after `o`: `p ↦ self(o(p))`.
    fn then(&self, o: &Xf) -> Xf {
        let (a, b) = (&self.0, &o.0);
        let mut r = [[0.0; 4]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                let mut v = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
                if j == 3 {
                    v += a[i][3];
                }
                *cell = v;
            }
        }
        Xf(r)
    }

    /// The object coordinate system of extrusion `n` to the drawing's (AutoCAD's arbitrary axis
    /// algorithm).
    fn ocs(n: P3) -> Xf {
        let n = unit(n);
        if (n[0] - 0.0).abs() < 1e-12 && (n[1] - 0.0).abs() < 1e-12 && (n[2] - 1.0).abs() < 1e-12 {
            return Xf::ID;
        }
        let ax = if n[0].abs() < 1.0 / 64.0 && n[1].abs() < 1.0 / 64.0 { cross([0.0, 1.0, 0.0], n) } else { cross([0.0, 0.0, 1.0], n) };
        let ax = unit(ax);
        let ay = unit(cross(n, ax));
        Xf([[ax[0], ay[0], n[0], 0.0], [ax[1], ay[1], n[1], 0.0], [ax[2], ay[2], n[2], 0.0]])
    }

    fn translate(t: P3) -> Xf {
        Xf([[1.0, 0.0, 0.0, t[0]], [0.0, 1.0, 0.0, t[1]], [0.0, 0.0, 1.0, t[2]]])
    }

    /// Quadrantal angles have exact axes; tiny trigonometric residuals would invent Z variation
    /// when an insert tilts the rotated plane. Other angles retain their original calculation.
    fn rotate_z(degrees: f64) -> Xf {
        let (s, c) = match degrees.rem_euclid(360.0) {
            0.0 => (0.0, 1.0),
            90.0 => (1.0, 0.0),
            180.0 => (0.0, -1.0),
            270.0 => (-1.0, 0.0),
            _ => degrees.to_radians().sin_cos(),
        };
        Xf([[c, -s, 0.0, 0.0], [s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]])
    }

    fn scale(s: P3) -> Xf {
        Xf([[s[0], 0.0, 0.0, 0.0], [0.0, s[1], 0.0, 0.0], [0.0, 0.0, s[2], 0.0]])
    }
}

/// What an entity draws, in the drawing's coordinates (a block's, before it is inserted).
#[derive(Clone, Debug)]
enum Shape {
    Point(P3),
    /// `constant_z` records a construction proof, not a comparison of rounded samples.
    Line {
        points: Vec<P3>,
        constant_z: bool,
    },
    Polygon(Vec<P3>),
    /// A hatch's boundary paths or polyface faces. The flag proves constant source height.
    Rings {
        rings: Vec<Vec<P3>>,
        constant_z: bool,
    },
    Insert(Insert),
}

impl Shape {
    /// A line without a sampled-curve construction proof; compare its heights exactly.
    fn line(points: Vec<P3>) -> Self {
        Self::Line { points, constant_z: false }
    }
}

#[derive(Clone, Debug)]
struct Insert {
    block: String,
    /// Insertion point, scale, rotation (degrees) and extrusion, in the insert's object coordinates.
    at: P3,
    scale: P3,
    rotation: f64,
    extrusion: P3,
    cols: usize,
    rows: usize,
    col_space: f64,
    row_space: f64,
    /// What a DIMENSION draws when its block isn't in the file: lines, and its text's point.
    fallback: Vec<Shape>,
    fallback_text: Option<String>,
}

/// An entity: what it draws, and what each feature it makes keeps.
#[derive(Clone, Debug)]
struct Ent {
    kind: &'static str,
    layer: String,
    /// The layer's name lower-cased (the table's key), once.
    layer_key: String,
    /// The colour index (256 BYLAYER, 0 BYBLOCK).
    color: i64,
    linetype: Option<String>,
    text: Option<String>,
    height: Option<f64>,
    rotation: Option<f64>,
    /// In paper space (a layout), not the model (group 67).
    paper: bool,
    shape: Shape,
    loss: PathLoss,
}

/// Items omitted while recovering an entity; charged only to surviving placements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PathLoss {
    unreadable_vertices: u64,
    capped: u64,
    missing_y: u64,
    invalid_z: u64,
    short_rings: u64,
}

impl PathLoss {
    fn add(&mut self, other: Self) {
        self.unreadable_vertices = self.unreadable_vertices.saturating_add(other.unreadable_vertices);
        self.capped = self.capped.saturating_add(other.capped);
        self.missing_y = self.missing_y.saturating_add(other.missing_y);
        self.invalid_z = self.invalid_z.saturating_add(other.invalid_z);
        self.short_rings = self.short_rings.saturating_add(other.short_rings);
    }
}

/// The entity kinds drawn, by name (the strings features keep).
fn kind_name(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "POINT" => "POINT",
        "LINE" => "LINE",
        "LWPOLYLINE" => "LWPOLYLINE",
        "POLYLINE" => "POLYLINE",
        "CIRCLE" => "CIRCLE",
        "ARC" => "ARC",
        "ELLIPSE" => "ELLIPSE",
        "SPLINE" => "SPLINE",
        "LEADER" => "LEADER",
        "TEXT" => "TEXT",
        "MTEXT" => "MTEXT",
        "ATTRIB" => "ATTRIB",
        "ATTDEF" => "ATTDEF",
        "HELIX" => "HELIX",
        "MLINE" => "MLINE",
        "HATCH" => "HATCH",
        "SOLID" => "SOLID",
        "TRACE" => "TRACE",
        "3DFACE" => "3DFACE",
        "INSERT" => "INSERT",
        "DIMENSION" => "DIMENSION",
        _ => return None,
    })
}

/// The sweep from angle `s` to `e` (counter-clockwise), within one turn of `full`; a whole turn
/// when they meet. Bounded whatever the angles (a loop adding turns wouldn't end for 1e300).
fn sweep(s: f64, e: f64, full: f64) -> f64 {
    let d = (e - s).rem_euclid(full);
    if d.is_finite() && d > 1e-12 { d } else { full }
}

/// The sampled segments of an arc, shared by drawing and omitted-coordinate counts.
fn arc_steps(sweep: f64) -> usize {
    ((sweep.abs() / TAU) * CIRCLE).ceil().clamp(2.0, CIRCLE) as usize
}

/// Points of an arc around `c` of radius `r` from angle `a0` sweeping `sweep` (radians), in the
/// plane z = `z`.
fn arc(c: [f64; 2], r: f64, a0: f64, sweep: f64, z: f64) -> Vec<P3> {
    let n = arc_steps(sweep);
    (0..=n)
        .map(|k| {
            let a = a0 + sweep * k as f64 / n as f64;
            [c[0] + r * a.cos(), c[1] + r * a.sin(), z]
        })
        .collect()
}

/// The sweep of a bulge, or no arc for the reader's straight and degenerate cases.
fn bulge_sweep(chord: f64, b: f64) -> Option<f64> {
    if b.abs() < 1e-9 || chord <= 0.0 || !b.is_finite() || b.abs() > 1e6 { None } else { Some(4.0 * b.atan()) }
}

/// The number of coordinates after a segment's start, without allocating its sampled points.
fn bulge_steps(p1: P3, p2: P3, b: f64) -> usize {
    let (dx, dy) = (p2[0] - p1[0], p2[1] - p1[1]);
    let chord = (dx * dx + dy * dy).sqrt();
    bulge_sweep(chord, b).map_or(1, arc_steps)
}

/// The points of a bulged segment after `p1`, up to `p2` (a bulge is the tangent of a quarter of
/// the arc's angle; positive turns counter-clockwise).
fn bulge(p1: P3, p2: P3, b: f64) -> Vec<P3> {
    let (dx, dy) = (p2[0] - p1[0], p2[1] - p1[1]);
    let chord = (dx * dx + dy * dy).sqrt();
    let Some(theta) = bulge_sweep(chord, b) else { return vec![p2] };
    let r = chord / (2.0 * (theta / 2.0).sin()).abs();
    let h = (chord / 2.0) * (1.0 - b * b) / (2.0 * b);
    let (ux, uy) = (dx / chord, dy / chord);
    let c = [(p1[0] + p2[0]) / 2.0 - uy * h, (p1[1] + p2[1]) / 2.0 + ux * h];
    let a0 = (p1[1] - c[1]).atan2(p1[0] - c[0]);
    let mut pts = arc(c, r, a0, theta, p1[2]);
    pts.remove(0);
    if let Some(last) = pts.last_mut() {
        *last = p2;
    }
    pts
}

/// Vertices with their bulges as a path, closed back to the first vertex when `closed`.
/// Preserve the existing sampled prefix and count every coordinate in unvisited segments.
fn bulged(vs: &[(P3, f64)], closed: bool, omitted: &mut u64) -> Option<Vec<P3>> {
    let mut out: Vec<P3> = Vec::new();
    let Some(first) = vs.first() else { return Some(out) };
    out.push(first.0);
    let n = vs.len();
    let segs = if closed { n } else { n.saturating_sub(1) };
    for i in 0..segs {
        if i % 1024 == 0 {
            crate::progress::check().ok()?;
        }
        let (Some(a), Some(b)) = (vs.get(i), vs.get((i + 1) % n)) else { break };
        if out.len() > MAX_VERTICES {
            *omitted += bulge_steps(a.0, b.0, a.1) as u64;
        } else {
            out.extend(bulge(a.0, b.0, a.1));
        }
    }
    Some(out)
}

/// A B-spline's points (de Boor), rational when `weights` are given: `samples` along it.
fn spline(degree: usize, knots: &[f64], ctrl: &[P3], weights: &[f64], samples: usize) -> Option<Vec<P3>> {
    let n = ctrl.len();
    if degree == 0 || degree > 10 || n <= degree || knots.len() != n + degree + 1 {
        return None;
    }
    if knots.windows(2).any(|w| w[1] < w[0]) || knots.iter().any(|k| !k.is_finite()) {
        return None;
    }
    let (t0, t1) = (*knots.get(degree)?, *knots.get(n)?);
    if t1 <= t0 {
        return None;
    }
    let w = |i: usize| weights.get(i).copied().filter(|w| *w > 0.0 && w.is_finite()).unwrap_or(1.0);
    let mut out = Vec::with_capacity(samples + 1);
    for s in 0..=samples {
        let t = t0 + (t1 - t0) * s as f64 / samples as f64;
        // The knot span holding t.
        let mut k = degree;
        while k < n - 1 && t >= *knots.get(k + 1)? {
            k += 1;
        }
        let mut d: Vec<[f64; 4]> = (0..=degree)
            .map(|j| {
                let i = k + j - degree;
                let (p, wi) = (ctrl.get(i).copied().unwrap_or([0.0; 3]), w(i));
                [p[0] * wi, p[1] * wi, p[2] * wi, wi]
            })
            .collect();
        for r in 1..=degree {
            for j in (r..=degree).rev() {
                let i = k + j - degree;
                let (ka, kb) = (*knots.get(i)?, *knots.get(i + degree + 1 - r)?);
                let alpha = if kb > ka { (t - ka) / (kb - ka) } else { 0.0 };
                let (a, b) = (*d.get(j - 1)?, *d.get(j)?);
                if let Some(dj) = d.get_mut(j) {
                    for c in 0..4 {
                        dj[c] = (1.0 - alpha) * a[c] + alpha * b[c];
                    }
                }
            }
        }
        let p = *d.get(degree)?;
        if p[3] == 0.0 {
            return None;
        }
        out.push([p[0] / p[3], p[1] / p[3], p[2] / p[3]]);
    }
    Some(out)
}

/// MTEXT and TEXT as plain text: formatting codes, stacking and grouping dropped; `\P` a line
/// break (a space), `%%d` a degree sign, `\U+00B0`-style escapes decoded.
fn plain_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' | '}' => {}
            '\\' => match it.next() {
                Some('P') | Some('n') | Some('~') => out.push(' '),
                Some('\\') => out.push('\\'),
                Some('{') => out.push('{'),
                Some('}') => out.push('}'),
                Some('U') if it.peek() == Some(&'+') => {
                    it.next();
                    // Up to four hex digits: the text after them is kept.
                    let mut hex = String::new();
                    while hex.len() < 4
                        && let Some(d) = it.next_if(char::is_ascii_hexdigit)
                    {
                        hex.push(d);
                    }
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                // Toggles without a value.
                Some('L' | 'l' | 'O' | 'o' | 'K' | 'k') => {}
                // Codes with a value up to ';' (font, height, colour, stacking...). Without its
                // ';', a code ends where the next code, group or (except a font's name) word
                // starts, so the text after it is kept.
                Some(code) => {
                    let font = matches!(code, 'f' | 'F');
                    while let Some(&x) = it.peek() {
                        if x == ';' {
                            it.next();
                            break;
                        }
                        if matches!(x, '\\' | '{' | '}') || (!font && x.is_whitespace()) {
                            break;
                        }
                        it.next();
                    }
                }
                None => {}
            },
            '%' if it.peek() == Some(&'%') => {
                it.next();
                match it.next().map(|x| x.to_ascii_lowercase()) {
                    Some('d') => out.push('°'),
                    Some('p') => out.push('±'),
                    Some('c') => out.push('⌀'),
                    Some('%') => out.push('%'),
                    _ => {}
                }
            }
            c => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reads one record as an entity (POLYLINE with its VERTEX records). `None` when it can't be read.
/// Cancellation also stops the Option-based builders; the caller checks and propagates it.
fn entity(kind: &'static str, g: &G, vertices: &[Record], loss: &mut PathLoss) -> Option<Shape> {
    let ocs = Xf::ocs(g.extrusion());
    Some(match kind {
        "POINT" => Shape::Point(g.pt(10)?),
        "LINE" => Shape::line(vec![g.pt(10)?, g.pt(11)?]),
        "LWPOLYLINE" => {
            let z = g.f(38).unwrap_or(0.0);
            let mut vs: Vec<(P3, f64)> = Vec::new();
            let mut tail = false;
            for (index, (c, v)) in g.p.iter().enumerate() {
                if index % 1024 == 0 {
                    crate::progress::check().ok()?;
                }
                if vs.len() >= MAX_VERTICES && (*c == 10 || tail) {
                    if *c == 10 {
                        // Count source vertices at the cap, including ones whose coordinates cannot be read.
                        loss.capped += 1;
                        tail = true;
                    }
                    continue;
                }
                match c {
                    10 => {
                        vs.push(([num(v)?, f64::NAN, z], 0.0));
                    }
                    20 => vs.last_mut()?.0[1] = num(v)?,
                    42 => vs.last_mut()?.1 = num(v).unwrap_or(0.0),
                    _ => {}
                }
            }
            if vs.iter().any(|(p, _)| !p[1].is_finite()) {
                return None;
            }
            let closed = g.i(70).unwrap_or(0) & 1 == 1;
            let pts: Vec<P3> = bulged(&vs, closed, &mut loss.capped)?.into_iter().map(|p| ocs.apply(p)).collect();
            Shape::line(pts)
        }
        "POLYLINE" => {
            let flags = g.i(70).unwrap_or(0);
            if flags & 64 != 0 {
                return polyface(vertices);
            }
            if flags & 16 != 0 {
                // A polygon mesh: its rows as lines would be clutter; left out.
                return None;
            }
            let three_d = flags & 8 != 0;
            let z = g.pt(10).map(|p| p[2]).unwrap_or(0.0);
            let curved = flags & 6 != 0;
            let mut vs = Vec::new();
            for (index, v) in vertices.iter().enumerate() {
                if index % 1024 == 0 {
                    crate::progress::check().ok()?;
                }
                let vg = G { p: &v.groups };
                let vf = vg.i(70).unwrap_or(0);
                // A smoothed polyline's frame (control) points aren't on the curve.
                if curved && vf & 16 != 0 {
                    continue;
                }
                if index >= MAX_VERTICES {
                    loss.capped += 1;
                    continue;
                }
                let Some(mut p) = vg.pt(10) else {
                    loss.unreadable_vertices += 1;
                    continue;
                };
                let invalid_z = three_d && vg.s(30).is_some() && vg.f(30).is_none();
                if invalid_z {
                    loss.invalid_z += 1;
                    continue;
                }
                if !three_d {
                    p[2] = z;
                }
                vs.push((p, vg.f(42).unwrap_or(0.0)));
            }
            let pts = bulged(&vs, flags & 1 == 1, &mut loss.capped)?;
            Shape::line(if three_d { pts } else { pts.into_iter().map(|p| ocs.apply(p)).collect() })
        }
        "CIRCLE" => {
            let (c, r) = (g.pt(10)?, g.f(40)?);
            Shape::line(arc([c[0], c[1]], r, 0.0, TAU, c[2]).into_iter().map(|p| ocs.apply(p)).collect())
        }
        "ARC" => {
            let (c, r) = (g.pt(10)?, g.f(40)?);
            let (s, e) = (g.f(50).unwrap_or(0.0), g.f(51).unwrap_or(360.0));
            Shape::line(
                arc([c[0], c[1]], r, s.rem_euclid(360.0).to_radians(), sweep(s, e, 360.0).to_radians(), c[2])
                    .into_iter()
                    .map(|p| ocs.apply(p))
                    .collect(),
            )
        }
        "ELLIPSE" => {
            let (c, major) = (g.pt(10)?, g.pt(11)?);
            let ratio = g.f(40).unwrap_or(1.0);
            let minor = cross(unit(g.extrusion()), major).map(|v| v * ratio);
            let s = g.f(41).unwrap_or(0.0).rem_euclid(TAU);
            let e = s + sweep(s, g.f(42).unwrap_or(TAU).rem_euclid(TAU), TAU);
            let n = (((e - s) / TAU) * CIRCLE).ceil().clamp(2.0, CIRCLE) as usize;
            Shape::line(
                (0..=n)
                    .map(|k| {
                        let t = s + (e - s) * k as f64 / n as f64;
                        let (sn, cs) = t.sin_cos();
                        [c[0] + major[0] * cs + minor[0] * sn, c[1] + major[1] * cs + minor[1] * sn, c[2] + major[2] * cs + minor[2] * sn]
                    })
                    .collect(),
            )
        }
        // A helix is a spline (its groups are a spline's).
        "SPLINE" | "HELIX" => {
            let degree = usize::try_from(g.i(71).unwrap_or(3)).ok()?;
            let (mut knots, mut weights, mut ctrl, mut fit) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let mut last: Option<i32> = None;
            for (c, v) in g.p {
                if knots.len() + ctrl.len() + fit.len() > MAX_COUNT {
                    break;
                }
                match c {
                    40 => knots.push(num(v)?),
                    41 => weights.push(num(v).unwrap_or(1.0)),
                    10 => ctrl.push([num(v)?, f64::NAN, 0.0]),
                    20 if last == Some(10) => ctrl.last_mut()?[1] = num(v)?,
                    30 if last == Some(20) && ctrl.last().is_some_and(|p: &P3| p[1].is_finite()) => ctrl.last_mut()?[2] = num(v)?,
                    11 => fit.push([num(v)?, f64::NAN, 0.0]),
                    21 if last == Some(11) => fit.last_mut()?[1] = num(v)?,
                    31 if last == Some(21) => fit.last_mut()?[2] = num(v)?,
                    _ => {}
                }
                last = Some(*c);
            }
            let mut missing_ctrl = 0u64;
            let mut missing_fit = 0u64;
            ctrl.retain(|p| {
                let keep = p[1].is_finite();
                missing_ctrl += u64::from(!keep);
                keep
            });
            fit.retain(|p| {
                let keep = p[1].is_finite();
                missing_fit += u64::from(!keep);
                keep
            });
            let samples = (ctrl.len() * 8).clamp(16, 4000);
            match spline(degree, &knots, &ctrl, &weights, samples) {
                Some(points) => {
                    loss.missing_y += missing_ctrl;
                    Shape::Line { points, constant_z: ctrl.first().is_some_and(|first| ctrl.iter().all(|p| p[2] == first[2])) }
                }
                None if fit.len() >= 2 => {
                    loss.missing_y += missing_fit;
                    Shape::line(fit)
                }
                None if ctrl.len() >= 2 => {
                    loss.missing_y += missing_ctrl;
                    Shape::line(ctrl)
                }
                None => return None,
            }
        }
        "LEADER" => {
            let mut pts: Vec<P3> = Vec::new();
            let mut it = g.p.iter().peekable();
            while let Some((c, v)) = it.next() {
                if *c == 10 && pts.len() < MAX_VERTICES {
                    let x = num(v)?;
                    let y = match it.peek() {
                        Some((20, y)) => num(y)?,
                        _ => return None,
                    };
                    pts.push([x, y, 0.0]);
                }
            }
            Shape::line(pts)
        }
        "TEXT" | "ATTRIB" | "ATTDEF" => {
            // The second alignment point when the text is justified, else the first.
            // Vertical alignment is 73 for TEXT, 74 for ATTRIB and ATTDEF (their 73 is a field
            // length).
            let vertical = if kind == "TEXT" { g.i(73) } else { g.i(74) };
            let justified = g.i(72).unwrap_or(0) != 0 || vertical.unwrap_or(0) != 0;
            let p = if justified { g.pt(11).or_else(|| g.pt(10))? } else { g.pt(10)? };
            Shape::Point(ocs.apply(p))
        }
        "MTEXT" => Shape::Point(g.pt(10)?),
        "SOLID" | "TRACE" => {
            let (a, b, c) = (g.pt(10)?, g.pt(11)?, g.pt(12)?);
            // A solid's corners zigzag: 1, 2, then 4 and 3.
            let mut ring = vec![a, b];
            match g.pt(13) {
                Some(d) if d != c => ring.extend([d, c]),
                _ => ring.push(c),
            }
            ring.push(a);
            Shape::Polygon(ring.into_iter().map(|p| ocs.apply(p)).collect())
        }
        "3DFACE" => {
            let (a, b, c) = (g.pt(10)?, g.pt(11)?, g.pt(12)?);
            let mut ring = vec![a, b, c];
            if let Some(d) = g.pt(13).filter(|d| *d != c) {
                ring.push(d);
            }
            ring.push(a);
            Shape::Polygon(ring)
        }
        "HATCH" => Shape::Rings {
            rings: hatch(g, loss)?.into_iter().map(|r| r.into_iter().map(|p| ocs.apply(p)).collect()).collect(),
            constant_z: ocs.0[2][0] == 0.0 && ocs.0[2][1] == 0.0,
        },
        "INSERT" => Shape::Insert(Insert {
            block: g.s(2)?.trim().to_string(),
            at: g.pt(10).unwrap_or([0.0; 3]),
            scale: [g.f(41).unwrap_or(1.0), g.f(42).unwrap_or(1.0), g.f(43).unwrap_or(1.0)],
            rotation: g.f(50).unwrap_or(0.0),
            extrusion: g.extrusion(),
            cols: usize::try_from(g.i(70).unwrap_or(1)).unwrap_or(1).max(1),
            rows: usize::try_from(g.i(71).unwrap_or(1)).unwrap_or(1).max(1),
            col_space: g.f(44).unwrap_or(0.0),
            row_space: g.f(45).unwrap_or(0.0),
            fallback: Vec::new(),
            fallback_text: None,
        }),
        // A dimension draws its (anonymous) block, already placed in the drawing's coordinates.
        "DIMENSION" => {
            let (fallback, fallback_text) = dimension(g);
            Shape::Insert(Insert {
                block: g.s(2).map(str::trim).unwrap_or("").to_string(),
                at: [0.0; 3],
                scale: [1.0; 3],
                rotation: 0.0,
                extrusion: [0.0, 0.0, 1.0],
                cols: 1,
                rows: 1,
                col_space: 0.0,
                row_space: 0.0,
                fallback,
                fallback_text,
            })
        }
        // A multiline: its centre line (the vertices it runs through).
        "MLINE" => {
            let mut pts: Vec<P3> = Vec::new();
            let mut it = g.p.iter().peekable();
            while let Some((c, v)) = it.next() {
                if *c == 11 && pts.len() < MAX_VERTICES {
                    let x = num(v)?;
                    let y = match it.peek() {
                        Some((21, y)) => num(y)?,
                        _ => return None,
                    };
                    pts.push([x, y, 0.0]);
                }
            }
            if g.i(71).unwrap_or(0) & 2 != 0
                && let Some(f) = pts.first().copied()
            {
                pts.push(f);
            }
            Shape::line(pts)
        }
        _ => return None,
    })
}

/// What a linear dimension (rotated or aligned, types 0 and 1) draws: its dimension line through
/// point 10, extension lines from the measured points 13 and 14, and its text at point 11 (the
/// text given, else the length measured). Other dimensions draw nothing without their block.
fn dimension(g: &G) -> (Vec<Shape>, Option<String>) {
    let kind = g.i(70).unwrap_or(0) & 7;
    let (Some(p10), Some(p13), Some(p14)) = (g.pt(10), g.pt(13), g.pt(14)) else { return (Vec::new(), None) };
    if kind > 1 {
        return (Vec::new(), None);
    }
    // The dimension line's direction: the rotation given, or along the measured points.
    let dir = if kind == 0 {
        let (s, c) = g.f(50).unwrap_or(0.0).to_radians().sin_cos();
        [c, s]
    } else {
        let (dx, dy) = (p14[0] - p13[0], p14[1] - p13[1]);
        let l = dx.hypot(dy);
        if l <= 0.0 {
            return (Vec::new(), None);
        }
        [dx / l, dy / l]
    };
    // A point's foot on the dimension line.
    let foot = |p: P3| {
        let t = (p[0] - p10[0]) * dir[0] + (p[1] - p10[1]) * dir[1];
        [p10[0] + t * dir[0], p10[1] + t * dir[1], p10[2]]
    };
    let (a, b) = (foot(p13), foot(p14));
    let length = (b[0] - a[0]).hypot(b[1] - a[1]);
    let text = match g.s(1).map(str::trim) {
        Some(t) if !t.is_empty() && t != "<>" => plain_text(&t.replace("<>", &measure(length))),
        _ => measure(length),
    };
    let mut shapes = vec![Shape::line(vec![a, b]), Shape::line(vec![p13, a]), Shape::line(vec![p14, b])];
    if let Some(t) = g.pt(11) {
        shapes.push(Shape::Point(t));
    }
    (shapes, Some(text))
}

/// A measured length as a dimension shows it: up to two decimals, trailing zeros dropped.
fn measure(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A polyface mesh's faces as polygons (its VERTEX records: positions, then faces by index).
fn polyface(vertices: &[Record]) -> Option<Shape> {
    let mut pos: Vec<P3> = Vec::new();
    let mut faces: Vec<Vec<P3>> = Vec::new();
    for v in vertices.iter().take(MAX_VERTICES) {
        let g = G { p: &v.groups };
        let flags = g.i(70).unwrap_or(0);
        if flags & 64 != 0 {
            pos.push(g.pt(10)?);
        } else if flags & 128 != 0 {
            let ring: Vec<P3> = [71, 72, 73, 74]
                .iter()
                .filter_map(|c| g.i(*c))
                .filter(|i| *i != 0)
                .filter_map(|i| usize::try_from(i.unsigned_abs()).ok())
                .filter_map(|i| pos.get(i.checked_sub(1)?).copied())
                .collect();
            if ring.len() >= 3 {
                let mut r = ring;
                if let Some(f) = r.first().copied() {
                    r.push(f);
                }
                faces.push(r);
            }
        }
    }
    (!faces.is_empty()).then_some(Shape::Rings { rings: faces, constant_z: false })
}

/// A cursor over a record's groups, read in order (HATCH boundary paths).
struct Cursor<'r, 'a> {
    p: &'r [Pair<'a>],
    at: usize,
}

impl Cursor<'_, '_> {
    /// The next group of `code`, from the cursor on.
    fn next(&mut self, code: i32) -> Option<f64> {
        while let Some((c, v)) = self.p.get(self.at) {
            self.at += 1;
            if *c == code {
                return num(v);
            }
        }
        None
    }
    /// An optional group: read when it is the next group, else nothing is consumed.
    fn opt(&mut self, code: i32) -> Option<f64> {
        match self.p.get(self.at) {
            Some((c, v)) if *c == code => {
                self.at += 1;
                num(v)
            }
            _ => None,
        }
    }
    fn count(&mut self, code: i32) -> Option<usize> {
        let n = self.next(code)?;
        (n >= 0.0 && n <= MAX_COUNT as f64).then_some(n as usize)
    }
    fn xy(&mut self, x: i32) -> Option<[f64; 2]> {
        Some([self.next(x)?, self.next(x + 10)?])
    }
}

/// A HATCH's boundary paths as rings (polyline paths with bulges, or edges: lines, arcs, elliptic
/// arcs and splines), in its object coordinates at its elevation.
fn hatch(g: &G, loss: &mut PathLoss) -> Option<Vec<Vec<P3>>> {
    let z = g.pt(10).map(|p| p[2]).unwrap_or(0.0);
    let mut cur = Cursor { p: g.p, at: 0 };
    let paths = cur.count(91)?;
    let mut rings: Vec<Vec<P3>> = Vec::new();
    for _ in 0..paths.min(MAX_COUNT) {
        let flags = cur.next(92)? as i64;
        let mut ring: Vec<P3> = Vec::new();
        if flags & 2 != 0 {
            let has_bulge = cur.next(72)? != 0.0;
            let _closed = cur.opt(73);
            let n = cur.count(93)?;
            let mut vs: Vec<(P3, f64)> = Vec::with_capacity(n.min(1 << 16));
            for _ in 0..n {
                let p = cur.xy(10)?;
                let b = if has_bulge { peek_bulge(&mut cur) } else { 0.0 };
                vs.push(([p[0], p[1], z], b));
            }
            ring = bulged(&vs, true, &mut loss.capped)?;
        } else {
            let edges = cur.count(93)?;
            for _ in 0..edges {
                let piece = match cur.next(72)? as i64 {
                    1 => {
                        let (a, b) = (cur.xy(10)?, cur.xy(11)?);
                        vec![[a[0], a[1], z], [b[0], b[1], z]]
                    }
                    2 => {
                        let c = cur.xy(10)?;
                        let r = cur.next(40)?;
                        let (s, e) = (cur.next(50)?, cur.next(51)?);
                        let ccw = cur.opt(73).unwrap_or(1.0) != 0.0;
                        // A clockwise arc's angles are mirrored (as GDAL reads them); which way
                        // round it runs, `append` finds from its ends.
                        let (s, e) = if ccw { (s, e) } else { (-e, -s) };
                        arc(c, r, s.rem_euclid(360.0).to_radians(), sweep(s, e, 360.0).to_radians(), z)
                    }
                    3 => {
                        let (c, major) = (cur.xy(10)?, cur.xy(11)?);
                        let ratio = cur.next(40)?;
                        let (s, e) = (cur.next(50)?, cur.next(51)?);
                        let ccw = cur.opt(73).unwrap_or(1.0) != 0.0;
                        let (s, e) = if ccw { (s, e) } else { (-e, -s) };
                        let s = s.rem_euclid(360.0);
                        let e = s + sweep(s, e.rem_euclid(360.0), 360.0);
                        let minor = [-major[1] * ratio, major[0] * ratio];
                        let n = (((e - s) / 360.0) * CIRCLE).ceil().clamp(2.0, CIRCLE) as usize;
                        (0..=n)
                            .map(|k| {
                                let t = (s + (e - s) * k as f64 / n as f64).to_radians();
                                let (sn, cs) = t.sin_cos();
                                [c[0] + major[0] * cs + minor[0] * sn, c[1] + major[1] * cs + minor[1] * sn, z]
                            })
                            .collect()
                    }
                    4 => {
                        let degree = cur.count(94)?;
                        let rational = cur.opt(73).unwrap_or(0.0) != 0.0;
                        let _periodic = cur.opt(74);
                        let (nk, nc) = (cur.count(95)?, cur.count(96)?);
                        let mut knots = Vec::with_capacity(nk.min(1 << 16));
                        for _ in 0..nk {
                            knots.push(cur.next(40)?);
                        }
                        let mut ctrl = Vec::with_capacity(nc.min(1 << 16));
                        let mut weights = Vec::new();
                        for _ in 0..nc {
                            let p = cur.xy(10)?;
                            ctrl.push([p[0], p[1], z]);
                            if rational {
                                weights.push(cur.opt(42).unwrap_or(1.0));
                            }
                        }
                        let samples = (nc * 8).clamp(16, 4000);
                        spline(degree, &knots, &ctrl, &weights, samples).unwrap_or(ctrl)
                    }
                    _ => return None,
                };
                append(&mut ring, piece);
                if ring.len() > MAX_VERTICES {
                    return None;
                }
            }
        }
        if ring.len() >= 3 {
            rings.push(ring);
        } else {
            loss.short_rings += 1;
        }
    }
    (!rings.is_empty()).then_some(rings)
}

/// A polyline path's bulge: group 42 when it comes next (before the next vertex).
fn peek_bulge(cur: &mut Cursor) -> f64 {
    match cur.p.get(cur.at) {
        Some((42, v)) => {
            cur.at += 1;
            num(v).unwrap_or(0.0)
        }
        _ => 0.0,
    }
}

/// Appends an edge's points to a ring, the edge turned round when its end meets the ring.
fn append(ring: &mut Vec<P3>, mut piece: Vec<P3>) {
    let d = |a: P3, b: P3| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    if let (Some(&last), Some(&first), Some(&end)) = (ring.last(), piece.first(), piece.last())
        && d(last, end) < d(last, first)
    {
        piece.reverse();
    }
    if let (Some(&last), Some(&first)) = (ring.last(), piece.first())
        && d(last, first) < 1e-9
    {
        piece.remove(0);
    }
    ring.extend(piece);
}

/// A drawing's layers (name → colour, linetype) and blocks (name → base point and entities).
struct Drawing {
    /// Lower-cased name → the name as written, colour, linetype.
    layers: HashMap<String, (String, i64, String)>,
    blocks: HashMap<String, (P3, Vec<Ent>)>,
    entities: Vec<Ent>,
    /// Entities not drawn, by kind.
    unread: BTreeMap<String, usize>,
    /// Entities that couldn't be read.
    bad: usize,
}

/// The magnitude of an ACI color index (negative means its layer is off). The one signed
/// integer without a positive counterpart is malformed and must not overflow while reading.
/// When encountered in a layer definition or drawn entity it refuses the drawing rather than
/// substituting a color. Skipped paper-space entities and unused blocks are not color-validated.
fn color_magnitude(color: i64) -> Result<i64> {
    color.checked_abs().ok_or_else(|| DataError::Format("DXF color index has no representable positive value".into()))
}

/// Reads the records of each section.
fn drawing(records: &[Record]) -> Result<Drawing> {
    let mut d = Drawing { layers: HashMap::new(), blocks: HashMap::new(), entities: Vec::new(), unread: BTreeMap::new(), bad: 0 };
    let mut section = "";
    // The block being read: name, base point, entities.
    let mut block: Option<(String, P3, Vec<Ent>)> = None;
    let mut i = 0usize;
    while let Some(r) = records.get(i) {
        crate::progress::check()?;
        i += 1;
        let g = G { p: &r.groups };
        match r.kind {
            "SECTION" => {
                section = g.s(2).map(str::trim).unwrap_or("");
                continue;
            }
            // Some writers name the next section right after ENDSEC, without its own SECTION.
            "ENDSEC" => {
                section = g.s(2).map(str::trim).unwrap_or("");
                continue;
            }
            _ => {}
        }
        match section {
            "TABLES" if r.kind == "LAYER" => {
                if let Some(name) = g.s(2).map(str::trim) {
                    let color = g.i(62).map(color_magnitude).transpose()?.unwrap_or(7);
                    d.layers.insert(name.to_ascii_lowercase(), (name.to_string(), color, g.s(6).map(str::trim).unwrap_or("").to_string()));
                }
            }
            "BLOCKS" | "ENTITIES" => {
                if section == "BLOCKS" {
                    if r.kind == "BLOCK" {
                        let name = g.s(2).map(str::trim).unwrap_or("").to_string();
                        block = Some((name, g.pt(10).unwrap_or([0.0; 3]), Vec::new()));
                        continue;
                    }
                    if r.kind == "ENDBLK" {
                        if let Some((name, base, ents)) = block.take()
                            && !name.is_empty()
                        {
                            d.blocks.insert(name.to_ascii_lowercase(), (base, ents));
                        }
                        continue;
                    }
                }
                // A POLYLINE's VERTEX records, up to its SEQEND.
                let mut vertices: &[Record] = &[];
                if r.kind == "POLYLINE" {
                    let start = i;
                    while records.get(i).is_some_and(|v| v.kind == "VERTEX") {
                        i += 1;
                    }
                    vertices = records.get(start..i).unwrap_or(&[]);
                    if records.get(i).is_some_and(|s| s.kind == "SEQEND") {
                        i += 1;
                    }
                }
                // An attribute definition is a template, drawn only when constant (no ATTRIB will
                // give its value).
                if matches!(r.kind, "SEQEND" | "VERTEX") || (r.kind == "ATTDEF" && g.i(70).unwrap_or(0) & 2 == 0) {
                    continue;
                }
                let Some(kind) = kind_name(r.kind) else {
                    *d.unread.entry(r.kind.to_string()).or_insert(0) += 1;
                    continue;
                };
                let mut loss = PathLoss::default();
                let shape = entity(kind, &g, vertices, &mut loss);
                // Entity builders use Option for malformed data; cancellation must escape as an error.
                crate::progress::check()?;
                let Some(shape) = shape else {
                    d.bad += 1;
                    continue;
                };
                let text = match kind {
                    "TEXT" | "ATTRIB" | "ATTDEF" => g.s(1).map(plain_text),
                    "MTEXT" => {
                        let mut t: String = g.p.iter().filter(|(c, _)| *c == 3).map(|(_, v)| *v).collect();
                        t.push_str(g.s(1).unwrap_or(""));
                        Some(plain_text(&t))
                    }
                    _ => None,
                };
                let e = Ent {
                    kind,
                    layer: g.s(8).map(str::trim).unwrap_or("0").to_string(),
                    layer_key: g.s(8).map(str::trim).unwrap_or("0").to_ascii_lowercase(),
                    color: g.i(62).unwrap_or(256),
                    linetype: g.s(6).map(|s| s.trim().to_string()),
                    text,
                    paper: g.i(67) == Some(1),
                    height: if matches!(kind, "TEXT" | "MTEXT" | "ATTRIB" | "ATTDEF") { g.f(40) } else { None },
                    rotation: if matches!(kind, "TEXT" | "MTEXT" | "ATTRIB" | "ATTDEF") { g.f(50) } else { None },
                    shape,
                    loss,
                };
                match &mut block {
                    Some((_, _, ents)) if section == "BLOCKS" => ents.push(e),
                    _ if section == "ENTITIES" => d.entities.push(e),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(d)
}

/// What an inserted entity takes from the INSERT placing it.
#[derive(Clone, Copy)]
struct Inherit<'a> {
    layer: Option<&'a str>,
    layer_key: Option<&'a str>,
    color: Option<i64>,
    linetype: Option<&'a str>,
    block: Option<&'a str>,
    paper: bool,
}

/// The features drawn, with their attributes (in the order of [`fields`]).
struct Out {
    points: Dataset,
    lines: Dataset,
    polygons: Dataset,
    count: usize,
    /// One flag per emitted feature, by geometry layer; counted only after CRS filtering.
    varying_z: [Vec<bool>; 3],
    /// Sparse recovery metadata, indexed before CRS filtering in each geometry layer.
    losses: [Vec<(usize, PathLoss)>; 3],
    capped: bool,
    cut: usize,
    /// Entity placements visited so far ([`MAX_STEPS`]), and whether the budget ran out.
    steps: usize,
    overworked: bool,
    /// Whether arrays were cut short ([`MAX_ARRAY`], [`MAX_PATH_COPIES`]).
    trimmed: bool,
    /// Paper-space entities left out.
    paper: usize,
    /// Inserted blocks (and dimensions) left out because `blocks` was false.
    blocked: usize,
    /// Coordinates placed so far ([`MAX_TOTAL_VERTICES`]), and whether the budget ran out.
    vertices: usize,
    vertex_capped: bool,
    /// The copies the arrays above make together on the current nesting path.
    copies: usize,
}

fn fields() -> Vec<Field> {
    vec![
        Field::new("layer", FieldType::Text),
        Field::new("entity", FieldType::Text),
        Field::new("color", FieldType::Integer),
        Field::new("linetype", FieldType::Text),
        Field::new("text", FieldType::Text),
        Field::new("height", FieldType::Double),
        Field::new("rotation", FieldType::Double),
        Field::new("block", FieldType::Text),
        Field::new("elevation", FieldType::Double),
        Field::new("paperspace", FieldType::Boolean),
    ]
}

fn xy(p: P3) -> Coord {
    Coord::new(p[0], p[1])
}

fn finite(ps: &[P3]) -> bool {
    ps.iter().all(|p| p.iter().all(|v| v.is_finite()))
}

/// Whether retained, placed vertices have heights the single elevation attribute cannot hold.
/// Compare computed heights exactly after entity and block transforms, which may flatten source Z.
/// Tilted entity axes or rotated/nested block placements can introduce sub-resolution roundoff;
/// report those computed differences conservatively. A tolerance can hide exact source-height loss.
/// Only a construction and placement proof rules out sampled variation without such a tolerance.
fn has_varying_z<'a>(mut points: impl Iterator<Item = &'a P3>, first: Option<P3>, constant_z: bool) -> bool {
    let Some(first) = first else { return false };
    // Only a source construction and placement proof can dismiss sampled roundoff. A numeric
    // tolerance could instead hide real, exactly representable height differences.
    !constant_z && points.any(|p| p[2] != first[2])
}

/// Draws entities through `xf` (a block's placement), into `out`.
fn emit(
    d: &Drawing,
    ents: &[Ent],
    xf: &Xf,
    from: Inherit,
    depth: usize,
    stack: &mut Vec<String>,
    opts: &Options,
    out: &mut Out,
) -> Result<()> {
    for e in ents {
        crate::progress::check()?;
        if out.vertex_capped {
            return Ok(());
        }
        if out.count >= MAX_FEATURES {
            out.capped = true;
            return Ok(());
        }
        // Every placement costs a step, drawn or not: nested arrays of empty or filtered blocks
        // can't make the walk run on.
        out.steps += 1;
        if out.steps > MAX_STEPS {
            out.overworked = true;
            return Ok(());
        }
        // A layout's entities (paper space) aren't the model's: drawn only when asked for.
        if depth == 0 && e.paper && !opts.paper_space {
            out.paper += 1;
            continue;
        }
        // An entity on layer 0 in a block takes the INSERT's layer; BYBLOCK its colour and linetype.
        let (layer, layer_key) = match (from.layer, from.layer_key) {
            (Some(l), Some(k)) if e.layer == "0" => (l, k),
            _ => (e.layer.as_str(), e.layer_key.as_str()),
        };
        // The layer's table entry, by the key lower-cased when the drawing was read.
        let table = d.layers.get(layer_key);
        // The linetype, worked out only for what is drawn or placed.
        let linetype = || -> Option<&str> {
            match e.linetype.as_deref() {
                Some(l) if l.eq_ignore_ascii_case("BYBLOCK") => from.linetype,
                Some(l) if !l.eq_ignore_ascii_case("BYLAYER") => Some(l),
                _ => table.map(|(_, _, l)| l.as_str()).filter(|l| !l.is_empty()),
            }
        };
        let color = match e.color {
            0 => from.color.unwrap_or(7),
            256 => table.map(|(_, c, _)| *c).unwrap_or(7),
            c => color_magnitude(c)?,
        };
        if let Shape::Insert(ins) = &e.shape {
            // The contents of blocks (图块) are drawn only when asked for: left out, they are
            // counted, not walked.
            if !opts.blocks && (ins.fallback.is_empty() || d.blocks.contains_key(&ins.block.to_ascii_lowercase())) {
                out.blocked += 1;
                continue;
            }
            // Block names are case-insensitive, in the cycle check too.
            let key = ins.block.to_ascii_lowercase();
            if depth >= MAX_DEPTH || stack.contains(&key) {
                out.cut += 1;
                continue;
            }
            let Some((base, inner)) = d.blocks.get(&key) else {
                // A dimension without its block: the lines and text it describes.
                if !ins.fallback.is_empty() {
                    let drawn: Vec<Ent> = ins
                        .fallback
                        .iter()
                        .map(|s| Ent {
                            kind: e.kind,
                            layer: e.layer.clone(),
                            layer_key: e.layer_key.clone(),
                            color: e.color,
                            linetype: e.linetype.clone(),
                            text: matches!(s, Shape::Point(_)).then(|| ins.fallback_text.clone()).flatten(),
                            height: None,
                            rotation: None,
                            paper: e.paper,
                            shape: s.clone(),
                            loss: PathLoss::default(),
                        })
                        .collect();
                    emit(d, &drawn, xf, from, depth + 1, stack, opts, out)?;
                    continue;
                }
                out.cut += 1;
                continue;
            };
            // The copies this array may make: within its own cap and what the arrays above it
            // leave of the path's.
            let allowed = (MAX_PATH_COPIES / out.copies.max(1)).clamp(1, MAX_ARRAY);
            let cols = ins.cols.min(allowed);
            let rows = ins.rows.min(allowed / cols.max(1)).max(1);
            if cols * rows < ins.cols.saturating_mul(ins.rows) {
                out.trimmed = true;
            }
            let outer = out.copies;
            out.copies = outer.max(1).saturating_mul(cols * rows);
            stack.push(key);
            for r in 0..rows {
                for c in 0..cols {
                    let local = Xf::ocs(ins.extrusion)
                        .then(&Xf::translate(ins.at))
                        .then(&Xf::rotate_z(ins.rotation))
                        .then(&Xf::translate([c as f64 * ins.col_space, r as f64 * ins.row_space, 0.0]))
                        .then(&Xf::scale(ins.scale))
                        .then(&Xf::translate([-base[0], -base[1], -base[2]]));
                    let inner_from = Inherit {
                        layer: Some(layer),
                        layer_key: Some(layer_key),
                        color: Some(color),
                        linetype: linetype().or(from.linetype),
                        block: Some(ins.block.as_str()),
                        paper: from.paper || e.paper,
                    };
                    emit(d, inner, &xf.then(&local), inner_from, depth + 1, stack, opts, out)?;
                    if out.capped || out.overworked || out.vertex_capped {
                        stack.pop();
                        out.copies = outer;
                        return Ok(());
                    }
                }
            }
            stack.pop();
            out.copies = outer;
            continue;
        }
        if let Some(want) = opts.layer
            && !layer.eq_ignore_ascii_case(want)
        {
            continue;
        }
        // Every coordinate placed is charged before it is built: a large polyline in a block
        // inserted many times can't fill the memory.
        let n = match &e.shape {
            Shape::Point(_) => 1,
            Shape::Line { points: ps, .. } | Shape::Polygon(ps) => ps.len(),
            Shape::Rings { rings: rs, .. } => rs.iter().map(Vec::len).sum(),
            Shape::Insert(_) => 0,
        };
        if out.vertices.saturating_add(n) > MAX_TOTAL_VERTICES {
            out.vertex_capped = true;
            return Ok(());
        }
        out.vertices += n;
        let preserves_z = xf.0[2][0] == 0.0 && xf.0[2][1] == 0.0;
        let tr = |ps: &[P3]| -> Vec<P3> { ps.iter().map(|p| xf.apply(*p)).collect() };
        let mut loss = e.loss;
        let (geometry, first, target, varying_z) = match &e.shape {
            Shape::Point(p) => {
                let p = xf.apply(*p);
                (finite(&[p]).then(|| Geometry::Point(xy(p))), Some(p), 0, false)
            }
            Shape::Line { points: ps, constant_z } => {
                let ps = tr(ps);
                let ok = ps.len() >= 2 && finite(&ps);
                (
                    ok.then(|| Geometry::Line(vec![ps.iter().map(|p| xy(*p)).collect()])),
                    ps.first().copied(),
                    1,
                    has_varying_z(ps.iter(), ps.first().copied(), *constant_z && preserves_z),
                )
            }
            Shape::Polygon(ps) => {
                let ps = tr(ps);
                let ok = ps.len() >= 4 && finite(&ps);
                (
                    ok.then(|| Geometry::Polygon(vec![vec![ps.iter().map(|p| xy(*p)).collect()]])),
                    ps.first().copied(),
                    2,
                    has_varying_z(ps.iter(), ps.first().copied(), false),
                )
            }
            Shape::Rings { rings, constant_z } => {
                let rings: Vec<Vec<P3>> = rings.iter().map(|r| tr(r)).filter(|r| finite(r)).collect();
                let first = rings.first().and_then(|r| r.first()).copied();
                if e.kind == "HATCH" {
                    loss.short_rings += rings
                        .iter()
                        .filter(|r| r.len() < 4 && (r.len() < 3 || r.first().map(|p| xy(*p)) == r.last().map(|p| xy(*p))))
                        .count() as u64;
                }
                let g = if e.kind == "POLYLINE" {
                    // A polyface mesh: a polygon per face.
                    let polys: Vec<Vec<Vec<Coord>>> =
                        rings.iter().filter(|r| r.len() >= 4).map(|r| vec![r.iter().map(|p| xy(*p)).collect()]).collect();
                    (!polys.is_empty()).then_some(Geometry::Polygon(polys))
                } else {
                    crate::mapinfo::region(rings.iter().map(|r| r.iter().map(|p| xy(*p)).collect()).collect())
                };
                // Count only rings the geometry builder keeps: polyfaces need four vertices;
                // region() also closes open rings, then requires four. Keep the original first
                // point above: its Z is the elevation attribute even if its ring was omitted.
                let kept = rings.iter().filter(|r| {
                    r.len() >= 4 || (e.kind != "POLYLINE" && r.len() == 3 && r.first().map(|p| xy(*p)) != r.last().map(|p| xy(*p)))
                });
                let varying = has_varying_z(kept.flatten(), first, *constant_z && preserves_z);
                (g, first, 2, varying)
            }
            Shape::Insert(_) => continue,
        };
        let Some(geometry) = geometry else {
            out.cut += 1;
            continue;
        };
        let attrs = vec![
            Value::Text(layer.to_string()),
            Value::Text(e.kind.to_string()),
            Value::Int(color),
            linetype().map(|l| Value::Text(l.to_string())).unwrap_or_default(),
            e.text.clone().map(Value::Text).unwrap_or_default(),
            e.height.map(Value::Float).unwrap_or_default(),
            e.rotation.map(Value::Float).unwrap_or_default(),
            from.block.map(|b| Value::Text(b.to_string())).unwrap_or_default(),
            first.map(|p| p[2]).filter(|z| z.is_finite()).map(Value::Float).unwrap_or_default(),
            Value::Bool(e.paper || from.paper),
        ];
        let ds = match target {
            0 => &mut out.points,
            1 => &mut out.lines,
            _ => &mut out.polygons,
        };
        if loss != PathLoss::default()
            && let Some(losses) = out.losses.get_mut(target)
        {
            losses.push((ds.features.len(), loss));
        }
        ds.push_next(Some(geometry), attrs);
        if let Some(flags) = out.varying_z.get_mut(target) {
            flags.push(varying_z);
        }
        out.count += 1;
    }
    Ok(())
}

/// Reads a DXF drawing into its points, lines and polygons layers (those with features).
pub fn read(file_name: &str, bytes: &[u8], opts: &Options) -> Result<Vec<(Dataset, ReadReport)>> {
    let name = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("dxf").to_string();
    if is_binary(bytes) {
        return Err(DataError::Unsupported(format!("{file_name}: a binary DXF (save it as an ASCII DXF to read it here)")));
    }
    crate::progress::check()?;
    let (text, encoding_note) = decode(bytes);
    crate::progress::begin(text.len());
    let (recs, cut_short) = records(text.trim_start_matches('\u{feff}'))?;
    if !recs.iter().any(|r| r.kind == "SECTION") {
        return Err(DataError::Format(match cut_short {
            Some(why) => format!("{file_name}: not a DXF drawing ({why})"),
            None => format!("{file_name}: not a DXF drawing"),
        }));
    }
    let d = drawing(&recs)?;
    let mut out = Out {
        points: Dataset::new(format!("{name} points"), GeometryType::Point, fields()),
        lines: Dataset::new(format!("{name} lines"), GeometryType::Polyline, fields()),
        polygons: Dataset::new(format!("{name} polygons"), GeometryType::Polygon, fields()),
        count: 0,
        varying_z: Default::default(),
        losses: Default::default(),
        capped: false,
        cut: 0,
        steps: 0,
        overworked: false,
        trimmed: false,
        paper: 0,
        blocked: 0,
        vertices: 0,
        vertex_capped: false,
        copies: 1,
    };
    let none = Inherit { layer: None, layer_key: None, color: None, linetype: None, block: None, paper: false };
    emit(&d, &d.entities, &Xf::ID, none, 0, &mut Vec::new(), opts, &mut out)?;

    let mut report = ReadReport::default();
    if let Some(why) = encoding_note {
        report.messages.push(why);
    }
    if let Some(why) = cut_short {
        report.messages.push(why);
    }
    if opts.crs.is_none() {
        report.messages.push(
            "a DXF drawing doesn't say its coordinate system (give \"crs\", such as EPSG:2263): coordinates are kept as they are".into(),
        );
    }
    if out.capped {
        report.messages.push(format!("the drawing makes more than {MAX_FEATURES} features: the rest are left out"));
    }
    if out.vertex_capped {
        report.messages.push(format!("the drawing's features hold more than {MAX_TOTAL_VERTICES} coordinates: the rest are left out"));
    }
    if out.paper > 0 {
        report.messages.push(format!(
            "{} entities of the drawing's layouts (paper space) were left out; add the drawing with \"paperSpace\" to draw them",
            out.paper
        ));
    }
    if out.blocked > 0 {
        report.messages.push(format!(
            "{} inserted blocks (图块) were left out; add the drawing with \"blocks\": true to draw their contents",
            out.blocked
        ));
    }
    if out.trimmed {
        report
            .messages
            .push(format!("block arrays were cut short: at most {MAX_ARRAY} copies each, {MAX_PATH_COPIES} along a nesting path"));
    }
    if out.overworked {
        report
            .messages
            .push(format!("the drawing's blocks place more than {MAX_STEPS} entities (arrays of nested blocks): the rest are left out"));
    }
    if !d.unread.is_empty() {
        let kinds: Vec<String> = d.unread.iter().map(|(k, n)| format!("{n} {k}")).collect();
        report.messages.push(format!("entities RustGIS doesn't draw were left out: {}", kinds.join(", ")));
    }
    let skipped = d.bad + out.cut;
    report.skipped = skipped;
    if skipped > 0 {
        report.messages.push(format!(
            "{skipped} entities couldn't be read or placed (missing blocks, blocks nested too deep or in a cycle) and were left out"
        ));
    }
    let transformer = match opts.crs.filter(|c| *c != Crs::LonLat) {
        Some(c) => Some((c.transformer().map_err(DataError::Format)?, c)),
        None => None,
    };
    let mut layers: Vec<(Dataset, ReadReport)> = Vec::new();
    let mut varying_count = 0usize;
    let mut recovered = PathLoss::default();
    for ((mut ds, flags), losses) in [out.points, out.lines, out.polygons].into_iter().zip(out.varying_z).zip(out.losses) {
        if ds.features.is_empty() {
            continue;
        }
        // What the drawing as a whole said goes with its first layer only.
        let mut rep = if layers.is_empty() { report.clone() } else { ReadReport::default() };
        if let Some((t, c)) = &transformer {
            // Kept as the drawing holds them, in its system, where they have a place in
            // longitude/latitude.
            ds.held_in = *c;
            let mut lost = 0usize;
            for (f, varying) in ds.features.iter_mut().zip(&flags) {
                crate::progress::check()?;
                if let Some(g) = f.geometry.take() {
                    if crate::io::places_in_lonlat(&g, t) {
                        f.geometry = Some(g);
                        ds.zm_dropped |= *varying;
                        varying_count += usize::from(*varying);
                    } else {
                        lost += 1;
                    }
                }
            }
            for (index, loss) in &losses {
                if ds.features.get(*index).is_some_and(|f| f.geometry.is_some()) {
                    recovered.add(*loss);
                }
            }
            ds.features.retain(|f| f.geometry.is_some());
            if lost > 0 {
                rep.messages.push(format!("{lost} features couldn't be converted from {}", c.label()));
            }
        }
        if transformer.is_none() {
            for (_, loss) in losses {
                recovered.add(loss);
            }
            let count = flags.iter().filter(|&&varying| varying).count();
            ds.zm_dropped = count > 0;
            varying_count += count;
        }
        layers.push((ds, rep));
    }
    if let Some((_, report)) = layers.first_mut() {
        for (count, single, plural, reason) in [
            (recovered.capped, "polyline path coordinate was", "polyline path coordinates were", "left out at the per-entity vertex limit"),
            (recovered.missing_y, "vertex without Y was", "vertices without Y were", "left out of spline paths"),
            (
                recovered.invalid_z,
                "POLYLINE vertex with unreadable supplied Z was",
                "POLYLINE vertices with unreadable supplied Z were",
                "left out",
            ),
            (recovered.short_rings, "short HATCH boundary ring was", "short HATCH boundary rings were", "left out of polygons"),
        ] {
            if count > 0 {
                let subject = if count == 1 { single } else { plural };
                report.messages.push(format!("{count} {subject} {reason}"));
            }
        }
    }
    if varying_count > 0
        && let Some((_, report)) = layers.first_mut()
    {
        let subject = if varying_count == 1 { "feature" } else { "features" };
        report.messages.push(format!("{varying_count} {subject} lost varying Z coordinates; only the elevation attribute is kept"));
    }
    if recovered.unreadable_vertices > 0
        && let Some((_, report)) = layers.first_mut()
    {
        let (subject, action) = if recovered.unreadable_vertices == 1 {
            ("vertex", "was left out of its line")
        } else {
            ("vertices", "were left out of their lines")
        };
        report.messages.push(format!("{} unreadable POLYLINE {subject} {action}", recovered.unreadable_vertices));
    }
    if layers.is_empty() {
        return Err(DataError::Format(match opts.layer {
            Some(l) => format!("{file_name}: nothing drawn on layer '{l}'"),
            // Everything the drawing draws is in inserted blocks, and blocks were left out.
            None if out.blocked > 0 => format!(
                "{file_name}: everything it draws is in its inserted blocks (图块), which were left out; add it with \"blocks\": true to draw them"
            ),
            None => format!("{file_name}: nothing RustGIS draws"),
        }));
    }
    Ok(layers)
}

/// The DXF layers a drawing draws on (for picking one with [`Options::layer`]).
pub fn layers(file_name: &str, bytes: &[u8]) -> Result<Vec<String>> {
    if is_binary(bytes) {
        return Err(DataError::Unsupported(format!("{file_name}: a binary DXF (save it as an ASCII DXF to read it here)")));
    }
    let (text, _) = decode(bytes);
    let (recs, _) = records(text.trim_start_matches('\u{feff}'))?;
    if !recs.iter().any(|r| r.kind == "SECTION") {
        return Err(DataError::Format(format!("{file_name}: not a DXF drawing")));
    }
    // The table's layers and those entities (paper space and blocks' too) are on, without
    // drawing anything. Layer 0 in a block takes its INSERT's, so it isn't one of its own there.
    let d = drawing(&recs)?;
    let mut names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |name: &str, key: &str| {
        if seen.insert(key.to_string()) {
            names.push(name.to_string());
        }
    };
    for (key, (name, _, _)) in &d.layers {
        add(name, key);
    }
    for e in &d.entities {
        add(&e.layer, &e.layer_key);
    }
    for (_, ents) in d.blocks.values() {
        for e in ents.iter().filter(|e| e.layer != "0") {
            add(&e.layer, &e.layer_key);
        }
    }
    names.sort_by_key(|n| n.to_ascii_lowercase());
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DXF from (code, value) pairs.
    fn dxf(pairs: &[(i32, &str)]) -> Vec<u8> {
        pairs.iter().map(|(c, v)| format!("{c:>3}\n{v}\n")).collect::<String>().into_bytes()
    }

    /// A bulged HATCH boundary preserves its capped prefix and counts the unvisited arc samples.
    #[test]
    fn hatch_bulge_tail_is_counted_at_the_vertex_limit() {
        use std::fmt::Write as _;
        // Every semicircle contributes 36 sampled coordinates after its starting point.
        let kept_segments = (MAX_VERTICES - 1).div_ceil(36);
        let vertices = (kept_segments + 2).div_ceil(2) * 2;
        let mut text = format!("0\nSECTION\n2\nENTITIES\n0\nHATCH\n91\n1\n92\n2\n72\n1\n73\n1\n93\n{vertices}\n");
        for i in 0..vertices {
            writeln!(text, "10\n{}\n20\n0\n42\n1", i % 2).unwrap();
        }
        text.push_str("0\nENDSEC\n0\nEOF\n");
        let layers = read("hatch.dxf", text.as_bytes(), &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].0.features.len(), 1);
        let Some(Geometry::Polygon(parts)) = &layers[0].0.features[0].geometry else { panic!("HATCH polygon") };
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].len(), 1);
        assert_eq!(parts[0][0].len(), 1 + kept_segments * 36 + kept_segments % 2);
        assert_eq!(parts[0][0].first(), Some(&Coord::new(0., 0.)));
        assert_eq!(layers[0].1.skipped, 0);
        assert_eq!(
            layers[0].1.messages,
            [format!("{} polyline path coordinates were left out at the per-entity vertex limit", (vertices - kept_segments) * 36,)]
        );
    }

    /// Each witness retains useful geometry; only losses in its chosen representation count.
    #[test]
    fn spline_vertices_without_y_are_counted_in_the_path_drawn() {
        for kind in ["SPLINE", "HELIX"] {
            for (x, y) in [(10, 20), (11, 21)] {
                let bytes = dxf(&[
                    (0, "SECTION"),
                    (2, "ENTITIES"),
                    (0, kind),
                    (71, "3"),
                    (x, "0"),
                    (y, "0"),
                    (x, "1"),
                    (x, "2"),
                    (y, "2"),
                    (0, "ENDSEC"),
                    (0, "EOF"),
                ]);
                let layers = read("a.dxf", &bytes, &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
                assert_eq!(layers[0].0.features[0].geometry, Some(Geometry::Line(vec![vec![Coord::new(0., 0.), Coord::new(2., 2.)]])));
                assert_eq!(layers[0].1.messages, ["1 vertex without Y was left out of spline paths"]);
                assert_eq!(layers[0].1.skipped, 0);
            }
        }
    }

    /// Invalid supplied Z is unreadable only on the 3D path; omission still means zero.
    #[test]
    fn supplied_invalid_3d_vertex_z_is_omitted_but_optional_z_stays_zero() {
        for (flags, z, lost) in [("8", Some("bad"), true), ("8", None, false), ("0", Some("bad"), false)] {
            let mut pairs = vec![(0, "SECTION"), (2, "ENTITIES"), (0, "POLYLINE"), (70, flags), (0, "VERTEX"), (10, "0"), (20, "0")];
            if let Some(z) = z {
                pairs.push((30, z));
            }
            pairs.extend([
                (0, "VERTEX"),
                (10, "1"),
                (20, "1"),
                (0, "VERTEX"),
                (10, "2"),
                (20, "2"),
                (0, "SEQEND"),
                (0, "ENDSEC"),
                (0, "EOF"),
            ]);
            let layers = read("a.dxf", &dxf(&pairs), &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
            let Some(Geometry::Line(parts)) = &layers[0].0.features[0].geometry else { panic!("line") };
            assert_eq!(parts[0].len(), if lost { 2 } else { 3 });
            assert_eq!(layers[0].0.value(0, 8), &Value::Float(0.));
            assert_eq!(layers[0].1.messages, if lost { vec!["1 POLYLINE vertex with unreadable supplied Z was left out"] } else { vec![] });
            assert_eq!(layers[0].1.skipped, 0);
        }
    }

    /// Both builder-rejected and closed two-point boundaries disappear beside a valid triangle.
    #[test]
    fn short_hatch_boundary_rings_are_counted() {
        for short in [vec!["1"], vec!["2"], vec!["1", "2"]] {
            let count = short.len();
            let paths = (count + 1).to_string();
            let mut p = vec![(0, "SECTION"), (2, "ENTITIES"), (0, "HATCH"), (91, paths.as_str())];
            for n in std::iter::once("3").chain(short) {
                p.extend([(92, "2"), (72, "0"), (73, "1"), (93, n), (10, "0"), (20, "0")]);
                if n != "1" {
                    p.extend([(10, "2"), (20, "0")]);
                }
                if n == "3" {
                    p.extend([(10, "0"), (20, "2")]);
                }
            }
            p.extend([(0, "ENDSEC"), (0, "EOF")]);
            let layers = read("a.dxf", &dxf(&p), &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
            assert_eq!(
                layers[0].0.features[0].geometry,
                Some(Geometry::Polygon(vec![vec![vec![Coord::new(0., 0.), Coord::new(2., 0.), Coord::new(0., 2.), Coord::new(0., 0.)]]]))
            );
            let subject = if count == 1 { "ring was" } else { "rings were" };
            assert_eq!(layers[0].1.messages, [format!("{count} short HATCH boundary {subject} left out of polygons")]);
            assert_eq!(layers[0].1.skipped, 0);
        }
    }

    /// Counting an expansion tail must still stop after its output prefix has hit the cap.
    #[test]
    fn counting_the_bulge_tail_stops_when_cancelled() {
        let vs = vec![([0., 0., 0.], 0.); MAX_VERTICES + 2048];
        let probe = std::sync::Arc::new(crate::progress::tests::Counted {
            after_checks: Some((MAX_VERTICES / 1024 + 1) as u64),
            ..Default::default()
        });
        let _guard = crate::progress::install(Some(probe));
        let mut omitted = 0;
        assert!(bulged(&vs, false, &mut omitted).is_none());
        assert!(omitted > 0, "cancellation reached the counting tail");
    }

    /// Generate over-limit sources transiently rather than keeping million-vertex fixtures.
    #[test]
    fn polyline_vertex_limits_count_omitted_coordinates() {
        use std::fmt::Write as _;
        for kind in ["LWPOLYLINE", "POLYLINE"] {
            for bulge in [false, true] {
                for unreadable_tail in [false, true] {
                    let mut text = format!("0\nSECTION\n2\nENTITIES\n0\n{kind}\n");
                    if kind == "POLYLINE" && unreadable_tail {
                        text.push_str("70\n10\n");
                    }
                    for _ in 0..MAX_VERTICES {
                        if kind == "POLYLINE" {
                            text.push_str("0\nVERTEX\n");
                        }
                        text.push_str("10\n0\n20\n0\n");
                    }
                    let tail: &[&str] = if !unreadable_tail {
                        &["10\n0\n20\n0\n"]
                    } else if kind == "POLYLINE" {
                        &["10\nbad\n20\n0\n", "10\n0\n20\nbad\n", "10\n0\n20\n0\n30\nbad\n"]
                    } else {
                        &["10\nbad\n20\n0\n", "10\n0\n20\nbad\n", "10\n0\n"]
                    };
                    for vertex in tail {
                        if kind == "POLYLINE" {
                            text.push_str("0\nVERTEX\n");
                        }
                        text.push_str(vertex);
                    }
                    if kind == "POLYLINE" && unreadable_tail {
                        // A smoothed frame point is not an on-curve vertex, even beyond the limit.
                        text.push_str("0\nVERTEX\n70\n16\n10\nbad\n");
                    }
                    // Distinct endpoints let the first segment expand beyond one vertex.
                    if bulge {
                        let start = if kind == "POLYLINE" { "0\nVERTEX\n10\n0\n20\n0\n" } else { "10\n0\n20\n0\n" };
                        let at = text.find(start).unwrap() + start.len();
                        text.insert_str(at, "42\n1\n");
                        let at = at + "42\n1\n".len();
                        let tail = &text[at..];
                        let offset = tail.find("10\n0\n").unwrap();
                        text.replace_range(at + offset..at + offset + 5, "10\n1\n");
                    }
                    if kind == "POLYLINE" {
                        text.push_str("0\nSEQEND\n");
                    }
                    writeln!(text, "0\nENDSEC\n0\nEOF").unwrap();
                    let layers = read("a.dxf", text.as_bytes(), &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
                    let Some(Geometry::Line(parts)) = &layers[0].0.features[0].geometry else { panic!("line") };
                    let held = parts[0].len();
                    let total = MAX_VERTICES + tail.len() + if bulge { 35 } else { 0 };
                    assert_eq!(
                        layers[0].1.messages,
                        [format!(
                            "{} polyline path {} left out at the per-entity vertex limit",
                            total - held,
                            if total - held == 1 { "coordinate was" } else { "coordinates were" }
                        )]
                    );
                    assert_eq!(layers[0].1.skipped, 0);
                }
            }
        }
    }

    /// Unused fit/control arrays, hidden entities and wholly unusable shapes have no partial loss.
    #[test]
    fn path_losses_follow_only_the_drawn_representation_and_retained_placements() {
        let opts = Options { crs: Some(Crs::LonLat), ..Options::default() };
        // A valid linear spline ignores incomplete fit data. A fit fallback ignores incomplete controls.
        for body in ["0 SPLINE 71 1 40 0 40 0 40 1 40 1 10 0 20 0 10 2 20 2 11 1", "0 SPLINE 71 3 10 1 11 0 21 0 11 2 21 2"] {
            let text = format!("0 SECTION 2 ENTITIES {body} 0 ENDSEC 0 EOF").split_whitespace().collect::<Vec<_>>().join("\n");
            let layers = read("a.dxf", text.as_bytes(), &opts).unwrap();
            assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
            assert_eq!(layers[0].1.skipped, 0);
        }
        for body in [
            "0 SPLINE 71 3 11 0 21 0 11 1",
            "0 POLYLINE 70 8 0 VERTEX 10 0 20 0 30 bad 0 VERTEX 10 1 20 1 0 SEQEND",
            "0 HATCH 91 1 92 2 72 0 73 1 93 2 10 0 20 0 10 1 20 1",
        ] {
            let text =
                format!("0 SECTION 2 ENTITIES 0 POINT 10 0 20 0 {body} 0 ENDSEC 0 EOF").split_whitespace().collect::<Vec<_>>().join("\n");
            let layers = read("a.dxf", text.as_bytes(), &opts).unwrap();
            assert_eq!(layers.iter().map(|(d, _)| d.features.len()).sum::<usize>(), 1);
            assert_eq!(layers[0].1.skipped, 1);
            assert_eq!(
                layers[0].1.messages,
                ["1 entities couldn't be read or placed (missing blocks, blocks nested too deep or in a cycle) and were left out"]
            );
        }
        // Losses follow the two surviving block copies; the paper-space and filtered copy stay quiet.
        let text = "0 SECTION 2 BLOCKS 0 BLOCK 2 B 0 SPLINE 71 3 11 0 21 0 11 1 11 2 21 2 0 ENDBLK 0 ENDSEC
            0 SECTION 2 ENTITIES 0 INSERT 2 B 8 keep 70 2 44 5 0 INSERT 2 B 8 omit 0 INSERT 2 B 8 keep 67 1 0 ENDSEC 0 EOF"
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("\n");
        let layers = read("a.dxf", text.as_bytes(), &Options { layer: Some("keep"), ..opts }).unwrap();
        assert_eq!(layers[0].0.features.len(), 2);
        assert_eq!(
            layers[0].1.messages,
            [
                "1 entities of the drawing's layouts (paper space) were left out; add the drawing with \"paperSpace\" to draw them",
                "2 vertices without Y were left out of spline paths",
            ]
        );
        // A CRS-rejected placement contributes no partial counters.
        let text = "0 SECTION 2 ENTITIES 0 POINT 10 0 20 0 0 SPLINE 71 3 11 0 21 100 11 1 11 2 21 101 0 ENDSEC 0 EOF"
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("\n");
        let layers = read("a.dxf", text.as_bytes(), &Options { crs: Some(Crs::from_epsg(4269).unwrap()), ..Options::default() }).unwrap();
        let messages: Vec<&str> = layers.iter().flat_map(|(_, report)| &report.messages).map(String::as_str).collect();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with("1 features couldn't be converted from "));
        assert!(!messages[0].contains("without Y"));
    }

    #[test]
    fn minimum_integer_color_is_refused_without_panicking() {
        for (section, kind) in [("TABLES", "LAYER"), ("ENTITIES", "POINT")] {
            let bytes = dxf(&[
                (0, "SECTION"),
                (2, section),
                (0, kind),
                (2, "Survey"),
                (62, "-9223372036854775808"),
                (10, "1"),
                (20, "2"),
                (0, "ENDSEC"),
                (0, "EOF"),
            ]);
            let error = read("color.dxf", &bytes, &Options::default()).unwrap_err().to_string();
            assert!(error.contains("color index"), "{section}: {error}");
        }
    }

    fn sample() -> Vec<u8> {
        let mut p: Vec<(i32, &str)> = vec![
            (0, "SECTION"),
            (2, "TABLES"),
            (0, "TABLE"),
            (2, "LAYER"),
            (0, "LAYER"),
            (2, "Roads"),
            (62, "1"),
            (6, "DASHED"),
            (0, "LAYER"),
            (2, "Trees"),
            (62, "-3"),
            (6, "CONTINUOUS"),
            (0, "ENDTAB"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "BLOCKS"),
            (0, "BLOCK"),
            (2, "TREE"),
            (10, "1"),
            (20, "1"),
            (30, "0"),
            (0, "CIRCLE"),
            (8, "0"),
            (62, "0"),
            (10, "1"),
            (20, "1"),
            (40, "0.5"),
            (0, "POINT"),
            (8, "0"),
            (10, "1"),
            (20, "1"),
            (0, "ENDBLK"),
            (0, "BLOCK"),
            (2, "LOOP"),
            (0, "INSERT"),
            (2, "LOOP"),
            (10, "0"),
            (20, "0"),
            (0, "ENDBLK"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "ENTITIES"),
        ];
        p.extend([
            (0, "LINE"),
            (8, "Roads"),
            (10, "0"),
            (20, "0"),
            (11, "10"),
            (21, "0"),
            // A closed polyline whose second segment is a half circle.
            (0, "LWPOLYLINE"),
            (8, "Roads"),
            (90, "3"),
            (70, "1"),
            (10, "0"),
            (20, "0"),
            (10, "2"),
            (20, "0"),
            (42, "1"),
            (10, "2"),
            (20, "2"),
            // Trees: a block inserted twice in an array, rotated and scaled.
            (0, "INSERT"),
            (8, "Trees"),
            (2, "TREE"),
            (10, "100"),
            (20, "200"),
            (41, "2"),
            (42, "2"),
            (50, "90"),
            (70, "2"),
            (44, "10"),
            (0, "INSERT"),
            (8, "Trees"),
            (2, "LOOP"),
            (10, "0"),
            (20, "0"),
            (0, "INSERT"),
            (2, "MISSING"),
            (0, "TEXT"),
            (8, "Labels"),
            (10, "5"),
            (20, "5"),
            (40, "2.5"),
            (1, "Main %%d St"),
            (0, "MTEXT"),
            (8, "Labels"),
            (10, "6"),
            (20, "6"),
            (3, "Two "),
            (1, "{\\fArial|b1;Lines}\\PHere"),
            (0, "SOLID"),
            (8, "Fill"),
            (10, "0"),
            (20, "0"),
            (11, "1"),
            (21, "0"),
            (12, "0"),
            (22, "1"),
            (13, "1"),
            (23, "1"),
            (0, "HATCH"),
            (8, "Fill"),
            (10, "0"),
            (20, "0"),
            (30, "0"),
            (2, "SOLID"),
            (91, "2"),
            (92, "2"),
            (72, "0"),
            (73, "1"),
            (93, "4"),
            (10, "0"),
            (20, "0"),
            (10, "10"),
            (20, "0"),
            (10, "10"),
            (20, "10"),
            (10, "0"),
            (20, "10"),
            (97, "0"),
            (92, "1"),
            (93, "4"),
            (72, "1"),
            (10, "2"),
            (20, "2"),
            (11, "4"),
            (21, "2"),
            (72, "1"),
            (10, "4"),
            (20, "4"),
            (11, "4"),
            (21, "2"),
            (72, "1"),
            (10, "4"),
            (20, "4"),
            (11, "2"),
            (21, "4"),
            (72, "1"),
            (10, "2"),
            (20, "4"),
            (11, "2"),
            (21, "2"),
            (97, "0"),
            (0, "3DSOLID"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        dxf(&p)
    }

    #[test]
    fn reads_entities_blocks_and_hatches() {
        let layers = read("site.dxf", &sample(), &Options::default()).unwrap();
        let names: Vec<&str> = layers.iter().map(|(d, _)| d.name.as_str()).collect();
        assert_eq!(names, ["site points", "site lines", "site polygons"]);
        let (points, rep) = &layers[0];
        assert!(rep.messages.iter().any(|m| m.contains("coordinate system")), "{:?}", rep.messages);
        assert!(rep.messages.iter().any(|m| m.contains("1 3DSOLID")), "{:?}", rep.messages);
        // The missing block and the block inserting itself are left out, counted.
        assert_eq!(rep.skipped, 2, "{:?}", rep.messages);
        let col = |ds: &Dataset, f: &str, i: usize| ds.value(i, ds.field_index(f).unwrap()).clone();
        // Tree points: the block's base point (1, 1) placed at the insert, then 10 along the
        // insert's x axis (rotated 90°) for the second column.
        let trees: Vec<Coord> = points
            .features
            .iter()
            .filter(|f| f.attrs[0] == Value::Text("Trees".into()))
            .filter_map(|f| match &f.geometry {
                Some(Geometry::Point(c)) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(trees.len(), 2);
        assert!((trees[0].x - 100.0).abs() < 1e-9 && (trees[0].y - 200.0).abs() < 1e-9, "{trees:?}");
        assert!((trees[1].x - 100.0).abs() < 1e-9 && (trees[1].y - 210.0).abs() < 1e-9, "{trees:?}");
        // A block entity on layer 0 takes the insert's layer, BYBLOCK its colour (Trees: 3).
        let lines = &layers[1].0;
        let circle = lines.features.iter().position(|f| f.attrs[1] == Value::Text("CIRCLE".into())).unwrap();
        assert_eq!(col(lines, "layer", circle), Value::Text("Trees".into()));
        assert_eq!(col(lines, "color", circle), Value::Int(3));
        assert_eq!(col(lines, "block", circle), Value::Text("TREE".into()));
        // Radius 0.5 scaled by 2: a circle of radius 1 around the first tree.
        if let Some(Geometry::Line(parts)) = &lines.features[circle].geometry {
            assert!(parts[0].iter().all(|c| ((c.x - 100.0).hypot(c.y - 200.0) - 1.0).abs() < 1e-9));
        } else {
            panic!("a circle is a line");
        }
        // Roads: BYLAYER colour and linetype.
        assert_eq!(col(lines, "color", 0), Value::Int(1));
        assert_eq!(col(lines, "linetype", 0), Value::Text("DASHED".into()));
        // The bulged polyline: the half circle from (2,0) to (2,2) passes x = 3.
        let lw = lines.features.iter().find(|f| f.attrs[1] == Value::Text("LWPOLYLINE".into())).unwrap();
        if let Some(Geometry::Line(parts)) = &lw.geometry {
            let maxx = parts[0].iter().map(|c| c.x).fold(f64::MIN, f64::max);
            assert!((maxx - 3.0).abs() < 1e-6, "{maxx}");
            assert_eq!(parts[0].first(), parts[0].last(), "closed");
        } else {
            panic!("a polyline is a line");
        }
        // Text, cleaned of its codes.
        let texts: Vec<Value> = points.features.iter().map(|f| f.attrs[4].clone()).filter(|v| !v.is_null()).collect();
        assert_eq!(texts, [Value::Text("Main ° St".into()), Value::Text("Two Lines Here".into())]);
        // The solid's zigzag corners make a square; the hatch a square with a hole.
        let polys = &layers[2].0;
        assert_eq!(polys.features.len(), 2);
        if let Some(Geometry::Polygon(p)) = &polys.features[0].geometry {
            assert_eq!(
                p[0][0],
                vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 1.0), Coord::new(0.0, 0.0)]
            );
        } else {
            panic!("a solid is a polygon");
        }
        if let Some(Geometry::Polygon(p)) = &polys.features[1].geometry {
            assert_eq!((p.len(), p[0].len()), (1, 2), "one polygon with a hole");
        } else {
            panic!("a hatch is a polygon");
        }
    }

    #[test]
    fn layers_filter_and_crs() {
        // The table's layers and the entities' (layer 0: an INSERT without a layer group).
        assert_eq!(layers("a.dxf", &sample()).unwrap(), ["0", "Fill", "Labels", "Roads", "Trees"]);
        let only = read("a.dxf", &sample(), &Options { layer: Some("roads"), ..Options::default() }).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].0.features.len(), 2);
        assert!(read("a.dxf", &sample(), &Options { layer: Some("nope"), ..Options::default() }).is_err());
        // Converted from a projected system: UTM 33N near its central meridian.
        let crs = Crs::parse("EPSG:32633").unwrap();
        let utm = dxf(&[(0, "SECTION"), (2, "ENTITIES"), (0, "POINT"), (10, "500000"), (20, "4649776"), (0, "ENDSEC"), (0, "EOF")]);
        let l = read("u.dxf", &utm, &Options { crs: Some(crs), ..Options::default() }).unwrap();
        assert!(l[0].1.messages.is_empty(), "{:?}", l[0].1.messages);
        assert_eq!(l[0].0.held_in, crs, "held in the drawing's system");
        if let Some(Geometry::Point(c)) = &l[0].0.in_lonlat().unwrap().features[0].geometry {
            assert!((c.x - 15.0).abs() < 1e-6 && (c.y - 42.0).abs() < 0.01, "{c:?}");
        } else {
            panic!("a point is a point");
        }
    }

    #[test]
    fn object_coordinates_and_curves() {
        // A circle in the object coordinate system of extrusion -Z is mirrored in x.
        let c = dxf(&[
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "CIRCLE"),
            (10, "5"),
            (20, "0"),
            (40, "1"),
            (210, "0"),
            (220, "0"),
            (230, "-1"),
            (0, "ARC"),
            (10, "0"),
            (20, "0"),
            (40, "2"),
            (50, "0"),
            (51, "90"),
            (0, "ELLIPSE"),
            (10, "0"),
            (20, "0"),
            (11, "4"),
            (21, "0"),
            (40, "0.5"),
            (0, "SPLINE"),
            (71, "1"),
            (40, "0"),
            (40, "0"),
            (40, "1"),
            (40, "1"),
            (10, "0"),
            (20, "0"),
            (10, "3"),
            (20, "4"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        let l = read("c.dxf", &c, &Options::default()).unwrap();
        let line = |i: usize| match &l[0].0.features[i].geometry {
            Some(Geometry::Line(p)) => p[0].clone(),
            _ => panic!("line {i}"),
        };
        assert!(line(0).iter().all(|p| ((p.x + 5.0).hypot(p.y) - 1.0).abs() < 1e-9), "mirrored centre at -5");
        let a = line(1);
        assert!((a[0].x - 2.0).abs() < 1e-9 && (a.last().unwrap().y - 2.0).abs() < 1e-9);
        let e = line(2);
        let maxy = e.iter().map(|p| p.y).fold(f64::MIN, f64::max);
        assert!((maxy - 2.0).abs() < 1e-6, "minor axis 2: {maxy}");
        let s = line(3);
        assert_eq!((s.first().copied(), s.last().copied()), (Some(Coord::new(0.0, 0.0)), Some(Coord::new(3.0, 4.0))));
    }

    #[test]
    fn polylines_and_polyfaces() {
        let p = dxf(&[
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "POLYLINE"),
            (66, "1"),
            (70, "8"),
            (0, "VERTEX"),
            (10, "0"),
            (20, "0"),
            (30, "1"),
            (0, "VERTEX"),
            (10, "1"),
            (20, "0"),
            (30, "2"),
            (0, "SEQEND"),
            (0, "POLYLINE"),
            (70, "64"),
            (0, "VERTEX"),
            (70, "192"),
            (10, "0"),
            (20, "0"),
            (0, "VERTEX"),
            (70, "192"),
            (10, "1"),
            (20, "0"),
            (0, "VERTEX"),
            (70, "192"),
            (10, "0"),
            (20, "1"),
            (0, "VERTEX"),
            (70, "128"),
            (71, "1"),
            (72, "2"),
            (73, "-3"),
            (0, "VERTEX"),
            (70, "128"),
            (71, "1"),
            (72, "9"),
            (73, "2"),
            (0, "SEQEND"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        let l = read("p.dxf", &p, &Options::default()).unwrap();
        assert_eq!(l[0].0.features.len(), 1);
        assert_eq!(l[0].0.value(0, 8), &Value::Float(1.0), "elevation of a 3D polyline");
        assert_eq!(l[1].0.geometry_type, GeometryType::Polygon);
        if let Some(Geometry::Polygon(p)) = &l[1].0.features[0].geometry {
            assert_eq!(p.len(), 1, "the face with an index past the vertices is left out");
        } else {
            panic!("a face is a polygon");
        }
    }

    /// Every drawing in `RUSTGIS_DXF_DIR`: features per layer and their extent (compared by hand
    /// against GDAL's reading of the same files).
    #[test]
    #[ignore = "needs external DXF samples in RUSTGIS_DXF_DIR"]
    fn dxf_samples() {
        let Ok(dir) = std::env::var("RUSTGIS_DXF_DIR") else { return };
        let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
        paths.sort();
        for p in paths.iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dxf"))) {
            let bytes = std::fs::read(p).unwrap();
            let t = std::time::Instant::now();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            match read(&name, &bytes, &Options::default()) {
                Ok(layers) => {
                    let n: usize = layers.iter().map(|(d, _)| d.features.len()).sum();
                    let e = layers.iter().fold(rustgis_geom::Extent::EMPTY, |e, (d, _)| e.union(&d.extent()));
                    let kinds: Vec<String> =
                        layers.iter().map(|(d, _)| format!("{}:{}", d.geometry_type.label(), d.features.len())).collect();
                    println!(
                        "{name}\t{n}\t{:.3},{:.3},{:.3},{:.3}\t{}\t{:?}\t{}",
                        e.min_x,
                        e.min_y,
                        e.max_x,
                        e.max_y,
                        kinds.join(" "),
                        t.elapsed(),
                        layers[0].1.messages.iter().filter(|m| !m.contains("coordinate system")).cloned().collect::<Vec<_>>().join(" | ")
                    );
                }
                Err(e) => println!("{name}\tERR\t{e}"),
            }
        }
    }

    /// Nested arrays multiply: a path's copies and the whole walk are bounded even when nothing is
    /// drawn (an empty block, or a layer filtered out).
    #[test]
    fn nested_arrays_are_bounded() {
        // Eight levels of 10,000-copy arrays around an empty block: 10,000^8 placements unbounded.
        let mut p: Vec<(i32, String)> = vec![(0, "SECTION".into()), (2, "BLOCKS".into())];
        p.extend([(0, "BLOCK".into()), (2, "L0".into()), (0, "ENDBLK".into())]);
        for level in 1..=8 {
            p.extend([
                (0, "BLOCK".into()),
                (2, format!("L{level}")),
                (0, "INSERT".into()),
                (2, format!("L{}", level - 1)),
                (70, "10000".into()),
                (71, "1".into()),
                (0, "ENDBLK".into()),
            ]);
        }
        // A block of one point on layer "x".
        p.extend([
            (0, "BLOCK".into()),
            (2, "P".into()),
            (0, "POINT".into()),
            (8, "x".into()),
            (10, "0".into()),
            (20, "0".into()),
            (0, "ENDBLK".into()),
        ]);
        p.extend([(0, "ENDSEC".into()), (0, "SECTION".into()), (2, "ENTITIES".into())]);
        p.extend([(0, "INSERT".into()), (2, "L8".into()), (70, "10000".into())]);
        // Many large arrays of the point block, read for another layer: nothing drawn, work done.
        for _ in 0..300 {
            p.extend([(0, "INSERT".into()), (2, "P".into()), (70, "10000".into()), (71, "10".into())]);
        }
        p.extend([(0, "ENDSEC".into()), (0, "EOF".into())]);
        let text: String = p.iter().map(|(c, v)| format!("{c:>3}\n{v}\n")).collect();
        let t = std::time::Instant::now();
        let r = read("arrays.dxf", text.as_bytes(), &Options { layer: Some("y"), ..Options::default() });
        assert!(t.elapsed() < std::time::Duration::from_secs(60), "{:?}", t.elapsed());
        assert!(r.is_err(), "nothing on layer y");
        // Drawn: a cap stops the walk and says so, and the arrays cut short are said.
        let l = read("arrays.dxf", text.as_bytes(), &Options::default()).unwrap();
        let said = &l[0].1.messages;
        assert!(said.iter().any(|m| m.contains("more than")), "{said:?}");
        assert!(said.iter().any(|m| m.contains("cut short")), "{said:?}");
        assert!(l[0].0.features.len() <= MAX_FEATURES);
    }

    /// Optional groups a writer leaves out (an arc's direction, a path's closed flag) don't take
    /// the rest of the hatch with them.
    #[test]
    fn hatch_without_optional_groups() {
        let h = dxf(&[
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "HATCH"),
            (10, "0"),
            (20, "0"),
            (30, "0"),
            (91, "2"),
            // Edges, the arc without its counter-clockwise flag (73).
            (92, "1"),
            (93, "2"),
            (72, "1"),
            (10, "-1"),
            (20, "0"),
            (11, "1"),
            (21, "0"),
            (72, "2"),
            (10, "0"),
            (20, "0"),
            (40, "1"),
            (50, "0"),
            (51, "180"),
            (97, "0"),
            // A polyline path without its closed flag (73).
            (92, "2"),
            (72, "0"),
            (93, "3"),
            (10, "5"),
            (20, "5"),
            (10, "6"),
            (20, "5"),
            (10, "6"),
            (20, "6"),
            (97, "0"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        let l = read("h.dxf", &h, &Options::default()).unwrap();
        match &l[0].0.features[0].geometry {
            Some(Geometry::Polygon(p)) => assert_eq!(p.len(), 2, "both paths read: {p:?}"),
            g => panic!("{g:?}"),
        }
    }

    /// Block names match whatever their case; layouts (paper space) are left out unless asked
    /// for; an attribute's 73 (its field length) isn't a justification.
    #[test]
    fn block_case_paper_space_and_attributes() {
        let d = dxf(&[
            (0, "SECTION"),
            (2, "BLOCKS"),
            (0, "BLOCK"),
            (2, "TREE"),
            (0, "POINT"),
            (10, "1"),
            (20, "1"),
            (0, "ENDBLK"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "INSERT"),
            (2, "Tree"),
            (10, "10"),
            (20, "10"),
            // A title block on a layout.
            (0, "LINE"),
            (67, "1"),
            (10, "0"),
            (20, "0"),
            (11, "297"),
            (21, "0"),
            // An attribute with a field length (73) but no justification: placed at its first point.
            (0, "ATTRIB"),
            (10, "5"),
            (20, "5"),
            (11, "50"),
            (21, "50"),
            (73, "12"),
            (1, "A"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        let l = read("p.dxf", &d, &Options::default()).unwrap();
        assert_eq!(l.len(), 1, "the layout's line is left out");
        let points: Vec<Option<Geometry>> = l[0].0.features.iter().map(|f| f.geometry.clone()).collect();
        assert_eq!(points, [Some(Geometry::Point(Coord::new(11.0, 11.0))), Some(Geometry::Point(Coord::new(5.0, 5.0)))]);
        assert!(l[0].1.messages.iter().any(|m| m.contains("paper space")), "{:?}", l[0].1.messages);
        assert_eq!(l[0].1.skipped, 0, "the block was found");
        let all = read("p.dxf", &d, &Options { paper_space: true, ..Options::default() }).unwrap();
        let lines = &all[1].0;
        let i = lines.field_index("paperspace").unwrap();
        assert_eq!(lines.value(0, i), &Value::Bool(true));
        assert_eq!(all[0].0.value(0, i), &Value::Bool(false));
    }

    /// A tilted polyline in a block inserted as a 100 × 100 array: coordinates are bounded in total,
    /// not only per entity, and height losses count only copies that were kept.
    #[test]
    fn total_coordinates_are_bounded() {
        let mut text = String::from("  0\nSECTION\n  2\nBLOCKS\n  0\nBLOCK\n  2\nBIG\n  0\nLWPOLYLINE\n210\n1\n220\n0\n230\n1\n");
        for i in 0..200_000 {
            text.push_str(&format!(" 10\n0\n 20\n{i}\n"));
        }
        text.push_str(
            "  0\nENDBLK\n  0\nENDSEC\n  0\nSECTION\n  2\nENTITIES\n  0\nINSERT\n  2\nBIG\n 70\n100\n 71\n100\n  0\nENDSEC\n  0\nEOF\n",
        );
        let t = std::time::Instant::now();
        let l = read("big.dxf", text.as_bytes(), &Options::default()).unwrap();
        assert!(t.elapsed() < std::time::Duration::from_secs(60), "{:?}", t.elapsed());
        let held: usize = l[0]
            .0
            .features
            .iter()
            .map(|f| match &f.geometry {
                Some(Geometry::Line(p)) => p.iter().map(Vec::len).sum(),
                _ => 0,
            })
            .sum();
        assert!(held <= MAX_TOTAL_VERTICES, "{held}");
        assert_eq!(l[0].0.features.len(), MAX_TOTAL_VERTICES / 200_000);
        assert!(l[0].0.zm_dropped);
        assert_eq!(
            l[0].1.messages.iter().filter(|m| m.contains("varying Z")).collect::<Vec<_>>(),
            [&format!("{} features lost varying Z coordinates; only the elevation attribute is kept", l[0].0.features.len())]
        );
        assert!(l[0].1.messages.iter().any(|m| m.contains("coordinates")), "{:?}", l[0].1.messages);
    }

    /// A line that isn't a group code ends the reading there: what came before is kept and said.
    #[test]
    fn a_bad_group_code_keeps_what_came_before() {
        let mut d = dxf(&[(0, "SECTION"), (2, "ENTITIES"), (0, "POINT"), (10, "1"), (20, "2"), (0, "POINT"), (10, "3"), (20, "4")]);
        d.extend_from_slice(b"not a code\nvalue\n  0\nPOINT\n 10\n5\n 20\n6\n");
        let l = read("b.dxf", &d, &Options::default()).unwrap();
        assert_eq!(l[0].0.features.len(), 2);
        assert!(l[0].1.messages.iter().any(|m| m.contains("line 17") && m.contains("read up to it")), "{:?}", l[0].1.messages);
    }

    /// A clockwise hatch arc (73 = 0) off the x-axis, as GDAL reads it (ogrdxf_hatch.cpp hands
    /// the angles un-negated to approximateArcAngles, which measures clockwise): the arc runs over
    /// the standard angles from -end to -start around its own centre.
    #[test]
    fn clockwise_hatch_arc_off_the_axis() {
        // Centre (10, 5), radius 2, start 30, end 120, clockwise; closed by a line between its ends.
        let (a, b) = (240f64.to_radians(), 330f64.to_radians());
        let (ax, ay, bx, by) = (10.0 + 2.0 * a.cos(), 5.0 + 2.0 * a.sin(), 10.0 + 2.0 * b.cos(), 5.0 + 2.0 * b.sin());
        let (ax, ay, bx, by) = (ax.to_string(), ay.to_string(), bx.to_string(), by.to_string());
        let h = dxf(&[
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "HATCH"),
            (10, "0"),
            (20, "0"),
            (30, "0"),
            (91, "1"),
            (92, "1"),
            (93, "2"),
            (72, "2"),
            (10, "10"),
            (20, "5"),
            (40, "2"),
            (50, "30"),
            (51, "120"),
            (73, "0"),
            (72, "1"),
            (10, &bx),
            (20, &by),
            (11, &ax),
            (21, &ay),
            (97, "0"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        let l = read("cw.dxf", &h, &Options::default()).unwrap();
        let Some(Geometry::Polygon(p)) = &l[0].0.features[0].geometry else { panic!("a polygon") };
        let ring = &p[0][0];
        // Every point on the circle is within 240°..330°: below the centre, right of 9.
        for c in ring {
            let r = (c.x - 10.0).hypot(c.y - 5.0);
            let t = (c.y - 5.0).atan2(c.x - 10.0).to_degrees().rem_euclid(360.0);
            assert!((r - 2.0).abs() < 1e-6 || r < 2.0, "{c:?}");
            if (r - 2.0).abs() < 1e-6 {
                assert!((239.999..=330.001).contains(&t), "{t} at {c:?}");
            }
        }
        let maxy = ring.iter().map(|c| c.y).fold(f64::MIN, f64::max);
        assert!((maxy - 4.0).abs() < 1e-6, "{maxy}");
    }

    /// Layers listed without drawing: the table's, the entities' (paper space too) and blocks'.
    #[test]
    fn layers_from_the_table_and_entities() {
        let d = dxf(&[
            (0, "SECTION"),
            (2, "TABLES"),
            (0, "LAYER"),
            (2, "Empty"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "BLOCKS"),
            (0, "BLOCK"),
            (2, "B"),
            (0, "POINT"),
            (8, "InBlock"),
            (10, "0"),
            (20, "0"),
            (0, "POINT"),
            (8, "0"),
            (10, "0"),
            (20, "0"),
            (0, "ENDBLK"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "LINE"),
            (8, "Layout"),
            (67, "1"),
            (10, "0"),
            (20, "0"),
            (11, "1"),
            (21, "1"),
            (0, "INSERT"),
            (8, "Trees"),
            (2, "B"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        assert_eq!(layers("l.dxf", &d).unwrap(), ["Empty", "InBlock", "Layout", "Trees"]);
    }

    /// A drawing's text in the code page its header names (AutoCAD 2004 and earlier): its Chinese
    /// layer names and text are read as they were written, not as Latin-1 mojibake. Comments naming
    /// a different code page, outside and inside HEADER, must not override the real variable.
    #[test]
    fn an_ansi_code_page_is_decoded() {
        let mut d =
            b"999\n$DWGCODEPAGE\n999\nANSI_1252\n  0\nSECTION\n  2\nHEADER\n999\n$DWGCODEPAGE\n999\nANSI_1252\n  9\n$DWGCODEPAGE\n999\nignored comment\n  3\nANSI_936\n  0\nENDSEC\n  0\nSECTION\n  2\nTABLES\n  0\nLAYER\n  2\n"
                .to_vec();
        let (gbk, _, _) = encoding_rs::GBK.encode("道路");
        d.extend_from_slice(&gbk);
        d.extend_from_slice(b"\n  0\nENDSEC\n  0\nSECTION\n  2\nENTITIES\n  0\nTEXT\n  8\n");
        d.extend_from_slice(&gbk);
        d.extend_from_slice(b"\n 10\n5\n 20\n5\n 40\n2.5\n  1\n");
        let (text, _, _) = encoding_rs::GBK.encode("中文");
        d.extend_from_slice(&text);
        d.extend_from_slice(b"\n  0\nENDSEC\n  0\nEOF\n");
        // Code-line whitespace is accepted by records(); empty comment values stay paired.
        for prefix in ["", "\n", " \r\n\t\n999\n\n"] {
            let d = [prefix.as_bytes(), d.as_slice()].concat();
            assert_eq!(layers("gbk.dxf", &d).unwrap(), ["道路"]);
            let l = read("gbk.dxf", &d, &Options::default()).unwrap();
            let points = &l[0].0;
            assert_eq!(points.value(0, 0), &Value::Text("道路".into()), "the layer's name");
            assert_eq!(points.value(0, 4), &Value::Text("中文".into()), "its text");
            // Nothing beyond the coordinate system had to be assumed.
            assert!(!l[0].1.messages.iter().any(|m| m.contains("code page") || m.contains("Latin-1")), "{:?}", l[0].1.messages);
        }
    }

    /// UTF-8 (2007 and later) needs no code page, and its Chinese text is kept as it is.
    #[test]
    fn utf8_chinese_text_is_read_as_it_is() {
        let d = dxf(&[(0, "SECTION"), (2, "ENTITIES"), (0, "POINT"), (8, "水系"), (10, "1"), (20, "2"), (0, "ENDSEC"), (0, "EOF")]);
        let l = read("utf8.dxf", &d, &Options::default()).unwrap();
        assert_eq!(l[0].0.value(0, 0), &Value::Text("水系".into()));
        assert!(!l[0].1.messages.iter().any(|m| m.contains("code page") || m.contains("Latin-1")), "{:?}", l[0].1.messages);
    }

    /// A code page RustGIS doesn't know falls back to Latin-1, and says so: a reader never guesses
    /// silently.
    #[test]
    fn an_unknown_code_page_is_said() {
        let mut d =
            b"  0\nSECTION\n  2\nHEADER\n  9\n$DWGCODEPAGE\n  3\nANSI_9999\n  0\nENDSEC\n  0\nSECTION\n  2\nENTITIES\n  0\nPOINT\n  8\n"
                .to_vec();
        d.push(0xE9);
        d.extend_from_slice(b"\n 10\n1\n 20\n2\n  0\nENDSEC\n  0\nEOF\n");
        let l = read("old.dxf", &d, &Options::default()).unwrap();
        assert_eq!(l[0].0.value(0, 0), &Value::Text("é".into()), "Latin-1");
        assert!(l[0].1.messages.iter().any(|m| m.contains("ANSI_9999") && m.contains("Latin-1")), "{:?}", l[0].1.messages);
    }

    /// Cancellation during record parsing returns an error rather than a partial drawing.
    #[test]
    fn reading_stops_when_cancelled() {
        let probe = std::sync::Arc::new(crate::progress::tests::Counted { after: Some(10), ..Default::default() });
        let _guard = crate::progress::install(Some(probe));
        assert!(matches!(read("site.dxf", &sample(), &Options::default()), Err(DataError::Cancelled)));
    }

    /// Disabling blocks keeps standalone entities and DIMENSION fallback geometry.
    #[test]
    fn blocks_can_be_left_out() {
        let l = read("site.dxf", &sample(), &Options { blocks: false, ..Options::default() }).unwrap();
        let names: Vec<&str> = l.iter().map(|(d, _)| d.name.as_str()).collect();
        assert_eq!(names, ["site points", "site lines", "site polygons"]);
        // The two texts only: the trees' point and circle are gone.
        assert_eq!(l[0].0.features.len(), 2);
        assert!(l.iter().all(|(d, _)| d.features.iter().all(|f| f.attrs[1] != Value::Text("CIRCLE".into()))));
        assert_eq!(l[0].1.skipped, 0, "{:?}", l[0].1.messages);
        assert!(l[0].1.messages.iter().any(|m| m.contains("3 inserted blocks") && m.contains("\"blocks\": true")), "{:?}", l[0].1.messages);
        // A DIMENSION without a block keeps the lines and text it describes itself.
        let dimension = dxf(&[
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "DIMENSION"),
            (2, "missing"),
            (70, "0"),
            (10, "0"),
            (20, "2"),
            (13, "0"),
            (23, "0"),
            (14, "5"),
            (24, "0"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        let kept = read("dimension.dxf", &dimension, &Options { blocks: false, ..Options::default() }).unwrap();
        let all_dimension = read("dimension.dxf", &dimension, &Options::default()).unwrap();
        assert_eq!(
            kept.iter().map(|(ds, _)| ds.features.len()).sum::<usize>(),
            all_dimension.iter().map(|(ds, _)| ds.features.len()).sum::<usize>()
        );
        assert!(kept.iter().any(|(ds, _)| ds.geometry_type == GeometryType::Polyline));
        assert!(kept.iter().all(|(_, r)| r.skipped == 0 && !r.messages.iter().any(|m| m.contains("left out"))));
        // Asked for, they are drawn as before.
        let all = read("site.dxf", &sample(), &Options::default()).unwrap();
        assert_eq!(all[0].0.features.len(), 2 + 2, "the block adds its point for each of two copies");
        // A drawing that is nothing but its blocks says why it came out empty.
        let only_block = dxf(&[
            (0, "SECTION"),
            (2, "BLOCKS"),
            (0, "BLOCK"),
            (2, "B"),
            (0, "POINT"),
            (10, "1"),
            (20, "1"),
            (0, "ENDBLK"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "INSERT"),
            (2, "B"),
            (10, "5"),
            (20, "5"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        let e = read("blocks.dxf", &only_block, &Options { blocks: false, ..Options::default() }).unwrap_err().to_string();
        assert!(e.contains("\"blocks\": true"), "{e}");
        assert_eq!(read("blocks.dxf", &only_block, &Options::default()).unwrap()[0].0.features.len(), 1);
    }

    /// Unused, filtered, paper-space and CRS-rejected copies do not inflate the vertex count.
    #[test]
    fn unreadable_vertices_follow_retained_placements() {
        let polyline = "0 POLYLINE 8 0 70 2 10 0 20 0 30 5
            0 VERTEX 10 0 20 0 0 VERTEX 10 1 0 VERTEX 10 bad 20 1
            0 VERTEX 70 16 10 bad 0 VERTEX 10 2 20 2 0 SEQEND";
        let text = format!(
            "0 SECTION 2 BLOCKS 0 BLOCK 2 B {polyline} 0 ENDBLK
            0 BLOCK 2 Unused {polyline} 0 ENDBLK 0 ENDSEC 0 SECTION 2 ENTITIES
            0 INSERT 8 Keep 2 B 10 10 20 20 70 2 44 4
            0 INSERT 8 Other 2 B 10 10 20 20
            0 INSERT 8 Keep 67 1 2 B 10 10 20 20
            0 INSERT 8 Keep 2 B 10 10 20 100
            0 POINT 8 Keep 10 0 20 0 0 ENDSEC 0 EOF"
        )
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("\n");
        let opts = Options { layer: Some("Keep"), crs: Some(Crs::from_epsg(4269).unwrap()), ..Options::default() };
        let layers = read("placed.dxf", text.as_bytes(), &opts).unwrap();
        assert_eq!(layers.len(), 2);
        let lines = &layers[1].0;
        assert_eq!(lines.features.len(), 2);
        assert_eq!(lines.held_in, opts.crs.unwrap());
        assert!(!lines.zm_dropped);
        for (i, x) in [10.0, 14.0].into_iter().enumerate() {
            assert_eq!(lines.features[i].geometry, Some(Geometry::Line(vec![vec![Coord::new(x, 20.0), Coord::new(x + 2.0, 22.0)]])));
            assert_eq!(
                lines.features[i].attrs,
                [
                    Value::Text("Keep".into()),
                    Value::Text("POLYLINE".into()),
                    Value::Int(7),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Text("B".into()),
                    Value::Float(5.0),
                    Value::Bool(false)
                ]
            );
        }
        let notes: Vec<&str> =
            layers.iter().flat_map(|(_, r)| &r.messages).filter(|m| m.contains("POLYLINE")).map(String::as_str).collect();
        assert_eq!(notes, ["4 unreadable POLYLINE vertices were left out of their lines"]);
        let no_blocks = read("placed.dxf", text.as_bytes(), &Options { blocks: false, ..opts }).unwrap();
        assert_eq!(no_blocks.len(), 1);
        assert!(no_blocks.iter().all(|(_, r)| r.messages.iter().all(|m| !m.contains("POLYLINE"))));
    }

    /// The coordinate budget counts losses only for the array copies it actually emits.
    #[test]
    fn unreadable_vertices_exclude_size_rejected_copies() {
        let mut text = String::from("0 SECTION 2 BLOCKS 0 BLOCK 2 B 0 POLYLINE 70 0 ");
        for i in 0..300 {
            text.push_str(&format!("0 VERTEX 10 {i} 20 0 "));
        }
        text.push_str(
            "0 VERTEX 10 bad 20 0 0 SEQEND 0 ENDBLK 0 ENDSEC
            0 SECTION 2 ENTITIES 0 INSERT 2 B 10 0 20 0 70 100 71 100 0 ENDSEC 0 EOF",
        );
        let text = text.split_whitespace().collect::<Vec<_>>().join("\n");
        let layers = read("size.dxf", text.as_bytes(), &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
        assert_eq!(layers.len(), 1);
        let kept = layers[0].0.features.len();
        assert_eq!(kept, MAX_TOTAL_VERTICES / 300);
        assert_eq!(
            layers[0].1.messages,
            [
                format!("the drawing's features hold more than {MAX_TOTAL_VERTICES} coordinates: the rest are left out"),
                format!("{kept} unreadable POLYLINE vertices were left out of their lines"),
            ]
        );
    }

    /// Counts placements after their transforms and filters, leaving constant elevations quiet.
    #[test]
    fn varying_heights_follow_retained_placements() {
        let text = "0 SECTION 2 BLOCKS
            0 BLOCK 2 B 0 LINE 8 0 10 0 20 0 30 5 11 1 21 1 31 9 0 ENDBLK
            0 ENDSEC 0 SECTION 2 ENTITIES
            0 INSERT 8 Keep 2 B 10 10 20 20 30 3 43 2 70 2 44 4
            0 INSERT 8 Keep 2 B 10 20 20 30 30 3 43 0
            0 LINE 8 Other 10 0 20 0 30 5 11 1 21 1 31 9
            0 LINE 8 Keep 67 1 10 0 20 0 30 5 11 1 21 1 31 9
            0 POINT 8 Keep 10 1 20 2 30 5
            0 ENDSEC 0 EOF"
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("\n");
        let opts = Options { layer: Some("Keep"), ..Options::default() };
        let layers = read("placed.dxf", text.as_bytes(), &opts).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].0.features.len(), 1);
        assert!(!layers[0].0.zm_dropped);
        let lines = &layers[1].0;
        assert_eq!(lines.features.len(), 3);
        assert!(lines.zm_dropped);
        for (i, (x, y, z)) in [(10.0, 20.0, 13.0), (14.0, 20.0, 13.0), (20.0, 30.0, 3.0)].into_iter().enumerate() {
            assert_eq!(lines.features[i].geometry, Some(Geometry::Line(vec![vec![Coord::new(x, y), Coord::new(x + 1.0, y + 1.0)]])));
            assert_eq!(lines.value(i, 0), &Value::Text("Keep".into()));
            assert_eq!(lines.value(i, 7), &Value::Text("B".into()));
            assert_eq!(lines.value(i, 8), &Value::Float(z));
        }
        let notes: Vec<&str> =
            layers.iter().flat_map(|(_, r)| &r.messages).filter(|m| m.contains("varying Z")).map(String::as_str).collect();
        assert_eq!(notes, ["2 features lost varying Z coordinates; only the elevation attribute is kept"]);
        let no_blocks = read("placed.dxf", text.as_bytes(), &Options { blocks: false, ..opts }).unwrap();
        assert_eq!(no_blocks.len(), 1);
        assert!(no_blocks.iter().all(|(ds, r)| !ds.zm_dropped && r.messages.iter().all(|m| !m.contains("varying Z"))));
    }

    /// Rational spline noise stays quiet even when a block's offsets cancel its height.
    #[test]
    fn constant_spline_height_stays_quiet() {
        for (height, changed, base, offset, elevation, tilted) in [
            ("0.1", "0.1000000001", "0", "0", 0.1, false),
            ("500.1", "500.1000000001", "500", "-0.1", 0.0, false),
            ("1125899906842624", "1125899906842625", "0", "-1125899906842624", 0.0, false),
            ("0.1", "0.1000000001", "0", "0", 0.0, true),
        ] {
            for varying in [false, true] {
                let end = if varying { changed } else { height };
                let bytes = dxf(&[
                    (0, "SECTION"),
                    (2, "BLOCKS"),
                    (0, "BLOCK"),
                    (2, "B"),
                    (10, "0"),
                    (20, "0"),
                    (30, base),
                    (0, "SPLINE"),
                    (71, "3"),
                    (40, "0"),
                    (40, "0"),
                    (40, "0"),
                    (40, "0"),
                    (40, "1"),
                    (40, "1"),
                    (40, "1"),
                    (40, "1"),
                    (41, "1"),
                    (41, "0.5"),
                    (41, "2"),
                    (41, "1"),
                    (10, "0"),
                    (20, "0"),
                    (30, height),
                    (10, "1"),
                    (20, "1"),
                    (30, height),
                    (10, "2"),
                    (20, "1"),
                    (30, height),
                    (10, "3"),
                    (20, "0"),
                    (30, end),
                    (0, "ENDBLK"),
                    (0, "ENDSEC"),
                    (0, "SECTION"),
                    (2, "ENTITIES"),
                    (0, "INSERT"),
                    (2, "B"),
                    (10, "0"),
                    (20, "0"),
                    (30, offset),
                    (210, if tilted { "1" } else { "0" }),
                    (220, "0"),
                    (230, if tilted { "0" } else { "1" }),
                    (0, "ENDSEC"),
                    (0, "EOF"),
                ]);
                let layers = read("spline.dxf", &bytes, &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
                assert_eq!(layers.len(), 1);
                let (ds, report) = &layers[0];
                assert_eq!(ds.features.len(), 1);
                assert_eq!(ds.value(0, 8), &Value::Float(elevation));
                assert_eq!(ds.zm_dropped, varying || tilted, "height={height}, offset={offset}: {:?}", report.messages);
                assert_eq!(report.skipped, 0);
                let expected = if varying || tilted {
                    vec!["1 feature lost varying Z coordinates; only the elevation attribute is kept"]
                } else {
                    vec![]
                };
                assert_eq!(report.messages, expected);
                let Some(Geometry::Line(parts)) = &ds.features[0].geometry else { panic!("spline line") };
                let (start_xy, end_xy) = if tilted {
                    (Coord::new(0.1, 0.0), Coord::new(end.parse().unwrap(), 3.0))
                } else {
                    (Coord::new(0.0, 0.0), Coord::new(3.0, 0.0))
                };
                assert_eq!(parts[0].first(), Some(&start_xy));
                assert_eq!(parts[0].last(), Some(&end_xy));
            }
        }
    }

    /// HATCH spline edges also sample a proven constant source height.
    #[test]
    fn constant_hatch_spline_height_stays_quiet() {
        for tilted in [false, true] {
            let bytes = dxf(&[
                (0, "SECTION"),
                (2, "ENTITIES"),
                (0, "HATCH"),
                (10, "0"),
                (20, "0"),
                (30, "0.1"),
                (210, if tilted { "1" } else { "0" }),
                (220, "0"),
                (230, "1"),
                (91, "1"),
                (92, "0"),
                (93, "1"),
                (72, "4"),
                (94, "3"),
                (73, "1"),
                (74, "0"),
                (95, "8"),
                (96, "4"),
                (40, "0"),
                (40, "0"),
                (40, "0"),
                (40, "0"),
                (40, "1"),
                (40, "1"),
                (40, "1"),
                (40, "1"),
                (10, "0"),
                (20, "0"),
                (42, "1"),
                (10, "1"),
                (20, "0"),
                (42, "0.5"),
                (10, "1"),
                (20, "1"),
                (42, "2"),
                (10, "0"),
                (20, "0"),
                (42, "1"),
                (0, "ENDSEC"),
                (0, "EOF"),
            ]);
            let layers = read("hatch.dxf", &bytes, &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
            assert_eq!(layers.len(), 1);
            let (ds, report) = &layers[0];
            assert_eq!(ds.geometry_type, GeometryType::Polygon);
            assert_eq!(ds.features.len(), 1);
            if tilted {
                let Value::Float(z) = ds.value(0, 8) else { panic!("hatch elevation") };
                assert!((z - 0.1 / 2.0_f64.sqrt()).abs() < 1e-14);
            } else {
                assert_eq!(ds.value(0, 8), &Value::Float(0.1));
            }
            assert_eq!(ds.zm_dropped, tilted);
            assert_eq!(report.skipped, 0);
            let expected = if tilted { vec!["1 feature lost varying Z coordinates; only the elevation attribute is kept"] } else { vec![] };
            assert_eq!(report.messages, expected);
        }
    }

    /// Cardinal block rotations have exact axes; a nearby angle still creates real Z variation.
    #[test]
    fn cardinal_insert_rotations_do_not_invent_height_variation() {
        for (rotation, x, y, end_y, varying) in [
            ("0", "1", "0", 1.0, false),
            ("90", "0", "1", -1.0, false),
            ("180", "1", "0", -1.0, false),
            ("270", "0", "1", 1.0, false),
            ("360", "1", "0", 1.0, false),
            ("89.999999", "0", "1", -1.0, true),
        ] {
            let bytes = dxf(&[
                (0, "SECTION"),
                (2, "BLOCKS"),
                (0, "BLOCK"),
                (2, "B"),
                (0, "LINE"),
                (10, "0"),
                (20, "0"),
                (30, "0"),
                (11, x),
                (21, y),
                (31, "0"),
                (0, "ENDBLK"),
                (0, "ENDSEC"),
                (0, "SECTION"),
                (2, "ENTITIES"),
                (0, "INSERT"),
                (2, "B"),
                (10, "0"),
                (20, "0"),
                (30, "0"),
                (50, rotation),
                (210, "1"),
                (220, "0"),
                (230, "0"),
                (0, "ENDSEC"),
                (0, "EOF"),
            ]);
            let layers = read("rotated.dxf", &bytes, &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
            assert_eq!(layers.len(), 1);
            let (ds, report) = &layers[0];
            assert_eq!(ds.features.len(), 1);
            assert_eq!(ds.value(0, 8), &Value::Float(0.0));
            assert_eq!(ds.zm_dropped, varying, "rotation={rotation}: {:?}", report.messages);
            let expected =
                if varying { vec!["1 feature lost varying Z coordinates; only the elevation attribute is kept"] } else { vec![] };
            assert_eq!(report.messages, expected);
            assert_eq!(report.skipped, 0);
            let Some(Geometry::Line(parts)) = &ds.features[0].geometry else { panic!("rotated line") };
            assert_eq!(parts[0].first(), Some(&Coord::new(0.0, 0.0)));
            let end = parts[0].last().unwrap();
            assert_eq!(end.x, 0.0);
            if varying {
                assert!((end.y - end_y).abs() < 1e-12);
            } else {
                assert_eq!(end.y, end_y);
            }
        }
    }

    /// Exact height differences survive a large block offset, as they do without placement.
    #[test]
    fn exact_line_heights_are_reported_after_large_offsets() {
        for (start, end, offset) in [("0", "1", "0"), ("1125899906842624", "1125899906842625", "-1125899906842624")] {
            let bytes = dxf(&[
                (0, "SECTION"),
                (2, "BLOCKS"),
                (0, "BLOCK"),
                (2, "B"),
                (0, "LINE"),
                (10, "0"),
                (20, "0"),
                (30, start),
                (11, "1"),
                (21, "1"),
                (31, end),
                (0, "ENDBLK"),
                (0, "ENDSEC"),
                (0, "SECTION"),
                (2, "ENTITIES"),
                (0, "INSERT"),
                (2, "B"),
                (10, "0"),
                (20, "0"),
                (30, offset),
                (0, "ENDSEC"),
                (0, "EOF"),
            ]);
            let layers = read("line.dxf", &bytes, &Options { crs: Some(Crs::LonLat), ..Options::default() }).unwrap();
            assert_eq!(layers.len(), 1);
            let (ds, report) = &layers[0];
            assert_eq!(ds.features.len(), 1);
            assert_eq!(ds.features[0].geometry, Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])));
            assert_eq!(ds.value(0, 8), &Value::Float(0.0));
            assert!(ds.zm_dropped, "start={start}, offset={offset}");
            assert_eq!(report.skipped, 0);
            assert_eq!(report.messages, ["1 feature lost varying Z coordinates; only the elevation attribute is kept"]);
        }
    }

    /// The corpus pair retains all XY/attributes, but the second height cannot fit in elevation.
    #[test]
    fn varying_line_height_is_reported() {
        for end in ["5", "9"] {
            let d = dxf(&[
                (0, "SECTION"),
                (2, "ENTITIES"),
                (0, "LINE"),
                (8, "0"),
                (10, "0"),
                (20, "0"),
                (30, "5"),
                (11, "1"),
                (21, "1"),
                (31, end),
                (0, "ENDSEC"),
                (0, "EOF"),
            ]);
            let layers = read("a.dxf", &d, &Options::default()).unwrap();
            assert_eq!(layers.len(), 1);
            let (ds, report) = &layers[0];
            assert_eq!(ds.features.len(), 1);
            assert_eq!(ds.features[0].geometry, Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])));
            assert_eq!(
                ds.features[0].attrs,
                [
                    Value::Text("0".into()),
                    Value::Text("LINE".into()),
                    Value::Int(7),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Float(5.0),
                    Value::Bool(false)
                ]
            );
            assert_eq!(report.skipped, 0);
            assert_eq!(
                report.messages[0],
                "a DXF drawing doesn't say its coordinate system (give \"crs\", such as EPSG:2263): coordinates are kept as they are"
            );
            assert_eq!(report.messages.len(), if end == "9" { 2 } else { 1 });
            if end == "9" {
                assert_eq!(report.messages[1], "1 feature lost varying Z coordinates; only the elevation attribute is kept");
            }
        }
    }

    /// A wholly rejected feature loses its geometry, not just heights in retained output.
    #[test]
    fn varying_heights_exclude_crs_rejected_features() {
        let text = "0 SECTION 2 ENTITIES 0 POINT 10 0 20 0 30 5
            0 LINE 10 0 20 100 30 5 11 1 21 100 31 9 0 ENDSEC 0 EOF"
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("\n");
        let layers = read("crs.dxf", text.as_bytes(), &Options { crs: Some(Crs::from_epsg(4269).unwrap()), ..Options::default() }).unwrap();
        assert_eq!(layers.iter().map(|(ds, _)| ds.features.len()).sum::<usize>(), 1);
        assert_eq!(layers[0].0.features[0].geometry, Some(Geometry::Point(Coord::new(0.0, 0.0))));
        assert!(layers.iter().all(|(ds, r)| !ds.zm_dropped && r.messages.iter().all(|m| !m.contains("varying Z"))));
        let messages: Vec<&str> = layers.iter().flat_map(|(_, r)| &r.messages).map(String::as_str).collect();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with("1 features couldn't be converted from "), "{messages:?}");
    }

    #[test]
    fn text_codes() {
        // A code without its ';' ends where the next word or code starts; \U+ takes hex digits.
        assert_eq!(plain_text("\\C1 red \\H2x\\Pnext"), "red next");
        assert_eq!(plain_text("\\U+00B0C and \\U+12 xyz"), "°C and \u{12} xyz");
        assert_eq!(plain_text("{\\fArial Narrow|b0;Wide} text"), "Wide text");
        assert_eq!(plain_text("{\\H2.5x;\\C1;Big} \\U+00B0C %%p1 \\Pnext"), "Big °C ±1 next");
        assert_eq!(plain_text("a\\\\b \\{x\\}"), "a\\b {x}");
    }

    #[test]
    fn hostile_drawings_are_errors_not_panics() {
        assert!(read("x.dxf", b"AutoCAD Binary DXF\r\n\x1a\0", &Options::default()).is_err());
        assert!(read("x.dxf", b"not a drawing at all", &Options::default()).is_err());
        assert!(read("x.dxf", b"", &Options::default()).is_err());
        let bytes = sample();
        for n in 0..bytes.len() {
            let _ = read("x.dxf", &bytes[..n], &Options::default());
        }
        // Counts that lie, and numbers that aren't.
        let lie = dxf(&[
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "HATCH"),
            (91, "99999999999"),
            (0, "HATCH"),
            (91, "3"),
            (92, "2"),
            (72, "1"),
            (73, "1"),
            (93, "1000000"),
            (10, "x"),
            (0, "LWPOLYLINE"),
            (20, "1"),
            (42, "nan"),
            (0, "SPLINE"),
            (71, "99"),
            (0, "INSERT"),
            (2, "B"),
            (70, "99999999"),
            (71, "99999999"),
            (0, "ENDSEC"),
            (0, "EOF"),
        ]);
        assert!(read("x.dxf", &lie, &Options::default()).is_err());
    }
}
