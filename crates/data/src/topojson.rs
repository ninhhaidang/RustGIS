//! TopoJSON: geometries sharing arcs, quantized and delta-encoded when the topology has a
//! transform. Each object of the topology becomes a layer (its geometries rebuilt from their arcs
//! as GeoJSON features, then read as GeoJSON).
//!
//! The file is hostile: arc indices are checked, nesting is fixed by the geometry types, and the
//! number of coordinates rebuilt is bounded.
//!
//! A position may hold more than x and y (TopoJSON specification §2.1.1, which leaves their meaning
//! open). The first further number (a height or measure) is carried into the rebuilt GeoJSON as is,
//! neither scaled nor delta-decoded (§2.1.2: the reference implementation copies it), so the
//! GeoJSON reading counts the features that had one and says it dropped them. (Positions take 24
//! bytes rather than 16 for it, within the `MAX_COORDS` bound.)
//!
//! A quantized arc with a position that can't be read is not read: every later position is a
//! delta from it, so leaving it out would move the rest of the line. Nor is a plain arc whose end
//! can't be read (its ends are the joints other arcs meet it at); a plain position inside an arc
//! is left out of it, and each layer drawing through it says how many. A geometry is read whole
//! or not at all, a collection with its members, and geometries left out are counted and said.

use serde_json::{Map, Value as Json, json};

use crate::{DataError, Dataset, ReadReport, Result};

/// The most coordinates rebuilt from arcs.
const MAX_COORDS: usize = 20_000_000;
/// The most objects (layers) read.
const MAX_OBJECTS: usize = 64;

/// Whether JSON text is a TopoJSON topology.
pub fn looks_like_topojson(text: &str) -> bool {
    // The first 512 bytes, cut at a character boundary.
    let end = (0..=text.len().min(512)).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    let head = text.get(..end).unwrap_or("");
    // `"type": "Topology"`, whatever the spacing (a property value "Topology" doesn't count).
    let squeezed: String = head.chars().filter(|c| !c.is_whitespace()).collect();
    squeezed.contains("\"type\":\"Topology\"")
}

/// A position: x, y and its first further number (a height or measure), or NaN for none (JSON
/// holds no NaN, so it can't be one read).
type Pos = [f64; 3];

/// A position as written (before any transform): x, y and the first number after them, or NaN.
/// `None` when it isn't an array starting with two numbers.
fn position(p: &Json) -> Option<Pos> {
    let a = p.as_array()?;
    let extra = a.iter().skip(2).find_map(Json::as_f64).unwrap_or(f64::NAN);
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?, extra])
}

/// "1 geometry" or "2 geometries": `n` and the word for one or for several.
fn counted(n: usize, one: &str, several: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {several}") }
}

/// A position as GeoJSON: `[x, y]`, or `[x, y, extra]` when it had a further number.
fn pos_json(p: &Pos) -> Json {
    let [x, y, e] = *p;
    if e.is_nan() { json!([x, y]) } else { json!([x, y, e]) }
}

/// Positions as a GeoJSON coordinate array.
fn line_json(l: &[Pos]) -> Json {
    Json::Array(l.iter().map(pos_json).collect())
}

/// Rings (or lines) as nested GeoJSON coordinate arrays.
fn rings_json(r: &[Vec<Pos>]) -> Json {
    Json::Array(r.iter().map(|l| line_json(l)).collect())
}

/// Why a geometry wasn't rebuilt.
#[derive(Clone, Copy)]
enum Lost {
    /// It, or an arc it uses, can't be read (or it is nested too deep).
    Unreadable,
    /// Rebuilding it would pass the limit of coordinates rebuilt from arcs.
    Cut,
}
use Lost::Unreadable;

/// Whether a geometry object holds nothing to lose: no type, a JSON null type or the text "null",
/// and no arcs, coordinates or geometries. The specification's types (§2) have no null type; such
/// an object is read as a feature without geometry, as GeoJSON's null geometry, without a notice.
/// One that has data but no type it lists can't be read, and is said. Anything else that isn't an
/// object (a number, an array) is not empty: it is read, and said when it can't be.
fn empty_on_purpose(g: &Json) -> bool {
    if !g.is_object() && !g.is_null() {
        return false;
    }
    let none = |k: &str| g.get(k).is_none_or(Json::is_null);
    let null_type = match g.get("type") {
        None | Some(Json::Null) => true,
        Some(t) => t.as_str() == Some("null"),
    };
    null_type && none("arcs") && none("coordinates") && none("geometries")
}

struct Topology {
    /// The arcs, decoded to coordinates; `None` for one that can't be read: a quantized arc with a
    /// position that can't be read (its later positions are deltas from it), or a plain arc whose
    /// first or last position can't be (its ends are the joints other arcs meet it at).
    arcs: Vec<Option<Vec<Pos>>>,
    /// By arc, the positions inside a plain arc that couldn't be read and were left out of it.
    arcs_lost: Vec<usize>,
    transform: Option<([f64; 2], [f64; 2])>,
    /// Coordinates still allowed to be rebuilt, across all objects. A geometry is checked whole
    /// and counted ([`Topology::need`]) before any arc is copied, so only geometries kept use it,
    /// and nothing is copied for one that can't be read or doesn't fit.
    budget: usize,
}

