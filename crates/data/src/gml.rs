//! GML: simple features in Geography Markup Language 2 and 3 — WFS feature collections, OGR's
//! GML, and other application schemas whose features are members of a collection. Each feature
//! type becomes a layer; a feature's simple child elements are its attributes and its first
//! geometry is its shape (points, lines, polygons and their multi forms, envelopes).
//!
//! Coordinates are converted to longitude/latitude from the `srsName` (EPSG codes, URNs and URLs);
//! the URN and URL forms of geographic systems are latitude first, as GML 3 says. A file without
//! one is taken as longitude/latitude, and the report says so when that looks wrong.
//!
//! The file is hostile: no DTD is read, the node count is capped, and features whose geometry
//! can't be read are counted.

use std::cell::Cell;
use std::collections::BTreeMap;

use roxmltree::{Node, ParsingOptions};
use rustgis_geom::Coord;

use crate::raster::Crs;
use crate::{DataError, Dataset, Field, Geometry, GeometryType, ReadReport, Result, Value};

/// GML file types.
pub const EXTENSIONS: &[&str] = &["gml"];

/// Most feature types (layers) read from one file.
const MAX_LAYERS: usize = 64;

pub fn is_gml_name(name: &str) -> bool {
    let ext = std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

pub(crate) fn local<'a>(n: &Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

fn is_gml(n: &Node) -> bool {
    n.tag_name().namespace().is_some_and(|ns| ns.starts_with("http://www.opengis.net/gml"))
}

fn text_of(n: &Node) -> String {
    n.children().filter(|t| t.is_text()).filter_map(|t| t.text()).collect::<String>()
}

/// A coordinate system named by `srsName`, and whether its axes are latitude first.
fn srs(name: &str) -> Option<(Crs, bool)> {
    let n = name.trim();
    let up = n.to_ascii_uppercase();
    if up.ends_with("CRS84") {
        return Some((Crs::LonLat, false));
    }
    let code: u32 = n.rsplit([':', '/', '#']).next()?.trim().parse().ok()?;
    let crs = Crs::from_epsg(code)?;
    Some((crs, lat_first(n, crs)))
}

/// Whether coordinates named by `name`, a name of `crs`, are latitude first. `EPSG:4326` and the
/// old `…epsg.xml#4326` are longitude first; URNs and the opengis.net/def URLs follow the EPSG axis
/// order, latitude first for geographic systems, but for OGC's CRS84, longitude first by definition.
/// A compound system's order is its horizontal part's.
pub(crate) fn lat_first(name: &str, crs: Crs) -> bool {
    let up = name.trim().to_ascii_uppercase();
    (up.starts_with("URN:") || up.contains("OPENGIS.NET/DEF/CRS")) && crs.is_geographic() && !crate::ogc::warp::is_crs84(horizontal(name))
}

/// The horizontal system of a compound URN (`urn:ogc:def:crs,crs:EPSG::25832,crs:EPSG::5783`, its
/// first part), or the name as it is.
pub(crate) fn horizontal(name: &str) -> &str {
    let name = name.trim();
    match name.get(..16) {
        Some(head) if head.eq_ignore_ascii_case("urn:ogc:def:crs,") => {
            name.get(16..).and_then(|rest| rest.split(',').next()).unwrap_or(name)
        }
        _ => name,
    }
}

/// GML geometry elements read.
const GEOMETRIES: &[&str] = &[
    "Point",
    "MultiPoint",
    "LineString",
    "MultiLineString",
    "Curve",
    "MultiCurve",
    "Polygon",
    "MultiPolygon",
    "Surface",
    "MultiSurface",
    "Envelope",
    "Box",
    "LinearRing",
];

/// Geometry elements this reader doesn't take as a feature's shape: the instantiable members of
/// the substitution groups of `gml:AbstractGeometry`, `gml:AbstractSurfacePatch` and
/// `gml:AbstractCurveSegment` in the GML 3.2.1 schema (geometryBasic0d1d.xsd, geometryAggregates.xsd,
/// geometryComplexes.xsd, geometryPrimitives.xsd, grids.xsd at schemas.opengis.net/gml/3.2.1) that aren't in
/// [`GEOMETRIES`], and GML 2's `GeometryCollection`.
const SHAPES_NOT_READ: &[&str] = &[
    "Arc",
    "ArcByBulge",
    "ArcByCenterPoint",
    "ArcString",
    "ArcStringByBulge",
    "BSpline",
    "Bezier",
    "Circle",
    "CircleByCenterPoint",
    "Clothoid",
    "Cone",
    "CubicSpline",
    "Cylinder",
    "Geodesic",
    "GeometryCollection",
    "GeodesicString",
    "LineStringSegment",
    "OffsetCurve",
    "PolygonPatch",
    "Rectangle",
    "Sphere",
    "Triangle",
    "CompositeCurve",
    "CompositeSolid",
    "CompositeSurface",
    "GeometricComplex",
    "Grid",
    "MultiGeometry",
    "MultiSolid",
    "OrientableCurve",
    "OrientableSurface",
    "PolyhedralSurface",
    "RectifiedGrid",
    "Ring",
    "Shell",
    "Solid",
    "Tin",
    "TriangulatedSurface",
];

/// Whether a GML element under a property is a shape (read, or of a kind not read). Any other GML
/// value (a time, a definition, a description, a measure…) is not.
fn is_shape(name: &str) -> bool {
    GEOMETRIES.contains(&name) || SHAPES_NOT_READ.contains(&name)
}

/// The coordinates under a node: `gml:pos`/`gml:posList` (with `srsDimension`),
/// `gml:coordinates` (`cs`, `ts`) or `gml:coord` (`X`, `Y`). `zm` is set when a third or fourth
/// value (a height or measure) is dropped.
fn coords(n: &Node, zm: &Cell<bool>) -> Option<Vec<Coord>> {
    let mut out = Vec::new();
    // Only coordinates that parse count.
    let mut extra = false;
    for c in n.descendants().filter(|d| d.is_element() && is_gml(d)) {
        match local(&c) {
            "pos" | "posList" | "lowerCorner" | "upperCorner" => {
                let v: Vec<f64> = text_of(&c).split_whitespace().map(|t| t.parse().ok()).collect::<Option<_>>()?;
                // Without srsDimension, one position's dimension is its count (a 3D `pos`).
                let one = local(&c) != "posList" && (2..=4).contains(&v.len());
                let dim: usize = c
                    .attribute("srsDimension")
                    .or_else(|| c.ancestors().find_map(|a| a.attribute("srsDimension")))
                    .and_then(|d| d.parse().ok())
                    .filter(|d| (2..=4).contains(d))
                    .unwrap_or(if one { v.len() } else { 2 });
                if !v.len().is_multiple_of(dim) {
                    return None;
                }
                if dim > 2 && !v.is_empty() {
                    extra = true;
                }
                out.extend(
                    v.chunks(dim).map(|p| Coord::new(p.first().copied().unwrap_or(f64::NAN), p.get(1).copied().unwrap_or(f64::NAN))),
                );
            }
            "coordinates" => {
                let cs = c.attribute("cs").unwrap_or(",");
                let ts = c.attribute("ts").unwrap_or(" ");
                let text = text_of(&c);
                for tuple in
                    text.split(|ch: char| if ts == " " { ch.is_whitespace() } else { ts.contains(ch) }).filter(|t| !t.trim().is_empty())
                {
                    let v: Vec<f64> = tuple.split(cs).map(|t| t.trim().parse().ok()).collect::<Option<_>>()?;
                    if v.len() > 2 {
                        extra = true;
                    }
                    out.push(Coord::new(*v.first()?, *v.get(1)?));
                }
            }
            "coord" => {
                let get =
                    |k: &str| c.children().find(|x| x.is_element() && local(x) == k).and_then(|x| text_of(&x).trim().parse::<f64>().ok());
                if get("Z").is_some() {
                    extra = true;
                }
                out.push(Coord::new(get("X")?, get("Y")?));
            }
            _ => {}
        }
    }
    let ok = out.iter().all(|c| c.is_finite());
    if ok && extra {
        zm.set(true);
    }
    ok.then_some(out)
}

fn ring(mut r: Vec<Coord>) -> Option<Vec<Coord>> {
    if r.first() != r.last() {
        r.push(*r.first()?);
    }
    (r.len() >= 4).then_some(r)
}

/// The rings of a polygon or surface patch: exterior (outerBoundaryIs) first. `lost` counts the
/// rings left out: too few points or unreadable, and exterior rings past the first.
fn polygon(p: &Node, zm: &Cell<bool>, lost: &Cell<usize>) -> Option<Vec<Vec<Coord>>> {
    // Each ring `ring` keeps (at least four positions once closed, closed if it wasn't), and whether
    // it had Z or M.
    let boundary = |names: &[&str]| -> Vec<(Vec<Coord>, bool)> {
        let kept = |b: Node| {
            let dims = Cell::new(false);
            let r = coords(&b, &dims).and_then(ring).map(|r| (r, dims.get()));
            lost.set(lost.get() + usize::from(r.is_none()));
            r
        };
        p.descendants().filter(|d| d.is_element() && is_gml(d) && names.contains(&local(d))).filter_map(kept).collect()
    };
    // The first exterior ring, then the interiors: only the rings kept mark Z or M.
    let mut outers = boundary(&["exterior", "outerBoundaryIs"]).into_iter();
    let outer = outers.next()?;
    lost.set(lost.get() + outers.count());
    let mut rings = vec![outer];
    rings.extend(boundary(&["interior", "innerBoundaryIs"]));
    zm.set(zm.get() || rings.iter().any(|(_, d)| *d));
    Some(rings.into_iter().map(|(r, _)| r).collect())
}

/// A GML geometry element as a geometry; `zm` is set when Z or M values are dropped, and `lost`
/// counts the rings and surface patches left out (too few points, or unreadable). Members of a
/// multi-geometry left out are counted by `parts_lost`.
fn geometry(g: &Node, zm: &Cell<bool>, lost: &Cell<usize>) -> Option<Geometry> {
    let kids =
        |names: &[&str]| -> Vec<Node> { g.descendants().filter(|d| d.is_element() && is_gml(d) && names.contains(&local(d))).collect() };
    match local(g) {
        "Point" => coords(g, zm)?.first().map(|c| Geometry::Point(*c)),
        "MultiPoint" => {
            let pts: Vec<Coord> = kids(&["Point"]).iter().filter_map(|p| coords(p, zm)?.first().copied()).collect();
            (!pts.is_empty()).then_some(Geometry::MultiPoint(pts))
        }
        "LineString" | "Curve" | "LinearRing" => {
            let c = coords(g, zm)?;
            (c.len() >= 2).then_some(Geometry::Line(vec![c]))
        }
        "MultiLineString" | "MultiCurve" => {
            // Only a part kept (two points or more) marks Z or M.
            let kept = |l: &Node| {
                let dims = Cell::new(false);
                let c = coords(l, &dims).filter(|c| c.len() >= 2)?;
                zm.set(zm.get() || dims.get());
                Some(c)
            };
            let parts: Vec<Vec<Coord>> = kids(&["LineString", "Curve"]).iter().filter_map(kept).collect();
            (!parts.is_empty()).then_some(Geometry::Line(parts))
        }
        "Polygon" => Some(Geometry::Polygon(vec![polygon(g, zm, lost)?])),
        "Surface" | "MultiPolygon" | "MultiSurface" => {
            let polys: Vec<Vec<Vec<Coord>>> = kids(&["Polygon", "PolygonPatch"])
                .iter()
                .filter_map(|p| {
                    // A polygon left out whole counts once, not by its rings; a multi-geometry's
                    // member left out is counted by `parts_lost`, so only a surface's patch here.
                    let rings = Cell::new(0);
                    let poly = polygon(p, zm, &rings);
                    let whole = usize::from(!local(g).starts_with("Multi"));
                    lost.set(lost.get() + if poly.is_some() { rings.get() } else { whole });
                    poly
                })
                .collect();
            // A plain surface's triangles, rectangles and parametric patches (cones, cylinders,
            // spheres) aren't read: said as left out (a
            // multi-surface's are counted by `parts_lost`).
            if !polys.is_empty() && !local(g).starts_with("Multi") {
                lost.set(lost.get() + kids(&["Triangle", "Rectangle", "Cone", "Cylinder", "Sphere"]).len());
            }
            (!polys.is_empty()).then_some(Geometry::Polygon(polys))
        }
        "Envelope" | "Box" => {
            let c = coords(g, zm)?;
            let (a, b) = (c.first()?, c.get(1)?);
            let (w, e, s, n) = (a.x.min(b.x), a.x.max(b.x), a.y.min(b.y), a.y.max(b.y));
            (w < e && s < n).then(|| {
                Geometry::Polygon(vec![vec![vec![
                    Coord::new(w, s),
                    Coord::new(e, s),
                    Coord::new(e, n),
                    Coord::new(w, n),
                    Coord::new(w, s),
                ]]])
            })
        }
        _ => None,
    }
}

/// How many parts of a multi-geometry give nothing to keep: a member that is only a reference
/// (`xlink:href`), or a part too short (a line of one point) or with unreadable coordinates. 0 for other geometries.
fn parts_lost(g: &Node) -> usize {
    let kind = local(g);
    if !kind.starts_with("Multi") {
        return 0;
    }
    let scratch = Cell::new(false);
    let leaf = |x: &Node| {
        x.is_element()
            && is_gml(x)
            && matches!(
                (kind, local(x)),
                ("MultiPoint", "Point")
                    | ("MultiLineString" | "MultiCurve", "LineString" | "Curve")
                    | ("MultiPolygon" | "MultiSurface", "Polygon" | "PolygonPatch")
            )
    };
    let gives = |x: &Node| match kind {
        "MultiPoint" => coords(x, &scratch).is_some_and(|c| !c.is_empty()),
        "MultiLineString" | "MultiCurve" => coords(x, &scratch).is_some_and(|c| c.len() >= 2),
        _ => polygon(x, &scratch, &Cell::new(0)).is_some(),
    };
    // A member nested in a part (a composite's `surfaceMember`) that is only a reference.
    let reference = |x: &Node| x.is_element() && is_gml(x) && local(x).ends_with("Member") && !x.children().any(|c| c.is_element());
    // A member lost whole (a reference, no part in it) counts once; one holding parts counts those
    // it can't give, and the unread shapes and references nested in it outside a part read or a
    // shape already counted, so a member with one part read and one not says the second.
    let lost = |part: Node| {
        let leaves: Vec<Node> = part.descendants().filter(&leaf).collect();
        if leaves.is_empty() {
            return 1;
        }
        // The elements holding a part (the parts too), marked once so each check below is a walk up
        // the tree, not down it: a hostile member can't make it quadratic.
        let holds: std::collections::HashSet<roxmltree::NodeId> =
            leaves.iter().flat_map(|l| l.ancestors().take_while(|a| *a != part).map(|a| a.id())).collect();
        // A shape nested in a part with nothing in it the reader takes: a triangle, an arc, a
        // surface that is only a reference.
        let unread_shape = |x: &Node| x.is_element() && is_gml(x) && is_shape(local(x)) && !holds.contains(&x.id());
        // Whether something between `x` and the part is a part read or a shape counted.
        let inside = |x: &Node| x.ancestors().skip(1).take_while(|a| *a != part).any(|a| unread_shape(&a) || leaf(&a));
        let shapes = part.descendants().skip(1).filter(|x| unread_shape(x) && !inside(x)).count();
        let refs = part.descendants().skip(1).filter(|x| reference(x) && !inside(x)).count();
        leaves.iter().filter(|x| !gives(x)).count() + shapes + refs
    };
    // A member property holds one part (`curveMember`), a members property several (`curveMembers`).
    g.children()
        .filter(|m| m.is_element())
        .flat_map(|m| {
            let n = local(&m);
            if n.ends_with("Members") {
                m.children().filter(|c| c.is_element()).collect()
            } else if n.ends_with("Member") {
                vec![m]
            } else {
                Vec::new()
            }
        })
        .map(lost)
        .sum()
}

/// The first few distinct names of what a notice counts; more are said as `…`.
#[derive(Default)]
struct Names(Vec<String>, bool);

impl Names {
    fn add(&mut self, p: String) {
        if !self.0.contains(&p) {
            if self.0.len() < 3 {
                self.0.push(p);
            } else {
                self.1 = true;
            }
        }
    }
    fn said(&self) -> String {
        self.0.join(", ") + if self.1 { ", …" } else { "" }
    }
}

/// Most attribute columns of a feature type.
const MAX_COLUMNS: usize = 500;

/// A feature type being read: its attribute columns in order of appearance, and its rows.
#[derive(Default)]
struct Kind {
    columns: Vec<String>,
    rows: Vec<(Option<Geometry>, BTreeMap<String, String>)>,
    /// Features whose Z or M values were dropped.
    zm: usize,
    /// Properties not read (a nested record, a time, a shape past the one kept): how many, and the
    /// first few names.
    unread: usize,
    unread_names: Names,
    /// Features whose shape couldn't be read.
    skipped: usize,
    /// Values of a property the feature already had (the last is kept), and values of properties
    /// past the column cap: how many, and the first few names.
    repeated: usize,
    repeated_names: Names,
    capped: usize,
    capped_names: Names,
    /// Members of multi-geometries that gave nothing to keep.
    parts_lost: usize,
    /// Rings and surface patches of its geometries left out.
    lost: usize,
}

/// Reads a GML file into a layer per feature type (those with features).
pub fn read(file_name: &str, bytes: &[u8]) -> Result<Vec<(Dataset, ReadReport)>> {
    let stem = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("gml").to_string();
    let text = std::str::from_utf8(bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")))?;
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
    let doc = crate::xml::parse(text.trim_start_matches('\u{feff}'), opts).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    let root = doc.root_element();
    // The features: children of member elements (not of one inside a feature, such as a
    // property or a geometry collection's member named alike).
    let is_member = |n: &Node| n.is_element() && matches!(local(n), "featureMember" | "featureMembers" | "member" | "members");
    let members: Vec<Node> = root
        .descendants()
        .filter(|n| is_member(n) && n.ancestors().skip(1).all(|a| !is_member(&a)))
        .flat_map(|m| m.children().filter(|c| c.is_element()))
        .filter(|f| !(is_gml(f) && GEOMETRIES.contains(&local(f))))
        .collect();
    if members.is_empty() {
        return Err(DataError::Format(format!("{file_name}: no features (featureMember elements) in the GML")));
    }
    let mut kinds: BTreeMap<String, Kind> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut file_srs: Option<(Crs, bool)> = None;
    let mut unknown_srs: Option<String> = None;
    let mut over = 0usize;
    // Rings and parts left out of the geometries kept.
    for f in members {
        let name = local(&f).to_string();
        if !kinds.contains_key(&name) {
            if kinds.len() >= MAX_LAYERS {
                over += 1;
                continue;
            }
            order.push(name.clone());
        }
        let kind = kinds.entry(name).or_default();
        let mut attrs = BTreeMap::new();
        let mut geom = None;
        let mut bad = false;
        // Whether the geometry kept had Z or M values.
        let mut zm = false;
        // Rings and surface patches left out of the geometry kept.
        let mut rings_lost = 0;
        // Properties not read, and those holding GML of a kind not read (another kind of
        // geometry): the feature's unreadable shape when it has none, else not read.
        // Each with the property's place, so the notice names them in document order.
        let (mut unread, mut shapes_not_read): (Vec<(usize, String)>, Vec<(usize, String)>) = (Vec::new(), Vec::new());
        // Geometry properties that couldn't be read: said as not read when another gave the shape.
        let mut failed = Vec::new();
        for (at, p) in f.children().filter(|c| c.is_element()).enumerate() {
            // A property holding a geometry, or a simple value.
            // A feature's bounding box (gml:boundedBy) is neither its shape nor an attribute.
            if is_gml(&p) && local(&p) == "boundedBy" {
                continue;
            }
            if let Some(g) = p.children().find(|c| c.is_element() && is_gml(c) && GEOMETRIES.contains(&local(c))) {
                if geom.is_some() {
                    unread.push((at, local(&p).to_string()));
                    continue;
                }
                let s = g.attribute("srsName").or_else(|| g.descendants().find_map(|d| d.attribute("srsName")));
                if let Some(s) = s {
                    match srs(s) {
                        Some(found) => {
                            file_srs.get_or_insert(found);
                        }
                        None => unknown_srs = Some(s.to_string()),
                    }
                }
                let (dims, lost) = (Cell::new(false), Cell::new(0));
                match geometry(&g, &dims, &lost) {
                    Some(gm) => {
                        zm = dims.get();
                        rings_lost += lost.get();
                        geom = Some(gm);
                        kind.parts_lost += parts_lost(&g);
                    }
                    None => {
                        bad = true;
                        failed.push((at, local(&p).to_string()));
                    }
                }
            } else if let Some(c) = p.children().find(|c| c.is_element() && is_gml(c)) {
                // A geometry of a kind not read (a CompositeCurve, a Solid…) may be the feature's
                // only shape; another GML value (a time, a definition) is no shape.
                if is_shape(local(&c)) { &mut shapes_not_read } else { &mut unread }.push((at, local(&p).to_string()));
            } else if p.children().any(|c| c.is_element()) {
                // A nested record isn't an attribute.
                unread.push((at, local(&p).to_string()));
            } else {
                let key = local(&p).to_string();
                if !kind.columns.contains(&key) {
                    if kind.columns.len() >= MAX_COLUMNS {
                        kind.capped += 1;
                        kind.capped_names.add(key);
                        continue;
                    }
                    kind.columns.push(key.clone());
                }
                if attrs.insert(key.clone(), text_of(&p).trim().to_string()).is_some() {
                    kind.repeated += 1;
                    kind.repeated_names.add(key);
                }
            }
        }
        // With no shape read, the first one that failed or isn't read is the feature's unreadable
        // shape; any others are said as not read.
        failed.append(&mut shapes_not_read);
        failed.sort_by_key(|(at, _)| *at);
        if geom.is_none() && !failed.is_empty() {
            bad = true;
            unread.extend(failed.drain(..).skip(1));
        } else {
            unread.append(&mut failed);
        }
        unread.sort_by_key(|(at, _)| *at);
        for (_, p) in unread {
            kind.unread += 1;
            kind.unread_names.add(p);
        }
        // A gml:id is a feature's id when it has no other.
        if let Some(id) = f.attributes().find(|a| a.name() == "id" || a.name() == "fid").map(|a| a.value().to_string()) {
            if !kind.columns.iter().any(|c| c == "gml_id") {
                kind.columns.insert(0, "gml_id".into());
            }
            attrs.insert("gml_id".into(), id);
        }
        // A geometry that couldn't be read, unless another property gave the feature its shape.
        kind.skipped += usize::from(bad && geom.is_none());
        kind.zm += usize::from(zm && geom.is_some());
        kind.lost += rings_lost;
        kind.rows.push((geom, attrs));
    }
    let mut report = ReadReport::default();
    if over > 0 {
        report.messages.push(format!("{over} features of more than {MAX_LAYERS} feature types were left out"));
    }
    if let Some(s) = &unknown_srs {
        report.messages.push(format!("unknown coordinate system '{s}': coordinates are read as they are"));
    }
    let transform = match file_srs {
        Some((c, swap)) => Some((if c == Crs::LonLat { None } else { Some(c.transformer().map_err(DataError::Format)?) }, swap)),
        None => None,
    };
    let single = order.len() == 1;
    let mut out = Vec::new();
    for name in order {
        let Some(kind) = kinds.remove(&name) else { continue };
        let fields: Vec<Field> = kind
            .columns
            .iter()
            .map(|c| Field::new(c.clone(), crate::io::text_type(kind.rows.iter().filter_map(|(_, a)| a.get(c)).map(String::as_str))))
            .collect();
        let title = if single { stem.clone() } else { format!("{stem} {name}") };
        let mut ds = Dataset::new(title, GeometryType::None, fields);
        let (zm, unread, parts) = (kind.zm, kind.unread, kind.parts_lost);
        let unread_names = kind.unread_names.said();
        let mut outside = false;
        for (g, attrs) in kind.rows {
            let g = g.map(|g| match &transform {
                Some((t, swap)) => g.map_coords(&|c| {
                    let c = if *swap { Coord::new(c.y, c.x) } else { c };
                    match t {
                        Some(t) => t.to_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)),
                        None => c,
                    }
                }),
                None => g,
            });
            if let Some(g) = &g {
                let e = g.extent();
                outside |= e.min_x < -180.0 || e.max_x > 180.0 || e.min_y < -90.0 || e.max_y > 90.0;
            }
            let row = ds.fields.iter().map(|f| attrs.get(&f.name).map(|v| Value::parse_source(v, f.ty)).unwrap_or_default()).collect();
            ds.push_next(g, row);
        }
        ds.geometry_type = crate::io::dominant_type(&ds);
        // The file's notices go with its first layer only.
        let mut rep = if out.is_empty() { report.clone() } else { ReadReport::default() };
        // Features whose shape couldn't be read are said with their own feature type.
        rep.skipped = kind.skipped;
        if kind.skipped > 0 {
            let what = if kind.skipped == 1 { "feature" } else { "features" };
            rep.messages.push(format!("{}: {} {what} had geometry that couldn't be read", ds.name, kind.skipped));
        }
        if file_srs.is_none() && outside {
            rep.messages.push("the GML doesn't say its coordinate system (srsName) and its coordinates aren't longitude/latitude".into());
        }
        // Said with the layer it happened in, which may be the only one added.
        if kind.lost > 0 {
            let what =
                if kind.lost == 1 { "ring or surface patch of a geometry was" } else { "rings or surface patches of geometries were" };
            rep.messages.push(format!(
                "{}: {} {what} left out: too few points, unreadable coordinates, a patch with no exterior ring or of a kind not read, or an exterior ring past the first",
                ds.name, kind.lost
            ));
        }
        if zm > 0 {
            ds.zm_dropped = true;
            rep.messages.push(format!("{}: {}", ds.name, crate::zm_note(Some(zm as u64))));
        }
        if parts > 0 {
            let what = if parts == 1 { "part" } else { "parts" };
            rep.messages.push(format!(
                "{}: {parts} {what} of multi-geometries not read (a reference, too short, invalid coordinates, or of a kind not read)",
                ds.name
            ));
        }
        if unread > 0 {
            let what = if unread == 1 { "property" } else { "properties" };
            rep.messages.push(format!(
                "{}: {unread} {what} not read: nested records, other GML values or further shapes ({unread_names})",
                ds.name
            ));
        }
        if kind.repeated > 0 {
            let what = if kind.repeated == 1 { "value" } else { "values" };
            rep.messages.push(format!(
                "{}: {} {what} of repeated properties not read, only the last is kept ({})",
                ds.name,
                kind.repeated,
                kind.repeated_names.said()
            ));
        }
        if kind.capped > 0 {
            let what = if kind.capped == 1 { "value" } else { "values" };
            rep.messages.push(format!(
                "{}: {} {what} of properties past the first {MAX_COLUMNS} names not read ({})",
                ds.name,
                kind.capped,
                kind.capped_names.said()
            ));
        }
        out.push((ds, rep));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FieldType;

    const WFS: &str = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:roads gml:id="r.1"><gml:boundedBy><gml:Envelope srsName="urn:ogc:def:crs:EPSG::4326"><gml:lowerCorner>48 10</gml:lowerCorner><gml:upperCorner>49 11</gml:upperCorner></gml:Envelope></gml:boundedBy><app:name>Main</app:name><app:lanes>2</app:lanes>
  <app:geom><gml:LineString srsName="urn:ogc:def:crs:EPSG::4326"><gml:posList>48 10 49 11</gml:posList></gml:LineString></app:geom></app:roads></wfs:member>
<wfs:member><app:roads gml:id="r.2"><app:name>Side</app:name><app:lanes>1</app:lanes>
  <app:geom><gml:LineString srsName="urn:ogc:def:crs:EPSG::4326"><gml:posList srsDimension="3">48 10 5 49 12 6</gml:posList></gml:LineString></app:geom></app:roads></wfs:member>
<wfs:member><app:parks gml:id="p.1"><app:name>Park</app:name>
  <app:geom><gml:Polygon srsName="urn:ogc:def:crs:EPSG::4326"><gml:exterior><gml:LinearRing><gml:posList>48 10 48 11 49 11 48 10</gml:posList></gml:LinearRing></gml:exterior>
  <gml:interior><gml:LinearRing><gml:posList>48.2 10.4 48.3 10.5 48.4 10.4 48.2 10.4</gml:posList></gml:LinearRing></gml:interior></gml:Polygon></app:geom></app:parks></wfs:member>
</wfs:FeatureCollection>"#;

    #[test]
    fn reads_wfs_gml3() {
        let layers = read("w.gml", WFS.as_bytes()).unwrap();
        let names: Vec<&str> = layers.iter().map(|(d, _)| d.name.as_str()).collect();
        assert_eq!(names, ["w roads", "w parks"]);
        let roads = &layers[0].0;
        assert_eq!(roads.geometry_type, GeometryType::Polyline);
        assert_eq!(roads.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["gml_id", "name", "lanes"]);
        assert_eq!(roads.fields[2].ty, FieldType::Integer);
        // URN axis order: latitude first, swapped to longitude first.
        assert_eq!(roads.features[0].geometry, Some(Geometry::Line(vec![vec![Coord::new(10.0, 48.0), Coord::new(11.0, 49.0)]])));
        assert_eq!(
            roads.features[1].geometry,
            Some(Geometry::Line(vec![vec![Coord::new(10.0, 48.0), Coord::new(12.0, 49.0)]])),
            "3D posList"
        );
        assert!(matches!(&layers[1].0.features[0].geometry, Some(Geometry::Polygon(p)) if p[0].len() == 2));
        assert_eq!(layers[0].1.skipped, 0, "a feature's boundedBy is no failed geometry");
        assert!(layers.iter().all(|(_, r)| !r.messages.iter().any(|m| m.contains("nested"))), "{layers:?}");
    }

    #[test]
    fn reads_gml2_and_projected() {
        // OGR-style GML 2: gml:coordinates, EPSG:32633 (easting/northing), a MultiPolygon.
        let ogr = r#"<ogr:FeatureCollection xmlns:ogr="http://ogr.maptools.org/" xmlns:gml="http://www.opengis.net/gml">
<gml:featureMember><ogr:sites fid="sites.0"><ogr:geometryProperty><gml:Point srsName="EPSG:32633"><gml:coordinates>500000,4649776</gml:coordinates></gml:Point></ogr:geometryProperty><ogr:id>1</ogr:id></ogr:sites></gml:featureMember>
<gml:featureMember><ogr:sites fid="sites.1"><ogr:geometryProperty><gml:MultiPolygon srsName="EPSG:32633"><gml:polygonMember><gml:Polygon><gml:outerBoundaryIs><gml:LinearRing><gml:coordinates>500000,4649776 500100,4649776 500100,4649876 500000,4649776</gml:coordinates></gml:LinearRing></gml:outerBoundaryIs></gml:Polygon></gml:polygonMember></gml:MultiPolygon></ogr:geometryProperty><ogr:id>2</ogr:id></ogr:sites></gml:featureMember>
<gml:featureMember><ogr:sites fid="sites.2"><ogr:geometryProperty><gml:Point><gml:coordinates>x,y</gml:coordinates></gml:Point></ogr:geometryProperty><ogr:id>3</ogr:id></ogr:sites></gml:featureMember>
</ogr:FeatureCollection>"#;
        let layers = read("s.gml", ogr.as_bytes()).unwrap();
        assert_eq!(layers.len(), 1);
        let (ds, rep) = &layers[0];
        assert_eq!(ds.name, "s");
        let Some(Geometry::Point(c)) = &ds.features[0].geometry else { panic!() };
        assert!((c.x - 15.0).abs() < 1e-6 && (c.y - 42.0).abs() < 0.01, "{c:?}");
        assert_eq!(rep.skipped, 1);
        assert_eq!(srs("EPSG:4326").map(|s| s.1), Some(false));
        assert_eq!(srs("http://www.opengis.net/def/crs/EPSG/0/4326").map(|s| s.1), Some(true));
        assert_eq!(srs("urn:ogc:def:crs:EPSG::32633").map(|s| s.1), Some(false), "projected: easting first");
    }

    #[test]
    fn three_d_positions_and_nested_members() {
        // A 3D `pos` without srsDimension, and a property named `member` inside the feature: one
        // feature, its point read.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:wells gml:id="w.1"><app:member><app:person>Ann</app:person></app:member>
  <app:geom><gml:Point srsName="EPSG:4326"><gml:pos>10 48 312</gml:pos></gml:Point></app:geom></app:wells></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("n.gml", gml.as_bytes()).unwrap();
        assert_eq!(layers.len(), 1, "{:?}", layers.iter().map(|l| &l.0.name).collect::<Vec<_>>());
        let (ds, rep) = &layers[0];
        assert_eq!((ds.features.len(), rep.skipped), (1, 0));
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(10.0, 48.0))));
        // Its height is dropped, and said; so is the nested `member` property, not an attribute.
        assert!(ds.zm_dropped && rep.messages.iter().any(|m| m.contains("Z or M values")), "{:?}", rep.messages);
        assert!(
            rep.messages.iter().any(|m| m == "n: 1 property not read: nested records, other GML values or further shapes (member)"),
            "{:?}",
            rep.messages
        );

        // A one-point 3D part is left out, and its height with it: the 2D line kept drops no Z,
        // and the part left out is said.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:roads gml:id="r.1"><app:geom><gml:MultiCurve srsName="EPSG:4326"><gml:curveMember><gml:LineString><gml:pos>10 48 312</gml:pos></gml:LineString></gml:curveMember>
  <gml:curveMember><gml:LineString><gml:posList srsDimension="2">10 48 11 49</gml:posList></gml:LineString></gml:curveMember></gml:MultiCurve></app:geom></app:roads></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("r.gml", gml.as_bytes()).unwrap();
        assert!(!layers[0].0.zm_dropped, "{:?}", layers[0].1.messages);
        // The part left out is said.
        assert!(
            layers[0]
                .1
                .messages
                .iter()
                .any(|m| m == "r: 1 part of multi-geometries not read (a reference, too short, invalid coordinates, or of a kind not read)"),
            "{:?}",
            layers[0].1.messages
        );
        assert_eq!(layers[0].0.features[0].geometry, Some(Geometry::Line(vec![vec![Coord::new(10.0, 48.0), Coord::new(11.0, 49.0)]])));

        // A two-point 3D interior ring is left out: the 2D polygon kept drops no Z, and the ring
        // left out is said.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:parcels gml:id="p.1"><app:geom><gml:Polygon srsName="EPSG:4326">
  <gml:exterior><gml:LinearRing><gml:posList srsDimension="2">10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior>
  <gml:interior><gml:LinearRing><gml:posList srsDimension="3">10.4 48.2 5 10.5 48.3 5</gml:posList></gml:LinearRing></gml:interior>
