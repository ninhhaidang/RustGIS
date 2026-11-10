//! Coordinate systems from WKT, the text of a `.prj` file next to a Shapefile or a grid, and to
//! WKT for the files RustGIS writes ([`to_wkt`]).
//!
//! WKT1 (OGC and Esri) is read directly, a projection's angular parameters folded from its
//! geographic system's unit into degrees. WKT2 (`PROJCRS`, `GEOGCRS`, as PROJ 6 and later write
//! it) is first rewritten as the WKT1 it corresponds to, its units folded into degrees and the
//! system's own linear unit.
//!
//! An EPSG authority is used when the text names one. The `.prj` files ArcGIS and GDAL write have
//! none (`PROJCS["WGS_1984_UTM_Zone_31N",…]`), so Web Mercator and UTM are also known by name (a
//! UTM zone only when the parameters are the zone's), and any other projection is turned into a
//! PROJ definition from its parameters. Known national datums recover the same Helmert
//! transformation as their bundled EPSG definition.

use super::crs::{Crs, CustomCrs, epsg_proj4};
use super::planetary;

/// The most nested brackets read.
const MAX_DEPTH: usize = 32;

/// Why a system whose axes point west or south is refused, where they can't be read as such.
const REVERSED_AXES: &str = "axes pointing west or south aren't supported";

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

    /// The first number among the node's values, to change.
    fn num_mut(&mut self) -> Option<&mut f64> {
        self.items.iter_mut().find_map(|i| match i {
            Item::Num(v) => Some(v),
            _ => None,
        })
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
/// A projection's axes pointing west or south are read as such. Longitudes counted west are
/// refused. So is another body's planetographic system, or a projection of one, unless its axes
/// say they count east. On most bodies planetographic longitudes count west, and an Esri `.prj`
/// has no axes to say otherwise.
pub fn read_wkt(text: &str) -> Result<(Crs, Option<String>), String> {
    let root = wkt1(parse(text)?, 0)?;
    // A shift the file gives (a `TOWGS84`, or a `BOUNDCRS`'s transformation) wins over the one
    // bundled for its EPSG code, as in PROJ and GDAL. The code is used only when the two are the
    // same.
    let files_shift = towgs84(geogcs(&root).and_then(|g| g.child("DATUM")));
    // EPSG's codes are the Earth's: one on another body's system is read from its parameters.
    let by_code = root
        .child("AUTHORITY")
        .filter(|a| a.title().eq_ignore_ascii_case("EPSG") && !on_another_body(geogcs(&root)))
        .and_then(authority_code)
        .and_then(|code| Some((code, Crs::from_epsg(code)?)));
    if let Some((code, crs)) = by_code
        && files_shift.as_ref().is_none_or(|shift| matches_bundled_shift(code, shift))
    {
        return Ok((crs, projection_note(&root, crs).map(str::to_owned)));
    }
    // A system that can't be read from its parameters is still read by its EPSG code, with the
    // code's shift in place of the file's, which is said.
    match (read_system(&root), by_code) {
        (Err(e), Some((code, crs))) => {
            // A code whose shift is none (WGS 84's, or a WGS 84-like datum's) doesn't replace the
            // file's either.
            let codes_shift = !matches!(crs, Crs::LonLat | Crs::WebMercator)
                && u16::try_from(code).ok().and_then(epsg_proj4).and_then(bundled_shift).is_some_and(|s| s.iter().any(|v| *v != 0.0));
            let note = if codes_shift {
                format!("the file's own shift to WGS 84 is set aside for EPSG:{code}'s, because {e}")
            } else {
                format!(
                    "the file's own shift to WGS 84 is set aside, because {e}, and EPSG:{code} is read without it, so positions may be off by as much as that shift"
                )
            };
            Ok((crs, with_projection_note(Some(note), &root, crs)))
        }
        // A shift of zero, where the code has one, may be a writer's placeholder: said, as the
        // positions then differ from the code's by that shift.
        (Ok((crs, note)), Some((code, _))) if files_shift.is_some_and(|s| s.iter().all(|v| *v == 0.0)) => {
            let zero = format!("the file gives a shift of zero to WGS 84, which is used in place of EPSG:{code}'s transformation");
            Ok((crs, Some(note.map_or(zero.clone(), |n| format!("{n}; {zero}")))))
        }
        (read, _) => read,
    }
}

/// A WKT1 system read from its parameters, with its note ([`read_wkt`]).
fn read_system(root: &Node) -> Result<(Crs, Option<String>), String> {
    match root.name.to_ascii_uppercase().as_str() {
        "GEOGCS" => {
            if on_another_body(Some(root)) && planetographic(root) && !counts_east(root) {
                return Err(planetary::OGRAPHIC.into());
            }
            let crs = geographic(root)?;
            Ok((crs, note(crs, Some(root))))
        }
        "PROJCS" => {
            if on_another_body(root.child("GEOGCS")) {
                mirrored_projection(root)?;
            }
            let crs = projected(root)?;
            let note = match crs {
                Crs::Custom(_) => note(crs, root.child("GEOGCS")),
                // Web Mercator is on WGS 84, so a shift the file gives its datum isn't applied.
                Crs::WebMercator if towgs84(geogcs(root).and_then(|g| g.child("DATUM"))).is_some_and(|s| s.iter().any(|v| *v != 0.0)) => {
                    Some("the file's own shift to WGS 84 is set aside, as Web Mercator is on WGS 84".into())
                }
                _ => None,
            };
            let note = with_projection_note(note, root, crs);
            Ok((crs, note))
        }
        other => Err(format!("{other} coordinate systems aren't supported")),
    }
}

/// A WKT1 system's geographic system: the system itself when it is one, else its `GEOGCS`.
fn geogcs(n: &Node) -> Option<&Node> {
    if n.name.eq_ignore_ascii_case("GEOGCS") { Some(n) } else { n.child("GEOGCS") }
}

/// `note`, with [`projection_note`]'s after it when there is one.
fn with_projection_note(note: Option<String>, root: &Node, crs: Crs) -> Option<String> {
    match (note, projection_note(root, crs)) {
        (Some(note), Some(compatibility)) => Some(format!("{note}; {compatibility}")),
        (note, compatibility) => note.or(compatibility.map(str::to_owned)),
    }
}

/// Whether `text` is a geographic or projected system in WKT this reader can parse (whether or not
/// it then reads the system).
pub fn parses(text: &str) -> bool {
    parse(text).and_then(|n| wkt1(n, 0)).is_ok_and(|n| ["GEOGCS", "PROJCS"].iter().any(|r| n.name.eq_ignore_ascii_case(r)))
}

/// The file's declared EPSG method differs from the formula used for compatibility with PROJ.
/// A known CRS authority can bypass reading its definition, but must still report that choice.
pub fn projection_compatibility_note(text: &str, crs: Crs) -> Option<&'static str> {
    eqc_semi_major(crs)?;
    projection_note(&wkt1(parse(text).ok()?, 0).ok()?, crs)
}

/// The semi-major axis used by the resolved equidistant cylindrical projection.
fn eqc_semi_major(crs: Crs) -> Option<f64> {
    let custom = crs.custom_def();
    let proj4 = match crs {
        Crs::Epsg(code) => epsg_proj4(code),
        Crs::Custom(_) => custom.as_ref().map(|d| d.proj4.as_str()),
        _ => None,
    };
    let proj4 = proj4.filter(|p| p.split_whitespace().any(|p| p == "+proj=eqc"))?;
    proj4rs::Proj::from_proj_string(proj4).ok().map(|proj| proj.ellipse_parameters().0)
}

/// Method 1028 is ellipsoidal, unlike the legacy PROJ 9.7.1 `eqc` formula; a sphere has no discrepancy.
fn projection_note(root: &Node, crs: Crs) -> Option<&'static str> {
    let method = root.child("PROJECTION")?.child("AUTHORITY")?;
    if !method.title().eq_ignore_ascii_case("EPSG") || authority_code(method) != Some(1028) {
        return None;
    }
    let spheroid = root.child("GEOGCS")?.child("DATUM")?.child("SPHEROID")?;
    let resolved_axis = eqc_semi_major(crs)?;
    if spheroid.num(0).is_some_and(|axis| (axis - resolved_axis).abs() > resolved_axis.abs() * 1e-12) {
        return Some(if spheroid.num(1).is_some_and(|rf| rf.is_finite() && rf > 0.0 && rf.recip() > 1e-12) {
            "the equidistant cylindrical WKT semi-major axis differs from the resolved CRS; RustGIS uses the resolved CRS semi-major axis, so coordinates may differ from the declared spheroid; the WKT method is interpreted as EPSG method 1028 (ellipsoidal), but RustGIS uses the legacy PROJ/GDAL spherical formula (PROJ 9.7.1 compatibility), which differs from the ellipsoidal formula"
        } else {
            "the equidistant cylindrical WKT semi-major axis differs from the resolved CRS; RustGIS uses the resolved CRS semi-major axis, so coordinates may differ from the declared spheroid"
        });
    }
    let rf = spheroid.num(1)?;
    (rf.is_finite()
        && rf > 0.0
        && rf.recip() > 1e-12)
    .then_some("the WKT method is interpreted as EPSG method 1028 (ellipsoidal); RustGIS uses the legacy PROJ/GDAL spherical equidistant cylindrical formula (PROJ 9.7.1 compatibility), which differs from the ellipsoidal formula")
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
        return Err(REVERSED_AXES.into());
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
        "PROJCS" => Ok(projected1(n)),
        _ => Ok(n),
    }
}

/// The Paris meridian in degrees as GDAL and Esri write it (EPSG's 2.5969213 grads).
const PARIS_IN_DEGREES: f64 = 2.337_229_17;

/// A WKT1 projected system with its angles in degrees, as [`projected2`] gives a WKT2 one. WKT1
/// gives its projection's angular parameters and its prime meridian in its `GEOGCS`'s angle unit:
/// NTF (Paris) / Lambert zone II has its latitude of origin at 52 grads, which is 46.8°. GDAL and
/// Esri write the Paris meridian in degrees all the same (2.33722917 beside grads), and that is
/// kept, as PROJ keeps it, without a notice: read in grads, 2.33722917 is 2.1035°E, not Paris
/// (2.5969213 grads), so a meridian named Paris with that value agrees with its name only in
/// degrees, and a notice would come with every NTF file GDAL and Esri write. Paris in grads, or
/// another meridian, is converted. A system in degrees is unchanged.
fn projected1(mut p: Node) -> Node {
    let f = factor(p.child("GEOGCS").and_then(|g| g.child("UNIT")));
    let paris_in_degrees = |pm: &Node| norm(pm.title()) == "PARIS" && pm.num(0).is_some_and(|v| (v - PARIS_IN_DEGREES).abs() < 1e-7);
    if let Some(pm) = p.child_mut("GEOGCS").and_then(|g| g.child_mut("PRIMEM"))
        && !paris_in_degrees(pm)
        && let Some(v) = pm.num_mut()
    {
        *v = convert(*v, f, 1f64.to_radians());
    }
    let angles = p.items.iter_mut().filter_map(|i| match i {
        Item::Node(n) if n.name.eq_ignore_ascii_case("PARAMETER") && angular(&norm(n.title())) => Some(n),
        _ => None,
    });
    for v in angles.filter_map(Node::num_mut) {
        *v = convert(*v, f, 1f64.to_radians());
    }
    p
}

