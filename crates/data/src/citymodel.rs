//! City models (the CityGML data model, whatever its encoding) read as 2D, the part each reader shares:
//! from an object's faces, lines and points to its rows, from the rows to a layer per object type
//! and geometry family, and the notices. A reader turns its own encoding into [`Faces`] and parts
//! ([`ObjectParts`]) and its objects into [`Built`]; see [`crate::cityjson`].
//!
//! Surfaces and solids become footprints, from the first source that gives one: LoD 0, the surfaces
//! marked `GroundSurface`, then the union of the faces of the highest LoD (see [`crate::footprint`]).
//! An object's lowest and highest Z, its children's included, are kept in `z_min` and `z_max`, and
//! the difference in `height`, the names File Geodatabase multipatches use, so an extrusion stands
//! both up the same way.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::Value as Json;

use crate::footprint::{Hull, P3, footprint, shows, usable_xy};
use crate::raster::Crs;
use crate::{Coord, DataError, Dataset, Field, Geometry, GeometryType, ReadReport, Result, Value};

/// The most layers read: extension types make their number the file's to choose.
pub(crate) const MAX_LAYERS: usize = 64;
/// Most faces of one geometry merged into its footprint; beyond, its outline is its points' hull.
pub(crate) const MAX_FACES: usize = 200_000;
/// Most faces merged for one file, whatever its objects ask: past it, footprints are hulls, so a
/// crafted file can't make loading take hours.
pub(crate) const MAX_FILE_FACES: usize = if cfg!(target_arch = "wasm32") { 500_000 } else { 4_000_000 };
/// The rounds of passing heights up to parents always allowed. The specification has two levels.
pub(crate) const MIN_PARENT_ROUNDS: usize = 8;
/// Most objects and parent links visited passing heights up: past it, a crafted chain stops, with a
/// notice, rather than costing the square of its length. [`MIN_PARENT_ROUNDS`] are run whatever it
/// says, so a file with more links than it pays for eight rounds of costs those eight.
pub(crate) const MAX_PARENT_VISITS: usize = if cfg!(target_arch = "wasm32") { 50_000_000 } else { 200_000_000 };
/// Most characters of an object type shown in a layer's name.
pub(crate) const MAX_TYPE_IN_NAME: usize = 64;
/// The fields every row starts with.
const LEAD: [&str; 4] = ["id", "type", "parent", "lod"];
/// The fields every row ends with.
const HEIGHTS: [&str; 3] = ["z_min", "z_max", "height"];
/// Most names listed in one notice.
const MAX_LISTED: usize = 16;

