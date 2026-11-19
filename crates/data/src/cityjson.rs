//! CityJSON 1.1 and 2.0 (the JSON encoding of the CityGML data model), read as 2D: each City Object
//! becomes a row of a layer per object type and geometry family. Points and lines stay as they are.
//! Surfaces and solids become footprints, with the object's heights in `z_min`, `z_max` and
//! `height`, as every city model is read (see [`crate::citymodel`]). Written from the
//! specifications: <https://www.cityjson.org/specs/>.
//!
//! A geometry instance (`GeometryInstance`) is its template from `geometry-templates`, placed by its
//! matrix at its reference point as CityGML's implicit geometries are (see
//! [`crate::citymodel::Placement`]); one whose template or matrix can't be read is drawn at its
//! reference point, and said.
//!
//! The file is hostile: vertex indexes are checked, the nesting is fixed by the geometry types, the
//! faces merged are bounded, and so are the points instances place, by the file's size. Each
//! object's geometry is parsed on its own and dropped once its rows are built; its attributes and
//! the root metadata are kept as JSON values until the layers are written. The geometry templates,
//! which every instance may place, are parsed once and kept for the whole read, as the vertices
//! are: bounded by the file's size.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Value as Json, value::RawValue};

use crate::citymodel::*;
use crate::footprint::P3;
use crate::raster::Crs;
use crate::{DataError, Dataset, ReadReport, Result};

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
    /// The shapes geometry instances place, read apart so a malformed one costs only the instances.
    #[serde(rename = "geometry-templates", borrow)]
    templates: Option<&'a RawValue>,
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
    /// A geometry instance's: its template's place in `geometry-templates`, and its matrix.
    template: Option<Json>,
    #[serde(rename = "transformationMatrix")]
    matrix: Option<Json>,
}

/// The shapes geometry instances place: each template with the vertices placing it reads, or none
/// when it can't be read (said once, not for each instance), and their vertices, in the templates'
/// own system (the file's `transform` doesn't apply to them).
struct Templates {
    /// Each with how many vertex indexes placing it reads, and how many rings of no area reading it
    /// left out (said then, once, not again for each instance).
    shapes: Vec<Option<(RawGeometry, usize, usize)>>,
    vertices: Vec<P3>,
}

impl Templates {
    /// A file's `geometry-templates`, none when they can't be read: each of its instances is then
    /// drawn at its reference point, and said so.
    fn of(raw: Option<&RawValue>, counts: &mut Counts) -> Option<Templates> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            templates: Vec<Json>,
            #[serde(rename = "vertices-templates", default)]
            vertices: Vec<P3>,
        }
        // A block that can't be read leaves every instance at its reference point, said with them;
        // the block itself is said as a geometry that can't be read.
        let Ok(raw) = serde_json::from_str::<Raw>(raw?.get()) else {
            counts.bad_geometries += 1;
            return None;
        };
        let vertex = |k: usize| raw.vertices.get(k).copied();
        let shapes = raw
            .templates
            .into_iter()
            .map(|t| {
                // A template that can't be read is said here, once; one that is an instance itself
                // places nothing.
                let shape = serde_json::from_value::<RawGeometry>(t).ok().filter(|g| g.ty != "GeometryInstance");
                let dropped = counts.dropped_rings;
                let read = shape.as_ref().and_then(|g| geometry(&mut ObjectParts::default(), g, &vertex, counts));
                if read.is_none() {
                    counts.bad_geometries += 1;
                }
                let dropped = counts.dropped_rings - dropped;
                // One with no vertex at all (`"boundaries": []`) places nothing: its instances are
                // their reference points, and said.
                shape.filter(|g| read.is_some() && has_vertex(&g.boundaries)).map(|g| {
                    let size = indexes(&g.boundaries);
                    (g, size, dropped)
                })
            })
            .collect();
        Some(Templates { shapes, vertices: raw.vertices })
    }
}

/// Whether boundaries hold a vertex index anywhere.
fn has_vertex(boundaries: &Json) -> bool {
    let mut stack = vec![boundaries];
    while let Some(j) = stack.pop() {
        match j {
            Json::Array(items) => stack.extend(items),
            Json::Number(_) => return true,
            _ => {}
        }
    }
    false
}

/// How many vertex indexes boundaries hold, which is what placing them reads; an empty array counts
/// as one, as placing it still makes and drops a ring or a face.
fn indexes(boundaries: &Json) -> usize {
    match boundaries {
        Json::Array(parts) if parts.is_empty() => 1,
        Json::Array(parts) => parts.iter().map(indexes).sum(),
        _ => 1,
    }
}

/// The points of a ring or line, `None` for an index that isn't one.
fn at(indexes: &Json, vertex: &impl Fn(usize) -> Option<P3>) -> Option<Vec<P3>> {
    indexes.as_array()?.iter().map(|i| vertex(usize::try_from(i.as_u64()?).ok()?)).collect()
}