impl Topology {
    /// The arc an id names (`~i` is arc `i` reversed) and whether reversed; `None` when it isn't
    /// an arc that was read.
    fn arc(&self, id: &Json) -> Option<(&[Pos], bool)> {
        let i = id.as_i64()?;
        let (k, rev) = if i < 0 { (!i, true) } else { (i, false) };
        Some((self.arcs.get(usize::try_from(k).ok()?)?.as_deref()?, rev))
    }

    /// The coordinates [`Topology::line`] writes from a line's arcs, each joint once, every id
    /// checked.
    fn line_len(&self, ids: &Json) -> std::result::Result<usize, Lost> {
        let mut n = 0usize;
        for id in ids.as_array().ok_or(Unreadable)? {
            let (arc, _) = self.arc(id).ok_or(Unreadable)?;
            // After the first, an arc's first position is the joint already written.
            let joint = usize::from(n > 0 && !arc.is_empty());
            n = n.saturating_add(arc.len() - joint);
        }
        Ok(n)
    }

    /// The coordinates a list of lines (or rings) writes, every id checked.
    fn rings_len(&self, v: &Json) -> std::result::Result<usize, Lost> {
        v.as_array().ok_or(Unreadable)?.iter().try_fold(0usize, |n, r| Ok(n.saturating_add(self.line_len(r)?)))
    }

    /// Takes `n` coordinates from the limit, or `Cut` (taking none) when they don't fit.
    fn charge(&mut self, n: usize) -> std::result::Result<(), Lost> {
        self.budget = self.budget.checked_sub(n).ok_or(Lost::Cut)?;
        Ok(())
    }

    /// A point's position, quantized (not delta-encoded) with a transform. One past f64's range
    /// (a huge scale or translate) is unreadable, not an infinite coordinate.
    fn point(&self, p: &Json) -> Option<Pos> {
        let [x, y, e] = position(p)?;
        let pos = match self.transform {
            Some(([sx, sy], [tx, ty])) => [x * sx + tx, y * sy + ty, e],
            None => [x, y, e],
        };
        (pos[0].is_finite() && pos[1].is_finite()).then_some(pos)
    }

    /// A line from arc ids, shared ends written once. Its ids are checked by [`Self::line_len`]
    /// first; one that isn't an arc read is passed over.
    fn line(&self, ids: &Json) -> Vec<Pos> {
        let mut out: Vec<Pos> = Vec::new();
        for (arc, rev) in ids.as_array().into_iter().flatten().filter_map(|id| self.arc(id)) {
            // The joint (this arc's first position as read), written once, keeps a further number
            // either arc gives it.
            let joint = if out.is_empty() {
                None
            } else if rev {
                arc.last()
            } else {
                arc.first()
            };
            if let Some(last) = out.last_mut()
                && let Some(first) = joint
                && last[2].is_nan()
            {
                last[2] = first[2];
            }
            let skip = usize::from(joint.is_some());
            if rev {
                out.extend(arc.iter().rev().skip(skip).copied());
            } else {
                out.extend(arc.iter().skip(skip).copied());
            }
        }
        out
    }

    /// Lines (or rings) from lists of arc ids, checked by [`Self::rings_len`] first.
    fn rings(&self, v: &Json) -> Vec<Vec<Pos>> {
        v.as_array().into_iter().flatten().map(|r| self.line(r)).collect()
    }

    /// Checks a geometry whole, members of a collection included, and counts the coordinates
    /// rebuilding it takes from arcs, before anything is rebuilt. It is read whole or not at all
    /// (GeoJSON reads a collection so): one member that can't be read, or nesting past four
    /// collections deep, leaves out the geometry, said once. Once empty members are dropped, a
    /// collection must have exactly one left, since GeoJSON holds one; any other count is
    /// unreadable, and takes no coordinates from the limit.
    fn need(&self, g: &Json, depth: usize) -> std::result::Result<usize, Lost> {
        let ty = g.get("type").and_then(Json::as_str).ok_or(Unreadable)?;
        let arcs = || g.get("arcs").ok_or(Unreadable);
        let coords = g.get("coordinates");
        match ty {
            "Point" => coords.and_then(|c| self.point(c)).map(|_| 0).ok_or(Unreadable),
            "MultiPoint" => {
                let all = coords.and_then(Json::as_array).ok_or(Unreadable)?.iter().all(|p| self.point(p).is_some());
                if all { Ok(0) } else { Err(Unreadable) }
            }
            "LineString" => self.line_len(arcs()?),
            "MultiLineString" | "Polygon" => self.rings_len(arcs()?),
            "MultiPolygon" => {
                arcs()?.as_array().ok_or(Unreadable)?.iter().try_fold(0usize, |n, p| Ok(n.saturating_add(self.rings_len(p)?)))
            }
            "GeometryCollection" if depth < 4 => {
                let members = g.get("geometries").and_then(Json::as_array).ok_or(Unreadable)?;
                let kept: Vec<&Json> = members.iter().filter(|m| !empty_on_purpose(m)).collect();
                let [only] = kept.as_slice() else { return Err(Unreadable) };
                self.need(only, depth + 1)
            }
            _ => Err(Unreadable),
        }
    }