/// Whether a name is one of the fields every row has, which a same-named attribute gives way to.
fn is_reserved(name: &str) -> bool {
    LEAD.iter().chain(&HEIGHTS).any(|r| r.eq_ignore_ascii_case(name))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Family {
    Polygon,
    Line,
    Point,
}

/// A row of a layer: an object has one per geometry family.
pub(crate) struct Row {
    pub(crate) family: Family,
    pub(crate) geometry: Option<Geometry>,
    /// Its object, in [`Built`].
    pub(crate) object: usize,
    /// The level of detail its geometry came from.
    pub(crate) lod: Option<String>,
    /// Its points' heights differ, and only their lowest and highest are kept.
    pub(crate) z_dropped: bool,
    /// Its geometry couldn't be converted to longitude/latitude: set by [`finish`], never by a reader.
    pub(crate) unconverted: bool,
    /// It draws something: a geometry, and for lines one of two different points.
    pub(crate) draws: bool,
}

/// An object's values, one per object whatever rows it makes.
pub(crate) struct Built {
    pub(crate) id: String,
    pub(crate) ty: String,
    pub(crate) parent_ids: Vec<String>,
    pub(crate) z: Option<(f64, f64)>,
    /// It has solids, so it is 3D however flat its heights.
    pub(crate) solid: bool,
    pub(crate) attributes: Vec<(String, Json)>,
}

/// What was left out or approximated, for one notice per kind.
#[derive(Default)]
pub(crate) struct Counts {
    pub(crate) skipped: usize,
    pub(crate) footprints: usize,
    pub(crate) wall_hulls: usize,
    pub(crate) unmerged: usize,
    /// Hulls because faces past [`MAX_FACES`] were left out, said apart from `unmerged`.
    pub(crate) capped: usize,
    pub(crate) no_footprint: usize,
    pub(crate) no_geometry: usize,
    pub(crate) bad_geometries: usize,
    pub(crate) bad_attributes: usize,
    pub(crate) addresses: usize,
    pub(crate) dropped_rings: usize,
    pub(crate) instances: usize,
    pub(crate) tin_reliefs: usize,
    pub(crate) unconverted: usize,
    pub(crate) bad_heights: usize,
    /// Objects whose lowest and highest Z are too far apart for their difference to be a number.
    pub(crate) height_overflows: usize,
    /// The rounds after which passing heights up to parents stopped, when it did.
    pub(crate) parent_rounds: usize,
    pub(crate) unknown: BTreeSet<String>,
    pub(crate) renamed: BTreeSet<String>,
    /// Objects drawn from another geometry or level of detail than one whose X or Y can't be drawn.
    pub(crate) unusable_passed: usize,
    /// Objects whose geometry with X or Y that can't be drawn was left out, the rest drawing nothing.
    pub(crate) unusable_lost: usize,
    /// Attributes renamed because another of their layer has the same name in another case.
    pub(crate) cased: BTreeSet<String>,
}

/// One surface geometry's faces, each its outer ring and then its holes.
#[derive(Default)]
pub(crate) struct Faces {
    pub(crate) faces: Vec<Vec<Vec<P3>>>,
    /// Whether each face is marked `GroundSurface` (beside `faces`, which is what a footprint takes).
    pub(crate) ground: Vec<bool>,
    /// The rings of what isn't in `faces`, whose points count for the outline all the same.
    pub(crate) left_out: Vec<Vec<P3>>,
    /// Faces past [`MAX_FACES`] were left out.
    pub(crate) truncated: bool,
    /// The rings of those marked `GroundSurface`, apart from `left_out`: a footprint from the ground
    /// misses them, and its outline takes their points (walls and roofs left out don't touch it).
    pub(crate) ground_left_out: Vec<Vec<P3>>,
    pub(crate) dropped_rings: usize,
}

/// A surface geometry and its level of detail, as written and as a number to compare.
pub(crate) struct Surface {
    pub(crate) lod: Option<String>,
    pub(crate) rank: f64,
    pub(crate) faces: Faces,
}

/// Whether a ring has at least three different points (real files hold `[6038, 6038, 6038]`).
fn has_area(ring: &[P3]) -> bool {
    let mut distinct: Vec<&P3> = Vec::with_capacity(3);
    for p in ring {
        if !distinct.contains(&p) {
            distinct.push(p);
            if distinct.len() == 3 {
                return true;
            }
        }
    }
    false
}

impl Faces {
    /// Adds a face, its outer ring and then its holes, marked `GroundSurface` or not. A ring of fewer
    /// than three different points is left out and counted; a face whose outer ring is one is left
    /// out whole. Past [`MAX_FACES`], faces are left out, their points kept for the outline.
    pub(crate) fn face(&mut self, rings: Vec<Vec<P3>>, ground: bool) {
        let mut kept = Vec::new();
        let mut outer = true;
        for (k, points) in rings.into_iter().enumerate() {
            if has_area(&points) {
                kept.push(points);
            } else {
                self.dropped_rings += 1;
                outer &= k != 0;
                self.left_out.push(points);
            }
        }
        if !outer || kept.is_empty() {
            self.left_out.append(&mut kept);
            return;
        }
        if self.faces.len() >= MAX_FACES {
            self.truncated = true;
            if ground {
                self.ground_left_out.append(&mut kept)
            } else {
                self.left_out.append(&mut kept)
            }
            return;
        }
        self.faces.push(kept);
        self.ground.push(ground);
    }

    /// All its rings.
    pub(crate) fn rings(&self) -> impl Iterator<Item = &Vec<P3>> {
        self.faces.iter().flatten().chain(&self.left_out).chain(&self.ground_left_out)
    }
}

/// A face that is already its footprint: it needs no union, so the browser keeps its shape too. Not
/// one that shows nothing from above, by the rule [`footprint`] measures a face with, nor one past
/// the coordinates it takes.
fn lone_face(face: &[Vec<P3>]) -> Option<Geometry> {
    if !face.iter().flatten().all(usable_xy) {
        return None;
    }
    let rings: Vec<Vec<Coord>> = face
        .iter()
        .map(|ring| {
            let mut r: Vec<Coord> = ring.iter().map(|p| Coord::new(p[0], p[1])).collect();
            if r.first() != r.last() {
                r.extend(r.first().copied());
            }
            r
        })
        .collect();
    let outer = rings.first()?;
    let span = |of: fn(&Coord) -> f64| {
        let (lo, hi) = outer.iter().map(of).fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(v), b.max(v)));
        hi - lo
    };
    let size = span(|c| c.x).max(span(|c| c.y));
    shows(face.first()?, size).then(|| Geometry::Polygon(vec![rings]))
}

/// An object's footprint and the level of detail it came from: the first of its LoD 0, the
/// `GroundSurface` of the highest LoD with one (ground faces past the face cap included, which
/// make it the outline of the ground's points), and the union of its highest LoD's faces, those of
/// all its geometries at that level, that gives one. Failing all, the outline of the highest LoD's
/// points, and failing that, none, from the first. Gives too whether a source passed over for the
/// one drawn had X or Y a footprint can't take, for a notice.
fn object_footprint(surfaces: Vec<Surface>, counts: &mut Counts, budget: &mut usize) -> (Option<Geometry>, Option<String>, bool) {
    let highest = |keep: fn(&Surface) -> bool| surfaces.iter().filter(|s| keep(s)).map(|s| s.rank).max_by(f64::total_cmp);
    let lod0 = surfaces.iter().any(|s| s.rank == 0.0 && !s.faces.faces.is_empty());
    let ground = highest(|s| s.faces.ground.contains(&true) || !s.faces.ground_left_out.is_empty());
    let all = highest(|s| !s.faces.faces.is_empty()).or_else(|| highest(|s| !s.faces.left_out.is_empty()));
    // LoD 0 tried whole is the union of its faces too: not tried twice. Its ground alone is, as a
    // wall past the coordinate limit fails the whole and not the ground.
    let all = all.filter(|rank| !(lod0 && *rank == 0.0));
    let sources = [lod0.then_some((0.0, false, true)), ground.map(|rank| (rank, true, true)), all.map(|rank| (rank, false, false))];
    let (mut first_lod, mut passed_unusable) = (None, false);
    for (i, (rank, ground_only, lone)) in sources.into_iter().flatten().enumerate() {
        let attempt = footprint_from(&surfaces, rank, ground_only, lone, budget);
        if i == 0 {
            first_lod = attempt.lod.clone();
        }
        let Some(geometry) = attempt.geometry else {
            passed_unusable |= attempt.unusable;
            continue;
        };
        counts.wall_hulls += usize::from(attempt.hull == Some(Hull::NoFaces));
        // Faces left out past the cap are why it is a hull: said as such, not as faces that
        // couldn't be merged.
        let unmerged = attempt.hull == Some(Hull::Unmerged);
        counts.capped += usize::from(unmerged && attempt.truncated);
        counts.unmerged += usize::from(unmerged && !attempt.truncated);
        return (Some(geometry), attempt.lod, passed_unusable);
    }
    (None, first_lod, false)
}

