//! Coordinate systems from WKT, the text of a `.prj` file next to a Shapefile or a grid, and to
//! WKT for the files RustGIS writes ([`to_wkt`]).
//!
//! WKT1 (OGC and Esri) is read directly. WKT2 (`PROJCRS`, `GEOGCRS`, as PROJ 6 and later write
//! it) is first rewritten as the WKT1 it corresponds to, its units folded into degrees and the
//! system's own linear unit.
//!
//! An EPSG authority is used when the text names one. The `.prj` files ArcGIS and GDAL write have
//! none (`PROJCS["WGS_1984_UTM_Zone_31N",…]`), so Web Mercator and UTM are also known by name, and
//! any other projection is turned into a PROJ definition from its parameters. Known national
//! datums recover the same Helmert transformation as their bundled EPSG definition.

use super::crs::{Crs, CustomCrs, epsg_proj4};
use super::planetary;

/// The most nested brackets read.
const MAX_DEPTH: usize = 32;

/// One value inside a WKT node's brackets.
#[derive(Debug, Clone)]
enum Item {
    Node(Node),
    Text(String),
    Num(f64),
    /// A bare word, such as `EAST` in `AXIS["X",EAST]`.
    Word(String),
}

/// `NAME[item, item, …]`.
#[derive(Debug, Clone)]
struct Node {
    name: String,
    items: Vec<Item>,
}

impl Node {
    /// The first child node of this name (case-insensitive).
    fn child(&self, name: &str) -> Option<&Node> {
        self.child_of(&[name])
    }

    /// The first child node with one of these names (case-insensitive): WKT2 has several
    /// spellings of most keywords (`ELLIPSOID`/`SPHEROID`, `PRIMEM`/`PRIMEMERIDIAN`).
    fn child_of(&self, names: &[&str]) -> Option<&Node> {
        self.nodes().find(|n| names.iter().any(|name| n.name.eq_ignore_ascii_case(name)))
    }

    /// The child nodes.
    fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.items.iter().filter_map(|i| match i {
            Item::Node(n) => Some(n),
            _ => None,
        })
    }

    /// The child nodes, taken.
    fn into_nodes(self) -> impl Iterator<Item = Node> {
        self.items.into_iter().filter_map(|i| match i {
            Item::Node(n) => Some(n),
            _ => None,
        })
    }

    /// The first child node of this name, to change.
    fn child_mut(&mut self, name: &str) -> Option<&mut Node> {
        self.items.iter_mut().find_map(|i| match i {
            Item::Node(n) if n.name.eq_ignore_ascii_case(name) => Some(n),
            _ => None,
        })
    }

    /// `NAME["title", items…]`.
    fn new(name: &str, title: &str, items: impl IntoIterator<Item = Item>) -> Node {
        Node { name: name.into(), items: std::iter::once(Item::Text(title.into())).chain(items).collect() }
    }

    /// The node's quoted name: `"WGS_1984"` in `DATUM["WGS_1984",…]`.
    fn title(&self) -> &str {
        match self.items.first() {
            Some(Item::Text(t)) => t,
            _ => "",
        }
    }

    /// The `n`th number among the node's values.
    fn num(&self, n: usize) -> Option<f64> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Num(v) => Some(*v),
                _ => None,
            })
            .nth(n)
            .filter(|v| v.is_finite())
    }

    /// Every number among the node's values.
    fn nums(&self) -> Vec<f64> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Num(v) => Some(*v),
                _ => None,
            })
            .collect()
    }
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&mut self) -> Option<u8> {
        while self.s.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
        self.s.get(self.at).copied()
    }

    fn word(&mut self) -> String {
        let start = self.at;
        while self.s.get(self.at).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
            self.at += 1;
        }
        String::from_utf8_lossy(self.s.get(start..self.at).unwrap_or_default()).into_owned()
    }

    fn node(&mut self, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err("the coordinate system is nested too deeply".into());
        }
        let name = self.word();
        if name.is_empty() {
            return Err("the coordinate system isn't WKT".into());
        }
        let close = match self.peek() {
            Some(b'[') => b']',
            Some(b'(') => b')',
            _ => return Err(format!("expected '[' after {name}")),
        };
        self.at += 1;
        let mut items = Vec::new();
        loop {
            let item = match self.peek() {
                Some(b'"') => Item::Text(self.text()?),
                Some(b) if b == b'-' || b == b'+' || b == b'.' || b.is_ascii_digit() => Item::Num(self.number()?),
                Some(b) if b.is_ascii_alphabetic() => {
                    let save = self.at;
                    let word = self.word();
                    if matches!(self.peek(), Some(b'[' | b'(')) {
                        self.at = save;
                        Item::Node(self.node(depth + 1)?)
                    } else {
                        Item::Word(word)
                    }
                }
                Some(b) if b == close && items.is_empty() => {
                    self.at += 1;
                    return Ok(Node { name, items });
                }
                _ => return Err(format!("unexpected text in {name}")),
            };
            items.push(item);
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b) if b == close => {
                    self.at += 1;
                    return Ok(Node { name, items });
                }
                _ => return Err(format!("expected ',' in {name}")),
            }
        }
    }

    /// A quoted string; `""` inside it is a quote.
    fn text(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = Vec::new();
        loop {
            match self.s.get(self.at) {
                None => return Err("a quoted name isn't closed".into()),
                Some(b'"') if self.s.get(self.at + 1) == Some(&b'"') => {
                    out.push(b'"');
                    self.at += 2;
                }
                Some(b'"') => {
                    self.at += 1;
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                Some(b) => {
                    out.push(*b);
                    self.at += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.at;
        while self.s.get(self.at).is_some_and(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E')) {
            self.at += 1;
        }
        let t = std::str::from_utf8(self.s.get(start..self.at).unwrap_or_default()).unwrap_or("");
        t.parse().map_err(|_| format!("'{t}' isn't a number"))
    }
}

fn parse(text: &str) -> Result<Node, String> {
    let mut p = Parser { s: text.trim_start_matches('\u{feff}').as_bytes(), at: 0 };
    p.peek();
    let root = p.node(0)?;
    match p.peek() {
        None => Ok(root),
        Some(_) => Err("text follows the coordinate system".into()),
    }
}

/// Upper case with spaces, dashes and slashes as underscores, runs of them as one: Esri and EPSG
/// spellings of a name compare equal (`WGS 84 / UTM zone 31N` → `WGS_84_UTM_ZONE_31N`).
fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' };
        if c != '_' || !(out.is_empty() || out.ends_with('_')) {
            out.push(c);
        }
    }
    out.trim_end_matches('_').to_string()
}

/// The coordinate system a WKT text (WKT1, OGC or Esri, or WKT2) describes.
pub fn crs_from_wkt(text: &str) -> Result<Crs, String> {
    read_wkt(text).map(|(crs, _)| crs)
}

/// The coordinate system a WKT text describes, with a note when:
/// - its datum has no conversion to WGS 84 available (for example, NAD 27), so positions may be off
///   by up to a few hundred metres;
/// - or it is another body's than the Earth's ([`planetary`]), whose longitude/latitude are read
///   as they are.
///
/// Longitudes counted west are refused. So is another body's planetographic system, or a
/// projection of one, unless its axes say they count east. On most bodies planetographic
/// longitudes count west, and an Esri `.prj` has no axes to say otherwise.
pub fn read_wkt(text: &str) -> Result<(Crs, Option<String>), String> {
    let root = wkt1(parse(text)?, 0)?;
    if let Some(a) = root.child("AUTHORITY")
        && a.title().eq_ignore_ascii_case("EPSG")
        && let Some(crs) = authority_code(a).and_then(Crs::from_epsg)
    {
        return Ok((crs, None));
    }
    match root.name.to_ascii_uppercase().as_str() {
        "GEOGCS" => {
            if on_another_body(Some(&root)) && planetographic(&root) && !counts_east(&root) {
                return Err(planetary::OGRAPHIC.into());
            }
            let crs = geographic(&root)?;
            Ok((crs, note(crs, Some(&root))))
        }
        "PROJCS" => {
            if on_another_body(root.child("GEOGCS")) {
                mirrored_projection(&root)?;
            }
            let crs = projected(&root)?;
            let note = if matches!(crs, Crs::Custom(_)) { note(crs, root.child("GEOGCS")) } else { None };
            Ok((crs, note))
        }
        other => Err(format!("{other} coordinate systems aren't supported")),
    }
}

/// Whether a WKT1 geographic system's ellipsoid is another body's. It is checked before the system
/// is read, because reading a system keeps it in the run's table even when it is then refused.
fn on_another_body(geog: Option<&Node>) -> bool {
    geog.and_then(|g| g.child("DATUM")).and_then(|d| d.child("SPHEROID")).and_then(|s| s.num(0)).is_some_and(planetary::off_earth)
}

/// Whether a system is planetographic by its name or its IAU code ([`planetary::iau_ographic`]).
fn planetographic(n: &Node) -> bool {
    planetary::ographic_name(n.title())
        || n.child("AUTHORITY").is_some_and(|a| authority_code(a).is_some_and(|c| planetary::iau_ographic(a.title(), i64::from(c))))
}

/// Whether one of a system's own axes points east. Axes tied to a meridian (polar ones) don't count.
fn counts_east(n: &Node) -> bool {
    n.nodes()
        .filter(|a| a.name.eq_ignore_ascii_case("AXIS") && a.child("MERIDIAN").is_none())
        .any(|a| a.items.iter().any(|i| matches!(i, Item::Word(w) if w.eq_ignore_ascii_case("east"))))
}

/// Why another body's projection would be read mirrored, if it would:
/// - its own axes point west or south;
/// - or it projects planetographic longitude/latitude, to which PROJ gives westings, and its axes
///   don't say x counts east.
///
/// It projects planetographic longitude/latitude when its own name or code says so, when its
/// geographic system's axes point west, or when that system's name or code says so and its axes
/// don't point east.
fn mirrored_projection(p: &Node) -> Result<(), String> {
    if reversed_axes(p) {
        return Err("axes pointing west or south aren't supported".into());
    }
    let base_ographic = p.child("GEOGCS").is_some_and(|g| reversed_axes(g) || (planetographic(g) && !counts_east(g)));
    if (planetographic(p) || base_ographic) && !counts_east(p) {
        return Err(planetary::OGRAPHIC_PROJECTED.into());
    }
    Ok(())
}

/// The note for reading `crs`, whose `GEOGCS` is `geog`. It says that the system is another body's,
/// or that its datum has no conversion to WGS 84 ([`datum_note`]). Never both: another body's
/// datum has no such conversion to lack.
fn note(crs: Crs, geog: Option<&Node>) -> Option<String> {
    if !crs.is_planetary() {
        return datum_note(geog);
    }
    let system = match geog.and_then(|g| g.child("DATUM")).map(Node::title).filter(|d| !d.trim().is_empty()) {
        Some(datum) => format!("{} (datum {datum})", crs.label()),
        None => crs.label(),
    };
    Some(planetary::read_note(&system))
}

/// The name of another body's datum (else of its ellipsoid) in a WKT text, told by the ellipsoid's
/// semi-major axis ([`planetary`]). It names the system when one that can't be read is refused.
/// `None` for the Earth's, and for text that isn't WKT.
pub fn planetary_datum(text: &str) -> Option<String> {
    fn find(n: &Node, depth: usize) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        if let Some(e) = n.child_of(&["SPHEROID", "ELLIPSOID"])
            && let Some(a) = e.num(0)
            && planetary::off_earth(a * factor(e.child_of(&LENGTH_UNIT)).unwrap_or(1.0))
        {
            return Some(if n.title().trim().is_empty() { e.title() } else { n.title() }.to_string());
        }
        n.nodes().find_map(|c| find(c, depth + 1))
    }
    find(&parse(text).ok()?, 0)
}

/// The WKT1 form of a coordinate system: WKT1 as it is, WKT2 rewritten. A compound system (a
/// horizontal one with heights) gives its horizontal part.
fn wkt1(n: Node, depth: usize) -> Result<Node, String> {
    if depth > 4 {
        return Err("the coordinate system is nested too deeply".into());
    }
    match n.name.to_ascii_uppercase().as_str() {
        "PROJCRS" | "PROJECTEDCRS" => projected2(&n),
        "GEOGCRS" | "GEOGRAPHICCRS" | "GEODCRS" | "GEODETICCRS" => geographic2(&n, true),
        "COMPOUNDCRS" | "COMPD_CS" => match n.into_nodes().find(|c| HORIZONTAL.iter().any(|k| c.name.eq_ignore_ascii_case(k))) {
            Some(h) => wkt1(h, depth + 1),
            None => Err("the compound coordinate system has no horizontal part".into()),
        },
        "BOUNDCRS" => bound(n, depth),
        _ => Ok(n),
    }
}

/// The parts of a compound system that can be its horizontal one.
const HORIZONTAL: [&str; 9] =
    ["PROJCRS", "PROJECTEDCRS", "GEOGCRS", "GEOGRAPHICCRS", "GEODCRS", "GEODETICCRS", "PROJCS", "GEOGCS", "BOUNDCRS"];

/// `ID["EPSG",32631]` as `AUTHORITY["EPSG",32631]`.
fn authority(n: &Node) -> Option<Item> {
    let id = n.child_of(&["ID", "AUTHORITY"])?;
    Some(Item::Node(Node { name: "AUTHORITY".into(), items: id.items.iter().take(2).cloned().collect() }))
}

/// The conversion factor of a unit node (`ANGLEUNIT["degree",0.0174532925199433]`).
fn factor(u: Option<&Node>) -> Option<f64> {
    u.and_then(|u| u.num(0)).filter(|f| *f > 0.0)
}

/// `v` in a unit of `f` (radians or metres per unit) as a value in a unit of `to`: unchanged when
/// the units agree, so that 49° doesn't come back as 49.000000000000014°.
fn convert(v: f64, f: Option<f64>, to: f64) -> f64 {
    match f {
        Some(f) if ((f - to) / to).abs() > 1e-12 => v * f / to,
        _ => v,
    }
}

/// The unit of a system's axes: given once after its `CS`, or on each `AXIS` (WKT2 2019).
fn cs_unit<'a>(n: &'a Node, kinds: &[&str]) -> Option<&'a Node> {
    n.child_of(kinds).or_else(|| n.nodes().filter(|a| a.name.eq_ignore_ascii_case("AXIS")).find_map(|a| a.child_of(kinds)))
}

const ANGLE_UNIT: [&str; 2] = ["ANGLEUNIT", "UNIT"];
const LENGTH_UNIT: [&str; 2] = ["LENGTHUNIT", "UNIT"];

/// A WKT2 geographic system as a `GEOGCS`: its datum (or datum ensemble) with the ellipsoid in
/// metres, the prime meridian in degrees and the angle unit. `top` adds its `ID` as the authority:
/// a projected system's base has its own code, which isn't the projected one's.
fn geographic2(g: &Node, top: bool) -> Result<Node, String> {
    if let Some(cs) = g.child("CS")
        && let Some(Item::Word(kind)) = cs.items.first()
        && !kind.eq_ignore_ascii_case("ellipsoidal")
    {
        return Err(format!("{kind} coordinate systems aren't supported"));
    }
    // Longitudes counted west (planetographic ones, on most bodies) would be read as east, which
    // mirrors the data. WKT1 has no way to say so, as for a projected system's axes.
    if top && reversed_axes(g) && epsg_id(g).and_then(Crs::from_epsg).is_none() {
        return Err("axes pointing west or south aren't supported".into());
    }
    let unit = cs_unit(g, &ANGLE_UNIT);
    let mut items = Vec::new();
    if let Some(d) = g.child_of(&["DATUM", "GEODETICDATUM", "TRF", "ENSEMBLE"]) {
        let mut datum = Vec::new();
        if let Some(e) = d.child_of(&["ELLIPSOID", "SPHEROID"]) {
            let to_m = factor(e.child_of(&LENGTH_UNIT)).unwrap_or(1.0);
            let a = e.num(0).ok_or_else(|| format!("the ellipsoid {} has no semi-major axis", e.title()))?;
            let axes = std::iter::once(a * to_m).chain(e.num(1)).map(Item::Num);
            datum.push(Item::Node(Node::new("SPHEROID", e.title(), axes)));
        }
        items.push(Item::Node(Node::new("DATUM", d.title(), datum)));
    }
    if let Some(pm) = g
        .child_of(&["PRIMEM", "PRIMEMERIDIAN"])
        .or_else(|| g.child_of(&["DATUM", "GEODETICDATUM", "TRF"])?.child_of(&["PRIMEM", "PRIMEMERIDIAN"]))
    {
        // In the prime meridian's own unit, else the system's.
        let f = factor(pm.child_of(&ANGLE_UNIT)).or(factor(unit));
        items.push(Item::Node(Node::new("PRIMEM", pm.title(), pm.num(0).map(|v| Item::Num(convert(v, f, 1f64.to_radians()))))));
    }
    if let Some(f) = factor(unit) {
        items.push(Item::Node(Node::new("UNIT", unit.map_or("", Node::title), [Item::Num(f)])));
    }
    // Its axes are kept so that [`read_wkt`] can check their directions.
    items.extend(axes(g));
    items.extend(authority(g).filter(|_| top));
    Ok(Node::new("GEOGCS", g.title(), items))
}

/// A system's `AXIS` nodes, as they are.
fn axes(n: &Node) -> impl Iterator<Item = Item> + '_ {
    n.nodes().filter(|a| a.name.eq_ignore_ascii_case("AXIS")).cloned().map(Item::Node)
}

/// How a parameter's value is measured, for its unit conversion.
#[derive(Clone, Copy, PartialEq)]
enum Measure {
    Angle,
    Length,
    Scale,
}