    /// A geometry [`Topology::need`] passed, rebuilt as GeoJSON. Its nesting is bounded there:
    /// `need` refuses a collection more than four deep, so this recursion is too.
    fn build(&self, g: &Json) -> Json {
        let ty = g.get("type").and_then(Json::as_str).unwrap_or("");
        let arcs = g.get("arcs").unwrap_or(&Json::Null);
        let coords = g.get("coordinates");
        match ty {
            "Point" => json!({"type": ty, "coordinates": coords.and_then(|c| self.point(c)).map_or(Json::Null, |p| pos_json(&p))}),
            "MultiPoint" => {
                let pts: Vec<Pos> = coords.and_then(Json::as_array).into_iter().flatten().filter_map(|p| self.point(p)).collect();
                json!({"type": ty, "coordinates": line_json(&pts)})
            }
            "LineString" => json!({"type": ty, "coordinates": line_json(&self.line(arcs))}),
            "MultiLineString" | "Polygon" => json!({"type": ty, "coordinates": rings_json(&self.rings(arcs))}),
            "MultiPolygon" => {
                let polys: Vec<Json> = arcs.as_array().into_iter().flatten().map(|p| rings_json(&self.rings(p))).collect();
                json!({"type": ty, "coordinates": polys})
            }
            _ => {
                let members = g.get("geometries").and_then(Json::as_array).into_iter().flatten();
                let gs: Vec<Json> = members.filter(|m| !empty_on_purpose(m)).map(|m| self.build(m)).collect();
                json!({"type": "GeometryCollection", "geometries": gs})
            }
        }
    }

    /// A TopoJSON geometry as GeoJSON, or why it can't be: checked and counted against the limit
    /// whole, then rebuilt.
    fn geometry(&mut self, g: &Json) -> std::result::Result<Json, Lost> {
        let n = self.need(g, 0)?;
        self.charge(n)?;
        Ok(self.build(g))
    }
}

/// The arcs a geometry draws through (members of a collection too, as deep as [`Topology::need`]
/// reads them), added to `used`.
fn arcs_used(g: &Json, depth: usize, used: &mut std::collections::BTreeSet<usize>) {
    /// Arc ids in nested lists, at most three lists deep (a multi-polygon's).
    fn ids(v: &Json, depth: usize, used: &mut std::collections::BTreeSet<usize>) {
        match v {
            Json::Array(items) if depth < 3 => items.iter().for_each(|i| ids(i, depth + 1, used)),
            Json::Number(_) => {
                if let Some(i) = v.as_i64() {
                    let k = if i < 0 { !i } else { i };
                    used.extend(usize::try_from(k).ok());
                }
            }
            _ => {}
        }
    }
    if let Some(a) = g.get("arcs") {
        ids(a, 0, used);
    }
    if depth < 4 {
        for m in g.get("geometries").and_then(Json::as_array).into_iter().flatten() {
            arcs_used(m, depth + 1, used);
        }
    }
}

/// A transform's scale and translate, for x and y; `None` when either can't be read.
fn read_transform(t: &Json) -> Option<([f64; 2], [f64; 2])> {
    let s = t.get("scale")?.as_array()?;
    let tr = t.get("translate")?.as_array()?;
    Some(([s.first()?.as_f64()?, s.get(1)?.as_f64()?], [tr.first()?.as_f64()?, tr.get(1)?.as_f64()?]))
}

/// Reads a TopoJSON topology into a layer per object.
pub fn read(file_name: &str, text: &str) -> Result<Vec<(Dataset, ReadReport)>> {
    read_with(file_name, text, MAX_COORDS)
}