/// One source's try at a footprint.
struct Attempt {
    geometry: Option<Geometry>,
    lod: Option<String>,
    /// Why it is a hull, if it is.
    hull: Option<Hull>,
    /// Faces past the cap were left out.
    truncated: bool,
    /// Some X or Y is one a footprint can't take.
    unusable: bool,
}

/// The footprint from one source: the faces of the surfaces at `rank`, only those marked
/// `GroundSurface` when `ground_only`. With `lone`, a single face is the footprint as it stands.
fn footprint_from(surfaces: &[Surface], rank: f64, ground_only: bool, lone: bool, budget: &mut usize) -> Attempt {
    let (mut faces, mut left_out): (Vec<&Vec<Vec<P3>>>, Vec<&Vec<P3>>) = (Vec::new(), Vec::new());
    let (mut truncated, mut lod) = (false, None);
    for s in surfaces.iter().filter(|s| s.rank == rank) {
        lod = lod.or_else(|| s.lod.clone());
        if ground_only {
            // The ground's footprint is whole unless ground faces were left out: walls and roofs
            // past the cap don't change it (their points still count for the heights), so they
            // need no notice.
            truncated |= !s.faces.ground_left_out.is_empty();
            faces.extend(s.faces.faces.iter().zip(&s.faces.ground).filter(|(_, ground)| **ground).map(|(face, _)| face));
            left_out.extend(&s.faces.ground_left_out);
        } else {
            truncated |= s.faces.truncated;
            faces.extend(&s.faces.faces);
            left_out.extend(&s.faces.left_out);
            left_out.extend(&s.faces.ground_left_out);
        }
    }
    // From LoD 0 or the ground, one face is the footprint as it stands.
    if lone
        && !truncated
        && let [face] = faces.as_slice()
        && let Some(geometry) = lone_face(face)
    {
        return Attempt { geometry: Some(geometry), lod, hull: None, truncated: false, unusable: false };
    }
    let points: Vec<P3> = faces.iter().flat_map(|f| f.iter()).chain(left_out.iter().copied()).flatten().copied().collect();
    let unusable = !points.iter().all(usable_xy);
    let (geometry, hull) = footprint(&faces, truncated, std::slice::from_ref(&points), budget);
    Attempt { geometry, lod, hull, truncated, unusable }
}

/// The horizontal part of the compound (and 3D) systems city models name, some of whose definitions
/// here are missing or ask for grids: heights are kept as they come, not converted.
pub(crate) fn horizontal_epsg(code: u32) -> u32 {
    match code {
        // Amersfoort / RD New + NAP height.
        7415 => 28992,
        // OSGB36 / British National Grid + ODN height.
        7405 => 27700,
        // ETRS89 / UTM zones 31N, 32N and 33N + DHHN92; 32N and 33N + DVR90.
        5554 => 25831,
        5555 | 7416 => 25832,
        5556 | 7417 => 25833,
        // JGD2011 + JGD2011 (vertical) height.
        6697 => 6668,
        // NAD83 + NAVD88 height.
        5498 => 4269,
        // WGS 84 in 3D, and + EGM2008 height.
        4979 | 9518 => 4326,
        c => c,
    }
}

/// The system a name stands for, by its horizontal part, if RustGIS knows it.
pub(crate) fn known_system(system: &str) -> Option<Crs> {
    crate::ogc::warp::epsg_of(system).and_then(|c| Crs::from_epsg(horizontal_epsg(c)))
}

