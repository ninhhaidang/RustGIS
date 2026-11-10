//! CityGML 1.0 and 2.0 (OGC 08-007r1 and 12-019), read as 2D like every city model: each city
//! object becomes a row of a layer per object type and geometry family, its surfaces and solids a
//! footprint with its heights in `z_min`, `z_max` and `height` (see [`crate::citymodel`]). Written
//! from the standards and their XML schemas.
//!
//! The objects are the features a city model's members (`cityObjectMember`) hold, and the features
//! nested in their properties that have a `gml:id` or a geometry (a `BuildingPart`, a `Room`), each
//! with its parent's id. Thematic surfaces (`WallSurface`, `GroundSurface`, …) and their openings
//! aren't objects: their faces are their object's, and a `GroundSurface`'s are the ground a
//! footprint can come from. A geometry's level of detail is the one its property names (`lod2Solid`
//! is 2); a geometry in another property (a group's, a relief's extent) has none. Coordinates are
//! converted from the system the `srsName`s name (on geometries and envelopes, often the only place
//! a file names it), by its horizontal part; the URN and URL forms of a geographic system are
//! latitude first, as GML 3 says. Names that disagree are refused, unless a system is given in
//! their place, which reads them all and says how many named another.
//!
//! Attributes: generic attributes (`gen:stringAttribute` and its kin) typed by their element, the
//! measures and counts the standard types so (`measuredHeight`, `storeysAboveGround`, …) and any
//! property with a unit (`uom`) in metres as numbers, a measure in another unit than the metre as
//! text with its unit, and every other simple property as text; a code's list (`codeSpace`) isn't
//! kept, and is said. A property given twice is a list, and a structured one (an external reference) its JSON.
//!
//! No tree of the whole model is built: one pass indexes the document (where each member lies, and
//! the systems its `srsName`s name), and each member is then parsed on its own and its tree dropped
//! once its rows are built, counted a member at a time and stopped between members when its run is
//! cancelled. The text and the rows are still the whole model's. The file is hostile: each member's
//! nesting and node count are bounded as every XML reader's are, no DTD is read, and the faces
//! merged are bounded.
//!
//! A geometry given by a link (`xlink:href`, `#` and a `gml:id`) to one written in its member, as a
//! solid's faces are to its walls' and roof's polygons, is read where the link stands, after what
//! its object writes out, and links a ground surface makes first: a geometry the object holds
//! already at that level of detail isn't read again, but as the ground when a ground surface links
//! to one read as another face, and through links each one is read at most once per object and
//! level, so a cycle of links ends. What links read in a member is bounded by the member's size.
//!
//! Not read, and said: geometries given by a link to another member, another file or nothing, or
//! to what isn't a geometry; objects, thematic surfaces and openings given only by a link; rings,
//! a line's or ring's points and curves, and an oriented curve's base curve given by one; terrain
//! intersection curves,
//! implicit geometries placed by their matrix (drawn at their reference point), appearances,
//! addresses, TIN reliefs, and the properties of thematic surfaces and openings (a name, generic
//! attributes); curve arcs and splines are read as straight lines. CityGML 3.0 is refused.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, hash_map::Entry};

use quick_xml::Reader;
use quick_xml::errors::SyntaxError;
use quick_xml::events::{BytesStart, Event};
use roxmltree::{Node, ParsingOptions};
use serde_json::{Map, Value as Json};

use crate::citymodel::*;
use crate::footprint::P3;
use crate::gml::{horizontal, lat_first, local};
use crate::raster::Crs;
use crate::{DataError, Dataset, ReadReport, Result};

/// How much of a file is looked at to recognize CityGML: its root's start tag, namespaces included.
pub const HEAD_BYTES: usize = 64 * 1024;

/// How many bytes of namespace declarations are copied around the members in all, at least: a
/// root declaring huge ones, with many members, would have them copied and parsed again for each.
const WRAP_BUDGET: usize = 1 << 30;

/// How many times over its own size, in bytes, a member's links may read: a geometry a link points
/// to is read once per object that links to it, so many objects linking to one large geometry (a
/// long `posList` as much as many elements) can't make a member's reading grow as their number
/// times its size.
const LINK_READS: usize = 4;

/// The namespace every CityGML version's core namespace starts with.
const CITYGML: &str = "http://www.opengis.net/citygml/";

/// The CityGML versions read, by their core namespace.
const VERSIONS: [&str; 2] = ["http://www.opengis.net/citygml/1.0", "http://www.opengis.net/citygml/2.0"];

/// The properties whose features are surfaces of their object (thematic surfaces, openings, traffic
/// areas), not objects of their own.
const SURFACE_PROPERTIES: [&str; 4] = ["boundedBy", "opening", "trafficArea", "auxiliaryTrafficArea"];

/// Features written in UpperCamelCase that are a property's value, not a city object.
const VALUES: [&str; 4] = ["Envelope", "Null", "Address", "Appearance"];

/// The standard's properties typed as numbers, by name: lengths (`gml:LengthType`) and counts or
/// years.
const LENGTHS: [&str; 5] = ["measuredHeight", "height", "trunkDiameter", "crownDiameter", "averageHeight"];
/// The GML geometries read, by local name.
const GEOMETRIES: [&str; 22] = [
    "Point",
    "MultiPoint",
    "LineString",
    "Curve",
    "LinearRing",
    "MultiCurve",
    "CompositeCurve",
    "Polygon",
    "Triangle",
    "Rectangle",
    "Surface",
    "PolyhedralSurface",
    "TriangulatedSurface",
    "Tin",
    "OrientableSurface",
    "TexturedSurface",
    "CompositeSurface",
    "MultiSurface",
    "Solid",
    "CompositeSolid",
    "MultiSolid",
    "MultiGeometry",
];
const COUNTS: [&str; 4] = ["storeysAboveGround", "storeysBelowGround", "yearOfConstruction", "yearOfDemolition"];

/// Whether a file is a CityGML city model, by its root: a `CityModel` in one of CityGML's
/// namespaces (3.0's too, which [`read`] refuses by name). Other GML, a WFS answer that only
/// mentions CityGML's namespaces, isn't.
pub fn looks_like_citygml(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(bytes.get(..HEAD_BYTES).unwrap_or(bytes));
    let mut reader = Reader::from_str(head.trim_start_matches('\u{feff}'));
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                return e.local_name().into_inner() == "CityModel" && root_namespace(&e).is_some_and(|ns| ns.starts_with(CITYGML));
            }
            Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => {}
        }
    }
}

/// Reads a CityGML file into a layer per object type and geometry family, those with something to
/// draw first. `given` is its coordinate system, when the caller knows it, over the one it names.
pub fn read(file_name: &str, bytes: &[u8], given: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    read_with(file_name, bytes, given, false)
}

/// As [`read`], for a member of an archive, where the system given is meant for what can't say its
/// own: a model that names one RustGIS knows keeps it.
pub(crate) fn read_member(file_name: &str, bytes: &[u8], given: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    read_with(file_name, bytes, given, true)
}