/// Whether a WKT1 projection parameter, by its [`norm`]alized name, is an angle: a latitude, a
/// longitude, a meridian, a parallel, an azimuth or another angle (`Rectified_Grid_Angle`,
/// Krovak's `XY_Plane_Rotation`). False eastings and northings, scale factors and heights aren't,
/// a scale factor not even with a parallel in its name (EPSG's `Scale factor on pseudo standard
/// parallel`, Krovak's).
fn angular(name: &str) -> bool {
    !name.contains("SCALE")
        && ["LATITUDE", "LONGITUDE", "MERIDIAN", "PARALLEL", "AZIMUTH", "ANGLE", "ROTATION"].iter().any(|w| name.contains(w))
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
    // mirrors the data. Such a system is refused here unless it is read by its EPSG code. Its
    // axes are also kept below, so a check on the WKT1 it becomes ([`geographic`]) sees them too
    // when it is the top system. A projected system's base axes don't move its coordinates (PROJ
    // places them the same), so on the Earth they aren't checked; another body's are
    // ([`mirrored_projection`]).
    if top && reversed_axes(g) && epsg_id(g).and_then(Crs::from_epsg).is_none() {
        return Err(REVERSED_AXES.into());
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
    (9808, "TRANSVERSE_MERCATOR_SOUTH_ORIENTATED", "Transverse_Mercator_South_Orientated"),
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

/// Whether any of a system's own axes points west or south. It is for the systems whose
/// reversed axes can't be read, which are then refused: geographic systems, Krovak, and another
/// body's projections. A projection's axes on the Earth are read through PROJ's `+axis` instead
/// ([`axis_flip`]). Polar axes follow their own meridians and are handled by the projection
/// itself.
fn reversed_axes(p: &Node) -> bool {
    p.nodes()
        .filter(|a| a.name.eq_ignore_ascii_case("AXIS") && a.child("MERIDIAN").is_none())
        .any(|a| a.items.iter().any(|i| matches!(i, Item::Word(w) if w.eq_ignore_ascii_case("west") || w.eq_ignore_ascii_case("south"))))
}

/// A WKT2 projected system as a `PROJCS`, its parameters in degrees and the system's linear unit.
fn projected2(p: &Node) -> Result<Node, String> {
    // Its axes are kept, so that axes pointing west or south (South Africa's Lo grids) are read
    // as such ([`axis_flip`]). Axis order isn't read: as with WKT1, coordinates are easting then
    // northing (GIS order), whatever order the system declares.
    let base = p.child_of(&["BASEGEOGCRS", "BASEGEODCRS"]).ok_or("the projected system has no base geographic system")?;
    let mut items = vec![Item::Node(geographic2(base, false)?)];
    let unit = cs_unit(p, &LENGTH_UNIT);
    let to_m = factor(unit).unwrap_or(1.0);
    if let Some(conv) = p.child("CONVERSION") {
        let method = conv.child_of(&["METHOD", "PROJECTION"]);
        let mapped = method.and_then(|m| lookup(m, METHODS, |r| (r.0, r.1)));
        let name = match mapped {
            Some(&(_, _, wkt1)) => wkt1,
            None => method.map_or("", Node::title),
        };
        // Retain the named EPSG method for compatibility diagnostics after normalization.
        let method_id = mapped.map(|&(code, _, _)| Item::Node(Node::new("AUTHORITY", "EPSG", [Item::Num(f64::from(code))])));
        items.push(Item::Node(Node::new("PROJECTION", name, method_id)));
        for param in conv.nodes().filter(|n| n.name.eq_ignore_ascii_case("PARAMETER")) {
            let Some(v) = param.num(0) else {
                // Kept with no number, so it can be told from a parameter not given.
                items.push(Item::Node(Node::new("PARAMETER", param.title(), None)));
                continue;
            };
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
/// of its transformation, in metres, arc-seconds and parts per million. A transformation to
/// another system than WGS 84 is followed by that system's own shift ([`onward_shift`]).
fn bound(b: Node, depth: usize) -> Result<Node, String> {
    let (mut source, mut target, mut transformation) = (None, None, None);
    for n in b.into_nodes() {
        if n.name.eq_ignore_ascii_case("SOURCECRS") {
            source = n.into_nodes().next();
        } else if n.name.eq_ignore_ascii_case("TARGETCRS") {
            target = n.into_nodes().next();
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
    let mut target = target.ok_or("the bound coordinate system has no target")?;
    // Only the target's datum and code matter, so its coordinate system isn't checked: a geocentric
    // target (a Cartesian `GEODCRS`, as EPSG defines Helmert transformations) is read for its datum.
    target.items.retain(|i| !matches!(i, Item::Node(n) if n.name.eq_ignore_ascii_case("CS")));
    let target = wkt1(target, depth + 1)?;
    let onward = onward_shift(&target)?;
    // With no onward shift, a three-parameter shift stays as the file wrote it, not padded to seven.
    if onward.iter().any(|v| *v != 0.0) {
        towgs84 = compose(&towgs84, onward)?;
    }
    let geog = if crs.name.eq_ignore_ascii_case("GEOGCS") { Some(&mut crs) } else { crs.child_mut("GEOGCS") };
    // A source without a datum has nowhere to keep the shift: refused, not read without it.
    let datum = geog.and_then(|g| g.child_mut("DATUM")).ok_or("the bound coordinate system's source has no datum to give the shift to")?;
    // The bound transformation replaces a `TOWGS84` the source (in WKT1) gives itself.
    datum.items.retain(|i| !matches!(i, Item::Node(n) if n.name.eq_ignore_ascii_case("TOWGS84")));
    datum.items.push(Item::Node(Node { name: "TOWGS84".into(), items: towgs84.into_iter().map(Item::Num).collect() }));
    Ok(crs)
}

/// The shift to WGS 84 of a bound system's target, as seven parameters: the one the target gives
/// (a `TOWGS84`, or its own bound transformation), none for WGS 84 itself (and the datums
/// [`wgs84_like`] takes for it), else the bundled one for its EPSG code or its national datum. A
/// target whose shift isn't known is refused: the data would be off by as much as that shift.
fn onward_shift(target: &Node) -> Result<[f64; 7], String> {
    let datum = geogcs(target).and_then(|g| g.child("DATUM"));
    let code = target.child("AUTHORITY").filter(|a| a.title().eq_ignore_ascii_case("EPSG")).and_then(authority_code);
    // A shift the target gives wins, as it does for any system. A zero one where the target's
    // code or datum has a shift may be a writer's placeholder, and this can't say so, so it is
    // refused.
    if let Some(shift) = towgs84(datum) {
        // As for a system's own shift ([`read_wkt`]): a zero that isn't its code's shift (NAD 27's
        // is a grid), or where its national datum has one.
        let placeholder = match code {
            Some(c) => !matches_bundled_shift(c, &shift),
            None => national_datum(datum).and_then(bundled_shift).is_some_and(|b| b.iter().any(|v| *v != 0.0)),
        };
        if shift.iter().all(|v| *v == 0.0) && placeholder {
            let name = match target.title().trim() {
                "" => "the bound system's target",
                t => t,
            };
            return Err(format!("{name} gives a shift of zero to WGS 84 where its datum has one"));
        }
        return Ok(std::array::from_fn(|i| shift.get(i).copied().unwrap_or(0.0)));
    }
    // WGS 84 itself, by its EPSG code or a datum read as WGS 84, needs no shift.
    if code.and_then(Crs::from_epsg) == Some(Crs::LonLat) || wgs84_like(datum).is_some() {
        return Ok([0.0; 7]);
    }
    let bundled = code.and_then(|c| u16::try_from(c).ok()).and_then(epsg_proj4).or_else(|| national_datum(datum));
    match bundled.and_then(bundled_shift) {
        Some(shift) => Ok(shift),
        None => {
            let name = match target.title().trim() {
                "" => "the bound system's target",
                t => t,
            };
            Err(format!("the shift from {name} to WGS 84 isn't known"))
        }
    }
}

/// The Helmert shift `first` (three or seven parameters) followed by `second`, as one.
///
/// Shifts are in metres, arc-seconds and parts per million. Rotations follow the position vector
/// convention (EPSG Guidance Note 7-2). The translations and scales combine exactly. The rotations
/// add, which leaves out their product. That costs at most |r1|·|r2| times the Earth's radius: 4 mm
/// for two rotations of 5″, far below the metres a datum shift is accurate to. Rotations whose
/// product could cost a centimetre are refused.
fn compose(first: &[f64], second: [f64; 7]) -> Result<Vec<f64>, String> {
    // A bound on the distance from the Earth's centre, in metres.
    const EARTH_RADIUS_M: f64 = 6_400_000.0;
    // The most that leaving out the rotations' product may move a point, in metres.
    const MAX_LEFT_OUT_M: f64 = 0.01;
    // A three-parameter shift has no rotations or scale.
    let first: [f64; 7] = std::array::from_fn(|i| first.get(i).copied().unwrap_or(0.0));
    // The size of a rotation, in radians.
    let rotation_rad = |r: &[f64]| r.iter().map(|r| (r / 3600.0).to_radians()).map(|r| r * r).sum::<f64>().sqrt();
    if rotation_rad(&first[3..6]) * rotation_rad(&second[3..6]) * EARTH_RADIUS_M > MAX_LEFT_OUT_M {
        return Err("the bound coordinate system's rotations are too large to combine".into());
    }
    let [tx, ty, tz, rx, ry, rz, s] = second;
    let [rx, ry, rz] = [rx, ry, rz].map(|r| (r / 3600.0).to_radians());
    let m = 1.0 + s * 1e-6;
    // `second` applied to `first`'s translation.
    let x = tx + m * (first[0] - rz * first[1] + ry * first[2]);
    let y = ty + m * (rz * first[0] + first[1] - rx * first[2]);
    let z = tz + m * (-ry * first[0] + rx * first[1] + first[2]);
    // The scales multiply: (1 + a) (1 + b) is 1 + a + b + ab.
    let scale = first[6] + s + first[6] * s * 1e-6;
    Ok(vec![x, y, z, first[3] + second[3], first[4] + second[4], first[5] + second[5], scale])
}

/// Whether `shift` (three or seven parameters) is the shift bundled for EPSG `code`: zero for WGS 84,
/// the datums taken for it and Web Mercator. A datum with no bundled shift (NAD 27's is a grid)
/// matches none.
fn matches_bundled_shift(code: u32, shift: &[f64]) -> bool {
    // Writers round the last digit differently: GDAL writes Hong Kong 1980's rotations as
    // -2.243648″ and -1.158828″, the bundled shift has -2.243649″ and -1.158827″. Differences
    // under 1e-5 (metres, arc-seconds or parts per million) move a point less than a millimetre.
    const ROUNDING: f64 = 1e-5;
    // Web Mercator's datum is WGS 84, so its shift is none. (It projects as if on a sphere, which
    // EPSG:3785 gives as its datum's ellipsoid.)
    let bundled = if Crs::from_epsg(code) == Some(Crs::WebMercator) {
        Some([0.0; 7])
    } else {
        u16::try_from(code).ok().and_then(epsg_proj4).and_then(bundled_shift)
    };
    let Some(bundled) = bundled else { return false };
    bundled.iter().enumerate().all(|(i, b)| (shift.get(i).copied().unwrap_or(0.0) - b).abs() < ROUNDING)
}

/// The shift to WGS 84 of a bundled EPSG definition, as seven parameters: its `+towgs84` (or its
/// named datum's), else zero on WGS 84 or GRS 1980. `None` for another datum without one (NAD 27's
/// shift is a grid).
fn bundled_shift(def: &str) -> Option<[f64; 7]> {
    // GRS 1980 counts as WGS 84: the datums on it (NAD 83, ETRS 89, GDA 2020, …) differ from
    // WGS 84 by a metre or so, and are read as WGS 84, as [`wgs84_like`] reads them by name.
    let on_wgs84 = def.split_whitespace().any(|k| ["+datum=WGS84", "+ellps=WGS84", "+ellps=GRS80"].contains(&k));
    earth_of(def).map(|(_, shift)| shift).or(on_wgs84.then_some([0.0; 7]))
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
        return Err(REVERSED_AXES.into());
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
    let pseudo = method == "MERCATOR_AUXILIARY_SPHERE" || method == "POPULAR_VISUALISATION_PSEUDO_MERCATOR";
    if name.contains("WEB_MERCATOR") || name.contains("PSEUDO_MERCATOR") || pseudo {
        // `parameters` leaves out a value that isn't a finite number, and one left out counts as
        // Web Mercator's default.
        let unusable = |i: &Item| matches!(i, Item::Node(n) if n.name.eq_ignore_ascii_case("PARAMETER") && n.num(0).is_none());
        if p.items.iter().any(unusable) {
            return Err("a Web Mercator system with a parameter that isn't a finite number isn't supported".into());
        }
        if is_web_mercator(p, &method) {
            if axis_flip(p)?.is_some() {
                return Err(REVERSED_AXES.into());
            }
            return Ok(Crs::WebMercator);
        }
        // The name is Web Mercator's, the parameters aren't: a named Mercator method is read from
        // them below, but these two methods are Web Mercator's alone and `custom` has no sphere
        // to put them on, and an EXTENSION says the sphere in a form `custom` doesn't read, so
        // refuse instead of placing points at Web Mercator's origin or on the wrong ellipsoid.
        if pseudo || p.child("EXTENSION").is_some() {
            return Err(
                "a Web Mercator system with parameters of its own (a shifted origin, scale, auxiliary sphere, unit or datum radius) isn't supported".into()
            );
        }
    }
    // The name gives a UTM zone's EPSG code, but the parameters say where the coordinates are.
    if let Some(code) = utm(&name)
        && is_utm_zone(p, &method, code)
        && let Some(crs) = Crs::from_epsg(code)
    {
        return Ok(crs);
    }
    custom(p, &method)
}

/// Whether a system named or projected as Web Mercator has Web Mercator's parameters: a Mercator
/// method, in metres, with central meridian, origin latitude, standard parallel, false easting and
/// northing 0, scale 1 and auxiliary sphere type 0 (the datum's semi-major axis), on a datum with
/// Web Mercator's radius. A parameter that isn't given counts as the default.
fn is_web_mercator(p: &Node, method: &str) -> bool {
    let params = parameters(p);
    let is_default = |names: &[&str], to: f64| parameter(&params, names).is_none_or(|v| (v - to).abs() <= 1e-9);
    (method.is_empty() || method.starts_with("MERCATOR") || method.ends_with("PSEUDO_MERCATOR"))
        && p.child("UNIT").and_then(|u| u.num(0)).is_none_or(|m| (m - 1.0).abs() <= 1e-9)
        && p.child("GEOGCS")
            .and_then(|g| g.child("DATUM"))
            .and_then(|d| d.child_of(&["SPHEROID", "ELLIPSOID"]))
            .and_then(|s| s.num(0))
            .is_none_or(|a| (a - 6_378_137.0).abs() <= 0.01)
        && is_default(&CENTRAL_MERIDIAN, 0.0)
        && is_default(&LATITUDE_OF_ORIGIN, 0.0)
        && is_default(&["STANDARD_PARALLEL_1"], 0.0)
        && is_default(&["FALSE_EASTING"], 0.0)
        && is_default(&["FALSE_NORTHING"], 0.0)
        && is_default(&["SCALE_FACTOR"], 1.0)
        && is_default(&["AUXILIARY_SPHERE_TYPE"], 0.0)
}

/// UTM's scale on the central meridian.
const UTM_SCALE: f64 = 0.9996;
/// UTM's false easting, in metres.
const UTM_FALSE_EASTING: f64 = 500_000.0;
/// UTM's false northing south of the equator, in metres (0 north of it).
const UTM_SOUTH_FALSE_NORTHING: f64 = 10_000_000.0;

/// The central meridian of UTM `zone`: zone 1 is centred on 177°W, and each zone is 6° wide.
fn utm_central_meridian(zone: f64) -> f64 {
    zone * 6.0 - 183.0
}

/// Whether a projected system is the UTM zone of EPSG `code`, as [`utm`] gives it:
/// - Transverse Mercator;
/// - on a WGS 84-like datum the file adds no shift of its own to, from Greenwich (NAD 83's
///   realisations count as one);
/// - in metres, with axes east and north;
/// - at the zone's central meridian, from the equator;
/// - with UTM's scale, false easting, and false northing (0 north of the equator).
///
/// A parameter that isn't given counts as [`custom`] reads it.
fn is_utm_zone(p: &Node, method: &str, code: u32) -> bool {
    let Some((_, zone, north)) = utm_of_code(code) else { return false };
    let geog = p.child("GEOGCS");
    let datum = geog.and_then(|g| g.child("DATUM"));
    let params = parameters(p);
    let get = |names: &[&str], default: f64| parameter(&params, names).unwrap_or(default);
    // Equal but for the rounding of written values: within a billionth of the value, or of 1 near 0.
    let near = |v: f64, to: f64| (v - to).abs() <= 1e-9 * to.abs().max(1.0);
    // NAD 83's realisations (HARN, 2011, CSRS, …) differ from WGS 84 by a metre or so, as NAD 83
    // does. They are told by their names, which start as NAD 83's: an ellipsoid alone doesn't say
    // which datum it is.
    let nad83_realisation = |d: &Node| {
        let name = norm(d.title());
        let name = name.trim_start_matches("D_");
        ["NORTH_AMERICAN_1983", "NORTH_AMERICAN_DATUM_1983", "NAD_1983", "NAD83"].iter().any(|n| name.starts_with(n))
            && ellipsoid_matches(d, "+proj=longlat +ellps=GRS80")
    };
    method == "TRANSVERSE_MERCATOR"
        && datum.is_none_or(|d| wgs84_like(Some(d)).is_some() || nad83_realisation(d))
        && !shifted(datum)
        && geog.and_then(|g| g.child("PRIMEM")).and_then(|m| m.num(0)).is_none_or(|pm| near(pm, 0.0))
        && p.child("UNIT").and_then(|u| u.num(0)).is_none_or(|m| near(m, 1.0))
        && axis_flip(p) == Ok(None)
        && near(get(&CENTRAL_MERIDIAN, 0.0), utm_central_meridian(f64::from(zone)))
        && near(get(&LATITUDE_OF_ORIGIN, 0.0), 0.0)
        && near(get(&SCALE_FACTOR, 1.0), UTM_SCALE)
        && near(get(&["FALSE_EASTING"], 0.0), UTM_FALSE_EASTING)
        && near(get(&["FALSE_NORTHING"], 0.0), if north { 0.0 } else { UTM_SOUTH_FALSE_NORTHING })
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

/// A projection's parameters, by their [`norm`]alized names.
fn parameters(p: &Node) -> Vec<(String, f64)> {
    p.items
        .iter()
        .filter_map(|i| match i {
            Item::Node(n) if n.name.eq_ignore_ascii_case("PARAMETER") => Some((norm(n.title()), n.num(0)?)),
            _ => None,
        })
        .collect()
}

/// The value of the first of `names` among `params`.
fn parameter(params: &[(String, f64)], names: &[&str]) -> Option<f64> {
    names.iter().find_map(|n| params.iter().find(|(k, _)| k == n).map(|(_, v)| *v))
}

/// The names a latitude and a longitude of origin go by. (GDAL writes Esri WKT for some methods
/// with EPSG's parameter names: `Latitude of natural origin`.)
const LATITUDE_OF_ORIGIN: [&str; 3] = ["LATITUDE_OF_ORIGIN", "LATITUDE_OF_CENTER", "LATITUDE_OF_NATURAL_ORIGIN"];
const CENTRAL_MERIDIAN: [&str; 4] = ["CENTRAL_MERIDIAN", "LONGITUDE_OF_CENTER", "LONGITUDE_OF_ORIGIN", "LONGITUDE_OF_NATURAL_ORIGIN"];
/// The names a scale factor goes by (`Scale factor at natural origin`, as GDAL writes some methods
/// in Esri WKT).
const SCALE_FACTOR: [&str; 2] = ["SCALE_FACTOR", "SCALE_FACTOR_AT_NATURAL_ORIGIN"];

/// PROJ's `+axis` for a projected system's own axes when they point west or south (South Africa's
/// Lo grids count westings and southings): `wnu`, `esu` or `wsu`, in the easting-then-northing
/// order coordinates are read in. `None` for east and north, for no axes, and for a polar
/// system's: they follow meridians, as its projection does. A height or depth axis (a
/// three-dimensional WKT2 system's) doesn't count. Other axes pointing west or south are refused,
/// and so are such axes declared northing first.
///
/// It is for an Earth projection's axes. Systems whose reversed axes can't be read this way
/// (geographic systems, Krovak, another body's projections) are refused by [`reversed_axes`].
fn axis_flip(p: &Node) -> Result<Option<&'static str>, String> {
    let directions: Vec<String> = p
        .nodes()
        .filter(|a| a.name.eq_ignore_ascii_case("AXIS") && a.child("MERIDIAN").is_none())
        .filter_map(|a| {
            // The direction is the last bare word: an unquoted name (`AXIS[Westing,WEST]`) comes
            // before it.
            a.items.iter().rev().find_map(|i| match i {
                Item::Word(w) => Some(w.to_ascii_uppercase()),
                _ => None,
            })
        })
        .filter(|d| d != "UP" && d != "DOWN")
        .collect();
    // Axes that aren't one easting and one northing are read as east and north, unless one of
    // them points west or south.
    let unless_west_or_south = || {
        if directions.iter().any(|d| d == "WEST" || d == "SOUTH") { Err(REVERSED_AXES.to_string()) } else { Ok(None) }
    };
    let [a, b] = directions.as_slice() else { return unless_west_or_south() };
    // Axes may be declared northing first (EPSG:2180's). Coordinates are read easting first all
    // the same, so the east-west axis is x.
    let northing_first = a == "NORTH" || a == "SOUTH";
    let (x, y) = if northing_first { (b, a) } else { (a, b) };
    // With axes pointing south or west, PROJ and GDAL keep the declared order, so a southing
    // declared first is x (Austria's Ferro grids, EPSG:8044). Read easting first, positions would
    // be hundreds of kilometres off, so such axes are refused.
    if northing_first && (x == "EAST" || x == "WEST") && !(x == "EAST" && y == "NORTH") {
        return Err(REVERSED_AXES.to_string());
    }
    match (x.as_str(), y.as_str()) {
        ("EAST", "NORTH") => Ok(None),
        ("WEST", "NORTH") => Ok(Some("wnu")),
        ("EAST", "SOUTH") => Ok(Some("esu")),
        ("WEST", "SOUTH") => Ok(Some("wsu")),
        // WKT1's polar axes point the same way twice, toward or away from the pole.
        ("NORTH", "NORTH") | ("SOUTH", "SOUTH") if polar(p) => Ok(None),
        _ => unless_west_or_south(),
    }
}

/// Whether a projection is centred on a pole: a polar stereographic one, or one whose origin is at
/// latitude 90° north or south.
fn polar(p: &Node) -> bool {
    let method = norm(p.child("PROJECTION").map_or("", Node::title));
    let lat0 = parameter(&parameters(p), &LATITUDE_OF_ORIGIN);
    // `POLAR_STEREOGRAPHIC`, or Esri's `STEREOGRAPHIC_NORTH_POLE` / `STEREOGRAPHIC_SOUTH_POLE`.
    method.contains("POLAR") || method.ends_with("_POLE") || lat0.is_some_and(|lat| (lat.abs() - 90.0).abs() < 1e-9)
}

/// A projection's PROJ definition built from its WKT parameters.
fn custom(p: &Node, method: &str) -> Result<Crs, String> {
    // Krovak's axes pointing south and west aren't given `+axis`, because what they mean can't be
    // told: EPSG:5513 and 2065 declare them, but their bundled PROJ definitions count east and
    // north (proj4rs's krovak without `+czech`).
    if method == "KROVAK" && reversed_axes(p) {
        return Err(REVERSED_AXES.into());
    }
    let params = parameters(p);
    let get = |names: &[&str]| parameter(&params, names);
    // Metres per linear unit: false eastings and northings are in the system's units, PROJ's in
    // metres.
    let to_m = p.child("UNIT").and_then(|u| u.num(0)).filter(|m| *m > 0.0).unwrap_or(1.0);
    let lat0 = get(&LATITUDE_OF_ORIGIN).unwrap_or(0.0);
    let lon0 = get(&CENTRAL_MERIDIAN).unwrap_or(0.0);
    let x0 = get(&["FALSE_EASTING"]).unwrap_or(0.0) * to_m;
    let y0 = get(&["FALSE_NORTHING"]).unwrap_or(0.0) * to_m;
    let k0 = get(&SCALE_FACTOR).unwrap_or(1.0);
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
        // South Orientated (EPSG's method 9808, South Africa's Lo grids) counts westings and
        // southings: the same projection, given `+axis=wsu` below. Its false easting and northing
        // add to the westing and southing at the natural origin (EPSG Guidance Note 7-2: a false
        // westing and southing), and `+axis=wsu` negates what `+x_0` and `+y_0` give, so they go
        // in negated (`0.0 -` keeps a zero one from reading `-0`).
        "TRANSVERSE_MERCATOR_SOUTH_ORIENTATED" => {
            format!("+proj=tmerc +lat_0={lat0} +k_0={k0} +lon_0={lon0} +x_0={} +y_0={}", 0.0 - x0, 0.0 - y0)
        }
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
        // ESRI writes plain `Cassini` to `.prj` files for the same ellipsoidal method (EPSG 9806):
        // ESRI's projection reference describes it as Cassini-Soldner, and GDAL's ESRI WKT
        // morphing maps `Cassini` and `Cassini_Soldner` to each other.
        "CASSINI_SOLDNER" | "CASSINI" => format!("+proj=cass +lat_0={lat0} {at}"),
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
                && get(&["XY_PLANE_ROTATION"]).is_none_or(|v| (v - 90.0).abs() < 1e-10) =>
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
    let mut flip = axis_flip(p)?;
    // A South Orientated system's westings and southings are its method's (PROJ writes it as
    // `+axis=wsu`). Axes that say so agree and no axes leave it to the method; any others
    // contradict it, and which one is meant can't be told.
    if method == "TRANSVERSE_MERCATOR_SOUTH_ORIENTATED" {
        let declared = p.nodes().any(|a| a.name.eq_ignore_ascii_case("AXIS"));
        if flip != Some("wsu") && (flip.is_some() || declared) {
            return Err("Transverse Mercator (South Orientated) with axes other than west and south isn't supported".into());
        }
        flip = Some("wsu");
    }
    let axis = match flip {
        // The projections RustGIS computes itself (sinusoidal, and equidistant cylindrical on an
        // ellipsoid) don't take `+axis`, and `Crs::custom` would refuse their definition.
        Some(_) if proj.starts_with("+proj=sinu") || (proj.starts_with("+proj=eqc") && !sphere) => return Err(REVERSED_AXES.into()),
        flip => flip.map(|a| format!(" +axis={a}")).unwrap_or_default(),
    };
    let label = match p.title().trim() {
        "" => "Custom projection".to_string(),
        t => t.to_string(),
    };
    Crs::custom(CustomCrs {
        proj4: format!("{proj}{axis} {} {units} +no_defs", earth(p.child("GEOGCS"))),
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
    def_wkt(&def, &name, code)
}

/// A PROJ definition as WKT1 named `name`, with no authority: a projection a map is drawn in that
/// has no EPSG code (a preset, `local:`), as [`to_wkt`] would write it. `None` as there.
pub fn proj4_to_wkt(def: &str, name: &str) -> Option<String> {
    def_wkt(def, name, None)
}

/// A PROJ definition as WKT1, with `code` as its EPSG authority.
fn def_wkt(def: &str, name: &str, code: Option<u16>) -> Option<String> {
    // proj4rs doesn't project the world projections RustGIS computes itself (rustgis-map's
    // `world`): their ellipsoid, prime meridian and unit are read from the definition as a
    // geographic one.
    let proj = proj4rs::Proj::from_proj_string(def).ok().or_else(|| {
        let kind = def.split_whitespace().find_map(|t| t.strip_prefix("+proj="))?;
        if !matches!(kind, "natearth" | "wintri" | "eck4") {
            return None;
        }
        proj4rs::Proj::from_proj_string(&def.replacen(&format!("+proj={kind}"), "+proj=longlat", 1)).ok()
    })?;
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
    // proj4rs computes `eqc` on a sphere of the semi-major axis and gives that sphere. Keep the
    // datum's original ellipsoid in the definition, as PROJ/GDAL do.
    let (a, b) = match text("proj") {
        Some("eqc") => super::crs::eqc_geographic(def).ok()?.ellipse_parameters(),
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
        None => match national_datum_name(def, code) {
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
                ("central_meridian", utm_central_meridian(zone)),
                ("scale_factor", UTM_SCALE),
                ("false_easting", UTM_FALSE_EASTING),
                ("false_northing", if flag("south") { UTM_SOUTH_FALSE_NORTHING } else { 0.0 }),
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
        // Younger than WKT1 too, or never in it: the names GDAL and PROJ read (Esri's).
        "natearth" => {
            ps = vec![("central_meridian", lon0)];
            "Natural_Earth"
        }
        "eck4" => {
            ps = vec![("central_meridian", lon0)];
            "Eckert_IV"
        }
        // Its standard parallel is PROJ's default, Aitoff's and Hammer's parallel of acos(2/π)
        // (about 50.46°), unless the definition gives one.
        "wintri" => {
            let lat1 = get("lat_1").unwrap_or_else(|| std::f64::consts::FRAC_2_PI.acos().to_degrees());
            ps = vec![("standard_parallel_1", lat1), ("central_meridian", lon0)];
            "Winkel_Tripel"
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
        quoted(name),
        geogcs(""),
        quoted(method),
        quoted(unit),
        num(to_m),
        authority(code),
    ))
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// The world projections RustGIS computes itself and proj4rs doesn't (Natural Earth, Winkel
    /// Tripel, Eckert IV) are written as WKT, on their datum, with their centre and offsets
    /// (#1191: a GeoPDF of a map drawn in one).
    #[test]
    fn world_projections_proj4rs_lacks_are_written() {
        for (def, method) in [
            ("+proj=natearth +lon_0=-60 +x_0=100 +y_0=-200 +datum=WGS84 +units=m +no_defs", "Natural_Earth"),
            ("+proj=wintri +lon_0=-60 +x_0=100 +y_0=-200 +datum=WGS84 +units=m +no_defs", "Winkel_Tripel"),
            ("+proj=eck4 +lon_0=-60 +x_0=100 +y_0=-200 +datum=WGS84 +units=m +no_defs", "Eckert_IV"),
        ] {
            let wkt = proj4_to_wkt(def, "World").unwrap_or_else(|| panic!("{def}"));
            assert!(wkt.contains(&format!("PROJECTION[\"{method}\"]")), "{wkt}");
            assert!(wkt.contains("PARAMETER[\"central_meridian\",-60]") && wkt.contains("PARAMETER[\"false_easting\",100]"), "{wkt}");
            assert!(wkt.contains("SPHEROID[\"WGS 84\",6378137,298.257223563"), "{wkt}");
        }
        let wkt = proj4_to_wkt("+proj=wintri +lon_0=0 +datum=WGS84 +units=m +no_defs", "W").unwrap();
        assert!(wkt.contains("PARAMETER[\"standard_parallel_1\",50.4597"), "{wkt}");
        // Another projection proj4rs can't read still has none.
        assert_eq!(proj4_to_wkt("+proj=nosuch +datum=WGS84 +units=m +no_defs", "X"), None);
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
    fn esri_cassini_name() {
        // ArcGIS writes Cassini-Soldner as plain `Cassini` in a `.prj`: Berlin's Soldner grid
        // (EPSG:3068) under that name and a name RustGIS doesn't know reads as the EPSG system.
        let epsg = Crs::from_epsg(3068).unwrap();
        let wkt = to_wkt(epsg).unwrap().replace(",AUTHORITY[\"EPSG\",\"3068\"]", "");
        let start = wkt.find('"').unwrap() + 1;
        let end = start + wkt[start..].find('"').unwrap();
        let esri =
            format!("{}My_Soldner{}", &wkt[..start], &wkt[end..]).replace("PROJECTION[\"Cassini_Soldner\"]", "PROJECTION[\"Cassini\"]");
        assert!(esri.contains("PROJECTION[\"Cassini\"]"), "{esri}");
        let crs = crs_from_wkt(&esri).unwrap_or_else(|e| panic!("{e}: {esri}"));
        // Near the grid origin and a few degrees from it, where a spherical form would differ.
        for lonlat in [Coord::new(13.4, 52.4), Coord::new(17.5, 55.5)] {
            let a = epsg.transformer().unwrap().from_lonlat(lonlat).unwrap();
            let b = crs.transformer().unwrap().from_lonlat(lonlat).unwrap();
            assert!((a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01, "{a:?} {b:?}");
        }
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
            let unshifted = note.as_ref().is_some_and(|n| n.contains("has no conversion to WGS 84"));
            assert_eq!(crs.lacks_datum_shift(), unshifted, "{}: {note:?}", crs.label());
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

    /// Hartebeesthoek94 / Lo19 (EPSG:2048) as GDAL writes its WKT1, without its code: westings
    /// and southings on a Transverse Mercator.
    const LO19: &str = r#"PROJCS["Hartebeesthoek94 / Lo19",GEOGCS["Hartebeesthoek94",DATUM["Hartebeesthoek94",SPHEROID["WGS 84",6378137,298.257223563],TOWGS84[0,0,0,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",19],PARAMETER["scale_factor",1],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1],AXIS["Y",WEST],AXIS["X",SOUTH]]"#;

    /// The same as PROJ writes its WKT2.
    const LO19_WKT2: &str = r#"PROJCRS["Hartebeesthoek94 / Lo19",BASEGEOGCRS["Hartebeesthoek94",DATUM["Hartebeesthoek94",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4148]],CONVERSION["South African Survey Grid zone 19",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",19,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",1,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",0,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["westing (Y)",west,ORDER[1],LENGTHUNIT["metre",1]],AXIS["southing (X)",south,ORDER[2],LENGTHUNIT["metre",1]]]"#;

    /// 18.5°E 33.9°S on Lo19: 46 246.6750 m west and 3 752 681.8425 m south of the origin, by
    /// Krüger's series for the Transverse Mercator (EPSG Guidance Note 7-2) worked out apart from
    /// RustGIS.
    const LO19_POINT: (Coord, Coord) = (Coord { x: 46_246.675, y: 3_752_681.842_5 }, Coord { x: 18.5, y: -33.9 });

    /// WGS 84 longitude/latitude, as PROJ writes its WKT2.
    const WGS84_WKT2: &str = r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4326]]"#;

    /// OSGB 36 longitude/latitude (EPSG:4277), as PROJ writes its WKT2.
    const OSGB36_WKT2: &str = r#"GEOGCRS["OSGB36",DATUM["Ordnance Survey of Great Britain 1936",ELLIPSOID["Airy 1830",6377563.396,299.3249646,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4277]]"#;

    /// ED50 longitude/latitude (EPSG:4230), as PROJ writes its WKT2.
    const ED50_WKT2: &str = r#"GEOGCRS["ED50",DATUM["European Datum 1950",ELLIPSOID["International 1924",6378388,297,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4230]]"#;

    /// Longitude/latitude on a datum of the Bessel 1841 ellipsoid RustGIS doesn't know.
    const BESSEL_WKT2: &str = r#"GEOGCRS["Local",DATUM["Local datum",ELLIPSOID["Bessel 1841",6377397.155,299.1528128,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]"#;

    /// An example shift from [`BESSEL_WKT2`]'s datum, in the position vector convention.
    const BESSEL_SHIFT: [f64; 7] = [598.1, 73.7, 418.2, 0.202, 0.045, -2.455, 6.7];

    /// A `BOUNDCRS` from `source` to `target` by a position vector transformation of `shift`
    /// (metres, arc-seconds, parts per million; three or seven of them).
    fn bound_crs(source: &str, target: &str, shift: &[f64]) -> String {
        let names =
            ["X-axis translation", "Y-axis translation", "Z-axis translation", "X-axis rotation", "Y-axis rotation", "Z-axis rotation"];
        let mut params: Vec<String> =
            shift.iter().zip(names).zip(8605..).map(|((v, name), code)| format!(r#"PARAMETER["{name}",{v},ID["EPSG",{code}]]"#)).collect();
        if let Some(ppm) = shift.get(6) {
            params.push(format!(r#"PARAMETER["Scale difference",{ppm},SCALEUNIT["parts per million",1E-06],ID["EPSG",8611]]"#));
        }
        format!(
            r#"BOUNDCRS[SOURCECRS[{source}],TARGETCRS[{target}],ABRIDGEDTRANSFORMATION["t",METHOD["Position Vector transformation (geog2D domain)",ID["EPSG",9606]],{}]]"#,
            params.join(",")
        )
    }

    /// A coordinate system that is easy to read in the wrong place, with a point in it.
    pub(crate) struct PlacementCase {
        /// What the case is, for messages.
        pub(crate) what: &'static str,
        pub(crate) wkt: String,
        /// The point, in the system's coordinates.
        pub(crate) xy: Coord,
        /// Where the point is in WGS 84 longitude/latitude, worked out apart from RustGIS. For the
        /// bound systems: the point at height 0 to geocentric coordinates on its own ellipsoid, each
        /// Helmert shift in turn with its full rotation matrix (position vector, EPSG Guidance Note
        /// 7-2 › 4.4.3.1), staying geocentric between them, then back to longitude/latitude on
        /// WGS 84. The shifts are the file's and EPSG's.
        pub(crate) lonlat: Coord,
    }

    /// The systems of #1486: axes pointing west and south, a UTM name with other parameters, and
    /// bound systems. The readers that take WKT are tested with them.
    pub(crate) fn placement_cases() -> Vec<PlacementCase> {
        let (lo19, lo19_lonlat) = LO19_POINT;
        vec![
            PlacementCase { what: "axes west and south", wkt: LO19.to_string(), xy: lo19, lonlat: lo19_lonlat },
            // On the central meridian the easting is the false easting and the northing 0.9996
            // times the meridian's length from the equator: 4 984 944.378 m to 45°N.
            PlacementCase {
                what: "a UTM zone's name with another false easting",
                wkt: ESRI_32631.replace("500000.0", "500100.0"),
                xy: Coord::new(500_100.0, 4_982_950.400_2),
                lonlat: Coord::new(3.0, 45.0),
            },
            // The reference applies the two shifts one after the other (EPSG Guidance Note 7-2),
            // with ED50's bundled -87, -98, -121 m: 10°E 50°N on the Bessel datum.
            PlacementCase {
                what: "a bound system to ED50",
                wkt: bound_crs(BESSEL_WKT2, ED50_WKT2, &BESSEL_SHIFT),
                xy: Coord::new(10.0, 50.0),
                lonlat: Coord::new(9.997_678_497_156_58, 49.998_865_240_498_596),
            },
            // The file's three-parameter shift, not EPSG:4277's bundled seven: they differ by
            // about 6 m here, at Westminster.
            PlacementCase {
                what: "a bound system over an EPSG code",
                wkt: bound_crs(OSGB36_WKT2, WGS84_WKT2, &[375.0, -111.0, 431.0]),
                xy: Coord::new(-0.1246, 51.5007),
                lonlat: Coord::new(-0.126_186_771_688_7, 51.501_157_314_855_305),
            },
        ]
    }

    /// The length of a degree of a great circle on a sphere of the Earth's mean radius, 6 371 km.
    const METRES_PER_DEGREE: f64 = 111_195.0;

    /// How far apart two longitudes/latitudes are, in metres (near enough for a few metres).
    pub(crate) fn metres_apart(a: Coord, b: Coord) -> f64 {
        ((a.x - b.x) * b.y.to_radians().cos()).hypot(a.y - b.y) * METRES_PER_DEGREE
    }

    #[test]
    fn explicit_method_1028_keeps_the_datum_warning() {
        let rows = corpus();
        let wkt = rows.iter().find(|r| r.0 == "EPSG:4087").unwrap().2;
        let local = wkt.replace("World Geodetic System 1984 ensemble", "Local datum");
        let (crs, note) = read_wkt(&local).unwrap();
        assert!(crs.lacks_datum_shift());
        let note = note.unwrap();
        assert!(note.contains("has no conversion to WGS 84") && note.contains("EPSG method 1028"), "{note}");
        assert_eq!(note.matches("EPSG method 1028").count(), 1);
    }

    #[test]
    fn method_1028_on_an_effective_sphere_has_no_compatibility_warning() {
        let rows = corpus();
        let wkt = rows.iter().find(|r| r.0 == "EPSG:4087").unwrap().2;
        let near_sphere = wkt.replace("298.257223563", "10000000000000");
        assert_ne!(wkt, near_sphere);
        assert!(read_wkt(&near_sphere).unwrap().1.is_none());
        assert!(read_wkt(wkt).unwrap().1.unwrap().contains("differs from the ellipsoidal formula"));
    }

    #[test]
    fn near_spherical_eqc_with_a_resolved_radius_mismatch_is_reported() {
        let rows = corpus();
        let wkt = rows.iter().find(|r| r.0 == "EPSG:4087").unwrap().2;
        let mismatched = wkt.replace("298.257223563", "10000000000000").replace("6378137", "6371000");
        let (_, notice) = read_wkt(&mismatched).unwrap();
        assert!(notice.is_some_and(|notice| notice.contains("semi-major axis")));
    }

    #[test]
    fn projection_review_axis_mismatch_keeps_the_formula_warning() {
        let rows = corpus();
        let wkt = rows.iter().find(|r| r.0 == "EPSG:4087").unwrap().2.replace("6378137", "6371000");
        let (_, notice) = read_wkt(&wkt).unwrap();
        let notice = notice.unwrap();
        assert!(notice.contains("semi-major axis") && notice.contains("EPSG method 1028"), "{notice}");
    }

    #[test]
    fn placement_cases_are_placed() {
        for PlacementCase { what, wkt, xy, lonlat } in placement_cases() {
            let (crs, note) = read_wkt(&wkt).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert!(note.is_none(), "{what}: {note:?}");
            let ll = crs.transformer().unwrap().to_lonlat(xy).unwrap();
            assert!(metres_apart(ll, lonlat) < 0.001, "{what}: {} m off", metres_apart(ll, lonlat));
        }
    }

    /// A WKT1 projection's angles are in its geographic system's unit: NTF (Paris) / Lambert zone
    /// II, in grads, as PROJ 9.8.1 writes it in WKT1 (GDAL's) and Esri WKT, without its code,
    /// places a point where PROJ does, as its WKT2 form does. Read as degrees, its latitude of
    /// origin of 52 grads put the point 578 km north (#1692).
    #[test]
    fn wkt1_angles_are_in_the_geographic_systems_unit() {
        const GDAL: &str = r#"PROJCS["NTF (Paris) / Lambert zone II",GEOGCS["NTF (Paris)",DATUM["Nouvelle_Triangulation_Francaise_Paris",SPHEROID["Clarke 1880 (IGN)",6378249.2,293.466021293627,AUTHORITY["EPSG","7011"]],AUTHORITY["EPSG","6807"]],PRIMEM["Paris",2.33722917,AUTHORITY["EPSG","8903"]],UNIT["grad",0.0157079632679489,AUTHORITY["EPSG","9105"]],AUTHORITY["EPSG","4807"]],PROJECTION["Lambert_Conformal_Conic_1SP"],PARAMETER["latitude_of_origin",52],PARAMETER["central_meridian",0],PARAMETER["scale_factor",0.99987742],PARAMETER["false_easting",600000],PARAMETER["false_northing",2200000],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#;
        const ESRI: &str = r#"PROJCS["NTF_Paris_Lambert_Zone_II",GEOGCS["GCS_NTF_Paris",DATUM["Nouvelle_Triangulation_Francaise_(Paris)",SPHEROID["Clarke_1880_IGN",6378249.2,293.466021293627]],PRIMEM["Paris",2.33722917],UNIT["Grad",0.0157079632679489]],PROJECTION["Lambert_Conformal_Conic"],PARAMETER["False_Easting",600000.0],PARAMETER["False_Northing",2200000.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",52.0],PARAMETER["Scale_Factor",0.99987742],PARAMETER["Latitude_Of_Origin",52.0],UNIT["Meter",1.0]]"#;
        const WKT2: &str = r#"PROJCRS["NTF (Paris) / Lambert zone II",BASEGEOGCRS["NTF (Paris)",DATUM["Nouvelle Triangulation Francaise (Paris)",ELLIPSOID["Clarke 1880 (IGN)",6378249.2,293.466021293627,LENGTHUNIT["metre",1]]],PRIMEM["Paris",2.5969213,ANGLEUNIT["grad",0.0157079632679489]],ID["EPSG",4807]],CONVERSION["Lambert zone II",METHOD["Lambert Conic Conformal (1SP)",ID["EPSG",9801]],PARAMETER["Latitude of natural origin",52,ANGLEUNIT["grad",0.0157079632679489],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",0,ANGLEUNIT["grad",0.0157079632679489],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.99987742,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",600000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",2200000,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["easting (X)",east,ORDER[1],LENGTHUNIT["metre",1]],AXIS["northing (Y)",north,ORDER[2],LENGTHUNIT["metre",1]]]"#;
        let xy = Coord::new(549_826.63, 2_192_990.71);
        // PROJ 9.8.1's position on NTF itself (EPSG:4275, from Greenwich): these systems have no
        // shift to WGS 84 here, so RustGIS reads them on NTF.
        let ntf = Coord::new(1.680_720_205, 46.735_054_487);
        // The prime meridian is in that unit too, as PROJ reads it, but for Paris at 2.33722917,
        // the degrees GDAL and Esri write: Paris in grads (2.5969213, as WKT1 defines it) is the
        // same place, and another meridian at 2.33722917 is 2.33722917 grads, 0.233722917° west
        // of Paris.
        let paris_in_grads = GDAL.replace(r#"PRIMEM["Paris",2.33722917"#, r#"PRIMEM["Paris",2.5969213"#);
        let another = GDAL.replace(r#"PRIMEM["Paris""#, r#"PRIMEM["Another""#);
        let west = Coord::new(1.446_997_288, 46.735_054_487);
        // A central meridian of 10 grads is 9° east of Paris's (PROJ 9.8.1 places the point there).
        let ten = GDAL.replace(r#"PARAMETER["central_meridian",0]"#, r#"PARAMETER["central_meridian",10]"#);
        let east = Coord::new(10.680_720_205, 46.735_054_487);
        let cases = [
            ("WKT1", GDAL, ntf),
            ("Esri", ESRI, ntf),
            ("WKT2", WKT2, ntf),
            ("Paris in grads", &paris_in_grads, ntf),
            ("another", &another, west),
            ("a central meridian of 10 grads", &ten, east),
        ];
        for (what, wkt, at) in cases {
            let crs = crs_from_wkt(wkt).unwrap_or_else(|e| panic!("{what}: {e}"));
            let ll = crs.transformer().unwrap().to_lonlat(xy).unwrap();
            assert!(metres_apart(ll, at) < 0.01, "{what}: {ll:?} is {} m off", metres_apart(ll, at));
        }
        // A system in degrees keeps its parameters as written.
        let degrees = GDAL.replace(r#"UNIT["grad",0.0157079632679489"#, r#"UNIT["degree",0.0174532925199433"#);
        let proj4 = crs_from_wkt(&degrees).unwrap().custom_def().map(|d| d.proj4.clone()).unwrap_or_default();
        assert!(proj4.contains("+lat_0=52 ") && proj4.contains("+k_0=0.99987742 ") && proj4.contains("+x_0=600000 "), "{proj4}");
        // A scale factor stays a scale, though its name has a parallel in it (EPSG's name for
        // Krovak's scale), and the parallel itself is an angle.
        assert!(!angular(&norm("Scale_Factor_On_Pseudo_Standard_Parallel")));
        assert!(angular(&norm("Pseudo_Standard_Parallel_1")));
    }

    /// Krovak East North (EPSG:5514) in Esri WKT with its angles in grads, as WKT1 allows, places
    /// the corpus's point as in degrees: its plane rotation of 100 grads converts to 90° but for
    /// the last bit, which the check for Krovak's fixed rotation allows. Another rotation is
    /// still refused.
    #[test]
    fn wkt1_krovak_in_grads_is_read_as_in_degrees() {
        let row = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with("5514\t")).unwrap();
        let cols: Vec<&str> = row.split('\t').collect();
        let (xy, lonlat) = (
            Coord::new(cols[1].parse().unwrap(), cols[2].parse().unwrap()),
            Coord::new(cols[3].parse().unwrap(), cols[4].parse().unwrap()),
        );
        let grads = cols[5]
            .replace(r#"UNIT["Degree",0.0174532925199433]"#, r#"UNIT["Grad",0.01570796326794897]"#)
            .replace(r#""Pseudo_Standard_Parallel_1",78.5]"#, r#""Pseudo_Standard_Parallel_1",87.22222222222223]"#)
            .replace(r#""Azimuth",30.28813975277778]"#, r#""Azimuth",33.65348861419753]"#)
            .replace(r#""Longitude_Of_Center",24.83333333333333]"#, r#""Longitude_Of_Center",27.59259259259259]"#)
            .replace(r#""Latitude_Of_Center",49.5]"#, r#""Latitude_Of_Center",55.0]"#)
            .replace(r#""XY_Plane_Rotation",90.0]"#, r#""XY_Plane_Rotation",100.0]"#);
        assert_eq!(grads.matches("Grad").count() + grads.matches("100.0]").count(), 2, "{grads}");
        for wkt in [cols[5], grads.as_str()] {
            let crs = crs_from_wkt(wkt).unwrap_or_else(|e| panic!("{e}: {wkt}"));
            let ll = crs.transformer().unwrap().to_lonlat(xy).unwrap();
            assert!(metres_apart(ll, lonlat) < 0.001, "{ll:?} is {} m off: {wkt}", metres_apart(ll, lonlat));
        }
        assert!(crs_from_wkt(&grads.replace(r#""XY_Plane_Rotation",100.0]"#, r#""XY_Plane_Rotation",101.0]"#)).is_err());
    }

    /// Transverse Mercator (South Orientated), EPSG's method 9808, as PROJ 9.8.1 writes South
    /// Africa's Lo19 (EPSG:2048) without its code: in WKT1 (GDAL's, without axes), in Esri WKT
    /// (with EPSG's parameter names) and in WKT2 (westings and southings), it is PROJ's
    /// `+proj=tmerc +axis=wsu`, and places the point where PROJ does. Axes that contradict the
    /// method are refused (#1693).
    #[test]
    fn transverse_mercator_south_orientated_is_read() {
        const GDAL: &str = r#"PROJCS["Hartebeesthoek94 / Lo19",GEOGCS["Hartebeesthoek94",DATUM["Hartebeesthoek94",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6148"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4148"]],PROJECTION["Transverse_Mercator_South_Orientated"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",19],PARAMETER["scale_factor",1],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1,AUTHORITY["EPSG","9001"]]]"#;
        const ESRI: &str = r#"PROJCS["Hartebeesthoek94_Lo19",GEOGCS["GCS_Hartebeesthoek_1994",DATUM["D_Hartebeesthoek_1994",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator_South_Orientated"],PARAMETER["Latitude of natural origin",0.0],PARAMETER["Longitude of natural origin",19.0],PARAMETER["Scale factor at natural origin",1.0],PARAMETER["False easting",0.0],PARAMETER["False northing",0.0],UNIT["Meter",1.0]]"#;
        const WKT2: &str = r#"PROJCRS["Hartebeesthoek94 / Lo19",BASEGEOGCRS["Hartebeesthoek94",DATUM["Hartebeesthoek94",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4148]],CONVERSION["South African Survey Grid zone 19",METHOD["Transverse Mercator (South Orientated)",ID["EPSG",9808]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",19,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",1,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",0,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["westing (Y)",west,ORDER[1],LENGTHUNIT["metre",1]],AXIS["southing (X)",south,ORDER[2],LENGTHUNIT["metre",1]]]"#;
        let proj4 = |wkt: &str| crs_from_wkt(wkt).map(|c| c.custom_def().map(|d| d.proj4.clone()).unwrap_or_default());
        // PROJ 9.8.1 places 46 246.675 m, 3 752 681.8425 m on Lo19 at 18.5°E 33.9°S.
        let (xy, lonlat) = LO19_POINT;
        // WKT2 may name the method otherwise: its code says which it is.
        let by_code = WKT2.replace("Transverse Mercator (South Orientated)", "TM (South Orientated)");
        for (what, wkt) in [("WKT1", GDAL), ("Esri", ESRI), ("WKT2", WKT2), ("WKT2 by code", &by_code)] {
            let p = proj4(wkt).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert!(p.starts_with("+proj=tmerc ") && p.contains(" +axis=wsu "), "{what}: {p}");
            let ll = crs_from_wkt(wkt).unwrap().transformer().unwrap().to_lonlat(xy).unwrap();
            assert!(metres_apart(ll, lonlat) < 0.001, "{what}: {ll:?} is {} m off", metres_apart(ll, lonlat));
        }
        // Axes that count east and north (or east and south) contradict the method: refused.
        for axes in [r#"AXIS["Easting",EAST],AXIS["Northing",NORTH]"#, r#"AXIS["Easting",EAST],AXIS["Southing",SOUTH]"#] {
            let wkt = GDAL.replace(r#"AUTHORITY["EPSG","9001"]]]"#, &format!(r#"AUTHORITY["EPSG","9001"]],{axes}]"#));
            assert!(wkt.contains(axes) && crs_from_wkt(&wkt).is_err(), "{wkt}");
        }
        // The scale factor under EPSG's name, as GDAL writes this method in Esri WKT, is read.
        let scaled = ESRI.replace(r#""Scale factor at natural origin",1.0"#, r#""Scale factor at natural origin",0.9999"#);
        assert!(proj4(&scaled).unwrap().contains("+k_0=0.9999 "), "{scaled}");
        // The false easting and northing add to the westing and southing at the natural origin
        // (EPSG's method 9808: a false westing and southing). With 1000 m and 2000 m, the origin,
        // 19°E on the equator, is at 1000 m, 2000 m, and the point 1000 m and 2000 m further on.
        // (PROJ 9.8.1 refuses this method with offsets other than 0.)
        let offset = GDAL
            .replace(r#""false_easting",0]"#, r#""false_easting",1000]"#)
            .replace(r#""false_northing",0]"#, r#""false_northing",2000]"#);
        assert!(offset.contains(r#""false_easting",1000]"#) && offset.contains(r#""false_northing",2000]"#), "{offset}");
        let t = crs_from_wkt(&offset).unwrap().transformer().unwrap();
        for (what, at, ll) in [
            ("the natural origin", Coord::new(1000.0, 2000.0), Coord::new(19.0, 0.0)),
            ("the point", Coord::new(xy.x + 1000.0, xy.y + 2000.0), lonlat),
        ] {
            let got = t.to_lonlat(at).unwrap();
            assert!(metres_apart(got, ll) < 0.001, "{what}: {got:?} is {} m off", metres_apart(got, ll));
        }
    }

    #[test]
    fn projected_axes_west_and_south_are_read() {
        let proj4 = |wkt: &str| crs_from_wkt(wkt).map(|c| c.custom_def().map(|d| d.proj4.clone()).unwrap_or_default());
        let at = |wkt: &str, xy: Coord| crs_from_wkt(wkt).unwrap().transformer().unwrap().to_lonlat(xy).unwrap();
        let (xy, lonlat) = LO19_POINT;
        // As EPSG:2048, whose bundled definition has PROJ's `+axis=wsu`, places it.
        let bundled = Crs::from_epsg(2048).unwrap().transformer().unwrap().to_lonlat(xy).unwrap();
        assert!(metres_apart(bundled, lonlat) < 0.001, "{bundled:?}");
        for wkt in [LO19, LO19_WKT2] {
            assert!(proj4(wkt).unwrap().contains("+axis=wsu"), "{wkt}");
            assert!(metres_apart(at(wkt, xy), lonlat) < 0.001, "{wkt}");
        }
        // With a height axis too, in a three-dimensional system.
        let three_d = LO19_WKT2.replace("CS[Cartesian,2]", "CS[Cartesian,3]").replace(
            r#"ORDER[2],LENGTHUNIT["metre",1]]]"#,
            r#"ORDER[2],LENGTHUNIT["metre",1]],AXIS["ellipsoidal height (h)",up,ORDER[3],LENGTHUNIT["metre",1]]]"#,
        );
        assert!(three_d.contains(",up,ORDER[3]") && proj4(&three_d).unwrap().contains("+axis=wsu"), "{three_d}");
        // Westings only, southings only: the other coordinate counts as usual.
        // Unquoted axis names, as some writers give them: the direction is still read.
        let unquoted = LO19.replace(r#"AXIS["Y",WEST],AXIS["X",SOUTH]"#, "AXIS[Y,WEST],AXIS[X,SOUTH]");
        assert_ne!(unquoted, LO19);
        assert!(proj4(&unquoted).unwrap().contains("+axis=wsu"));
        let west = LO19.replace(r#"AXIS["X",SOUTH]"#, r#"AXIS["X",NORTH]"#);
        assert!(proj4(&west).unwrap().contains("+axis=wnu"));
        assert!(metres_apart(at(&west, Coord::new(xy.x, -xy.y)), lonlat) < 0.001);
        let south = LO19.replace(r#"AXIS["Y",WEST]"#, r#"AXIS["Y",EAST]"#);
        assert!(proj4(&south).unwrap().contains("+axis=esu"));
        assert!(metres_apart(at(&south, Coord::new(-xy.x, xy.y)), lonlat) < 0.001);
        // East and north, or none: no `+axis`.
        let east = LO19.replace(r#"AXIS["Y",WEST],AXIS["X",SOUTH]"#, r#"AXIS["Y",EAST],AXIS["X",NORTH]"#);
        let none = LO19.replace(r#",AXIS["Y",WEST],AXIS["X",SOUTH]"#, "");
        for wkt in [east, none] {
            assert!(!proj4(&wkt).unwrap().contains("+axis"), "{wkt}");
            assert!(metres_apart(at(&wkt, Coord::new(-xy.x, -xy.y)), lonlat) < 0.001);
        }
        // By an EPSG code that's known, it's read as that code; by an unknown one, from its
        // parameters.
        let with_id = |code: u32| format!(r#"{},ID["EPSG",{code}]]"#, LO19_WKT2.strip_suffix(']').unwrap());
        assert_eq!(crs_from_wkt(&with_id(2048)), Ok(Crs::from_epsg(2048).unwrap()));
        assert!(metres_apart(at(&with_id(1), xy), lonlat) < 0.001);
        // Polar axes along meridians are read (EPSG:3413's point south), and so are WKT1's polar
        // axes, which point the same way twice: no `+axis`.
        let rows = corpus();
        let polar = rows.iter().find(|r| r.0 == "EPSG:3413").unwrap().2;
        assert!(polar.contains("south,MERIDIAN") && !proj4(polar).unwrap().contains("+axis"));
        let gdal_3413 = r#"PROJCS["NSIDC Sea Ice Polar Stereographic North",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Polar_Stereographic"],PARAMETER["latitude_of_origin",70],PARAMETER["central_meridian",-45],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1],AXIS["Easting",SOUTH],AXIS["Northing",SOUTH]]"#;
        let p = proj4(gdal_3413).unwrap();
        assert!(!p.contains("+axis"), "{p}");
        let corpus_3413 = rows.iter().find(|r| r.0 == "EPSG:3413").unwrap().1;
        let ll = at(gdal_3413, Coord::new(corpus_3413[0], corpus_3413[1]));
        assert!(metres_apart(ll, Coord::new(corpus_3413[2], corpus_3413[3])) < 1.0, "{ll:?}");
        // So are they under Esri's name for the method, with its latitude of true scale.
        let esri_3413 = gdal_3413.replace(
            r#"PROJECTION["Polar_Stereographic"],PARAMETER["latitude_of_origin",70]"#,
            r#"PROJECTION["Stereographic_North_Pole"],PARAMETER["standard_parallel_1",70]"#,
        );
        assert_ne!(esri_3413, gdal_3413);
        assert!(!proj4(&esri_3413).unwrap().contains("+axis"));
        let ll = at(&esri_3413, Coord::new(corpus_3413[0], corpus_3413[1]));
        assert!(metres_apart(ll, Coord::new(corpus_3413[2], corpus_3413[3])) < 1.0, "{ll:?}");
    }

    /// A projection that can't take axes pointing west or south is refused rather than mirrored.
    /// (Krovak's are tested in `krovak_south_west_axes_are_refused`.)
    #[test]
    fn projected_axes_west_or_south_are_refused_where_they_cant_be_read() {
        // Web Mercator, known by its name, has no PROJ definition to add `+axis` to.
        let web = ESRI_3857.replace(r#"UNIT["Meter",1.0]]"#, r#"UNIT["Meter",1.0],AXIS["X",WEST],AXIS["Y",NORTH]]"#);
        assert_eq!(crs_from_wkt(&web), Err("axes pointing west or south aren't supported".into()));
        // RustGIS computes Sinusoidal itself, without PROJ's `+axis`.
        let sinusoidal = format!(
            r#"PROJCS["x",{GEOG_4326},PROJECTION["Sinusoidal"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],UNIT["Meter",1.0],AXIS["X",WEST],AXIS["Y",NORTH]]"#
        );
        assert_eq!(crs_from_wkt(&sinusoidal), Err("axes pointing west or south aren't supported".into()));
        // Axes that aren't one east-west and one north-south, one pointing west or south: a lone
        // westing, or westings twice.
        let lone = LO19.replace(r#",AXIS["X",SOUTH]"#, "");
        let twice = LO19.replace(r#"AXIS["X",SOUTH]"#, r#"AXIS["X",WEST]"#);
        let web_lone = web.replace(r#",AXIS["Y",NORTH]"#, "");
        // Southings twice, as WKT1 writes a north polar system's, on a projection that isn't polar.
        let south_twice = LO19.replace(r#"AXIS["Y",WEST]"#, r#"AXIS["Y",SOUTH]"#);
        for wkt in [lone, twice, web_lone, south_twice] {
            assert_eq!(crs_from_wkt(&wkt), Err("axes pointing west or south aren't supported".into()), "{wkt}");
        }
        // So does its equidistant cylindrical on an ellipsoid (`crs::ellipsoidal_eqc`); on a
        // sphere, proj4rs computes it and reads `+axis`.
        let eqc = sinusoidal.replace(r#"PROJECTION["Sinusoidal"]"#, r#"PROJECTION["Equidistant_Cylindrical"]"#);
        assert_eq!(crs_from_wkt(&eqc), Err("axes pointing west or south aren't supported".into()));
        let sphere = eqc.replace(
            r#"DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]]"#,
            r#"DATUM["D_Sphere",SPHEROID["Sphere",6371000.0,0.0]]"#,
        );
        assert_ne!(sphere, eqc);
        let def = crs_from_wkt(&sphere).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+proj=eqc") && def.proj4.contains("+axis=wnu"), "{}", def.proj4);
    }

    /// Axes declared northing first are read easting first when they point north and east, and
    /// refused when they point south or west: PROJ reads the southing as x there.
    #[test]
    fn northing_first_axes_pointing_south_or_west_are_refused() {
        // EPSG:8044 as GDAL writes it without its code. Read with `+axis=wsu`, its points would be
        // hundreds of kilometres from where PROJ puts them.
        let gusterberg = r#"PROJCS["Gusterberg Grid (Ferro)",GEOGCS["Gusterberg (Ferro)",DATUM["Gusterberg_Ferro",SPHEROID["Zach 1812",6376045,310]],PRIMEM["Ferro",-17.6666666666667],UNIT["degree",0.0174532925199433]],PROJECTION["Cassini_Soldner"],PARAMETER["latitude_of_origin",48.0384638888889],PARAMETER["central_meridian",31.8041805555556],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1],AXIS["Southing",SOUTH],AXIS["Westing",WEST]]"#;
        assert_eq!(crs_from_wkt(gusterberg), Err("axes pointing west or south aren't supported".into()));
        let swapped = LO19.replace(r#"AXIS["Y",WEST],AXIS["X",SOUTH]"#, r#"AXIS["X",SOUTH],AXIS["Y",WEST]"#);
        assert_ne!(swapped, LO19);
        assert_eq!(crs_from_wkt(&swapped), Err("axes pointing west or south aren't supported".into()));
        // The Lo grids declare their westing first: read, as `projected_axes_west_and_south_are_read`
        // checks.
        let (xy, lonlat) = LO19_POINT;
        let lo19 = crs_from_wkt(LO19).unwrap().transformer().unwrap().to_lonlat(xy).unwrap();
        assert!(metres_apart(lo19, lonlat) < 0.001, "{lo19:?}");
        // EPSG:2180 declares its northing first, north and east: read easting first, without
        // `+axis`. PROJ puts 509 847.9005 m E, 511 024.2427 m N at 19.145°E 52.465°N.
        let pl_1992 = r#"PROJCS["ETRS89 / PL-1992",GEOGCS["ETRS89",DATUM["European_Terrestrial_Reference_System_1989",SPHEROID["GRS 1980",6378137,298.257222101]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",19],PARAMETER["scale_factor",0.9993],PARAMETER["false_easting",500000],PARAMETER["false_northing",-5300000],UNIT["metre",1],AXIS["Northing",NORTH],AXIS["Easting",EAST]]"#;
        let (crs, note) = read_wkt(pl_1992).unwrap();
        assert!(note.is_none() && !crs.custom_def().unwrap().proj4.contains("+axis"), "{crs:?} {note:?}");
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(509_847.900_535_462_54, 511_024.242_654_006)).unwrap();
        assert!(metres_apart(ll, Coord::new(19.145, 52.465)) < 0.001, "{ll:?}");
    }

    /// A UTM zone's name gives its EPSG code only when the parameters are that zone's; otherwise
    /// they're read as they are.
    #[test]
    fn a_utm_name_gives_its_code_only_with_the_zones_parameters() {
        assert_eq!(crs_from_wkt(ESRI_32631), Ok(Crs::Epsg(32631)));
        // 100 m more false easting: 100 m east of EPSG:32631 (`placement_cases` places 3°E 45°N
        // in it).
        let shifted = ESRI_32631.replace("500000.0", "500100.0");
        let crs = crs_from_wkt(&shifted).unwrap();
        assert!(matches!(crs, Crs::Custom(_)), "{crs:?}");
        // 2.5°E 48.8°N is at 463 282.595 m E, 5 405 343.6636 m N in EPSG:32631, and 100 m further
        // east here.
        let ll = Coord::new(2.5, 48.8);
        let utm = Crs::Epsg(32631).transformer().unwrap().from_lonlat(ll).unwrap();
        assert!((utm.x - 463_282.595).abs() < 0.001 && (utm.y - 5_405_343.663_6).abs() < 0.001, "{utm:?}");
        let xy = crs.transformer().unwrap().from_lonlat(ll).unwrap();
        assert!((xy.x - (utm.x + 100.0)).abs() < 0.001 && (xy.y - utm.y).abs() < 0.001, "{xy:?}");
        // Each of the zone's parameters, its unit, datum, prime meridian and axes: one off is
        // read from the parameters.
        for (from, to) in [
            (r#""Central_Meridian",3.0"#, r#""Central_Meridian",3.5"#),
            (r#""Scale_Factor",0.9996"#, r#""Scale_Factor",1.0"#),
            (r#",PARAMETER["Scale_Factor",0.9996]"#, ""),
            (r#""Latitude_Of_Origin",0.0"#, r#""Latitude_Of_Origin",1.0"#),
            (r#""False_Northing",0.0"#, r#""False_Northing",10000000.0"#),
            (r#"UNIT["Meter",1.0]]"#, r#"UNIT["Foot_US",0.3048006096012192]]"#),
            (
                r#"DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]]"#,
                r#"DATUM["D_European_1950",SPHEROID["International_1924",6378388.0,297.0]]"#,
            ),
            (r#"298.257223563]]"#, r#"298.257223563],TOWGS84[1,2,3]]"#),
            (r#"PRIMEM["Greenwich",0.0]"#, r#"PRIMEM["Paris",2.33722917]"#),
            (r#"UNIT["Meter",1.0]]"#, r#"UNIT["Meter",1.0],AXIS["E",WEST],AXIS["N",NORTH]]"#),
        ] {
            let off = ESRI_32631.replace(from, to);
            assert_ne!(off, ESRI_32631, "{from}");
            assert!(matches!(crs_from_wkt(&off), Ok(Crs::Custom(_))), "{from}: {:?}", crs_from_wkt(&off));
        }
        // A zone south of the equator, and NAD 83's and SIRGAS 2000's, with their parameters.
        let south = ESRI_32631
            .replace("Zone_31N", "Zone_23S")
            .replace(r#""False_Northing",0.0"#, r#""False_Northing",10000000.0"#)
            .replace(r#""Central_Meridian",3.0"#, r#""Central_Meridian",-45.0"#);
        assert_eq!(crs_from_wkt(&south), Ok(Crs::Epsg(32723)));
        assert!(matches!(crs_from_wkt(&south.replace("10000000.0", "0.0")), Ok(Crs::Custom(_))));
        let nad83 = ESRI_32631
            .replace("WGS_1984_UTM_Zone_31N", "NAD_1983_UTM_Zone_17N")
            .replace(
                r#"DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]]"#,
                r#"DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]]"#,
            )
            .replace(r#""Central_Meridian",3.0"#, r#""Central_Meridian",-81.0"#);
        assert_eq!(crs_from_wkt(&nad83), Ok(Crs::Epsg(26917)));
        // So it is on NAD 83's realisations, told by their names, without a note.
        for datum in ["D_North_American_1983_HARN", "D_NAD_1983_2011", "D_North_American_1983_CSRS"] {
            let realisation = nad83.replace(r#""D_North_American_1983""#, &format!(r#""{datum}""#));
            assert_ne!(realisation, nad83);
            assert_eq!(read_wkt(&realisation), Ok((Crs::Epsg(26917), None)), "{datum}");
        }
        // Another datum on the same ellipsoid isn't taken for WGS 84: it is read from its
        // parameters, with its note.
        let local = ESRI_32631.replace(
            r#"DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]]"#,
            r#"DATUM["D_Local_Survey",SPHEROID["GRS_1980",6378137.0,298.257222101]]"#,
        );
        assert_ne!(local, ESRI_32631);
        let (crs, note) = read_wkt(&local).unwrap();
        assert!(matches!(crs, Crs::Custom(_)), "{crs:?}");
        assert!(note.is_some_and(|n| n.starts_with("the datum D_Local_Survey has no conversion to WGS 84")));
        // WKT2, without its ID: by EPSG's parameter names.
        let rows = corpus();
        let wkt2 = rows.iter().find(|r| r.0 == "EPSG:32631").unwrap().2;
        assert_eq!(crs_from_wkt(wkt2), Ok(Crs::Epsg(32631)));
        let off = wkt2.replace(r#""False easting",500000"#, r#""False easting",500100"#);
        assert_ne!(off, wkt2);
        assert!(matches!(crs_from_wkt(&off), Ok(Crs::Custom(_))));
    }

    /// A `BOUNDCRS` whose transformation goes to another datum than WGS 84 goes on by that datum's
    /// shift; one whose shift isn't known is refused.
    #[test]
    fn a_bound_system_to_another_datum_adds_that_datums_shift() {
        let at = |wkt: &str| crs_from_wkt(wkt).unwrap().transformer().unwrap().to_lonlat(Coord::new(10.0, 50.0)).unwrap();
        // ED50 by its code, and by its datum's name (the reference is `placement_cases`'). Read
        // as if to WGS 84, the point would be 82 m east.
        let ed50 = Coord::new(9.997_678_497_156_58, 49.998_865_240_498_596);
        let by_name = ED50_WKT2.replace(r#",ID["EPSG",4230]"#, "");
        for target in [ED50_WKT2, &by_name] {
            assert!(metres_apart(at(&bound_crs(BESSEL_WKT2, target, &BESSEL_SHIFT)), ed50) < 0.001, "{target}");
        }
        let to_wgs84 = at(&bound_crs(BESSEL_WKT2, WGS84_WKT2, &BESSEL_SHIFT));
        assert!(metres_apart(to_wgs84, ed50) > 80.0, "{to_wgs84:?}");
        // OSGB 36's seven parameters, rotations in both shifts: the two applied one after the
        // other, worked out apart from RustGIS as for `placement_cases`. One shift's rotations
        // turn the other's translations; leaving out the rotations' product costs under 0.1 mm.
        let osgb36 = Coord::new(9.996_185_110_396_146, 49.999_055_129_957_98);
        let ll = at(&bound_crs(BESSEL_WKT2, OSGB36_WKT2, &BESSEL_SHIFT));
        assert!(metres_apart(ll, osgb36) < 0.001, "{} m off", metres_apart(ll, osgb36));
        // A shift the target gives wins over its datum's: here ED50 with its own, which adds to the
        // source's (a shift without rotations turns nothing).
        let ed50_shifted = r#"GEOGCS["ED50",DATUM["European_Datum_1950",SPHEROID["International 1924",6378388,297],TOWGS84[-80,-90,-110,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4230"]]"#;
        let [tx, ty, tz, rx, ry, rz, s] = BESSEL_SHIFT;
        let added = [tx - 80.0, ty - 90.0, tz - 110.0, rx, ry, rz, s];
        let ll = at(&bound_crs(BESSEL_WKT2, ed50_shifted, &BESSEL_SHIFT));
        let expected = at(&bound_crs(BESSEL_WKT2, WGS84_WKT2, &added));
        assert!(metres_apart(ll, expected) < 0.001 && metres_apart(ll, ed50) > 5.0, "{ll:?}");
        // A zero one where the target's code has a shift (ED50's) may be a placeholder: refused,
        // as the bound system can't say which it used. Zero on a code without a shift is kept.
        let ed50_zero = ed50_shifted.replace("TOWGS84[-80,-90,-110,0,0,0,0]", "TOWGS84[0,0,0,0,0,0,0]");
        assert_ne!(ed50_zero, ed50_shifted);
        assert_eq!(
            crs_from_wkt(&bound_crs(BESSEL_WKT2, &ed50_zero, &BESSEL_SHIFT)),
            Err("ED50 gives a shift of zero to WGS 84 where its datum has one".into())
        );
        // So it is told by its datum's name, without a code.
        let ed50_zero_by_name = ed50_zero.replace(r#",AUTHORITY["EPSG","4230"]"#, "");
        assert_ne!(ed50_zero_by_name, ed50_zero);
        assert_eq!(
            crs_from_wkt(&bound_crs(BESSEL_WKT2, &ed50_zero_by_name, &BESSEL_SHIFT)),
            Err("ED50 gives a shift of zero to WGS 84 where its datum has one".into())
        );
        // And on a code without a Helmert shift (NAD 27's is a grid).
        let nad27_zero = r#"GEOGCS["NAD27",DATUM["North_American_Datum_1927",SPHEROID["Clarke 1866",6378206.4,294.978698213898],TOWGS84[0,0,0,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4267"]]"#;
        assert_eq!(
            crs_from_wkt(&bound_crs(BESSEL_WKT2, nad27_zero, &BESSEL_SHIFT)),
            Err("NAD27 gives a shift of zero to WGS 84 where its datum has one".into())
        );
        let wgs84_zero = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563],TOWGS84[0,0,0,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4326"]]"#;
        assert!(metres_apart(at(&bound_crs(BESSEL_WKT2, wgs84_zero, &BESSEL_SHIFT)), to_wgs84) < 0.001);
        // A geocentric WGS 84 target, as EPSG defines Helmert transformations, is WGS 84.
        let geocentric = r#"GEODCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[Cartesian,3],AXIS["(X)",geocentricX],AXIS["(Y)",geocentricY],AXIS["(Z)",geocentricZ],LENGTHUNIT["metre",1],ID["EPSG",4978]]"#;
        let ll = at(&bound_crs(BESSEL_WKT2, geocentric, &BESSEL_SHIFT));
        assert!(metres_apart(ll, to_wgs84) < 0.001, "{ll:?}");
        // So is one whose EPSG code is bundled on GRS 1980 with no shift, as matching a file's
        // shift to its code takes it: NAD83(2011), whose datum isn't known by its name.
        let nad83_2011 = r#"GEOGCRS["NAD83(2011)",DATUM["NAD83 (National Spatial Reference System 2011)",ELLIPSOID["GRS 1980",6378137,298.257222101,LENGTHUNIT["metre",1]]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",6318]]"#;
        assert_eq!(epsg_proj4(6318), Some("+proj=longlat +ellps=GRS80 +no_defs"));
        let ll = at(&bound_crs(BESSEL_WKT2, nad83_2011, &BESSEL_SHIFT));
        assert!(metres_apart(ll, to_wgs84) < 0.001, "{ll:?}");
        // A three-parameter shift to WGS 84 stays three.
        let def = crs_from_wkt(&bound_crs(BESSEL_WKT2, WGS84_WKT2, &[1.0, 2.0, 3.0])).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=1,2,3 "), "{}", def.proj4);
        // A target whose shift isn't known (NAD 27's isn't a Helmert one), or none, is refused.
        let nad27 = r#"GEOGCRS["NAD27",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4267]]"#;
        let unknown = BESSEL_WKT2.replace("Local", "Other");
        for (target, name) in [(nad27.to_string(), "NAD27"), (nad27.replace(r#",ID["EPSG",4267]"#, ""), "NAD27"), (unknown, "Other")] {
            let e = crs_from_wkt(&bound_crs(BESSEL_WKT2, &target, &BESSEL_SHIFT)).unwrap_err();
            assert_eq!(e, format!("the shift from {name} to WGS 84 isn't known"));
        }
        // Rotations whose product, left out, could reach a centimetre are refused.
        let turned = [598.1, 73.7, 418.2, 100.0, 100.0, 100.0, 6.7];
        let e = crs_from_wkt(&bound_crs(BESSEL_WKT2, OSGB36_WKT2, &turned)).unwrap_err();
        assert_eq!(e, "the bound coordinate system's rotations are too large to combine");
        let no_target = bound_crs(BESSEL_WKT2, WGS84_WKT2, &BESSEL_SHIFT).replace(&format!("TARGETCRS[{WGS84_WKT2}],"), "");
        assert_eq!(crs_from_wkt(&no_target), Err("the bound coordinate system has no target".into()));
        // A source without a datum has nowhere to keep the shift: refused, not read without it.
        let no_datum =
            BESSEL_WKT2.replace(r#"DATUM["Local datum",ELLIPSOID["Bessel 1841",6377397.155,299.1528128,LENGTHUNIT["metre",1]]],"#, "");
        assert_ne!(no_datum, BESSEL_WKT2);
        assert_eq!(
            crs_from_wkt(&bound_crs(&no_datum, WGS84_WKT2, &BESSEL_SHIFT)),
            Err("the bound coordinate system's source has no datum to give the shift to".into())
        );
    }

    /// The shift a file gives wins over the one bundled for its system's EPSG code. The code is
    /// used only when the two are the same.
    #[test]
    fn a_files_own_shift_wins_over_its_epsg_code() {
        let osgb = epsg_proj4(4277).and_then(earth_of).unwrap().1;
        assert_eq!(read_wkt(&bound_crs(OSGB36_WKT2, WGS84_WKT2, &osgb)), Ok((Crs::Epsg(4277), None)));
        let (crs, note) = read_wkt(&bound_crs(OSGB36_WKT2, WGS84_WKT2, &[375.0, -111.0, 431.0])).unwrap();
        assert!(matches!(crs, Crs::Custom(_)) && note.is_none(), "{crs:?} {note:?}");
        // A projected source, in WKT2 and in WKT1 with a `TOWGS84`.
        let rows = corpus();
        let bng = format!(r#"{},ID["EPSG",27700]]"#, rows.iter().find(|r| r.0 == "EPSG:27700").unwrap().2.strip_suffix(']').unwrap());
        assert_eq!(crs_from_wkt(&bound_crs(&bng, WGS84_WKT2, &osgb)), Ok(Crs::Epsg(27700)));
        let def = crs_from_wkt(&bound_crs(&bng, WGS84_WKT2, &[375.0, -111.0, 431.0])).unwrap().custom_def().unwrap();
        assert!(def.proj4.starts_with("+proj=tmerc +lat_0=49") && def.proj4.contains("+towgs84=375,-111,431 "), "{}", def.proj4);
        // A WKT1 source with a `TOWGS84` of its own: the bound transformation replaces it.
        let bng_wkt1 = to_wkt(Crs::Epsg(27700)).unwrap();
        assert!(bng_wkt1.contains("TOWGS84[446.448,"), "{bng_wkt1}");
        let def = crs_from_wkt(&bound_crs(&bng_wkt1, WGS84_WKT2, &[375.0, -111.0, 431.0])).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=375,-111,431 ") && !def.proj4.contains("446.448"), "{}", def.proj4);
        // A system that can't be read from its parameters (Krovak North Orientated, in WKT2) is
        // read by its code, with the code's shift instead of the file's, said.
        let krovak = format!(r#"{},ID["EPSG",5514]]"#, rows.iter().find(|r| r.0 == "EPSG:5514").unwrap().2.strip_suffix(']').unwrap());
        assert_eq!(epsg_proj4(5514).and_then(earth_of).unwrap().1, [589.0, 76.0, 480.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(read_wkt(&bound_crs(&krovak, WGS84_WKT2, &[589.0, 76.0, 480.0])), Ok((Crs::Epsg(5514), None)));
        let (crs, note) = read_wkt(&bound_crs(&krovak, WGS84_WKT2, &[570.8, 85.7, 462.8])).unwrap();
        assert_eq!(crs, Crs::Epsg(5514));
        assert_eq!(
            note.as_deref(),
            Some(
                "the file's own shift to WGS 84 is set aside for EPSG:5514's, because the Krovak (North Orientated) projection isn't supported"
            )
        );
        // With a code that has no shift of its own (NAD 27's is a grid), the notice says the file's
        // shift isn't applied at all.
        assert_eq!(epsg_proj4(4267).and_then(bundled_shift), None);
        let nad27_code = krovak.replace(r#"ID["EPSG",5514]]"#, r#"ID["EPSG",4267]]"#);
        assert_ne!(nad27_code, krovak);
        let (crs, note) = read_wkt(&bound_crs(&nad27_code, WGS84_WKT2, &[570.8, 85.7, 462.8])).unwrap();
        assert_eq!(crs, Crs::Epsg(4267));
        assert_eq!(
            note.as_deref(),
            Some(
                "the file's own shift to WGS 84 is set aside, because the Krovak (North Orientated) projection isn't supported, and EPSG:4267 is read without it, so positions may be off by as much as that shift"
            )
        );
        // So it does with a code whose shift is none (WGS 84's).
        let wgs84_code = krovak.replace(r#"ID["EPSG",5514]]"#, r#"ID["EPSG",4326]]"#);
        let (crs, note) = read_wkt(&bound_crs(&wgs84_code, WGS84_WKT2, &[570.8, 85.7, 462.8])).unwrap();
        assert_eq!(crs, Crs::LonLat);
        assert!(note.as_deref().is_some_and(|n| n.contains("EPSG:4326 is read without it")), "{note:?}");
        let utm_code = krovak.replace(r#"ID["EPSG",5514]]"#, r#"ID["EPSG",32631]]"#);
        let (crs, note) = read_wkt(&bound_crs(&utm_code, WGS84_WKT2, &[570.8, 85.7, 462.8])).unwrap();
        assert_eq!(crs, Crs::Epsg(32631));
        assert!(note.as_deref().is_some_and(|n| n.contains("EPSG:32631 is read without it")), "{note:?}");
        let wkt1 = to_wkt(Crs::Epsg(27700)).unwrap();
        assert!(wkt1.contains("TOWGS84[446.448,-125.157,542.06,0.15,0.247,0.842,-20.489]"), "{wkt1}");
        // A shift of zero where the code has one is the file's too, and said.
        let zero = wkt1.replace("TOWGS84[446.448,-125.157,542.06,0.15,0.247,0.842,-20.489]", "TOWGS84[0,0,0,0,0,0,0]");
        assert_ne!(zero, wkt1);
        let (crs, note) = read_wkt(&zero).unwrap();
        assert!(matches!(crs, Crs::Custom(_)), "{crs:?}");
        assert_eq!(
            note.as_deref(),
            Some("the file gives a shift of zero to WGS 84, which is used in place of EPSG:27700's transformation")
        );
        assert_eq!(crs_from_wkt(&wkt1), Ok(Crs::Epsg(27700)));
        let other = wkt1.replace("TOWGS84[446.448,-125.157,542.06,0.15,0.247,0.842,-20.489]", "TOWGS84[375,-111,431,0,0,0,0]");
        let def = crs_from_wkt(&other).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=375,-111,431,0,0,0,0"), "{}", def.proj4);
        // The same shift rounded otherwise, as GDAL writes Hong Kong 1980's, is the same.
        let hk = to_wkt(Crs::Epsg(4611)).unwrap();
        let bundled = "TOWGS84[-162.619,-276.959,-161.764,0.067753,-2.243649,-1.158827,-1.094246]";
        assert!(hk.contains(bundled), "{hk}");
        let gdal = hk.replace(bundled, "TOWGS84[-162.619,-276.959,-161.764,0.067753,-2.243648,-1.158828,-1.094246]");
        assert_eq!(crs_from_wkt(&gdal), Ok(Crs::Epsg(4611)));
        // NAD 27's code has no bundled shift: a shift the file gives, zero too, is used.
        let nad27 = r#"GEOGCS["NAD27",DATUM["North_American_Datum_1927",SPHEROID["Clarke 1866",6378206.4,294.978698213898],TOWGS84[0,0,0,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4267"]]"#;
        let def = crs_from_wkt(nad27).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=0,0,0,0,0,0,0"), "{}", def.proj4);
        // A WGS 84 code needs no shift: it is kept with a shift of zero, not with another.
        let utm = format!(r#"{},AUTHORITY["EPSG","32631"]]"#, ESRI_32631.strip_suffix(']').unwrap());
        let towgs84 = |t: &str| utm.replace("298.257223563]]", &format!("298.257223563],{t}]"));
        assert_eq!(crs_from_wkt(&towgs84("TOWGS84[0,0,0]")), Ok(Crs::Epsg(32631)));
        assert!(matches!(crs_from_wkt(&towgs84("TOWGS84[1,2,3]")), Ok(Crs::Custom(_))));
    }

    /// Another body's system with an EPSG code (the Earth's) is read from its parameters, not as
    /// the code's Earth system: refused when it can't be read, with a shift of its own or without.
    #[test]
    fn another_bodys_system_is_not_read_by_an_epsg_code() {
        let mars = r#"GEOGCS["Mars_2015_Ographic",DATUM["D_Mars_2015",SPHEROID["Mars_2015",3396190.0,169.894447223612]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433],AUTHORITY["EPSG","4326"]]"#;
        assert_eq!(read_wkt(mars), Err(planetary::OGRAPHIC.to_string()));
        let shifted = mars.replace("169.894447223612]]", "169.894447223612],TOWGS84[1,2,3]]");
        assert_ne!(shifted, mars);
        assert_eq!(read_wkt(&shifted), Err(planetary::OGRAPHIC.to_string()));
        // A planetocentric one is read as the Mars system it is.
        let ocentric = mars.replace("Mars_2015_Ographic", "Mars_2015_Ocentric").replace("169.894447223612", "0");
        let (crs, _) = read_wkt(&ocentric).unwrap();
        assert!(crs.is_planetary(), "{crs:?}");
    }

    /// Web Mercator's codes with a shift of zero, as GDAL writes them, are Web Mercator. Read from
    /// their parameters, as Mercator on an ellipsoid, points would be 21 km off.
    #[test]
    fn web_mercator_with_a_zero_shift_is_web_mercator() {
        // EPSG:3785, and GDAL 1's EPSG:900913.
        let popular = r#"PROJCS["Popular Visualisation CRS / Mercator",GEOGCS["Popular Visualisation CRS (deprecated)",DATUM["Popular_Visualisation_Datum",SPHEROID["Popular Visualisation Sphere",6378137,0,AUTHORITY["EPSG","7059"]],TOWGS84[0,0,0,0,0,0,0],AUTHORITY["EPSG","6055"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AXIS["Latitude",NORTH],AXIS["Longitude",EAST],AUTHORITY["EPSG","4055"]],PROJECTION["Mercator_1SP"],PARAMETER["central_meridian",0],PARAMETER["scale_factor",1],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH],EXTENSION["PROJ4","+proj=merc +a=6378137 +b=6378137 +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +k=1 +units=m +nadgrids=@null +wktext +no_defs"],AUTHORITY["EPSG","3785"]]"#;
        let google = r#"PROJCS["Google Maps Global Mercator",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],TOWGS84[0,0,0,0,0,0,0],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.01745329251994328,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]],PROJECTION["Mercator_2SP"],PARAMETER["standard_parallel_1",0],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",0],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["Meter",1],EXTENSION["PROJ4","+proj=merc +a=6378137 +b=6378137 +lat_ts=0.0 +lon_0=0.0 +x_0=0.0 +y_0=0 +k=1.0 +units=m +nadgrids=@null +wktext  +no_defs"],AUTHORITY["EPSG","900913"]]"#;
        for wkt in [popular, google] {
            assert_eq!(read_wkt(wkt), Ok((Crs::WebMercator, None)), "{wkt}");
            // PROJ puts 6 679 169.4476 m E, 5 621 521.4862 m N at 60°E 45°N.
            let ll = Crs::WebMercator.transformer().unwrap().to_lonlat(Coord::new(6_679_169.447_596_414, 5_621_521.486_192_066)).unwrap();
            assert!(metres_apart(ll, Coord::new(60.0, 45.0)) < 0.001, "{ll:?}");
        }
        // Another shift is the file's own, read from its parameters.
        let shifted = google.replace("TOWGS84[0,0,0,0,0,0,0]", "TOWGS84[1,2,3,0,0,0,0]");
        assert!(matches!(crs_from_wkt(&shifted), Ok(Crs::Custom(_))));
        // A system named and projected as Web Mercator stays Web Mercator, and a shift its file
        // gives is said to be set aside.
        let esri = ESRI_3857.replace("6378137.0,298.257223563]]", "6378137.0,298.257223563],TOWGS84[1,2,3]]");
        assert_ne!(esri, ESRI_3857);
        let (crs, note) = read_wkt(&esri).unwrap();
        assert_eq!(crs, Crs::WebMercator);
        assert_eq!(note.as_deref(), Some("the file's own shift to WGS 84 is set aside, as Web Mercator is on WGS 84"));
        assert_eq!(read_wkt(ESRI_3857), Ok((Crs::WebMercator, None)));
        let zero = ESRI_3857.replace("6378137.0,298.257223563]]", "6378137.0,298.257223563],TOWGS84[0,0,0]]");
        assert_eq!(read_wkt(&zero), Ok((Crs::WebMercator, None)));
    }

    /// A Web Mercator name or method gives `Crs::WebMercator` only with Web Mercator's parameters.
    #[test]
    fn a_web_mercator_name_gives_web_mercator_only_with_its_parameters() {
        assert_eq!(crs_from_wkt(ESRI_3857), Ok(Crs::WebMercator));
        let gdal = WEB_MERCATOR;
        assert_eq!(crs_from_wkt(gdal), Ok(Crs::WebMercator));
        // Esri's method: a parameter off is refused, never placed at Web Mercator's origin.
        for (from, to) in [
            (r#""False_Easting",0.0"#, r#""False_Easting",1000.0"#),
            (r#""False_Northing",0.0"#, r#""False_Northing",1000.0"#),
            (r#""Central_Meridian",0.0"#, r#""Central_Meridian",10.0"#),
            (r#""Auxiliary_Sphere_Type",0.0"#, r#""Auxiliary_Sphere_Type",1.0"#),
            (r#"UNIT["Meter",1.0]]"#, r#"UNIT["Foot_US",0.3048006096012192]]"#),
        ] {
            let off = ESRI_3857.replace(from, to);
            assert_ne!(off, ESRI_3857, "{from}");
            assert!(crs_from_wkt(&off).is_err(), "{from}: {:?}", crs_from_wkt(&off));
        }
        // The same name on a Mercator method is read from its parameters: 1 km east here. Not
        // with GDAL's EXTENSION, which says the sphere in a form the parameters don't. (Without
        // the EPSG code: a file's own code is trusted over its parameters.)
        let shift = |east: &str| {
            gdal.replace(r#"PARAMETER["false_easting",0]"#, &format!(r#"PARAMETER["false_easting",{east}]"#))
                .replace(r#",AUTHORITY["EPSG","3857"]]"#, "]")
        };
        let no_ext = |east: &str| {
            let w = shift(east);
            let from = w.find(",EXTENSION[").unwrap();
            let to = from + w[from..].find(r#""]"#).unwrap() + 2;
            format!("{}{}", &w[..from], &w[to..])
        };
        let e = crs_from_wkt(&shift("1000")).unwrap_err();
        assert!(e.contains("isn't supported"), "{e}");
        let shifted = no_ext("1000");
        assert!(!shifted.contains("EXTENSION") && !shifted.contains("3857") && shifted.contains("1000"), "{shifted}");
        let crs = crs_from_wkt(&shifted).unwrap();
        assert!(matches!(crs, Crs::Custom(_)), "{crs:?}");
        let ll = Coord::new(60.0, 45.0);
        let xy = crs.transformer().unwrap().from_lonlat(ll).unwrap();
        // The same system 2 km east of the origin: 1 km further than the first, on the file's
        // ellipsoid as written.
        let web = crs_from_wkt(&no_ext("2000")).unwrap().transformer().unwrap().from_lonlat(ll).unwrap();
        assert!((web.x - xy.x - 1000.0).abs() < 0.001 && (xy.y - web.y).abs() < 0.001, "{xy:?} {web:?}");
        // A datum with another radius isn't Web Mercator's: Esri's method is refused, and a
        // Mercator method is read from its parameters.
        let clarke = ESRI_3857.replace("6378137.0,298.257223563", "6378206.4,294.978698213898");
        assert_ne!(clarke, ESRI_3857);
        assert!(crs_from_wkt(&clarke).is_err(), "{:?}", crs_from_wkt(&clarke));
        let mercator = no_ext("0").replace("6378137,298.257223563", "6378206.4,294.978698213898");
        assert!(mercator.contains("6378206.4"));
        assert!(matches!(crs_from_wkt(&mercator), Ok(Crs::Custom(_))), "{:?}", crs_from_wkt(&mercator));
        // WKT2's method: Web Mercator without a shift, refused with one.
        let wkt2 = include_str!("wkt2_corpus.tsv")
            .lines()
            .find(|l| l.contains("Popular Visualisation Pseudo Mercator"))
            .and_then(|l| l.rsplit('\t').next())
            .unwrap();
        assert_eq!(crs_from_wkt(wkt2), Ok(Crs::WebMercator));
        let east = wkt2.replace(r#"PARAMETER["False easting",0,"#, r#"PARAMETER["False easting",1000,"#);
        assert_ne!(east, wkt2);
        let e = crs_from_wkt(&east).unwrap_err();
        assert!(e.contains("a Web Mercator system with parameters of its own") && e.contains("isn't supported"), "{e}");
        // A parameter that isn't a finite number isn't a default: refused, never read as one.
        for huge in [
            ESRI_3857.replace(r#""False_Easting",0.0"#, r#""False_Easting",1e400"#),
            no_ext("1e400"),
            wkt2.replace(r#"PARAMETER["False easting",0,"#, r#"PARAMETER["False easting",1e400,"#),
        ] {
            assert!(huge.contains("1e400"), "{huge}");
            let e = crs_from_wkt(&huge).unwrap_err();
            assert!(e.contains("isn't a finite number"), "{e}");
        }
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