/// The coordinate system: the one given, the one the file names (`named`), or longitude/latitude
/// when it names none and every vertex can be one. Anything else is an error, not a guess. A file
/// only names EPSG codes: what it says is never registered as a new definition.
pub(crate) fn coordinate_system(
    file_name: &str,
    given: Option<Crs>,
    named: Option<&str>,
    vertices: &[P3],
    notes: &mut Vec<String>,
) -> Result<Crs> {
    let within = |x: f64, y: f64| vertices.iter().all(|p| p[0].abs() <= x && p[1].abs() <= y);
    let crs = match (given, named) {
        (Some(crs), named) => {
            // What the file names is said whenever the system given takes its place, known or not.
            match named.map(|system| (system, known_system(system))) {
                Some((_, Some(own))) if own != crs => {
                    notes.push(format!("the file names {}, and is read in {}, the coordinate system given", own.label(), crs.label()));
                }
                Some((system, None)) => {
                    let system: String = system.chars().take(80).collect();
                    notes.push(format!(
                        "the file names a coordinate system RustGIS doesn't know ({system}), and is read in {}, the coordinate system given",
                        crs.label()
                    ));
                }
                _ => {}
            }
            crs
        }
        (None, Some(system)) => known_system(system).ok_or_else(|| {
            let system: String = system.chars().take(80).collect();
            DataError::Format(format!("{file_name}: it names a coordinate system RustGIS doesn't know ({system}); give one with the crs parameter of layer.addData (from the command line, Python or the AI Assistant)"))
        })?,
        (None, None) if within(180.0, 90.0) => {
            notes.push("the file names no coordinate system: its coordinates are read as longitude/latitude".into());
            return Ok(Crs::LonLat);
        }
        (None, None) => {
            return Err(DataError::Format(format!(
                "{file_name}: it names no coordinate system and its coordinates aren't longitude/latitude, so it can't be placed; name one in the file, or give it with the crs parameter of layer.addData (from the command line, Python or the AI Assistant)"
            )));
        }
    };
    if crs.is_geocentric() {
        return Err(DataError::Format(format!("{file_name}: {} is a geocentric coordinate system, which isn't a map's", crs.label())));
    }
    if crs == Crs::LonLat && !within(180.5, 90.5) {
        notes.push("coordinates are not longitude/latitude; RustGIS reads them as WGS 84 and they may draw out of place".into());
    }
    if let Some(unit) = crs.height_unit_not_metre() {
        notes.push(format!(
            "{} measures in {unit}: z_min, z_max and height are kept as the file has them, and Style › Extrusion reads metres, so objects may stand at the wrong height",
            crs.label()
        ));
    }
    Ok(crs)
}

/// The names a layer's attributes go by, as the file spells them, decided from all its objects so a
/// column holds one source attribute: one named as a field every row has, or the same as another
/// of the layer but for case, takes the first number that leaves it apart from every other.
fn attribute_names<'a>(objects: impl Iterator<Item = &'a Built>, counts: &mut Counts) -> HashMap<String, String> {
    let mut spellings: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for b in objects {
        for (name, _) in &b.attributes {
            if seen.insert(name.as_str()) {
                spellings.push(name.as_str());
            }
        }
    }
    let mut used: HashSet<String> = spellings.iter().map(|n| n.to_ascii_lowercase()).collect();
    // The first spelling of each name not a field's keeps it.
    let mut first: HashMap<String, &str> = HashMap::new();
    // The number each name goes on from, whatever its case: `used` is finite, so one is always
    // free, and the spellings of a name don't each search again past the same numbers.
    let mut next: HashMap<String, usize> = HashMap::new();
    let mut names = HashMap::new();
    for name in spellings {
        let lower = name.to_ascii_lowercase();
        if is_reserved(name) {
            counts.renamed.insert(name.chars().take(64).collect());
        } else if *first.entry(lower.clone()).or_insert(name) != name {
            counts.cased.insert(name.chars().take(64).collect());
        } else {
            continue;
        }
        let k = next.entry(lower).or_insert(1);
        let free = loop {
            let candidate = format!("{name}_{k}");
            *k += 1;
            if used.insert(candidate.to_ascii_lowercase()) {
                break candidate;
            }
        };
        names.insert(name.to_string(), free);
    }
    names
}

/// The rows, heights and solids of a file's objects as layers, with the file's notes and one notice
/// per kind of what was left out. `stem` names the layers. The rows' geometries come in `crs` and
/// are converted to longitude/latitude here.
pub(crate) fn finish(
    file_name: &str,
    stem: &str,
    mut rows: Vec<Row>,
    mut built: Vec<Built>,
    mut counts: Counts,
    notes: Vec<String>,
    crs: Crs,
) -> Result<Vec<(Dataset, ReadReport)>> {
    // Converted once every object is in: a reader that streams its objects knows its system, and
    // whether the coordinates fit it, only then.
    if crs != Crs::LonLat {
        let to_lonlat = crs.transformer().map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
        let mut objects = HashSet::new();
        for row in &mut rows {
            if let Some(g) = row.geometry.take() {
                row.geometry = crate::io::geometry_to_lonlat(&g, &to_lonlat);
                if row.geometry.is_none() {
                    (row.unconverted, row.draws) = (true, false);
                    objects.insert(row.object);
                }
            }
        }
        counts.unconverted += objects.len();
    }
    counts.parent_rounds = raise_parents(&mut built, MAX_PARENT_VISITS).unwrap_or(0);
    counts.height_overflows = built.iter().filter(|b| b.z.is_some_and(|(lo, hi)| !(hi - lo).is_finite())).count();
    let is_3d = |b: &Built| b.solid || b.z.is_some_and(|(lo, hi)| hi > lo);
    counts.footprints = rows
        .iter()
        .filter(|r| r.family == Family::Polygon && r.geometry.is_some())
        .filter(|r| built.get(r.object).is_some_and(is_3d))
        .count();
    if counts.unconverted > 0 && rows.iter().all(|r| r.geometry.is_none()) {
        return Err(DataError::Format(format!(
            "{file_name}: no object could be converted from {} to longitude/latitude: that coordinate system doesn't seem to match the coordinates",
            crs.label()
        )));
    }

    let mut out = layers(stem, rows, &built, crs, &mut counts);
    if out.is_empty() {
        let mut why = String::new();
        if counts.tin_reliefs > 0 {
            why = format!(": it holds {} TINRelief objects, which aren't drawn", counts.tin_reliefs);
        } else if counts.skipped > 0 {
            why = format!(": {} city objects couldn't be read", counts.skipped);
        }
        return Err(DataError::Format(format!("{file_name}: nothing to draw{why}")));
    }
    if let Some((_, report)) = out.first_mut() {
        report.skipped += counts.skipped;
        report.messages.extend(notes);
        report.messages.extend(messages(&counts));
    }
    Ok(out)
}