/// Reads a CityGML document; with `keep_named`, the system it names, when RustGIS knows it, goes
/// before `given`.
fn read_with(file_name: &str, bytes: &[u8], given: Option<Crs>, keep_named: bool) -> Result<Vec<(Dataset, ReadReport)>> {
    if bytes.len() as u64 > crate::io::MAX_BYTES {
        return Err(DataError::Format(format!("file is too large ({} bytes)", bytes.len())));
    }
    // A file cut inside a character is read up to it, as one cut inside a tag is.
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) if e.error_len().is_none() => std::str::from_utf8(bytes.get(..e.valid_up_to()).unwrap_or_default()).unwrap_or_default(),
        Err(_) => return Err(DataError::Format(format!("{file_name} is not UTF-8 text"))),
    };
    let text = text.trim_start_matches('\u{feff}');
    let stem = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("city model");
    // A system given for the whole file takes the place of every one it names, so names that
    // disagree don't stop it; in an archive, where the one a model names comes first, they do.
    let index = Index::of(file_name, text, given.is_some() && !keep_named, keep_named)?;
    if index.members.is_empty() {
        return Err(DataError::Format(format!("{file_name}: no city objects (cityObjectMember elements)")));
    }
    let named = index.system.as_deref().map(horizontal);
    let given = given.filter(|_| !keep_named || named.and_then(known_system).is_none());
    // Latitude first as the file's system says, when that is the system its coordinates are read in.
    let swap = index
        .system
        .as_deref()
        .is_some_and(|n| known_system(horizontal(n)).is_some_and(|crs| lat_first(n, crs) && given.is_none_or(|g| g.epsg() == crs.epsg())));
    let mut reading = Reading { budget: MAX_FILE_FACES, swap, ..Reading::default() };
    reading.said.appearances = index.appearances;
    reading.said.links = index.links;
    let mut wrapped = String::new();
    let mut wrap_left = text.len().saturating_mul(4).max(WRAP_BUDGET);
    // Counted a member at a time, and stopped between members when its run is cancelled (in the
    // desktop app's background read); the index before is one pass over the text, with no check.
    crate::progress::begin(index.members.len());
    for &(start, end, holder) in &index.members {
        crate::progress::step(1)?;
        // A member is parsed with the root's namespaces, as it stands in the document; an object of a
        // `featureMembers`, inside a copy of that element's start and end tags.
        let Some(member) = text.get(start..end) else {
            reading.counts.skipped += 1;
            continue;
        };
        let linked = member.contains("href");
        let (open, close) = holder.and_then(|k| index.holders.get(k)).map_or(("", ""), |(o, c)| (o.as_str(), c.as_str()));
        // Past the budget, a member isn't read, and is counted.
        let extra = index.namespaces.len() + open.len() + close.len();
        if extra > wrap_left {
            reading.counts.skipped += 1;
            continue;
        }
        wrap_left -= extra;
        wrapped.clear();
        wrapped.push_str("<m");
        wrapped.push_str(&index.namespaces);
        wrapped.push('>');
        wrapped.push_str(open);
        wrapped.push_str(member);
        wrapped.push_str(close);
        wrapped.push_str("</m>");
        let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
        let Ok(doc) = crate::xml::parse(&wrapped, opts) else {
            reading.counts.skipped += 1;
            continue;
        };
        let Some(member) = elements(doc.root_element()).next() else {
            reading.counts.skipped += 1;
            continue;
        };
        if only_link(&member) {
            reading.said.links += 1;
        }
        let mut targets = Targets::of(doc.root_element(), linked);
        reading.said.repeated_geometry_ids += targets.repeated;
        for object in elements(member) {
            match local(&object) {
                "Appearance" => reading.said.appearances += 1,
                value if VALUES.contains(&value) => {}
                // A property or a geometry where a feature belongs (malformed): left out, and said.
                name if !name.starts_with(char::is_uppercase) || is_gml(&object) => reading.said.not_features += 1,
                _ if only_link(&object) => reading.said.links += 1,
                _ => reading.object(object, None, &mut targets),
            }
        }
    }
    let Reading { counts, built, rows, reach, said, .. } = reading;
    let mut notes = Vec::new();
    let crs = coordinate_system(file_name, given, named, &reach, &mut notes)?;
    if let Some(other) = &index.other {
        notes.push(format!(
            "{} geometries or envelopes name a coordinate system other than the file's first ({other}), and are read in {}, the coordinate system given",
            index.others,
            crs.label()
        ));
    }
    notes.extend(said.notes(index.cut));
    finish(file_name, stem, rows, built, counts, notes, crs)
}

/// Where a document's members lie, the namespaces its root declares, and the system it names.
#[derive(Default)]
struct Index {
    /// Each member's byte range, and the holder in `holders` it is read in, when it is an object of a
    /// `featureMembers` or one of the objects a member cut short finished.
    members: Vec<(usize, usize, Option<usize>)>,
    /// The start tag as written, and the end tag, of a `featureMembers` element or of a member cut
    /// short, which the objects it finished are read in.
    holders: Vec<(String, String)>,
    /// The root's namespace declarations, written out as attributes.
    namespaces: String,
    /// The first `srsName`.
    system: Option<String>,
    /// A system is given in place of those the file names, so `srsName`s that disagree are kept to
    /// the first rather than refused.
    overridden: bool,
    /// A member of an archive, which keeps the systems its model names over the one given: a
    /// refusal says so rather than asking for one.
    archive: bool,
    /// When `overridden`: the elements naming a system other than the first, and the first of
    /// those names, said as read in the system given.
    others: usize,
    other: Option<String>,
    /// The appearances the model holds, which aren't read.
    appearances: usize,
    /// Members given only by a link that the index alone meets (an empty `featureMembers`), which
    /// aren't followed.
    links: usize,
    /// The file ends before its root closes: what came after is left out.
    cut: bool,
}

fn position(reader: &Reader<&[u8]>) -> usize {
    usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX)
}

/// An attribute's value by local name, its character and predefined entity references expanded
/// (as written, when they can't be).
fn start_attribute<'a>(e: &'a BytesStart, local: &str) -> Option<Cow<'a, str>> {
    let a = e.attributes().flatten().find(|a| a.key.local_name().into_inner() == local)?;
    Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or(a.value))
}

/// The namespace an element's own name is in, by the declarations it makes itself (as a root does),
/// its references expanded.
fn root_namespace<'a>(e: &'a BytesStart) -> Option<Cow<'a, str>> {
    let declaration = match e.name().into_inner().split_once(':') {
        Some((prefix, _)) => format!("xmlns:{prefix}"),
        None => "xmlns".to_string(),
    };
    let a = e.attributes().flatten().find(|a| a.key.into_inner() == declaration)?;
    Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or(a.value))
}

