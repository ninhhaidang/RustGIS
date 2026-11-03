//! CityJSON 1.1 and 2.0 (the JSON encoding of the CityGML data model), read as 2D: each City Object
//! becomes a row of a layer per object type and geometry family. Points and lines stay as they are.
//! Surfaces and solids become footprints, from the first source that gives one: LoD 0, the surfaces
//! marked `GroundSurface`, then the union of the faces of the highest LoD (see [`crate::footprint`]).
//! An object's lowest and highest Z, its children's included, are kept in `z_min` and `z_max`, and
//! the difference in `height`, the names File Geodatabase multipatches use, so an extrusion stands
//! both up the same way. Written from the specifications: <https://www.cityjson.org/specs/>.
//!
//! The file is hostile: vertex indexes are checked, the nesting is fixed by the geometry types, and
//! the faces merged are bounded. Each object's geometry is parsed on its own and dropped once its rows
//! are built; its attributes and the root metadata are kept as JSON values until the layers are
//! written.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Value as Json, value::RawValue};

use crate::footprint::{Hull, P3, footprint};
use crate::raster::{Crs, Transformer};
use crate::{Coord, DataError, Dataset, Field, Geometry, GeometryType, ReadReport, Result, Value};

/// The most layers read: extension types make their number the file's to choose.
const MAX_LAYERS: usize = 64;
/// Most faces of one geometry merged into its footprint; beyond, its outline is its points' hull.
const MAX_FACES: usize = 200_000;
/// Most faces merged for one file, whatever its objects ask: past it, footprints are hulls, so a
/// crafted file can't make loading take hours.
const MAX_FILE_FACES: usize = if cfg!(target_arch = "wasm32") { 500_000 } else { 4_000_000 };
/// The rounds of passing heights up to parents always allowed. The specification has two levels.
const MIN_PARENT_ROUNDS: usize = 8;
/// Most objects and parent links visited passing heights up: past it, a crafted chain stops, with a
/// notice, rather than costing the square of its length. [`MIN_PARENT_ROUNDS`] are run whatever it
/// says, so a file with more links than it pays for eight rounds of costs those eight.
const MAX_PARENT_VISITS: usize = if cfg!(target_arch = "wasm32") { 50_000_000 } else { 200_000_000 };
/// Most characters of an object type shown in a layer's name.
const MAX_TYPE_IN_NAME: usize = 64;
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

/// Whether a file's name says it is CityJSON.
pub fn is_cityjson_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".city.json")
}

/// Whether JSON text is a CityJSON document. Its root `type` can come anywhere (megabytes in, after
/// the objects), so the keys of the root are walked, only when the text names `CityObjects`.
pub fn looks_like_cityjson(text: &str) -> bool {
    #[derive(Deserialize)]
    struct Head {
        #[serde(rename = "type")]
        ty: Option<String>,
    }
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with('{')
        && text.contains("\"CityObjects\"")
        && serde_json::from_str::<Head>(text).is_ok_and(|h| h.ty.as_deref() == Some("CityJSON"))
}

#[derive(Deserialize)]
struct Doc<'a> {
    #[serde(rename = "type")]
    ty: Option<String>,
    version: Option<Json>,
    transform: Option<Transform>,
    #[serde(default)]
    vertices: Vec<P3>,
    #[serde(rename = "CityObjects", borrow)]
    objects: Option<BTreeMap<String, &'a RawValue>>,
    metadata: Option<Json>,
    #[serde(borrow)]
    appearance: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Transform {
    scale: P3,
    translate: P3,
}