/// What one object makes.
pub(crate) struct ObjectRows {
    /// One per geometry family.
    pub(crate) rows: Vec<Row>,
    pub(crate) z: Option<(f64, f64)>,
    pub(crate) solid: bool,
}

/// A family's parts at the highest level of detail an object has: its other levels are other
/// pictures of it, not more of it, as for its surfaces. Parts that draw something go before any
/// level: a line of one point at LoD 2 doesn't take the place of a road drawn at LoD 1, nor do
/// points that a transform took past the numbers [`usable_xy`] takes take the place of good ones.
pub(crate) struct Highest<T> {
    drawable: bool,
    /// Its parts' X and Y are all ones [`usable_xy`] takes: the parts kept share it, as they share
    /// `drawable` and `rank`.
    usable: bool,
    rank: f64,
    lod: Option<String>,
    parts: Vec<T>,
    /// The highest level of detail with a geometry whose X or Y can't be drawn ([`usable_xy`]).
    unusable: Option<f64>,
    /// One of those would draw something but for its X or Y.
    would_draw: bool,
}

impl<T> Default for Highest<T> {
    fn default() -> Self {
        Highest { drawable: false, usable: false, rank: f64::NEG_INFINITY, lod: None, parts: Vec::new(), unusable: None, would_draw: false }
    }
}

impl<T> Highest<T> {
    /// Adds a geometry's parts at the level of detail `rank` (a number to compare), written `lod`.
    fn add(&mut self, rank: f64, lod: impl FnOnce() -> Option<String>, parts: Vec<T>, usable: bool, drawable: bool) {
        if !usable {
            self.unusable = Some(self.unusable.map_or(rank, |r| r.max(rank)));
            self.would_draw |= drawable;
        }
        let drawable = usable && drawable;
        // Usable parts go before others at a higher level too: a line of one point kept is a row
        // with geometry, and the one left out is said.
        let key = |d: bool, u: bool, r: f64| (d, u, r);
        if key(drawable, usable, rank) > key(self.drawable, self.usable, self.rank) {
            *self = Highest { drawable, usable, rank, lod: lod(), parts: Vec::new(), ..*self };
        }
        if drawable == self.drawable && usable == self.usable && rank == self.rank {
            self.parts.extend(parts);
        }
    }

    /// A geometry whose X or Y can't be drawn was left out, at this level or above, though
    /// the family draws: no notice says so otherwise.
    fn left_unusable(&self) -> bool {
        self.drawable && self.unusable.is_some_and(|r| r >= self.rank)
    }

    /// A geometry that would draw but for its X or Y was left out, and the family keeps one that
    /// draws nothing (a line of one point).
    fn lost_unusable(&self) -> bool {
        !self.drawable && self.would_draw
    }
}

impl Highest<Vec<P3>> {
    /// Adds a geometry's lines, which the caller has found aren't empty. They draw when one has two
    /// different points.
    pub(crate) fn add_lines(&mut self, rank: f64, lod: impl FnOnce() -> Option<String>, lines: Vec<Vec<P3>>) {
        let usable = lines.iter().flatten().all(usable_xy);
        // Two points that differ, told apart only by X and Y that are numbers: NaN differs from itself.
        let drawable = lines.iter().any(|line| {
            let mut points = line.iter().filter(|p| !p[0].is_nan() && !p[1].is_nan());
            points.next().is_some_and(|first| points.any(|p| first[..2] != p[..2]))
        });
        self.add(rank, lod, lines, usable, drawable);
    }
}

impl Highest<(P3, bool)> {
    /// Adds a geometry's points, which the caller has found aren't empty, each with whether it is
    /// a geometry instance's.
    pub(crate) fn add_points(&mut self, rank: f64, lod: impl FnOnce() -> Option<String>, points: Vec<(P3, bool)>) {
        let usable = points.iter().all(|(p, _)| usable_xy(p));
        self.add(rank, lod, points, usable, true);
    }
}

/// An object's lowest and highest Z, and whether some height wasn't a finite number.
#[derive(Default)]
pub(crate) struct Heights {
    z: Option<(f64, f64)>,
    bad: bool,
}

impl Heights {
    /// Takes in some points' heights.
    pub(crate) fn add(&mut self, points: &[P3]) {
        for h in points.iter().map(|p| p[2]) {
            if h.is_finite() {
                self.z = Some(self.z.map_or((h, h), |(lo, hi): (f64, f64)| (lo.min(h), hi.max(h))));
            } else {
                self.bad = true;
            }
        }
    }
}

/// What a reader finds in one object's geometries: its surfaces, its lines and points at their
/// highest level of detail (points with whether each is a geometry instance's), its heights, and
/// whether it has solids. The heights are those of every point, line and ring the reader keeps, but
/// not a geometry instance's: they are in its template, which isn't read.
#[derive(Default)]
pub(crate) struct ObjectParts {
    pub(crate) surfaces: Vec<Surface>,
    pub(crate) lines: Highest<Vec<P3>>,
    pub(crate) points: Highest<(P3, bool)>,
    pub(crate) heights: Heights,
    pub(crate) solid: bool,
}