impl Index {
    /// Indexes a document in one pass, building no tree. Refuses one whose root isn't a CityGML 1.0
    /// or 2.0 city model, and, unless `overridden` (a system is given in their place), one whose
    /// `srsName`s name different systems; `archive` for a member of one.
    fn of(file_name: &str, text: &str, overridden: bool, archive: bool) -> Result<Index> {
        let invalid = |e: quick_xml::Error| DataError::Format(format!("{file_name}: {e}"));
        let mut reader = Reader::from_str(text);
        let mut index = Index { overridden, archive, ..Index::default() };
        // Open elements, and whether the root has been seen.
        let (mut depth, mut rooted) = (0usize, false);
        // The member being read: where it starts, and the depth its end tag brings back.
        let mut member: Option<(usize, usize)> = None;
        // The `featureMembers` being read, by its place in `holders`.
        let mut holder: Option<usize> = None;
        // The objects finished inside the member being read, and the one open now.
        let mut finished: Vec<(usize, usize)> = Vec::new();
        let mut child_start: Option<usize> = None;
        // The file ends inside the root's end tag, every member in.
        let mut in_root_end = false;
        loop {
            let start = position(&reader);
            let event = match reader.read_event() {
                Ok(event) => event,
                // A syntax error is the text ending inside markup (a tag, a comment): a file cut
                // short, as one cut between tags is. But for `<!` followed by what no markup
                // starts with, which is a file broken there.
                Err(quick_xml::Error::Syntax(e)) if !matches!(e, SyntaxError::InvalidBangMarkup) || text.ends_with("<!") => {
                    let at = usize::try_from(reader.error_position()).unwrap_or(usize::MAX);
                    in_root_end = depth == 1 && text.get(at..).is_some_and(|rest| rest.starts_with("</"));
                    break;
                }
                Err(e) => return Err(invalid(e)),
            };
            let (e, empty) = match &event {
                Event::Start(e) => (e, false),
                Event::Empty(e) => (e, true),
                Event::End(_) => {
                    depth = depth.saturating_sub(1);
                    if let Some((start, _)) = member.filter(|&(_, at)| at == depth) {
                        member = None;
                        index.members.push((start, position(&reader), holder));
                    } else if let Some(s) = child_start.filter(|_| depth == 2) {
                        child_start = None;
                        finished.push((s, position(&reader)));
                    }
                    if depth == 1 {
                        holder = None;
                    }
                    continue;
                }
                Event::Eof => break,
                _ => continue,
            };
            if depth == 0 {
                if rooted {
                    return Err(DataError::Format(format!("{file_name}: more than one root element")));
                }
                rooted = true;
                check_root(file_name, e)?;
                index.namespaces = declarations(e);
            }
            if let Some(srs) = start_attribute(e, "srsName").filter(|s| !s.trim().is_empty()) {
                index.see_system(file_name, srs.trim())?;
            }
            match (depth, holder) {
                (1, _) => match e.local_name().into_inner() {
                    "cityObjectMember" | "featureMember" if empty => index.members.push((start, position(&reader), None)),
                    "cityObjectMember" | "featureMember" => {
                        member = Some((start, 1));
                        finished.clear();
                    }
                    // One that points elsewhere (`xlink:href`, which GML gives a property in place of
                    // its contents) is a link, not followed.
                    "featureMembers" if empty => index.links += usize::from(start_attribute(e, "href").is_some()),
                    "featureMembers" => {
                        index.links += usize::from(start_attribute(e, "href").is_some());
                        let open = text.get(start..position(&reader)).unwrap_or_default().to_string();
                        index.holders.push((open, format!("</{}>", e.name().into_inner())));
                        holder = Some(index.holders.len() - 1);
                    }
                    "appearanceMember" => index.appearances += 1,
                    _ => {}
                },
                // An object of a `featureMembers`.
                (2, Some(_)) if empty => index.members.push((start, position(&reader), holder)),
                (2, Some(_)) => member = Some((start, 2)),
                (2, None) if matches!(member, Some((_, 1))) => {
                    if empty {
                        finished.push((start, position(&reader)));
                    } else {
                        child_start = Some(start);
                    }
                }
                _ => {}
            }
            if !empty {
                depth += 1;
            }
        }
        // A member cut short keeps the objects it finished, each in a copy of its member's tags.
        let first = finished.first().map(|&(s, _)| s);
        if let (Some((start, 1)), Some(first)) = (member, first) {
            let name: String = text
                .get(start + 1..first)
                .unwrap_or_default()
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '>' | '/'))
                .collect();
            index.holders.push((text.get(start..first).unwrap_or_default().to_string(), format!("</{name}>")));
            let holder = index.holders.len() - 1;
            index.members.extend(finished.into_iter().map(|(s, e)| (s, e, Some(holder))));
        }
        // A file cut short, its root not closed: what came after the cut is left out, and said.
        index.cut = depth > 0 && !in_root_end;
        Ok(index)
    }

    /// Takes in an `srsName`, on a geometry or on an envelope (often the only place a file names its
    /// system): the first names the file's system, and one naming another is an error, unless a
    /// system is given in their place. One system with its axes in different orders is an error
    /// either way: the order a name gives is how its coordinates are read.
    fn see_system(&mut self, file_name: &str, srs: &str) -> Result<()> {
        let Some(first) = &self.system else {
            self.system = Some(srs.to_string());
            return Ok(());
        };
        if first == srs {
            return Ok(());
        }
        let system = |name: &str| known_system(horizontal(name)).map(|crs| (crs, lat_first(name, crs)));
        let cut = |s: &str| s.chars().take(80).collect::<String>();
        match (system(first), system(srs)) {
            (Some(a), Some(b)) if a == b => Ok(()),
            (Some((a, _)), Some((b, _))) if a == b => Err(DataError::Unsupported(format!(
                "{file_name}: it names one coordinate system with its axes in different orders ({} and {}), which aren't read together",
                cut(first),
                cut(srs)
            ))),
            _ if self.overridden => {
                self.others += 1;
                self.other.get_or_insert_with(|| cut(srs));
                Ok(())
            }
            _ if self.archive => Err(DataError::Unsupported(format!(
                "{file_name}: it names different coordinate systems ({} and {}), which aren't read together; in an archive a model keeps the systems it names, so a coordinate system given doesn't apply to it",
                cut(first),
                cut(srs)
            ))),
            _ => Err(DataError::Unsupported(format!(
                "{file_name}: it names different coordinate systems ({} and {}), which aren't read together; give the coordinate system (crs) to read it in one",
                cut(first),
                cut(srs)
            ))),
        }
    }
}

/// Refuses a root that isn't a CityGML 1.0 or 2.0 city model.
fn check_root(file_name: &str, e: &BytesStart) -> Result<()> {
    if e.local_name().into_inner() != "CityModel" {
        return Err(DataError::Format(format!("{file_name}: not a CityGML city model (its root isn't CityModel)")));
    }
    match root_namespace(e).as_deref().map(str::trim) {
        Some(ns) if VERSIONS.contains(&ns) => Ok(()),
        Some("http://www.opengis.net/citygml/3.0") => {
            Err(DataError::Unsupported(format!("{file_name}: CityGML 3.0: RustGIS reads CityGML 1.0 and 2.0")))
        }
        _ => Err(DataError::Format(format!("{file_name}: not a CityGML 1.0 or 2.0 city model (its root's namespace isn't one)"))),
    }
}

/// The namespaces an element declares, written out as attributes again, each after a space.
fn declarations(e: &BytesStart) -> String {
    let mut out = String::new();
    for a in e.attributes().flatten() {
        let key = a.key.into_inner();
        if key == "xmlns" || key.starts_with("xmlns:") {
            // As written, so entities stay as they were; quoted with what the value doesn't hold.
            let quote = if a.value.contains('"') { '\'' } else { '"' };
            out.push_str(&format!(" {key}={quote}{}{quote}", a.value));
        }
    }
    out
}

fn elements<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(Node::is_element)
}

/// An attribute's value, by local name (`gml:id`, `xlink:href`).
fn attribute<'a>(n: &Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

/// An element's text, borrowed when it is one piece (as numbers and names are).
fn text<'a>(n: &Node<'a, '_>) -> Cow<'a, str> {
    let mut pieces = n.children().filter(Node::is_text).filter_map(|t| t.text());
    match (pieces.next(), pieces.next()) {
        (None, _) => Cow::Borrowed(""),
        (Some(one), None) => Cow::Borrowed(one),
        (Some(first), Some(second)) => Cow::Owned([first, second].into_iter().chain(pieces).collect()),
    }
}

/// A link with nothing written in place.
fn only_link(n: &Node) -> bool {
    attribute(n, "href").is_some() && elements(*n).next().is_none()
}

