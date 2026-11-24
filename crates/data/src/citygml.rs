//! CityGML 1.0, 2.0 and 3.0 (OGC 08-007r1, 12-019 and 21-006), read as 2D like every city model: each city
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
//! their place, which reads them all and says how many named another. Heights are kept as the
//! file has them: a compound URN's vertical part that isn't a height in metres (feet, a depth, or
//! one RustGIS doesn't know) is said, each one the file names once, the system given or not; a
//! vertical part named says the heights' unit in place of the horizontal system's.
//!
//! 3.0 is read by the same names where they stay (its geometry properties, `cityObjectMember`), and
//! by its own where they change: thematic surfaces in `boundary`, openings in `fillingSurface`, a
//! prototype in `relativeGeometry`, parts linked by `buildingPart` (and a bridge's or tunnel's),
//! generic attributes named by a `gen:name`, and a group member inside its `Role`. Its spaces (a
//! `BuildingRoom`, a `Storey`) are objects like any other. Those names are read only in 3.0's
//! namespaces ([`in_3_0`]): an extension of a 1.0 or 2.0 model that uses one keeps its own meaning.
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
//! A geometry given by a link (`xlink:href`, `#` and a `gml:id`), as a solid's faces are to its
//! walls' and roof's polygons, is read where the link stands, after what its object writes out, and
//! links a ground surface makes first: a geometry the object holds already at that level of detail
//! isn't read again, but as the ground when a ground surface links to one read as another face, and
//! through links each one is read at most once per object and level, so a cycle of links ends. One
//! written in another member is found by the index, which notes where each element whose `gml:id`
//! a link names lies: it is copied, with the namespaces and dimension around it, into a
//! document parsed beside the member, and the links in it are followed in turn. What a member's
//! links read is bounded by the member's size, and what links copy and read from other members by
//! the file's. A geometry that can't be read is said once, whichever object or link reads it first.
//!
//! An implicit geometry (`ImplicitGeometry`) is its prototype ([`PROTOTYPES`], written in
//! place or given by a link), placed by its matrix at its reference point (OGC 12-019 §8.3), in the
//! file's axis order: the prototype is read once, in its own system, and placed for each instance,
//! the points placed in all as many as the file could write out.
//!
//! A part, a group's member or a group's parent given by a link (`consistsOfBuildingPart`,
//! `buildingPart`, `bridgePart`, `tunnelPart`, `groupMember`, a `CityObjectGroup`'s `parent`) makes the objects it names a child and its parent,
//! as if one were written in the other, once every member is read; the link is kept as an attribute
//! too.
//!
//! Not read, and said: geometries given by a link to another file or nothing, to what isn't a
//! geometry, or to one where this reader draws nothing (an appearance's, a terrain intersection's);
//! a prototype's geometry linked from outside its implicit geometry, and links inside a prototype;
//! thematic surfaces and openings given only by a link; rings, a line's or ring's points and curves,
//! and an oriented curve's base curve given by one; terrain intersection curves; an implicit
//! geometry whose prototype is in another file (`libraryObject`) or whose matrix isn't 16 numbers
//! with `0 0 0 1` last (drawn at its reference point); a group member's role; appearances,
//! addresses, TIN reliefs, and the properties of thematic surfaces and openings (a name, generic
//! attributes); 3.0's relations between objects (`CityObjectRelation`) and its versions
//! (`versionMember`, `versionTransitionMember`); curve arcs and splines are read as straight lines.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::rc::Rc;

use quick_xml::Reader;
use quick_xml::errors::SyntaxError;
use quick_xml::events::{BytesStart, Event};
use roxmltree::{Node, ParsingOptions};
use serde_json::{Map, Value as Json};

use crate::citymodel::*;
use crate::footprint::P3;
use crate::gml::{horizontal, lat_first, local, vertical};
use crate::raster::Crs;
use crate::{DataError, Dataset, ReadReport, Result};

/// How much of a file is looked at to recognize CityGML: its root's start tag, namespaces included.
pub const HEAD_BYTES: usize = 64 * 1024;

/// How many bytes of namespace declarations are copied around the members in all, at least: a
/// root declaring huge ones, with many members, would have them copied and parsed again for each.
const WRAP_BUDGET: usize = 1 << 30;

/// How many times over its own size, in bytes, a member's links may read, and the file's links copy
/// and read from other members: a geometry a link points to is read once per object that links to
/// it, so many objects linking to one large geometry (a long `posList` as much as many elements)
/// can't make reading grow as their number times its size.
const LINK_READS: usize = 4;

/// The namespace every CityGML version's core namespace starts with.
const CITYGML: &str = "http://www.opengis.net/citygml/";

/// CityGML 3.0's core namespace.
const CITYGML_3: &str = "http://www.opengis.net/citygml/3.0";

/// The CityGML versions read, by their core namespace.
const VERSIONS: [&str; 3] = ["http://www.opengis.net/citygml/1.0", "http://www.opengis.net/citygml/2.0", CITYGML_3];

/// The properties whose features are surfaces of their object (thematic surfaces, openings, traffic
/// areas), not objects of their own.
const SURFACE_PROPERTIES: [&str; 4] = ["boundedBy", "opening", "trafficArea", "auxiliaryTrafficArea"];

/// 3.0's names for them, read only in its namespaces ([`in_3_0`]): a thematic surface's
/// `boundary`, an opening's `fillingSurface`.
const SURFACE_PROPERTIES_3: [&str; 2] = ["boundary", "fillingSurface"];

/// The property holding an implicit geometry's prototype: `relativeGMLGeometry` in 1.0 and 2.0,
/// `relativeGeometry` in 3.0 (read only there, see [`in_3_0`]).
const PROTOTYPES: [&str; 2] = ["relativeGMLGeometry", "relativeGeometry"];

/// 3.0's properties linking a part to its object, which 1.0 and 2.0 call `consistsOf…` (read only
/// in its namespaces, see [`in_3_0`]).
const PARTS: [&str; 3] = ["buildingPart", "bridgePart", "tunnelPart"];

/// Whether an element is in one of CityGML 3.0's namespaces. The names 3.0 adds (`boundary`,
/// `fillingSurface`, `relativeGeometry`, `buildingPart`, `genericAttribute`, `relatedTo`, a group
/// member's `Role`) are read only there: an extension (ADE) of a 1.0 or 2.0 model may give one of
/// them to a property of its own, which keeps the meaning it had.
fn in_3_0(n: &Node) -> bool {
    n.tag_name().namespace().is_some_and(|ns| ns.starts_with(CITYGML) && ns.ends_with("/3.0"))
}

/// Whether a property's features are surfaces of its object ([`SURFACE_PROPERTIES`],
/// [`SURFACE_PROPERTIES_3`]).
fn surface_property(p: &Node) -> bool {
    let name = local(p);
    SURFACE_PROPERTIES.contains(&name) || (SURFACE_PROPERTIES_3.contains(&name) && in_3_0(p))
}

/// Whether a property holds an implicit geometry's prototype ([`PROTOTYPES`]), by its name and, in
/// the index, which doesn't resolve namespaces, whether the model is 3.0 (`three`).
fn prototype_name(name: &str, three: bool) -> bool {
    let [old, new] = PROTOTYPES;
    name == old || (three && name == new)
}

/// Whether an element holds an implicit geometry's prototype ([`PROTOTYPES`]).
fn prototype_property(n: &Node) -> bool {
    prototype_name(local(n), in_3_0(n))
}

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
/// namespaces. Other GML, a WFS answer that only
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
    let max = crate::io::max_bytes();
    if bytes.len() as u64 > max {
        return Err(DataError::Format(crate::io::file_size_error(file_name, max)));
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
    // What links copy from other members and read there is bounded by the file's size, as what a
    // member's links read is by the member's; the points implicit geometries place, by as many as
    // the file could write out ([`POINT_BYTES`]).
    let (link_bytes, placing) = (text.len().saturating_mul(LINK_READS), text.len() / POINT_BYTES);
    let mut reading = Reading { budget: MAX_FILE_FACES, swap, link_bytes, placing, ..Reading::default() };
    reading.said.appearances = index.appearances;
    reading.said.versions = index.versions;
    reading.said.links = index.links;
    reading.said.repeated_geometry_ids = index.repeated;
    let (mut wrapped, mut far) = (String::new(), Vec::new());
    let mut wrap_left = text.len().saturating_mul(4).max(WRAP_BUDGET);
    // The namespace work parsing the members and their copies takes is bounded by the file's size, as
    // one document's is: each wrapper repeats the root's declarations, which cost their square.
    let mut work_left = crate::xml::namespace_budget(text.len());
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
        reading.wrap_at = (start, wrapped.len() - member.len() - close.len() - "</m>".len());
        let Ok(doc) = crate::xml::parse_within(&wrapped, options(), &mut work_left) else {
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
        // The geometries its links point to in other members, copied and parsed beside it, each on
        // its own: one that can't be parsed leaves the links to it pointing to nothing, and said so.
        let copied = if linked {
            copy_far(&index, text, (start..end, doc.root_element()), &reading.prototypes, &mut reading.link_bytes, &mut far)
        } else {
            far.clear();
            Copied::default()
        };
        // Each copy, with where in the file its geometry is written and where in the copy it starts:
        // what an element of it is known by ([`Reading::place_of`]).
        // The member and its copies, all alive at once, hold together no more nodes than one member
        // may: the copies share what the member's own document leaves. One past that isn't parsed,
        // as one that can't be.
        let mut nodes = if far.is_empty() {
            0
        } else {
            crate::io::XML_NODES.saturating_sub(u32::try_from(doc.descendants().count()).unwrap_or(u32::MAX))
        };
        let far_docs: Vec<_> = far
            .iter()
            .filter_map(|(at, copy)| {
                let doc = crate::xml::parse_within(copy, ParsingOptions { nodes_limit: nodes, ..options() }, &mut work_left).ok()?;
                nodes = nodes.saturating_sub(u32::try_from(doc.descendants().count()).unwrap_or(u32::MAX));
                Some((*at, doc))
            })
            .collect();
        reading.copies = far_docs
            .iter()
            .map(|(at, d)| {
                let from = elements(d.root_element()).next().and_then(|w| elements(w).next()).map_or(0, |g| g.range().start);
                (std::ptr::from_ref(d).addr(), (*at, from))
            })
            .collect();
        let mut targets = Targets::of(doc.root_element(), far_docs.iter().map(|(_, d)| d.root_element()), linked, copied);
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
    let Reading { counts, mut built, rows, reach, mut said, relations, .. } = reading;
    said.object_links += relate(&mut built, relations);
    let mut notes = crate::loss::Notes::new();
    let crs = coordinate_system(file_name, given, named, index.vertical_named, &reach, &mut notes)?;
    if let Some(other) = &index.other {
        notes.push(format!(
            "{} geometries or envelopes name a coordinate system other than the file's first ({other}), and are read in {}, the coordinate system given",
            index.others,
            crs.label()
        ));
    }
    notes.extend(index.verticals.iter().filter_map(|(_, v)| vertical_note(v)));
    if index.verticals_past > 0 {
        notes.push(format!(
            "{} geometries or envelopes name a vertical system, past the first {VERTICALS} said, that isn't a height in metres: their heights are kept as the file has them",
            index.verticals_past
        ));
    }
    notes.extend(said.notes(index.cut));
    finish(file_name, stem, rows, built, counts, notes, crs)
}

/// Most vertical systems a file's `srsName`s name that are said one by one, so crafted names can't
/// make the notices grow with their number: past them, the `srsName`s naming another are counted.
const VERTICALS: usize = 8;

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
    /// The vertical parts compound `srsName`s name that aren't heights in metres, each once by its
    /// EPSG code (by its name when it names none), at most [`VERTICALS`]: the horizontal part alone
    /// decides whether names agree, so each is said for itself. Past them, the `srsName`s naming
    /// another are counted (`verticals_past`).
    verticals: Vec<(Option<u32>, String)>,
    verticals_past: usize,
    /// Whether an `srsName` names a vertical part, in metres or not: its unit is the heights', so
    /// the horizontal system's isn't said as theirs.
    vertical_named: bool,
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
    /// The versions the model holds (3.0), which aren't read.
    versions: usize,
    /// Whether the model is CityGML 3.0, whose own names the index reads only then.
    three: bool,
    /// Members given only by a link that the index alone meets (an empty `featureMembers`), which
    /// aren't followed.
    links: usize,
    /// The file ends before its root closes: what came after is left out.
    cut: bool,
    /// The ids the file's links name (`#` and a `gml:id`): only elements with one of them are
    /// indexed.
    named: HashSet<String>,
    /// The elements of its members whose `gml:id` a link names, by id, the first of an id given
    /// twice: where a link from another member finds what it points to.
    spots: HashMap<Box<str>, Spot>,
    /// Geometries a link names whose `gml:id` an earlier element of a member has too.
    repeated: usize,
    /// The namespace declarations around the elements in `spots`, written as attributes: each set
    /// once, shared by the elements it stands around.
    contexts: Vec<String>,
}