#[derive(Deserialize)]
struct RawObject {
    #[serde(rename = "type")]
    ty: String,
    attributes: Option<Json>,
    #[serde(default)]
    geometry: Vec<RawGeometry>,
    #[serde(default)]
    parents: Vec<String>,
    /// Not read, only said.
    address: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct RawGeometry {
    #[serde(rename = "type")]
    ty: String,
    lod: Option<Json>,
    #[serde(default)]
    boundaries: Json,
    semantics: Option<Json>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    Polygon,
    Line,
    Point,
}

/// A row of a layer: an object has one per geometry family.
struct Row {
    family: Family,
    geometry: Option<Geometry>,
    /// Its object, in [`Built`].
    object: usize,
    /// The level of detail its geometry came from.
    lod: Option<String>,
    /// Its points' heights differ, and only their lowest and highest are kept.
    z_dropped: bool,
    /// Its geometry couldn't be converted to longitude/latitude.
    unconverted: bool,
}

/// An object's values, one per object whatever rows it makes.
struct Built {
    id: String,
    ty: String,
    parent_ids: Vec<String>,
    z: Option<(f64, f64)>,
    /// It has solids, so it is 3D however flat its heights.
    solid: bool,
    attributes: Vec<(String, Json)>,
}

/// What was left out or approximated, for one notice per kind.
#[derive(Default)]
struct Counts {
    skipped: usize,
    footprints: usize,
    wall_hulls: usize,
    unmerged: usize,
    no_footprint: usize,
    no_geometry: usize,
    bad_geometries: usize,
    bad_attributes: usize,
    addresses: usize,
    dropped_rings: usize,
    instances: usize,
    tin_reliefs: usize,
    unconverted: usize,
    bad_heights: usize,
    /// Objects whose lowest and highest Z are too far apart for their difference to be a number.
    height_overflows: usize,
    /// The rounds after which passing heights up to parents stopped, when it did.
    parent_rounds: usize,
    unknown: BTreeSet<String>,
    renamed: BTreeSet<String>,
}

/// One surface geometry's faces, each its outer ring and then its holes.
#[derive(Default)]
struct Faces {
    faces: Vec<Vec<Vec<P3>>>,
    /// Whether each face is marked `GroundSurface` (beside `faces`, which is what a footprint takes).
    ground: Vec<bool>,
    /// The rings of what isn't in `faces`, whose points count for the outline all the same.
    left_out: Vec<Vec<P3>>,
    /// Faces past [`MAX_FACES`] were left out.
    truncated: bool,
    dropped_rings: usize,
}

/// A surface geometry and its level of detail, as written and as a number to compare.
struct Surface {
    lod: Option<String>,
    rank: f64,
    faces: Faces,
}

/// The points of a ring or line, `None` for an index that isn't one.
fn at(indexes: &Json, vertices: &[P3]) -> Option<Vec<P3>> {
    indexes.as_array()?.iter().map(|i| vertices.get(usize::try_from(i.as_u64()?).ok()?).copied()).collect()
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
    /// `boundaries`, surfaces nested `depth` levels deep, with their semantic `values` alongside.
    fn walk(&mut self, boundaries: &Json, values: Option<&Json>, depth: usize, marks: &[Json], vertices: &[P3]) -> Option<()> {
        for (i, part) in boundaries.as_array()?.iter().enumerate() {
            let value = values.and_then(|v| v.get(i));
            // At this depth the parts are a solid's shells: past the first they are cavities, whose
            // floors are no footprint.
            let marks = if depth == 1 && i > 0 { &[][..] } else { marks };
            if depth > 0 {
                self.walk(part, value, depth - 1, marks, vertices)?;
            } else {
                self.surface(part, value, marks, vertices)?;
            }
        }
        Some(())
    }

    fn surface(&mut self, rings: &Json, value: Option<&Json>, marks: &[Json], vertices: &[P3]) -> Option<()> {
        let mut kept = Vec::new();
        let mut outer = true;
        for (k, ring) in rings.as_array()?.iter().enumerate() {
            let points = at(ring, vertices)?;
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
            return Some(());
        }
        if self.faces.len() >= MAX_FACES {
            self.truncated = true;
            self.left_out.append(&mut kept);
            return Some(());
        }
        let ground = value
            .and_then(Json::as_u64)
            .and_then(|m| marks.get(usize::try_from(m).ok()?))
            .and_then(|m| m.get("type"))
            .and_then(Json::as_str)
            == Some("GroundSurface");
        self.faces.push(kept);
        self.ground.push(ground);
        Some(())
    }

    /// All its rings.
    fn rings(&self) -> impl Iterator<Item = &Vec<P3>> {
        self.faces.iter().flatten().chain(&self.left_out)
    }
}

/// A geometry's level of detail as a number to compare (none, or one that isn't a number, is the
/// lowest).
fn lod_number(lod: Option<&Json>) -> f64 {
    let n = match lod {
        Some(Json::String(s)) => s.trim().parse().unwrap_or(-1.0),
        Some(n) => n.as_f64().unwrap_or(-1.0),
        None => -1.0,
    };
    if n.is_finite() { n } else { -1.0 }
}

fn lod_text(lod: Option<&Json>) -> Option<String> {
    match lod {
        Some(Json::String(s)) => Some(s.clone()),
        Some(n @ Json::Number(_)) => Some(n.to_string()),
        _ => None,
    }
}

/// A face that is already its footprint: it needs no union, so the browser keeps its shape too. Not
/// one that shows nothing from above, by the rule [`footprint`] measures a face with.
fn lone_face(face: &[Vec<P3>]) -> Option<Geometry> {
    if !face.iter().flatten().all(|p| p[0].is_finite() && p[1].is_finite()) {
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
    (crate::geometry::ring_area(outer).abs() > size * size * 5e-5).then(|| Geometry::Polygon(vec![rings]))
}

/// An object's footprint and the level of detail it came from: the first of its LoD 0, the
/// `GroundSurface` of the highest LoD with one, and the union of its highest LoD's faces, those of
/// all its geometries at that level. Failing all, the outline of the highest LoD's points.
fn object_footprint(surfaces: Vec<Surface>, counts: &mut Counts, budget: &mut usize) -> (Option<Geometry>, Option<String>) {
    let highest = |keep: fn(&Surface) -> bool| surfaces.iter().filter(|s| keep(s)).map(|s| s.rank).max_by(f64::total_cmp);
    let lod0 = surfaces.iter().any(|s| s.rank == 0.0 && !s.faces.faces.is_empty());
    let (rank, ground_only) = match (lod0, highest(|s| s.faces.ground.contains(&true))) {
        (true, _) => (Some(0.0), false),
        (false, Some(rank)) => (Some(rank), true),
        (false, None) => (highest(|s| !s.faces.faces.is_empty()).or_else(|| highest(|s| !s.faces.left_out.is_empty())), false),
    };
    let Some(rank) = rank else { return (None, None) };
    let (mut faces, mut left_out): (Vec<Vec<Vec<P3>>>, Vec<Vec<P3>>) = (Vec::new(), Vec::new());
    let (mut truncated, mut lod) = (false, None);
    for s in surfaces.into_iter().filter(|s| s.rank == rank) {
        lod = lod.or(s.lod);
        truncated |= s.faces.truncated;
        if ground_only {
            faces.extend(s.faces.faces.into_iter().zip(s.faces.ground).filter(|(_, ground)| *ground).map(|(face, _)| face));
        } else {
            faces.extend(s.faces.faces);
            left_out.extend(s.faces.left_out);
        }
    }
    // From LoD 0 or the ground, one face is the footprint as it stands.
    if (lod0 || ground_only)
        && !truncated
        && let [face] = faces.as_slice()
        && let Some(geometry) = lone_face(face)
    {
        return (Some(geometry), lod);
    }
    let points: Vec<P3> = faces.iter().flatten().chain(&left_out).flatten().copied().collect();
    let (geometry, hull) = footprint(&faces, truncated, std::slice::from_ref(&points), budget);
    counts.wall_hulls += usize::from(hull == Some(Hull::NoFaces));
    counts.unmerged += usize::from(hull == Some(Hull::Unmerged));
    (geometry, lod)
}

/// The horizontal part of the compound (and 3D) systems city models name, some of whose definitions
/// here are missing or ask for grids: heights are kept as they come, not converted.
fn horizontal_epsg(code: u32) -> u32 {
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

/// The system a file's metadata names, if it names one.
fn named_system(metadata: Option<&Json>) -> Option<&str> {
    metadata.and_then(|m| m.get("referenceSystem")).and_then(Json::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// The system a name stands for, by its horizontal part, if RustGIS knows it.
fn known_system(system: &str) -> Option<Crs> {
    crate::ogc::warp::epsg_of(system).and_then(|c| Crs::from_epsg(horizontal_epsg(c)))
}

/// The coordinate system: the one given, the one the file names, or longitude/latitude when it
/// names none and every vertex can be one. Anything else is an error, not a guess. A file only names
/// EPSG codes: what it says is never registered as a new definition.
fn coordinate_system(
    file_name: &str,
    given: Option<Crs>,
    metadata: Option<&Json>,
    vertices: &[P3],
    notes: &mut Vec<String>,
) -> Result<Crs> {
    let within = |x: f64, y: f64| vertices.iter().all(|p| p[0].abs() <= x && p[1].abs() <= y);
    let crs = match (given, named_system(metadata)) {
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
            DataError::Format(format!("{file_name}: it names a coordinate system RustGIS doesn't know ({system})"))
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

/// What a file's appearance holds, which isn't read: a notice, or nothing for one that holds nothing.
fn appearance_note(raw: &RawValue) -> Option<String> {
    #[derive(Deserialize)]
    struct Appearance {
        #[serde(default)]
        materials: Vec<IgnoredAny>,
        #[serde(default)]
        textures: Vec<IgnoredAny>,
    }
    let Ok(held) = serde_json::from_str::<Appearance>(raw.get()) else {
        return Some("the file's appearance isn't read".to_string());
    };
    let (materials, textures) = (held.materials.len(), held.textures.len());
    (materials + textures > 0)
        .then(|| format!("the file's appearance isn't read: {materials} materials and {textures} textures are left out"))
}

/// The name a layer's type goes by: an extension's as written, but `+GenericCityObject` (1.1) and
/// `GenericCityObject` (2.0) share one.
fn type_name(ty: &str) -> String {
    let ty = if ty == "+GenericCityObject" { "GenericCityObject" } else { ty.trim() };
    if ty.is_empty() { "CityObject".to_string() } else { ty.to_string() }
}

/// An object's attributes, apart from the fields every row has, and from each other.
fn attribute_names(attributes: serde_json::Map<String, Json>, counts: &mut Counts) -> Vec<(String, Json)> {
    let mut used: BTreeSet<String> = attributes.keys().map(|k| k.to_ascii_lowercase()).collect();
    // The number each reserved name goes on from, whatever its case: `used` is finite, so one is
    // always free, and the spellings of a name don't each search again past the same numbers.
    let mut next: HashMap<String, usize> = HashMap::new();
    attributes
        .into_iter()
        .map(|(name, value)| {
            if !is_reserved(&name) {
                return (name, value);
            }
            counts.renamed.insert(name.chars().take(64).collect());
            let k = next.entry(name.to_ascii_lowercase()).or_insert(1);
            let free = loop {
                let candidate = format!("{name}_{k}");
                *k += 1;
                if used.insert(candidate.to_ascii_lowercase()) {
                    break candidate;
                }
            };
            (free, value)
        })
        .collect()
}

/// Reads a CityJSON file into a layer per object type and geometry family, those with something to
/// draw first. `given` is its coordinate system, when the caller knows it, over the one it names.
pub fn read(file_name: &str, bytes: &[u8], given: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    read_with(file_name, bytes, given, false)
}

/// As [`read`], for a member of an archive, where the system given is meant for what can't say its
/// own: a model that names one RustGIS knows keeps it.
pub(crate) fn read_member(file_name: &str, bytes: &[u8], given: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    read_with(file_name, bytes, given, true)
}

fn read_with(file_name: &str, bytes: &[u8], given: Option<Crs>, keep_named: bool) -> Result<Vec<(Dataset, ReadReport)>> {
    if bytes.len() as u64 > crate::io::MAX_BYTES {
        return Err(DataError::Format(format!("file is too large ({} bytes)", bytes.len())));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")))?;
    let leaf = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let stem = if leaf.to_ascii_lowercase().ends_with(".city") { leaf.get(..leaf.len() - 5).unwrap_or(leaf) } else { leaf };
    let stem = if stem.is_empty() { "city model" } else { stem };
    let doc: Doc = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    if doc.ty.as_deref() != Some("CityJSON") {
        return Err(DataError::Format(format!("{file_name}: not a CityJSON document")));
    }
    let version = doc.version.as_ref().and_then(Json::as_str).unwrap_or("");
    let mut numbers = version.split('.');
    if !matches!((numbers.next(), numbers.next()), (Some("1"), Some("1")) | (Some("2"), Some("0"))) {
        let said = if version.is_empty() { "of no version".to_string() } else { format!("version {version}") };
        return Err(DataError::Unsupported(format!("{file_name}: CityJSON {said}: RustGIS reads 1.1 and 2.0 (cjio upgrades older files)")));
    }
    let Some(transform) = doc.transform else {
        return Err(DataError::Format(format!("{file_name}: CityJSON without its \"transform\", which the specification requires")));
    };
    let mut vertices = doc.vertices;
    let (s, t) = (&transform.scale, &transform.translate);
    for v in &mut vertices {
        *v = [v[0] * s[0] + t[0], v[1] * s[1] + t[1], v[2] * s[2] + t[2]];
    }
    let mut notes = Vec::new();
    let given = given.filter(|_| !keep_named || named_system(doc.metadata.as_ref()).and_then(known_system).is_none());
    let crs = coordinate_system(file_name, given, doc.metadata.as_ref(), &vertices, &mut notes)?;
    notes.extend(doc.appearance.and_then(appearance_note));
    let to_lonlat =
        if crs == Crs::LonLat { None } else { Some(crs.transformer().map_err(|e| DataError::Format(format!("{file_name}: {e}")))?) };
    let Some(objects) = doc.objects else {
        return Err(DataError::Format(format!("{file_name}: no CityObjects")));
    };

    let mut counts = Counts::default();
    let mut budget = MAX_FILE_FACES;
    let mut built: Vec<Built> = Vec::with_capacity(objects.len());
    let mut rows: Vec<Row> = Vec::new();
    for (id, raw) in &objects {
        let Ok(object) = serde_json::from_str::<RawObject>(raw.get()) else {
            counts.skipped += 1;
            continue;
        };
        if object.ty == "TINRelief" {
            counts.tin_reliefs += 1;
            continue;
        }
        let at_row = built.len();
        counts.addresses += usize::from(object.address.is_some());
        let attributes = match object.attributes {
            Some(Json::Object(m)) => attribute_names(m, &mut counts),
            None | Some(Json::Null) => Vec::new(),
            Some(_) => {
                counts.bad_attributes += 1;
                Vec::new()
            }
        };
        let made = object_rows(&object.geometry, at_row, &vertices, to_lonlat.as_ref(), &mut counts, &mut budget);
        if made.rows.is_empty() {
            // Its attributes stay: its children, in their own rows, are where it is drawn.
            counts.no_geometry += 1;
            rows.push(Row { family: Family::Polygon, geometry: None, object: at_row, lod: None, z_dropped: false, unconverted: false });
        }
        rows.extend(made.rows);
        built.push(Built {
            id: id.clone(),
            ty: type_name(&object.ty),
            parent_ids: object.parents,
            z: made.z,
            solid: made.solid,
            attributes,
        });
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

    let mut out = layers(stem, rows, &built, crs)?;
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
struct ObjectRows {
    /// One per geometry family.
    rows: Vec<Row>,
    z: Option<(f64, f64)>,
    solid: bool,
}

/// A family's parts at the highest level of detail an object has: its other levels are other
/// pictures of it, not more of it, as for its surfaces. Parts that draw something go before any
/// level: a line of one point at LoD 2 doesn't take the place of a road drawn at LoD 1.
struct Highest<T> {
    drawable: bool,
    rank: f64,
    lod: Option<String>,
    parts: Vec<T>,
}

impl<T> Default for Highest<T> {
    fn default() -> Self {
        Highest { drawable: false, rank: f64::NEG_INFINITY, lod: None, parts: Vec::new() }
    }
}

impl<T> Highest<T> {
    /// Adds a geometry's parts, which the caller has found aren't empty.
    fn add(&mut self, g: &RawGeometry, parts: impl IntoIterator<Item = T>, drawable: bool) {
        let rank = lod_number(g.lod.as_ref());
        let key = |d: bool, r: f64| (d, r);
        if key(drawable, rank) > key(self.drawable, self.rank) {
            *self = Highest { drawable, rank, lod: lod_text(g.lod.as_ref()), parts: Vec::new() };
        }
        if drawable == self.drawable && rank == self.rank {
            self.parts.extend(parts);
        }
    }
}

/// Whether the heights of some points differ.
fn heights_differ<'a>(mut points: impl Iterator<Item = &'a P3>) -> bool {
    let finite = points.by_ref().map(|p| p[2]).find(|h| h.is_finite());
    finite.is_some_and(|first| points.any(|p| p[2].is_finite() && p[2] != first))
}

/// An object's rows, its heights, and whether it has solids. An instance adds none of its heights:
/// they are in its template, which isn't read.
fn object_rows(
    geometries: &[RawGeometry],
    object: usize,
    vertices: &[P3],
    to_lonlat: Option<&Transformer>,
    counts: &mut Counts,
    budget: &mut usize,
) -> ObjectRows {
    let mut z: Option<(f64, f64)> = None;
    let mut bad_height = false;
    let mut heights = |points: &[P3]| {
        for h in points.iter().map(|p| p[2]) {
            if h.is_finite() {
                z = Some(z.map_or((h, h), |(lo, hi): (f64, f64)| (lo.min(h), hi.max(h))));
            } else {
                bad_height = true;
            }
        }
    };
    let (mut surfaces, mut solid) = (Vec::<Surface>::new(), false);
    // Points with whether each is a geometry instance's, which is said only when it is drawn.
    let (mut points, mut lines) = (Highest::<(P3, bool)>::default(), Highest::<Vec<P3>>::default());
    for g in geometries {
        let depth = match g.ty.as_str() {
            "MultiSurface" | "CompositeSurface" => 0,
            "Solid" => 1,
            "MultiSolid" | "CompositeSolid" => 2,
            "MultiPoint" => {
                match at(&g.boundaries, vertices) {
                    Some(p) if p.is_empty() => {}
                    Some(p) => {
                        heights(&p);
                        points.add(g, p.into_iter().map(|p| (p, false)), true);
                    }
                    None => counts.bad_geometries += 1,
                }
                continue;
            }
            "GeometryInstance" => {
                // Its template isn't placed: it is drawn at its reference point.
                match at(&g.boundaries, vertices).as_deref() {
                    Some([p, ..]) => {
                        points.add(g, [(*p, true)], true);
                    }
                    _ => counts.bad_geometries += 1,
                }
                continue;
            }
            "MultiLineString" => {
                let parts: Option<Vec<Vec<P3>>> = g.boundaries.as_array().and_then(|l| l.iter().map(|line| at(line, vertices)).collect());
                match parts {
                    Some(parts) => {
                        let parts: Vec<Vec<P3>> = parts.into_iter().filter(|p| !p.is_empty()).collect();
                        parts.iter().for_each(|p| heights(p));
                        if !parts.is_empty() {
                            let drawable = parts.iter().any(|p| p.iter().any(|q| p.first().is_some_and(|f| f[..2] != q[..2])));
                            lines.add(g, parts, drawable);
                        }
                    }
                    None => counts.bad_geometries += 1,
                }
                continue;
            }
            other => {
                counts.unknown.insert(other.chars().take(64).collect());
                continue;
            }
        };
        let semantics = g.semantics.as_ref();
        let marks = semantics.and_then(|s| s.get("surfaces")).and_then(Json::as_array).map_or(&[][..], Vec::as_slice);
        let mut faces = Faces::default();
        if faces.walk(&g.boundaries, semantics.and_then(|s| s.get("values")), depth, marks, vertices).is_none() {
            counts.bad_geometries += 1;
            continue;
        }
        counts.dropped_rings += faces.dropped_rings;
        faces.rings().for_each(|p| heights(p));
        solid |= depth > 0;
        surfaces.push(Surface { lod: lod_text(g.lod.as_ref()), rank: lod_number(g.lod.as_ref()), faces });
    }
    counts.bad_heights += usize::from(bad_height);
    let mut rows = Vec::new();
    // Counted once for the object, whichever of its families are lost.
    let mut shapeless = false;
    let mut push = |family: Family, lod: Option<String>, geometry: Option<Geometry>, z_dropped: bool| {
        shapeless |= geometry.is_none();
        let (geometry, unconverted) = match (geometry, to_lonlat) {
            (Some(g), Some(t)) => {
                let converted = crate::io::geometry_to_lonlat(&g, t);
                let failed = converted.is_none();
                (converted, failed)
            }
            (g, _) => (g, false),
        };
        rows.push(Row { family, geometry, object, lod, z_dropped, unconverted });
    };
    if !surfaces.is_empty() {
        let (geometry, lod) = object_footprint(surfaces, counts, budget);
        push(Family::Polygon, lod, geometry, false);
    }
    let coords = |p: &[P3]| p.iter().map(|p| Coord::new(p[0], p[1])).collect::<Vec<_>>();
    let finite = |p: &P3| p[0].is_finite() && p[1].is_finite();
    let Highest { lod: line_lod, parts: lines, .. } = lines;
    if !lines.is_empty() {
        let geometry = lines.iter().flatten().all(finite).then(|| Geometry::Line(lines.iter().map(|l| coords(l)).collect()));
        push(Family::Line, line_lod, geometry, heights_differ(lines.iter().flatten()));
    }
    let Highest { lod: point_lod, parts: points, .. } = points;
    counts.instances += points.iter().filter(|(_, instance)| *instance).count();
    let points: Vec<P3> = points.into_iter().map(|(p, _)| p).collect();
    if !points.is_empty() {
        let geometry = points.iter().all(finite).then(|| match points.as_slice() {
            [p] => Geometry::Point(Coord::new(p[0], p[1])),
            many => Geometry::MultiPoint(coords(many)),
        });
        push(Family::Point, point_lod, geometry, heights_differ(points.iter()));
    }
    counts.no_footprint += usize::from(shapeless);
    counts.unconverted += usize::from(rows.iter().any(|r| r.unconverted));
    ObjectRows { rows, z, solid }
}

/// An object's heights take in its children's: a building whose solids are all in its parts stands
/// as tall as they do, though its own geometry is a flat outline or nothing. As many rounds as there
/// are objects settle any chain or cycle, but only as many as `visits` pays for, and never fewer than
/// [`MIN_PARENT_ROUNDS`], so a crafted chain's cost is bounded. Gives the rounds it stopped after,
/// when heights were still moving.
fn raise_parents(built: &mut [Built], visits: usize) -> Option<usize> {
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

/// A row's properties as a GeoJSON feature with no geometry.
fn feature_text(b: &Built, lod: Option<&str>) -> String {
    let (lo, hi) = (b.z.map(|z| z.0), b.z.map(|z| z.1));
    let parent = (!b.parent_ids.is_empty()).then(|| b.parent_ids.join(", "));
    let fixed = [
        ("id", Json::from(b.id.as_str())),
        ("type", Json::from(b.ty.as_str())),
        ("parent", parent.map_or(Json::Null, Json::from)),
        ("lod", lod.map_or(Json::Null, Json::from)),
    ];
    let own = b.attributes.iter().map(|(k, v)| (k.as_str(), v.clone()));
    let heights = [("z_min", number(lo)), ("z_max", number(hi)), ("height", number(lo.zip(hi).map(|(l, h)| h - l)))];
    let fields: Vec<String> = fixed.into_iter().chain(own).chain(heights).map(|(k, v)| format!("{}:{v}", Json::from(k))).collect();
    format!("{{\"type\":\"Feature\",\"geometry\":null,\"properties\":{{{}}}}}", fields.join(","))
}

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

/// The rows as layers: their fields found as GeoJSON's are, so a CityJSON attribute reads like any.
/// Those with something to draw come first: a file named `.json` adds only the first layer, and the
/// cap leaves out the last.
fn layers(stem: &str, rows: Vec<Row>, built: &[Built], crs: Crs) -> Result<Vec<(Dataset, ReadReport)>> {
    let mut groups: BTreeMap<(String, Family), Vec<Row>> = BTreeMap::new();
    for row in rows {
        let Some(b) = built.get(row.object) else { continue };
        groups.entry((b.ty.clone(), row.family)).or_default().push(row);
    }
    let total = groups.len();
    let mut families: HashMap<String, usize> = HashMap::new();
    for (ty, _) in groups.keys() {
        *families.entry(ty.clone()).or_default() += 1;
    }
    let mut sorted: Vec<_> = groups.into_iter().collect();
    sorted.sort_by_key(|(_, rows)| !rows.iter().any(|r| r.geometry.is_some()));
    let mut out = Vec::with_capacity(total.min(MAX_LAYERS));
    for ((ty, family), rows) in sorted.into_iter().take(MAX_LAYERS) {
        let suffix = match family {
            Family::Line if families.get(&ty).is_some_and(|n| *n > 1) => " lines",
            Family::Point if families.get(&ty).is_some_and(|n| *n > 1) => " points",
            _ => "",
        };
        let shown: String = ty.chars().take(MAX_TYPE_IN_NAME).collect();
        let name = if total == 1 { stem.to_string() } else { format!("{stem} {shown}{suffix}") };
        let features: Vec<String> = rows.iter().filter_map(|r| built.get(r.object).map(|b| feature_text(b, r.lod.as_deref()))).collect();
        let text = format!("{{\"type\":\"FeatureCollection\",\"features\":[{}]}}", features.join(","));
        let (table, mut report) = crate::io::read_geojson(&text, &name)?;
        let any = rows.iter().any(|r| r.geometry.is_some());
        let kind = match family {
            _ if !any => GeometryType::None,
            Family::Polygon => GeometryType::Polygon,
            Family::Line => GeometryType::Polyline,
            Family::Point => GeometryType::Point,
        };
        let order = field_order(&table.fields);
        let mut ds = Dataset::new(&name, kind, order.iter().filter_map(|&i| table.fields.get(i).cloned()).collect());
        let z_dropped = rows.iter().filter(|r| r.z_dropped).count();
        // What couldn't be converted can't come back by defining the projection again.
        ds.crs_incomplete = rows.iter().any(|r| r.unconverted);
        // Rows and attributes are paired by position: one missing would shift every later row.
        if table.features.len() != rows.len() {
            return Err(DataError::Format(format!("{name}: {} rows were built but {} have attributes", rows.len(), table.features.len())));
        }
        for (row, feature) in rows.into_iter().zip(table.features.iter()) {
            ds.push_next(row.geometry, order.iter().map(|&i| feature.attrs.get(i).cloned().unwrap_or(Value::Null)).collect());
        }
        ds.crs = (crs != Crs::LonLat).then_some(crs);
        ds.drop_zm(&mut report, z_dropped);
        out.push((ds, report));
    }
    if total > MAX_LAYERS
        && let Some((_, report)) = out.first_mut()
    {
        report.messages.push(format!("{} of {total} layers are left out: only the first {MAX_LAYERS} are read", total - MAX_LAYERS));
    }
    Ok(out)
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
        c.no_footprint,
        format!(
            "{} city objects have a shape that can't be drawn (a footprint whose points don't outline an area, fewer than three or all on one line, or points that aren't finite numbers): the shape is kept without geometry",
            c.no_footprint
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
    out
}

#[cfg(test)]
mod tests;