/// Whether a namespace is GML's, in any of its versions (`…/gml`, `…/gml/3.2`), and not one that
/// only starts with its name.
fn gml_namespace(ns: &str) -> bool {
    const GML: &str = "http://www.opengis.net/gml";
    ns.strip_prefix(GML).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Whether an element is GML's (a geometry, or an envelope).
fn is_gml(n: &Node) -> bool {
    n.tag_name().namespace().is_some_and(gml_namespace)
}

/// An element's `gml:id` (not an extension's `id`), trimmed; a blank one is none.
fn gml_id<'a>(n: &Node<'a, '_>) -> Option<&'a str> {
    n.attributes()
        .find(|a| a.name() == "id" && a.namespace().is_some_and(gml_namespace))
        .map(|a| a.value().trim())
        .filter(|id| !id.is_empty())
}

/// Whether an element is a GML geometry this reader draws.
fn is_geometry(g: &Node) -> bool {
    is_gml(g) && GEOMETRIES.contains(&local(g))
}

/// Whether a link where a geometry belongs points to one this reader reads there: a GML geometry,
/// a solid's shell, a surface's patch, an oriented curve, or an implicit geometry (drawn at its
/// reference point as one written out is). Not an envelope, a ring's curve or a segment.
fn linkable(n: &Node) -> bool {
    local(n) == "ImplicitGeometry"
        || is_geometry(n)
        || (is_gml(n) && matches!(local(n), "Shell" | "PolygonPatch" | "OrientableCurve" | "GeometricComplex"))
}

/// Whether a line or ring holds a point or curve given only by a link, so it isn't whole.
fn has_linked_part(n: &Node) -> bool {
    n.descendants().any(|d| matches!(local(&d), "curveMember" | "baseCurve" | "pointProperty" | "pointRep") && only_link(&d))
}

/// The level of detail a geometry property names (`lod2Solid` is 2), when it holds a geometry or
/// links to one: a property so named that holds text is a value.
fn property_lod(p: &Node) -> Option<u32> {
    // A geometry, an implicit one, or a link: not an extension's structured value (`lod2Metadata`).
    let holds = elements(*p).next().is_some_and(|g| is_gml(&g) || local(&g) == "ImplicitGeometry") || attribute(p, "href").is_some();
    local(p).strip_prefix("lod").and_then(|rest| rest.chars().next()).and_then(|c| c.to_digit(10)).filter(|_| holds)
}

/// Whether a curve has a segment that isn't a straight line (an arc or a spline).
fn curved(n: &Node) -> bool {
    n.descendants().filter(|d| local(d) == "segments").flat_map(elements).any(|s| local(&s) != "LineStringSegment")
}

/// Whether a feature in a property is a city object rather than a structured value or a geometry:
/// city objects have a `gml:id` or a geometry. An extension's data types are written like features
/// but have neither.
fn is_object(e: &Node) -> bool {
    local(e).starts_with(char::is_uppercase)
        && !is_gml(e)
        && !VALUES.contains(&local(e))
        // An id given, even blank (which isn't kept as its id).
        && (e.attributes().any(|a| a.name() == "id" && a.namespace().is_some_and(gml_namespace))
            || local(e) == "TINRelief"
            || elements(*e).any(|p| {
                // A terrain intersection is an auxiliary line, not the feature's geometry.
                !local(&p).ends_with("TerrainIntersection")
                    && (property_lod(&p).is_some()
                        || elements(p).next().is_some_and(|g| is_geometry(&g))
                        || (SURFACE_PROPERTIES.contains(&local(&p)) && elements(p).any(|s| !is_gml(&s)))
                        // A wrapper with no id of its own, which carries the objects it holds.
                        || elements(p).any(|c| is_object(&c)))
            }))
}

/// Adds a value, a key given again making a list.
fn insert(map: &mut Map<String, Json>, key: &str, value: Json) {
    // No value read is a list itself, so a list here is a key already given.
    match map.get_mut(key) {
        Some(Json::Array(values)) => values.push(value),
        Some(existing) => {
            let first = existing.take();
            *existing = Json::Array(vec![first, value]);
        }
        None => {
            map.insert(key.to_string(), value);
        }
    }
}

/// A number, or text with its unit when that isn't the metre.
fn measure(text: &str, uom: Option<&str>) -> Option<Json> {
    // A blank unit is no unit.
    match uom.map(str::trim).filter(|u| !u.is_empty() && !is_metre(u)) {
        None => text.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Json::Number),
        Some(uom) => Some(Json::from(format!("{text} {uom}"))),
    }
}

/// A generic attribute's value, typed by its element (`gen:intAttribute`, `gen:doubleAttribute`,
/// `gen:measureAttribute` with its unit; the others are text), and a set's as an object of its
/// attributes.
fn generic_value(a: &Node) -> Json {
    if local(a) == "genericAttributeSet" {
        let mut set = Map::new();
        for member in elements(*a) {
            if let Some(name) = attribute(&member, "name") {
                insert(&mut set, name, generic_value(&member));
            }
        }
        return Json::Object(set);
    }
    let value = elements(*a).find(|v| local(v) == "value");
    let text = value.as_ref().map(text).unwrap_or_default();
    let text = text.trim();
    let number = match local(a) {
        "intAttribute" => text.parse::<i64>().ok().map(Json::from),
        "doubleAttribute" => measure(text, None),
        "measureAttribute" => measure(text, value.as_ref().and_then(|v| attribute(v, "uom")).or_else(|| attribute(a, "uom"))),
        _ => None,
    };
    number.unwrap_or_else(|| Json::from(text))
}

/// Whether a unit of measure is the metre.
fn is_metre(uom: &str) -> bool {
    let u = uom.trim().to_ascii_lowercase();
    matches!(u.as_str(), "m" | "#m" | "metre" | "meter")
        || [":m", "#m", "::9001", "/epsg/0/9001", ":metre", "/metre", "#metre"].iter().any(|end| u.ends_with(end))
}

/// A simple property's value: a number for the standard's lengths and counts (a length in another
/// unit is text with its unit), text otherwise. A structured one is the JSON of its parts.
fn value(p: &Node) -> Option<Json> {
    if elements(*p).next().is_some() {
        let mut map = Map::new();
        for c in elements(*p) {
            if let Some(v) = value(&c) {
                insert(&mut map, local(&c), v);
            }
        }
        return Some(Json::Object(map));
    }
    let text = text(p);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let name = local(p);
    let typed = if LENGTHS.contains(&name) || attribute(p, "uom").is_some() {
        measure(text, attribute(p, "uom"))
    } else if COUNTS.contains(&name) {
        text.parse::<i64>().ok().map(Json::from)
    } else {
        None
    };
    Some(typed.unwrap_or_else(|| Json::from(text)))
}

/// What this reader leaves out that the others don't meet, for one notice per kind.
#[derive(Default, Clone)]
struct Said {
    /// Geometries whose coordinates can't be read, or that lack a part they need.
    bad_coordinates: usize,
    /// Geometries and objects given only by a link that isn't followed: a geometry's to another
    /// member, another file or nothing, and any to an object, a thematic surface or opening, a ring,
    /// a line's or ring's point or curve, or an oriented curve's base curve.
    links: usize,
    /// Links where a geometry belongs to an element of the member that isn't one.
    not_geometry_links: usize,
    /// Links to geometries not followed because the member's links had read what they may.
    links_past_limit: usize,
    /// Objects with no id, holding parts that can't name them as their parent.
    anonymous: usize,
    /// Objects whose id another object has too.
    repeated_ids: usize,
    /// Geometries in a member with links whose `gml:id` an earlier element there has too.
    repeated_geometry_ids: usize,
    terrain_intersections: usize,
    appearances: usize,
    /// Curves with arcs or splines, read as straight lines through their points.
    curved: usize,
    /// `gml:Point`s written with more than one position, from which only the first is read.
    multi_position_points: usize,
    /// Features beside city objects in a property that have no id and no geometry.
    not_objects: usize,
    /// Values naming the code list they are taken from (`codeSpace`), which isn't kept.
    code_spaces: usize,
    /// Values with attributes other than their unit and code list (a language, `xml:lang`), which
    /// aren't kept.
    value_attributes: usize,
    /// Elements in a city object member that aren't features (a `gml:name`, a property).
    not_features: usize,
    /// Members of a generic attribute set with no name, which aren't kept.
    unnamed_generic: usize,
    /// Thematic surfaces and openings with properties of their own, which aren't kept.
    surface_properties: usize,
}