/// Whether the heights of some points differ.
fn heights_differ<'a>(mut points: impl Iterator<Item = &'a P3>) -> bool {
    let finite = points.by_ref().map(|p| p[2]).find(|h| h.is_finite());
    finite.is_some_and(|first| points.any(|p| p[2].is_finite() && p[2] != first))
}

/// An object's rows, its heights, and whether it has solids, from its parts.
pub(crate) fn object_rows(parts: ObjectParts, object: usize, counts: &mut Counts, budget: &mut usize) -> ObjectRows {
    let ObjectParts { surfaces, lines, points, heights, solid } = parts;
    counts.bad_heights += usize::from(heights.bad);
    let mut rows = Vec::new();
    // Counted once for the object, whichever of its families are lost.
    let mut shapeless = false;
    // A geometry with X or Y that can't be drawn was passed over for another: counted once too.
    let mut unusable = lines.left_unusable() || points.left_unusable();
    // One that would have drawn was left out, and what is kept draws nothing: not shapeless, so said
    // apart.
    let mut lost = false;
    let mut push = |family: Family, lod: Option<String>, geometry: Option<Geometry>, z_dropped: bool, drawable: bool| {
        shapeless |= geometry.is_none();
        let draws = drawable && geometry.is_some();
        rows.push(Row { family, geometry, object, lod, z_dropped, unconverted: false, draws });
    };
    if !surfaces.is_empty() {
        let (geometry, lod, passed_unusable) = object_footprint(surfaces, counts, budget);
        unusable |= passed_unusable;
        push(Family::Polygon, lod, geometry, false, true);
    }
    let coords = |p: &[P3]| p.iter().map(|p| Coord::new(p[0], p[1])).collect::<Vec<_>>();
    let lines_lost = lines.lost_unusable();
    let Highest { lod: line_lod, parts: lines, drawable: lines_draw, usable: lines_usable, .. } = lines;
    if !lines.is_empty() {
        let geometry = lines_usable.then(|| Geometry::Line(lines.iter().map(|l| coords(l)).collect()));
        lost |= lines_lost && geometry.is_some();
        push(Family::Line, line_lod, geometry, heights_differ(lines.iter().flatten()), lines_draw);
    }
    let Highest { lod: point_lod, parts: points, usable: points_usable, .. } = points;
    // Only those drawn: a point row kept without geometry draws no instance.
    if points_usable {
        counts.instances += points.iter().filter(|(_, instance)| *instance).count();
    }
    let points: Vec<P3> = points.into_iter().map(|(p, _)| p).collect();
    if !points.is_empty() {
        let geometry = points_usable.then(|| match points.as_slice() {
            [p] => Geometry::Point(Coord::new(p[0], p[1])),
            many => Geometry::MultiPoint(coords(many)),
        });
        push(Family::Point, point_lod, geometry, heights_differ(points.iter()), true);
    }
    counts.no_footprint += usize::from(shapeless);
    counts.unusable_passed += usize::from(unusable);
    counts.unusable_lost += usize::from(lost);
    ObjectRows { rows, z: heights.z, solid }
}

/// An object's heights take in its children's: a building whose solids are all in its parts stands
/// as tall as they do, though its own geometry is a flat outline or nothing. As many rounds as there
/// are objects settle any chain or cycle, but only as many as `visits` pays for, and never fewer than
/// [`MIN_PARENT_ROUNDS`], so a crafted chain's cost is bounded. Gives the rounds it stopped after,
/// when heights were still moving.
pub(crate) fn raise_parents(built: &mut [Built], visits: usize) -> Option<usize> {
    let links: Vec<Vec<usize>> = {
        let index: HashMap<&str, usize> = built.iter().enumerate().map(|(i, b)| (b.id.as_str(), i)).collect();
        built.iter().map(|b| b.parent_ids.iter().filter_map(|p| index.get(p.as_str()).copied()).collect()).collect()
    };
    let per_round = links.iter().map(|l| l.len() + 1).sum::<usize>().max(1);
    let rounds = built.len().min(visits / per_round).max(MIN_PARENT_ROUNDS);
    for _ in 0..rounds {
        let mut changed = false;
        for (i, parents) in links.iter().enumerate() {
            let Some((lo, hi)) = built.get(i).and_then(|b| b.z) else { continue };
            for &p in parents {
                if let Some(parent) = built.get_mut(p) {
                    let raised = parent.z.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi)));
                    changed |= parent.z != Some(raised);
                    parent.z = Some(raised);
                }
            }
        }
        if !changed {
            return None;
        }
    }
    // The last round may have been the one that settled it: that says too much, never too little.
    Some(rounds)
}

/// A row's properties, its attributes under their layer's `names` (unique, as [`attribute_names`]
/// makes them), sorted by name as a GeoJSON feature's are. Attributes are borrowed, not copied.
fn properties<'a>(b: &'a Built, lod: Option<&'a str>, names: &'a HashMap<String, String>) -> BTreeMap<&'a str, Cow<'a, Json>> {
    let (lo, hi) = (b.z.map(|z| z.0), b.z.map(|z| z.1));
    let parent = (!b.parent_ids.is_empty()).then(|| b.parent_ids.join(", "));
    let fixed = [
        ("id", Json::from(b.id.as_str())),
        ("type", Json::from(b.ty.as_str())),
        ("parent", parent.map_or(Json::Null, Json::from)),
        ("lod", lod.map_or(Json::Null, Json::from)),
    ];
    let heights = [("z_min", number(lo)), ("z_max", number(hi)), ("height", number(lo.zip(hi).map(|(l, h)| h - l)))];
    let own = b.attributes.iter().map(|(k, v)| (names.get(k).map_or(k.as_str(), String::as_str), Cow::Borrowed(v)));
    fixed.into_iter().map(|(k, v)| (k, Cow::Owned(v))).chain(own).chain(heights.into_iter().map(|(k, v)| (k, Cow::Owned(v)))).collect()
}