/// Where an element with a `gml:id` lies in the text, what it is, and what its text alone loses of
/// where it stands.
struct Spot {
    start: usize,
    /// None while it is open, for one the file is cut inside, and for one whose declarations
    /// couldn't be kept ([`Scopes::left`]): a link to it finds nothing.
    end: Option<usize>,
    kind: Kind,
    /// The namespaces declared around it in its member, the innermost of a prefix declared twice:
    /// its place in [`Index::contexts`].
    declarations: usize,
    /// The `srsDimension` its nearest ancestor in its member gives, written as an attribute.
    dim: Option<Box<str>>,
}

/// What a link finds at a `gml:id`.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A geometry a link reads where it stands.
    Geometry,
    /// A geometry in an implicit geometry's prototype ([`PROTOTYPES`]), read only placed.
    Prototype,
    /// Anything else: an object, a surface, a ring, or a geometry where this reader draws nothing.
    Other,
}

/// An element open in a member, while the file has links.
struct Open {
    depth: usize,
    /// Its `gml:id`, when it is the first element with it: its end is noted when it closes.
    id: Option<Box<str>>,
    /// The namespaces declared where its children stand: its place in [`Scopes::stack`], which it
    /// took itself (`own`, given back when it closes) when it declares some.
    scope: usize,
    own: bool,
    /// The `srsDimension` its children stand in, written as an attribute.
    dim: Option<Box<str>>,
    /// It is in a prototype, or where this reader draws nothing (its own name included).
    prototype: bool,
    undrawn: bool,
}

/// The namespaces declared inside the root around the elements being indexed, a set for each
/// element that declares some, nested as they are.
struct Scopes {
    stack: Vec<Scope>,
    /// Bytes the sets may still take, copied as they nest and written out for the elements in them:
    /// past it, a link to an element in a set that couldn't be made finds nothing, and is said, so
    /// crafted declarations can't make indexing grow as their number times their size.
    left: usize,
}

/// One set of namespaces declared inside the root.
#[derive(Default)]
struct Scope {
    /// Each by its key, with its attribute as written and whether it binds GML's namespace.
    pairs: Vec<(String, String, bool)>,
    /// The prefixes bound to GML's namespace, the root's among them, at most [`GML_PREFIXES`]:
    /// shared, as most sets keep their parent's.
    gml: Rc<[String]>,
    /// Its place in [`Index::contexts`], once an element with a `gml:id` in it is noted.
    context: Option<usize>,
    /// Past what the sets may take: an element in it can't be copied.
    spent: bool,
    /// What its pairs and GML prefixes take, in bytes: what a set nested in it copies.
    bytes: usize,
}

impl Scopes {
    /// The set an element's own declarations make inside `parent`'s, or none when it makes none.
    fn within(&mut self, parent: usize, e: &BytesStart) -> Option<Scope> {
        let mine: Vec<(String, String, bool)> = e
            .attributes()
            .flatten()
            .filter(|a| a.key.into_inner() == "xmlns" || a.key.into_inner().starts_with("xmlns:"))
            .map(|a| {
                let key = a.key.into_inner().to_string();
                let written = written_attribute(&key, &a.value);
                let ns = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or(a.value);
                (key, written, gml_namespace(ns.trim()))
            })
            .collect();
        if mine.is_empty() {
            return None;
        }
        let around = self.stack.get(parent)?;
        let size = |pairs: &[(String, String, bool)]| pairs.iter().map(|(k, w, _)| k.len() + w.len()).sum::<usize>();
        // Known before its parent's pairs are looked at: an element in a spent set, or past the bound,
        // costs only its own declarations, however many its parent's set holds.
        let cost = around.bytes.saturating_add(size(&mine));
        if around.spent || cost > self.left {
            // Its ids are still found by the root's prefixes; what is in it can't be copied.
            let gml = self.stack.first().map_or_else(|| Rc::from([]), |r| Rc::clone(&r.gml));
            return Some(Scope { gml, spent: true, ..Scope::default() });
        }
        self.left -= cost;
        let mut pairs = around.pairs.clone();
        // Each key's place among the pairs, so a set of many declarations is merged in their number,
        // not its square.
        let mut place: HashMap<String, usize> = pairs.iter().enumerate().map(|(k, (key, _, _))| (key.clone(), k)).collect();
        let mut gml: Vec<String> =
            around.gml.iter().filter(|g| !mine.iter().any(|(k, _, _)| k.strip_prefix("xmlns:") == Some(g.as_str()))).cloned().collect();
        for (key, written, is_gml) in mine {
            if is_gml
                && gml.len() < GML_PREFIXES
                && let Some(prefix) = key.strip_prefix("xmlns:")
            {
                gml.push(prefix.to_string());
            }
            match place.get(&key).and_then(|&k| pairs.get_mut(k)) {
                Some(pair) => *pair = (key, written, is_gml),
                None => {
                    place.insert(key.clone(), pairs.len());
                    pairs.push((key, written, is_gml));
                }
            }
        }
        let bytes = size(&pairs) + gml.iter().map(String::len).sum::<usize>();
        Some(Scope { pairs, gml: gml.into(), context: None, spent: false, bytes })
    }

    /// The place in `contexts` of a set's declarations, written once for all the elements in it;
    /// none past what the sets may take.
    fn context(&mut self, at: usize, contexts: &mut Vec<String>) -> Option<usize> {
        let scope = self.stack.get_mut(at)?;
        if scope.context.is_some() || scope.spent {
            return scope.context;
        }
        let written: String = scope.pairs.iter().map(|(_, w, _)| w.as_str()).collect();
        if written.len() > self.left {
            scope.spent = true;
            return None;
        }
        self.left -= written.len();
        contexts.push(written);
        scope.context = Some(contexts.len() - 1);
        scope.context
    }
}

/// An `srsDimension` as written, for the elements a copy wraps. A value no dimension is that long
/// is kept as one that can't be read, as it can't where it is written.
fn written_dim(e: &BytesStart) -> Option<Box<str>> {
    let a = e.attributes().flatten().find(|a| a.key.into_inner() == "srsDimension")?;
    let value = if a.value.len() <= 16 { &*a.value } else { "?" };
    Some(written_attribute("srsDimension", value).into())
}