impl Said {
    /// The notices, and whether the file ends inside a member.
    fn notes(&self, cut: bool) -> Vec<String> {
        let mut out = Vec::new();
        if cut {
            out.push(
                "the file ends before its city model closes, as if cut short: any city object from the cut on is left out".to_string(),
            );
        }
        let mut say = |n: usize, text: String| {
            if n > 0 {
                out.push(text);
            }
        };
        say(
            self.bad_coordinates,
            format!(
                "{} geometries of city objects have coordinates that can't be read (not numbers, or not as many as their dimension asks for) or lack a part they need, and are left out",
                self.bad_coordinates
            ),
        );
        say(
            self.links,
            format!(
                "{} geometries or objects are given only by a link to another element (xlink:href), which isn't followed: they are left out, and what they point to is drawn only where it is written out",
                self.links
            ),
        );
        say(
            self.not_geometry_links,
            format!(
                "{} links (xlink:href) where a geometry belongs point to an element that isn't a geometry, and are left out",
                self.not_geometry_links
            ),
        );
        say(
            self.links_past_limit,
            format!(
                "{} links (xlink:href) to geometries aren't followed, as they would read their city object member over many times: what they point to is left out where they stand",
                self.links_past_limit
            ),
        );
        say(
            self.anonymous,
            format!(
                "{} city objects have no gml:id: their parts are kept without a parent, and their heights don't take in their parts'",
                self.anonymous
            ),
        );
        say(
            self.repeated_ids,
            format!("{} city objects have a gml:id another object has too: their parts' heights go to only one of them", self.repeated_ids),
        );
        say(
            self.repeated_geometry_ids,
            format!(
                "{} geometries have a gml:id an earlier element of their city object member has too: a link to that id reads the first",
                self.repeated_geometry_ids
            ),
        );
        say(
            self.terrain_intersections,
            format!(
                "{} terrain intersection curves (where objects meet the ground) aren't read and are left out",
                self.terrain_intersections
            ),
        );
        say(self.appearances, format!("the file's appearances (textures and materials) aren't read: {} are left out", self.appearances));
        say(self.curved, format!("{} curves have arcs or splines, read as straight lines through their points", self.curved));
        say(
            self.multi_position_points,
            format!(
                "{} gml:Points give more than one position, but a GML Point holds one: only the first is read",
                self.multi_position_points
            ),
        );
        say(
            self.not_objects,
            format!("{} nested features with no gml:id and no geometry, beside city objects, are left out", self.not_objects),
        );
        say(
            self.code_spaces,
            format!(
                "{} values name the code list they are taken from (codeSpace), which isn't kept: the code is kept on its own",
                self.code_spaces
            ),
        );
        say(
            self.value_attributes,
            format!(
                "{} values have attributes other than their unit (uom) or code list (codeSpace), such as a language (xml:lang), which aren't kept: the value is kept on its own",
                self.value_attributes
            ),
        );
        say(
            self.unnamed_generic,
            format!("{} generic attributes in a set (genericAttributeSet) have no name, and are left out", self.unnamed_generic),
        );
        say(
            self.not_features,
            format!(
                "{} elements in city object members aren't city objects (a gml:name, a property where a feature belongs), and are left out",
                self.not_features
            ),
        );
        say(
            self.surface_properties,
            format!(
                "{} thematic surfaces or openings (walls, roofs, windows, doors) have properties of their own (a name, generic attributes), which aren't kept: their faces are kept as their object's",
                self.surface_properties
            ),
        );
        out
    }
}

/// The elements of a member a link can point to, by `gml:id`, and how many bytes of it its links
/// may still read.
struct Targets<'a, 'i> {
    by_id: HashMap<&'a str, Node<'a, 'i>>,
    budget: usize,
    /// Geometries a link may point to whose `gml:id` an earlier element of the member has too.
    repeated: usize,
    /// The geometries of the member's objects whose coordinates couldn't be read, said already: a
    /// link to one, from any of its objects, is left out without being said again.
    failed: HashSet<Key<'a>>,
}

impl<'a, 'i> Targets<'a, 'i> {
    /// A member's elements by `gml:id`, the first of an id given twice; none when it holds no link.
    fn of(member: Node<'a, 'i>, linked: bool) -> Self {
        let mut by_id = HashMap::new();
        let mut repeated = 0;
        if linked {
            for n in member.descendants().filter(Node::is_element) {
                if let Some(id) = gml_id(&n) {
                    match by_id.entry(id) {
                        Entry::Vacant(v) => {
                            v.insert(n);
                        }
                        Entry::Occupied(_) => repeated += usize::from(linkable(&n)),
                    }
                }
            }
        }
        Targets { by_id, budget: member.range().len().saturating_mul(LINK_READS), repeated, failed: HashSet::new() }
    }
}

/// Where a geometry is read into its object: the level of detail its property names (none in a
/// property that names none), and whether its faces are the ground's.
#[derive(Clone, Copy)]
struct At {
    lod: Option<u32>,
    ground: bool,
}

impl At {
    /// The level of detail as a number to compare, -1 for none.
    fn rank(self) -> f64 {
        self.lod.map_or(-1.0, f64::from)
    }
}

/// A link to a geometry met while reading an object, followed once the object's own geometries
/// are read.
struct Pending<'a, 'i> {
    href: &'a str,
    /// What is read only when the link is found: a solid's cavities, when its exterior is a link,
    /// in the `srsDimension` where the link stands (`dim`).
    then: Vec<Node<'a, 'i>>,
    dim: Option<usize>,
    at: At,
}

/// A geometry of an object, by its `gml:id` and the level of detail it is read at.
type Key<'a> = (&'a str, Option<u32>);

/// One object's links: the geometries it has read, each with whether it was read as the ground, and
/// the links left to follow, the ground's apart.
#[derive(Default)]
struct Links<'a, 'i> {
    seen: HashMap<Key<'a>, bool>,
    /// What `seen` took in, in order, with what it held before, so a geometry left out can take its
    /// own marks back.
    marked: Vec<(Key<'a>, Option<bool>)>,
    /// The member's geometries whose coordinates couldn't be read ([`Targets::failed`]), lent to the
    /// object while it is read.
    failed: HashSet<Key<'a>>,
    ground: Vec<Pending<'a, 'i>>,
    others: Vec<Pending<'a, 'i>>,
}

impl<'a, 'i> Links<'a, 'i> {
    /// Whether a geometry needn't be read for `ground`: read already, and as the ground if that is
    /// what it is read for now.
    fn read(&self, key: Key<'a>, ground: bool) -> bool {
        self.seen.get(&key).is_some_and(|&was| was || !ground)
    }

    /// Marks a geometry read, as the ground or not; false when it needn't be read.
    fn mark(&mut self, key: Key<'a>, ground: bool) -> bool {
        if self.read(key, ground) {
            return false;
        }
        let before = self.seen.insert(key, ground);
        self.marked.push((key, before));
        true
    }

    /// Takes back the marks made since `from`, latest first.
    fn unmark(&mut self, from: usize) {
        let taken: Vec<_> = self.marked.drain(from..).collect();
        for (key, before) in taken.into_iter().rev() {
            match before {
                Some(was) => self.seen.insert(key, was),
                None => self.seen.remove(&key),
            };
        }
    }

    /// Keeps a link to follow, in the ground's queue when it is a ground surface's.
    fn push(&mut self, link: Pending<'a, 'i>) {
        if link.at.ground { self.ground.push(link) } else { self.others.push(link) }
    }

    /// The next link to follow: the ground's first, so a face a ground surface links to is the
    /// ground's whatever else links to it, and whatever order they are written in.
    fn next(&mut self) -> Option<Pending<'a, 'i>> {
        self.ground.pop().or_else(|| self.others.pop())
    }
}

/// The reading of a document's objects.
#[derive(Default)]
struct Reading {
    counts: Counts,
    said: Said,
    budget: usize,
    built: Vec<Built>,
    rows: Vec<Row>,
    reach: Reach,
    /// Coordinates come latitude first.
    swap: bool,
    /// The ids already given, for those given again.
    ids: HashSet<String>,
}

impl Reading {
    /// Counts what a generic attribute's value carries that isn't kept: its code list
    /// (`codeSpace`), its other attributes (a language; a unit but on a `measureAttribute`), and a
    /// set's members with no name.
    fn said_generic(&mut self, a: &Node) {
        for v in a.descendants().filter(|n| n.is_element() && local(n) == "value") {
            let measure = v.parent_element().is_some_and(|p| local(&p) == "measureAttribute");
            self.said.code_spaces += usize::from(attribute(&v, "codeSpace").is_some());
            self.said.value_attributes +=
                usize::from(v.attributes().any(|at| !(at.name() == "codeSpace" || (measure && at.name() == "uom"))));
        }
        self.said.unnamed_generic += a
            .descendants()
            .filter(|n| n.is_element() && n.parent_element().is_some_and(|p| local(&p) == "genericAttributeSet"))
            .filter(|n| attribute(n, "name").is_none())
            .count();
    }