/// [`read`], rebuilding at most `budget` coordinates from arcs.
fn read_with(file_name: &str, text: &str, budget: usize) -> Result<Vec<(Dataset, ReadReport)>> {
    let stem = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("topology");
    let stem = stem.strip_suffix(".topo").unwrap_or(stem);
    let v: Json = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    if v.get("type").and_then(Json::as_str) != Some("Topology") {
        return Err(DataError::Format(format!("{file_name}: not a TopoJSON topology")));
    }
    // A transform that is there but can't be read would leave every quantized position wrong, so
    // the file is refused rather than read as if unquantized.
    let transform = match v.get("transform") {
        None | Some(Json::Null) => None,
        Some(t) => Some(read_transform(t).ok_or_else(|| DataError::Format(format!("{file_name}: the transform can't be read")))?),
    };
    // Arcs: x and y delta-encoded integers with a transform, plain coordinates without; a further
    // number is copied as is. A plain position that can't be read inside an arc is left out of it
    // and counted; one at its end (a joint), or a quantized one (every later position is a delta
    // from it), makes the arc unreadable.
    let mut total = 0usize;
    let (mut arcs, mut arcs_lost) = (Vec::new(), Vec::new());
    for a in v.get("arcs").and_then(Json::as_array).map(Vec::as_slice).unwrap_or_default() {
        // An arc that isn't an array of positions can't be read either.
        let Some(positions) = a.as_array() else {
            arcs.push(None);
            arcs_lost.push(0);
            continue;
        };
        total += positions.len();
        if total > MAX_COORDS {
            return Err(DataError::Format(format!("{file_name}: too many arc coordinates")));
        }
        let pts: Option<Vec<Pos>> = match transform {
            Some(([sx, sy], [tx, ty])) => {
                let (mut x, mut y) = (0.0f64, 0.0f64);
                positions
                    .iter()
                    .map(|p| {
                        let [px, py, e] = position(p)?;
                        x += px;
                        y += py;
                        // Past f64's range, the position is unreadable: the arc is, as its later
                        // positions are deltas from it.
                        let pos = [x * sx + tx, y * sy + ty, e];
                        (pos[0].is_finite() && pos[1].is_finite()).then_some(pos)
                    })
                    .collect()
            }
            None => {
                let read: Vec<Option<Pos>> = positions.iter().map(position).collect();
                let ends = read.first().is_none_or(Option::is_some) && read.last().is_none_or(Option::is_some);
                ends.then(|| read.into_iter().flatten().collect::<Vec<Pos>>())
            }
        };
        arcs_lost.push(match (&pts, transform) {
            (Some(p), None) => positions.len() - p.len(),
            _ => 0,
        });
        arcs.push(pts);
    }
    let mut topo = Topology { arcs, arcs_lost, transform, budget };
    let objects = v.get("objects").and_then(Json::as_object).ok_or_else(|| DataError::Format(format!("{file_name}: no objects")))?;
    let mut out = Vec::new();
    for (name, obj) in objects.iter().take(MAX_OBJECTS) {
        // A collection without a list of geometries holds nothing that can be read.
        let mut no_list = false;
        let members: Vec<&Json> = match obj.get("type").and_then(Json::as_str) {
            Some("GeometryCollection") => match obj.get("geometries").and_then(Json::as_array) {
                Some(g) => g.iter().collect(),
                None => {
                    no_list = true;
                    Vec::new()
                }
            },
            _ => vec![obj],
        };
        // Geometries that couldn't be read, those left out at the coordinate limit, and the arcs
        // drawn through by those rebuilt.
        let (mut bad, mut cut) = (0usize, 0usize);
        let mut used = std::collections::BTreeSet::new();
        let features: Vec<Json> = members
            .into_iter()
            .map(|g| {
                let geometry = if empty_on_purpose(g) {
                    Json::Null
                } else {
                    match topo.geometry(g) {
                        Ok(geometry) => {
                            arcs_used(g, 0, &mut used);
                            geometry
                        }
                        Err(Unreadable) => {
                            bad += 1;
                            Json::Null
                        }
                        Err(Lost::Cut) => {
                            cut += 1;
                            Json::Null
                        }
                    }
                };
                let mut props = g.get("properties").and_then(Json::as_object).cloned().unwrap_or_else(Map::new);
                if let Some(id) = g.get("id")
                    && !props.contains_key("id")
                {
                    props.insert("id".into(), id.clone());
                }
                json!({"type": "Feature", "properties": props, "geometry": geometry})
            })
            .collect();
        let fc = json!({"type": "FeatureCollection", "features": features});
        let title = if objects.len() == 1 { stem.to_string() } else { format!("{stem} {name}") };
        let (ds, mut rep) = crate::io::read_geojson(&fc.to_string(), &title)?;
        // Geometries left out are said with why; their features are kept, with their attributes,
        // so they aren't counted as skipped.
        if bad > 0 {
            rep.messages.push(format!(
                "{} couldn't be read (unreadable coordinates or arcs, a member of a collection that can't be read, or a kind of geometry not read): kept as features without geometry",
                counted(bad, "geometry", "geometries")
            ));
        }
        if cut > 0 {
            rep.messages.push(format!(
                "{} left out at the limit of {budget} coordinates rebuilt from the file's arcs: kept as features without geometry",
                counted(cut, "geometry", "geometries")
            ));
        }
        let lost: usize = used.iter().filter_map(|i| topo.arcs_lost.get(*i)).sum();
        if lost > 0 {
            rep.messages.push(format!(
                "{} in its arcs couldn't be read: left out of the lines and rings drawn through them",
                counted(lost, "position", "positions")
            ));
        }
        if no_list {
            rep.messages.push(format!("object {name} is a geometry collection without a list of geometries: nothing in it was read"));
        }
        out.push((ds, rep));
    }
    if objects.len() > MAX_OBJECTS
        && let Some((_, rep)) = out.first_mut()
    {
        rep.messages.push(format!(
            "{} left out past the limit of {MAX_OBJECTS} objects: only the first {MAX_OBJECTS} are read",
            counted(objects.len() - MAX_OBJECTS, "object", "objects")
        ));
    }
    if out.is_empty() {
        return Err(DataError::Format(format!("{file_name}: no objects")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Geometry, GeometryType};
    use rustgis_geom::Coord;

    // The TopoJSON specification's example: a point, a line and a polygon, quantized.
    const TOPO: &str = r#"{"type":"Topology","transform":{"scale":[0.0005000500050005,0.00010001000100010001],"translate":[100,0]},
"objects":{"example":{"type":"GeometryCollection","geometries":[
{"type":"Point","properties":{"prop0":"value0"},"coordinates":[4000,5000]},
{"type":"LineString","properties":{"prop0":"value0","prop1":0},"arcs":[0]},
{"type":"Polygon","properties":{"prop0":"value0","prop1":{"this":"that"}},"arcs":[[-2]]}]}},
"arcs":[[[4000,0],[1999,9999],[2000,-9999],[2000,9999]],[[0,0],[0,9999],[2000,0],[0,-9999],[-2000,0]]]}"#;

    #[test]
    fn reads_the_specification_example() {
        assert!(looks_like_topojson(TOPO));
        assert!(!looks_like_topojson(r#"{"type": "Feature", "properties": {"kind": "Topology"}}"#));
        // A multi-byte character across byte 512.
        let padded = format!("{{\"type\": \"Topology\", \"x\": \"{}é\"}}", "a".repeat(480));
        assert!(looks_like_topojson(&padded));
        let layers = read("example.topojson", TOPO).unwrap();
        assert_eq!(layers.len(), 1);
        let ds = &layers[0].0;
        assert_eq!(ds.name, "example");
        assert_eq!(ds.features.len(), 3);
        let Some(Geometry::Point(p)) = &ds.features[0].geometry else { panic!() };
        assert!((p.x - 102.0).abs() < 1e-3 && (p.y - 0.5).abs() < 1e-3, "{p:?}");
        let Some(Geometry::Line(l)) = &ds.features[1].geometry else { panic!() };
        assert_eq!(l[0].len(), 4);
        assert!((l[0][3].x - 105.0).abs() < 1e-3);
        // The polygon is arc 1 reversed: still a closed ring of 5.
        let Some(Geometry::Polygon(poly)) = &ds.features[2].geometry else { panic!() };
        assert_eq!(poly[0][0].len(), 5);
        assert_eq!(poly[0][0].first(), poly[0][0].last());
        assert!(poly[0][0].iter().all(|c: &Coord| (100.0 - 1e-3..=101.0 + 1e-3).contains(&c.x)), "{:?}", poly[0][0]);
        assert_ne!(ds.geometry_type, GeometryType::None);
    }

    #[test]
    fn json_topologies_read_every_object() {
        let two = r#"{"type":"Topology","objects":{"a":{"type":"Point","coordinates":[1,2]},"b":{"type":"Point","coordinates":[3,4]}},"arcs":[]}"#;
        let layers = crate::io::read_layers("us.json", two.as_bytes()).unwrap();
        assert_eq!(layers.len(), 2);
        // The first alone says what else the file holds.
        let (ds, rep) = crate::io::read_bytes("us.json", two.as_bytes()).unwrap();
        assert_eq!(ds.features.len(), 1);
        assert!(rep.messages.iter().any(|m| m.contains("also holds")), "{:?}", rep.messages);
    }

    /// What reading `text` says of dropped heights, and whether the layer is marked as having lost
    /// them.
    fn zm_said(text: &str) -> (bool, Vec<String>) {
        let (ds, rep) = read("z.topojson", text).unwrap().remove(0);
        (ds.zm_dropped, rep.messages)
    }

    #[test]
    fn heights_dropped_are_said() {
        // The specification's example says nothing: it has no heights.
        let (lost, said) = zm_said(TOPO);
        assert!(!lost && said.is_empty(), "{said:?}");

        // A quantized arc with a height on one position; the point and the line use it, the
        // polygon doesn't. The height is copied, not delta-decoded or scaled.
        let quantized = r#"{"type":"Topology","transform":{"scale":[1,1],"translate":[0,0]},
"objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"Point","coordinates":[1,2,30]},
{"type":"LineString","arcs":[0]},
{"type":"Polygon","arcs":[[1]]}]}},
"arcs":[[[0,0],[1,1,12.5]],[[0,0],[0,1],[1,0],[-1,-1]]]}"#;
        let (lost, said) = zm_said(quantized);
        assert!(lost && said == [crate::zm_note(Some(2))], "{said:?}");

        // Without a transform: a height on a plain arc, read through a reversed index.
        let plain = r#"{"type":"Topology","objects":{"o":{"type":"LineString","arcs":[-1]}},
"arcs":[[[10,10,100],[11,11,101]]]}"#;
        let (lost, said) = zm_said(plain);
        assert!(lost && said == [crate::zm_note(Some(1))], "{said:?}");
        // The x and y read are unchanged by the height.
        let (ds, _) = read("z.topojson", plain).unwrap().remove(0);
        let Some(Geometry::Line(l)) = &ds.features[0].geometry else { panic!() };
        assert_eq!((l[0][0].x, l[0][0].y, l[0][1].x), (11.0, 11.0, 10.0));

        // A measure alone (the third value null, a fourth a number) is dropped too.
        let measure = r#"{"type":"Topology","objects":{"o":{"type":"MultiPoint","coordinates":[[1,2,null,7],[3,4]]}},"arcs":[]}"#;
        let (lost, said) = zm_said(measure);
        assert!(lost && said == [crate::zm_note(Some(1))], "{said:?}");

        // A null third value is no height.
        let null = r#"{"type":"Topology","objects":{"o":{"type":"Point","coordinates":[1,2,null]}},"arcs":[]}"#;
        let (lost, said) = zm_said(null);
        assert!(!lost && said.is_empty(), "{said:?}");
    }

    #[test]
    fn a_height_on_a_shared_joint_is_kept() {
        // Only the second arc's first position, the joint written once, has a height.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"LineString","arcs":[0,1]}},
"arcs":[[[0,0],[1,1]],[[1,1,5],[2,2]]]}"#;
        let (lost, said) = zm_said(text);
        assert!(lost && said == [crate::zm_note(Some(1))], "{said:?}");
    }

    #[test]
    fn an_arc_with_an_unreadable_position_is_not_read() {
        // Quantized: leaving out the bad delta would move (1,1) to where (x+1, 6) should be.
        let quantized = r#"{"type":"Topology","transform":{"scale":[1,1],"translate":[0,0]},
"objects":{"o":{"type":"GeometryCollection","geometries":[{"type":"LineString","arcs":[0]},{"type":"LineString","arcs":[1]}]}},
"arcs":[[[0,0],["x",5],[1,1]],[[0,0],[1,1]]]}"#;
        let (ds, rep) = read("q.topojson", quantized).unwrap().remove(0);
        assert_eq!(rep.skipped, 0);
        assert!(rep.messages.iter().any(|m| m.starts_with("1 geometry couldn't be read")), "{:?}", rep.messages);
        assert!(ds.features[0].geometry.is_none());
        let Some(Geometry::Line(l)) = &ds.features[1].geometry else { panic!() };
        assert_eq!((l[0][1].x, l[0][1].y), (1.0, 1.0));
        // An arc that isn't an array: the same.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"LineString","arcs":[0]}},"arcs":[5]}"#;
        let (_, rep) = read("p.topojson", text).unwrap().remove(0);
        assert_eq!(rep.skipped, 0);
    }

    #[test]
    fn a_plain_position_not_read_is_left_out_of_its_arc_and_said() {
        // Plain coordinates aren't deltas: the arc keeps its other positions, and the file says
        // how many it left out.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"LineString","arcs":[0]}},"arcs":[[[0,0],[null,1],[2,2]]]}"#;
        let (ds, rep) = read("p.topojson", text).unwrap().remove(0);
        assert_eq!(rep.skipped, 0);
        let Some(Geometry::Line(l)) = &ds.features[0].geometry else { panic!("{:?}", rep.messages) };
        assert_eq!((l[0].len(), l[0][1].x), (2, 2.0));
        assert!(rep.messages.iter().any(|m| m.starts_with("1 position in its arcs couldn't be read")), "{:?}", rep.messages);
    }

    #[test]
    fn geometries_not_read_are_said_whatever_the_cause() {
        // A point has no arcs: the notice doesn't blame them. Two: the plural.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"Point","coordinates":["x",1]},{"type":"Sphere"},{"type":"Point","coordinates":[1,2]}]}},"arcs":[]}"#;
        let (ds, rep) = read("g.topojson", text).unwrap().remove(0);
        assert_eq!((ds.features.len(), rep.skipped), (3, 0));
        assert!(
            rep.messages.iter().any(|m| m.starts_with("2 geometries couldn't be read") && m.ends_with("kept as features without geometry")),
            "{:?}",
            rep.messages
        );
    }

    #[test]
    fn the_coordinate_limit_is_said_as_such() {
        // Three lines of 2 coordinates each, with room for 4: the third is cut, not unreadable.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"LineString","arcs":[0]},{"type":"LineString","arcs":[0]},{"type":"LineString","arcs":[0]}]}},
"arcs":[[[0,0],[1,1]]]}"#;
        let (ds, rep) = read_with("l.topojson", text, 4).unwrap().remove(0);
        assert_eq!((ds.features.len(), rep.skipped), (3, 0));
        // The limit said is the one used.
        assert!(rep.messages.iter().any(|m| m.starts_with("1 geometry left out at the limit of 4 coordinates")), "{:?}", rep.messages);
        assert!(rep.messages.iter().all(|m| !m.contains("couldn't be read")), "{:?}", rep.messages);

        // A nested collection at the limit is said as such too (one member, which GeoJSON holds).
        let nested = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"GeometryCollection","geometries":[{"type":"LineString","arcs":[0]}]}]}},