/// WKT2's projection methods by EPSG code and EPSG name, with the WKT1 name they're read by.
const METHODS: &[(u32, &str, &str)] = &[
    (9807, "TRANSVERSE_MERCATOR", "Transverse_Mercator"),
    (9804, "MERCATOR_VARIANT_A", "Mercator_1SP"),
    (1026, "MERCATOR_SPHERICAL", "Mercator_Spherical"),
    (9840, "ORTHOGRAPHIC", "Orthographic"),
    (9805, "MERCATOR_VARIANT_B", "Mercator_2SP"),
    (1024, "POPULAR_VISUALISATION_PSEUDO_MERCATOR", "Popular_Visualisation_Pseudo_Mercator"),
    (9801, "LAMBERT_CONIC_CONFORMAL_1SP", "Lambert_Conformal_Conic_1SP"),
    (9802, "LAMBERT_CONIC_CONFORMAL_2SP", "Lambert_Conformal_Conic_2SP"),
    (9822, "ALBERS_EQUAL_AREA", "Albers_Conic_Equal_Area"),
    (9820, "LAMBERT_AZIMUTHAL_EQUAL_AREA", "Lambert_Azimuthal_Equal_Area"),
    (1027, "LAMBERT_AZIMUTHAL_EQUAL_AREA_SPHERICAL", "Lambert_Azimuthal_Equal_Area"),
    (9810, "POLAR_STEREOGRAPHIC_VARIANT_A", "Polar_Stereographic"),
    (9829, "POLAR_STEREOGRAPHIC_VARIANT_B", "Polar_Stereographic"),
    (9809, "OBLIQUE_STEREOGRAPHIC", "Oblique_Stereographic"),
    (9812, "HOTINE_OBLIQUE_MERCATOR_VARIANT_A", "Hotine_Oblique_Mercator"),
    (9815, "HOTINE_OBLIQUE_MERCATOR_VARIANT_B", "Hotine_Oblique_Mercator_Azimuth_Center"),
    (1028, "EQUIDISTANT_CYLINDRICAL", "Equirectangular"),
    (1029, "EQUIDISTANT_CYLINDRICAL_SPHERICAL", "Equidistant_Cylindrical_Spherical"),
    (9823, "EQUIDISTANT_CYLINDRICAL_SPHERICAL", "Equidistant_Cylindrical_Spherical"),
    (9835, "LAMBERT_CYLINDRICAL_EQUAL_AREA", "Cylindrical_Equal_Area"),
    (9834, "LAMBERT_CYLINDRICAL_EQUAL_AREA_SPHERICAL", "Cylindrical_Equal_Area"),
];

/// WKT2's projection parameters by EPSG code and EPSG name, with their WKT1 name.
const PARAMETERS: &[(u32, &str, &str, Measure)] = &[
    (8801, "LATITUDE_OF_NATURAL_ORIGIN", "Latitude_Of_Origin", Measure::Angle),
    (8802, "LONGITUDE_OF_NATURAL_ORIGIN", "Central_Meridian", Measure::Angle),
    (8805, "SCALE_FACTOR_AT_NATURAL_ORIGIN", "Scale_Factor", Measure::Scale),
    (8806, "FALSE_EASTING", "False_Easting", Measure::Length),
    (8807, "FALSE_NORTHING", "False_Northing", Measure::Length),
    (8821, "LATITUDE_OF_FALSE_ORIGIN", "Latitude_Of_Origin", Measure::Angle),
    (8822, "LONGITUDE_OF_FALSE_ORIGIN", "Central_Meridian", Measure::Angle),
    (8823, "LATITUDE_OF_1ST_STANDARD_PARALLEL", "Standard_Parallel_1", Measure::Angle),
    (8824, "LATITUDE_OF_2ND_STANDARD_PARALLEL", "Standard_Parallel_2", Measure::Angle),
    (8826, "EASTING_AT_FALSE_ORIGIN", "False_Easting", Measure::Length),
    (8827, "NORTHING_AT_FALSE_ORIGIN", "False_Northing", Measure::Length),
    (8832, "LATITUDE_OF_STANDARD_PARALLEL", "Standard_Parallel_1", Measure::Angle),
    (8833, "LONGITUDE_OF_ORIGIN", "Central_Meridian", Measure::Angle),
    (8811, "LATITUDE_OF_PROJECTION_CENTRE", "Latitude_Of_Center", Measure::Angle),
    (8812, "LONGITUDE_OF_PROJECTION_CENTRE", "Longitude_Of_Center", Measure::Angle),
    (8815, "SCALE_FACTOR_ON_INITIAL_LINE", "Scale_Factor", Measure::Scale),
    (8816, "EASTING_AT_PROJECTION_CENTRE", "False_Easting", Measure::Length),
    (8817, "NORTHING_AT_PROJECTION_CENTRE", "False_Northing", Measure::Length),
    (8813, "AZIMUTH_OF_INITIAL_LINE", "Azimuth", Measure::Angle),
    (8814, "ANGLE_FROM_RECTIFIED_TO_SKEW_GRID", "Rectified_Grid_Angle", Measure::Angle),
];

/// The EPSG code in a node's `ID["EPSG",n]`.
fn epsg_id(n: &Node) -> Option<u32> {
    n.child("ID").filter(|id| id.title().eq_ignore_ascii_case("EPSG")).and_then(authority_code)
}

/// A table row for a node, by its EPSG code, else its name.
fn lookup<'a, T>(n: &Node, table: &'a [T], key: impl Fn(&T) -> (u32, &str)) -> Option<&'a T> {
    let code = epsg_id(n);
    let name = norm(n.title());
    code.and_then(|c| table.iter().find(|r| key(r).0 == c)).or_else(|| table.iter().find(|r| key(r).1 == name))
}

/// Axes whose signs differ from the East/North convention used by the WKT reader.
/// Polar axes follow their own meridians and are handled by the projection itself.
fn reversed_axes(p: &Node) -> bool {
    p.nodes()
        .filter(|a| a.name.eq_ignore_ascii_case("AXIS") && a.child("MERIDIAN").is_none())
        .any(|a| a.items.iter().any(|i| matches!(i, Item::Word(w) if w.eq_ignore_ascii_case("west") || w.eq_ignore_ascii_case("south"))))
}

/// A WKT2 projected system as a `PROJCS`, its parameters in degrees and the system's linear unit.
fn projected2(p: &Node) -> Result<Node, String> {
    // Axes pointing west or south (South Africa's Lo grids) would be read with the wrong sign, as
    // WKT1 has no way to say so; unless the system is read by its EPSG code. (Polar systems tie
    // their axes to a meridian, which is how WKT1 reads them.)
    // Axis order isn't read: as with WKT1, coordinates are easting then northing (GIS order),
    // whatever order the system declares.
    let flipped = reversed_axes(p);
    if flipped && epsg_id(p).and_then(Crs::from_epsg).is_none() {
        return Err("axes pointing west or south aren't supported".into());
    }
    let base = p.child_of(&["BASEGEOGCRS", "BASEGEODCRS"]).ok_or("the projected system has no base geographic system")?;
    let mut items = vec![Item::Node(geographic2(base, false)?)];
    let unit = cs_unit(p, &LENGTH_UNIT);
    let to_m = factor(unit).unwrap_or(1.0);
    if let Some(conv) = p.child("CONVERSION") {
        let method = conv.child_of(&["METHOD", "PROJECTION"]);
        let name = match method.and_then(|m| lookup(m, METHODS, |r| (r.0, r.1))) {
            Some(&(_, _, wkt1)) => wkt1,
            None => method.map_or("", Node::title),
        };
        items.push(Item::Node(Node::new("PROJECTION", name, [])));
        for param in conv.nodes().filter(|n| n.name.eq_ignore_ascii_case("PARAMETER")) {
            let Some(v) = param.num(0) else { continue };
            let (name, measure) = match lookup(param, PARAMETERS, |r| (r.0, r.1)) {
                Some(&(_, _, wkt1, m)) => (wkt1, Some(m)),
                None => (param.title(), None),
            };
            let angle = param.child("ANGLEUNIT");
            let length = param.child("LENGTHUNIT");
            let any = param.child_of(&["ANGLEUNIT", "LENGTHUNIT", "SCALEUNIT", "UNIT"]);
            let v = match measure {
                Some(Measure::Angle) => convert(v, factor(angle.or(any)), 1f64.to_radians()),
                Some(Measure::Length) => convert(v, factor(length.or(any)), to_m),
                Some(Measure::Scale) => convert(v, factor(any), 1.0),
                None if angle.is_some() => convert(v, factor(angle), 1f64.to_radians()),
                None => convert(v, factor(length), to_m),
            };
            items.push(Item::Node(Node::new("PARAMETER", name, [Item::Num(v)])));
        }
    }
    items.push(Item::Node(Node::new("UNIT", unit.map_or("metre", Node::title), [Item::Num(to_m)])));
    items.extend(axes(p));
    items.extend(authority(p));
    Ok(Node::new("PROJCS", p.title(), items))
}

/// The EPSG methods that rotate by the coordinate frame convention: geog2D, geocentric,
/// time-dependent and time-specific, full matrix, geog3D and geog3D to compound.
const FRAME_METHODS: [u32; 8] = [9607, 1032, 1056, 1057, 1066, 1133, 1140, 1149];

/// A `BOUNDCRS` (PROJ's form of a WKT1 `TOWGS84`) as its source system with the shift to WGS 84
/// of its transformation, in metres, arc-seconds and parts per million.
fn bound(b: Node, depth: usize) -> Result<Node, String> {
    let (mut source, mut transformation) = (None, None);
    for n in b.into_nodes() {
        if n.name.eq_ignore_ascii_case("SOURCECRS") {
            source = n.into_nodes().next();
        } else if n.name.eq_ignore_ascii_case("ABRIDGEDTRANSFORMATION") {
            transformation = Some(n);
        }
    }
    let mut crs = wkt1(source.ok_or("the bound coordinate system has no source")?, depth + 1)?;
    let Some(t) = transformation.as_ref() else { return Ok(crs) };
    let method = t.child("METHOD");
    let frame =
        method.is_some_and(|m| epsg_id(m).is_some_and(|c| FRAME_METHODS.contains(&c)) || norm(m.title()).contains("COORDINATE_FRAME"));
    // EPSG codes of the translations, rotations and scale difference.
    let get = |code: u32, name: &str, measure: Measure| {
        let p = t.nodes().find(|p| p.name.eq_ignore_ascii_case("PARAMETER") && (epsg_id(p) == Some(code) || norm(p.title()) == name))?;
        let v = p.num(0)?;
        let f = factor(p.child_of(&["LENGTHUNIT", "ANGLEUNIT", "SCALEUNIT", "UNIT"]));
        Some(match measure {
            Measure::Length => f.map_or(v, |f| v * f),
            Measure::Angle => f.map_or(v, |f| v * f / (1f64.to_radians() / 3600.0)),
            // The scale as a pure number, from its unit (`unity`, `parts per million`). Without
            // one, PROJ's BOUNDCRS writes the factor (1.000007) and EPSG would mean ppm; datum
            // scales are a few hundred ppm at most, so a value within 1% of 1 is the factor (1 +
            // the difference) and anything else the difference. A unitless ppm value within 1% of 1
            // (1.0 ppm) is misread as the factor (0 ppm): no writer is known to produce one.
            // (Rounded to a millionth of a ppm: 0.999979511 is -20.489, not -20.488999999956903.)
            Measure::Scale => {
                let s = match f {
                    Some(f) => v * f,
                    None if (v - 1.0).abs() < 0.01 => v,
                    None => v * 1e-6,
                };
                let d = if (s - 1.0).abs() < 0.01 { s - 1.0 } else { s };
                (d * 1e12).round() / 1e6
            }
        })
    };
    let shift = [
        get(8605, "X_AXIS_TRANSLATION", Measure::Length),
        get(8606, "Y_AXIS_TRANSLATION", Measure::Length),
        get(8607, "Z_AXIS_TRANSLATION", Measure::Length),
    ];
    let turn = [
        get(8608, "X_AXIS_ROTATION", Measure::Angle),
        get(8609, "Y_AXIS_ROTATION", Measure::Angle),
        get(8610, "Z_AXIS_ROTATION", Measure::Angle),
    ];
    let Some(mut towgs84) = shift.into_iter().collect::<Option<Vec<f64>>>() else { return Ok(crs) };
    let scale = get(8611, "SCALE_DIFFERENCE", Measure::Scale);
    if scale.is_some() || turn.iter().any(Option::is_some) {
        // Seven parameters, the ones not given none. TOWGS84 rotates by the position vector
        // convention, the opposite of coordinate frame.
        towgs84.extend(turn.into_iter().map(|r| r.map_or(0.0, |r| if frame { -r } else { r })));
        towgs84.push(scale.unwrap_or(0.0));
    }
    let geog = if crs.name.eq_ignore_ascii_case("GEOGCS") { Some(&mut crs) } else { crs.child_mut("GEOGCS") };
    if let Some(datum) = geog.and_then(|g| g.child_mut("DATUM")) {
        datum.items.push(Item::Node(Node { name: "TOWGS84".into(), items: towgs84.into_iter().map(Item::Num).collect() }));
    }
    Ok(crs)
}

/// The code in `AUTHORITY["EPSG","32631"]` (quoted or not).
fn authority_code(a: &Node) -> Option<u32> {
    match a.items.get(1)? {
        Item::Text(t) => t.trim().parse().ok(),
        Item::Num(n) if n.fract() == 0.0 && *n > 0.0 && *n <= f64::from(u32::MAX) => Some(*n as u32),
        _ => None,
    }
}

/// Longitude/latitude in degrees: WGS 84 itself for WGS 84, NAD 83, ETRS 89 and SIRGAS 2000
/// (which differ by a metre or so) from Greenwich, otherwise on the file's own datum and prime
/// meridian.
fn geographic(g: &Node) -> Result<Crs, String> {
    // A WKT1 system's own axes, when it gives them: longitudes counted west would be mirrored.
    if reversed_axes(g) {
        return Err("axes pointing west or south aren't supported".into());
    }
    if let Some(f) = g.child("UNIT").and_then(|u| u.num(0))
        && (f - 1f64.to_radians()).abs() > 1e-9
    {
        return Err(format!("angles in {} aren't supported", g.child("UNIT").map_or("", Node::title)));
    }
    let datum = g.child("DATUM");
    let greenwich = g.child("PRIMEM").and_then(|m| m.num(0)).is_none_or(|pm| pm == 0.0);
    if greenwich && (datum.is_none() || (wgs84_like(datum).is_some() && !shifted(datum))) {
        return Ok(Crs::LonLat);
    }
    let label = match g.title().trim() {
        "" => "Custom coordinate system".to_string(),
        t => t.to_string(),
    };
    Crs::custom(CustomCrs {
        proj4: format!("+proj=longlat {} +no_defs", earth(Some(g))),
        label,
        geokeys: Vec::new(),
        doubles: Vec::new(),
        ascii: String::new(),
    })
}

/// A projected system: by EPSG code for the common names, else as a PROJ definition.
fn projected(p: &Node) -> Result<Crs, String> {
    let name = norm(p.title());
    let method = norm(p.child("PROJECTION").map_or("", Node::title));
    if name.contains("WEB_MERCATOR")
        || name.contains("PSEUDO_MERCATOR")
        || method == "MERCATOR_AUXILIARY_SPHERE"
        || method == "POPULAR_VISUALISATION_PSEUDO_MERCATOR"
    {
        return Ok(Crs::WebMercator);
    }
    // (By its EPSG code only when the file adds no shift of its own to the datum.)
    if !shifted(p.child("GEOGCS").and_then(|g| g.child("DATUM")))
        && let Some(crs) = utm(&name).and_then(Crs::from_epsg)
    {
        return Ok(crs);
    }
    custom(p, &method)
}

/// The EPSG code of a UTM zone on WGS 84, NAD 83, ETRS 89 or SIRGAS 2000 from its name
/// (`WGS_1984_UTM_Zone_31N`, `WGS 84 / UTM zone 31N`, `NAD83 / UTM zone 17N`,
/// `SIRGAS_2000_UTM_Zone_24S`).
fn utm(name: &str) -> Option<u32> {
    let (datum, zone) = name.split_once("_UTM_ZONE_")?;
    let digits: String = zone.chars().take_while(char::is_ascii_digit).collect();
    let z: u32 = digits.parse().ok().filter(|z| (1..=60).contains(z))?;
    let north = match zone.get(digits.len()..)? {
        "N" => true,
        "S" => false,
        _ => return None,
    };
    match datum {
        "WGS_1984" | "WGS_84" => Some(if north { 32600 } else { 32700 } + z),
        "NAD_1983" | "NAD83" if north => Some(26900 + z),
        "ETRS_1989" | "ETRS89" if north => Some(25800 + z),
        "SIRGAS_2000" | "SIRGAS2000" => match (north, z) {
            (true, 11..=22) => Some(31954 + z),
            (true, 23) => Some(6210),
            (true, 24) => Some(6211),
            (false, 17..=25) => Some(31960 + z),
            (false, 26) => Some(5396),
            _ => None,
        },
        _ => None,
    }
}