    /// Reads a city object into its rows, then the objects nested in it. Its links are followed
    /// after its own geometries, with no look at Cancel: what they read is bounded by the member's
    /// size ([`LINK_READS`]), as parsing the member is, and Cancel is looked at between members.
    fn object<'a, 'i>(&mut self, e: Node<'a, 'i>, parent: Option<&str>, targets: &mut Targets<'a, 'i>) {
        let ty = local(&e);
        if ty == "TINRelief" {
            self.counts.tin_reliefs += 1;
            return;
        }
        let id = gml_id(&e);
        let mut parts = ObjectParts::default();
        let mut links = Links { failed: std::mem::take(&mut targets.failed), ..Links::default() };
        let mut attributes = Map::new();
        let mut children = Vec::new();
        let mut address = false;
        for p in elements(e) {
            let name = local(&p);
            let first = elements(p).next();
            if (name.ends_with("Attribute") || name == "genericAttributeSet")
                && let Some(key) = attribute(&p, "name")
            {
                self.said_generic(&p);
                insert(&mut attributes, key, generic_value(&p));
            } else if name.ends_with("TerrainIntersection") {
                // Where the object meets the ground: an auxiliary line, not the object's shape.
                self.said.terrain_intersections += 1;
            } else if let Some(rank) = property_lod(&p) {
                self.geometry(&p, At { lod: Some(rank), ground: false }, &mut parts, &mut links);
            } else if name == "address" {
                address = true;
            } else if name == "appearance" {
                self.said.appearances += 1;
            } else if only_link(&p) && SURFACE_PROPERTIES.contains(&name) {
                self.said.links += 1;
            } else if let Some(href) = attribute(&p, "href").filter(|_| first.is_none()) {
                // A relation to another feature (a group's members, a generalization). A part given
                // by a link is kept as one, and the link said: the part is read only where it is
                // written out.
                self.said.links += usize::from(name.starts_with("consistsOf"));
                insert(&mut attributes, name, Json::from(href.trim()));
            } else if let Some(first) = first.filter(|f| VALUES.contains(&local(f))) {
                // An envelope (its system is read with the others), or what is said apart.
                self.said.appearances += usize::from(local(&first) == "Appearance");
                address |= local(&first) == "Address";
            } else if first.is_some_and(|f| is_gml(&f)) {
                // A geometry in a property that names no level of detail (a group's, a relief's).
                self.geometry(&p, At { lod: None, ground: false }, &mut parts, &mut links);
            } else if first.is_some_and(|f| SURFACE_PROPERTIES.contains(&name) && local(&f).starts_with(char::is_uppercase)) {
                elements(p).for_each(|s| self.surface(&s, &mut parts, &mut links));
            } else if elements(p).any(|c| is_object(&c)) {
                // The objects; a feature beside them with no id and no geometry is said.
                for c in elements(p) {
                    if is_object(&c) {
                        children.push(c);
                    } else {
                        self.said.not_objects += 1;
                    }
                }
            } else if let Some(v) = value(&p) {
                self.said.code_spaces += p.descendants().filter(|n| n.is_element() && attribute(n, "codeSpace").is_some()).count();
                self.said.value_attributes +=
                    p.descendants().filter(|n| n.is_element() && n.attributes().any(|a| !matches!(a.name(), "uom" | "codeSpace"))).count();
                insert(&mut attributes, name, v);
            }
        }
        // Links last, once what the object writes out is read: a link to that adds nothing, and a
        // ground face stays the ground's though a solid links to it first.
        while let Some(link) = links.next() {
            self.follow(link, targets, &mut parts, &mut links);
        }
        targets.failed = links.failed;
        self.counts.addresses += usize::from(address);
        let at_row = self.built.len();
        let made = object_rows(parts, at_row, &mut self.counts, &mut self.budget);
        if made.rows.is_empty() {
            self.counts.no_geometry += 1;
            self.rows.push(Row::without_geometry(at_row));
        }
        self.rows.extend(made.rows);
        match id {
            Some(id) => self.said.repeated_ids += usize::from(!self.ids.insert(id.to_string())),
            None => self.said.anonymous += usize::from(!children.is_empty()),
        }
        self.built.push(Built {
            id: id.unwrap_or_default().to_string(),
            ty: ty.to_string(),
            parent_ids: parent.map(str::to_string).into_iter().collect(),
            z: made.z,
            solid: made.solid,
            attributes: attributes.into_iter().collect(),
        });
        for child in children {
            self.object(child, id, targets);
        }
    }

    /// Reads the geometry a link points to in its member, as if written where the link stands. A
    /// link to another member, another file or nothing is said, as is one to what isn't a geometry.
    fn follow<'a, 'i>(&mut self, link: Pending<'a, 'i>, targets: &mut Targets<'a, 'i>, parts: &mut ObjectParts, links: &mut Links<'a, 'i>) {
        let id = link.href.trim().strip_prefix('#').map(str::trim);
        let Some((id, target)) = id.and_then(|id| targets.by_id.get(id).map(|t| (id, *t))) else {
            self.said.links += 1;
            return;
        };
        if !linkable(&target) {
            self.said.not_geometry_links += 1;
            return;
        }
        // Read already in this object at this level of detail, written out or by another link:
        // nothing to add.
        let key = (id, link.at.lod);
        if links.failed.contains(&key) {
            // Said where it failed. A solid whose exterior it is lacks the part it needs, and is left
            // out with its cavities.
            self.said.bad_coordinates += usize::from(!link.then.is_empty());
            return;
        }
        let read = links.read(key, link.at.ground);
        if read && link.then.is_empty() {
            return;
        }
        // Its bytes in the member: what reading it parses, its elements and its coordinates' text.
        let cost = if read { 0 } else { target.range().len() };
        if cost > targets.budget {
            self.said.links_past_limit += 1;
            return;
        }
        targets.budget -= cost;
        // The dimension of the geometries it is written in, not of the link's.
        let dim = target.ancestors().skip(1).find_map(|a| attribute(&a, "srsDimension"));
        let Pending { then, dim: at_link, .. } = link;
        self.walk(
            |w| {
                // Marks are made as it is read, inside the walk, so a walk that fails (a cavity that
                // can't be read) takes them back, and the target is read again by its next link.
                w.following = true;
                if !read {
                    if let Some(d) = dim {
                        let Ok(d) = d.trim().parse() else {
                            w.failed = Some(key);
                            return None;
                        };
                        w.dim = Some(d);
                    }
                    w.geometry(&target)?;
                }
                // The cavities are written where the link stands, in its dimension.
                w.dim = at_link;
                then.iter().try_for_each(|cavity| w.member(cavity))
            },
            link.at,
            parts,
            links,
        );
    }

    /// A thematic surface or an opening: its geometries' faces are its object's, the ground's when it
    /// is a `GroundSurface`. Its openings are read with it. Its own properties (a name, generic
    /// attributes) aren't kept, and are counted.
    fn surface<'a, 'i>(&mut self, s: &Node<'a, 'i>, parts: &mut ObjectParts, links: &mut Links<'a, 'i>) {
        if only_link(s) {
            self.said.links += 1;
            return;
        }
        let ground = local(s) == "GroundSurface";
        let mut own = false;
        for p in elements(*s) {
            if let Some(rank) = property_lod(&p) {
                self.geometry(&p, At { lod: Some(rank), ground }, parts, links);
            } else if local(&p) == "opening" {
                if only_link(&p) {
                    self.said.links += 1;
                } else {
                    elements(p).for_each(|o| self.surface(&o, parts, links));
                }
            } else if local(&p) == "appearance" {
                self.said.appearances += 1;
            } else if !(is_gml(&p) && local(&p) == "boundedBy") && !local(&p).starts_with("lod") {
                // Its extent (`gml:boundedBy`) is where it lies, and an empty geometry property
                // holds nothing: neither is a property of its own.
                own = true;
            }
        }
        self.said.surface_properties += usize::from(own);
    }

    /// A geometry property's faces, lines and points into the object's parts, at `at`; the
    /// property given by a link is followed with the object's other links.
    fn geometry<'a, 'i>(&mut self, p: &Node<'a, 'i>, at: At, parts: &mut ObjectParts, links: &mut Links<'a, 'i>) {
        if let Some(href) = attribute(p, "href").filter(|_| only_link(p)) {
            links.push(Pending { href, then: Vec::new(), dim: None, at });
            return;
        }
        let Some(g) = elements(*p).next() else { return };
        self.walk(|w| w.geometry(&g), at, parts, links);
    }