/// A height as JSON: null when there is none, or it isn't a finite number.
fn number(v: Option<f64>) -> Json {
    v.and_then(serde_json::Number::from_f64).map_or(Json::Null, Json::Number)
}

/// A layer's fields in the order they show: the reader's own, the object's attributes (which GeoJSON's
/// reader gives sorted by name), then the heights.
fn field_order(fields: &[Field]) -> Vec<usize> {
    let rank = |f: &Field| {
        let lead = LEAD.iter().position(|r| *r == f.name);
        lead.or_else(|| HEIGHTS.iter().position(|r| *r == f.name).map(|at| LEAD.len() + 1 + at)).unwrap_or(LEAD.len())
    };
    let mut order: Vec<usize> = (0..fields.len()).collect();
    order.sort_by_key(|&i| fields.get(i).map_or(LEAD.len(), rank));
    order
}

/// Layer names made distinct, in order and ignoring case (as a `layer` pick reads them): types cut
/// to [`MAX_TYPE_IN_NAME`] may share a name, as may a type's lines and a type named so. A name
/// already given becomes the first `name (n)` no layer has.
pub(crate) fn unique_names(names: Vec<String>) -> Vec<String> {
    let mut taken: HashSet<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let mut given: HashSet<String> = HashSet::new();
    names
        .into_iter()
        .map(|name| {
            if given.insert(name.to_ascii_lowercase()) {
                return name;
            }
            // `taken` is finite (a name and a suffixed one per layer, at most 64), so a free
            // number turns up; the fallback is never reached.
            let free = (2u32..).map(|n| format!("{name} ({n})")).find(|c| !taken.contains(&c.to_ascii_lowercase()));
            let free = free.unwrap_or(name);
            taken.insert(free.to_ascii_lowercase());
            given.insert(free.to_ascii_lowercase());
            free
        })
        .collect()
}

/// The rows as layers: their fields typed as GeoJSON's are, so a city model's attribute reads like
/// any. Those that draw something come first: a file named `.json` adds only the first layer, and
/// the cap leaves out the last.
fn layers(stem: &str, rows: Vec<Row>, built: &[Built], crs: Crs, counts: &mut Counts) -> Vec<(Dataset, ReadReport)> {
    let mut groups: BTreeMap<(String, Family), Vec<(Row, &Built)>> = BTreeMap::new();
    for row in rows {
        let Some(b) = built.get(row.object) else { continue };
        groups.entry((b.ty.clone(), row.family)).or_default().push((row, b));
    }
    let total = groups.len();
    let mut families: HashMap<String, usize> = HashMap::new();
    for (ty, _) in groups.keys() {
        *families.entry(ty.clone()).or_default() += 1;
    }
    let mut sorted: Vec<_> = groups.into_iter().collect();
    // Layers that draw go first, and then those with some geometry: a `.json` file adds the first.
    sorted.sort_by_cached_key(|(_, rows)| (!rows.iter().any(|(r, _)| r.draws), !rows.iter().any(|(r, _)| r.geometry.is_some())));
    sorted.truncate(MAX_LAYERS);
    let names = unique_names(
        sorted
            .iter()
            .map(|((ty, family), _)| {
                let suffix = match family {
                    Family::Line if families.get(ty).is_some_and(|n| *n > 1) => " lines",
                    Family::Point if families.get(ty).is_some_and(|n| *n > 1) => " points",
                    _ => "",
                };
                let shown: String = ty.chars().take(MAX_TYPE_IN_NAME).collect();
                if total == 1 { stem.to_string() } else { format!("{stem} {shown}{suffix}") }
            })
            .collect(),
    );
    let mut out = Vec::with_capacity(total.min(MAX_LAYERS));
    for (((_, family), rows), name) in sorted.into_iter().zip(names) {
        let renames = attribute_names(rows.iter().map(|(_, b)| *b), counts);
        // Typed as the GeoJSON reader types properties, without writing them out and reading them
        // back: first every value for the types, then each row's, one row at a time.
        let mut schema = crate::io::Schema::default();
        for (r, b) in &rows {
            for (k, v) in &properties(b, r.lod.as_deref(), &renames) {
                schema.see(k, v);
            }
        }
        let fields = schema.fields();
        let any = rows.iter().any(|(r, _)| r.geometry.is_some());
        let kind = match family {
            _ if !any => GeometryType::None,
            Family::Polygon => GeometryType::Polygon,
            Family::Line => GeometryType::Polyline,
            Family::Point => GeometryType::Point,
        };
        let order = field_order(&fields);
        // Where each field goes among the layer's, in `order`.
        let mut at = vec![0; fields.len()];
        for (to, &from) in order.iter().enumerate() {
            if let Some(slot) = at.get_mut(from) {
                *slot = to;
            }
        }
        let mut ds = Dataset::new(&name, kind, order.iter().filter_map(|&i| fields.get(i).cloned()).collect());
        let z_dropped = rows.iter().filter(|(r, _)| r.z_dropped).count();
        // What couldn't be converted can't come back by defining the projection again.
        ds.crs_incomplete = rows.iter().any(|(r, _)| r.unconverted);
        for (row, b) in rows {
            let mut attrs = vec![Value::Null; fields.len()];
            for (k, v) in &properties(b, row.lod.as_deref(), &renames) {
                let i = schema.index(k);
                if let Some(field) = i.and_then(|i| fields.get(i))
                    && let Some(slot) = i.and_then(|i| at.get(i)).and_then(|&to| attrs.get_mut(to))
                {
                    *slot = crate::io::coerce(Value::from_json(v), field.ty);
                }
            }
            ds.push_next(row.geometry, attrs);
        }
        let mut report = ReadReport::default();
        ds.crs = (crs != Crs::LonLat).then_some(crs);
        ds.drop_zm(&mut report, z_dropped);
        out.push((ds, report));
    }
    if total > MAX_LAYERS
        && let Some((_, report)) = out.first_mut()
    {
        report.messages.push(format!("{} of {total} layers are left out: only the first {MAX_LAYERS} are read", total - MAX_LAYERS));
    }
    out
}