/// The datum and zone of a UTM zone's EPSG code on WGS 84, NAD 83, ETRS 89 or SIRGAS 2000, the
/// codes [`utm`] reads names into: `31983` is `("SIRGAS 2000", 23, false)` (zone 23 south). Only
/// the codes EPSG defines (NAD 83 zones 1 to 23, ETRS 89 zones 28 to 38). For naming a map's
/// projection where its PROJ definition gives only an ellipsoid.
pub fn utm_of_code(code: u32) -> Option<(&'static str, u32, bool)> {
    Some(match code {
        32601..=32660 => ("WGS 84", code - 32600, true),
        32701..=32760 => ("WGS 84", code - 32700, false),
        26901..=26923 => ("NAD83", code - 26900, true),
        25828..=25838 => ("ETRS89", code - 25800, true),
        31965..=31976 => ("SIRGAS 2000", code - 31954, true),
        6210 => ("SIRGAS 2000", 23, true),
        6211 => ("SIRGAS 2000", 24, true),
        31977..=31985 => ("SIRGAS 2000", code - 31960, false),
        5396 => ("SIRGAS 2000", 26, false),
        _ => return None,
    })
}

/// A projection's PROJ definition built from its WKT parameters.
fn custom(p: &Node, method: &str) -> Result<Crs, String> {
    if method == "KROVAK" && reversed_axes(p) {
        return Err("axes pointing west or south aren't supported".into());
    }
    let params: Vec<(String, f64)> = p
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Node(n) if n.name.eq_ignore_ascii_case("PARAMETER") => Some((norm(n.title()), n.num(0)?)),
            _ => None,
        })
        .collect();
    let get = |names: &[&str]| names.iter().find_map(|n| params.iter().find(|(k, _)| k == n).map(|(_, v)| *v));
    // Metres per linear unit: false eastings and northings are in the system's units, PROJ's in
    // metres.
    let to_m = p.child("UNIT").and_then(|u| u.num(0)).filter(|m| *m > 0.0).unwrap_or(1.0);
    // (GDAL writes Esri WKT for some methods with EPSG's parameter names: `Latitude of natural
    // origin`.)
    let lat0 = get(&["LATITUDE_OF_ORIGIN", "LATITUDE_OF_CENTER", "LATITUDE_OF_NATURAL_ORIGIN"]).unwrap_or(0.0);
    let lon0 = get(&["CENTRAL_MERIDIAN", "LONGITUDE_OF_CENTER", "LONGITUDE_OF_ORIGIN", "LONGITUDE_OF_NATURAL_ORIGIN"]).unwrap_or(0.0);
    let x0 = get(&["FALSE_EASTING"]).unwrap_or(0.0) * to_m;
    let y0 = get(&["FALSE_NORTHING"]).unwrap_or(0.0) * to_m;
    let k0 = get(&["SCALE_FACTOR"]).unwrap_or(1.0);
    let lat1 = get(&["STANDARD_PARALLEL_1"]);
    let lat2 = get(&["STANDARD_PARALLEL_2"]);
    let at = format!("+lon_0={lon0} +x_0={x0} +y_0={y0}");
    // Whether the earth the definition gets is a sphere; Esri names its datums `D_…`.
    let sphere = proj4rs::Proj::from_proj_string(&format!("+proj=longlat {}", earth(p.child("GEOGCS"))))
        .is_ok_and(|e| e.ellipse_parameters().0 == e.ellipse_parameters().1);
    let esri = p
        .child("GEOGCS")
        .and_then(|g| g.child("DATUM"))
        .is_some_and(|d| d.title().trim().get(..2).is_some_and(|p| p.eq_ignore_ascii_case("D_")));
    // Hotine's oblique Mercator by its azimuth: `no_uoff` puts the origin at the natural origin
    // on the centre line (EPSG's variant A, WKT1's plain `Hotine_Oblique_Mercator`), else at the
    // centre (EPSG's variant B, `…_Azimuth_Center`). An azimuth of 90° (or 270°, the same
    // east-west centre line) isn't oblique, and the omerc formula is degenerate there in proj4rs
    // and PROJ alike (the Swiss case is `somerc` below), so refuse it instead of placing points
    // wrong.
    let omerc = |no_uoff: bool| -> Result<String, String> {
        let azimuth =
            get(&["AZIMUTH"]).ok_or_else(|| format!("the {} projection has no azimuth", p.child("PROJECTION").map_or("", Node::title)))?;
        if (azimuth.rem_euclid(180.0) - 90.0).abs() < 1e-10 {
            return Err("a Hotine projection with its azimuth at 90° (or 270°) isn't supported".into());
        }
        let gamma = get(&["RECTIFIED_GRID_ANGLE"]).unwrap_or(azimuth);
        Ok(format!(
            "+proj=omerc +lat_0={lat0} +lonc={lon0} +alpha={azimuth} +gamma={gamma} +k_0={k0} {} {at}",
            if no_uoff { "+no_uoff" } else { "" }
        ))
    };
    let proj = match method {
        "TRANSVERSE_MERCATOR" | "GAUSS_KRUGER" => format!("+proj=tmerc +lat_0={lat0} +k_0={k0} {at}"),
        // proj4rs supports the Swiss special case and, since 0.3, the azimuth forms in general;
        // the two-point one (no AZIMUTH) isn't supported. The Swiss system is the azimuth-90°
        // Hotine on a Swiss datum — the one azimuth the guard below refuses, because an oblique
        // Mercator at exactly 90° is either Swiss or degenerate.
        "HOTINE_OBLIQUE_MERCATOR_AZIMUTH_CENTER"
            if national_datum(p.child("GEOGCS").and_then(|g| g.child("DATUM")))
                .is_some_and(|d| Some(d) == epsg_proj4(4149) || Some(d) == epsg_proj4(4150))
                && get(&["AZIMUTH"]).is_some_and(|v| (v - 90.0).abs() < 1e-10)
                && get(&["RECTIFIED_GRID_ANGLE"]).is_none_or(|v| (v - 90.0).abs() < 1e-10) =>
        {
            format!("+proj=somerc +lat_0={lat0} +k_0={k0} {at}")
        }
        "HOTINE_OBLIQUE_MERCATOR_AZIMUTH_CENTER" => omerc(false)?,
        // ESRI's WKT1 name for EPSG's variant A (ArcGIS writes it to `.prj`); WKT2's variant A
        // arrives as the plain name through the METHODS table.
        "HOTINE_OBLIQUE_MERCATOR" | "HOTINE_OBLIQUE_MERCATOR_AZIMUTH_NATURAL_ORIGIN" if get(&["AZIMUTH"]).is_some() => omerc(true)?,
        // EPSG's methods 9806 and 9818 have no scale factor; PROJ's cass and poly ignore one.
        "CASSINI_SOLDNER" => format!("+proj=cass +lat_0={lat0} {at}"),
        "POLYCONIC" => format!("+proj=poly +lat_0={lat0} {at}"),
        "EQUAL_EARTH" => format!("+proj=eqearth {at}"),
        // The implementation fixes the cone axis and pseudo standard parallel. Refuse other
        // variants and axis conventions instead of treating their coordinates as East/North.
        "KROVAK"
            if national_datum(p.child("GEOGCS").and_then(|g| g.child("DATUM"))) == epsg_proj4(4156)
                && get(&["AZIMUTH"]).is_some_and(|v| (v - 30.28813975277778).abs() < 1e-6)
                && get(&["PSEUDO_STANDARD_PARALLEL_1"]).is_some_and(|v| (v - 78.5).abs() < 1e-10)
                && get(&["X_SCALE"]).is_none_or(|v| v == -1.0)
                && get(&["Y_SCALE"]).is_none_or(|v| v == 1.0)
                && get(&["XY_PLANE_ROTATION"]).is_none_or(|v| v == 90.0) =>
        {
            format!("+proj=krovak +lat_0={lat0} +k_0={k0} {at}")
        }
        // `Mercator_Variant_A` is Esri's name for EPSG's method 9804 (as GDAL writes the IAU's
        // Mercator systems); `Mercator_Spherical` is EPSG's 1026, the same on a sphere only.
        "MERCATOR_SPHERICAL" if !sphere => return Err("the Mercator (Spherical) projection on an ellipsoid isn't supported".into()),
        "MERCATOR" | "MERCATOR_1SP" | "MERCATOR_2SP" | "MERCATOR_VARIANT_A" | "MERCATOR_SPHERICAL" => match lat1 {
            Some(ts) => format!("+proj=merc +lat_ts={ts} {at}"),
            None => format!("+proj=merc +k_0={k0} {at}"),
        },
        "LAMBERT_CONFORMAL_CONIC" | "LAMBERT_CONFORMAL_CONIC_1SP" | "LAMBERT_CONFORMAL_CONIC_2SP" => {
            let l1 = lat1.unwrap_or(lat0);
            let l2 = lat2.unwrap_or(l1);
            format!("+proj=lcc +lat_1={l1} +lat_2={l2} +lat_0={lat0} +k_0={k0} {at}")
        }
        "ALBERS" | "ALBERS_CONIC_EQUAL_AREA" => {
            let l1 = lat1.unwrap_or(lat0);
            let l2 = lat2.unwrap_or(l1);
            format!("+proj=aea +lat_1={l1} +lat_2={l2} +lat_0={lat0} {at}")
        }
        "LAMBERT_AZIMUTHAL_EQUAL_AREA" => format!("+proj=laea +lat_0={lat0} {at}"),
        "AZIMUTHAL_EQUIDISTANT" => format!("+proj=aeqd +lat_0={lat0} {at}"),
        "STEREOGRAPHIC" => format!("+proj=stere +lat_0={lat0} +k_0={k0} {at}"),
        "OBLIQUE_STEREOGRAPHIC" | "DOUBLE_STEREOGRAPHIC" => format!("+proj=sterea +lat_0={lat0} +k_0={k0} {at}"),
        "POLAR_STEREOGRAPHIC" | "STEREOGRAPHIC_NORTH_POLE" | "STEREOGRAPHIC_SOUTH_POLE" => {
            // The pole on the side of the latitude of true scale. A latitude of origin at the pole
            // (UPS, EPSG:32761) gives the scale there instead.
            let ts = lat1.unwrap_or(lat0);
            let pole = if method.ends_with("SOUTH_POLE") || ts < 0.0 { -90.0 } else { 90.0 };
            if ts.abs() >= 90.0 || ts == 0.0 {
                format!("+proj=stere +lat_0={pole} +k_0={k0} {at}")
            } else {
                format!("+proj=stere +lat_0={pole} +lat_ts={ts} {at}")
            }
        }
        // EPSG's methods 1029 and 9823 are the spherical form: on a sphere the same, on an
        // ellipsoid not the ellipsoidal one (method 1028) the definition would get, so refused.
        "EQUIDISTANT_CYLINDRICAL_SPHERICAL" if !sphere => {
            return Err("the Equidistant Cylindrical (Spherical) projection on an ellipsoid isn't supported".into());
        }
        // A latitude of origin (PROJ's `+lat_0`, which GDAL writes as `latitude_of_origin`) moves
        // the northings' origin.
        "EQUIRECTANGULAR" | "EQUIDISTANT_CYLINDRICAL" | "PLATE_CARREE" | "EQUIDISTANT_CYLINDRICAL_SPHERICAL" if lat0 != 0.0 => {
            format!("+proj=eqc +lat_ts={} +lat_0={lat0} {at}", lat1.unwrap_or(0.0))
        }
        "EQUIRECTANGULAR" | "EQUIDISTANT_CYLINDRICAL" | "PLATE_CARREE" | "EQUIDISTANT_CYLINDRICAL_SPHERICAL" => {
            format!("+proj=eqc +lat_ts={} {at}", lat1.unwrap_or(0.0))
        }
        "CYLINDRICAL_EQUAL_AREA" | "LAMBERT_CYLINDRICAL_EQUAL_AREA" | "BEHRMANN" => {
            let ts = lat1.unwrap_or(if method == "BEHRMANN" { 30.0 } else { 0.0 });
            format!("+proj=cea +lat_ts={ts} {at}")
        }
        "MOLLWEIDE" => format!("+proj=moll {at}"),
        // Sinusoidal is computed by RustGIS (`rustgis_geom::Sinusoidal`), Robinson by proj4rs
        // from Robinson's table, on a sphere of the semi-major axis as in PROJ.
        "SINUSOIDAL" => format!("+proj=sinu {at}"),
        "ROBINSON" => format!("+proj=robin {at}"),
        // EPSG's method 9840, on the ellipsoid (proj4rs follows the EPSG Guidance Note 7-2). Esri's
        // Orthographic is a spherical one: on a sphere they agree, on an ellipsoid they don't, and
        // which one an Esri file means can't be told, so that is refused rather than guessed.
        "ORTHOGRAPHIC" | "ORTHOGRAPHIC_SPHERICAL" if !sphere && (esri || method == "ORTHOGRAPHIC_SPHERICAL") => {
            return Err("the Orthographic projection as Esri defines it (on a sphere) isn't supported on an ellipsoid".into());
        }
        "ORTHOGRAPHIC" | "ORTHOGRAPHIC_SPHERICAL" => format!("+proj=ortho +lat_0={lat0} {at}"),
        "MILLER_CYLINDRICAL" => format!("+proj=mill {at}"),
        "" => return Err("the projection isn't named".into()),
        _ => return Err(format!("the {} projection isn't supported", p.child("PROJECTION").map_or("", Node::title))),
    };
    let units = if (to_m - 1.0).abs() > 1e-12 { format!("+to_meter={to_m}") } else { "+units=m".to_string() };
    let label = match p.title().trim() {
        "" => "Custom projection".to_string(),
        t => t.to_string(),
    };
    Crs::custom(CustomCrs {
        proj4: format!("{proj} {} {units} +no_defs", earth(p.child("GEOGCS"))),
        label,
        geokeys: Vec::new(),
        doubles: Vec::new(),
        ascii: String::new(),
    })
}

/// The PROJ datum for WGS 84 and the datums that differ from it by a metre or so.
fn wgs84_like(datum: Option<&Node>) -> Option<&'static str> {
    // (WKT2 names a datum ensemble: `World Geodetic System 1984 ensemble`.)
    match norm(datum.map_or("", Node::title)).trim_start_matches("D_").trim_end_matches("_ENSEMBLE") {
        "WGS_1984" | "WGS84" | "WORLD_GEODETIC_SYSTEM_1984" => Some("+datum=WGS84"),
        // New Italian aliases require the published ellipsoid. GRS80 and the WGS84
        // ellipsoid in older IGM95 definitions differ by less than a millimetre.
        // MILITAIRE is the legacy spelling in crs-definitions 0.6's EPSG:3064/4670 WKT.
        "RETE_DINAMICA_NAZIONALE_2008" | "RDN2008" | "IGM_1995" | "IGM95"
        | "ISTITUTO_GEOGRAFICO_MILITARE_1995" | "ISTITUTO_GEOGRAFICO_MILITAIRE_1995" => datum
            .filter(|d| ellipsoid_matches(d, "+proj=longlat +ellps=GRS80"))
            .map(|_| "+ellps=GRS80 +towgs84=0,0,0"),
        "NORTH_AMERICAN_1983"
        | "NORTH_AMERICAN_DATUM_1983"
        | "NAD_1983"
        | "NAD83"
        | "ETRS_1989"
        | "ETRS89"
        | "EUROPEAN_TERRESTRIAL_REFERENCE_SYSTEM_1989"
        | "GDA_1994"
        | "GDA94"
        | "GEOCENTRIC_DATUM_OF_AUSTRALIA_1994"
        // GDA2020 (EPSG:7844): not in the list before, so it got the "no conversion" warning
        // though it's drawn in the right place, like GDA94.
        | "GDA_2020"
        | "GDA2020"
        | "GEOCENTRIC_DATUM_OF_AUSTRALIA_2020"
        // SIRGAS 2000, the datum of Brazil and most of Latin America (EPSG:4674): its
        // transformation to WGS 84 is null, like the others here.
        | "SIRGAS_2000"
        | "SIRGAS2000"
        | "SISTEMA_DE_REFERENCIA_GEOCENTRICO_PARA_LAS_AMERICAS_2000" => Some("+ellps=GRS80 +towgs84=0,0,0"),
        // MAGNA-SIRGAS, Colombia's densification of SIRGAS (EPSG:4686, and its 2018 realization):
        // null too, as the bundled EPSG:9377 (GRS 1980, no shift) has it (#1476), on that
        // ellipsoid only, so a definition naming another one keeps it.
        "MARCO_GEOCENTRICO_NACIONAL_DE_REFERENCIA"
        | "MARCO_GEOCENTRICO_NACIONAL_DE_REFERENCIA_2018"
        | "MAGNA"
        | "MAGNA_SIRGAS"
        | "MAGNA_SIRGAS_2018" => datum
            .filter(|d| ellipsoid_matches(d, "+proj=longlat +ellps=GRS80"))
            .map(|_| "+ellps=GRS80 +towgs84=0,0,0"),
        _ => None,
    }
}

/// A national datum's bundled EPSG definition. Match the datum, never just the projected
/// system's title, and require the published ellipsoid so a locally modified grid is not
/// silently assigned a different datum. These are the same Helmert approximations used when
/// the file supplies an EPSG code; regional grid transformations are not inferred.
fn national_datum(datum: Option<&Node>) -> Option<&'static str> {
    let datum = datum?;
    let name = norm(datum.title());
    let code = match name.strip_prefix("D_").unwrap_or(&name) {
        // Monte Mario (Rome), EPSG:6806, is the same datum measured from the Rome meridian: it
        // takes Monte Mario's shift, and `earth` keeps the file's prime meridian as `+pm`.
        "MONTE_MARIO" | "MONTE_MARIO_ROME" => 4265,
        "EUROPEAN_1950" | "EUROPEAN_DATUM_1950" | "ED50" => 4230,
        "OSGB_1936" | "OSGB36" | "ORDNANCE_SURVEY_OF_GREAT_BRITAIN_1936" => 4277,
        "DEUTSCHES_HAUPTDREIECKSNETZ" | "DHDN" => 4314,
        "AMERSFOORT" => 4289,
        "GGRS_1987" | "GGRS87" | "GREEK_GEODETIC_REFERENCE_SYSTEM_1987" => 4121,
        "CH1903" => {
            if datum.title().contains('+') {
                4150
            } else {
                4149
            }
        }
        "S_JTSK" | "SYSTEM_JEDNOTNE_TRIGONOMETRICKE_SITE_KATASTRALNI" | "SYSTEM_OF_THE_UNIFIED_TRIGONOMETRICAL_CADASTRAL_NETWORK" => 4156,
        _ => return None,
    };
    let def = epsg_proj4(code)?;
    ellipsoid_matches(datum, def).then_some(def)
}