</gml:Polygon></app:geom></app:parcels></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("p.gml", gml.as_bytes()).unwrap();
        assert!(!layers[0].0.zm_dropped, "{:?}", layers[0].1.messages);
        assert!(
            layers[0].1.messages.iter().any(|m| m.contains(": 1 ring or surface patch of a geometry was left out")),
            "{:?}",
            layers[0].1.messages
        );
        assert!(matches!(&layers[0].0.features[0].geometry, Some(Geometry::Polygon(p)) if p.len() == 1 && p[0].len() == 1));
    }

    /// Rings with too few points are left out and counted, once a kind, and a multi-geometry's
    /// member left out only as a part; a clean file says nothing of them.
    #[test]
    fn rings_and_parts_left_out_are_counted() {
        let layers = read("w.gml", WFS.as_bytes()).unwrap();
        assert!(layers.iter().all(|(_, r)| !r.messages.iter().any(|m| m.contains("or surface patch"))), "{:?}", layers[0].1.messages);
        // A second exterior ring is a ring left out; a multi-surface's polygon whose only ring is
        // two points is a part left out, said once, by the parts notice.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:parcels gml:id="p.1"><app:geom><gml:Polygon srsName="EPSG:4326">
  <gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior>
  <gml:exterior><gml:LinearRing><gml:posList>20 48 21 48 21 49 20 48</gml:posList></gml:LinearRing></gml:exterior>
</gml:Polygon></app:geom></app:parcels></wfs:member>
<wfs:member><app:parcels gml:id="p.2"><app:geom><gml:MultiSurface srsName="EPSG:4326">
  <gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember>
  <gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember>
</gml:MultiSurface></app:geom></app:parcels></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("p.gml", gml.as_bytes()).unwrap();
        let (ds, rep) = &layers[0];
        assert_eq!((ds.features.len(), rep.skipped), (2, 0));
        let said: Vec<&String> = rep.messages.iter().filter(|m| m.contains("or surface patch")).collect();
        assert_eq!(said.len(), 1, "{:?}", rep.messages);
        assert!(said[0].contains(": 1 ring or surface patch of a geometry was left out"), "{said:?}");
        assert!(rep.messages.iter().any(|m| m.contains(": 1 part of multi-geometries")), "{:?}", rep.messages);

        // With two feature types, the notice goes with the one that lost a part, not the first.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:roads gml:id="r.1"><app:geom><gml:LineString srsName="EPSG:4326"><gml:posList>10 48 11 49</gml:posList></gml:LineString></app:geom></app:roads></wfs:member>
<wfs:member><app:parcels gml:id="p.1"><app:geom><gml:MultiSurface srsName="EPSG:4326">
  <gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember>
  <gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember>
</gml:MultiSurface></app:geom></app:parcels></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("two.gml", gml.as_bytes()).unwrap();
        let said = |i: usize| layers[i].1.messages.iter().any(|m| m.contains("multi-geometries"));
        assert_eq!(layers.len(), 2);
        assert!(!said(0) && said(1), "{:?}", layers.iter().map(|l| &l.1.messages).collect::<Vec<_>>());
        assert!(layers[1].1.messages.iter().any(|m| m.starts_with(&format!("{}: 1 part", layers[1].0.name))), "{:?}", layers[1].1.messages);

        // A multi-surface's polygon with only an interior ring is left out and counted once, as a
        // part, not again as a ring.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:parcels gml:id="p.1"><app:geom><gml:MultiSurface srsName="EPSG:4326">
  <gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember>
  <gml:surfaceMember><gml:Polygon><gml:interior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:interior></gml:Polygon></gml:surfaceMember>
</gml:MultiSurface></app:geom></app:parcels></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("hole.gml", gml.as_bytes()).unwrap();
        let messages = &layers[0].1.messages;
        assert!(!messages.iter().any(|m| m.contains("or surface patch")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains(": 1 part of multi-geometries")), "{messages:?}");
    }

    /// A multi-geometry member that is only a reference gives nothing, and is said; a whole one
    /// says nothing.
    #[test]
    fn referenced_members_are_said() {
        let gml = r##"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:app="http://example.com/app">
<wfs:member><app:r gml:id="r.1"><app:geom><gml:MultiCurve><gml:curveMember><gml:LineString><gml:posList>10 48 11 49</gml:posList></gml:LineString></gml:curveMember>
<gml:curveMember><gml:OrientableCurve><gml:baseCurve xlink:href="#c.9"/></gml:OrientableCurve></gml:curveMember></gml:MultiCurve></app:geom></app:r></wfs:member>
<wfs:member><app:r gml:id="r.2"><app:geom><gml:MultiCurve><gml:curveMembers><gml:LineString><gml:posList>20 48 21 49</gml:posList></gml:LineString>
<gml:OrientableCurve><gml:baseCurve><gml:LineString><gml:posList>22 48 23 49</gml:posList></gml:LineString></gml:baseCurve></gml:OrientableCurve></gml:curveMembers></gml:MultiCurve></app:geom></app:r></wfs:member>
</wfs:FeatureCollection>"##;
        let layers = read("m.gml", gml.as_bytes()).unwrap();
        let (ds, rep) = &layers[0];
        assert_eq!((ds.features.len(), rep.skipped), (2, 0));
        assert!(matches!(&ds.features[1].geometry, Some(Geometry::Line(l)) if l.len() == 2));
        let said: Vec<&String> = rep.messages.iter().filter(|m| m.contains("multi-geometries")).collect();
        assert_eq!(said, ["m: 1 part of multi-geometries not read (a reference, too short, invalid coordinates, or of a kind not read)"]);
    }

    /// Properties that aren't attributes (a nested record, a time, a second shape) are said on
    /// their own feature type's layer, counted.
    #[test]
    fn unread_properties_are_said_on_their_layer() {
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:roads gml:id="r.1"><app:name>A</app:name><app:geom><gml:LineString><gml:posList>10 48 11 49</gml:posList></gml:LineString></app:geom></app:roads></wfs:member>
<wfs:member><app:parks gml:id="p.1"><app:owner><app:who>Ann</app:who></app:owner><app:built><gml:TimeInstant><gml:timePosition>2020</gml:timePosition></gml:TimeInstant></app:built>
  <app:geom><gml:Point><gml:pos>10 48</gml:pos></gml:Point></app:geom><app:entrance><gml:Point><gml:pos>10 49</gml:pos></gml:Point></app:entrance></app:parks></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("t.gml", gml.as_bytes()).unwrap();
        // The file's skipped count goes with the first layer: no shape was unreadable.
        assert_eq!(layers[0].1.skipped, 0, "{:?}", layers[0].1.messages);
        assert!(!layers[0].1.messages.iter().any(|m| m.contains("not read")), "{:?}", layers[0].1.messages);
        let (ds, rep) = &layers[1];
        assert_eq!(ds.features.len(), 1);
        // The first shape is the one kept; the second is said as not read.
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(10.0, 48.0))));
        assert!(
            rep.messages
                .iter()
                .any(|m| m == "t parks: 3 properties not read: nested records, other GML values or further shapes (owner, built, entrance)"),
            "{:?}",
            rep.messages
        );

        // A GML value that isn't a shape (a definition) on a feature without one is a property not
        // read, not an unreadable shape; past three names the list says it goes on.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:notes gml:id="n.1"><app:a><app:x>1</app:x></app:a><app:b><app:x>2</app:x></app:b><app:c><app:x>3</app:x></app:c>
  <app:d><gml:Definition gml:id="d.1"><gml:identifier codeSpace="x">d</gml:identifier></gml:Definition></app:d></app:notes></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("d.gml", gml.as_bytes()).unwrap();
        let (ds, rep) = &layers[0];
        assert_eq!((ds.features.len(), rep.skipped), (1, 0), "{:?}", rep.messages);
        assert!(
            rep.messages.iter().any(|m| m == "d: 4 properties not read: nested records, other GML values or further shapes (a, b, c, …)"),
            "{:?}",
            rep.messages
        );
    }

    #[test]
    fn unread_geometry_kinds_are_counted() {
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:roads gml:id="r.1"><app:name>A</app:name><app:geom><gml:CompositeCurve><gml:curveMember/></gml:CompositeCurve></app:geom></app:roads></wfs:member>
<wfs:member><app:roads gml:id="r.2"><app:name>B</app:name><app:geom><gml:LineString><gml:posList>10 48 11 49</gml:posList></gml:LineString></app:geom></app:roads></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("c.gml", gml.as_bytes()).unwrap();
        let (ds, rep) = &layers[0];
        assert_eq!((ds.features.len(), rep.skipped), (2, 1));
        assert!(ds.features[0].geometry.is_none());
        // A surface patch as the only shape is an unreadable shape too, not a property not read.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:plots gml:id="t.1"><app:geom><gml:Triangle><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 10 49 10 48</gml:posList></gml:LinearRing></gml:exterior></gml:Triangle></app:geom></app:plots></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("t.gml", gml.as_bytes()).unwrap();
        let (ds, rep) = &layers[0];
        assert_eq!((ds.features.len(), rep.skipped), (1, 1), "{:?}", rep.messages);
        assert!(!rep.messages.iter().any(|m| m.contains("not read:")), "{:?}", rep.messages);
        // Two such shapes and metadata: one unreadable shape, the second shape and the metadata said.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:plots gml:id="t.1"><app:a><gml:Triangle/></app:a><app:b><gml:Solid/></app:b><app:m><gml:GenericMetaData>x</gml:GenericMetaData></app:m></app:plots></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("u.gml", gml.as_bytes()).unwrap();
        let (_, rep) = &layers[0];
        assert_eq!(rep.skipped, 1, "{:?}", rep.messages);
        assert!(rep.messages.iter().any(|m| m.ends_with("further shapes (b, m)")), "{:?}", rep.messages);
        // A shape that can't be read, with another that can: the feature has its shape, and the
        // one left out is said.
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">
<wfs:member><app:sites gml:id="s.1"><app:a><gml:LineString><gml:posList>x y</gml:posList></gml:LineString></app:a><app:b><gml:Point><gml:pos>10 48</gml:pos></gml:Point></app:b></app:sites></wfs:member>
</wfs:FeatureCollection>"#;
        let layers = read("s.gml", gml.as_bytes()).unwrap();
        let (ds, rep) = &layers[0];
        assert_eq!((rep.skipped, ds.features[0].geometry.clone()), (0, Some(Geometry::Point(Coord::new(10.0, 48.0)))));
        assert!(
            rep.messages.iter().any(|m| m.ends_with("not read: nested records, other GML values or further shapes (a)")),
            "{:?}",
            rep.messages
        );
    }

    fn doc(members: &str) -> String {
        format!(
            r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app">{members}</wfs:FeatureCollection>"#
        )
    }

    fn said(layer: &(Dataset, ReadReport), what: &str) -> bool {
        layer.1.messages.iter().any(|m| m.contains(what))
    }

    /// #1620: a member holding one part read and one not says the second; a whole one says nothing.
    #[test]
    fn a_member_partly_read_says_its_lost_part() {
        let patch = |pos: &str| {
            format!(
                "<gml:PolygonPatch><gml:exterior><gml:LinearRing><gml:posList>{pos}</gml:posList></gml:LinearRing></gml:exterior></gml:PolygonPatch>"
            )
        };
        let member = |second: &str| {
            doc(&format!(
                "<wfs:member><app:p><app:g><gml:MultiSurface><gml:surfaceMember><gml:Surface><gml:patches>{}{}</gml:patches></gml:Surface></gml:surfaceMember></gml:MultiSurface></app:g></app:p></wfs:member>",
                patch("10 48 11 48 11 49 10 48"),
                patch(second)
            ))
        };
        let lossy = read("m.gml", member("x y z").as_bytes()).unwrap();
        assert!(
            said(&lossy[0], "m: 1 part of multi-geometries not read (a reference, too short, invalid coordinates"),
            "{:?}",
            lossy[0].1.messages
        );
        assert!(lossy[0].0.features[0].geometry.is_some());
        let clean = read("m.gml", member("10 48 12 48 12 49 10 48").as_bytes()).unwrap();
        assert!(!said(&clean[0], "multi-geometries"), "{:?}", clean[0].1.messages);
        assert!(matches!(&clean[0].0.features[0].geometry, Some(Geometry::Polygon(p)) if p.len() == 2));
        // A triangle beside a patch read, and a reference nested in a composite beside a polygon
        // read: each is a part not read.
        let tri = "<gml:Triangle><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior></gml:Triangle>";
        let gml = doc(&format!(
            "<wfs:member><app:p><app:g><gml:MultiSurface><gml:surfaceMember><gml:Surface><gml:patches>{}{tri}</gml:patches></gml:Surface></gml:surfaceMember></gml:MultiSurface></app:g></app:p></wfs:member>",
            patch("10 48 11 48 11 49 10 48")
        ));
        let layers = read("m.gml", gml.as_bytes()).unwrap();
        assert!(said(&layers[0], "m: 1 part of multi-geometries not read"), "{:?}", layers[0].1.messages);
        let gml = doc(
            "<wfs:member><app:p><app:g><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface><gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember><gml:surfaceMember xlink:href=\"#s\" xmlns:xlink=\"http://www.w3.org/1999/xlink\"/></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface></app:g></app:p></wfs:member>",
        );
        let layers = read("m.gml", gml.as_bytes()).unwrap();
        assert!(layers[0].0.features[0].geometry.is_some());
        assert!(said(&layers[0], "m: 1 part of multi-geometries not read"), "{:?}", layers[0].1.messages);
    }

    /// A plain surface's triangle or rectangle beside a patch read is said as left out; two
    /// patches read say nothing.
    #[test]
    fn a_surface_says_its_unread_patches() {
        let ring = "<gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior>";
        let surface = |second: &str| {
            doc(&format!(
                "<wfs:member><app:p><app:g><gml:Surface><gml:patches><gml:PolygonPatch>{ring}</gml:PolygonPatch><gml:{second}>{ring}</gml:{second}></gml:patches></gml:Surface></app:g></app:p></wfs:member>"
            ))
        };
        for kind in ["Triangle", "Rectangle"] {
            let layers = read("s.gml", surface(kind).as_bytes()).unwrap();
            assert!(layers[0].0.features[0].geometry.is_some());
            assert!(said(&layers[0], "s: 1 ring or surface patch of a geometry was left out"), "{kind}: {:?}", layers[0].1.messages);
            assert!(said(&layers[0], "or of a kind not read"), "{:?}", layers[0].1.messages);
        }
        let layers = read("s.gml", surface("Cylinder").as_bytes()).unwrap();
        assert!(said(&layers[0], "s: 1 ring or surface patch of a geometry was left out"), "{:?}", layers[0].1.messages);
        let clean = read("s.gml", surface("PolygonPatch").as_bytes()).unwrap();
        assert!(!said(&clean[0], "surface patch"), "{:?}", clean[0].1.messages);
    }

    /// Unread shapes nested beside a part read are said: an arc in a composite curve, a surface
    /// that is only a reference; a curve's own arc segment is not a part lost (the curve keeps its
    /// control points, a separate approximation).
    #[test]
    fn nested_unread_shapes_are_said() {
        let line = "<gml:LineString><gml:posList>10 48 11 48</gml:posList></gml:LineString>";
        let arc = "<gml:Arc><gml:posList>10 48 10.5 48.5 11 48</gml:posList></gml:Arc>";
        let multi = |inner: &str| doc(&format!("<wfs:member><app:p><app:g>{inner}</app:g></app:p></wfs:member>"));
        let gml = multi(&format!(
            "<gml:MultiCurve><gml:curveMember><gml:CompositeCurve><gml:curveMember>{arc}</gml:curveMember><gml:curveMember>{line}</gml:curveMember></gml:CompositeCurve></gml:curveMember></gml:MultiCurve>"
        ));
        let layers = read("n.gml", gml.as_bytes()).unwrap();
        assert!(layers[0].0.features[0].geometry.is_some());
        assert!(said(&layers[0], "n: 1 part of multi-geometries not read"), "{:?}", layers[0].1.messages);
        let poly = "<gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>";
        let gml = multi(&format!(
            "<gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface><gml:surfaceMember>{poly}</gml:surfaceMember><gml:surfaceMembers><gml:Surface xlink:href=\"#s\" xmlns:xlink=\"http://www.w3.org/1999/xlink\"/></gml:surfaceMembers></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface>"
        ));
        let layers = read("n.gml", gml.as_bytes()).unwrap();
        assert!(said(&layers[0], "n: 1 part of multi-geometries not read"), "{:?}", layers[0].1.messages);
        let gml = multi(&format!(
            "<gml:MultiCurve><gml:curveMember><gml:Curve><gml:segments>{arc}</gml:segments></gml:Curve></gml:curveMember><gml:curveMember>{line}</gml:curveMember></gml:MultiCurve>"
        ));
        let layers = read("n.gml", gml.as_bytes()).unwrap();
        assert!(!said(&layers[0], "multi-geometries"), "{:?}", layers[0].1.messages);
    }

    /// A GML 2 feature whose only shape is a GeometryCollection had geometry that couldn't be read.
    #[test]
    fn a_gml2_geometry_collection_is_a_shape_not_read() {
        let gml = r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs" xmlns:gml="http://www.opengis.net/gml" xmlns:app="http://example.com/app"><gml:featureMember><app:n><app:g><gml:GeometryCollection><gml:geometryMember><gml:Point><gml:coordinates>10,48</gml:coordinates></gml:Point></gml:geometryMember></gml:GeometryCollection></app:g></app:n></gml:featureMember></wfs:FeatureCollection>"#;
        let layers = read("c.gml", gml.as_bytes()).unwrap();
        assert_eq!(layers[0].1.skipped, 1, "{:?}", layers[0].1.messages);
    }

    /// #1620: an interior ring left out is said (already so since #1612).
    #[test]
    fn an_unreadable_interior_ring_is_said() {
        let poly = |hole: &str| {
            doc(&format!(
                "<wfs:member><app:p><app:g><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>10 48 11 48 11 49 10 48</gml:posList></gml:LinearRing></gml:exterior><gml:interior><gml:LinearRing><gml:posList>{hole}</gml:posList></gml:LinearRing></gml:interior></gml:Polygon></app:g></app:p></wfs:member>"
            ))
        };
        let lossy = read("i.gml", poly("x y").as_bytes()).unwrap();
        assert!(said(&lossy[0], ": 1 ring or surface patch of a geometry was left out"), "{:?}", lossy[0].1.messages);
        let clean = read("i.gml", poly("10.2 48.1 10.8 48.1 10.8 48.3 10.2 48.1").as_bytes()).unwrap();
        assert!(!said(&clean[0], "ring"), "{:?}", clean[0].1.messages);
    }

    /// #1620: features without a readable shape are counted and said per feature type.
    #[test]
    fn unreadable_shapes_are_said_on_their_own_feature_type() {
        let two = |second: &str| {
            doc(&format!(
                "<wfs:member><app:roads><app:g><gml:Point><gml:pos>10 48</gml:pos></gml:Point></app:g></app:roads></wfs:member><wfs:member><app:parks><app:g>{second}</app:g></app:parks></wfs:member>"
            ))
        };
        let lossy = read("t.gml", two("<gml:Solid/>").as_bytes()).unwrap();
        assert_eq!((lossy[0].1.skipped, lossy[1].1.skipped), (0, 1));
        assert!(!said(&lossy[0], "couldn't be read") && said(&lossy[1], "t parks: 1 feature had geometry that couldn't be read"));
        let clean = read("t.gml", two("<gml:Point><gml:pos>10 49</gml:pos></gml:Point>").as_bytes()).unwrap();
        assert!(clean.iter().all(|l| l.1.skipped == 0 && !said(l, "couldn't be read")));
    }

    /// #1620: unread property names come in document order, and a `gml:description` value is no shape.
    #[test]
    fn unread_names_follow_the_document_and_a_description_is_no_shape() {
        let gml = doc(
            "<wfs:member><app:n><app:g><gml:Point><gml:pos>10 48</gml:pos></gml:Point></app:g><app:z><gml:Solid/></app:z><app:b><app:x>1</app:x></app:b><app:a><gml:Definition gml:id=\"d\"/></app:a></app:n></wfs:member>",
        );
        let layers = read("o.gml", gml.as_bytes()).unwrap();
        assert!(said(&layers[0], "further shapes (z, b, a)"), "{:?}", layers[0].1.messages);
        // A grid (gml:AbstractImplicitGeometry) is a shape not read, the feature's only one here.
        let gml = doc("<wfs:member><app:n><app:cover><gml:RectifiedGrid gml:id=\"g\" dimension=\"2\"/></app:cover></app:n></wfs:member>");
        let layers = read("g.gml", gml.as_bytes()).unwrap();
        assert_eq!(layers[0].1.skipped, 1, "{:?}", layers[0].1.messages);
        let gml = doc("<wfs:member><app:n><app:note><gml:description>x</gml:description></app:note></app:n></wfs:member>");
        let layers = read("d.gml", gml.as_bytes()).unwrap();
        assert_eq!(layers[0].1.skipped, 0, "{:?}", layers[0].1.messages);
        assert!(said(&layers[0], "d: 1 property not read"), "{:?}", layers[0].1.messages);
    }

    /// #1620: a repeated property keeps its last value and says the rest; so does one past 500 names.
    #[test]
    fn repeated_and_capped_properties_are_said() {
        let feature = |props: &str| doc(&format!("<wfs:member><app:n>{props}</app:n></wfs:member>"));
        let layers = read("r.gml", feature("<app:alias>A</app:alias><app:alias>B</app:alias><app:alias>C</app:alias>").as_bytes()).unwrap();
        assert_eq!(layers[0].0.value(0, 0), &Value::Text("C".into()));
        assert!(
            said(&layers[0], "r: 2 values of repeated properties not read, only the last is kept (alias)"),
            "{:?}",
            layers[0].1.messages
        );
        let layers = read("r.gml", feature("<app:alias>A</app:alias><app:other>B</app:other>").as_bytes()).unwrap();
        assert!(!said(&layers[0], "repeated") && !said(&layers[0], "past the first"), "{:?}", layers[0].1.messages);

        let many = |n: usize| (0..n).map(|i| format!("<app:p{i}>v</app:p{i}>")).collect::<String>();
        let layers = read("c.gml", feature(&many(501)).as_bytes()).unwrap();
        assert_eq!(layers[0].0.fields.len(), MAX_COLUMNS);
        assert!(said(&layers[0], "c: 1 value of properties past the first 500 names not read (p500)"), "{:?}", layers[0].1.messages);
        let layers = read("c.gml", feature(&many(500)).as_bytes()).unwrap();
        assert!(!said(&layers[0], "past the first"), "{:?}", layers[0].1.messages);
    }

    #[test]
    fn junk_is_refused() {
        assert!(read("x.gml", b"<a/>").is_err());
        assert!(read("x.gml", b"\xff").is_err());
        for cut in (0..WFS.len()).step_by(53) {
            let _ = read("x.gml", WFS.as_bytes().get(..cut).unwrap_or_default());
        }
    }
}