/// Some of a set's names, for a notice, and how many more there are.
fn listed(names: &BTreeSet<String>) -> String {
    let mut text = names.iter().take(MAX_LISTED).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > MAX_LISTED {
        text.push_str(&format!(" and {} more", names.len() - MAX_LISTED));
    }
    text
}

/// One notice per kind, with a count.
fn messages(c: &Counts) -> Vec<String> {
    let mut out = Vec::new();
    let mut say = |n: usize, text: String| {
        if n > 0 {
            out.push(text);
        }
    };
    say(c.skipped, format!("{} city objects couldn't be read and are left out", c.skipped));
    say(
        c.footprints,
        format!(
            "{} 3D city objects are drawn as their footprints, with their lowest and highest Z in z_min and z_max, and the difference in height",
            c.footprints
        ),
    );
    say(c.wall_hulls, format!("{} city objects with no faces seen from above are drawn as the outline of their points", c.wall_hulls));
    say(
        c.unmerged,
        if cfg!(target_arch = "wasm32") {
            format!("{} city objects are drawn as the outline of their points (the desktop app draws their footprints)", c.unmerged)
        } else {
            format!("{} city objects whose faces couldn't be merged are drawn as the outline of their points", c.unmerged)
        },
    );
    say(
        c.capped,
        format!(
            "{} city objects have a geometry of more than {MAX_FACES} faces, past which faces aren't merged: they are drawn as the outline of their points",
            c.capped
        ),
    );
    say(
        c.no_footprint,
        format!(
            "{} city objects have a shape that can't be drawn (a footprint whose points don't outline an area, fewer than three or all on one line, or points whose X or Y isn't a finite number or is too large): the shape is kept without geometry",
            c.no_footprint
        ),
    );
    say(
        c.unusable_passed,
        format!(
            "{} city objects have a geometry whose X or Y can't be drawn (not a finite number, or too large), left out: they are drawn from another of their geometries or levels of detail",
            c.unusable_passed
        ),
    );
    say(
        c.unusable_lost,
        format!(
            "{} city objects have a line whose X or Y can't be drawn (not a finite number, or too large), left out: what is kept of their lines draws nothing",
            c.unusable_lost
        ),
    );
    say(c.no_geometry, format!("{} city objects have no geometry of their own: they are kept without geometry", c.no_geometry));
    say(
        c.bad_geometries,
        format!("{} geometries of city objects have vertex indexes that can't be read and are left out", c.bad_geometries),
    );
    say(c.bad_attributes, format!("{} city objects have attributes that aren't names and values, which are left out", c.bad_attributes));
    say(c.addresses, format!("the addresses of {} city objects are left out", c.addresses));
    say(c.dropped_rings, format!("{} rings with fewer than three different points are left out", c.dropped_rings));
    say(c.instances, format!("{} geometry instances are drawn as a point at their reference point, with no height", c.instances));
    say(c.tin_reliefs, format!("{} TINRelief objects aren't drawn and are left out", c.tin_reliefs));
    say(
        c.unconverted,
        format!("{} city objects have coordinates outside their coordinate system: they are kept without geometry", c.unconverted),
    );
    say(
        c.bad_heights,
        format!("{} city objects have heights that aren't finite numbers, which z_min, z_max and height leave out", c.bad_heights),
    );
    say(
        c.height_overflows,
        format!(
            "{} city objects have a lowest and highest Z too far apart for their difference to be a number: their height is left empty",
            c.height_overflows
        ),
    );
    say(
        c.parent_rounds,
        format!(
            "parent chains go deeper than {} levels: the heights of some parents don't take in all their descendants'",
            c.parent_rounds
        ),
    );
    if !c.unknown.is_empty() {
        out.push(format!("geometries of unknown types are left out: {}", listed(&c.unknown)));
    }
    if !c.renamed.is_empty() {
        out.push(format!("attributes renamed with a number, as these fields are the reader's own: {}", listed(&c.renamed)));
    }
    if !c.cased.is_empty() {
        out.push(format!(
            "attributes renamed with a number, as another of their layer has the same name in another case: {}",
            listed(&c.cased)
        ));
    }
    out
}