"arcs":[[[0,0],[1,1]]]}"#;
        let (_, rep) = read_with("n.topojson", nested, 1).unwrap().remove(0);
        assert!(rep.messages.iter().any(|m| m.starts_with("1 geometry left out at the limit of 1")), "{:?}", rep.messages);
    }

    #[test]
    fn a_collection_the_reader_cannot_keep_takes_no_coordinates() {
        // Two lines in a collection (4 coordinates) can't be kept: GeoJSON holds one member. It is
        // said unreadable and takes nothing from the limit of 2, so the line after it is read.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"GeometryCollection","geometries":[{"type":"LineString","arcs":[0]},{"type":"LineString","arcs":[0]}]},
{"type":"LineString","arcs":[0]}]}},"arcs":[[[0,0],[1,1]]]}"#;
        let (ds, rep) = read_with("c.topojson", text, 2).unwrap().remove(0);
        assert!(ds.features[0].geometry.is_none(), "{:?}", rep.messages);
        assert!(ds.features[1].geometry.is_some(), "{:?}", rep.messages);
        assert!(rep.messages.iter().any(|m| m.starts_with("1 geometry couldn't be read")), "{:?}", rep.messages);
        assert!(rep.messages.iter().all(|m| !m.contains("limit")), "{:?}", rep.messages);
    }

    #[test]
    fn a_member_that_is_not_an_object_is_said() {
        // A number and an array are not empty geometries: each is said unreadable, not dropped.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[5,[1,2],{"type":"Point","coordinates":[1,2]}]}},"arcs":[]}"#;
        let (ds, rep) = read("m.topojson", text).unwrap().remove(0);
        assert_eq!((ds.features.len(), rep.skipped), (3, 0));
        assert!(rep.messages.iter().any(|m| m.starts_with("2 geometries couldn't be read")), "{:?}", rep.messages);
        let top = r#"{"type":"Topology","objects":{"o":5},"arcs":[]}"#;
        let (ds, rep) = read("m.topojson", top).unwrap().remove(0);
        assert_eq!(ds.features.len(), 1);
        assert!(rep.messages.iter().any(|m| m.starts_with("1 geometry couldn't be read")), "{:?}", rep.messages);
    }

    #[test]
    fn a_transform_that_cant_be_read_refuses_the_file() {
        let text = r#"{"type":"Topology","transform":{"scale":"x","translate":[0,0]},"objects":{"o":{"type":"Point","coordinates":[1,2]}},"arcs":[]}"#;
        assert!(read("t.topojson", text).is_err());
        // Absent, or null, there is no transform to read.
        let none = r#"{"type":"Topology","transform":null,"objects":{"o":{"type":"Point","coordinates":[1,2]}},"arcs":[]}"#;
        assert!(read("t.topojson", none).is_ok());
    }

    #[test]
    fn objects_past_the_limit_are_said() {
        let objects: Vec<String> = (0..=MAX_OBJECTS).map(|i| format!("\"o{i}\":{{\"type\":\"Point\",\"coordinates\":[1,2]}}")).collect();
        let text = format!(r#"{{"type":"Topology","objects":{{{}}},"arcs":[]}}"#, objects.join(","));
        let layers = read("many.topojson", &text).unwrap();
        assert_eq!(layers.len(), MAX_OBJECTS);
        assert!(layers[0].1.messages.iter().any(|m| m.starts_with("1 object left out past the limit of 64")), "{:?}", layers[0].1.messages);
    }

    #[test]
    fn geometries_failing_on_an_arc_copy_nothing() {
        // Each multi-line names arc 0 (2 coordinates), then an arc the file doesn't hold. Arcs
        // are checked before any is copied, so these copy nothing and take nothing from the limit
        // (2): however many there are, the line after them is still read, and nothing is blamed
        // on the limit.
        let bad = r#"{"type":"MultiLineString","arcs":[[0],[9]]}"#;
        let text = format!(
            r#"{{"type":"Topology","objects":{{"o":{{"type":"GeometryCollection","geometries":[{bad},{bad},{bad},{{"type":"LineString","arcs":[0]}}]}}}},"arcs":[[[0,0],[1,1]]]}}"#
        );
        let (ds, rep) = read_with("w.topojson", &text, 2).unwrap().remove(0);
        assert_eq!(rep.skipped, 0);
        assert!(ds.features[3].geometry.is_some(), "{:?}", rep.messages);
        assert!(rep.messages.iter().any(|m| m.starts_with("3 geometries couldn't be read")), "{:?}", rep.messages);
        assert!(rep.messages.iter().all(|m| !m.contains("limit")), "{:?}", rep.messages);
    }

    #[test]
    fn a_geometry_not_read_gives_back_its_coordinates() {
        // The first line uses arc 0 (2 coordinates), then an arc the file doesn't hold: it isn't
        // read, and its 2 coordinates don't count against the limit of 2, so the second is read.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"MultiLineString","arcs":[[0],[9]]},{"type":"LineString","arcs":[0]}]}},"arcs":[[[0,0],[1,1]]]}"#;
        let (ds, rep) = read_with("b.topojson", text, 2).unwrap().remove(0);
        assert!(ds.features[1].geometry.is_some(), "{:?}", rep.messages);
        assert!(rep.messages.iter().all(|m| !m.contains("limit")), "{:?}", rep.messages);
    }

    #[test]
    fn a_collection_without_geometries_and_members_without_a_type_are_said() {
        let no_list = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":5}},"arcs":[]}"#;
        let (_, rep) = read("n.topojson", no_list).unwrap().remove(0);
        assert_eq!(rep.skipped, 0);
        assert!(rep.messages.iter().any(|m| m.contains("without a list of geometries")), "{:?}", rep.messages);

        // Arcs without a type, with a number for one, or with a null type can't be read; a null
        // type, the text "null" or no type, with nothing else, holds nothing to lose.
        let types = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"arcs":[0]},{"type":7,"arcs":[0]},{"type":null,"arcs":[0]},{"type":"null"},{"type":null},{"properties":{"a":1}}]}},"arcs":[[[0,0],[1,1]]]}"#;
        let (ds, rep) = read("t.topojson", types).unwrap().remove(0);
        assert_eq!((ds.features.len(), rep.skipped), (6, 0));
        assert!(rep.messages.iter().any(|m| m.starts_with("3 geometries couldn't be read")), "{:?}", rep.messages);

        // Inside a collection, a null member is left out without a notice.
        let nested = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"GeometryCollection","geometries":[{"type":null},{"type":"Point","coordinates":[1,2]}]}]}},"arcs":[]}"#;
        let (_, rep) = read("n.topojson", nested).unwrap().remove(0);
        assert!(rep.messages.is_empty(), "{:?}", rep.messages);
    }

    #[test]
    fn collection_members_left_out_are_said() {
        let text = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]},{"type":"LineString","arcs":[9]}]}]}},"arcs":[]}"#;
        let (_, rep) = read("c.topojson", text).unwrap().remove(0);
        assert!(rep.messages.iter().any(|m| m.starts_with("1 geometry couldn't be read")), "{:?}", rep.messages);
        // Counted for each object: a second object without losses says nothing of them.
        let two = r#"{"type":"Topology","objects":{