/// Whether the `<!` markup error at `at` is the text ending at `<!` (a cut), not markup that `<!`
/// starts and nothing knows, earlier in the text.
fn bang_at_end(text: &str, at: u64) -> bool {
    // The error is placed at the `<` of the markup.
    usize::try_from(at).ok().and_then(|at| text.get(at..)).is_some_and(|rest| rest.trim_end() == "<!")
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

/// An attribute as written, quoted again with a quote its value doesn't hold, after a space.
fn written_attribute(key: &str, value: &str) -> String {
    let quote = if value.contains('"') { '\'' } else { '"' };
    format!(" {key}={quote}{value}{quote}")
}

/// Most prefixes for GML's namespace an element's `gml:id` is looked for by, while indexing: past
/// them, an id under another isn't indexed, and a link to it from another member finds nothing, so
/// crafted declarations can't make every element's look grow with their number.
const GML_PREFIXES: usize = 8;

/// The prefixes an element declares for GML's namespace, at most [`GML_PREFIXES`].
fn gml_prefixes(e: &BytesStart) -> Vec<String> {
    e.attributes()
        .flatten()
        .filter_map(|a| {
            let prefix = a.key.into_inner().strip_prefix("xmlns:")?;
            let ns = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or(a.value);
            gml_namespace(ns.trim()).then(|| prefix.to_string())
        })
        .take(GML_PREFIXES)
        .collect()
}

/// An element's `gml:id`, by the prefixes bound to GML where it stands, trimmed; a blank one is none.
fn start_id(e: &BytesStart, gml: &[String]) -> Option<String> {
    let a = e.attributes().flatten().find(|a| a.key.into_inner().strip_suffix(":id").is_some_and(|p| gml.iter().any(|g| g == p)))?;
    let value = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or(a.value);
    Some(value.trim().to_string()).filter(|id| !id.is_empty())
}

/// Whether a name is one a link where a geometry belongs may point to: a GML geometry, a solid's
/// shell, a surface's patch, an oriented curve, or an implicit geometry.
fn linkable_name(name: &str) -> bool {
    GEOMETRIES.contains(&name) || matches!(name, "Shell" | "PolygonPatch" | "OrientableCurve" | "GeometricComplex" | "ImplicitGeometry")
}

/// Whether an element holds what this reader doesn't draw: an appearance, a terrain intersection
/// curve, a TIN relief. A link to a geometry in one isn't followed.
fn undrawn(name: &str) -> bool {
    matches!(name, "appearance" | "appearanceMember" | "Appearance" | "TINRelief") || terrain_intersection(name)
}

/// Whether a property is a terrain intersection, where an object meets the ground: an auxiliary
/// line, not the object's shape (`lod2TerrainIntersection`, `lod2TerrainIntersectionCurve` in 3.0).
fn terrain_intersection(name: &str) -> bool {
    name.ends_with("TerrainIntersection") || name.ends_with("TerrainIntersectionCurve")
}

impl Index {
    /// Notes an element of a member, while the file has links: where it lies when it has a `gml:id`
    /// a link names, and what its children stand in. `base` is the set of namespaces the member
    /// stands in (its `featureMembers`'s, or none but the root's).
    fn spot(
        &mut self,
        e: &BytesStart,
        (start, end): (usize, usize),
        empty: bool,
        depth: usize,
        base: usize,
        scopes: &mut Scopes,
        open: &mut Vec<Open>,
    ) {
        // Nested too deep to parse, it is noted no further, nor its children: a link to it finds
        // nothing, and is said.
        if open.len() > crate::xml::MAX_DEPTH {
            return;
        }
        let name = e.local_name().into_inner();
        let (around, dim, prototype, hidden) =
            open.last().map_or((base, None, false, false), |o| (o.scope, o.dim.clone(), o.prototype, o.undrawn));
        let kind = match () {
            _ if hidden || undrawn(name) || !linkable_name(name) => Kind::Other,
            _ if prototype => Kind::Prototype,
            _ => Kind::Geometry,
        };
        // Its own declarations hold for its own `gml:id`, and for its children.
        let mine = scopes.within(around, e);
        let gml = mine.as_ref().or(scopes.stack.get(around)).map_or(&[][..], |s| &s.gml[..]);
        let mut first = None;
        if let Some(id) = start_id(e, gml).filter(|id| self.named.contains(id)) {
            match self.spots.entry(id.into()) {
                Entry::Vacant(v) => {
                    // What its text alone loses: the declarations around it and the dimension its
                    // nearest ancestor gives. Past what declarations may take, it can't be copied:
                    // a link to it finds nothing.
                    let context = scopes.context(around, &mut self.contexts);
                    first = (context.is_some() && !empty).then(|| v.key().clone());
                    let end = context.and(empty.then_some(end));
                    v.insert(Spot { start, end, kind, declarations: context.unwrap_or_default(), dim: dim.clone() });
                }
                Entry::Occupied(_) => self.repeated += usize::from(kind != Kind::Other),
            }
        }
        if empty {
            return;
        }
        let own = mine.is_some();
        if let Some(scope) = mine {
            scopes.stack.push(scope);
        }
        open.push(Open {
            depth,
            id: first,
            scope: if own { scopes.stack.len() - 1 } else { around },
            own,
            dim: written_dim(e).or(dim),
            prototype: prototype || prototype_name(name, self.three),
            undrawn: hidden || undrawn(name),
        });
    }

    /// Indexes a document in one pass, building no tree. Refuses one whose root isn't a CityGML 1.0,
    /// 2.0 or 3.0 city model, and, unless `overridden` (a system is given in their place), one whose
    /// `srsName`s name different systems; `archive` for a member of one.
    fn of(file_name: &str, text: &str, overridden: bool, archive: bool) -> Result<Index> {
        let invalid = |e: quick_xml::Error| DataError::Format(format!("{file_name}: {e}"));
        let mut reader = Reader::from_str(text);
        // The ids links name, by a scan of their own before the index's: a link may come before what
        // it points to.
        // Not looking at Cancel: one pass over the text, as the index's is.
        let named = if text.contains("href") { hrefs_in(text) } else { HashSet::new() };
        let mut index = Index { overridden, archive, named, ..Index::default() };
        // While the file has links: the namespaces declared inside the root (the root's own are
        // written on every copy), the set the members being read stand in (`base`, their
        // `featureMembers`'s), and the elements open in a member.
        let mut scopes = Scopes { stack: Vec::new(), left: text.len() };
        let (mut base, mut open) = (0, Vec::<Open>::new());
        // The depth of the prototype being read ([`PROTOTYPES`]): its system is its own, not
        // the file's.
        let mut prototype: Option<usize> = None;
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
                // The `<!` must be the one the text ends at, not one earlier in it.
                Err(quick_xml::Error::Syntax(e))
                    if !matches!(e, SyntaxError::InvalidBangMarkup) || bang_at_end(text, reader.error_position()) =>
                {
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
                    if prototype == Some(depth) {
                        prototype = None;
                    }
                    if let Some(closed) = open.pop_if(|o| o.depth == depth) {
                        if closed.own {
                            scopes.stack.pop();
                        }
                        if let Some(spot) = closed.id.and_then(|id| index.spots.get_mut(id.as_ref())) {
                            spot.end = Some(position(&reader));
                        }
                    }
                    if let Some((start, _)) = member.filter(|&(_, at)| at == depth) {
                        member = None;
                        index.members.push((start, position(&reader), holder));
                    } else if let Some(s) = child_start.filter(|_| depth == 2) {
                        child_start = None;
                        finished.push((s, position(&reader)));
                    }
                    if depth == 1 {
                        holder = None;
                        scopes.stack.truncate(1);
                        base = 0;
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
                index.three = root_namespace(e).as_deref().map(str::trim) == Some(CITYGML_3);
                index.namespaces = declarations(e);
                let gml = gml_prefixes(e);
                let bytes = gml.iter().map(String::len).sum();
                scopes.stack.push(Scope { gml: gml.into(), bytes, ..Scope::default() });
            }
            if prototype.is_none()
                && let Some(srs) = start_attribute(e, "srsName").filter(|s| !s.trim().is_empty())
            {
                index.see_system(file_name, srs.trim())?;
            }
            if prototype.is_none() && !empty && prototype_name(e.local_name().into_inner(), index.three) {
                prototype = Some(depth);
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
                        let tag = text.get(start..position(&reader)).unwrap_or_default().to_string();
                        index.holders.push((tag, format!("</{}>", e.name().into_inner())));
                        holder = Some(index.holders.len() - 1);
                        if let Some(scope) = scopes.within(0, e) {
                            scopes.stack.push(scope);
                            base = scopes.stack.len() - 1;
                        }
                    }
                    "appearanceMember" => index.appearances += 1,
                    "versionMember" | "versionTransitionMember" => index.versions += 1,
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
            if !index.named.is_empty() && member.is_some() {
                index.spot(e, (start, position(&reader)), empty, depth, base, &mut scopes, &mut open);
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
    /// either way: the order a name gives is how its coordinates are read. Vertical parts don't
    /// decide whether names agree; each one named is kept, to be said.
    fn see_system(&mut self, file_name: &str, srs: &str) -> Result<()> {
        if self.system.as_deref() == Some(srs) {
            return Ok(());
        }
        let named = vertical(srs);
        self.vertical_named |= named.is_some();
        // A part with an EPSG code is the same system as one with the same code, however spelled;
        // one with none, as one of the same name.
        if let Some((v, code)) = named.and_then(|v| Some((v, vertical_code(v)?)))
            && !self.verticals.iter().any(|(c, seen)| *c == code && (code.is_some() || seen == v))
        {
            if self.verticals.len() < VERTICALS {
                self.verticals.push((code, v.to_string()));
            } else {
                self.verticals_past += 1;
            }
        }
        let Some(first) = &self.system else {
            self.system = Some(srs.to_string());
            return Ok(());
        };
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
            // One RustGIS doesn't know may be the same system as the other: said as unknown.
            (a, b) if a.is_none() || b.is_none() => {
                let (unknown, other) = if a.is_none() { (first.as_str(), srs) } else { (srs, first.as_str()) };
                let both = if a.is_none() && b.is_none() { "two coordinate systems" } else { "a coordinate system" };
                let then = if self.archive {
                    "which aren't read together in a model in an archive"
                } else {
                    "which aren't read together; give the coordinate system (crs) to read it in one"
                };
                Err(DataError::Unsupported(format!(
                    "{file_name}: it names {both} RustGIS doesn't know ({} beside {}), {then}",
                    cut(unknown),
                    cut(other)
                )))
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

/// Refuses a root that isn't a CityGML 1.0, 2.0 or 3.0 city model.
fn check_root(file_name: &str, e: &BytesStart) -> Result<()> {
    if e.local_name().into_inner() != "CityModel" {
        return Err(DataError::Format(format!("{file_name}: not a CityGML city model (its root isn't CityModel)")));
    }
    match root_namespace(e).as_deref().map(str::trim) {
        Some(ns) if VERSIONS.contains(&ns) => Ok(()),
        _ => Err(DataError::Format(format!("{file_name}: not a CityGML 1.0, 2.0 or 3.0 city model (its root's namespace isn't one)"))),
    }
}

/// The namespaces an element declares, written out as attributes again, each after a space.
fn declarations(e: &BytesStart) -> String {
    let mut out = String::new();
    for a in e.attributes().flatten() {
        let key = a.key.into_inner();
        if key == "xmlns" || key.starts_with("xmlns:") {
            // As written, so entities stay as they were.
            out.push_str(&written_attribute(key, &a.value));
        }
    }
    out
}

/// How a member, or a copy of another member's geometry, is parsed: no DTD, and no more nodes than
/// any XML the app reads.
fn options<'i>() -> ParsingOptions<'i> {
    ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() }
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
/// a solid's shell, a surface's patch, an oriented curve, or an implicit geometry (placed as one
/// written out is). Not an envelope, a ring's curve or a segment.
fn linkable(n: &Node) -> bool {
    local(n) == "ImplicitGeometry" || (is_gml(n) && linkable_name(local(n)))
}

/// Whether an element stands where this reader draws nothing ([`undrawn`]): a link to it isn't
/// followed.
fn undrawn_at(n: &Node) -> bool {
    n.ancestors().skip(1).any(|a| undrawn(local(&a)))
}

/// Whether an element is in an implicit geometry's prototype, in its own system: only its implicit
/// geometries place it.
fn in_prototype(n: &Node) -> bool {
    n.ancestors().skip(1).any(|a| prototype_property(&a))
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
                !terrain_intersection(local(&p))
                    && (property_lod(&p).is_some()
                        || elements(p).next().is_some_and(|g| is_geometry(&g))
                        || (surface_property(&p) && elements(p).any(|s| !is_gml(&s)))
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
    // 3.0 names them with a capital (`gen:IntAttribute`), and wraps each of a set's in a
    // `genericAttribute`.
    let kind = local(a);
    if kind.eq_ignore_ascii_case("genericAttributeSet") {
        let mut set = Map::new();
        for member in elements(*a) {
            let member = if local(&member) == "genericAttribute" { elements(member).next() } else { Some(member) };
            if let Some((member, name)) = member.and_then(|m| generic_name(&m).map(|n| (m, n))) {
                insert(&mut set, &name, generic_value(&member));
            }
        }
        return Json::Object(set);
    }
    let value = elements(*a).find(|v| local(v) == "value");
    let text = value.as_ref().map(text).unwrap_or_default();
    let text = text.trim();
    let number = if kind.eq_ignore_ascii_case("intAttribute") {
        text.parse::<i64>().ok().map(Json::from)
    } else if kind.eq_ignore_ascii_case("doubleAttribute") {
        measure(text, None)
    } else if kind.eq_ignore_ascii_case("measureAttribute") {
        measure(text, value.as_ref().and_then(|v| attribute(v, "uom")).or_else(|| attribute(a, "uom")))
    } else {
        None
    };
    number.unwrap_or_else(|| Json::from(text))
}

/// A generic attribute's name: an attribute of it in 1.0 and 2.0 (kept as written), its `gen:name`
/// in 3.0 (none when blank).
fn generic_name(a: &Node) -> Option<String> {
    if let Some(name) = attribute(a, "name") {
        return Some(name.to_string());
    }
    elements(*a).find(|n| local(n) == "name").map(|n| text(&n).trim().to_string()).filter(|n| !n.is_empty())
}

/// 3.0's group member: a `groupMember` holding a `Role`, which holds the member (a `groupMember`)
/// and its role (`role`). Gives the member, and whether the role is given.
fn group_role<'a, 'i>(p: &Node<'a, 'i>) -> Option<(bool, Node<'a, 'i>)> {
    if local(p) != "groupMember" || !in_3_0(p) {
        return None;
    }
    let role = elements(*p).next().filter(|r| local(r) == "Role")?;
    let member = elements(role).find(|m| local(m) == "groupMember")?;
    Some((elements(role).any(|r| local(&r) == "role" && !text(&r).trim().is_empty()), member))
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
    /// Geometries whose coordinates can't be read, or that lack a part they need: each written one
    /// once, whichever object or link reads it first, a solid whose exterior is a link to one, and
    /// a solid with cavities whose exterior has nothing read and no notice says why.
    bad_coordinates: usize,
    /// Geometries and objects given only by a link that isn't followed: a geometry's to another
    /// file, nothing, or a member that can't be parsed; a thematic surface's or opening's, a ring's,
    /// a line's or ring's point or curve's, an oriented curve's base curve's, a member's, and any in
    /// a prototype.
    links: usize,
    /// Links where a geometry belongs to an element that isn't one, or to a geometry this reader
    /// doesn't read there: one where it draws nothing (an appearance's, a terrain intersection's) or
    /// a prototype's outside its implicit geometry.
    not_geometry_links: usize,
    /// Links to geometries not followed because the member's links, or the file's, had read what
    /// they may.
    links_past_limit: usize,
    /// Objects with no id, holding parts that can't name them as their parent.
    anonymous: usize,
    /// Objects whose id another object has too.
    repeated_ids: usize,
    /// Geometries whose `gml:id`, which a link names, an earlier element of a member has too.
    repeated_geometry_ids: usize,
    /// Links that would make one object another's part (`consistsOfBuildingPart`, 3.0's
    /// `buildingPart`, `bridgePart` and `tunnelPart`, `groupMember`, a group's `parent`) naming no other object of the file, or made by an object with no id.
    object_links: usize,
    /// Group members with a role in their group (`role`), which isn't kept.
    group_roles: usize,
    /// 3.0's relations between city objects (`relatedTo`, a `CityObjectRelation`), not read.
    object_relations: usize,
    /// 3.0's versions of the model (`versionMember`, `versionTransitionMember`), not read.
    versions: usize,
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
    /// Sets of generic attributes naming a code list (`codeSpace`), which isn't kept.
    set_code_spaces: usize,
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
    /// The notices that say why a geometry is left out: what can't be read, and links not followed.
    /// Counted before and after reading a solid's exterior, or following a link of it, to know
    /// whether its loss is said.
    fn noticed(&self) -> usize {
        self.bad_coordinates + self.links + self.not_geometry_links + self.links_past_limit
    }

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
                "{} links (xlink:href) where a geometry belongs point to an element that isn't a geometry, or to a geometry where nothing is drawn (a terrain intersection, an appearance) or that only an implicit geometry places, and are left out",
                self.not_geometry_links
            ),
        );
        say(
            self.links_past_limit,
            format!(
                "{} links (xlink:href) to geometries aren't followed, as they would read their city object member, or the file, over many times: what they point to is left out where they stand",
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
                "{} geometries have a gml:id an earlier element of the file has too: a link to that id reads the one in its city object member, or else the first",
                self.repeated_geometry_ids
            ),
        );
        say(
            self.object_links,
            format!(
                "{} links between city objects (consistsOfBuildingPart, buildingPart, bridgePart, tunnelPart, groupMember, a group's parent) name no other city object of the file, or are made by one with no gml:id: the link is kept as an attribute, but no parent is",
                self.object_links
            ),
        );
        say(self.group_roles, format!("{} members of groups (groupMember) have a role in their group, which isn't kept", self.group_roles));
        say(
            self.object_relations,
            format!("{} relations between city objects (CityObjectRelation) aren't read and are left out", self.object_relations),
        );
        say(
            self.versions,
            format!("the file's versions (versionMember, versionTransitionMember) aren't read: {} are left out", self.versions),
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
            self.set_code_spaces,
            format!(
                "{} sets of generic attributes (genericAttributeSet) name a code list (codeSpace), which isn't kept",
                self.set_code_spaces
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
            format!(
                "{} generic attributes (genericAttribute, or a member of a genericAttributeSet) have no name, and are left out",
                self.unnamed_generic
            ),
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

/// The elements a member's links can point to, by `gml:id`, and how many bytes of it its links may
/// still read.
struct Targets<'a, 'i> {
    /// Each with whether it is a copy of another member's, whose reading the file's budget pays for
    /// ([`Reading::link_bytes`]). The first of an id given twice, the member's own first.
    by_id: HashMap<&'a str, (Node<'a, 'i>, bool)>,
    budget: usize,
    /// Ids of other members' geometries left uncopied, the file's links having copied what they may.
    refused: HashSet<String>,
    /// Ids of other members' elements that aren't a geometry a link reads.
    elsewhere: HashSet<String>,
    /// Ids of other members' prototypes read already, by where they are written ([`Copied::read`]).
    prototypes: HashMap<String, usize>,
    /// For each geometry given only by a link, what its chain of such links ends at
    /// ([`Self::end_of`]).
    ends: HashMap<&'a str, Option<&'a str>>,
}

impl<'a, 'i> Targets<'a, 'i> {
    /// A member's elements by `gml:id`, then the elements copied from other members (`far`); none
    /// when the member holds no link.
    fn of(member: Node<'a, 'i>, far: impl Iterator<Item = Node<'a, 'i>>, linked: bool, copied: Copied) -> Self {
        let mut by_id = HashMap::new();
        if linked {
            let own = member.descendants().map(|n| (n, false));
            // Of a copy, only the element copied: it is what a link to its id names, as the first in
            // the file (an id inside it that a link names has a copy of its own).
            let copies = far.filter_map(|f| elements(f).next().and_then(|w| elements(w).next()));
            for (n, copy) in own.chain(copies.map(|n| (n, true))) {
                if let Some(id) = gml_id(&n) {
                    by_id.entry(id).or_insert((n, copy));
                }
            }
        }
        let Copied { refused, elsewhere, read } = copied;
        let budget = member.range().len().saturating_mul(LINK_READS);
        Targets { by_id, budget, refused, elsewhere, prototypes: read, ends: HashMap::new() }
    }

    /// What a geometry given only by a link stands for: the link its chain of such geometries ends
    /// with (the last one's), or none for a chain that comes back on itself. Found once in a member
    /// for each, so a long chain costs its length, not its square, though no look at Cancel is
    /// made while it is followed.
    fn end_of(&mut self, id: &'a str) -> Option<&'a str> {
        let (mut path, mut on, mut at) = (Vec::new(), HashSet::new(), id);
        let end = loop {
            if let Some(&known) = self.ends.get(at) {
                break known;
            }
            let Some(href) = self.by_id.get(at).and_then(|(n, _)| attribute(n, "href")) else { break None };
            path.push(at);
            on.insert(at);
            // The next stands for another link only when a link reads it as a geometry.
            let next = fragment(href)
                .and_then(|k| self.by_id.get_key_value(k))
                .filter(|(_, (n, _))| only_link(n) && linkable(n) && !undrawn_at(n) && !in_prototype(n));
            match next {
                Some((&k, _)) if on.contains(k) => break None,
                Some((&k, _)) => at = k,
                None => break Some(href),
            }
        };
        for k in path {
            self.ends.insert(k, end);
        }
        end
    }
}

/// What copying other members' geometries for a member's links left out.
#[derive(Default)]
struct Copied {
    refused: HashSet<String>,
    elsewhere: HashSet<String>,
    /// Ids of other members' prototypes read already ([`Reading::prototypes`]), by where they are
    /// written: placed from what was read, not copied again.
    read: HashMap<String, usize>,
}

/// The id a link to an element of the same file names (`#` and a `gml:id`), trimmed.
fn fragment(href: &str) -> Option<&str> {
    href.trim().strip_prefix('#').map(str::trim).filter(|id| !id.is_empty())
}

/// The ids the links in some text name, each once, by a scan that builds no tree.
fn hrefs_in(text: &str) -> HashSet<String> {
    let mut reader = Reader::from_str(text);
    let mut out = HashSet::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                if let Some(id) = start_attribute(&e, "href").as_deref().and_then(fragment) {
                    out.insert(id.to_string());
                }
            }
            Ok(Event::Eof) | Err(_) => return out,
            Ok(_) => {}
        }
    }
}

/// Copies into `out`, each as a document of its own, the geometries a member's links point to in
/// other members, and those their links point to in turn, so one that can't be parsed costs only
/// the links to it. Each is in an element that carries what its text alone loses of where it
/// stands: the namespaces declared around it, the `srsDimension` above it, and, for a prototype's,
/// a `relativeGMLGeometry` around it; each with where it is written in the file, so it is known as
/// the element it copies ([`Reading::place_of`]). An id the member gives an element of its own isn't
/// copied: its links read that one. Nor is a prototype read already (`read`, by where it is
/// written): many members placing one prototype read it once. What the file's links copy is
/// bounded by `left`, in bytes, and their trees, parsed together, by the nodes one member may hold
/// less those of the member's own document;
/// copying, and parsing the copies, don't look at Cancel, which is looked at between members: for
/// all members together they are bounded by that budget.
fn copy_far(
    index: &Index,
    text: &str,
    (member, root): (std::ops::Range<usize>, Node),
    read: &HashMap<usize, Option<(Got, usize)>>,
    left: &mut usize,
    out: &mut Vec<(usize, String)>,
) -> Copied {
    out.clear();
    let mut copied = Copied::default();
    let mut taken = HashSet::new();
    let mut queue: Vec<String> =
        root.descendants().filter_map(|n| attribute(&n, "href")).filter_map(fragment).map(str::to_string).collect();
    // Only a member linking to an id the index knows outside it copies anything.
    queue.retain(|id| index.spots.get(id.as_str()).is_some_and(|spot| !member.contains(&spot.start)));
    if queue.is_empty() {
        return copied;
    }
    let own: HashSet<&str> = root.descendants().filter_map(|n| gml_id(&n)).collect();
    while let Some(id) = queue.pop() {
        let Some(spot) = index.spots.get(id.as_str()) else { continue };
        if own.contains(id.as_str()) || member.contains(&spot.start) || !taken.insert(spot.start) {
            continue;
        }
        if spot.kind == Kind::Prototype && read.contains_key(&spot.start) {
            copied.read.insert(id, spot.start);
            continue;
        }
        let written = spot.end.and_then(|end| text.get(spot.start..end));
        let Some(written) = written.filter(|_| spot.kind != Kind::Other) else {
            // Not a geometry a link reads, or one the file is cut inside (as good as nothing).
            if spot.kind == Kind::Other {
                copied.elsewhere.insert(id);
            }
            continue;
        };
        let wrap = if spot.kind == Kind::Prototype { "relativeGMLGeometry" } else { "copied" };
        let dim = spot.dim.as_deref().unwrap_or_default();
        let declarations = index.contexts.get(spot.declarations).map_or("", String::as_str);
        let cost = written.len() + declarations.len() + dim.len() + 2 * wrap.len() + index.namespaces.len() + 10;
        if cost > *left {
            copied.refused.insert(id);
            continue;
        }
        *left -= cost;
        queue.extend(hrefs_in(written));
        out.push((spot.start, format!("<f{}><{wrap}{declarations}{dim}>{written}</{wrap}></f>", index.namespaces)));
    }
    copied
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

/// What a link points to: an element by its `gml:id`, or, for an implicit geometry whose prototype
/// is written in place, that prototype.
enum To<'a, 'i> {
    Href(&'a str),
    Node(Node<'a, 'i>),
}

/// A link to a geometry met while reading an object, followed once the object's own geometries
/// are read.
struct Pending<'a, 'i> {
    to: To<'a, 'i>,
    /// The solid whose exterior it stands in, when that solid has cavities ([`Links::waiting`]).
    wait: Option<usize>,
    at: At,
    /// It is a solid's exterior: the solid is lost with what it points to.
    solid: bool,
    /// An implicit geometry's: where its prototype is placed.
    place: Option<Placement>,
}

/// A geometry of an object, by where it is written ([`place_in`]) and the level of detail it is read
/// at.
type Key = (usize, Option<u32>);

/// Why a link isn't followed, each said in its own notice.
#[derive(Clone, Copy)]
enum Unread {
    /// To another file, nothing, or what the file is cut before.
    Missing,
    /// The member's or the file's links read what they may.
    PastLimit,
    /// To what isn't a geometry a link reads there.
    NotGeometry,
}

/// A solid's cavities, kept until its exterior has something read: read alone they would be drawn
/// as its faces.
enum Wait<'a, 'i> {
    /// Waiting, with the `srsDimension` the solid is written in, and whether a notice says why its
    /// exterior has nothing read yet (a link of it to nothing, or to what can't be read).
    Cavities(Vec<Node<'a, 'i>>, Option<usize>, bool),
    /// Taken to read with its exterior.
    Taken,
    /// A cavity can't be read: the solid is left out whole, its exterior's other links too.
    Lost,
}

/// One object's links: the geometries it has read, each with whether it was read as the ground, and
/// the links left to follow, the ground's apart.
#[derive(Default)]
struct Links<'a, 'i> {
    seen: HashMap<Key, bool>,
    /// What `seen` took in, in order, with what it held before, so a geometry left out can take its
    /// own marks back.
    marked: Vec<(Key, Option<bool>)>,
    ground: Vec<Pending<'a, 'i>>,
    others: Vec<Pending<'a, 'i>>,
    /// The solids whose exterior has had nothing read yet, each with its cavities: the links in that
    /// exterior name it ([`Pending::wait`]), and the first that reads something reads them. One still
    /// waiting once the object's links are followed is left out with its cavities, and said: by the
    /// notices of its links (to nothing, or to what can't be read; in a prototype, where they aren't
    /// followed), else as a solid that lacks the part it needs.
    waiting: Vec<Wait<'a, 'i>>,
}

impl<'a, 'i> Links<'a, 'i> {
    /// Keeps a solid's cavities, written in `dim`, until its exterior has something read.
    fn wait(&mut self, cavities: Vec<Node<'a, 'i>>, dim: Option<usize>) -> usize {
        self.waiting.push(Wait::Cavities(cavities, dim, false));
        self.waiting.len() - 1
    }

    /// Whether the solid `wait` names still waits for its exterior.
    fn waits(&self, wait: Option<usize>) -> bool {
        wait.and_then(|w| self.waiting.get(w)).is_some_and(|w| matches!(w, Wait::Cavities(..)))
    }

    /// Whether the solid `wait` names is left out.
    fn lost(&self, wait: Option<usize>) -> bool {
        wait.and_then(|w| self.waiting.get(w)).is_some_and(|w| matches!(w, Wait::Lost))
    }

    /// The cavities the solid `wait` names waits with, taken to read: none when it doesn't wait.
    fn take(&mut self, wait: Option<usize>) -> Option<(Vec<Node<'a, 'i>>, Option<usize>)> {
        let w = self.waiting.get_mut(wait?)?;
        match std::mem::replace(w, Wait::Taken) {
            Wait::Cavities(cavities, dim, _) => Some((cavities, dim)),
            other => {
                *w = other;
                None
            }
        }
    }

    /// Notes that a notice says why the exterior of the solid `wait` names has nothing read yet.
    fn explained(&mut self, wait: Option<usize>) {
        if let Some(Wait::Cavities(_, _, said)) = wait.and_then(|w| self.waiting.get_mut(w)) {
            *said = true;
        }
    }

    /// The solids still waiting that no notice explains: their exterior has nothing read and says
    /// nothing of why (an empty shell, written in place or linked).
    fn unexplained(&self) -> usize {
        self.waiting.iter().filter(|w| matches!(w, Wait::Cavities(_, _, false))).count()
    }

    /// Leaves out the solid `wait` names.
    fn lose(&mut self, wait: Option<usize>) {
        if let Some(w) = wait.and_then(|w| self.waiting.get_mut(w)) {
            *w = Wait::Lost;
        }
    }

    /// Whether a geometry needn't be read for `ground`: read already, and as the ground if that is
    /// what it is read for now.
    fn read(&self, key: Key, ground: bool) -> bool {
        self.seen.get(&key).is_some_and(|&was| was || !ground)
    }

    /// Marks a geometry read, as the ground or not; false when it needn't be read.
    fn mark(&mut self, key: Key, ground: bool) -> bool {
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

/// What a geometry being read is, so that one left out is said once: where it is written when it has a `gml:id`,
/// whether a link reads it (and so what fails may be the link's cavities rather than it), and
/// whether it is a solid's exterior given by a link (the solid is lost with it, and said).
#[derive(Clone, Copy)]
struct Root {
    id: Option<usize>,
    linked: bool,
    solid: bool,
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
    /// The geometries with a `gml:id` whose coordinates can't be read or that lack a part they need,
    /// in any member, and those holding one, by where they are written ([`Self::place_of`]): a link
    /// to one is left out without reading it again. By place, not by id: two geometries given one id
    /// are two.
    failed: HashSet<usize>,
    /// The broken ones said already, each the innermost with an id a failure ran through: each is
    /// said once, whichever link or object reads it first.
    counted: HashSet<usize>,
    /// Where the member being read starts in the file, and how many bytes its document puts before
    /// it: what [`Self::place_of`] takes an element of it back to the file by.
    wrap_at: (usize, usize),
    /// The copies of other members' geometries the member being read links to, by their document:
    /// where in the file what each copies is written, and where in the copy it starts.
    copies: HashMap<usize, (usize, usize)>,
    /// Bytes the file's links may still copy from other members and read there.
    link_bytes: usize,
    /// Points implicit geometries may still place, all their instances together.
    placing: usize,
    /// Prototypes read in their own system, by where they are written: none for one that can't be
    /// read. Each with the points placing it costs, counted once ([`Got::size`]).
    prototypes: HashMap<usize, Option<(Got, usize)>>,
    /// Links that make one object another's part (child, parent), resolved once every member is read.
    relations: Vec<(String, String)>,
}

impl Reading {
    /// Counts what a generic attribute's value carries that isn't kept: its code list
    /// (`codeSpace`), its other attributes (a language; a unit but on a `measureAttribute`), and a
    /// set's members with no name.
    fn said_generic(&mut self, a: &Node) {
        for v in a.descendants().filter(|n| n.is_element() && local(n) == "value") {
            let measure = v.parent_element().is_some_and(|p| local(&p).eq_ignore_ascii_case("measureAttribute"));
            self.said.code_spaces += usize::from(attribute(&v, "codeSpace").is_some());
            self.said.value_attributes +=
                usize::from(v.attributes().any(|at| !(at.name() == "codeSpace" || (measure && at.name() == "uom"))));
        }
        // A set's members with no name, and the code list a set names: 2.0 names a member by its
        // `name` and a set's list by its `codeSpace`, attributes; 3.0 by `gen:name` and
        // `gen:codeSpace` children, each member in a `genericAttribute`.
        for set in a.descendants().filter(|n| n.is_element() && local(n).eq_ignore_ascii_case("genericAttributeSet")) {
            self.said.set_code_spaces +=
                usize::from(attribute(&set, "codeSpace").is_some() || elements(set).any(|c| local(&c) == "codeSpace"));
            for member in elements(set).filter(|m| !matches!(local(m), "name" | "codeSpace")) {
                let member = if local(&member) == "genericAttribute" { elements(member).next() } else { Some(member) };
                self.said.unnamed_generic += usize::from(member.is_none_or(|m| generic_name(&m).is_none()));
            }
        }
    }

    /// Reads a city object into its rows, then the objects nested in it. Its links are followed
    /// after its own geometries, with no look at Cancel: what they read is bounded by the member's
    /// size ([`LINK_READS`]) and the file's, as parsing the member is, and Cancel is looked at
    /// between members. A link that makes another object its part, or makes it a group's part, is
    /// kept to resolve once every member is read.
    fn object<'a, 'i>(&mut self, e: Node<'a, 'i>, parent: Option<&str>, targets: &mut Targets<'a, 'i>) {
        let ty = local(&e);
        if ty == "TINRelief" {
            self.counts.tin_reliefs += 1;
            return;
        }
        let id = gml_id(&e);
        let mut parts = ObjectParts::default();
        let mut links = Links::default();
        let mut attributes = Map::new();
        let mut children = Vec::new();
        // The objects a group names as its parent, written in place: read as objects of their own.
        let mut heads = Vec::new();
        let mut address = false;
        for p in elements(e) {
            // A group member's role in its group isn't kept; 3.0 writes the member inside it.
            let p = match group_role(&p) {
                Some((role, member)) => {
                    self.said.group_roles += usize::from(role);
                    member
                }
                None => p,
            };
            let name = local(&p);
            let first = elements(p).next();
            self.said.group_roles += usize::from(name == "groupMember" && attribute(&p, "role").is_some());
            if (name.ends_with("Attribute") || name == "genericAttributeSet")
                && let Some(key) = attribute(&p, "name")
            {
                self.said_generic(&p);
                insert(&mut attributes, key, generic_value(&p));
            } else if name == "genericAttribute" && in_3_0(&p) {
                // 3.0's: the attribute inside, named by its `gen:name`.
                for a in elements(p) {
                    match generic_name(&a) {
                        Some(key) => {
                            self.said_generic(&a);
                            insert(&mut attributes, &key, generic_value(&a));
                        }
                        None => self.said.unnamed_generic += 1,
                    }
                }
            } else if name == "relatedTo" && in_3_0(&p) && first.is_some_and(|f| local(&f) == "CityObjectRelation") {
                self.said.object_relations += 1;
            } else if terrain_intersection(name) {
                // Where the object meets the ground: an auxiliary line, not the object's shape.
                self.said.terrain_intersections += 1;
            } else if let Some(rank) = property_lod(&p) {
                self.geometry(&p, At { lod: Some(rank), ground: false }, &mut parts, &mut links);
            } else if name == "address" {
                address = true;
            } else if name == "appearance" {
                self.said.appearances += 1;
            } else if only_link(&p) && surface_property(&p) {
                self.said.links += 1;
            } else if let Some(href) = attribute(&p, "href").filter(|_| first.is_none()) {
                // A relation to another feature (a group's members, a generalization), kept as
                // written. A part, a group's member and a group's parent also make the objects they
                // link a child and its parent, as if written one in the other.
                let other = fragment(href);
                let relation = match name {
                    _ if name.starts_with("consistsOf") || (PARTS.contains(&name) && in_3_0(&p)) || name == "groupMember" => {
                        Some((other, id))
                    }
                    "parent" if ty == "CityObjectGroup" => Some((id, other)),
                    _ => None,
                };
                match relation {
                    Some((Some(child), Some(parent))) => self.relations.push((child.to_string(), parent.to_string())),
                    Some(_) => self.said.object_links += 1,
                    None => {}
                }
                insert(&mut attributes, name, Json::from(href.trim()));
            } else if let Some(first) = first.filter(|f| VALUES.contains(&local(f))) {
                // An envelope (its system is read with the others), or what is said apart.
                self.said.appearances += usize::from(local(&first) == "Appearance");
                address |= local(&first) == "Address";
            } else if first.is_some_and(|f| is_gml(&f)) {
                // A geometry in a property that names no level of detail (a group's, a relief's).
                self.geometry(&p, At { lod: None, ground: false }, &mut parts, &mut links);
            } else if first.is_some_and(|f| surface_property(&p) && local(&f).starts_with(char::is_uppercase)) {
                elements(p).for_each(|s| self.surface(&s, &mut parts, &mut links));
            } else if elements(p).any(|c| is_object(&c)) {
                // The objects; a feature beside them with no id and no geometry is said. A group's
                // parent is the group's parent, not its child.
                let up = name == "parent" && ty == "CityObjectGroup";
                for c in elements(p) {
                    match () {
                        _ if !is_object(&c) => self.said.not_objects += 1,
                        _ if up => heads.push(c),
                        _ => children.push(c),
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
            let (wait, before) = (link.wait, self.said.noticed());
            self.follow(link, targets, &mut parts, &mut links);
            if self.said.noticed() > before {
                links.explained(wait);
            }
        }
        // A solid whose exterior has nothing read, and no notice says why: it lacks the part it
        // needs, and is left out with its cavities.
        self.said.bad_coordinates += links.unexplained();
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
            ground: None,
            solid: made.solid,
            attributes: attributes.into_iter().collect(),
        });
        for child in children {
            self.object(child, id, targets);
        }
        for head in heads {
            match (id, gml_id(&head)) {
                (Some(group), Some(parent)) => self.relations.push((group.to_string(), parent.to_string())),
                _ => self.said.object_links += 1,
            }
            self.object(head, None, targets);
        }
    }

    /// The element a link points to, in its member or copied from another (`far`), or why it isn't
    /// followed.
    fn find<'a, 'i>(href: &'a str, targets: &Targets<'a, 'i>) -> std::result::Result<(&'a str, Node<'a, 'i>, bool), Unread> {
        let id = fragment(href).ok_or(Unread::Missing)?;
        match targets.by_id.get(id) {
            Some(&(target, far)) => Ok((id, target, far)),
            None if targets.refused.contains(id) => Err(Unread::PastLimit),
            // Another member's element that isn't a geometry a link reads there, or a prototype's
            // read already, which only its implicit geometries place.
            None if targets.elsewhere.contains(id) || targets.prototypes.contains_key(id) => Err(Unread::NotGeometry),
            None => Err(Unread::Missing),
        }
    }

    /// Pays for reading what a link points to: from the member's budget, or, for another member's,
    /// the file's, as copying it was. False, and said, when what is left doesn't cover it.
    fn pay(&mut self, targets: &mut Targets, far: bool, cost: usize) -> bool {
        let left = if far { &mut self.link_bytes } else { &mut targets.budget };
        if cost > *left {
            self.unread(Unread::PastLimit);
            return false;
        }
        *left -= cost;
        true
    }

    /// Says a link that isn't followed.
    fn unread(&mut self, why: Unread) {
        match why {
            Unread::Missing => self.said.links += 1,
            Unread::PastLimit => self.said.links_past_limit += 1,
            Unread::NotGeometry => self.said.not_geometry_links += 1,
        }
    }

    /// Reads the geometry a link points to, in its member or another, as if written where the link
    /// stands. One an implicit geometry points to is its prototype, placed ([`Self::instance`]).
    fn follow<'a, 'i>(&mut self, link: Pending<'a, 'i>, targets: &mut Targets<'a, 'i>, parts: &mut ObjectParts, links: &mut Links<'a, 'i>) {
        if let Some(place) = link.place {
            self.instance(link, place, targets, parts, links);
            return;
        }
        let Pending { to: To::Href(href), wait, at, solid, .. } = link else { return };
        // In the exterior of a solid left out (a cavity can't be read): it goes with the solid,
        // said once.
        if links.lost(wait) {
            return;
        }
        let (id, target, far) = match Self::find(href, targets) {
            Ok(found) => found,
            Err(why) => return self.unread(why),
        };
        // Not a geometry, one where this reader draws nothing (an appearance's), or a prototype,
        // which only its implicit geometry places.
        if !linkable(&target) || undrawn_at(&target) || in_prototype(&target) {
            return self.unread(Unread::NotGeometry);
        }
        let place = self.place_of(&target);
        if self.failed.contains(&place) {
            // Said where it failed. A solid whose exterior it is lacks the part it needs, and is left
            // out with its cavities: said too. One whose surface it is is said by it, however many
            // links read it.
            self.said.bad_coordinates += usize::from(solid);
            links.explained(wait);
            return;
        }
        // Read already in this object at this level of detail, written out or by another link:
        // nothing to add. One given only by a link stands for what its chain of such links ends at
        // ([`Targets::end_of`]), so a link it couldn't follow is said for this one too, as is one
        // whose chain comes back on itself.
        let key = (place, at.lod);
        let read = links.read(key, at.ground);
        let waits = links.waits(wait);
        // A link in the exterior of a solid whose cavities wait, to what is itself only a link:
        // they wait for the end of that chain, so a chain that ends in nothing leaves the solid out
        // with them, as an exterior link to nothing does. So too when the link was read already at
        // this level (a surface holds it): that read was the link's, not the shell's.
        if (read || waits) && only_link(&target) {
            match targets.end_of(id) {
                Some(end) => links.push(Pending { to: To::Href(end), wait, at, solid, place: None }),
                None => self.unread(Unread::Missing),
            }
            return;
        }
        if read && !waits {
            return;
        }
        // Its bytes: what reading it parses, its elements and its coordinates' text.
        if !self.pay(targets, far, if read { 0 } else { target.range().len() }) {
            return;
        }
        // Whether the solid's cavities were taken to read in this walk: when it fails, that solid is
        // left out.
        let mut took = false;
        let got = self.read(
            |w| {
                // Marks are made as it is read, inside the walk, so a walk that fails (a cavity that
                // can't be read) takes them back, and the target is read again by its next link.
                w.following = true;
                // What it holds given by links stands in the same exterior.
                w.wait = wait;
                if !read {
                    w.where_written(&target)?;
                    w.geometry(&target)?;
                }
                // The exterior has something read (or read already): its solid's cavities are read
                // with it, written where the solid is, in its dimension. One that holds only links
                // leaves them to those links. Read already means marked at this level, not that it
                // drew: a composite of links to nothing that another exterior of the object read
                // first takes them too (those links are said), as does one whose links come back to
                // it in a cycle with nothing drawn (not said). Only broken files make that so.
                if (read || w.drawn > 0)
                    && let Some((cavities, dim)) = w.links.take(wait)
                {
                    took = true;
                    w.wait = None;
                    w.dim = dim;
                    cavities.iter().try_for_each(|cavity| w.member(cavity))?;
                }
                Some(())
            },
            at,
            links,
            Root { id: Some(place), linked: true, solid },
        );
        match got {
            Some(got) => self.add(parts, at, got),
            None if took => links.lose(wait),
            None => {}
        }
    }

    /// Places an implicit geometry's prototype as its matrix says, at its reference point: read once
    /// in its own system, for every instance that links to it, and placed for each. An instance
    /// whose prototype can't be found, read or placed is drawn at its reference point, as one whose
    /// prototype is in another file is (said with them).
    fn instance<'a, 'i>(
        &mut self,
        link: Pending<'a, 'i>,
        place: Placement,
        targets: &mut Targets<'a, 'i>,
        parts: &mut ObjectParts,
        links: &mut Links<'a, 'i>,
    ) {
        let Pending { to, at, .. } = link;
        let reference = |parts: &mut ObjectParts| {
            parts.points.add_points(at.rank(), || at.lod.map(|l| l.to_string()), vec![(place.reference(), true)]);
        };
        // The prototype, by where it is written: one read in place and through a link is one, and two
        // given one id are two. One in another member read already isn't copied again: it is known
        // by its place alone.
        let (key, shape, far, linked) = match to {
            To::Node(shape) => (self.place_of(&shape), Some(shape), false, false),
            To::Href(href) => match Self::find(href, targets) {
                // A geometry of a prototype: not one an object or a surface writes out.
                Ok((_, shape, far)) if linkable(&shape) && local(&shape) != "ImplicitGeometry" && in_prototype(&shape) => {
                    (self.place_of(&shape), Some(shape), far, true)
                }
                Ok(_) => {
                    self.unread(Unread::NotGeometry);
                    return reference(parts);
                }
                Err(why) => match fragment(href).and_then(|id| targets.prototypes.get(id)) {
                    Some(&at) => (at, None, true, true),
                    _ => {
                        self.unread(why);
                        return reference(parts);
                    }
                },
            },
        };
        let placed = match (self.prototypes.get(&key), shape) {
            (Some(known), _) => known.as_ref().and_then(|(g, size)| g.placed(*size, &place, &mut self.placing)),
            // Known by its place only when it was read: never here.
            (None, None) => None,
            (None, Some(shape)) => {
                let id = gml_id(&shape).map(|_| key);
                // Read where it is written, paid for once, as a link's target is.
                if !self.pay(targets, far, if linked { shape.range().len() } else { 0 }) {
                    return reference(parts);
                }
                let got = self.read(
                    |w| {
                        w.local = true;
                        w.where_written(&shape)?;
                        w.geometry(&shape)
                    },
                    at,
                    links,
                    Root { id, linked, solid: false },
                );
                // Its rings of no area are said once, not for each instance.
                self.counts.dropped_rings += got.as_ref().map_or(0, |g| g.faces.dropped_rings);
                let got = got.map(|g| {
                    let size = g.size();
                    (g, size)
                });
                let placed = got.as_ref().and_then(|(g, size)| g.placed(*size, &place, &mut self.placing));
                self.prototypes.insert(key, got);
                placed
            }
        };
        match placed {
            Some(placed) => {
                placed.reach(&mut self.reach);
                self.add(parts, at, placed);
            }
            None => reference(parts),
        }
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
            } else if local(&p) == "opening" || (local(&p) == "fillingSurface" && in_3_0(&p)) {
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
            links.push(Pending { to: To::Href(href), wait: None, at, solid: false, place: None });
            return;
        }
        let Some(g) = elements(*p).next() else { return };
        if let Some(got) =
            self.read(|w| w.geometry(&g), at, links, Root { id: gml_id(&g).map(|_| self.place_of(&g)), linked: false, solid: false })
        {
            self.add(parts, at, got);
        }
    }

    /// Reads a geometry with `go`, at `at`. The links it meets are left to follow after the
    /// object's own geometries, unless it is left out, when they go with it: then it leaves nothing
    /// behind (no mark, no point in the file's reach, no notice of its parts) and is said once, as
    /// `root` says.
    fn read<'a, 'i>(
        &mut self,
        go: impl FnOnce(&mut Walk<'_, 'a, 'i>) -> Option<()>,
        at: At,
        links: &mut Links<'a, 'i>,
        root: Root,
    ) -> Option<Got> {
        let (reach, ground, others, marked, waiting) =
            (self.reach, links.ground.len(), links.others.len(), links.marked.len(), links.waiting.len());
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
            local: false,
            wait: None,
            drawn: 0,
            failed: Vec::new(),
            wrap_at: self.wrap_at,
            copies: &self.copies,
            got: Got::default(),
            links,
        };
        let done = go(&mut walk);
        let Walk { got, failed, .. } = walk;
        if done.is_some() {
            return Some(got);
        }
        self.said = said;
        self.reach = reach;
        links.ground.truncate(ground);
        links.others.truncate(others);
        // And the solids it held, which only its links waited on.
        links.waiting.truncate(waiting);
        // Its geometries weren't read: a link to one of them still reads it.
        links.unmark(marked);
        // Said once. What a link points to failed: the geometry that can't be read is the innermost
        // with an id the failure ran through, said by whichever object or link reads it first (with
        // none, the one written here), and a solid whose exterior it is too. Else what the link
        // reads where it stands failed (a solid's cavity): that solid is lost.
        let failed: Vec<usize> = failed.iter().map(|n| self.place_of(n)).collect();
        let lost = if root.linked && !root.id.is_some_and(|r| failed.contains(&r)) {
            1
        } else {
            let broken = failed.first().map_or(1, |&at| usize::from(self.counted.insert(at)));
            broken + usize::from(root.solid)
        };
        self.said.bad_coordinates += lost;
        self.failed.extend(failed);
        None
    }

    /// Where an element is written in the file, by its first byte: in the member being read, or in
    /// another member when it is in a copy ([`copy_far`]).
    fn place_of(&self, n: &Node) -> usize {
        place_in(n, self.wrap_at, &self.copies)
    }

    /// A geometry read into the object's parts at `at`.
    fn add(&mut self, parts: &mut ObjectParts, at: At, got: Got) {
        let Got { faces, surface, mut lines, points, heights, solid } = got;
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

/// Makes each object a link names as another's part (or a group's member, or a group's parent) a
/// child of that other, as if written in it: its heights then go to it. Gives the links that name
/// no other object of the file (nothing, another file, the object itself): no parent is made
/// (the link stays an attribute). A part named by an id two objects have is the first's.
fn relate(built: &mut [Built], relations: Vec<(String, String)>) -> usize {
    let mut unresolved = 0;
    let parents: Vec<(usize, String)> = {
        let mut at: HashMap<&str, usize> = HashMap::new();
        for (i, b) in built.iter().enumerate().filter(|(_, b)| !b.id.is_empty()) {
            at.entry(b.id.as_str()).or_insert(i);
        }
        relations
            .into_iter()
            .filter_map(|(child, parent)| {
                let found = at.get(child.as_str()).copied().filter(|_| child != parent && at.contains_key(parent.as_str()));
                unresolved += usize::from(found.is_none());
                found.map(|c| (c, parent))
            })
            .collect()
    };
    // A parent given twice (written around it and linked, or linked twice) is one: each child's
    // parents are told apart once, so many links to one object cost their number, not its square.
    let (mut given, mut met) = (HashSet::<(usize, String)>::new(), HashSet::new());
    for (c, parent) in parents {
        let Some(b) = built.get_mut(c) else { continue };
        if met.insert(c) {
            given.extend(b.parent_ids.iter().map(|p| (c, p.clone())));
        }
        if given.insert((c, parent.clone())) {
            b.parent_ids.push(parent);
        }
    }
    unresolved
}

/// Where an element is written in the file ([`Reading::place_of`]): `wrap_at` for the member being
/// read, `copies` for those of other members' geometries.
fn place_in(n: &Node, wrap_at: (usize, usize), copies: &HashMap<usize, (usize, usize)>) -> usize {
    let r = n.range().start;
    match copies.get(&std::ptr::from_ref(n.document()).addr()) {
        Some(&(at, from)) => at.saturating_add(r).saturating_sub(from),
        None => wrap_at.0.saturating_add(r).saturating_sub(wrap_at.1),
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

impl Got {
    /// A prototype read in its own system, placed; none when it holds nothing to place, the
    /// instances' points left to place (`placing`) don't cover it, or a point placed isn't a finite
    /// number. Its rings of no area were
    /// left out and said when it was read; those its placing flattens (a matrix that scales an axis
    /// to nothing) are left out the same way, in `dropped_rings`, which [`Reading::add`] says for
    /// each instance.
    /// `size` is what placing it costs ([`Self::size`]), counted once when it is read, so an
    /// instance past the budget costs no walk through its rings.
    fn placed(&self, size: usize, place: &Placement, placing: &mut usize) -> Option<Got> {
        let f = &self.faces;
        // Nothing to place (a prototype whose surfaces are all links, which it doesn't follow): its
        // instance is its reference point, and said.
        if f.rings().next().is_none() && self.lines.is_empty() && self.points.is_empty() {
            return None;
        }
        if size > *placing {
            return None;
        }
        *placing -= size;
        let ring = |r: &Vec<P3>| r.iter().map(|p| place.place(p)).collect::<Option<Vec<P3>>>();
        let rings = |rs: &Vec<Vec<P3>>| rs.iter().map(ring).collect::<Option<Vec<_>>>();
        let mut faces = Faces {
            left_out: rings(&f.left_out)?,
            truncated: f.truncated,
            ground_left_out: rings(&f.ground_left_out)?,
            ..Faces::default()
        };
        for (face, ground) in f.faces.iter().zip(&f.ground) {
            faces.face(rings(face)?, *ground);
        }
        Some(Got {
            faces,
            surface: self.surface,
            lines: rings(&self.lines)?,
            points: self.points.iter().map(|(p, reference)| Some((place.place(p)?, *reference))).collect::<Option<_>>()?,
            heights: self.heights.iter().map(|p| place.place(p)).collect::<Option<_>>()?,
            solid: self.solid,
        })
    }

    /// The points placing it costs: those of its rings and lines, its points and its heights.
    fn size(&self) -> usize {
        self.faces.rings().chain(&self.lines).map(Vec::len).sum::<usize>() + self.points.len() + self.heights.len()
    }

    /// Takes its points into the file's reach.
    fn reach(&self, reach: &mut Reach) {
        self.faces.rings().chain(&self.lines).flatten().chain(self.points.iter().map(|(p, _)| p)).for_each(|p| reach.add(p));
    }
}

/// A walk through a geometry's elements. `None` when coordinates can't be read or a part a geometry
/// needs is missing: the whole geometry is left out.
struct Walk<'r, 'a, 'i> {
    counts: &'r mut Counts,
    said: &'r mut Said,
    reach: &'r mut Reach,
    swap: bool,
    /// What tells where an element is written ([`place_in`]): a geometry is marked read by its
    /// place, so two given one id are two.
    wrap_at: (usize, usize),
    copies: &'r HashMap<usize, (usize, usize)>,
    /// Where its geometry goes in its object: its level of detail, and whether its faces are a
    /// `GroundSurface`'s.
    at: At,
    /// The `srsDimension` a geometry it is in gives.
    dim: Option<usize>,
    /// Reading an implicit geometry's reference point.
    instance: bool,
    /// Reading what a link points to: a geometry its object has read at this level isn't read again.
    following: bool,
    /// Reading a prototype in its own system: its coordinates aren't the file's (not turned
    /// latitude first, not in its reach), no mark is made, and its links aren't followed.
    local: bool,
    /// The solid whose exterior is being read, when its cavities wait ([`Links::waiting`]): the
    /// links met there name it.
    wait: Option<usize>,
    /// The faces, lines and points read, and the geometries met read already: whether what it
    /// read has something of its own, not only links.
    drawn: usize,
    /// The geometries with a `gml:id` a failure ran through, from the innermost out: each is lost.
    failed: Vec<Node<'a, 'i>>,
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
        if !self.local
            && gml_id(g).is_some()
            && !self.links.mark((place_in(g, self.wrap_at, self.copies), self.at.lod), self.at.ground)
            && self.following
        {
            self.drawn += 1;
            return Some(());
        }
        // A geometry given by a link is followed once its object's own are read.
        if only_link(g) {
            self.link(g, false);
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
        // Each geometry with an id the failure runs through is lost, and a link to it isn't read
        // again. Its parts that didn't fail aren't: a link to one still reads it.
        if done.is_none() && gml_id(g).is_some() {
            self.failed.push(*g);
        }
        done
    }

    /// Takes the dimension of the geometries an element is written in, for one read where something
    /// else stands (a link's target, a prototype). One that can't be read fails, and the element with
    /// it, when it has a `gml:id`.
    fn where_written(&mut self, g: &Node<'a, 'i>) -> Option<()> {
        if let Some(d) = g.ancestors().skip(1).find_map(|a| attribute(&a, "srsDimension")) {
            let Ok(d) = d.trim().parse() else {
                if gml_id(g).is_some() {
                    self.failed.push(*g);
                }
                return None;
            };
            self.dim = Some(d);
        }
        Some(())
    }

    /// Keeps a link to a geometry to follow, in the exterior of the solid whose cavities wait, if
    /// any; `solid` when it is that exterior. In a prototype, links aren't followed, and are said.
    fn link(&mut self, n: &Node<'a, 'i>, solid: bool) {
        let Some(href) = attribute(n, "href") else { return };
        if self.local {
            self.said.links += 1;
            return;
        }
        self.links.push(Pending { to: To::Href(href), wait: self.wait, at: self.at, solid, place: None });
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
                self.drawn += 1;
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
                self.drawn += 1;
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
                self.drawn += 1;
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
                // Its cavities are read once its exterior has something read: read alone they
                // would be drawn as its faces. An exterior given by links (the shell, or each of its
                // surfaces) leaves them to the first of those links that reads something; when none
                // does, the solid is left out with them, and the links are said.
                let cavities: Vec<Node> = elements(*g).filter(|c| local(c) == "interior").collect();
                let outer = self.wait;
                let own = (!cavities.is_empty()).then(|| self.links.wait(cavities, self.dim));
                self.wait = own.or(outer);
                let (drawn, before) = (self.drawn, self.said.noticed());
                let done = if only_link(&exterior) {
                    self.link(&exterior, true);
                    Some(())
                } else {
                    self.member(&exterior)
                };
                self.wait = outer;
                done?;
                if self.drawn > drawn {
                    if let Some((cavities, _)) = self.links.take(own) {
                        // Its cavities aren't the outer solid's exterior, as where a link reads them.
                        let held = self.wait.take();
                        let read = cavities.iter().try_for_each(|cavity| self.member(cavity));
                        self.wait = held;
                        read?;
                    }
                } else if self.said.noticed() > before {
                    // Nothing read, and a link of it said (a ring's, or any in a prototype, which
                    // aren't followed): that notice says why the solid is left out.
                    self.links.explained(own);
                }
            }
            "CompositeSolid" | "MultiSolid" => self.members(g, &["solidMember"], "solidMembers")?,
            "MultiGeometry" | "GeometricComplex" => self.members(g, &["geometryMember", "element"], "geometryMembers")?,
            "ImplicitGeometry" => self.implicit(g)?,
            other => {
                self.counts.unknown.insert(other.chars().take(64).collect());
            }
        }
        Some(())
    }

    /// An implicit geometry: its prototype ([`PROTOTYPES`]), written in place or given by a
    /// link, placed by its matrix at its reference point once its object's own geometries are read.
    /// One whose prototype is in another file (`libraryObject`), or whose matrix isn't 16 numbers
    /// with `0 0 0 1` last, is its reference point, with no height.
    fn implicit(&mut self, g: &Node<'a, 'i>) -> Option<()> {
        if self.local {
            // A prototype's geometries are GML's: one that is an implicit geometry places nothing.
            self.counts.nested_instances += 1;
            return Some(());
        }
        let at = elements(*g).find(|c| local(c) == "referencePoint")?;
        let point = elements(at).next()?;
        self.instance = true;
        let p = self.positions(&point);
        self.instance = false;
        let reference = *p?.first()?;
        let matrix = match elements(*g).find(|c| local(c) == "transformationMatrix") {
            None => Some(vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]),
            Some(m) => text(&m).split_ascii_whitespace().map(|v| v.parse().ok()).collect::<Option<Vec<f64>>>(),
        };
        // The matrix places the prototype in the file's axis order, as its reference point is
        // written: in a file read latitude first, its first row gives the latitude, so its first two
        // rows change places as the reference point's coordinates do.
        let place = matrix.and_then(|mut v| {
            if self.swap && v.len() == 16 {
                (0..4).for_each(|k| v.swap(k, k + 4));
            }
            Placement::new(&v, reference)
        });
        let shape = elements(*g).find(prototype_property);
        let to = match shape {
            Some(rel) if only_link(&rel) => attribute(&rel, "href").map(To::Href),
            Some(rel) => elements(rel).next().map(To::Node),
            None => None,
        };
        match (place, to) {
            (Some(place), Some(to)) => self.links.push(Pending { to, wait: None, at: self.at, solid: false, place: Some(place) }),
            _ => self.got.points.push((reference, true)),
        }
        Some(())
    }

    /// The geometry a member property holds: written in place, or a link, followed later.
    fn member(&mut self, m: &Node<'a, 'i>) -> Option<()> {
        if only_link(m) {
            self.link(m, false);
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
        self.drawn += 1;
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
                // A prototype's coordinates are in its own system: never latitude first, and not where
                // the file's objects lie until placed.
                let (x, y) = if self.swap && !self.local { (b, a) } else { (a, b) };
                let p = [x, y, v.get(2).copied().unwrap_or(0.0)];
                if !self.local {
                    self.reach.add(&p);
                }
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