/// Whether the file's ellipsoid axes agree with a known definition within a centimetre.
fn ellipsoid_matches(datum: &Node, def: &str) -> bool {
    let Ok(proj) = proj4rs::Proj::from_proj_string(def) else { return false };
    let Some(sph) = datum.child_of(&["SPHEROID", "ELLIPSOID"]) else { return false };
    let (Some(actual_a), Some(rf)) = (sph.num(0), sph.num(1)) else { return false };
    let (a, b) = proj.ellipse_parameters();
    let actual_b = if rf > 0.0 { actual_a * (1.0 - 1.0 / rf) } else { actual_a };
    (actual_a - a).abs() < 0.01 && (actual_b - b).abs() < 0.01
}

/// Whether the file gives the datum a `TOWGS84` shift other than none, which then wins over the
/// null shift a WGS 84-like datum would get by its name: the file says what it was made with.
fn shifted(datum: Option<&Node>) -> bool {
    towgs84(datum).is_some_and(|t| t.iter().any(|v| *v != 0.0))
}

/// The datum's `TOWGS84` shift, when it gives a usable one.
fn towgs84(datum: Option<&Node>) -> Option<Vec<f64>> {
    datum.and_then(|d| d.child("TOWGS84")).map(Node::nums).filter(|t| (t.len() == 3 || t.len() == 7) && t.iter().all(|v| v.is_finite()))
}

/// A note when the datum (of a `GEOGCS`) is neither WGS 84-like nor shifted to it by `TOWGS84`.
fn datum_note(geog: Option<&Node>) -> Option<String> {
    let datum = geog?.child("DATUM")?;
    if wgs84_like(Some(datum)).is_some() || towgs84(Some(datum)).is_some() || national_datum(Some(datum)).is_some() {
        return None;
    }
    Some(format!(
        "the datum {} has no conversion to WGS 84 in the file, so positions may be off by up to a few hundred metres",
        datum.title()
    ))
}

/// The datum of a `GEOGCS`, by name for the common ones, else by its spheroid's axes; and its
/// prime meridian.
fn earth(geog: Option<&Node>) -> String {
    let datum = geog.and_then(|g| g.child("DATUM"));
    let known = wgs84_like(datum).map(str::to_string).or_else(|| {
        national_datum(datum).map(|def| {
            def.split_whitespace()
                .filter(|p| p.starts_with("+ellps=") || p.starts_with("+datum=") || p.starts_with("+towgs84="))
                .collect::<Vec<_>>()
                .join(" ")
        })
    });
    // A national datum yields to any explicit TOWGS84, including a zero shift.
    // WGS 84-like datums can keep their equivalent named definition for a zero shift.
    let mut out = match known.filter(|_| towgs84(datum).is_none() || (wgs84_like(datum).is_some() && !shifted(datum))) {
        Some(d) => d.to_string(),
        None => {
            let sph = datum.and_then(|d| d.child("SPHEROID").or_else(|| d.child("ELLIPSOID")));
            let mut s = match (sph.and_then(|s| s.num(0)), sph.and_then(|s| s.num(1))) {
                (Some(a), Some(rf)) if a > 0.0 && rf > 0.0 => format!("+a={a} +rf={rf}"),
                (Some(a), _) if a > 0.0 => format!("+a={a} +b={a}"),
                _ => "+datum=WGS84".to_string(),
            };
            if let Some(t) = towgs84(datum) {
                s.push_str(&format!(" +towgs84={}", t.iter().map(f64::to_string).collect::<Vec<_>>().join(",")));
            }
            s
        }
    };
    if let Some(pm) = geog.and_then(|g| g.child("PRIMEM")).and_then(|m| m.num(0))
        && pm != 0.0
    {
        out.push_str(&format!(" +pm={pm}"));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Writing

/// WGS 84 longitude/latitude.
const WGS84_GEOGCS: &str = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AXIS["Longitude",EAST],AXIS["Latitude",NORTH],AUTHORITY["EPSG","4326"]]"#;

/// Web Mercator, as GDAL writes it.
const WEB_MERCATOR: &str = r#"PROJCS["WGS 84 / Pseudo-Mercator",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]],PROJECTION["Mercator_1SP"],PARAMETER["central_meridian",0],PARAMETER["scale_factor",1],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH],EXTENSION["PROJ4","+proj=merc +a=6378137 +b=6378137 +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +k=1 +units=m +nadgrids=@null +wktext +no_defs"],AUTHORITY["EPSG","3857"]]"#;

/// PROJ's datums by name: the WKT datum name, the ellipsoid's PROJ name and the shift to WGS 84
/// PROJ gives it.
const DATUMS: &[(&str, &str, &str, Option<&str>)] = &[
    ("WGS84", "WGS_1984", "WGS84", None),
    ("NAD83", "North_American_Datum_1983", "GRS80", Some("0,0,0")),
    ("NAD27", "North_American_Datum_1927", "clrk66", None),
    ("GGRS87", "Greek_Geodetic_Reference_System_1987", "GRS80", Some("-199.87,74.79,246.62")),
    ("potsdam", "Deutsches_Hauptdreiecksnetz", "bessel", Some("598.1,73.7,418.2,0.202,0.045,-2.455,6.7")),
    ("carthage", "Carthage", "clrk80ign", Some("-263.0,6.0,431.0")),
    ("hermannskogel", "Militar_Geographische_Institut", "bessel", Some("577.326,90.129,463.919,5.137,1.474,5.297,2.4232")),
    ("ire65", "TM65", "mod_airy", Some("482.530,-130.596,564.557,-1.042,-0.214,-0.631,8.15")),
    ("nzgd49", "New_Zealand_Geodetic_Datum_1949", "intl", Some("59.47,-5.04,187.44,0.47,-0.1,1.024,-4.5993")),
    ("OSGB36", "OSGB_1936", "airy", Some("446.448,-125.157,542.06,0.15,0.247,0.842,-20.489")),
];

/// The national datums the reader recognizes by name ([`national_datum`]), by the geographic
/// system whose ellipsoid and shift a definition has: named, the reader takes a projection that
/// needs its datum (the Swiss oblique Mercator, Krovak) from the parameters alone.
const NATIONAL_DATUMS: &[(u16, &str)] = &[
    (4149, "CH1903"),
    (4156, "System_Jednotne_Trigonometricke_Site_Katastralni"),
    (4265, "Monte_Mario"),
    (4230, "European_Datum_1950"),
    (4277, "OSGB_1936"),
    (4314, "Deutsches_Hauptdreiecksnetz"),
    (4289, "Amersfoort"),
    (4121, "Greek_Geodetic_Reference_System_1987"),
];

/// The WKT name of a definition's national datum, when its ellipsoid and shift are one of
/// [`NATIONAL_DATUMS`], however they're written (`+ellps=bessel` or its axes, three parameters or
/// seven).
fn national_datum_name(def: &str, code: Option<u16>) -> Option<&'static str> {
    let ((a, b), shift) = earth_of(def)?;
    let name = NATIONAL_DATUMS
        .iter()
        .find(|(c, _)| {
            epsg_proj4(*c).and_then(earth_of).is_some_and(|((na, nb), ns)| {
                (a - na).abs() < 0.01 && (b - nb).abs() < 0.01 && shift.iter().zip(ns).all(|(s, n)| (s - n).abs() < 1e-6)
            })
        })?
        .1;
    // CH1903+ has CH1903's ellipsoid and shift in the table: only their codes tell them apart.
    Some(if name == "CH1903" && matches!(code, Some(2056 | 4150)) { "CH1903+" } else { name })
}

/// A definition's ellipsoid axes and its shift to WGS 84 as seven parameters (three padded with
/// zeros), a named datum's shift included; `None` without a shift.
fn earth_of(def: &str) -> Option<((f64, f64), [f64; 7])> {
    let proj = proj4rs::Proj::from_proj_string(def).ok()?;
    let value = |k: &str| def.split_whitespace().find_map(|t| t.strip_prefix(k));
    let shift = value("+towgs84=").or_else(|| DATUMS.iter().find(|r| value("+datum=").is_some_and(|d| r.0.eq_ignore_ascii_case(d)))?.3)?;
    let parts: Vec<f64> = shift.split(',').map(|v| v.trim().parse().ok()).collect::<Option<_>>()?;
    if parts.len() != 3 && parts.len() != 7 {
        return None;
    }
    let mut seven = [0.0; 7];
    seven.iter_mut().zip(parts).for_each(|(s, p)| *s = p);
    Some((proj.ellipse_parameters(), seven))
}

/// PROJ's ellipsoids by name, with the name WKT gives them and their inverse flattening (0 for a
/// sphere), as EPSG gives it.
const ELLIPSOIDS: &[(&str, &str, f64)] = &[
    ("WGS84", "WGS 84", 298.257223563),
    ("GRS80", "GRS 1980", 298.257222101),
    ("WGS72", "WGS 72", 298.26),
    ("intl", "International 1924", 297.0),
    ("bessel", "Bessel 1841", 299.1528128),
    ("bess_nam", "Bessel Namibia (GLM)", 299.1528128),
    ("clrk66", "Clarke 1866", 294.978698213898),
    ("clrk80", "Clarke 1880 (RGS)", 293.465),
    ("clrk80ign", "Clarke 1880 (IGN)", 293.466021293627),
    ("krass", "Krassowsky 1940", 298.3),
    ("airy", "Airy 1830", 299.3249646),
    ("mod_airy", "Airy Modified 1849", 299.3249646),
    ("aust_SA", "Australian National Spheroid", 298.25),
    ("helmert", "Helmert 1906", 298.3),
    ("evrst30", "Everest 1830", 300.8017),
    ("evrstSS", "Everest 1830 (1967 Definition)", 300.8017),
    ("GRS67", "GRS 1967", 298.247167427),
    ("sphere", "Sphere", 0.0),
];

/// A WKT text value, its quotes doubled.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A number as WKT writes it: no `.0` on a whole number, no exponent for ordinary values.
fn num(v: f64) -> String {
    if v == 0.0 { "0".into() } else { format!("{v}") }
}

/// A coordinate system as WKT1 (OGC, the form `.prj` files, GeoPackages and FlatGeobuf hold),
/// built from its PROJ definition, with its EPSG code as the authority. `None` for a projection
/// it can't describe (one WKT1 has no name for).
pub fn to_wkt(crs: Crs) -> Option<String> {
    let (def, name, code) = match crs {
        Crs::LonLat => return Some(WGS84_GEOGCS.into()),
        Crs::WebMercator => return Some(WEB_MERCATOR.into()),
        Crs::Epsg(c) => (epsg_proj4(c)?.to_string(), format!("EPSG:{c}"), Some(c)),
        Crs::Custom(_) => {
            let d = crs.custom_def()?;
            (d.proj4.clone(), d.label.clone(), None)
        }
    };
    let proj = proj4rs::Proj::from_proj_string(&def).ok()?;
    let params: Vec<(&str, Option<&str>)> = def
        .split_whitespace()
        .filter_map(|t| t.strip_prefix('+'))
        .map(|t| t.split_once('=').map_or((t, None), |(k, v)| (k, Some(v))))
        .collect();
    let text = |k: &str| params.iter().find(|(n, _)| *n == k).and_then(|(_, v)| *v);
    let flag = |k: &str| params.iter().any(|(n, _)| *n == k);
    let get = |k: &str| text(k).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
    let authority = |c: Option<u16>| c.map(|c| format!(",AUTHORITY[\"EPSG\",\"{c}\"]")).unwrap_or_default();

    // The datum and its ellipsoid.
    let datum = text("datum").and_then(|d| DATUMS.iter().find(|r| r.0.eq_ignore_ascii_case(d)));
    let ellps = text("ellps").or(datum.map(|d| d.2));
    let known = ellps.and_then(|e| ELLIPSOIDS.iter().find(|r| r.0.eq_ignore_ascii_case(e)));
    let ellps_name = known.map_or("unnamed", |r| r.1);
    // proj4rs computes `eqc` on a sphere of the semi-major axis and gives that sphere, but RustGIS
    // computes it on the datum's ellipsoid (`crs::ellipsoidal_eqc`), the one written here.
    let (a, b) = match text("proj") {
        Some("eqc") => super::crs::eqc_geographic(&def).ok()?.ellipse_parameters(),
        _ => proj.ellipse_parameters(),
    };
    // A named ellipsoid's flattening as published; else from its axes (PROJ keeps a and b).
    let rf = match known {
        Some(k) if text("rf").is_none() && text("b").is_none() => k.2,
        _ if (a - b).abs() < 1e-9 => 0.0,
        _ => (a / (a - b) * 1e9).round() / 1e9,
    };
    let datum_name = match datum {
        Some(d) => d.1.to_string(),
        None => match national_datum_name(&def, code) {
            Some(n) => n.to_string(),
            None => format!("Unknown based on {ellps_name} ellipsoid"),
        },
    };
    // A null shift (ETRS89, NAD83, GDA94…) is left unsaid: readers would take the system as one
    // bound to WGS 84 rather than as itself.
    let towgs84 = text("towgs84")
        .or(datum.and_then(|d| d.3))
        .filter(|t| t.split(',').any(|v| v.trim().parse::<f64>().is_ok_and(|v| v != 0.0)))
        .map(|t| format!(",TOWGS84[{t}]"))
        .unwrap_or_default();
    // A grid shift or axes other than east and north have no plain WKT1 form.
    if flag("nadgrids") || text("axis").is_some_and(|a| a != "enu") {
        return None;
    }
    // A prime meridian by its longitude (`+pm=2.5`) or name (`+pm=paris`, which proj4rs resolves),
    // in degrees east of Greenwich.
    let pm = match get("pm") {
        Some(pm) => pm,
        None => proj.from_greenwich().to_degrees(),
    };
    if !pm.is_finite() {
        return None;
    }
    let pm_name = match text("pm").filter(|_| get("pm").is_none()) {
        Some(p) => p.get(..1).unwrap_or("").to_ascii_uppercase() + p.get(1..).unwrap_or(""),
        None if pm == 0.0 => "Greenwich".to_string(),
        None => "unnamed".to_string(),
    };
    let geog_name = match datum {
        Some(d) if d.0 == "WGS84" => "WGS 84".to_string(),
        _ => datum_name.trim_start_matches("Unknown based on ").to_string(),
    };
    let geogcs = |auth: &str| {
        format!(
            "GEOGCS[{},DATUM[{},SPHEROID[{},{},{}]{towgs84}],PRIMEM[{},{}],UNIT[\"degree\",0.0174532925199433]{auth}]",
            quoted(&geog_name),
            quoted(&datum_name),
            quoted(ellps_name),
            num(a),
            num(rf),
            quoted(&pm_name),
            num(pm),
        )
    };
    let kind = text("proj")?;
    if kind == "longlat" || kind == "latlong" {
        return Some(geogcs(&authority(code)));
    }

    // The projection and its parameters, in degrees and metres.
    let lat0 = get("lat_0").unwrap_or(0.0);
    let lon0 = get("lon_0").unwrap_or(0.0);
    let k0 = get("k_0").or(get("k")).unwrap_or(1.0);
    let to_m = proj.to_meter();
    let (x0, y0) = (get("x_0").unwrap_or(0.0), get("y_0").unwrap_or(0.0));
    let lat1 = get("lat_1");
    let lat2 = get("lat_2");
    let lat_ts = get("lat_ts");
    let mut ps: Vec<(&str, f64)>;
    let method = match kind {
        "utm" => {
            let zone = get("zone").filter(|z| (1.0..=60.0).contains(z))?;
            ps = vec![
                ("latitude_of_origin", 0.0),
                ("central_meridian", zone * 6.0 - 183.0),
                ("scale_factor", 0.9996),
                ("false_easting", 500_000.0),
                ("false_northing", if flag("south") { 10_000_000.0 } else { 0.0 }),
            ];
            "Transverse_Mercator"
        }
        "tmerc" => {
            ps = vec![("latitude_of_origin", lat0), ("central_meridian", lon0), ("scale_factor", k0)];
            "Transverse_Mercator"
        }
        "merc" => match lat_ts {
            Some(ts) => {
                ps = vec![("standard_parallel_1", ts), ("central_meridian", lon0)];
                "Mercator_2SP"
            }
            None => {
                ps = vec![("central_meridian", lon0), ("scale_factor", k0)];
                "Mercator_1SP"
            }
        },
        "lcc" => {
            let l1 = lat1.unwrap_or(lat0);
            match lat2.filter(|l2| (l2 - l1).abs() > 1e-12) {
                Some(l2) => {
                    ps = vec![
                        ("standard_parallel_1", l1),
                        ("standard_parallel_2", l2),
                        ("latitude_of_origin", lat0),
                        ("central_meridian", lon0),
                    ];
                    "Lambert_Conformal_Conic_2SP"
                }
                None => {
                    ps = vec![("latitude_of_origin", l1), ("central_meridian", lon0), ("scale_factor", k0)];
                    "Lambert_Conformal_Conic_1SP"
                }
            }
        }
        "aea" => {
            let l1 = lat1.unwrap_or(lat0);
            ps = vec![
                ("standard_parallel_1", l1),
                ("standard_parallel_2", lat2.unwrap_or(l1)),
                ("latitude_of_center", lat0),
                ("longitude_of_center", lon0),
            ];
            "Albers_Conic_Equal_Area"
        }
        "laea" => {
            ps = vec![("latitude_of_center", lat0), ("longitude_of_center", lon0)];
            "Lambert_Azimuthal_Equal_Area"
        }
        "aeqd" => {
            ps = vec![("latitude_of_center", lat0), ("longitude_of_center", lon0)];
            "Azimuthal_Equidistant"
        }
        "sterea" => {
            ps = vec![("latitude_of_origin", lat0), ("central_meridian", lon0), ("scale_factor", k0)];
            "Oblique_Stereographic"
        }
        "stere" if lat0.abs() == 90.0 => {
            ps = match lat_ts {
                Some(ts) => vec![("latitude_of_origin", ts), ("central_meridian", lon0)],
                None => vec![("latitude_of_origin", lat0), ("central_meridian", lon0), ("scale_factor", k0)],
            };
            "Polar_Stereographic"
        }
        "stere" => {
            ps = vec![("latitude_of_origin", lat0), ("central_meridian", lon0), ("scale_factor", k0)];
            "Stereographic"
        }
        "eqc" => {
            ps = vec![("standard_parallel_1", lat_ts.unwrap_or(0.0)), ("central_meridian", lon0)];
            if lat0 != 0.0 {
                ps.insert(0, ("latitude_of_origin", lat0));
            }
            "Equirectangular"
        }
        "cea" => {
            ps = vec![("standard_parallel_1", lat_ts.unwrap_or(0.0)), ("central_meridian", lon0)];
            "Cylindrical_Equal_Area"
        }
        "moll" => {
            ps = vec![("central_meridian", lon0)];
            "Mollweide"
        }
        "mill" => {
            ps = vec![("central_meridian", lon0)];
            "Miller_Cylindrical"
        }
        "sinu" => {
            ps = vec![("central_meridian", lon0)];
            "Sinusoidal"
        }
        "robin" => {
            ps = vec![("central_meridian", lon0)];
            "Robinson"
        }
        // OGC's name, which the reader takes as EPSG's ellipsoidal method 9840, as proj4rs
        // computes it.
        "ortho" => {
            ps = vec![("latitude_of_origin", lat0), ("central_meridian", lon0)];
            "Orthographic"
        }
        // The Swiss oblique Mercator is Hotine's, centred on its origin, with the azimuth and the
        // grid's angle at 90° (EPSG's method 9815), the form the reader takes it in.
        "somerc" => {
            ps = vec![
                ("latitude_of_center", lat0),
                ("longitude_of_center", lon0),
                ("azimuth", 90.0),
                ("rectified_grid_angle", 90.0),
                ("scale_factor", k0),
            ];
            "Hotine_Oblique_Mercator_Azimuth_Center"
        }
        // East and north only: EPSG:5513 and 2065 are the variants with axes pointing south and
        // west, which a `.prj` with these axes would contradict. proj4rs fixes the azimuth and
        // pseudo standard parallel, and has its own defaults for the centre and scale.
        "krovak" if !flag("czech") && !matches!(code, Some(5513 | 2065)) => {
            ps = vec![
                ("latitude_of_center", get("lat_0").unwrap_or(49.5)),
                ("longitude_of_center", get("lon_0").unwrap_or(24.833_333_333_333_33)),
                ("azimuth", 30.288_139_752_777_78),
                ("pseudo_standard_parallel_1", 78.5),
                ("scale_factor", get("k_0").or(get("k")).unwrap_or(0.9999)),
            ];
            "Krovak"
        }
        // Cassini-Soldner and the polyconic: an origin and a central meridian, no scale (EPSG's
        // methods 9806 and 9818 have none).
        "cass" => {
            ps = vec![("latitude_of_origin", lat0), ("central_meridian", lon0)];
            "Cassini_Soldner"
        }
        "poly" => {
            ps = vec![("latitude_of_origin", lat0), ("central_meridian", lon0)];
            "Polyconic"
        }
        // Equal Earth is younger than WKT1; GDAL's name for it.
        "eqearth" => {
            ps = vec![("central_meridian", lon0)];
            "Equal_Earth"
        }
        // Hotine's oblique Mercator by its azimuth, the general form proj4rs 0.3 projects (the
        // Swiss case is `somerc` above). EPSG's variant B puts the origin at the centre; its
        // variant A, PROJ's `+no_uoff`, at the natural origin on the centre line, which WKT1 says
        // with the plain name, as GDAL writes it.
        "omerc" if get("alpha").is_some() => {
            let alpha = get("alpha").unwrap_or(0.0);
            ps = vec![
                ("latitude_of_center", lat0),
                ("longitude_of_center", get("lonc").unwrap_or(lon0)),
                ("azimuth", alpha),
                ("rectified_grid_angle", get("gamma").unwrap_or(alpha)),
                ("scale_factor", k0),
            ];
            if flag("no_uoff") || flag("no_off") { "Hotine_Oblique_Mercator" } else { "Hotine_Oblique_Mercator_Azimuth_Center" }
        }
        _ => return None,
    };
    if kind != "utm" {
        // PROJ's false easting and northing are in metres, WKT's in the system's unit.
        ps.push(("false_easting", x0 / to_m));
        ps.push(("false_northing", y0 / to_m));
    }
    let unit = match text("units") {
        Some("m") | None if (to_m - 1.0).abs() < 1e-12 => "metre",
        Some("us-ft") => "US survey foot",
        Some("ft") => "foot",
        Some("km") => "kilometre",
        _ => "unknown",
    };
    let parameters: String = ps.iter().map(|(n, v)| format!(",PARAMETER[{},{}]", quoted(n), num(*v))).collect();
    Some(format!(
        "PROJCS[{},{},PROJECTION[{}]{parameters},UNIT[{},{}],AXIS[\"Easting\",EAST],AXIS[\"Northing\",NORTH]{}]",
        quoted(&name),
        geogcs(""),
        quoted(method),
        quoted(unit),
        num(to_m),
        authority(code),
    ))
}