"a":{"type":"GeometryCollection","geometries":[{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]},{"type":"LineString","arcs":[9]}]}]},
"b":{"type":"Point","coordinates":[3,4]}},"arcs":[]}"#;
        let layers = read("c.topojson", two).unwrap();
        assert!(layers[1].1.messages.iter().all(|m| !m.contains("members")), "{:?}", layers[1].1.messages);
    }

    #[test]
    fn a_geometry_not_read_isnt_counted_as_dropping_heights() {
        // The first geometry's arc doesn't exist; the second has a height.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"LineString","arcs":[5]},
{"type":"Point","coordinates":[1,2,3]}]}},"arcs":[[[0,0,9],[1,1,9]]]}"#;
        let (ds, rep) = read("z.topojson", text).unwrap().remove(0);
        assert_eq!(rep.skipped, 0);
        assert!(ds.zm_dropped && rep.messages.contains(&crate::zm_note(Some(1))), "{:?}", rep.messages);
    }

    #[test]
    fn junk_is_refused() {
        assert!(read("x.topojson", "{}").is_err());
        assert!(read("x.topojson", "not json").is_err());
        let bad_arc = r#"{"type":"Topology","objects":{"a":{"type":"LineString","arcs":[7]}},"arcs":[]}"#;
        let (_, rep) = read("x.topojson", bad_arc).unwrap().remove(0);
        assert_eq!(rep.skipped, 0);
        for cut in (0..TOPO.len()).step_by(11) {
            let _ = read("x.topojson", TOPO.get(..cut).unwrap_or(""));
        }
    }

    #[test]
    fn a_plain_arc_whose_end_cant_be_read_isnt_joined_across_the_gap() {
        // Arc 1's first position, the joint with arc 0, can't be read: leaving it out would drop
        // (5,5) as the joint and draw a segment from (1,1) to (6,6) that isn't in the file.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"LineString","arcs":[0,1]}},
"arcs":[[[0,0],[1,1]],[[null,1],[5,5],[6,6]]]}"#;
        let (ds, rep) = read("e.topojson", text).unwrap().remove(0);
        assert!(ds.features[0].geometry.is_none());
        assert!(rep.messages.iter().any(|m| m.starts_with("1 geometry couldn't be read")), "{:?}", rep.messages);
    }

    #[test]
    fn a_position_past_the_range_of_a_number_isnt_read() {
        // A scale of 1e308 puts 10 * scale past f64::MAX: the point and the line's arc are
        // unreadable, and neither is kept as an infinite coordinate.
        let text = r#"{"type":"Topology","transform":{"scale":[1e308,1e308],"translate":[0,0]},
"objects":{"o":{"type":"GeometryCollection","geometries":[
{"type":"Point","coordinates":[10,10]},
{"type":"LineString","arcs":[0]}]}},
"arcs":[[[10,10],[10,10]]]}"#;
        let (ds, rep) = read("i.topojson", text).unwrap().remove(0);
        assert!(ds.features[0].geometry.is_none() && ds.features[1].geometry.is_none(), "{:?}", rep.messages);
        assert!(rep.messages.iter().any(|m| m.starts_with("2 geometries couldn't be read")), "{:?}", rep.messages);
    }

    #[test]
    fn positions_left_out_are_said_by_the_layer_drawing_through_them() {
        // Arc 1 has an unreadable position inside it; only object b draws through it, and arc 2
        // (also with one) is drawn by no one.
        let text = r#"{"type":"Topology","objects":{
"a":{"type":"LineString","arcs":[0]},"b":{"type":"LineString","arcs":[1]}},
"arcs":[[[0,0],[1,1]],[[0,0],[null,1],[2,2]],[[0,0],["x",1],[3,3]]]}"#;
        let layers = read("p.topojson", text).unwrap();
        let said = |i: usize| layers[i].1.messages.iter().any(|m| m.contains("in its arcs couldn't be read"));
        assert!(!said(0) && said(1), "{:?} / {:?}", layers[0].1.messages, layers[1].1.messages);
        assert!(layers[1].1.messages.iter().any(|m| m.starts_with("1 position")), "{:?}", layers[1].1.messages);
    }

    #[test]
    fn a_joint_is_counted_once_against_the_limit() {
        // Two arcs of 2 positions joined: 3 coordinates written, so a limit of 3 holds it.
        let text = r#"{"type":"Topology","objects":{"o":{"type":"LineString","arcs":[0,1]}},
"arcs":[[[0,0],[1,1]],[[1,1],[2,2]]]}"#;
        let (ds, rep) = read_with("j.topojson", text, 3).unwrap().remove(0);
        let Some(Geometry::Line(l)) = &ds.features[0].geometry else { panic!("{:?}", rep.messages) };
        assert_eq!(l[0].len(), 3);
        assert!(rep.messages.is_empty(), "{:?}", rep.messages);
    }
}