    /// Walks a geometry with `go` into the object's parts at `at`. The links it meets are left to
    /// follow after the object's own geometries, unless it is left out, when they go with it.
    fn walk<'a, 'i>(
        &mut self,
        go: impl FnOnce(&mut Walk<'_, 'a, 'i>) -> Option<()>,
        at: At,
        parts: &mut ObjectParts,
        links: &mut Links<'a, 'i>,
    ) {
        // The reach is the file's and no geometry's: a geometry that is left out leaves no point in it,
        // and no link to follow.
        let (reach, ground, others, marked) = (self.reach, links.ground.len(), links.others.len(), links.marked.len());
        // What it says of its parts: a geometry left out is said as one, so a link that reads it
        // again doesn't say its parts again.
        let said = self.said.clone();
        let mut walk = Walk {
            counts: &mut self.counts,
            said: &mut self.said,
            reach: &mut self.reach,
            swap: self.swap,
            at,
            dim: None,
            instance: false,
            following: false,
            failed: None,
            got: Got::default(),
            links,
        };
        if go(&mut walk).is_none() {
            // Said once: a link to the geometry that failed doesn't say it again.
            if let Some(key) = walk.failed {
                links.failed.insert(key);
            }
            self.said = said;
            self.said.bad_coordinates += 1;
            self.reach = reach;
            links.ground.truncate(ground);
            links.others.truncate(others);
            // Its geometries weren't read: a link to one of them still reads it.
            links.unmark(marked);
            return;
        }
        let Got { faces, surface, mut lines, points, heights, solid } = walk.got;
        parts.heights.add(&heights);
        parts.solid |= solid;
        let (rank, lod) = (at.rank(), at.lod.map(|l| l.to_string()));
        if surface {
            self.counts.dropped_rings += faces.dropped_rings;
            parts.surfaces.push(Surface { lod: lod.clone(), rank, faces });
        }
        lines.retain(|l| !l.is_empty());
        if !lines.is_empty() {
            parts.lines.add_lines(rank, || lod.clone(), lines);
        }
        if !points.is_empty() {
            parts.points.add_points(rank, || lod, points);
        }
    }
}

/// What one geometry holds.
#[derive(Default)]
struct Got {
    faces: Faces,
    /// It holds surfaces, kept or not.
    surface: bool,
    lines: Vec<Vec<P3>>,
    /// Its points, each with whether it is an implicit geometry's reference point.
    points: Vec<(P3, bool)>,
    /// The positions given with a height, which an object's heights come from (not an implicit
    /// geometry's reference point).
    heights: Vec<P3>,
    solid: bool,
}

/// A walk through a geometry's elements. `None` when coordinates can't be read or a part a geometry
/// needs is missing: the whole geometry is left out.
struct Walk<'r, 'a, 'i> {
    counts: &'r mut Counts,
    said: &'r mut Said,
    reach: &'r mut Reach,
    swap: bool,
    /// Where its geometry goes in its object: its level of detail, and whether its faces are a
    /// `GroundSurface`'s.
    at: At,
    /// The `srsDimension` a geometry it is in gives.
    dim: Option<usize>,
    /// Reading an implicit geometry's reference point.
    instance: bool,
    /// Reading what a link points to: a geometry its object has read at this level isn't read again.
    following: bool,
    /// The innermost geometry with a `gml:id` whose coordinates couldn't be read, when one is.
    failed: Option<Key<'a>>,
    got: Got,
    /// Its object's links: the geometries read, and those to follow.
    links: &'r mut Links<'a, 'i>,
}

impl<'a, 'i> Walk<'_, 'a, 'i> {
    /// Reads a geometry into what its walk holds, in the `srsDimension` it gives; one given only by
    /// a link is kept to follow.
    fn geometry(&mut self, g: &Node<'a, 'i>) -> Option<()> {
        // Read for its object at this level of detail: a link to it from there adds nothing, and
        // reached through a link once read (a linked composite of polygons read before), it isn't
        // read again. Marked before a link it is is kept, so a link to itself ends.
        if let Some(id) = gml_id(g)
            && !self.links.mark((id, self.at.lod), self.at.ground)
            && self.following
        {
            return Some(());
        }
        // A geometry given by a link is followed once its object's own are read.
        if only_link(g) {
            self.link(g, Vec::new());
            return Some(());
        }
        // A geometry's `srsDimension` holds for what it holds.
        let outer = self.dim;
        let done = match attribute(g, "srsDimension") {
            Some(d) => d.trim().parse().ok().map(|d| self.dim = Some(d)),
            None => Some(()),
        }
        .and_then(|()| self.shape(g));
        self.dim = outer;
        // The innermost geometry that failed is the one lost: a link to its parts still reads them.
        if done.is_none()
            && let Some(id) = gml_id(g)
        {
            self.failed.get_or_insert((id, self.at.lod));
        }
        done
    }

    /// Keeps a link to a geometry to follow, and what is read only when it is found.
    fn link(&mut self, n: &Node<'a, 'i>, then: Vec<Node<'a, 'i>>) {
        if let Some(href) = attribute(n, "href") {
            self.links.push(Pending { href, then, dim: self.dim, at: self.at });
        }
    }

    fn shape(&mut self, g: &Node<'a, 'i>) -> Option<()> {
        match local(g) {
            "Point" => {
                // A point is its first position, and its height that one's alone.
                let before = self.got.heights.len();
                let p = self.tagged(g)?;
                let (first, first_has_height) = *p.first()?;
                if p.len() > 1 {
                    self.said.multi_position_points += 1;
                }
                // The height it keeps is the first position's own, if that was given: a later
                // position's isn't the point's.
                self.got.heights.truncate(before);
                if first_has_height {
                    self.got.heights.push(first);
                }
                self.got.points.push((first, false));
            }
            "MultiPoint" => self.members(g, &["pointMember"], "pointMembers")?,
            name @ ("LineString" | "Curve" | "LinearRing") => {
                if name == "Curve" {
                    self.said.curved += usize::from(curved(g));
                }
                // A line with a point or curve given by a link isn't whole, and is left out.
                if has_linked_part(g) {
                    self.said.links += 1;
                    return Some(());
                }
                let line = self.positions(g)?;
                self.got.lines.push(line);
            }
            "MultiCurve" | "CompositeCurve" => self.members(g, &["curveMember"], "curveMembers")?,
            "OrientableCurve" => {
                // An oriented curve is its base curve's line, written the other way when its
                // orientation is `-`. A base curve given by a link isn't followed.
                let base = elements(*g).find(|c| local(c) == "baseCurve")?;
                // A base curve given by a link, or with a point or curve given by one, isn't whole.
                if only_link(&base) || has_linked_part(&base) {
                    self.said.links += 1;
                    return Some(());
                }
                self.said.curved += usize::from(curved(&base));
                let mut line = self.positions(g)?;
                if attribute(g, "orientation").map(str::trim) == Some("-") {
                    line.reverse();
                }
                self.got.lines.push(line);
            }
            "Polygon" | "Triangle" | "Rectangle" | "PolygonPatch" => self.face(g)?,
            name @ ("Surface" | "PolyhedralSurface" | "TriangulatedSurface" | "Tin") => {
                let patches: Vec<Node> =
                    elements(*g).filter(|c| matches!(local(c), "patches" | "polygonPatches" | "trianglePatches")).collect();
                if patches.is_empty() {
                    // A TIN given by its control points only, to be triangulated: not read.
                    self.counts.unknown.insert(format!("{name} without patches"));
                }
                for patch in patches.into_iter().flat_map(elements) {
                    self.geometry(&patch)?;
                }
            }
            "OrientableSurface" | "TexturedSurface" => {
                let base = elements(*g).find(|c| local(c) == "baseSurface")?;
                self.member(&base)?;
            }
            "CompositeSurface" | "MultiSurface" | "Shell" => self.members(g, &["surfaceMember"], "surfaceMembers")?,
            "Solid" => {
                // A solid with no exterior shell (its cavities only) lacks the part it needs.
                let exterior = elements(*g).find(|c| local(c) == "exterior")?;
                self.got.solid = true;
                if only_link(&exterior) {
                    // Its cavities are taken off that shell: read alone they would be drawn as
                    // footprints, so they are read only once the shell is found.
                    let cavities = elements(*g).filter(|c| local(c) == "interior").collect();
                    self.link(&exterior, cavities);
                } else {
                    self.member(&exterior)?;
                    self.members(g, &["interior"], "")?;
                }
            }
            "CompositeSolid" | "MultiSolid" => self.members(g, &["solidMember"], "solidMembers")?,
            "MultiGeometry" | "GeometricComplex" => self.members(g, &["geometryMember", "element"], "geometryMembers")?,
            "ImplicitGeometry" => {
                // Placed by its matrix it is its prototype's shape; here it is its reference point.
                let at = elements(*g).find(|c| local(c) == "referencePoint")?;
                let point = elements(at).next()?;
                self.instance = true;
                let p = self.positions(&point);
                self.instance = false;
                self.got.points.push((*p?.first()?, true));
            }
            other => {
                self.counts.unknown.insert(other.chars().take(64).collect());
            }
        }
        Some(())
    }