#[cfg(test)]
mod tests {
    use rustgis_geom::Coord;

    use super::*;

    const GEOG_4326: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
    const ESRI_3857: &str = r#"PROJCS["WGS_1984_Web_Mercator_Auxiliary_Sphere",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Mercator_Auxiliary_Sphere"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",0.0],PARAMETER["Auxiliary_Sphere_Type",0.0],UNIT["Meter",1.0]]"#;
    const ESRI_32631: &str = r#"PROJCS["WGS_1984_UTM_Zone_31N",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",3.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;

    #[test]
    fn esri_names() {
        assert_eq!(crs_from_wkt(GEOG_4326), Ok(Crs::LonLat));
        assert_eq!(crs_from_wkt(ESRI_3857), Ok(Crs::WebMercator));
        assert_eq!(crs_from_wkt(ESRI_32631), Ok(Crs::Epsg(32631)));
        assert_eq!(utm("WGS_84_UTM_ZONE_17S"), Some(32717));
        assert_eq!(utm(&norm("NAD83 / UTM zone 17N")), Some(26917));
        assert_eq!(utm("ETRS_1989_UTM_ZONE_32N"), Some(25832));
        assert_eq!(utm("WGS_1984_UTM_ZONE_61N"), None);
    }

    /// The methods Esri and IAU `.prj` files name that the reader takes: Sinusoidal, Robinson,
    /// Orthographic and Mercator (variant A), in WKT1 and WKT2, with their parameters.
    #[test]
    fn sinusoidal_robinson_orthographic_and_mercator_variant_a() {
        let proj4 = |wkt: &str| crs_from_wkt(wkt).map(|c| c.custom_def().map(|d| d.proj4.clone()).unwrap_or_default());
        let esri = |method: &str, params: &str| format!(r#"PROJCS["x",{GEOG_4326},PROJECTION["{method}"],{params},UNIT["Meter",1.0]]"#);
        let common = r#"PARAMETER["False_Easting",100.0],PARAMETER["False_Northing",-200.0],PARAMETER["Central_Meridian",-75.0]"#;
        let p = proj4(&esri("Sinusoidal", common)).unwrap();
        assert!(p.starts_with("+proj=sinu +lon_0=-75 +x_0=100 +y_0=-200") && p.contains("+datum=WGS84"), "{p}");
        let p = proj4(&esri("Robinson", common)).unwrap();
        assert!(p.starts_with("+proj=robin +lon_0=-75 +x_0=100 +y_0=-200"), "{p}");
        let p = proj4(&esri("Mercator_Variant_A", &format!(r#"{common},PARAMETER["Scale_Factor",0.997]"#))).unwrap();
        assert!(p.starts_with("+proj=merc +k_0=0.997 +lon_0=-75"), "{p}");
        // Orthographic by its natural origin's names (as GDAL writes Esri WKT for the IAU's), or
        // Esri's centre: on a sphere.
        let sphere = GEOG_4326.replace("D_WGS_1984", "D_Sphere").replace("6378137.0,298.257223563", "6378137.0,0.0");
        for params in [
            r#"PARAMETER["Latitude of natural origin",40.0],PARAMETER["Longitude of natural origin",-100.0]"#,
            r#"PARAMETER["Latitude_Of_Center",40.0],PARAMETER["Longitude_Of_Center",-100.0]"#,
        ] {
            let wkt = esri("Orthographic", params).replace(GEOG_4326, &sphere);
            let p = proj4(&wkt).unwrap();
            assert!(p.starts_with("+proj=ortho +lat_0=40 +lon_0=-100") && p.contains("+a=6378137 +b=6378137"), "{p}");
        }

        // WKT2: EPSG's Orthographic (9840) on an ellipsoid; Esri's names without an ID.
        let wkt2 = |method: &str, params: &str, ellipsoid: &str| {
            format!(
                r#"PROJCRS["x",BASEGEOGCRS["g",DATUM["d",ELLIPSOID["e",{ellipsoid},LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]]],CONVERSION["c",{method},{params}PARAMETER["False easting",0,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1]]]"#
            )
        };
        let origin = r#"PARAMETER["Latitude of natural origin",55,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",5,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],"#;
        let p = proj4(&wkt2(r#"METHOD["Orthographic",ID["EPSG",9840]]"#, origin, "6378137,298.257223563")).unwrap();
        assert!(p.starts_with("+proj=ortho +lat_0=55 +lon_0=5") && p.contains("+a=6378137 +rf=298.257223563"), "{p}");
        let p = proj4(&wkt2(r#"METHOD["Robinson"]"#, origin, "6378137,298.257223563")).unwrap();
        assert!(p.starts_with("+proj=robin +lon_0=5"), "{p}");
        let p = proj4(&wkt2(r#"METHOD["Sinusoidal"]"#, origin, "3396190,0")).unwrap();
        assert!(p.starts_with("+proj=sinu +lon_0=5") && p.contains("+a=3396190 +b=3396190"), "{p}");
        // Mercator (Spherical), EPSG's 1026, on a sphere; refused on an ellipsoid, as is Esri's
        // (spherical) Orthographic.
        let spherical = r#"METHOD["Mercator (Spherical)",ID["EPSG",1026]]"#;
        let p = proj4(&wkt2(spherical, origin, "3396190,0")).unwrap();
        assert!(p.starts_with("+proj=merc +k_0=1 +lon_0=5"), "{p}");
        let e = proj4(&wkt2(spherical, origin, "6378137,298.257223563")).unwrap_err();
        assert!(e.contains("Mercator (Spherical) projection on an ellipsoid isn't supported"), "{e}");
        // Equidistant Cylindrical (Spherical), EPSG's 1029: the same on a sphere, refused on an
        // ellipsoid rather than computed as the ellipsoidal method 1028.
        let eqc_spherical = r#"METHOD["Equidistant Cylindrical (Spherical)",ID["EPSG",1029]]"#;
        let p = proj4(&wkt2(eqc_spherical, origin, "3396190,0")).unwrap();
        // Its latitude of natural origin (PROJ's `+lat_0`) is kept.
        assert!(p.starts_with("+proj=eqc +lat_ts=0 +lat_0=55 +lon_0=5"), "{p}");
        let e = proj4(&wkt2(eqc_spherical, origin, "6378137,298.257223563")).unwrap_err();
        assert!(e.contains("Equidistant Cylindrical (Spherical) projection on an ellipsoid isn't supported"), "{e}");
        let e = proj4(&wkt2(r#"METHOD["Orthographic (Spherical)"]"#, origin, "6378137,298.257223563")).unwrap_err();
        assert!(e.contains("isn't supported on an ellipsoid"), "{e}");
        let e =
            proj4(&esri("Orthographic", r#"PARAMETER["Latitude_Of_Center",40.0],PARAMETER["Longitude_Of_Center",-100.0]"#)).unwrap_err();
        assert!(e.contains("isn't supported on an ellipsoid"), "{e}");
    }

    /// The EPSG Guidance Note 7-2's example of Mercator (variant A), EPSG:3002 (Makassar / NEIEZ),
    /// as an Esri `.prj` names the method: 3°S 120°E on the Bessel 1841 ellipsoid is
    /// E = 5 009 726.58 m, N = 569 150.82 m. The same as Snyder 1987's ellipsoidal formulas
    /// (ch. 7, eqs. 7-6 and 7-7) give.
    #[test]
    fn mercator_variant_a_guidance_note_example() {
        let wkt = r#"PROJCS["Makassar_NEIEZ",GEOGCS["GCS_Makassar",DATUM["D_Makassar",SPHEROID["Bessel_1841",6377397.155,299.1528128]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Mercator_Variant_A"],PARAMETER["False_Easting",3900000.0],PARAMETER["False_Northing",900000.0],PARAMETER["Central_Meridian",110.0],PARAMETER["Scale_Factor",0.997],UNIT["Meter",1.0]]"#;
        let (crs, note) = read_wkt(wkt).unwrap();
        // Makassar has no shift to WGS 84 in the file: said, and left out, so longitude/latitude
        // on its own datum is what goes in.
        assert!(note.is_some_and(|n| n.contains("D_Makassar")));
        let m = crs.transformer().unwrap().from_lonlat(Coord::new(120.0, -3.0)).unwrap();
        assert!((m.x - 5_009_726.58).abs() < 0.01 && (m.y - 569_150.82).abs() < 0.01, "{m:?}");
        let (a, f) = (6_377_397.155f64, 1.0f64 / 299.152_812_8);
        let e = (f * (2.0 - f)).sqrt();
        let phi = -3f64.to_radians();
        let x = 3_900_000.0 + a * 0.997 * 10f64.to_radians();
        let y = 900_000.0
            + a * 0.997
                * ((std::f64::consts::FRAC_PI_4 + phi / 2.0).tan() * ((1.0 - e * phi.sin()) / (1.0 + e * phi.sin())).powf(e / 2.0)).ln();
        assert!((m.x - x).abs() < 0.001 && (m.y - y).abs() < 0.001, "{m:?} {x} {y}");
    }

    /// Sinusoidal, Robinson, orthographic and equidistant cylindrical systems are written back to
    /// WKT and read again as the same.
    #[test]
    fn new_methods_are_written_back() {
        for (def, lonlat) in [
            ("+proj=sinu +lon_0=15 +x_0=200000 +y_0=-100000 +datum=WGS84 +units=m +no_defs", (36.82, -1.29)),
            ("+proj=robin +lon_0=150 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs", (170.0, 40.0)),
            ("+proj=ortho +lat_0=55 +lon_0=5 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs", (2.13, 53.81)),
            ("+proj=ortho +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +a=3396190 +b=3396190 +units=m +no_defs", (30.0, 20.0)),
            // An equidistant cylindrical system's latitude of origin, on an ellipsoid and a sphere.
            ("+proj=eqc +lat_ts=30 +lat_0=10 +lon_0=10 +x_0=1000 +y_0=2000 +datum=WGS84 +units=m +no_defs", (12.0, 40.0)),
            ("+proj=eqc +lat_ts=0 +lat_0=-20 +lon_0=0 +x_0=0 +y_0=0 +R=3396190 +units=m +no_defs", (30.0, 20.0)),
            // An unnamed ellipsoid by its axes and flattening (Mars's IAU 2015 ellipsoid), which
            // proj4rs's own eqc replaces with a sphere: the ellipsoid is the one written.
            ("+proj=eqc +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +a=3396190 +rf=169.894447223612 +units=m +no_defs", (12.0, 40.0)),
            ("+proj=eqc +lat_ts=20 +lon_0=0 +x_0=0 +y_0=0 +a=6378137 +b=6356752.314245 +units=m +no_defs", (12.0, 40.0)),
        ] {
            let crs = Crs::parse(def).unwrap();
            let wkt = to_wkt(crs).unwrap_or_else(|| panic!("{def}"));
            let read = crs_from_wkt(&wkt).unwrap_or_else(|e| panic!("{def}: {e}: {wkt}"));
            let c = Coord::new(lonlat.0, lonlat.1);
            let (a, b) = (crs.transformer().unwrap().from_lonlat(c).unwrap(), read.transformer().unwrap().from_lonlat(c).unwrap());
            assert!((a.x - b.x).abs() < 0.001 && (a.y - b.y).abs() < 0.001, "{def}: {a:?} {b:?}\n{wkt}");
        }
    }

    #[test]
    fn epsg_authority() {
        let wkt = r#"PROJCS["anything",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4326"]],PROJECTION["Transverse_Mercator"],PARAMETER["central_meridian",-81],UNIT["metre",1],AXIS["Easting",EAST],AXIS["Northing",NORTH],AUTHORITY["EPSG","32617"]]"#;
        assert_eq!(crs_from_wkt(wkt), Ok(Crs::Epsg(32617)));
    }

    #[test]
    fn projection_parameters() {
        // NAD 1983 StatePlane New York Long Island FIPS 3104 (US feet), as ArcGIS writes it.
        let wkt = r#"PROJCS["NAD_1983_StatePlane_New_York_Long_Island_FIPS_3104_Feet",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Lambert_Conformal_Conic"],PARAMETER["False_Easting",984250.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-74.0],PARAMETER["Standard_Parallel_1",40.66666666666666],PARAMETER["Standard_Parallel_2",41.03333333333333],PARAMETER["Latitude_Of_Origin",40.16666666666666],UNIT["Foot_US",0.3048006096012192]]"#;
        let crs = crs_from_wkt(wkt).unwrap();
        assert!(matches!(crs, Crs::Custom(_)));
        assert_eq!(crs.label(), "NAD_1983_StatePlane_New_York_Long_Island_FIPS_3104_Feet");
        // Times Square, from PROJ: 988_267 ft E, 215_437 ft N.
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(988_267.0, 215_437.1)).unwrap();
        assert!((ll.x + 73.9855).abs() < 1e-5 && (ll.y - 40.758).abs() < 1e-5, "{ll:?}");

        // The UTM zone by its parameters, when the name isn't one known.
        let renamed = ESRI_32631.replace("WGS_1984_UTM_Zone_31N", "My_Grid");
        let crs = crs_from_wkt(&renamed).unwrap();
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(500_000.0, 5_000_000.0)).unwrap();
        assert!((ll.x - 3.0).abs() < 1e-6 && (ll.y - 45.153_477).abs() < 1e-5, "{ll:?}");
    }

    #[test]
    fn datums_and_prime_meridians() {
        // A prime meridian east of Greenwich moves longitudes east.
        let paris = r#"GEOGCS["Paris lon/lat",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Paris",2.33722917],UNIT["Degree",0.0174532925199433]]"#;
        let (crs, note) = read_wkt(paris).unwrap();
        assert!(matches!(crs, Crs::Custom(_)) && note.is_none());
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(0.0, 48.85)).unwrap();
        assert!((ll.x - 2.33722917).abs() < 1e-6 && (ll.y - 48.85).abs() < 1e-6, "{ll:?}");
        // NAD 27 with no TOWGS84: read, with a note.
        let nad27 = r#"GEOGCS["GCS_North_American_1927",DATUM["D_North_American_1927",SPHEROID["Clarke_1866",6378206.4,294.9786982]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        let (crs, note) = read_wkt(nad27).unwrap();
        assert!(crs.is_geographic());
        assert!(note.is_some_and(|n| n.contains("D_North_American_1927")));
        // With TOWGS84 there's nothing to say.
        let shifted = nad27.replace("294.9786982]]", "294.9786982],TOWGS84[-8,160,176,0,0,0,0]]");
        assert_eq!(read_wkt(&shifted).map(|r| r.1), Ok(None));
        // Esri names for WGS 84 systems need no note.
        assert_eq!(read_wkt(GEOG_4326), Ok((Crs::LonLat, None)));
        assert_eq!(read_wkt(ESRI_32631), Ok((Crs::Epsg(32631), None)));
    }

    /// A system kept with a layer says, as [`Crs::lacks_datum_shift`], what reading its WKT
    /// noted: the datum has no shift to WGS 84. Checked over both corpora, NAD 27 with and without
    /// TOWGS84, and a datum renamed off the national table.
    #[test]
    fn a_kept_system_says_its_datum_has_no_shift_as_reading_did() {
        let nad27 = r#"GEOGCS["GCS_North_American_1927",DATUM["D_North_American_1927",SPHEROID["Clarke_1866",6378206.4,294.9786982]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        let shifted = nad27.replace("294.9786982]]", "294.9786982],TOWGS84[-8,160,176,0,0,0,0]]");
        let prjs = include_str!("esri_datum_corpus.tsv").lines().filter(|l| !l.starts_with('#')).map(|l| l.split('\t').nth(5).unwrap());
        let local = prjs.clone().next().unwrap().replace("D_Monte_Mario", "D_Local");
        // An unknown datum on a sphere of WGS 84's major axis: noted, like any unknown datum.
        let sphere =
            r#"GEOGCS["Local",DATUM["Local",SPHEROID["Sphere",6378137,0]],PRIMEM["Greenwich",0],UNIT["Degree",0.0174532925199433]]"#;
        let mercator = format!(
            r#"PROJCS["Local Mercator",{sphere},PROJECTION["Mercator_1SP"],PARAMETER["central_meridian",0],PARAMETER["scale_factor",1],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["Meter",1]]"#
        );
        let wkts: Vec<String> = [nad27.to_string(), shifted, local, GEOG_4326.to_string(), sphere.to_string()]
            .into_iter()
            .chain(prjs.map(str::to_string))
            .chain(corpus().into_iter().map(|(_, _, w)| w.to_string()))
            .collect();
        // (Systems in the corpus that can't be read, such as Hotine Oblique Mercator variant B, are
        // passed over.)
        for (crs, note) in wkts.iter().filter_map(|w| read_wkt(w).ok()) {
            assert_eq!(crs.lacks_datum_shift(), note.is_some(), "{}: {note:?}", crs.label());
        }
        assert!(crs_from_wkt(nad27).unwrap().lacks_datum_shift());
        // The one exception: a Mercator on that sphere has Web Mercator's definition (typed as PROJ,
        // it can't take `+nadgrids=@null`) and converts exactly as Web Mercator does, so its export
        // says nothing, though reading it notes the unnamed datum. Web Mercator's own WKT reads as
        // EPSG:3857.
        let (crs, note) = read_wkt(&mercator).unwrap();
        assert!(note.is_some() && !crs.lacks_datum_shift());
        // A system known by its code, or typed without an ellipsoid, on WGS 84's or GRS 80's, or on
        // Web Mercator's sphere, has its shift; one typed on another ellipsoid without a datum hasn't.
        assert!(!Crs::LonLat.lacks_datum_shift() && !Crs::Epsg(32616).lacks_datum_shift());
        for (typed, lacks) in [
            ("+proj=utm +zone=16", false),
            ("+proj=utm +zone=16 +ellps=WGS84", false),
            ("+proj=utm +zone=16 +ellps=GRS80", false),
            ("+proj=utm +zone=16 +ellps=clrk66", true),
            ("+proj=merc +a=6378137 +b=6378137 +units=m", false),
            ("+proj=merc +R=6378137", false),
            ("+proj=merc +R=6378137.0 +units=m", false),
            ("+proj=longlat +R=6378137", true),
        ] {
            assert_eq!(Crs::parse(typed).unwrap().lacks_datum_shift(), lacks, "{typed}");
        }
    }

    #[test]
    fn sirgas_2000_reads_as_wgs_84_without_a_note() {
        // As FBDS ships its hydrography of Brazil (Esri names), projected and geographic.
        let utm_24s = r#"PROJCS["SIRGAS_2000_UTM_Zone_24S",GEOGCS["GCS_SIRGAS_2000",DATUM["D_SIRGAS_2000",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",10000000.0],PARAMETER["Central_Meridian",-39.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        assert_eq!(read_wkt(utm_24s), Ok((Crs::Epsg(31984), None)));
        let geographic = r#"GEOGCS["GCS_SIRGAS_2000",DATUM["D_SIRGAS_2000",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert_eq!(read_wkt(geographic), Ok((Crs::LonLat, None)));
        // The EPSG datum name, as OGC WKT writes it.
        let ogc = geographic.replace("D_SIRGAS_2000", "Sistema_de_Referencia_Geocentrico_para_las_AmericaS_2000");
        assert_eq!(read_wkt(&ogc), Ok((Crs::LonLat, None)));
        // Every SIRGAS 2000 UTM zone with an EPSG code, by its name.
        assert_eq!(utm(&norm("SIRGAS 2000 / UTM zone 11N")), Some(31965));
        assert_eq!(utm("SIRGAS_2000_UTM_ZONE_22N"), Some(31976));
        assert_eq!(utm("SIRGAS_2000_UTM_ZONE_23N"), Some(6210));
        assert_eq!(utm("SIRGAS_2000_UTM_ZONE_24N"), Some(6211));
        assert_eq!(utm("SIRGAS_2000_UTM_ZONE_17S"), Some(31977));
        assert_eq!(utm("SIRGAS_2000_UTM_ZONE_23S"), Some(31983));
        assert_eq!(utm("SIRGAS_2000_UTM_ZONE_25S"), Some(31985));
        assert_eq!(utm("SIRGAS_2000_UTM_ZONE_26S"), Some(5396));
        assert_eq!(utm("SIRGAS_2000_UTM_ZONE_10S"), None);
        for code in [31965, 31976, 6210, 6211, 31977, 31985, 5396] {
            assert!(Crs::from_epsg(code).is_some(), "EPSG:{code}");
        }
        // And back: every code's datum and zone name it again.
        for code in [32631, 32717, 26917, 25832, 31965, 31976, 6210, 6211, 31977, 31983, 31985, 5396] {
            let (datum, zone, north) = utm_of_code(code).unwrap_or_else(|| panic!("EPSG:{code}"));
            let name = format!("{}_UTM_ZONE_{zone}{}", datum.replace(' ', "_"), if north { "N" } else { "S" });
            assert_eq!(utm(&name), Some(code), "{name}");
        }
        assert_eq!(utm_of_code(31983), Some(("SIRGAS 2000", 23, false)));
        assert_eq!(utm_of_code(3857), None);
        // Only the zones EPSG defines: NAD 83 up to zone 23, ETRS 89 from 28 to 38.
        assert_eq!(utm_of_code(26923), Some(("NAD83", 23, true)));
        assert_eq!(utm_of_code(25838), Some(("ETRS89", 38, true)));
        for code in [26924, 26950, 25827, 25839, 25801] {
            assert_eq!(utm_of_code(code), None, "EPSG:{code}");
        }
        // A zone without an EPSG code is still read, by its parameters, and without a note.
        let zone_10s = utm_24s.replace("Zone_24S", "Zone_10S").replace("-39.0", "-123.0");
        let (crs, note) = read_wkt(&zone_10s).unwrap();
        assert!(matches!(crs, Crs::Custom(_)) && note.is_none());
        // A shift the file gives the datum wins over the null one its name would get, for every
        // WGS 84-like datum, geographic or projected; a zero shift changes nothing.
        let shift = r#"TOWGS84[-67.35,3.88,-38.22,0,0,0,0]"#;
        let with = |wkt: &str, datum: &str| wkt.replace(&format!(r#"DATUM["{datum}","#), &format!(r#"DATUM["{datum}",{shift},"#));
        for (wkt, datum) in [(geographic, "D_SIRGAS_2000"), (utm_24s, "D_SIRGAS_2000"), (GEOG_4326, "D_WGS_1984")] {
            let (crs, note) = read_wkt(&with(wkt, datum)).unwrap();
            let def = crs.custom_def().unwrap_or_else(|| panic!("{datum}: {crs:?}"));
            assert!(def.proj4.contains("+towgs84=-67.35,3.88,-38.22"), "{}", def.proj4);
            assert!(note.is_none());
        }
        let nad83 = r#"GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",TOWGS84[1,2,3],SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert!(read_wkt(nad83).unwrap().0.custom_def().is_some_and(|d| d.proj4.contains("+towgs84=1,2,3")));
        let zero = utm_24s.replace(r#"DATUM["D_SIRGAS_2000","#, r#"DATUM["D_SIRGAS_2000",TOWGS84[0,0,0,0,0,0,0],"#);
        assert_eq!(read_wkt(&zero), Ok((Crs::Epsg(31984), None)));
        // Brazil's older datums keep the note: without TOWGS84 they're tens of metres off.
        for (datum, spheroid) in [
            ("D_South_American_1969", r#"SPHEROID["GRS_1967_Truncated",6378160.0,298.25]"#),
            ("D_Corrego_Alegre", r#"SPHEROID["International_1924",6378388.0,297.0]"#),
        ] {
            let old = format!(r#"GEOGCS["GCS_old",DATUM["{datum}",{spheroid}],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#);
            assert!(read_wkt(&old).unwrap().1.is_some_and(|n| n.contains(datum)), "{datum}");
        }
    }

    #[test]
    fn gda2020_reads_as_wgs_84_without_a_note() {
        // As ArcGIS writes a GDA2020 .prj (Esri name).
        let geographic = r#"GEOGCS["GCS_GDA2020",DATUM["D_GDA2020",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert_eq!(read_wkt(geographic), Ok((Crs::LonLat, None)));
        // The EPSG datum name, as OGC WKT writes it.
        let ogc = geographic.replace("D_GDA2020", "Geocentric_Datum_of_Australia_2020");
        assert_eq!(read_wkt(&ogc), Ok((Crs::LonLat, None)));
    }

    #[test]
    fn esri_national_datums_match_epsg() {
        for line in include_str!("esri_datum_corpus.tsv").lines().filter(|l| !l.starts_with('#')) {
            let fields: Vec<_> = line.split('\t').collect();
            let code: u32 = fields[0].parse().unwrap();
            let number = |i: usize| fields[i].parse::<f64>().unwrap();
            let xy = Coord::new(number(1), number(2));
            let (crs, note) = read_wkt(fields[5]).unwrap_or_else(|e| panic!("EPSG:{code}: {e}"));
            assert!(note.is_none(), "EPSG:{code}: {note:?}");
            let actual = crs.transformer().unwrap().to_lonlat(xy).unwrap();
            let expected = Crs::from_epsg(code).unwrap().transformer().unwrap().to_lonlat(xy).unwrap();
            let distance = |a: Coord, b: Coord| ((a.x - b.x) * b.y.to_radians().cos()).hypot(a.y - b.y) * 111_195.0;
            assert!(distance(actual, expected) < 0.01, "EPSG:{code}: {actual:?} vs {expected:?}");
            assert!(distance(actual, Coord::new(number(3), number(4))) < 0.1, "EPSG:{code}: {actual:?}");
        }
    }

    #[test]
    fn national_datum_fallback_respects_file_definition() {
        let line = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with("3003\t")).unwrap();
        let wkt = line.split('\t').nth(5).unwrap();
        for shift in ["0,0,0", "1,2,3", "1,2,3,4,5,6,7"] {
            let explicit = wkt.replace(r#"DATUM["D_Monte_Mario","#, &format!(r#"DATUM["D_Monte_Mario",TOWGS84[{shift}],"#));
            let (crs, note) = read_wkt(&explicit).unwrap();
            let def = crs.custom_def().unwrap();
            assert!(def.proj4.contains(&format!("+towgs84={shift} ")), "{}", def.proj4);
            assert!(note.is_none());
        }
        // Neither the CRS title nor the ellipsoid alone identifies a datum. An altered
        // ellipsoid must not inherit a national datum's transformation either.
        for changed in
            [wkt.replace("D_Monte_Mario", "D_Local"), wkt.replace("D_Monte_Mario", "D_Roma_1940"), wkt.replace("6378388.0", "6378300.0")]
        {
            let (crs, note) = read_wkt(&changed).unwrap();
            assert!(note.is_some());
            assert!(!crs.custom_def().unwrap().proj4.contains("+towgs84="));
        }
        // A locally named projection with different false easting retains its parameters.
        let local = wkt.replace("Monte_Mario_Italy_1", "Local grid").replace("1500000.0", "1500100.0");
        let def = crs_from_wkt(&local).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+x_0=1500100") && def.proj4.contains("+towgs84="));
        // Geographic WKT uses the same fallback; explicit shifts still take precedence.
        let geog = parse(wkt).unwrap().child("GEOGCS").unwrap().clone();
        assert!(earth(Some(&geog)).contains("+towgs84=-104.1,-49.1,-9.9"));
        assert!(datum_note(Some(&geog)).is_none());
        // An altered azimuth isn't the Swiss special case: it reads as the oblique Mercator its
        // parameters say, never as Swiss. The Krovak mutations stay refused: proj4rs fixes the
        // cone axis and pseudo standard parallel.
        let swiss = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with("21781\t")).unwrap();
        let changed = swiss.split('\t').nth(5).unwrap().replace("90.0", "45.0");
        let def = crs_from_wkt(&changed).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+proj=omerc") && !def.proj4.contains("+proj=somerc"), "{}", def.proj4);
        // 270° is the same east-west centre line read the other way, and just as degenerate.
        let changed = swiss.split('\t').nth(5).unwrap().replace("90.0", "270.0");
        assert!(crs_from_wkt(&changed).is_err(), "{changed}");
        for (code, from, to) in [("5514", "78.5", "77.5"), ("5514", r#""X_Scale",-1.0"#, r#""X_Scale",1.0"#)] {
            let row = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with(&format!("{code}\t"))).unwrap();
            let changed = row.split('\t').nth(5).unwrap().replace(from, to);
            assert!(crs_from_wkt(&changed).is_err(), "{changed}");
        }
    }

    /// Monte Mario (Rome), EPSG:4806, as GDAL writes it to a `.prj` (`Monte_Mario_(Rome)`) and as
    /// WKT2 names it (`Monte Mario (Rome)`): Monte Mario's shift, with the Rome prime meridian.
    #[test]
    fn monte_mario_rome_takes_the_monte_mario_shift() {
        let shift = epsg_proj4(4265).unwrap().split_whitespace().find(|p| p.starts_with("+towgs84=")).unwrap();
        let esri = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with("4806\t")).unwrap();
        let wkt2 = include_str!("wkt2_corpus.tsv").lines().find(|l| l.starts_with("EPSG:4806\t")).unwrap();
        for wkt in [esri, wkt2].map(|l| l.split('\t').nth(5).unwrap()) {
            let (crs, note) = read_wkt(wkt).unwrap();
            assert!(note.is_none(), "{note:?}");
            let proj4 = &crs.custom_def().unwrap().proj4;
            assert!(proj4.contains(shift) && proj4.contains("+pm=12.4523333333333"), "{proj4}");
        }
    }

    #[test]
    fn italian_datums_require_their_ellipsoid() {
        for datum in ["D_Rete_Dinamica_Nazionale_2008", "D_IGM_1995", "Istituto_Geografico_Militaire_1995"] {
            let geographic = GEOG_4326.replace("D_WGS_1984", datum).replace("6378137.0", "6378000.0");
            let (crs, note) = read_wkt(&geographic).unwrap();
            assert!(note.is_some(), "{datum}");
            let def = crs.custom_def().unwrap();
            assert!(def.proj4.contains("+a=6378000") && !def.proj4.contains("+towgs84="), "{}", def.proj4);
        }
        for code in ["6707", "3064"] {
            let row = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with(&format!("{code}\t"))).unwrap();
            let projected = row.split('\t').nth(5).unwrap().replace("6378137.0", "6378000.0");
            let (crs, note) = read_wkt(&projected).unwrap();
            assert!(note.is_some(), "{code}");
            let def = crs.custom_def().unwrap();
            assert!(def.proj4.contains("+a=6378000") && !def.proj4.contains("+towgs84="), "{}", def.proj4);
        }
    }

    #[test]
    fn swiss_projection_requires_a_swiss_datum() {
        let row = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with("21781\t")).unwrap();
        let wkt = row.split('\t').nth(5).unwrap();
        for datum in ["D_Local", "D_OSGB_1936"] {
            assert!(crs_from_wkt(&wkt.replace("D_CH1903", datum)).is_err());
        }
        for datum in ["Istituto_Geografico_Militare_1995", "Istituto_Geografico_Militaire_1995"] {
            assert_eq!(read_wkt(&GEOG_4326.replace("D_WGS_1984", datum)), Ok((Crs::LonLat, None)));
        }
    }

    #[test]
    fn krovak_south_west_axes_are_refused() {
        let row = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with("5514\t")).unwrap();
        let wkt = row.split('\t').nth(5).unwrap();
        let changed = wkt.replace(r#",PARAMETER["X_Scale",-1.0],PARAMETER["Y_Scale",1.0],PARAMETER["XY_Plane_Rotation",90.0]"#, "");
        let changed = format!(r#"{},AXIS["Southing",SOUTH],AXIS["Westing",WEST]]"#, changed.strip_suffix(']').unwrap());
        assert_eq!(crs_from_wkt(&changed), Err("axes pointing west or south aren't supported".into()));
    }

    #[test]
    fn polar_stereographic() {
        // GDAL's WKT for EPSG:3031 (true scale at 71°S) and EPSG:32761 (UPS South, scale 0.994 at
        // the pole), without their authorities; the point is 45°E 75°S, from PROJ.
        let geog = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#;
        let antarctic = format!(
            r#"PROJCS["Antarctic",{geog},PROJECTION["Polar_Stereographic"],PARAMETER["latitude_of_origin",-71],PARAMETER["central_meridian",0],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1]]"#
        );
        let ups = format!(
            r#"PROJCS["UPS South",{geog},PROJECTION["Polar_Stereographic"],PARAMETER["latitude_of_origin",-90],PARAMETER["central_meridian",0],PARAMETER["scale_factor",0.994],PARAMETER["false_easting",2000000],PARAMETER["false_northing",2000000],UNIT["metre",1]]"#
        );
        for (wkt, xy) in [(antarctic, 1_158_794.740_772_6), (ups, 3_184_085.797_412_3)] {
            let ll = crs_from_wkt(&wkt).unwrap().transformer().unwrap().to_lonlat(Coord::new(xy, xy)).unwrap();
            assert!((ll.x - 45.0).abs() < 1e-6 && (ll.y + 75.0).abs() < 1e-6, "{ll:?}");
        }
    }

    /// The rows of the WKT2 corpus: code, x, y, longitude, latitude, WKT2 without its ID.
    fn corpus() -> Vec<(&'static str, [f64; 4], &'static str)> {
        include_str!("wkt2_corpus.tsv")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                let n = |i: usize| f[i].parse::<f64>().unwrap();
                (f[0], [n(1), n(2), n(3), n(4)], f[5])
            })
            .collect()
    }

    /// Every system in the corpus is read from its WKT2: by its EPSG code when the text gives
    /// it, and from its parameters to within a metre of PROJ when it doesn't.
    #[test]
    fn wkt2_corpus() {
        // Projections RustGIS builds no PROJ definition for: read by their EPSG code only, as in
        // WKT1.
        let by_code_only = ["EPSG:5514"];
        let rows = corpus();
        assert_eq!(rows.len(), 84);
        for (code, [x, y, lon, lat], wkt) in rows {
            if let Some(n) = code.strip_prefix("EPSG:") {
                let with_id = format!("{},ID[\"EPSG\",{n}]]", wkt.strip_suffix(']').unwrap());
                let expected = Crs::from_epsg(n.parse().unwrap()).unwrap();
                assert_eq!(crs_from_wkt(&with_id), Ok(expected), "{code}");
                assert_eq!(Crs::parse(&with_id), Ok(expected), "{code}");
            }
            if by_code_only.contains(&code) {
                assert!(crs_from_wkt(wkt).is_err_and(|e| e.contains("isn't supported")), "{code}");
                continue;
            }
            let crs = crs_from_wkt(wkt).unwrap_or_else(|e| panic!("{code}: {e}"));
            let ll = crs.transformer().unwrap().to_lonlat(Coord::new(x, y)).unwrap();
            let dlon = (ll.x - lon + 540.0).rem_euclid(360.0) - 180.0;
            let metres = (dlon * lat.to_radians().cos()).hypot(ll.y - lat) * 111_195.0;
            assert!(metres < 1.0, "{code}: {metres} m off, {:?}", crs.custom_def().map(|d| d.proj4.clone()));
            // Degrees come through as written, not as 49.000000000000014.
            assert!(crs.custom_def().is_none_or(|d| !d.proj4.contains("00000000")), "{code}: {:?}", crs.custom_def());
        }
    }

    #[test]
    fn wkt2_datums() {
        let rows = corpus();
        let row = |code: &str| rows.iter().find(|r| r.0 == code).map(|r| r.2).unwrap();
        // The WGS 84 ensemble, NAD 83 and GDA 94 are WGS 84 itself, with no note.
        assert_eq!(read_wkt(row("EPSG:4326")), Ok((Crs::LonLat, None)));
        assert_eq!(read_wkt(row("EPSG:32631")), Ok((Crs::Epsg(32631), None)));
        assert_eq!(read_wkt(row("EPSG:3857")), Ok((Crs::WebMercator, None)));
        assert_eq!(read_wkt(row("EPSG:4283")), Ok((Crs::LonLat, None)));
        // OSGB 36 gets the same bundled Helmert shift as its EPSG definition.
        assert!(read_wkt(row("EPSG:27700")).unwrap().1.is_none());
        // A geocentric system isn't a map's.
        let geocentric = r#"GEODCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[Cartesian,3],AXIS["(X)",geocentricX],AXIS["(Y)",geocentricY],AXIS["(Z)",geocentricZ],LENGTHUNIT["metre",1]]"#;
        assert_eq!(crs_from_wkt(geocentric), Err("Cartesian coordinate systems aren't supported".into()));
        // A compound system is read by its horizontal part.
        let compound = format!(
            r#"COMPOUNDCRS["UTM 31N + height",{},VERTCRS["EGM2008 height",VDATUM["EGM2008 geoid"],CS[vertical,1],AXIS["gravity-related height (H)",up,LENGTHUNIT["metre",1]]],ID["EPSG",9999]]"#,
            row("EPSG:32631")
        );
        assert_eq!(read_wkt(&compound), Ok((Crs::Epsg(32631), None)));
    }

    #[test]
    fn wkt2_axes_west_and_south() {
        // Hartebeesthoek94 / Lo19, as PROJ writes it, but as a plain Transverse Mercator: its
        // westing and southing would come out mirrored.
        let lo19 = r#"PROJCRS["Hartebeesthoek94 / Lo19",BASEGEOGCRS["Hartebeesthoek94",DATUM["Hartebeesthoek94",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4148]],CONVERSION["South African Survey Grid zone 19",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",19,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",1,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",0,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["westing (Y)",west,ORDER[1],LENGTHUNIT["metre",1]],AXIS["southing (X)",south,ORDER[2],LENGTHUNIT["metre",1]]]"#;
        assert_eq!(crs_from_wkt(lo19), Err("axes pointing west or south aren't supported".into()));
        // By an EPSG code that's known, it's read as that code; by an unknown one, refused.
        let lo19_code = Crs::from_epsg(2048).unwrap();
        assert_eq!(crs_from_wkt(&format!(r#"{},ID["EPSG",2048]]"#, lo19.strip_suffix(']').unwrap())), Ok(lo19_code));
        assert!(crs_from_wkt(&format!(r#"{},ID["EPSG",1]]"#, lo19.strip_suffix(']').unwrap())).is_err());
        // Polar axes along meridians are read (EPSG:3413's point south).
        let rows = corpus();
        let polar = rows.iter().find(|r| r.0 == "EPSG:3413").unwrap().2;
        assert!(polar.contains("south,MERIDIAN") && crs_from_wkt(polar).is_ok());
    }

    /// PROJ's `+towgs84` as WKT2: a `BOUNDCRS` with the shift to WGS 84 as its transformation.
    #[test]
    fn wkt2_bound_crs() {
        let wkt = include_str!("wkt2_corpus.tsv").lines().find_map(|l| l.strip_prefix("#bound\t")).unwrap();
        let (crs, note) = read_wkt(wkt).unwrap();
        assert!(note.is_none());
        let def = crs.custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489"), "{}", def.proj4);
        // Big Ben, from PROJ through the same shift.
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(530_000.0, 180_000.0)).unwrap();
        let metres = ((ll.x + 0.128_353_940_479_469_35) * 51.5f64.to_radians().cos()).hypot(ll.y - 51.503_990_827_633_78) * 111_195.0;
        assert!(metres < 1.0, "{metres} m off");
        // The coordinate frame convention turns the other way.
        let frame = wkt.replace(
            r#""Position Vector transformation (geog2D domain)",ID["EPSG",9606]"#,
            r#""Coordinate Frame rotation (geog2D domain)",ID["EPSG",9607]"#,
        );
        let def = crs_from_wkt(&frame).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=446.448,-125.157,542.06,-0.15,-0.247,-0.842,-20.489"), "{}", def.proj4);
        // Every coordinate frame method, by its EPSG code, turns the other way; position vector
        // methods don't.
        for (code, sign) in FRAME_METHODS.iter().map(|c| (*c, "-")).chain([(9606, ""), (1033, ""), (1037, "")]) {
            let m =
                wkt.replace(r#""Position Vector transformation (geog2D domain)",ID["EPSG",9606]"#, &format!(r#""m",ID["EPSG",{code}]"#));
            let def = crs_from_wkt(&m).unwrap().custom_def().unwrap();
            assert!(def.proj4.contains(&format!(",{sign}0.15,{sign}0.247,{sign}0.842,")), "{code}: {}", def.proj4);
        }
        // A three-parameter shift, in a unit of its own.
        let three = r#"BOUNDCRS[SOURCECRS[GEOGCRS["NAD27",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898]],PRIMEM["Greenwich",0],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]],ABRIDGEDTRANSFORMATION["NAD27 to WGS 84",METHOD["Geocentric translations (geog2D domain)",ID["EPSG",9603]],PARAMETER["X-axis translation",-8,ID["EPSG",8605]],PARAMETER["Y-axis translation",16000,LENGTHUNIT["centimetre",0.01],ID["EPSG",8606]],PARAMETER["Z-axis translation",176,ID["EPSG",8607]]]]"#;
        // A unitless scale near 1 is PROJ's factor, otherwise parts per million.
        for (scale, ppm) in [
            ("1.000007", "7"),
            ("0.6", "0.6"),
            ("-3", "-3"),
            (r#"1.000007,SCALEUNIT["unity",1]"#, "7"),
            (r#"7,SCALEUNIT["parts per million",1E-06]"#, "7"),
            // The edges of the factor form: within 1% of 1, a factor; past it, ppm.
            ("0.995", "-5000"),
            ("1.0", "0"),
            ("1.02", "1.02"),
        ] {
            let seven = wkt.replace("0.999979511", scale);
            let def = crs_from_wkt(&seven).unwrap().custom_def().unwrap();
            assert!(def.proj4.contains(&format!("0.842,{ppm} ")), "{scale}: {}", def.proj4);
        }
        // Rotations without a scale, or the reverse: the missing ones are none.
        let no_scale = wkt.replace(r#",PARAMETER["Scale difference",0.999979511,ID["EPSG",8611]]"#, "");
        let def = crs_from_wkt(&no_scale).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,0 "), "{}", def.proj4);
        let no_turn = wkt.replace(r#",PARAMETER["Y-axis rotation",0.247,ID["EPSG",8609]]"#, "");
        let def = crs_from_wkt(&no_turn).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=446.448,-125.157,542.06,0.15,0,0.842,-20.489 "), "{}", def.proj4);
        let (crs, note) = read_wkt(three).unwrap();
        assert!(note.is_none() && crs.custom_def().is_some_and(|d| d.proj4.contains("+towgs84=-8,160,176 ")), "{:?}", crs.custom_def());
    }

    #[test]
    fn hostile_text() {
        for bad in
            ["", "garbage", "PROJCS[", "PROJCS[\"x\"", "PROJCS[\"x\",PROJECTION[\"Hotine\"]]", "GEOGCS[\"x\"] trailing", "VERT_CS[\"x\"]"]
        {
            assert!(crs_from_wkt(bad).is_err(), "{bad}");
        }
        // WKT2 with parts missing, or nested past what's read.
        for bad in [
            "PROJCRS[\"x\"]",
            "PROJCRS[\"x\",BASEGEOGCRS[\"y\"]]",
            "PROJCRS[\"x\",BASEGEOGCRS[\"y\"],CONVERSION[\"c\",METHOD[\"Krovak\"]]]",
            "BOUNDCRS[]",
            "BOUNDCRS[SOURCECRS[]]",
            "COMPOUNDCRS[\"x\",VERTCRS[\"h\"]]",
            "GEOGCRS[\"x\",CS[Cartesian,3]]",
            "GEOGCRS[\"x\",ANGLEUNIT[\"grad\",0.015707963267949]]",
            "VERTCRS[\"h\"]",
            "GEOGCRS[\"x\",DATUM[\"d\",ELLIPSOID[\"e\",\"298.25\"]]]",
        ] {
            assert!(crs_from_wkt(bad).is_err(), "{bad}");
        }
        let nested = format!("{}GEOGCRS[\"x\"]{}", "COMPOUNDCRS[\"c\",".repeat(8), "]".repeat(8));
        assert!(crs_from_wkt(&nested).is_err());
        let nested = format!("{}GEOGCRS[\"x\"]{}", "BOUNDCRS[SOURCECRS[".repeat(8), "]]".repeat(8));
        assert!(crs_from_wkt(&nested).is_err());
        // Values out of range are read or refused, never a crash.
        for odd in [
            r#"PROJCRS["x",BASEGEOGCRS["y",DATUM["d",ELLIPSOID["e",-1,0,LENGTHUNIT["m",-5]]],PRIMEM["p",1e308,ANGLEUNIT["a",1e308]]],CONVERSION["c",METHOD["Transverse Mercator"],PARAMETER["False easting",1e308,LENGTHUNIT["x",1e308]],PARAMETER["Latitude of natural origin",1e300,ANGLEUNIT["x",1e300]]],LENGTHUNIT["u",0]]"#,
            r#"BOUNDCRS[SOURCECRS[GEOGCRS["x"]],ABRIDGEDTRANSFORMATION["t",PARAMETER["X-axis translation",1e308,LENGTHUNIT["m",1e308]],PARAMETER["Y-axis translation",0],PARAMETER["Z-axis translation",0]]]"#,
            r#"GEOGCRS["x",ID["EPSG",-4326]]"#,
            r#"GEOGCRS["x",ID["EPSG",99999999999]]"#,
        ] {
            let _ = read_wkt(odd);
        }
        let deep = format!("{}{}", "A[".repeat(10_000), "]".repeat(10_000));
        assert!(crs_from_wkt(&deep).is_err());
        assert_eq!(crs_from_wkt(r#"GEOGCS["a ""quoted"" name",UNIT["Degree",0.0174532925199433]]"#), Ok(Crs::LonLat));
    }

    #[test]
    fn written_wkt_reads_back_as_the_same_system() {
        // UTM (with and without the zone prefixed to the easting), transverse Mercator with a
        // datum shift, Lambert conformal in US feet, Albers, Lambert azimuthal, polar
        // stereographic, Mercator and geographic systems; each with a place in its area.
        let systems = [
            (25832u32, (10.0, 50.0)),
            (4647, (10.0, 50.0)),
            (32617, (-81.0, 42.0)),
            (32735, (27.0, -10.0)),
            (27700, (-1.5, 52.0)),
            (31467, (10.0, 50.0)),
            (2263, (-73.98, 40.75)),
            (5070, (-95.0, 40.0)),
            (3035, (10.0, 50.0)),
            (3413, (-45.0, 75.0)),
            (3395, (10.0, 50.0)),
            (4258, (10.0, 50.0)),
            (4269, (-95.0, 40.0)),
        ];
        for (code, (lon, lat)) in systems {
            let crs = Crs::from_epsg(code).unwrap();
            let wkt = to_wkt(crs).unwrap_or_else(|| panic!("EPSG:{code}"));
            assert!(wkt.ends_with(&format!("AUTHORITY[\"EPSG\",\"{code}\"]]")), "{wkt}");
            assert_eq!(crs_from_wkt(&wkt), Ok(crs), "{wkt}");
            // Without its code, the parameters alone place points where the system does.
            let bare = wkt.replace(&format!(",AUTHORITY[\"EPSG\",\"{code}\"]"), "");
            let read = crs_from_wkt(&bare).unwrap_or_else(|e| panic!("EPSG:{code}: {e}: {bare}"));
            let lonlat = Coord::new(lon, lat);
            let a = crs.transformer().unwrap().from_lonlat(lonlat).unwrap();
            let b = read.transformer().unwrap().from_lonlat(lonlat).unwrap();
            let tolerance = if crs.is_geographic() { 1e-8 } else { 0.01 };
            assert!((a.x - b.x).abs() < tolerance && (a.y - b.y).abs() < tolerance, "EPSG:{code}: {a:?} {b:?}\n{bare}");
        }
        assert_eq!(crs_from_wkt(&to_wkt(Crs::LonLat).unwrap()), Ok(Crs::LonLat));
        assert_eq!(crs_from_wkt(&to_wkt(Crs::WebMercator).unwrap()), Ok(Crs::WebMercator));
        // A custom system has no code; its parameters say it.
        let custom = crs_from_wkt(&ESRI_32631.replace("WGS_1984_UTM_Zone_31N", "My_Grid")).unwrap();
        let wkt = to_wkt(custom).unwrap();
        assert!(wkt.starts_with("PROJCS[\"My_Grid\"") && !wkt.contains("AUTHORITY"), "{wkt}");
        let back = crs_from_wkt(&wkt).unwrap().transformer().unwrap().to_lonlat(Coord::new(500_000.0, 5_000_000.0)).unwrap();
        assert!((back.x - 3.0).abs() < 1e-6 && (back.y - 45.153_477).abs() < 1e-5, "{back:?}");
    }

    #[test]
    fn every_system_east_and_north_gets_a_prj() {
        // Each EPSG code proj4rs projects is written, and read back by its code, except a
        // geocentric system, a grid shift, axes other than east and north and EPSG:5513 and 2065,
        // the Krovak variants pointing south and west.
        let mut written = 0;
        for code in 1..=u16::MAX {
            let Some(def) = epsg_proj4(code) else { continue };
            if proj4rs::Proj::from_proj_string(def).is_err()
                || ["+proj=geocent", "+nadgrids", "+axis="].iter().any(|k| def.contains(k))
                || matches!(code, 5513 | 2065)
            {
                continue;
            }
            let crs = Crs::from_epsg(u32::from(code)).unwrap();
            let wkt = to_wkt(crs).unwrap_or_else(|| panic!("EPSG:{code}: {def}"));
            assert_eq!(crs_from_wkt(&wkt), Ok(crs), "{wkt}");
            written += 1;
        }
        assert!(written > 5000, "{written}");
        assert_eq!(to_wkt(Crs::from_epsg(5513).unwrap()), None);
        assert_eq!(to_wkt(Crs::from_epsg(2065).unwrap()), None);
    }

    #[test]
    fn oblique_and_national_meridian_systems_read_back_from_their_parameters() {
        // LV95, LV03 and LV03 on the Bern meridian; Krovak, on Greenwich and on Ferro; France's
        // NTF Lambert II on the Paris meridian, Austria's Gauss-Krüger on Ferro; geographic NTF,
        // Bogotá and Batavia (Jakarta). Each with a place in its area.
        let systems = [
            (2056u16, (8.54, 47.37), "Hotine_Oblique_Mercator_Azimuth_Center"),
            (21781, (8.54, 47.37), "Hotine_Oblique_Mercator_Azimuth_Center"),
            (21780, (8.54, 47.37), "PRIMEM[\"Bern\",7.43958333"),
            (5514, (14.42, 50.09), "PROJECTION[\"Krovak\"]"),
            (5221, (14.42, 50.09), "PRIMEM[\"Ferro\",-17.66666666"),
            (27572, (2.35, 48.86), "PRIMEM[\"Paris\",2.33722916"),
            (31251, (10.4, 47.3), "PRIMEM[\"Ferro\""),
            (4807, (2.35, 48.86), "PRIMEM[\"Paris\""),
            (4802, (-74.08, 4.6), "PRIMEM[\"Bogota\""),
            (4813, (106.8, -6.2), "PRIMEM[\"Jakarta\""),
            (3003, (9.19, 45.46), "DATUM[\"Monte_Mario\""),
            // Oman's oblique Mercator about its centre, Michigan's about its natural origin;
            // Equal Earth, Brazil's SAD69 polyconic and Berlin's Cassini-Soldner.
            (2057, (58.5, 23.6), "PROJECTION[\"Hotine_Oblique_Mercator_Azimuth_Center\"]"),
            (3078, (-84.6, 46.47), "PROJECTION[\"Hotine_Oblique_Mercator\"]"),
            (8857, (10.0, 40.0), "PROJECTION[\"Equal_Earth\"]"),
            (29100, (-54.0, -10.0), "PROJECTION[\"Polyconic\"]"),
            (3068, (13.4, 52.4), "PROJECTION[\"Cassini_Soldner\"]"),
        ];
        for (code, (lon, lat), says) in systems {
            let crs = Crs::from_epsg(u32::from(code)).unwrap();
            let wkt = to_wkt(crs).unwrap_or_else(|| panic!("EPSG:{code}"));
            assert!(wkt.contains(says), "EPSG:{code}: {wkt}");
            let bare = wkt.replace(&format!(",AUTHORITY[\"EPSG\",\"{code}\"]"), "");
            let read = crs_from_wkt(&bare).unwrap_or_else(|e| panic!("EPSG:{code}: {e}: {bare}"));
            let lonlat = Coord::new(lon, lat);
            let a = crs.transformer().unwrap().from_lonlat(lonlat).unwrap();
            let b = read.transformer().unwrap().from_lonlat(lonlat).unwrap();
            let tolerance = if crs.is_geographic() { 1e-8 } else { 0.01 };
            assert!((a.x - b.x).abs() < tolerance && (a.y - b.y).abs() < tolerance, "EPSG:{code}: {a:?} {b:?}\n{bare}");
        }
        // Each new method against PROJ (pyproj 3.8.0 on the system's own PROJ definition, from
        // EPSG:4326): the round trip above can't catch a wrong formula, both sides being the same
        // code, and the same definition keeps the bundled datum shift in the comparison.
        let checked = [
            (2057u16, (58.5, 23.6), (1_260_860.830, 2_623_439.977)),
            (3078, (-84.6, 46.47), (607_329.382, 658_531.220)),
            (8857, (10.0, 40.0), (849_749.831, 4_921_020.062)),
            (3068, (13.4, 52.4), (24_656.900, 8_104.416)),
            (29100, (-54.0, -10.0), (5_000_045.526, 8_894_187.489)),
        ];
        for (code, (lon, lat), (x, y)) in checked {
            let m = Crs::from_epsg(u32::from(code)).unwrap().transformer().unwrap().from_lonlat(Coord::new(lon, lat)).unwrap();
            assert!((m.x - x).abs() < 1.0 && (m.y - y).abs() < 1.0, "EPSG:{code}: {m:?}");
        }
        // LV95 takes CH1903+'s name, LV03 CH1903's.
        assert!(to_wkt(Crs::from_epsg(2056).unwrap()).unwrap().contains("DATUM[\"CH1903+\""));
        assert!(to_wkt(Crs::from_epsg(21781).unwrap()).unwrap().contains("DATUM[\"CH1903\""));
        // A Swiss system read from a .prj with its shift and no code (its ellipsoid by its axes,
        // a three-parameter shift) is written with its datum named, and reads back as itself.
        let lv95 = to_wkt(Crs::from_epsg(2056).unwrap()).unwrap().replace(",AUTHORITY[\"EPSG\",\"2056\"]", "");
        let lv95 = lv95.replace("TOWGS84[674.374,15.056,405.346,0,0,0,0]", "TOWGS84[674.374,15.056,405.346]");
        let read = crs_from_wkt(&lv95).unwrap();
        assert!(read.custom_def().unwrap().proj4.contains("+a="), "{:?}", read.custom_def());
        let again = to_wkt(read).unwrap();
        assert!(again.contains("DATUM[\"CH1903+\"") || again.contains("DATUM[\"CH1903\""), "{again}");
        assert!(crs_from_wkt(&again).is_ok(), "{again}");
    }

    /// Another body's system (Mars, IAU 2015) gets its own note instead of the datum note, and its
    /// longitudes counted west are refused; no Earth system the bundle holds gets the note.
    #[test]
    fn planetary_systems_are_said_and_west_longitudes_refused() {
        let esri = r#"GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#;
        let (crs, note) = read_wkt(esri).unwrap();
        let note = note.unwrap();
        // (Named as the run's table first knew its definition.)
        assert!(crs.is_planetary() && note.contains(&format!("{} (datum D_Mars_2015_-_Sphere)", crs.label())), "{note}");
        assert!(!note.contains("few hundred metres"), "{note}");
        assert_eq!(crs.custom_def().unwrap().proj4, "+proj=longlat +a=3396190 +b=3396190 +no_defs");
        let projected =
            format!(r#"PROJCS["Mars eqc",{esri},PROJECTION["Equirectangular"],PARAMETER["central_meridian",0],UNIT["metre",1]]"#);
        let (crs, note) = read_wkt(&projected).unwrap();
        assert!(crs.is_planetary() && !crs.is_geographic() && note.is_some_and(|n| n.contains("D_Mars_2015_-_Sphere")));
        // Longitudes counted west, in WKT2 and WKT1, are refused; north/east axes are read.
        let wkt2 = |lon: &str| {
            format!(
                r#"GEOGCRS["Mars (2015) / Ographic",DATUM["Mars (2015)",ELLIPSOID["Mars (2015)",3396.19,169.894447223612,LENGTHUNIT["kilometre",1000]]],PRIMEM["Reference Meridian",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",{lon},ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["IAU",49901,2015]]"#
            )
        };
        assert_eq!(read_wkt(&wkt2("west")).unwrap_err(), "axes pointing west or south aren't supported");
        assert_eq!(planetary_datum(&wkt2("west")).as_deref(), Some("Mars (2015)"));
        assert!(read_wkt(&wkt2("east")).unwrap().0.is_planetary());
        let wkt1 = esri
            .replace("UNIT[\"Degree\",0.0174532925199433]]", "UNIT[\"Degree\",0.0174532925199433],AXIS[\"Lon\",WEST],AXIS[\"Lat\",NORTH]]");
        assert_eq!(read_wkt(&wkt1).unwrap_err(), "axes pointing west or south aren't supported");
        // Planetographic with no axes saying east (an Esri `.prj` has none), by its name or its
        // IAU code, in WKT1 and WKT2: refused; axes saying east are read. Not merely a name
        // ending so.
        let ographic = esri.replace("GCS_Mars_2015_-_Sphere_Ocentric", "GCS_Mars_2015_Ographic");
        assert_eq!(read_wkt(&ographic).unwrap_err(), planetary::OGRAPHIC);
        let east = ographic.replace("0.0174532925199433]]", r#"0.0174532925199433],AXIS["Lon",EAST],AXIS["Lat",NORTH]]"#);
        let north_only = ographic.replace("0.0174532925199433]]", r#"0.0174532925199433],AXIS["Lat",NORTH]]"#);
        assert_eq!(read_wkt(&north_only).unwrap_err(), planetary::OGRAPHIC);
        assert!(read_wkt(&east).unwrap().0.is_planetary());
        let coded = esri.replace("0.0174532925199433]]", r#"0.0174532925199433],AUTHORITY["IAU_2015","49901"]]"#);
        assert_eq!(read_wkt(&coded).unwrap_err(), planetary::OGRAPHIC);
        assert!(read_wkt(&coded.replace("49901", "49900")).is_ok());
        assert!(read_wkt(&esri.replace("Sphere_Ocentric", "cartographic")).is_ok());
        let bare2 = wkt2("west").replace(r#"CS[ellipsoidal,2],"#, "");
        let bare2: String = bare2.split(",AXIS[").next().map(|head| format!("{head},ID[\"IAU\",49901,2015]]")).unwrap();
        assert!(!bare2.contains("AXIS"), "{bare2}");
        assert_eq!(read_wkt(&bare2).unwrap_err(), planetary::OGRAPHIC);
        // A projection of it: by its own name or code, or its geographic system's, unless its axes
        // say x counts east; its own axes pointing west are refused.
        let eqc = |geog: &str, name: &str, tail: &str| {
            format!(r#"PROJCS["{name}",{geog},PROJECTION["Equirectangular"],PARAMETER["central_meridian",0],UNIT["metre",1]{tail}]"#)
        };
        for (geog, name, tail) in [
            (ographic.as_str(), "Mars eqc", ""),
            (esri, "Mars_2015_Ographic_Equirectangular", ""),
            (esri, "Mars eqc", r#",AUTHORITY["IAU_2015","49911"]"#),
            (esri, "Mars eqc", r#",AUTHORITY["IAU_2015","49916"]"#),
            (wkt1.as_str(), "Mars eqc", ""),
        ] {
            assert_eq!(read_wkt(&eqc(geog, name, tail)).unwrap_err(), planetary::OGRAPHIC_PROJECTED, "{name}{tail}");
        }
        // A refused system takes no place in the run's table: the next system with its definition
        // is named as that one is. (On an ellipsoid no other test reads.)
        let venus = |name: &str, axes: &str| {
            format!(
                r#"GEOGCS["{name}",DATUM["D_Venus",SPHEROID["Venus",6051800.5,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]{axes}]"#
            )
        };
        assert_eq!(read_wkt(&venus("GCS_Venus_Ographic", "")).unwrap_err(), planetary::OGRAPHIC);
        let projected = eqc(&venus("GCS_Venus_Ographic", ""), "Refused eqc", "");
        assert_eq!(read_wkt(&projected).unwrap_err(), planetary::OGRAPHIC_PROJECTED);
        let (read, _) = read_wkt(&venus("Venus east", r#",AXIS["Lon",EAST],AXIS["Lat",NORTH]"#)).unwrap();
        assert_eq!(read.label(), "Venus east");
        let (read, _) = read_wkt(&eqc(&venus("Venus", ""), "Venus eqc", r#",AXIS["Easting",EAST]"#)).unwrap();
        assert_eq!(read.label(), "Venus eqc");
        let northing = eqc(ographic.as_str(), "Mars eqc", r#",AXIS["Northing",NORTH]"#);
        assert_eq!(read_wkt(&northing).unwrap_err(), planetary::OGRAPHIC_PROJECTED);
        let westing = eqc(esri, "Mars eqc", r#",AXIS["Westing",WEST],AXIS["Northing",NORTH]"#);
        assert_eq!(read_wkt(&westing).unwrap_err(), "axes pointing west or south aren't supported");
        for (geog, name, tail) in [
            (ographic.as_str(), "Mars eqc", r#",AXIS["Easting",EAST],AXIS["Northing",NORTH]"#),
            (east.as_str(), "Mars eqc", ""),
            (esri, "Mars eqc", r#",AUTHORITY["IAU_2015","49910"]"#),
            (esri, "Mars eqc", r#",AUTHORITY["IAU_2015","49912"]"#),
        ] {
            assert!(read_wkt(&eqc(geog, name, tail)).unwrap().0.is_planetary(), "{name}{tail}");
        }
        // An Earth geographic system with the usual axes is read as before.
        assert_eq!(read_wkt(r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],UNIT["degree",0.0174532925199433],AXIS["Lat",NORTH],AXIS["Long",EAST]]"#).unwrap(), (Crs::LonLat, None));
        // Earth datums aren't another body's; hostile text is nobody's.
        for text in [
            "",
            "garbage",
            "GEOGCS[",
            r#"GEOGCS["x",DATUM["d",SPHEROID["s",NaN,0]]]"#,
            r#"GEOGCS["x",DATUM["d",SPHEROID["s",-3396190,0]]]"#,
        ] {
            assert_eq!(planetary_datum(text), None, "{text}");
        }
        let deep = format!("{}{}", "A[".repeat(10_000), "]".repeat(10_000));
        assert_eq!(planetary_datum(&deep), None);
        // Every system in the bundle, written as WKT: never another body's datum. (Read back by
        // its parameters for a sample only: each custom system read takes a place in the run's
        // table.)
        let mut written = 0;
        for code in 1024..=u16::MAX {
            let Some(wkt) = Crs::from_epsg(u32::from(code)).and_then(to_wkt) else { continue };
            assert_eq!(planetary_datum(&wkt), None, "EPSG:{code}");
            written += 1;
        }
        assert!(written > 3000, "{written}");
        let mut sampled = 0;
        // (Not UTM zone 31N: the table names a definition after its first reader, and
        // `written_wkt_reads_back_as_the_same_system` names that one My_Grid.)
        for code in [4267u32, 4230, 4277, 27700, 2056, 31370, 28992, 3035, 2193, 32634, 26917, 3413, 5514, 4087, 8857, 3006] {
            let Some(wkt) = Crs::from_epsg(code).and_then(to_wkt) else { continue };
            let wkt = wkt.replace(&format!(",AUTHORITY[\"EPSG\",\"{code}\"]"), "");
            let (crs, note) = read_wkt(&wkt).unwrap_or_else(|e| panic!("EPSG:{code}: {e}"));
            assert!(!crs.is_planetary(), "EPSG:{code}");
            assert!(!note.is_some_and(|n| n.contains("another body")), "EPSG:{code}");
            sampled += 1;
        }
        assert!(sampled >= 12, "{sampled}");
    }
}