/// `boundaries`, surfaces nested `depth` levels deep, with their semantic `values` alongside, into
/// `faces`.
fn walk(
    faces: &mut Faces,
    boundaries: &Json,
    values: Option<&Json>,
    depth: usize,
    marks: &[Json],
    vertices: &impl Fn(usize) -> Option<P3>,
) -> Option<()> {
    for (i, part) in boundaries.as_array()?.iter().enumerate() {
        let value = values.and_then(|v| v.get(i));
        // At this depth the parts are a solid's shells: past the first they are cavities, whose
        // floors are no footprint.
        let marks = if depth == 1 && i > 0 { &[][..] } else { marks };
        if depth > 0 {
            walk(faces, part, value, depth - 1, marks, vertices)?;
        } else {
            surface(faces, part, value, marks, vertices)?;
        }
    }
    Some(())
}

fn surface(faces: &mut Faces, rings: &Json, value: Option<&Json>, marks: &[Json], vertices: &impl Fn(usize) -> Option<P3>) -> Option<()> {
    let rings = rings.as_array()?.iter().map(|ring| at(ring, vertices)).collect::<Option<Vec<_>>>()?;
    let ground =
        value.and_then(Json::as_u64).and_then(|m| marks.get(usize::try_from(m).ok()?)).and_then(|m| m.get("type")).and_then(Json::as_str)
            == Some("GroundSurface");
    faces.face(rings, ground);
    Some(())
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

/// A geometry's level of detail, as a number to compare and as written.
fn level(g: &RawGeometry) -> (f64, impl FnOnce() -> Option<String> + '_) {
    (lod_number(g.lod.as_ref()), move || lod_text(g.lod.as_ref()))
}

/// The system a file's metadata names, if it names one.
fn named_system(metadata: Option<&Json>) -> Option<&str> {
    metadata.and_then(|m| m.get("referenceSystem")).and_then(Json::as_str).map(str::trim).filter(|s| !s.is_empty())
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

/// Reads a CityJSON document; with `keep_named`, the system it names, when RustGIS knows it, goes
/// before `given`.
fn read_with(file_name: &str, bytes: &[u8], given: Option<Crs>, keep_named: bool) -> Result<Vec<(Dataset, ReadReport)>> {
    let max = crate::io::max_bytes();
    if bytes.len() as u64 > max {
        return Err(DataError::Format(crate::io::file_size_error(file_name, max)));
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
    // A compound URN is read by its horizontal part; its vertical part, said when its heights
    // aren't metres upward, gives the heights' unit.
    let system = named_system(doc.metadata.as_ref());
    let named = system.map(crate::gml::horizontal);
    let vertical = system.and_then(crate::gml::vertical);
    let given = given.filter(|_| !keep_named || named.and_then(known_system).is_none());
    let mut reach = Reach::default();
    vertices.iter().for_each(|v| reach.add(v));
    let crs = coordinate_system(file_name, given, named, vertical.is_some(), &reach, &mut notes)?;
    notes.extend(vertical.and_then(vertical_note));
    notes.extend(doc.appearance.and_then(appearance_note));
    let Some(objects) = doc.objects else {
        return Err(DataError::Format(format!("{file_name}: no CityObjects")));
    };

    let mut counts = Counts::default();
    let templates = Templates::of(doc.templates, &mut counts);
    // Placing an instance reads its template's vertices again: all of them together are as many as
    // the file could write out ([`POINT_BYTES`]).
    let mut placing = text.len() / POINT_BYTES;
    let mut budget = MAX_FILE_FACES;
    let mut built: Vec<Built> = Vec::with_capacity(objects.len());
    let mut rows: Vec<Row> = Vec::new();
    // Counted an object at a time, and stopped between objects when its run is cancelled (in the
    // desktop app's background read); parsing the document before is one step, with no check.
    crate::progress::begin(objects.len());
    for (id, raw) in &objects {
        crate::progress::step(1)?;
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
            Some(Json::Object(m)) => m.into_iter().collect(),
            None | Some(Json::Null) => Vec::new(),
            Some(_) => {
                counts.bad_attributes += 1;
                Vec::new()
            }
        };
        let parts = object_parts(&object.geometry, &vertices, templates.as_ref(), &mut placing, &mut counts);
        let made = object_rows(parts, at_row, &mut counts, &mut budget);
        if made.rows.is_empty() {
            // Its attributes stay: its children, in their own rows, are where it is drawn.
            counts.no_geometry += 1;
            rows.push(Row::without_geometry(at_row));
        }
        rows.extend(made.rows);
        built.push(Built {
            id: id.clone(),
            ty: type_name(&object.ty),
            parent_ids: object.parents,
            ground: None,
            z: made.z,
            solid: made.solid,
            attributes,
        });
    }
    finish(file_name, stem, rows, built, counts, notes, crs)
}

/// An object's surfaces, lines, points and heights, from its geometries. An instance is its
/// template placed by its matrix at its reference point, or, when that can't be done, its reference
/// point, which adds no height.
fn object_parts(
    geometries: &[RawGeometry],
    vertices: &[P3],
    templates: Option<&Templates>,
    placing: &mut usize,
    counts: &mut Counts,
) -> ObjectParts {
    let mut parts = ObjectParts::default();
    let vertex = |k: usize| vertices.get(k).copied();
    for g in geometries {
        if g.ty != "GeometryInstance" {
            if geometry(&mut parts, g, &vertex, counts).is_none() {
                counts.bad_geometries += 1;
            }
            continue;
        }
        let Some(reference) = at(&g.boundaries, &vertex).and_then(|p| p.first().copied()) else {
            counts.bad_geometries += 1;
            continue;
        };
        if !place(&mut parts, g, reference, templates, placing, counts) {
            let (rank, lod) = level(g);
            parts.points.add_points(rank, lod, vec![(reference, true)]);
        }
    }
    parts
}

/// Places an instance's template into the object's parts; false when it isn't placed: its
/// template is missing, can't be read or draws nothing, its matrix is missing (CityJSON 1.1 §3.4
/// requires one) or isn't 16 numbers with `0 0 0 1` last, or the file's placing is spent. The level of detail is its template's: an instance has none of its own
/// (CityJSON 1.1 §3.4).
fn place(
    parts: &mut ObjectParts,
    g: &RawGeometry,
    reference: P3,
    templates: Option<&Templates>,
    placing: &mut usize,
    counts: &mut Counts,
) -> bool {
    let Some(t) = templates else { return false };
    let shape = g.template.as_ref().and_then(Json::as_u64).and_then(|i| usize::try_from(i).ok()).and_then(|i| t.shapes.get(i));
    let Some(Some((shape, size, template_dropped))) = shape else { return false };
    let placement = g.matrix.as_ref().and_then(Json::as_array).and_then(|v| v.iter().map(Json::as_f64).collect::<Option<Vec<f64>>>());
    let Some(placement) = placement.and_then(|v| Placement::new(&v, reference)) else { return false };
    if *size > *placing {
        return false;
    }
    *placing -= size;
    let vertex = |k: usize| t.vertices.get(k).and_then(|v| placement.place(v));
    // The template was read once already, and its rings of no area said then: not again for each
    // instance. Placing may flatten others (a matrix that scales an axis to nothing): those are said
    // for each instance. A ring of no area keeps none once placed, so the placed rings left out are
    // the template's and the new ones. What fails is a point placed past what a number holds.
    let mut placed = Counts::default();
    let done = geometry(parts, shape, &vertex, &mut placed) == Some(true);
    counts.dropped_rings += placed.dropped_rings.saturating_sub(*template_dropped);
    done
}

/// One geometry into an object's parts, its points from `vertex`: whether it added something, or
/// none when it can't be read.
fn geometry(parts: &mut ObjectParts, g: &RawGeometry, vertex: &impl Fn(usize) -> Option<P3>, counts: &mut Counts) -> Option<bool> {
    let depth = match g.ty.as_str() {
        "MultiSurface" | "CompositeSurface" => 0,
        "Solid" => 1,
        "MultiSolid" | "CompositeSolid" => 2,
        "MultiPoint" => {
            let p = at(&g.boundaries, vertex)?;
            if p.is_empty() {
                return Some(false);
            }
            parts.heights.add(&p);
            let (rank, lod) = level(g);
            parts.points.add_points(rank, lod, p.into_iter().map(|p| (p, false)).collect());
            return Some(true);
        }
        "MultiLineString" => {
            let lines: Vec<Vec<P3>> = g.boundaries.as_array()?.iter().map(|line| at(line, vertex)).collect::<Option<_>>()?;
            let lines: Vec<Vec<P3>> = lines.into_iter().filter(|p| !p.is_empty()).collect();
            lines.iter().for_each(|p| parts.heights.add(p));
            if lines.is_empty() {
                return Some(false);
            }
            let (rank, lod) = level(g);
            parts.lines.add_lines(rank, lod, lines);
            return Some(true);
        }
        other => {
            counts.unknown.insert(other.chars().take(64).collect());
            return Some(false);
        }
    };
    let semantics = g.semantics.as_ref();
    let marks = semantics.and_then(|s| s.get("surfaces")).and_then(Json::as_array).map_or(&[][..], Vec::as_slice);
    let mut faces = Faces::default();
    walk(&mut faces, &g.boundaries, semantics.and_then(|s| s.get("values")), depth, marks, vertex)?;
    counts.dropped_rings += faces.dropped_rings;
    faces.rings().for_each(|p| parts.heights.add(p));
    parts.solid |= depth > 0;
    parts.surfaces.push(Surface { lod: lod_text(g.lod.as_ref()), rank: lod_number(g.lod.as_ref()), faces });
    Some(true)
}

#[cfg(test)]
mod tests;