    /// The geometry a member property holds: written in place, or a link, followed later.
    fn member(&mut self, m: &Node<'a, 'i>) -> Option<()> {
        if only_link(m) {
            self.link(m, Vec::new());
            return Some(());
        }
        elements(*m).try_for_each(|g| self.geometry(&g))
    }

    /// An aggregate's members: properties named in `one` hold a geometry each, `many` several.
    fn members(&mut self, g: &Node<'a, 'i>, one: &[&str], many: &str) -> Option<()> {
        for c in elements(*g) {
            if one.contains(&local(&c)) {
                self.member(&c)?;
            } else if local(&c) == many {
                elements(c).try_for_each(|m| self.geometry(&m))?;
            }
        }
        Some(())
    }

    /// A polygon's face: its exterior and then its holes, each a ring of positions. A ring given by a
    /// link isn't read: a face whose exterior is one is left out, a hole that is one is left out of
    /// its face.
    fn face(&mut self, g: &Node) -> Option<()> {
        let exterior = elements(*g).find(|c| matches!(local(c), "exterior" | "outerBoundaryIs"))?;
        let interiors = elements(*g).filter(|c| matches!(local(c), "interior" | "innerBoundaryIs"));
        let bounds: Vec<Node> = std::iter::once(exterior).chain(interiors).collect();
        // Curves a ring is carried in: any segments other than straight lines are read through
        // their points, and said like a standalone Curve's are.
        for b in &bounds {
            if let Some(ring) = elements(*b).next() {
                self.said.curved += usize::from(curved(&ring));
            }
        }
        let mut rings = Vec::new();
        for (k, b) in bounds.into_iter().enumerate() {
            if only_link(&b) {
                self.said.links += 1;
                if k == 0 {
                    return Some(());
                }
                continue;
            }
            let ring = elements(b).next()?;
            // A ring given by a link, or with a curve or point given by one: the ring isn't whole,
            // and is left out as one (a hole alone, its face kept).
            if only_link(&ring) || has_linked_part(&ring) {
                self.said.links += 1;
                if k == 0 {
                    return Some(());
                }
                continue;
            }
            rings.push(self.positions(&ring)?);
        }
        self.got.surface = true;
        self.got.faces.face(rings, self.at.ground);
        Some(())
    }

    /// The positions an element gives, in the order written: its `gml:posList`s, `gml:pos`es and
    /// `gml:coordinates`. `None` when one isn't numbers, not as many as its dimension asks for, or
    /// none is left (an arc given by its centre gives no points on the curve).
    fn positions(&mut self, n: &Node) -> Option<Vec<P3>> {
        Some(self.tagged(n)?.into_iter().map(|(p, _)| p).collect())
    }

    /// The same positions, each with whether it was given with a height (a 3D position).
    fn tagged(&mut self, n: &Node) -> Option<Vec<(P3, bool)>> {
        let mut out = Vec::new();
        // A curve given in the opposite direction (`orientation="-"`) is read and reversed whole.
        let mut reversed: Option<Node> = None;
        for c in n.descendants().filter(Node::is_element) {
            if reversed.is_some_and(|r| c.ancestors().any(|a| a == r)) {
                continue;
            }
            // The centre an arc is given by (`ArcByCenterPoint`, `CircleByCenterPoint`) isn't on
            // the curve: its positions aren't read, and an arc by its centre alone is left with
            // none, so its geometry is left out.
            if matches!(local(&c), "pos" | "posList" | "coordinates" | "coord")
                && c.ancestors().take_while(|a| a != n).any(|a| matches!(local(&a), "ArcByCenterPoint" | "CircleByCenterPoint"))
            {
                continue;
            }
            if c != *n && local(&c) == "OrientableCurve" && attribute(&c, "orientation").map(str::trim) == Some("-") {
                let outer = self.dim;
                let mut between = c.ancestors().skip(1).take_while(|a| a != n).chain(std::iter::once(*n));
                if let Some(d) = between.find_map(|a| attribute(&a, "srsDimension")) {
                    self.dim = Some(d.trim().parse().ok()?);
                }
                let curve = self.tagged(&c);
                self.dim = outer;
                out.extend(curve?.into_iter().rev());
                reversed = Some(c);
                continue;
            }
            let (values, dim): (Vec<f64>, usize) = match local(&c) {
                "pos" | "posList" => {
                    let values: Vec<f64> = text(&c).split_ascii_whitespace().map(|v| v.parse().ok()).collect::<Option<_>>()?;
                    // Its own `srsDimension`, or one between it and `n`, or the geometry's. Without
                    // one, CityGML is 3D, and a single position is as long as it is written.
                    let own = c.ancestors().take_while(|a| a != n).chain(std::iter::once(*n)).find_map(|a| attribute(&a, "srsDimension"));
                    let dim = match own {
                        Some(d) => d.trim().parse().ok()?,
                        None if local(&c) == "pos" && (2..=3).contains(&values.len()) => self.dim.unwrap_or(values.len()),
                        None => self.dim.unwrap_or(3),
                    };
                    (values, dim)
                }
                "coordinates" => {
                    // Tuples split by `ts` (blanks by default), their values by `cs` (a comma).
                    let cs = attribute(&c, "cs").unwrap_or(",");
                    let ts = attribute(&c, "ts").unwrap_or(" ");
                    let decimal = attribute(&c, "decimal").filter(|d| *d != "." && !d.is_empty());
                    let number = |t: &str| match decimal {
                        Some(d) => t.trim().replace(d, ".").parse::<f64>().ok(),
                        None => t.trim().parse::<f64>().ok(),
                    };
                    let written = text(&c);
                    let tuples = written.split(|ch: char| if ts == " " { ch.is_whitespace() } else { ts.contains(ch) });
                    let (mut values, mut dim) = (Vec::new(), None);
                    for tuple in tuples.filter(|t| !t.trim().is_empty()) {
                        let v: Vec<f64> = tuple.split(cs).map(number).collect::<Option<_>>()?;
                        // Every tuple as long as the first.
                        if *dim.get_or_insert(v.len()) != v.len() {
                            return None;
                        }
                        values.extend(v);
                    }
                    (values, dim.unwrap_or(3))
                }
                // GML 2's position, its ordinates as elements.
                "coord" => {
                    let get = |k: &str| elements(c).find(|x| local(x) == k).map(|x| text(&x).trim().parse::<f64>().ok());
                    let mut values = vec![get("X").flatten()?, get("Y").flatten()?];
                    if let Some(z) = get("Z") {
                        values.push(z?);
                    }
                    let dim = values.len();
                    (values, dim)
                }
                _ => continue,
            };
            if !(2..=3).contains(&dim) || !values.len().is_multiple_of(dim) {
                return None;
            }
            for v in values.chunks(dim) {
                let (a, b) = (v.first().copied()?, v.get(1).copied()?);
                let (x, y) = if self.swap { (b, a) } else { (a, b) };
                let p = [x, y, v.get(2).copied().unwrap_or(0.0)];
                self.reach.add(&p);
                let has_height = dim == 3 && !self.instance;
                if has_height {
                    self.got.heights.push(p);
                }
                out.push((p, has_height));
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

#[cfg(test)]
mod tests;
