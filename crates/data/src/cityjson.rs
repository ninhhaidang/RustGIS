//! CityJSON 1.1 and 2.0 (the JSON encoding of the CityGML data model), read as 2D: each City Object
//! becomes a row of a layer per object type and geometry family. Points and lines stay as they are.
//! Surfaces and solids become footprints, with the object's heights in `z_min`, `z_max` and
//! `height`, as every city model is read (see [`crate::citymodel`]). Written from the
//! specifications: <https://www.cityjson.org/specs/>.
//!
//! The file is hostile: vertex indexes are checked, the nesting is fixed by the geometry types, and
//! the faces merged are bounded. Each object's geometry is parsed on its own and dropped once its rows
//! are built; its attributes and the root metadata are kept as JSON values until the layers are
//! written.

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

/// The points of a ring or line, `None` for an index that isn't one.
fn at(indexes: &Json, vertices: &[P3]) -> Option<Vec<P3>> {
    indexes.as_array()?.iter().map(|i| vertices.get(usize::try_from(i.as_u64()?).ok()?).copied()).collect()
}

/// `boundaries`, surfaces nested `depth` levels deep, with their semantic `values` alongside, into
/// `faces`.
fn walk(faces: &mut Faces, boundaries: &Json, values: Option<&Json>, depth: usize, marks: &[Json], vertices: &[P3]) -> Option<()> {
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

fn surface(faces: &mut Faces, rings: &Json, value: Option<&Json>, marks: &[Json], vertices: &[P3]) -> Option<()> {
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
    let named = named_system(doc.metadata.as_ref());
    let given = given.filter(|_| !keep_named || named.and_then(known_system).is_none());
    let crs = coordinate_system(file_name, given, named, &vertices, &mut notes)?;
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
            Some(Json::Object(m)) => m.into_iter().collect(),
            None | Some(Json::Null) => Vec::new(),
            Some(_) => {
                counts.bad_attributes += 1;
                Vec::new()
            }
        };
        let parts = object_parts(&object.geometry, &vertices, &mut counts);
        let made = object_rows(parts, at_row, to_lonlat.as_ref(), &mut counts, &mut budget);
        if made.rows.is_empty() {
            // Its attributes stay: its children, in their own rows, are where it is drawn.
            counts.no_geometry += 1;
            rows.push(Row {
                family: Family::Polygon,
                geometry: None,
                object: at_row,
                lod: None,
                z_dropped: false,
                unconverted: false,
                draws: false,
            });
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
    finish(file_name, stem, rows, built, counts, notes, crs)
}

/// An object's surfaces, lines, points and heights, from its geometries. An instance adds none of
/// its heights: they are in its template, which isn't read.
fn object_parts(geometries: &[RawGeometry], vertices: &[P3], counts: &mut Counts) -> ObjectParts {
    let mut parts = ObjectParts::default();
    for g in geometries {
        let depth = match g.ty.as_str() {
            "MultiSurface" | "CompositeSurface" => 0,
            "Solid" => 1,
            "MultiSolid" | "CompositeSolid" => 2,
            "MultiPoint" => {
                match at(&g.boundaries, vertices) {
                    Some(p) if p.is_empty() => {}
                    Some(p) => {
                        parts.heights.add(&p);
                        let (rank, lod) = level(g);
                        parts.points.add_points(rank, lod, p.into_iter().map(|p| (p, false)).collect());
                    }
                    None => counts.bad_geometries += 1,
                }
                continue;
            }
            "GeometryInstance" => {
                // Its template isn't placed: it is drawn at its reference point.
                match at(&g.boundaries, vertices).as_deref() {
                    Some([p, ..]) => {
                        let (rank, lod) = level(g);
                        parts.points.add_points(rank, lod, vec![(*p, true)]);
                    }
                    _ => counts.bad_geometries += 1,
                }
                continue;
            }
            "MultiLineString" => {
                let lines: Option<Vec<Vec<P3>>> = g.boundaries.as_array().and_then(|l| l.iter().map(|line| at(line, vertices)).collect());
                match lines {
                    Some(lines) => {
                        let lines: Vec<Vec<P3>> = lines.into_iter().filter(|p| !p.is_empty()).collect();
                        lines.iter().for_each(|p| parts.heights.add(p));
                        if !lines.is_empty() {
                            let (rank, lod) = level(g);
                            parts.lines.add_lines(rank, lod, lines);
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
        if walk(&mut faces, &g.boundaries, semantics.and_then(|s| s.get("values")), depth, marks, vertices).is_none() {
            counts.bad_geometries += 1;
            continue;
        }
        counts.dropped_rings += faces.dropped_rings;
        faces.rings().for_each(|p| parts.heights.add(p));
        parts.solid |= depth > 0;
        parts.surfaces.push(Surface { lod: lod_text(g.lod.as_ref()), rank: lod_number(g.lod.as_ref()), faces });
    }
    parts
}

#[cfg(test)]
mod tests;
