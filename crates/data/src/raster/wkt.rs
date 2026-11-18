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
            _ => return Err(format!("expected '[' after {}", clip(&name))),
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
                _ => return Err(format!("unexpected text in {}", clip(&name))),
            };
            items.push(item);
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b) if b == close => {
                    self.at += 1;
                    return Ok(Node { name, items });
                }
                _ => return Err(format!("expected ',' in {}", clip(&name))),
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
        t.parse().map_err(|_| format!("'{}' isn't a number", clip(t)))
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

/// The coordinate system a WKT text (WKT1, OGC or Esri, or WKT2) describes, without the note
/// [`read_wkt`] gives: a reader that can pass a notice on calls that instead.
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
///
/// A shift to WGS 84 the file gives that can't be applied (a grid, [`unapplied`]) is said,
/// whichever way the system is read: by its code, by a UTM name or from its parameters. So is a
/// bound system without a transformation, read as its source ([`untransformed`]).
pub fn read_wkt(text: &str) -> Result<(Crs, Option<String>), String> {
    // As the parser reads it: without a byte order mark (a `.prj` saved on Windows) or the space
    // around it, which terrio-crs doesn't take and the kept WKT doesn't need.
    let text = text.trim_start_matches('\u{feff}').trim();
    read_tree(&tree(text)?, None, text)
}

/// Whether WKT `text` is read in place of the EPSG `code` RustGIS knows that comes with it (a
/// GeoPackage row's, a File Geodatabase's WKID):
/// - `None`: the code is used. The text gives no shift to WGS 84, or the code's own, or it isn't
///   WKT.
/// - `Some(Ok(..))`: the system read from the text in place of the code, with its note. A text
///   that gives another shift but can't be read with it gives the code's system, and the note
///   says that the text's shift is set aside.
/// - `Some(Err(..))`: the text comes first but is refused: another body's system that can't be
///   read.
///
/// The code stands in for the text's own `AUTHORITY`, and the same rule applies ([`read_root`]).
/// EPSG's codes are the Earth's, so another body's system is read from its text whatever the code.
pub fn read_wkt_over_code(text: &str, code: u32) -> Option<Result<(Crs, Option<String>), String>> {
    let text = text.trim_start_matches('\u{feff}').trim();
    // Another body is looked for in the text first: `tree` can't parse a WKT2 whose longitudes
    // count west, so a check on the tree would miss it.
    if planetary_datum(text).is_some() {
        return Some(read_wkt(text));
    }
    let root = match tree(text) {
        Ok(root) => root,
        // A bound system that can't be read (its target's shift to WGS 84 isn't known, say)
        // still gives a transformation. The code is used without it, which is said. Other text
        // that can't be read gives no shift to set aside.
        Err(e) => {
            let by_code = Crs::from_epsg(code)?;
            let transformed = parse(text).is_ok_and(|n| has_bound_transformation(&n, 0));
            return transformed.then(|| Ok((by_code, Some(set_aside_note(code, by_code, &e)))));
        }
    };
    let files_shift = towgs84(geogcs(&root).and_then(|g| g.child("DATUM")));
    if files_shift.is_none() || Crs::from_epsg(code).is_some_and(|by_code| places_as(text, by_code)) {
        return None;
    }
    Some(read_tree(&root, Some(code), text))
}

/// What is said when WKT `text` gives a shift to WGS 84 other than PROJ definition `def`'s, and
/// the system is read by `def` (named `name`) all the same, because of `why`. `None` when the
/// text gives no shift, or `def`'s, as [`read_wkt_over_code`] tells them apart.
///
/// A definition that gives no shift counts as a shift of zero only on WGS 84 or GRS 1980
/// ([`bundled_shift`]). On another ellipsoid it matches no shift the text gives, not even zero:
/// no datum is then changed, not even the ellipsoid, which any shift, zero too, would change.
/// The note then says the ellipsoid beside the shift.
pub(crate) fn set_aside_shift_note(text: &str, name: &str, def: &str, why: &str) -> Option<String> {
    let defs_shift = bundled_shift(def);
    let (another, zero) = match tree(text) {
        Ok(root) => {
            let shift = towgs84(geogcs(&root).and_then(|g| g.child("DATUM")));
            (shift.as_ref().is_some_and(|s| !matches_shift(defs_shift, s)), shift.is_some_and(|s| s.iter().all(|v| *v == 0.0)))
        }
        // A bound system that can't be read still gives a transformation.
        Err(_) => (parse(text).is_ok_and(|n| has_bound_transformation(&n, 0)), false),
    };
    let codes_shift = defs_shift.is_some_and(|s| s.iter().any(|v| *v != 0.0));
    let ellipsoid = "is read on its own ellipsoid without it, so positions may be off by";
    another.then(|| match (defs_shift, zero) {
        (None, true) => format!(
            "the file's shift of zero to WGS 84 is set aside, because {why}, and {name} {ellipsoid} the difference between that ellipsoid and WGS 84's"
        ),
        (None, false) => format!(
            "the file's own shift to WGS 84 is set aside, because {why}, and {name} {ellipsoid} as much as that shift and the difference between that ellipsoid and WGS 84's"
        ),
        _ => set_aside_wording(name, codes_shift, why),
    })
}

/// Whether a parsed WKT2 text holds a `BOUNDCRS` with a transformation of its own, whether or not
/// [`bound`] can read it. The bound system may be nested: a compound system's horizontal part,
/// or another bound system's source. A transformation outside a `BOUNDCRS` shifts no system.
fn has_bound_transformation(n: &Node, depth: usize) -> bool {
    let bound = n.name.eq_ignore_ascii_case("BOUNDCRS") && n.child("ABRIDGEDTRANSFORMATION").is_some();
    depth <= MAX_DEPTH && (bound || n.nodes().any(|c| has_bound_transformation(c, depth + 1)))
}

/// [`read_wkt`] of a WKT1 tree. `code`, an EPSG code that comes with the text, replaces the tree's
/// own `AUTHORITY` ([`read_root`]).
fn read_tree(root: &Node, code: Option<u32>, text: &str) -> Result<(Crs, Option<String>), String> {
    let (crs, note) = read_root(root, code, text)?;
    // A system without a datum, read on WGS 84 as before ([`with_wgs84_datum`]; a geographic one
    // on Greenwich as WGS 84 longitude/latitude), which PROJ 9.8.1 refuses.
    // Not one read by its EPSG code (`AUTHORITY["EPSG","4326"]`), which is that code exactly.
    let by_code = earth_code(root, code).is_some();
    let note = match geogcs(root).filter(|g| (crs.custom_def().is_some() || (crs == Crs::LonLat && !by_code)) && g.child("DATUM").is_none())
    {
        Some(_) => {
            let said = format!(
                "the coordinate system {} has no datum and is read on WGS 84, so positions may be off by up to a few hundred metres",
                clip(root.title())
            );
            Some(note.map_or(said.clone(), |n| format!("{n}; {said}")))
        }
        None => note,
    };
    let dropped = dropped_note(root, "the file's", crs);
    let note = match (note, dropped) {
        (Some(note), Some(dropped)) => Some(format!("{note}; {dropped}")),
        (note, dropped) => note.or(dropped),
    };
    Ok((crs, note))
}

/// A WKT text as a WKT1 tree, with a grid GDAL gives in its `PROJ4` extension
/// ([`lift_proj4_grids`]) marked on its datum.
fn tree(text: &str) -> Result<Node, String> {
    let mut root = wkt1(parse(text)?, 0)?;
    lift_proj4_grids(&mut root);
    Ok(root)
}

/// GDAL's other form of a grid (`EXTENSION["PROJ4","… +nadgrids=conus …"]` on the system itself)
/// marked on the source's datum ([`UNAPPLIED`]), as [`unapplied`] reads the `PROJ4_GRIDS` one.
/// `@null`, which old Web Mercator files carry, is no shift.
fn lift_proj4_grids(root: &mut Node) {
    let grids =
        root.nodes().filter(|n| n.name.eq_ignore_ascii_case("EXTENSION") && n.title().eq_ignore_ascii_case("PROJ4")).find_map(|e| {
            let Some(Item::Text(def)) = e.items.get(1) else { return None };
            let value = def.split_whitespace().find_map(|k| k.strip_prefix("+nadgrids="))?;
            grid_names(value)
        });
    if let Some(grids) = grids
        && unapplied(geogcs(root).and_then(|g| g.child("DATUM"))).is_none()
    {
        put_mark(root, UNAPPLIED, &format!("grid shift ({grids})"));
    }
}

/// The grid files a comma-separated list names, without `@null` (no shift) and the `@` that marks
/// one optional; `None` when none is left.
fn grid_names(value: &str) -> Option<String> {
    let grids: Vec<&str> =
        value.split(',').map(|g| g.trim().trim_start_matches('@')).filter(|g| !g.is_empty() && !g.eq_ignore_ascii_case("null")).collect();
    (!grids.is_empty()).then(|| grids.join(", "))
}

/// The EPSG code `crs` is read by when its bundled shift to WGS 84 is other than none: a
/// transformation the file gives that isn't applied is then replaced by that shift, not by none.
fn code_with_shift(crs: Crs) -> Option<u16> {
    let code = crs.epsg();
    (code > 0 && epsg_proj4(code).and_then(bundled_shift).is_some_and(|s| s.iter().any(|v| *v != 0.0))).then_some(code)
}

/// What [`read_wkt`] says of a shift to WGS 84 a system gives but RustGIS can't apply
/// ([`unapplied`]), or of a bound system without a transformation ([`untransformed`]); `whose`
/// says whose it is (`the file's`), and `crs` is the system it is read as.
fn dropped_note(root: &Node, whose: &str, crs: Crs) -> Option<String> {
    dropped_note_by(root, whose, code_with_shift(crs).map(|code| format!("EPSG:{code}")).as_deref())
}

/// [`dropped_note`] for a system read by `by`, a code whose own shift to WGS 84 is used in place
/// of one the file gives that isn't applied, or by none.
fn dropped_note_by(root: &Node, whose: &str, by: Option<&str>) -> Option<String> {
    let datum = geogcs(root).and_then(|g| g.child("DATUM"));
    // On the source's datum, or on the source itself when it has none ([`put_mark`]).
    if let Some(what) = unapplied(datum).or_else(|| mark(Some(root), UNAPPLIED)) {
        let what = clip(&what);
        // Read by a code whose own shift is applied in its place: off by the difference.
        return Some(match by {
            Some(code) => format!(
                "{whose} {what} isn't applied; {code}'s own shift to WGS 84 is used in its place, so positions may be off by the difference between the two"
            ),
            None => format!("{whose} {what} isn't applied, so positions may be off by as much as that shift"),
        });
    }
    let to = untransformed(datum).or_else(|| mark(Some(root), UNTRANSFORMED))?;
    Some(format!("{whose} bound coordinate system has no transformation to {}, so it is read as its source system", clip(&to)))
}

/// What [`read_wkt`] would say of a shift WKT `text` gives but RustGIS can't apply, for a system
/// read otherwise as `crs`: by its EPSG code, or by [`Crs::parse`], which drops [`read_wkt`]'s
/// note. `None` for text that isn't WKT.
pub fn unapplied_note(text: &str, crs: Crs) -> Option<String> {
    dropped_note(&tree(text).ok()?, "the definition's", crs)
}

/// [`unapplied_note`] for a system read by PROJ definition `def`, named `name` (an Esri code), whose
/// own shift to WGS 84, when it has one, is used in place of the one the text gives.
pub(crate) fn unapplied_note_beside(text: &str, name: &str, def: &str) -> Option<String> {
    let by = bundled_shift(def).is_some_and(|s| s.iter().any(|v| *v != 0.0)).then_some(name);
    dropped_note_by(&tree(text).ok()?, "the definition's", by)
}

/// The notes a coordinate system given as text keeps when [`Crs::parse`] reads it as `crs`: its
/// projection compatibility choice ([`projection_compatibility_note`]) and a shift it doesn't
/// apply ([`unapplied_note`]).
pub fn typed_crs_note(text: &str, crs: Crs) -> Option<String> {
    compatibility_and_dropped(text, crs, "the definition's")
}

/// [`typed_crs_note`] for a file's own text, whose other notes are said otherwise (another body's
/// `.prj` next to a grid): the shift it doesn't apply is the file's.
pub(crate) fn file_crs_note(text: &str, crs: Crs) -> Option<String> {
    compatibility_and_dropped(text, crs, "the file's")
}

fn compatibility_and_dropped(text: &str, crs: Crs, whose: &str) -> Option<String> {
    let dropped = tree(text).ok().and_then(|root| dropped_note(&root, whose, crs));
    let notes: Vec<String> = projection_compatibility_note(text, crs).map(str::to_owned).into_iter().chain(dropped).collect();
    (!notes.is_empty()).then(|| notes.join("; "))
}

/// [`read_wkt`] of a system in WKT1 form, without what it says of a shift it can't apply. `code`
/// is an EPSG code that comes with the text, used in place of its own `AUTHORITY`.
fn read_root(root: &Node, code: Option<u32>, text: &str) -> Result<(Crs, Option<String>), String> {
    let Some((code, by_code)) = earth_code(root, code) else {
        return read_system(root, text);
    };
    // A shift the file gives (a `TOWGS84`, or a `BOUNDCRS`'s transformation) wins over the
    // transformation of its EPSG code, as in PROJ and GDAL. The code is used only when the two
    // place points alike.
    let files_shift = towgs84(geogcs(root).and_then(|g| g.child("DATUM")));
    if files_shift.is_none() || places_as(text, by_code) {
        return Ok((by_code, projection_note(root, by_code).map(str::to_owned)));
    }
    // A system that can't be read from its parameters is still read by its EPSG code, with the
    // code's shift in place of the file's, which is said.
    match read_system(root, text) {
        Err(e) => Ok((by_code, with_projection_note(Some(set_aside_note(code, by_code, &e)), root, by_code))),
        // A shift of zero, where the code has one, may be a writer's placeholder: said, as the
        // positions then differ from the code's by that shift.
        Ok((crs, note)) if files_shift.is_some_and(|s| s.iter().all(|v| *v == 0.0)) => {
            let zero = format!("the file gives a shift of zero to WGS 84, which is used in place of EPSG:{code}'s transformation");
            Ok((crs, Some(note.map_or(zero.clone(), |n| format!("{n}; {zero}")))))
        }
        read => read,
    }
}

/// Whether WKT `text`, as terrio-crs reads it, places points as `crs` does: within a centimetre
/// (or the degrees of one) at the centre of the area of use of its EPSG code. A file's shift to
/// WGS 84 that is its code's transformation then reads as the code. Only there: the text RustGIS
/// writes for some codes approximates them away from it (Guam's projection, EPSG:3993, 1.5 cm at
/// 0.1°) and is read back as the code; and a shift on WGS 84 itself, which a WGS 84 or Web
/// Mercator code's file may give, is none for PROJ 9.8.1 anywhere.
pub(crate) fn places_as(text: &str, crs: Crs) -> bool {
    let (Ok(file), Ok(code)) = (super::crs::transformer_of_wkt(text), super::crs::not_waiting_for_grids(|| crs.transformer())) else {
        return false;
    };
    let centre = match crs {
        Crs::Epsg(c) => super::crs::area_centre(c),
        _ => None,
    }
    .unwrap_or(rustgis_geom::Coord::new(0.0, 0.0));
    let (Some(a), Some(b)) = (file.from_lonlat(centre), code.from_lonlat(centre)) else { return false };
    let apart = (a.x - b.x).hypot(a.y - b.y);
    if code.is_geographic() { apart < 1e-7 } else { apart < 0.01 }
}

/// What is said of a file's shift to WGS 84 when its system is read by EPSG `code` (`by_code`)
/// without it, because of `why`.
fn set_aside_note(code: u32, by_code: Crs, why: &str) -> String {
    // A code whose shift is none (WGS 84's, or a WGS 84-like datum's) doesn't replace the file's
    // either.
    let codes_shift = !matches!(by_code, Crs::LonLat | Crs::WebMercator)
        && u16::try_from(code).ok().and_then(epsg_proj4).and_then(bundled_shift).is_some_and(|s| s.iter().any(|v| *v != 0.0));
    set_aside_wording(&format!("EPSG:{code}"), codes_shift, why)
}

/// [`set_aside_note`] for the system `name`, which has a shift of its own (`codes_shift`) or none.
fn set_aside_wording(name: &str, codes_shift: bool, why: &str) -> String {
    if codes_shift {
        format!("the file's own shift to WGS 84 is set aside for {name}'s, because {why}")
    } else {
        format!(
            "the file's own shift to WGS 84 is set aside, because {why}, and {name} is read without it, so positions may be off by as much as that shift"
        )
    }
}

/// The EPSG code a WKT1 system on the Earth may be read by, with the system it names: `code` when
/// one comes with the text, else its own `AUTHORITY`'s. `None` for another body's system, which is
/// read from its parameters, as EPSG's codes are the Earth's.
fn earth_code(root: &Node, code: Option<u32>) -> Option<(u32, Crs)> {
    if on_another_body(geogcs(root)) {
        return None;
    }
    let code = code.or_else(|| root.child("AUTHORITY").filter(|a| a.title().eq_ignore_ascii_case("EPSG")).and_then(authority_code))?;
    Some((code, Crs::from_epsg(code)?))
}

/// A WKT1 system read from its parameters, with its note ([`read_wkt`]).
fn read_system(root: &Node, text: &str) -> Result<(Crs, Option<String>), String> {
    match root.name.to_ascii_uppercase().as_str() {
        "GEOGCS" => {
            if on_another_body(Some(root)) && planetographic(root) && !counts_east(root) {
                return Err(planetary::OGRAPHIC.into());
            }
            let crs = geographic(root, text)?;
            Ok((crs, note(crs, Some(root))))
        }
        "PROJCS" => {
            if on_another_body(root.child("GEOGCS")) {
                mirrored_projection(root)?;
            }
            let crs = projected(root, text)?;
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
        other => Err(format!("{} coordinate systems aren't supported", clip(other))),
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

/// Degrees in a unit of a geographic system's coordinates, from its WKT `text`: WKT1's `UNIT`, or
/// WKT2's `ANGLEUNIT` of the system or of its first axis (not its prime meridian's), of a bound
/// system's source or a compound one's first part. 1 when it
/// gives none or isn't WKT this reader parses.
pub(crate) fn angle_unit_degrees(text: &str) -> f64 {
    let Ok(mut root) = parse(text) else { return 1.0 };
    // A bound system's source, a compound one's first (horizontal) part.
    while let Some(inner) = match root.name.to_ascii_uppercase().as_str() {
        "BOUNDCRS" => root.child("SOURCECRS").and_then(|s| s.nodes().next()).cloned(),
        "COMPOUNDCRS" | "COMPD_CS" => {
            root.nodes().find(|n| n.name.to_ascii_uppercase().ends_with("CRS") || n.name.to_ascii_uppercase().ends_with("CS")).cloned()
        }
        _ => None,
    } {
        root = inner;
    }
    let unit = root
        .child_of(&["UNIT", "ANGLEUNIT"])
        .or_else(|| root.nodes().find(|n| n.name.eq_ignore_ascii_case("AXIS")).and_then(|a| a.child("ANGLEUNIT")));
    unit.and_then(|u| u.num(0)).filter(|f| *f > 0.0).map_or(1.0, f64::to_degrees)
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
    earth_params(proj4).map(|p| p.a)
}

/// A definition read for its ellipsoid, prime meridian and unit ([`crate::raster::crs::def_params`]).
fn earth_params(def: &str) -> Option<terrio_crs::ProjParams> {
    crate::raster::crs::def_params(def)
}

/// The ellipsoidal formula of EPSG method 1028, as PROJ 9.8 computes it (PROJ #4656): files
/// written with the spherical formula of PROJ 9.7 and earlier may be placed kilometres away.
/// Said when a WKT names the method on an ellipsoid; a sphere has no discrepancy.
const ELLIPSOIDAL_EQC: &str = "the WKT method is interpreted as EPSG method 1028 (ellipsoidal), as PROJ 9.8 and later compute it; coordinates written with the spherical formula of PROJ 9.7 and earlier are placed up to tens of kilometres away from where that software put them";

/// An equidistant cylindrical WKT naming EPSG method 1028 on an ellipsoid: [`ELLIPSOIDAL_EQC`],
/// and, when its semi-major axis isn't the resolved system's, that the resolved one is used.
fn projection_note(root: &Node, crs: Crs) -> Option<&'static str> {
    let method = root.child("PROJECTION")?.child("AUTHORITY")?;
    if !method.title().eq_ignore_ascii_case("EPSG") || authority_code(method) != Some(1028) {
        return None;
    }
    let spheroid = root.child("GEOGCS")?.child("DATUM")?.child("SPHEROID")?;
    let resolved_axis = eqc_semi_major(crs)?;
    let flattened = spheroid.num(1).is_some_and(|rf| rf.is_finite() && rf > 0.0 && rf.recip() > 1e-12);
    if spheroid.num(0).is_some_and(|axis| (axis - resolved_axis).abs() > resolved_axis.abs() * 1e-12) {
        return Some(if flattened {
            "the equidistant cylindrical WKT semi-major axis differs from the resolved CRS; RustGIS uses the resolved CRS semi-major axis, so coordinates may differ from the declared spheroid; the WKT method is interpreted as EPSG method 1028 (ellipsoidal), as PROJ 9.8 and later compute it; coordinates written with the spherical formula of PROJ 9.7 and earlier are placed up to tens of kilometres away from where that software put them"
        } else {
            "the equidistant cylindrical WKT semi-major axis differs from the resolved CRS; RustGIS uses the resolved CRS semi-major axis, so coordinates may differ from the declared spheroid"
        });
    }
    flattened.then_some(ELLIPSOIDAL_EQC)
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
        // A system read from its WKT: as terrio-crs transforms it, whether or not RustGIS's own
        // tables name its datum (an Esri name whose EPSG system PROJ 9.8.1 has no transformation
        // of, as Qornoq's, has none), as exporting it says ([`Crs::lacks_datum_shift`]).
        if matches!(crs, Crs::Custom(_)) {
            return crs.datum_unshifted().then(|| unshifted_datum_note(geog)).flatten();
        }
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
        return Err(format!("{} coordinate systems aren't supported", clip(kind)));
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
            let a = e.num(0).ok_or_else(|| format!("the ellipsoid {} has no semi-major axis", clip(e.title())))?;
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
    let unusable_unit = unit.is_some() && factor(unit).is_none();
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
    items.push(Item::Node(Node::new("UNIT", unit.map_or("metre", Node::title), (!unusable_unit).then_some(Item::Num(to_m)))));
    items.extend(axes(p));
    items.extend(authority(p));
    Ok(Node::new("PROJCS", p.title(), items))
}

/// The EPSG methods that rotate by the coordinate frame convention: geog2D, geocentric,
/// time-dependent and time-specific, full matrix, geog3D and geog3D to compound.
const FRAME_METHODS: [u32; 8] = [9607, 1032, 1056, 1057, 1066, 1133, 1140, 1149];

/// A `BOUNDCRS` (PROJ's form of a WKT1 `TOWGS84`) as its source system with the shift to WGS 84
/// of its transformation, in metres, arc-seconds and parts per million. A transformation to
/// another system than WGS 84 is followed by that system's own shift ([`onward_shift`]). One
/// without Helmert translations (a grid, NTv2 or NADCON, which PROJ refuses too without the grid
/// file) can't be applied: the source is read without it, which [`read_wkt`] says ([`UNAPPLIED`]).
/// One without a transformation, which ISO 19162 requires and PROJ refuses, is read as its source
/// too, which [`read_wkt`] says ([`UNTRANSFORMED`]): refused, a Shapefile's `.prj` would leave its
/// data read as longitude/latitude, far off for a projected source. In either case a source
/// without a datum is read as it is, the mark on the source itself ([`put_mark`]).
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
    let to = target.as_ref().map(|t| t.title().trim()).filter(|t| !t.is_empty()).unwrap_or("its target").to_string();
    let Some(t) = transformation.as_ref() else {
        // A source on WGS 84 (or a datum read as it) needs no transformation: nothing is lost.
        if wgs84_like(geogcs(&crs).and_then(|g| g.child("DATUM"))).is_none() {
            put_mark(&mut crs, UNTRANSFORMED, &to);
        }
        return Ok(crs);
    };
    let method = t.child("METHOD");
    // PROJ's placeholders. A ballpark offset is no known shift: the source is read alone, and its
    // datum says so as without one. A null offset is a shift of none, applied as such.
    let placeholder = |prefix: &str| method.is_some_and(|m| norm(m.title()).starts_with(prefix));
    if placeholder("BALLPARK_") {
        return Ok(crs);
    }
    let null = placeholder("NULL_");
    if null && source_datum(&mut crs).is_none() {
        return Ok(crs);
    }
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
    let shift = if null { [Some(0.0); 3] } else { shift };
    let Some(mut towgs84) = shift.into_iter().collect::<Option<Vec<f64>>>() else {
        // Named with its method, its target and its grid files, on the source's datum, where the
        // shift would have gone. The target plays no part in where the source is read, so isn't
        // checked.
        let method = method.map(|m| m.title().trim()).filter(|m| !m.is_empty()).unwrap_or("unnamed");
        let files: Vec<&str> = t
            .nodes()
            .filter(|n| n.name.eq_ignore_ascii_case("PARAMETERFILE"))
            .filter_map(|f| f.items.get(1).and_then(|i| if let Item::Text(file) = i { Some(file.trim()) } else { None }))
            .filter(|f| !f.is_empty())
            .collect();
        let files = if files.is_empty() { String::new() } else { format!(" ({})", files.join(", ")) };
        put_mark(&mut crs, UNAPPLIED, &format!("{method} transformation to {to}{files}"));
        return Ok(crs);
    };
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
            return Err(format!("{} gives a shift of zero to WGS 84 where its datum has one", clip(name)));
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
            Err(format!("the shift from {} to WGS 84 isn't known", clip(name)))
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
    // Web Mercator's datum is WGS 84, so its shift is none. (It projects as if on a sphere, which
    // EPSG:3785 gives as its datum's ellipsoid.)
    let bundled = if Crs::from_epsg(code) == Some(Crs::WebMercator) {
        Some([0.0; 7])
    } else {
        u16::try_from(code).ok().and_then(epsg_proj4).and_then(bundled_shift)
    };
    matches_shift(bundled, shift)
}

/// Whether `shift` (three or seven parameters) is `bundled`, a definition's shift
/// ([`bundled_shift`]). `None` matches none.
fn matches_shift(bundled: Option<[f64; 7]>, shift: &[f64]) -> bool {
    // Writers round the last digit differently: GDAL writes Hong Kong 1980's rotations as
    // -2.243648″ and -1.158828″, the bundled shift has -2.243649″ and -1.158827″. Differences
    // under 1e-5 (metres, arc-seconds or parts per million) move a point less than a millimetre.
    const ROUNDING: f64 = 1e-5;
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
fn geographic(g: &Node, text: &str) -> Result<Crs, String> {
    // A WKT1 system's own axes, when it gives them: longitudes counted west would be mirrored.
    if reversed_axes(g) {
        return Err(REVERSED_AXES.into());
    }
    // Angles in another unit than degrees (grads, as PROJ and GDAL write EPSG:4807) are terrio-crs's
    // to read: coordinates are in that unit, as PROJ 9.8.1 reads them.
    if g.child("UNIT").and_then(|u| u.num(0)).is_some_and(|f| (f - 1f64.to_radians()).abs() > 1e-9) {
        return terrio_custom(g, text);
    }
    let datum = g.child("DATUM");
    let greenwich = g.child("PRIMEM").and_then(|m| m.num(0)).is_none_or(|pm| pm == 0.0);
    if greenwich && (datum.is_none() || (wgs84_like(datum).is_some() && !shifted(datum))) {
        return Ok(Crs::LonLat);
    }
    terrio_custom(g, text)
}

/// A projected system: by EPSG code for the common names, else from its WKT `text` through
/// terrio-crs ([`terrio_custom`]).
fn projected(p: &Node, text: &str) -> Result<Crs, String> {
    let invalid_unit = p.child("UNIT").is_some_and(|unit| unit.num(0).is_none());
    let invalid_axis = p
        .child("GEOGCS")
        .and_then(|g| g.child("DATUM"))
        .and_then(|datum| datum.child_of(&["SPHEROID", "ELLIPSOID"]))
        .is_some_and(|ellipsoid| ellipsoid.num(0).is_none());
    if invalid_unit || invalid_axis {
        return Err("a projected system with a unit or datum radius that isn't a finite number isn't supported".into());
    }
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
            // With axes pointing west or south it is another system, read as PROJ 9.8.1 reads it.
            if axis_flip(p)?.is_some() {
                return terrio_custom(p, text);
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
    if let Some(why) = refusal(p, &method) {
        return Err(why);
    }
    terrio_custom(p, text)
}

/// A system terrio-crs reads from the file's WKT `text` (`SpatialRef::from_wkt`), as PROJ 9.8.1
/// does: its axes, angle unit and datum transformation included. `n` is its WKT1 tree, for its name.
fn terrio_custom(n: &Node, text: &str) -> Result<Crs, String> {
    let sr = terrio_crs::SpatialRef::from_wkt(text);
    // What is said when terrio-crs can't read it: its projection, by the name the file gives it.
    // With the reason, when terrio-crs reads the definition (it writes it as a PROJ string) but
    // can't transform it (a latitude of origin past a pole): that projection is supported.
    let refused = |e: &str| {
        let reason = e.strip_prefix("coordinate reference system: ").unwrap_or(e);
        (sr.to_proj4().is_some() && !reason.contains("unsupported projection"))
            .then(|| format!("the coordinate system {} can't be read: {}", clip(n.title()), clip(reason)))
    };
    let unread = || match n.child("PROJECTION").map(Node::title).map(str::trim) {
        // As PROJ 9.8.1: a geographic system needs its datum (ISO 19162).
        _ if geogcs(n).is_some_and(|g| g.child("DATUM").is_none()) => format!("the coordinate system {} has no datum", clip(n.title())),
        Some("") => "the projection isn't named".to_string(),
        Some(method) => format!("the {} projection isn't supported", clip(method)),
        None => format!("the coordinate system {} can't be read", clip(n.title())),
    };
    // Its closest PROJ string, for what reads one (notes, older releases' saved projects); the
    // transform is the WKT's.
    // A system PROJ can't write as a PROJ string (EPSG 9808 with false westing and southing) has
    // none: it is kept by its WKT, and unread only if that can't be transformed either.
    // Where PROJ writes none, the string RustGIS wrote before ([`legacy_proj4`]), so a project
    // saved with the system still opens in an older release.
    let mut proj4 = sr.to_proj4().map(|d| d.replace(" +type=crs", "")).or_else(|| legacy_proj4(text)).unwrap_or_default();
    // terrio-crs writes `+axis` only for westings and southings both; older releases read the
    // others from it too.
    if !proj4.is_empty()
        && !proj4.contains("+axis=")
        && let Ok(Some(axis)) = axis_flip(n)
        && let Some(at) = proj4.find(' ')
    {
        proj4.insert_str(at, &format!(" +axis={axis}"));
    }
    // PROJ writes an Esri `.prj` on a national datum (OSGB36, ED50, DHDN …) as its EPSG system's
    // string, without the shift. An older release reads only this string, so it gets the national
    // datum's bundled `+datum` or `+towgs84` where the file gives none: the shift RustGIS saved
    // before for the datums it named itself, and for a datum known by its Esri name (Pulkovo 1942,
    // PRS92 …) the bundled shift of its EPSG system, near where PROJ places it.
    let datum = geogcs(n).and_then(|g| g.child("DATUM"));
    if !proj4.is_empty()
        && !["+towgs84=", "+datum=", "+nadgrids="].iter().any(|k| proj4.contains(k))
        && towgs84(datum).is_none()
        && let Some(def) = national_datum(datum)
    {
        for t in def.split_whitespace().filter(|t| t.starts_with("+datum=") || t.starts_with("+towgs84=")) {
            proj4.push(' ');
            proj4.push_str(t);
        }
    }
    let label = match n.title().trim() {
        "" => "Custom coordinate system".to_string(),
        t => t.to_string(),
    };
    // A table full of files' systems says so; a system terrio-crs can't transform is unread.
    match Crs::custom(CustomCrs { proj4: proj4.clone(), label: label.clone(), wkt: Some(compact(text)), ..CustomCrs::default() }) {
        Err(e) if e.starts_with("too many") => Err(e),
        Ok(crs) => Ok(crs),
        // GDAL's `PROJ4` extension, which PROJ reads in place of the WKT, can name a grid that
        // isn't here, and PROJ 9.8.1 then refuses the system: read without it, as RustGIS read it
        // before, which [`unapplied_note`] says. So is a `BOUNDCRS` whose transformation isn't
        // applied ([`unapplied_bound_source`]): its source alone, which [`dropped_note`] says.
        // As RustGIS read them before, though PROJ 9.8.1 refuses them: a time-specific shift
        // (EPSG 1066) without its epoch as the coordinate frame shift ([`time_specific_as_frame`]),
        // and a system without a datum on WGS 84 ([`with_wgs84_datum`]): a projected one, or a
        // geographic one in degrees (with its own prime meridian; in another unit it was refused).
        Err(e) => {
            let e = e.strip_prefix(&format!("{proj4}: ")).unwrap_or(&e).to_string();
            // Without the space the parser allows between a keyword and its bracket, so the
            // keywords are found below.
            let text = &compact(text);
            let degrees = n.child("UNIT").and_then(|u| u.num(0)).is_none_or(|f| (f - 1f64.to_radians()).abs() <= 1e-9);
            let bare = without_proj4_extension(text);
            let bare = unapplied_bound_source(bare.as_deref().unwrap_or(text)).or(bare);
            let bare = time_specific_as_frame(bare.as_deref().unwrap_or(text)).or(bare);
            match with_wgs84_datum(bare.as_deref().unwrap_or(text), n.name.eq_ignore_ascii_case("GEOGCS") && degrees).or(bare) {
                Some(bare) => {
                    let proj4 = terrio_crs::SpatialRef::from_wkt(&bare)
                        .to_proj4()
                        .map(|d| d.replace(" +type=crs", ""))
                        .or_else(|| legacy_proj4(&bare))
                        .unwrap_or_default();
                    Crs::custom(CustomCrs { proj4, label, wkt: Some(compact(&bare)), ..CustomCrs::default() })
                        .map_err(|e| if e.starts_with("too many") { e } else { unread() })
                }
                None => Err(refused(&e).unwrap_or_else(unread)),
            }
        }
    }
}

/// Where the keyword `key` (in capitals, with its bracket) first is in WKT `t` without space between
/// keywords and brackets ([`compact`]), in any case, as the parser reads keywords.
fn find_key(t: &str, key: &str) -> Option<usize> {
    t.to_ascii_uppercase().find(key)
}

/// WKT `text` without the space outside its quoted names (line breaks of a pretty-printed `.prj`),
/// as it is kept: two files of one system that differ only so are the same system.
fn compact(text: &str) -> String {
    let mut quoted = false;
    text.chars()
        .filter(|c| {
            if *c == '"' {
                quoted = !quoted;
            }
            quoted || !c.is_whitespace()
        })
        .collect()
}

/// The end of the bracketed node starting at `at` (the index just past its closing bracket),
/// brackets inside quotes left out.
fn node_end(text: &str, at: usize) -> Option<usize> {
    let (mut depth, mut quoted) = (0usize, false);
    for (i, c) in text[at..].char_indices() {
        match c {
            '"' => quoted = !quoted,
            '[' | '(' if !quoted => depth += 1,
            ']' | ')' if !quoted => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The source of a WKT 2 `BOUNDCRS` whose transformation isn't a Helmert one (a grid file,
/// geographic offsets) or that has none, or `None` for any other text. RustGIS read these as their
/// source before, saying the transformation isn't applied; terrio-crs and PROJ 9.8.1 refuse them.
/// A transformation counts as a Helmert one when a parameter of it is an axis translation. A
/// Helmert one terrio-crs refuses (time-specific, without its epoch) is read as the coordinate
/// frame shift instead ([`time_specific_as_frame`]).
fn unapplied_bound_source(text: &str) -> Option<String> {
    let t = text.trim_start();
    if !t.get(..9).is_some_and(|k| k.eq_ignore_ascii_case("BOUNDCRS[")) {
        return None;
    }
    if let Some(transformation) = find_key(t, "ABRIDGEDTRANSFORMATION[") {
        let end = node_end(t, transformation)?;
        if t[transformation..end].to_ascii_lowercase().contains("axis translation") {
            return None;
        }
    }
    let source = find_key(t, "SOURCECRS[")?;
    let source_end = node_end(t, source)?;
    Some(t[source + "SOURCECRS[".len()..source_end - 1].to_string())
}

/// A WKT 2 `BOUNDCRS` whose transformation is EPSG's time-specific coordinate frame rotation
/// (1066) with its method written as the coordinate frame rotation (9607), or `None` for any other
/// text. Without its epoch PROJ 9.8.1 and terrio-crs refuse it ("Input is not a transformation");
/// RustGIS read it before as the coordinate frame shift, the epoch left out.
fn time_specific_as_frame(text: &str) -> Option<String> {
    bound_method_as_frame(text, &[1066])
}

/// A WKT 2 `BOUNDCRS` whose transformation is one of the EPSG `methods`, with its method written as
/// the coordinate frame rotation (9607), or `None` for any other text.
fn bound_method_as_frame(text: &str, methods: &[u32]) -> Option<String> {
    let t = text.trim_start();
    if !t.get(..9).is_some_and(|k| k.eq_ignore_ascii_case("BOUNDCRS[")) {
        return None;
    }
    let transformation = find_key(t, "ABRIDGEDTRANSFORMATION[")?;
    let method = transformation + find_key(&t[transformation..], "METHOD[")?;
    let end = node_end(t, method)?;
    let compact: String = t[method..end].chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
    methods
        .iter()
        .any(|m| compact.ends_with(&format!(r#"ID["EPSG",{m}]]"#)))
        .then(|| format!(r#"{}METHOD["Coordinate Frame rotation (geog2D domain)",ID["EPSG",9607]]{}"#, &t[..method], &t[end..]))
}

/// A `BOUNDCRS` as RustGIS read it before, where that differs from PROJ 9.8.1's reading: without
/// a `TOWGS84` of its WKT1 source (the bound transformation replaced it), and with a position
/// vector or coordinate frame shift's missing parameters as 0. `None` for any other text.
fn bound_as_read_before(text: &str) -> Option<String> {
    let t = text.trim_start();
    if !t.get(..9).is_some_and(|k| k.eq_ignore_ascii_case("BOUNDCRS[")) {
        return None;
    }
    // The source's own TOWGS84 nodes, each with the comma before it.
    let (mut out, _) = without_nodes(t, "TOWGS84[")?;
    let transformation = out.to_ascii_uppercase().find("ABRIDGEDTRANSFORMATION[")?;
    let end = node_end(&out, transformation)?;
    let upper: String = out[transformation..end].chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
    let helmert = [9606, 9607, 1032, 1033, 1037, 1038].iter().any(|m| upper.contains(&format!(r#"ID["EPSG",{m}]"#)))
        || upper.contains("POSITION_VECTOR")
        || upper.contains("POSITIONVECTOR")
        || upper.contains("COORDINATEFRAME")
        || upper.contains("COORDINATE_FRAME");
    if helmert {
        let params = [
            (8605, "X-axis translation", ""),
            (8606, "Y-axis translation", ""),
            (8607, "Z-axis translation", ""),
            (8608, "X-axis rotation", ""),
            (8609, "Y-axis rotation", ""),
            (8610, "Z-axis rotation", ""),
            (8611, "Scale difference", r#",SCALEUNIT["parts per million",1E-06]"#),
        ];
        let missing: String = params
            .iter()
            .filter(|(code, name, _)| {
                let name = name.to_ascii_uppercase().replace(' ', "");
                !upper.contains(&format!(r#"ID["EPSG",{code}]"#)) && !upper.contains(&name) && !upper.contains(&name.replace('-', "_"))
            })
            .map(|(code, name, unit)| format!(r#",PARAMETER["{name}",0{unit},ID["EPSG",{code}]]"#))
            .collect();
        out.insert_str(end - 1, &missing);
    }
    (out != t).then_some(out)
}

/// The PROJ string RustGIS wrote before for a system PROJ 9.8.1 writes none for, for older
/// releases, which read a saved system by it; `None` for any other. Newer ones read its WKT.
/// - Transverse Mercator (South Orientated) with a false easting or northing (EPSG 9808): the
///   method with `+axis=wsu` and the offsets negated, as RustGIS read it (it places points as
///   the WKT does).
/// - A `BOUNDCRS` by a coordinate frame method PROJ writes no `+towgs84` for (full matrix,
///   time-dependent geog2D or geocentric, or time-specific; 1058, geog3D, RustGIS didn't read
///   before either): its parameters as the coordinate frame rotation's, as
///   RustGIS read them before (the full matrix's small-angle form).
/// - A `BOUNDCRS` as RustGIS read it before ([`bound_as_read_before`]): its own transformation, not
///   its WKT1 source's `TOWGS84`, and a Helmert shift's missing parameters as 0.
pub(crate) fn legacy_proj4(text: &str) -> Option<String> {
    let text = &compact(text);
    let proj4 = |w: &str| terrio_crs::SpatialRef::from_wkt(w).to_proj4().map(|d| d.replace(" +type=crs", ""));
    let bound = bound_as_read_before(text);
    let text = bound.as_deref().unwrap_or(text);
    if let Some(frame) = bound_method_as_frame(text, &[1056, 1057, 1066, 1132, 1133, 1140, 1149]) {
        return proj4(&frame);
    }
    if bound.is_some() {
        return proj4(text);
    }
    let upper = text.to_ascii_uppercase();
    if !upper.contains("SOUTH_ORIENTATED") && !upper.contains("SOUTH ORIENTATED") && !upper.contains(r#"ID["EPSG",9808]"#) {
        return None;
    }
    // Each offset's number with its own unit's factor (a WKT2 `LENGTHUNIT`; none: the system's),
    // and the text with it set to 0.
    let mut zeroed = text.to_string();
    let mut offsets = [(0.0, None); 2];
    for (i, names) in [["FALSE_EASTING", "FALSE EASTING"], ["FALSE_NORTHING", "FALSE NORTHING"]].iter().enumerate() {
        let Some(at) = names.iter().find_map(|n| zeroed.to_ascii_uppercase().find(&format!(r#"PARAMETER["{n}","#))) else { continue };
        let start = at + zeroed[at..].find(',')? + 1;
        let end = start + zeroed[start..].find([',', ']'])?;
        let value: f64 = zeroed[start..end].trim().parse().ok()?;
        let rest = zeroed[end..].trim_start_matches(',').trim_start();
        let own = match rest.get(..11).is_some_and(|k| k.eq_ignore_ascii_case("LENGTHUNIT[")) {
            true => {
                let n = rest.find(',')? + 1;
                let m = n + rest[n..].find([',', ']'])?;
                Some(rest[n..m].trim().parse::<f64>().ok().filter(|f| f.is_finite() && *f > 0.0)?)
            }
            false => None,
        };
        offsets[i] = (value, own);
        zeroed.replace_range(start..end, "0");
    }
    let def = proj4(&zeroed)?;
    if !def.contains("+axis=wsu") {
        return None;
    }
    // PROJ takes `+x_0` and `+y_0` in metres, whatever `+units` says.
    let words: Vec<&str> = def.split_whitespace().collect();
    let to_m = match words.iter().find_map(|t| t.strip_prefix("+to_meter=")) {
        Some(f) => f.parse::<f64>().ok().filter(|f| f.is_finite() && *f > 0.0)?,
        None => match words.iter().find_map(|t| t.strip_prefix("+units=")) {
            None | Some("m") => 1.0,
            Some("km") => 1000.0,
            Some("ft") => 0.3048,
            Some("us-ft") => 1200.0 / 3937.0,
            Some(_) => return None,
        },
    };
    let offsets = offsets.map(|(v, own)| v * own.unwrap_or(to_m));
    let set = |def: String, key: &str, v: f64| {
        let v = 0.0 - v;
        let kept: Vec<&str> = def.split_whitespace().filter(|t| !t.starts_with(key)).collect();
        format!("{} {key}{v}", kept.join(" "))
    };
    Some(set(set(def, "+x_0=", offsets[0]), "+y_0=", offsets[1]))
}

/// WKT `text` with a WGS 84 datum in its geographic system, where that has none, or `None` when
/// it has one or there is none. PROJ 9.8.1 refuses a system without a datum ("Missing DATUM or
/// ENSEMBLE node"); RustGIS read it before on WGS 84, which [`read_tree`] says. A projected one,
/// and a geographic one only where `geographic` (in degrees: in another unit RustGIS refused it).
fn with_wgs84_datum(text: &str, geographic: bool) -> Option<String> {
    let root = parse(text).ok()?;
    let projected = ["PROJCS", "PROJCRS", "PROJECTEDCRS"].iter().any(|k| root.name.eq_ignore_ascii_case(k));
    if !projected && !geographic {
        return None;
    }
    // The geographic system's keyword, as the file writes it, WKT 1 or 2.
    let keywords = ["BASEGEOGCRS", "BASEGEODCRS", "GEOGCRS", "GEODCRS", "GEOGCS", "GEOGRAPHICCRS", "GEODETICCRS"];
    let geog = find_node(&root, &keywords, 0)?;
    if geog.child_of(&["DATUM", "GEODETICDATUM", "TRF", "ENSEMBLE"]).is_some() {
        return None;
    }
    let wkt2 = !geog.name.eq_ignore_ascii_case("GEOGCS");
    let upper = text.to_ascii_uppercase();
    let at = keywords.iter().filter_map(|k| upper.find(&format!("{k}["))).min()?;
    // Past its name: the first comma outside quotes after the bracket.
    let open = at + text[at..].find('[')? + 1;
    let mut quoted = false;
    let comma = open
        + text[open..]
            .char_indices()
            .find(|(_, c)| {
                if *c == '"' {
                    quoted = !quoted;
                }
                *c == ',' && !quoted
            })?
            .0;
    let datum = if wkt2 {
        r#"DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],"#
    } else {
        r#"DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],"#
    };
    Some(format!("{}{datum}{}", &text[..=comma], &text[comma + 1..]))
}

/// The semi-major axis of WKT `text`'s ellipsoid (its first `SPHEROID` or `ELLIPSOID`), in the
/// unit the file gives it.
pub(crate) fn semi_major(text: &str) -> Option<f64> {
    find_node(&parse(text).ok()?, &["SPHEROID", "ELLIPSOID"], 0)?.num(0)
}

/// The first node named one of `names`, depth first.
fn find_node<'a>(n: &'a Node, names: &[&str], depth: usize) -> Option<&'a Node> {
    if names.iter().any(|k| n.name.eq_ignore_ascii_case(k)) {
        return Some(n);
    }
    if depth > MAX_DEPTH {
        return None;
    }
    n.nodes().find_map(|c| find_node(c, names, depth + 1))
}

/// `text` without its `EXTENSION["PROJ4",…]` parts, or `None` if it has none.
fn without_proj4_extension(text: &str) -> Option<String> {
    without_nodes(text, r#"EXTENSION["PROJ4""#).and_then(|(out, found)| found.then_some(out))
}

/// `text` without the nodes that start with `key` (upper case), each with the comma before it,
/// and whether there were any: one pass over the text, however many there are. `None` when one
/// isn't closed.
fn without_nodes(text: &str, key: &str) -> Option<(String, bool)> {
    // ASCII upper case keeps every byte where it is.
    let upper = text.to_ascii_uppercase();
    let (mut out, mut from, mut found) = (String::with_capacity(text.len()), 0, false);
    while let Some(at) = upper[from..].find(key).map(|i| from + i) {
        // Its end: the bracket that closes it, outside quotes.
        let end = node_end(text, at)?;
        out.push_str(&text[from..at]);
        let kept = out.trim_end().len();
        if out[..kept].ends_with(',') {
            out.truncate(kept - 1);
        }
        (from, found) = (end, true);
    }
    out.push_str(&text[from..]);
    Some((out, found))
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

/// Why a projected system is refused rather than read, where its WKT can't say what it means or
/// the projection degenerates; `None` to read it ([`terrio_custom`]).
fn refusal(p: &Node, method: &str) -> Option<String> {
    let params = parameters(p);
    let get = |names: &[&str]| parameter(&params, names);
    let projection = || clip(p.child("PROJECTION").map_or("", Node::title));
    // Whether the earth the definition gets is a sphere; Esri names its datums `D_…`.
    let sphere = earth_params(&format!("+proj=longlat {}", earth(p.child("GEOGCS")))).is_some_and(|e| e.a == e.b);
    let esri = p
        .child("GEOGCS")
        .and_then(|g| g.child("DATUM"))
        .is_some_and(|d| d.title().trim().get(..2).is_some_and(|p| p.eq_ignore_ascii_case("D_")));
    let swiss = || {
        // CH1903 and CH1903+, also on the Bern meridian (EPSG:4801): by the datum's shift, its
        // EPSG code or its name.
        let datum = p.child("GEOGCS").and_then(|g| g.child("DATUM"));
        national_datum(datum).is_some_and(|d| Some(d) == epsg_proj4(4149) || Some(d) == epsg_proj4(4150))
            || datum.and_then(|d| d.child("AUTHORITY")).and_then(authority_code).is_some_and(|c| [6149, 6150, 6801].contains(&c))
            || datum.is_some_and(|d| {
                let name = d.title().trim().to_ascii_uppercase();
                name.strip_prefix("D_").unwrap_or(&name).starts_with("CH1903")
            })
    };
    match method {
        // An azimuth of 90° (or 270°, the same east-west centre line) isn't oblique, and the
        // omerc formula is degenerate there in terrio-crs and PROJ alike; exactly 90° on a Swiss
        // datum is the Swiss projection, which is read.
        "HOTINE_OBLIQUE_MERCATOR_AZIMUTH_CENTER" | "HOTINE_OBLIQUE_MERCATOR" | "HOTINE_OBLIQUE_MERCATOR_AZIMUTH_NATURAL_ORIGIN"
            if get(&["AZIMUTH"]).is_some_and(|a| (a.rem_euclid(180.0) - 90.0).abs() < 1e-10)
                && !(swiss() && get(&["AZIMUTH"]).is_some_and(|a| (a - 90.0).abs() < 1e-10)) =>
        {
            Some("a Hotine projection with its azimuth at 90° (or 270°) isn't supported".into())
        }
        // EPSG's spherical forms (Mercator 1026, equidistant cylindrical 1029 and 9823) and Esri's
        // Orthographic are spherical: on a sphere the same as the ellipsoidal methods, on an
        // ellipsoid not, and which one a file means can't be told, so refused rather than guessed.
        "MERCATOR_SPHERICAL" if !sphere => Some("the Mercator (Spherical) projection on an ellipsoid isn't supported".into()),
        "EQUIDISTANT_CYLINDRICAL_SPHERICAL" if !sphere => {
            Some("the Equidistant Cylindrical (Spherical) projection on an ellipsoid isn't supported".into())
        }
        "ORTHOGRAPHIC" | "ORTHOGRAPHIC_SPHERICAL" if !sphere && (esri || method == "ORTHOGRAPHIC_SPHERICAL") => {
            Some("the Orthographic projection as Esri defines it (on a sphere) isn't supported on an ellipsoid".into())
        }
        // A parameter that isn't a finite number (`1e400`) is refused rather than taken as not
        // given, as for Web Mercator: a parallel given so would be read as 0. None of the three has
        // a scale factor, and PROJ ignores one: other than 1 it is refused (`crs::transformer_of`),
        // rather than read without it.
        "NATURAL_EARTH" | "ECKERT_IV" | "WINKEL_TRIPEL"
            if p.nodes().any(|n| n.name.eq_ignore_ascii_case("PARAMETER") && n.num(0).is_none()) =>
        {
            Some(format!("the {} projection with a parameter that isn't a finite number isn't supported", projection()))
        }
        "NATURAL_EARTH" | "ECKERT_IV" | "WINKEL_TRIPEL" if get(&SCALE_FACTOR).is_some_and(|k| k != 1.0) => {
            Some(format!("the {} projection with a scale factor other than 1 isn't supported", projection()))
        }
        // Its standard parallel must be short of a pole, as in a definition (`crs::transformer_of`).
        "WINKEL_TRIPEL" if get(&["STANDARD_PARALLEL_1"]).is_some_and(|v| v.abs() >= 90.0) => {
            Some(format!("the {} projection with its standard parallel at a pole isn't supported", projection()))
        }
        // Krovak fixes its cone's azimuth and pseudo standard parallel (PROJ 9.8.1 refuses other
        // values), and Esri's parameters its axes' scales and the plane's rotation (PROJ ignores
        // other values, which would place points as if they weren't given): refused.
        "KROVAK"
            if get(&["AZIMUTH"]).is_some_and(|v| (v - 30.288_139_752_777_78).abs() > 1e-6)
                || get(&["PSEUDO_STANDARD_PARALLEL_1"]).is_some_and(|v| (v - 78.5).abs() > 1e-10)
                || get(&["X_SCALE"]).is_some_and(|v| v != -1.0)
                || get(&["Y_SCALE"]).is_some_and(|v| v != 1.0)
                || get(&["XY_PLANE_ROTATION"]).is_some_and(|v| (v - 90.0).abs() > 1e-10) =>
        {
            Some(format!("the {} projection with axes scaled or rotated otherwise than Krovak's isn't supported", projection()))
        }
        // South Orientated counts westings and southings: axes that say otherwise contradict the
        // method, and which one is meant can't be told.
        "TRANSVERSE_MERCATOR_SOUTH_ORIENTATED"
            if !matches!(axis_flip(p), Ok(Some("wsu"))) && p.nodes().any(|a| a.name.eq_ignore_ascii_case("AXIS")) =>
        {
            Some("Transverse Mercator (South Orientated) with axes other than west and south isn't supported".into())
        }
        _ => axis_flip(p).err(),
    }
}

/// Whether WKT `text` names its datum as WGS 84, or one read as it ([`wgs84_like`]).
pub(crate) fn names_wgs84_datum(text: &str) -> bool {
    tree(text).is_ok_and(|root| wgs84_like(geogcs(&root).and_then(|g| g.child("DATUM"))).is_some())
}

/// The centre of the area of use of the EPSG geographic system a WKT's datum is, by the
/// geographic system's own code or the datum's Esri name ([`esri_datum_code`]): a place where a
/// transformation of a datum covering an island only (Grenada 1953's) holds.
pub(crate) fn datum_area_centre(text: &str) -> Option<rustgis_geom::Coord> {
    let root = tree(text).ok()?;
    let geog = geogcs(&root)?;
    let by_code = geog.child("AUTHORITY").filter(|a| a.title().eq_ignore_ascii_case("EPSG")).and_then(authority_code);
    let by_name = || esri_datum_code(geog.child("DATUM")?, Some(geog));
    let code = u16::try_from(by_code.or_else(by_name)?).ok()?;
    super::crs::area_centre(code)
}

/// The PROJ datum for WGS 84 and the datums that differ from it by a metre or so.
fn wgs84_like(datum: Option<&Node>) -> Option<&'static str> {
    // (WKT2 names a datum ensemble: `World Geodetic System 1984 ensemble`.)
    match norm(datum.map_or("", Node::title)).trim_start_matches("D_").trim_end_matches("_ENSEMBLE") {
        "WGS_1984" | "WGS84" | "WORLD_GEODETIC_SYSTEM_1984" => Some("+datum=WGS84"),
        // New Italian aliases require the published ellipsoid. GRS80 and the WGS84
        // ellipsoid in older IGM95 definitions differ by less than a millimetre.
        // MILITAIRE is the legacy spelling in older EPSG:3064/4670 WKT (PostGIS's table).
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
        // TWD97, Taiwan's datum (EPSG:3824), including the 2020 realization the Ministry of the
        // Interior's own data uses (`GCS_TWD97[2020]` on `D_TWD_1997`): EPSG's TWD97 to WGS 84
        // (1) (EPSG:3830) is null too, to about 1 m. On GRS 1980 only, like MAGNA below.
        "TWD_1997" | "TWD97" | "TAIWAN_DATUM_1997" => datum
            .filter(|d| ellipsoid_matches(d, "+proj=longlat +ellps=GRS80"))
            .map(|_| "+ellps=GRS80 +towgs84=0,0,0"),
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
/// the file supplies an EPSG code; regional grid transformations are not inferred. Datums not
/// named here are looked up by their Esri name ([`esri_datum_code`]).
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
        _ => u16::try_from(esri_datum_code(datum, None)?).ok()?,
    };
    let def = epsg_proj4(code)?;
    ellipsoid_matches(datum, def).then_some(def)
}

/// The EPSG geographic system a datum is, by its name, as terrio-crs identifies it as PROJ does
/// (an Esri name such as `D_Philippine_Reference_System_1992`, with its own ellipsoid): `None` for
/// a name it doesn't know. With its geographic system `geog`, that system as the file gives it (its
/// name, prime meridian and unit: Bern 1898 (Bern) is EPSG:4801 on the Bern meridian only); else the
/// datum on Greenwich in degrees, under the name Esri writes for it (`D_` as `GCS_`), as PROJ finds a
/// few by that name only (`GCS_Greenland_1996`, whose Esri datum name alone isn't known).
fn esri_datum_code(datum: &Node, geog: Option<&Node>) -> Option<u32> {
    fn write(n: &Node, out: &mut String) {
        out.push_str(&n.name);
        out.push('[');
        for (i, item) in n.items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            match item {
                Item::Node(n) => write(n, out),
                Item::Text(t) => {
                    out.push('"');
                    out.push_str(&t.replace('"', "\"\""));
                    out.push('"');
                }
                Item::Num(v) => out.push_str(&v.to_string()),
                Item::Word(w) => out.push_str(w),
            }
        }
        out.push(']');
    }
    let mut wkt = String::new();
    match geog {
        Some(g) => write(g, &mut wkt),
        None => {
            let name = format!("GCS_{}", datum.title().strip_prefix("D_").unwrap_or(datum.title()));
            wkt = format!(r#"GEOGCS["{}","#, name.replace('"', "\"\""));
            write(datum, &mut wkt);
            wkt.push_str(r#",PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#);
        }
    }
    terrio_crs::SpatialRef::from_wkt(&wkt).geographic_code()
}

/// Whether the file's ellipsoid axes agree with a known definition within a centimetre.
///
/// When the definition names one of PROJ's [`ELLIPSOIDS`], the file may also give that ellipsoid's
/// EPSG flattening. They differ for Clarke 1880 (RGS) only: PROJ's `clrk80` has a minor axis 10 cm
/// longer than the one ESRI WKT writes for Arc 1960, Minna, Adindan and the other datums on it.
fn ellipsoid_matches(datum: &Node, def: &str) -> bool {
    let Some(proj) = earth_params(def) else { return false };
    let Some(sph) = datum.child_of(&["SPHEROID", "ELLIPSOID"]) else { return false };
    let (Some(actual_a), Some(rf)) = (sph.num(0), sph.num(1)) else { return false };
    let actual_b = if rf > 0.0 { actual_a * (1.0 - 1.0 / rf) } else { actual_a };
    let agrees = |b: f64| (actual_a - proj.a).abs() < 0.01 && (actual_b - b).abs() < 0.01;
    let ellps = def.split_whitespace().find_map(|t| t.strip_prefix("+ellps="));
    let epsg_rf = ELLIPSOIDS.iter().find(|e| Some(e.0) == ellps).map(|e| e.2).filter(|rf| *rf > 0.0);
    agrees(proj.b) || epsg_rf.is_some_and(|rf| agrees(proj.a * (1.0 - 1.0 / rf)))
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

/// The name of the mark [`bound`] leaves for a transformation it can't apply ([`put_mark`]): its
/// title names it (`NTv2 transformation to WGS 84 (ntv2_0.gsb)`). [`Parser`] reads names of
/// letters, digits and `_` only, so no file can give one; the tree is never written out.
const UNAPPLIED: &str = "rustgis:unapplied";

/// The name of the mark [`bound`] leaves for a bound system without a transformation: its title
/// names the target. No file can give one either ([`UNAPPLIED`]).
const UNTRANSFORMED: &str = "rustgis:untransformed";

/// The datum of a WKT1 system (its own `GEOGCS`'s, or its base one's), where [`bound`] puts the
/// shift or says why there is none.
fn source_datum(crs: &mut Node) -> Option<&mut Node> {
    let geog = if crs.name.eq_ignore_ascii_case("GEOGCS") { Some(crs) } else { crs.child_mut("GEOGCS") };
    geog.and_then(|g| g.child_mut("DATUM"))
}

/// Leaves the mark `name` titled `value` on a bound system's source: on its datum, or on the
/// source itself when it has none, which is read as before without a datum.
fn put_mark(crs: &mut Node, name: &str, value: &str) {
    let mark = Item::Node(Node::new(name, value, []));
    match source_datum(crs) {
        Some(datum) => datum.items.push(mark),
        None => crs.items.push(mark),
    }
}

/// The title of the mark `name` [`put_mark`] left among `node`'s children.
fn mark(node: Option<&Node>, name: &str) -> Option<String> {
    node?.nodes().find(|n| n.name == name).map(|n| n.title().to_string())
}

/// The target a bound system without a transformation names ([`UNTRANSFORMED`]), on its datum.
fn untransformed(datum: Option<&Node>) -> Option<String> {
    mark(datum, UNTRANSFORMED)
}

/// At most 120 characters of `s`, for a notice: the names in it come from the file. A notice
/// over 16 KiB isn't shown at all (`diagnostics::redact`), so a name left whole could hide it.
pub(crate) fn clip(s: &str) -> String {
    let mut out: String = s.chars().take(120).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// The shift to WGS 84 that a datum's file gives but RustGIS can't apply, named: a bound
/// system's transformation [`bound`] left on it ([`UNAPPLIED`]), or GDAL's WKT1 grid
/// (`EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"]`), whose `@null` is no shift. PROJ applies a grid only
/// with its file, and refuses the system without it; RustGIS reads the datum without it.
fn unapplied(datum: Option<&Node>) -> Option<String> {
    mark(datum, UNAPPLIED).or_else(|| {
        datum?.nodes().filter(|n| n.name.eq_ignore_ascii_case("EXTENSION") && n.title().eq_ignore_ascii_case("PROJ4_GRIDS")).find_map(|e| {
            let Some(Item::Text(value)) = e.items.get(1) else { return None };
            // Named without "to WGS 84": another body's datum can carry one too.
            Some(format!("grid shift ({})", grid_names(value)?))
        })
    })
}

/// A note when the datum (of a `GEOGCS`) is neither WGS 84-like nor shifted to it by `TOWGS84`.
/// A datum whose file gives a shift that can't be applied has [`read_wkt`] say that instead.
fn datum_note(geog: Option<&Node>) -> Option<String> {
    let datum = geog?.child("DATUM")?;
    if wgs84_like(Some(datum)).is_some()
        || towgs84(Some(datum)).is_some()
        || national_datum(Some(datum)).is_some()
        || unapplied(Some(datum)).is_some()
    {
        return None;
    }
    Some(format!(
        "the datum {} has no conversion to WGS 84 in the file, so positions may be off by up to a few hundred metres",
        clip(datum.title())
    ))
}

/// [`datum_note`] for a system terrio-crs finds no transformation of: said unless the file names
/// WGS 84 or gives a shift (applied, or said not to be by [`unapplied_note`]).
fn unshifted_datum_note(geog: Option<&Node>) -> Option<String> {
    let datum = geog?.child("DATUM")?;
    if wgs84_like(Some(datum)).is_some() || towgs84(Some(datum)).is_some() || unapplied(Some(datum)).is_some() {
        return None;
    }
    Some(format!(
        "the datum {} has no conversion to WGS 84 in the file, so positions may be off by up to a few hundred metres",
        clip(datum.title())
    ))
}

/// The datum of a `GEOGCS`, by name for the common ones, else by its spheroid's axes; and its
/// prime meridian.
fn earth(geog: Option<&Node>) -> String {
    let datum = geog.and_then(|g| g.child("DATUM"));
    let known = wgs84_like(datum).map(str::to_string).or_else(|| {
        // Some definitions give their ellipsoid by its axes (NTF, Kalianpur, Arc 1950 …).
        national_datum(datum).map(|def| {
            def.split_whitespace()
                .filter(|p| ["+ellps=", "+a=", "+b=", "+rf=", "+datum=", "+towgs84="].iter().any(|k| p.starts_with(k)))
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
    let proj = earth_params(def)?;
    let value = |k: &str| def.split_whitespace().find_map(|t| t.strip_prefix(k));
    let shift = value("+towgs84=").or_else(|| DATUMS.iter().find(|r| value("+datum=").is_some_and(|d| r.0.eq_ignore_ascii_case(d)))?.3)?;
    let parts: Vec<f64> = shift.split(',').map(|v| v.trim().parse().ok()).collect::<Option<_>>()?;
    if parts.len() != 3 && parts.len() != 7 {
        return None;
    }
    let mut seven = [0.0; 7];
    seven.iter_mut().zip(parts).for_each(|(s, p)| *s = p);
    Some(((proj.a, proj.b), seven))
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

/// The radius of the sphere with the area of the ellipsoid of axes `a` and `b` (PROJ's `+R_A`).
fn authalic_radius(a: f64, b: f64) -> f64 {
    let e2 = 1.0 - (b / a).powi(2);
    let e = e2.sqrt();
    if e < 1e-12 {
        return a;
    }
    let q = (1.0 - e2) * (1.0 / (1.0 - e2) - ((1.0 - e) / (1.0 + e)).ln() / (2.0 * e));
    a * (q / 2.0).sqrt()
}

/// A WKT text value, its quotes doubled.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A number as WKT writes it: no `.0` on a whole number, no exponent for ordinary values.
fn num(v: f64) -> String {
    if v == 0.0 { "0".into() } else { format!("{v}") }
}

/// Whether [`to_wkt`] writes a custom system as the WKT it was read from (names and all), not as
/// RustGIS's own text, which gives another body's datum only by its ellipsoid's axes.
pub fn writes_its_own_wkt(crs: Crs) -> bool {
    crs.custom_def().and_then(|d| d.wkt.as_deref().map(terrio_crs::SpatialRef::from_wkt)).and_then(|sr| sr.to_wkt1_strict()).is_some()
}

/// A coordinate system as WKT1 (OGC, the form `.prj` files, GeoPackages and FlatGeobuf hold),
/// built from its PROJ definition, with its EPSG code as the authority. `None` for a projection
/// it can't describe (one WKT1 has no name for).
pub fn to_wkt(crs: Crs) -> Option<String> {
    // terrio-crs's WKT1 that needs no PROJ4 extension (`to_wkt1_strict`), as PROJ 9.8.1 and GDAL
    // write `.prj` files: an EPSG code's from its catalog, a system read from WKT from that WKT.
    // Else RustGIS's own, from the PROJ definition (a map's preset, a code the catalog gives
    // another's definition).
    let strict = |sr: Option<terrio_crs::SpatialRef>| sr.and_then(|sr| sr.to_wkt1_strict());
    let (def, name, code) = match crs {
        Crs::LonLat => return Some(WGS84_GEOGCS.into()),
        Crs::WebMercator => return Some(WEB_MERCATOR.into()),
        Crs::Epsg(c) => {
            if super::crs::catalog_has_own(c) {
                if let Some(wkt) = strict(terrio_crs::SpatialRef::from_epsg(u32::from(c)).ok()) {
                    return Some(wkt);
                }
                // A code WKT 1 can't hold (Lambert Conic Conformal (2SP Michigan), with its
                // ellipsoid scaling): GDAL writes no `.prj`, and RustGIS's own text would be
                // another system, 74 km off for EPSG:6200. Only text that is read back as the code
                // and places points as it does (by its parameters, not its authority alone) is
                // written: a geographic 3D system's (TWD97, EPSG:3823) as its 2D one.
                let def = epsg_proj4(c)?;
                let wkt = def_wkt(def, &format!("EPSG:{c}"), Some(c))?;
                if crs_from_wkt(&wkt) != Ok(crs) {
                    return None;
                }
                let unnamed = def_wkt(def, &format!("EPSG:{c}"), None)?;
                return super::crs::places_as_wkt(def, &unnamed).then_some(wkt);
            }
            // RustGIS's own text, where it is read back as the code. Else, for a compound code
            // whose own text carries an older shift (EPSG:7410's PSD93, 0.35 m off PROJ 9.8.1),
            // the `COMPD_CS` GDAL writes, which places points as the code does.
            let own = def_wkt(epsg_proj4(c)?, &format!("EPSG:{c}"), Some(c));
            if own.as_ref().is_some_and(|w| crs_from_wkt(w) == Ok(crs)) {
                return own;
            }
            return strict(terrio_crs::SpatialRef::from_epsg(u32::from(c)).ok()).filter(|w| w.starts_with("COMPD_CS[")).or(own);
        }
        Crs::Custom(_) => {
            let d = crs.custom_def()?;
            if let Some(wkt) = strict(d.wkt.as_deref().map(terrio_crs::SpatialRef::from_wkt)) {
                return Some(wkt);
            }
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
    let proj = earth_params(def)?;
    let params: Vec<(&str, Option<&str>)> = def
        .split_whitespace()
        .filter_map(|t| t.strip_prefix('+'))
        .map(|t| t.split_once('=').map_or((t, None), |(k, v)| (k, Some(v))))
        .collect();
    let text = |k: &str| params.iter().find(|(n, _)| *n == k).and_then(|(_, v)| *v);
    let flag = |k: &str| params.iter().any(|(n, _)| *n == k);
    // Angles as they are read ([`crate::raster::crs::angle_degrees`]): `50d27'35"` is no 0.
    let get = |k: &str| {
        text(k)
            .and_then(|v| if crate::raster::crs::angular(k, v) { crate::raster::crs::angle_degrees(v) } else { v.parse::<f64>().ok() })
            .filter(|v| v.is_finite())
    };
    let authority = |c: Option<u16>| c.map(|c| format!(",AUTHORITY[\"EPSG\",\"{c}\"]")).unwrap_or_default();

    // The datum and its ellipsoid.
    let datum = text("datum").and_then(|d| DATUMS.iter().find(|r| r.0.eq_ignore_ascii_case(d)));
    let ellps = text("ellps").or(datum.map(|d| d.2));
    let known = ellps.and_then(|e| ELLIPSOIDS.iter().find(|r| r.0.eq_ignore_ascii_case(e)));
    // The figure the projection is computed on, as PROJ 9.8.1 computes it, which isn't always
    // the ellipsoid terrio-crs reports for the definition: `+f`, `+es` or `+e` without `+a`,
    // `+R`, `+rf` or `+b` reshape the named ellipsoid (or the datum's), and `+R_A`, switched on,
    // turns an ellipsoid into the sphere with its area (EPSG:9311's).
    let number = |k: &str| text(k).and_then(|v| v.trim().parse::<f64>().ok());
    let reshaped = ["f", "es", "e"].iter().any(|k| flag(k)) && !["a", "R", "rf", "b"].iter().any(|k| flag(k));
    let reported_es = 1.0 - (proj.b / proj.a).powi(2);
    // Reshaped to another shape than the reported one: `+f` equal to the named ellipsoid's
    // flattening, written in other digits, isn't.
    let es = match (number("f"), number("es"), number("e")) {
        (Some(f), _, _) if reshaped => f * (2.0 - f),
        (None, Some(es), _) if reshaped => es,
        (None, None, Some(e)) if reshaped => e * e,
        _ => reported_es,
    };
    let switched_on = |k: &str| {
        params.iter().find(|(n, _)| *n == k).is_some_and(|(_, v)| {
            v.is_none_or(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "t" | "yes" | "y" | "on" | "1"))
        })
    };
    let reshaped_to_another = (es - reported_es).abs() > 1e-12;
    let authalic = switched_on("R_A") && es != 0.0;
    let (a, b) = if authalic {
        let r = authalic_radius(proj.a, proj.a * (1.0 - es).sqrt());
        (r, r)
    } else if reshaped_to_another {
        (proj.a, proj.a * (1.0 - es).sqrt())
    } else {
        (proj.a, proj.b)
    };
    if !(a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0) {
        return None;
    }
    // A figure other than the named ellipsoid isn't given its name, nor its datum's: a reader
    // would take the named one back. The authalic sphere isn't given EPSG's name either, whose
    // radius is rounded.
    let named_axes = ellps.and_then(|e| earth_params(&format!("+proj=longlat +ellps={e}"))).map(|p| (p.a, p.b));
    let changed = authalic || reshaped_to_another || named_axes.is_some_and(|(na, nb)| (na - a).abs() > 1e-6 || (nb - b).abs() > 1e-6);
    let ellps_name = match known {
        Some(r) if authalic => format!("Authalic sphere of {}", r.1),
        Some(r) if !changed => r.1.to_string(),
        _ if authalic => "Authalic sphere".to_string(),
        _ => "unnamed".to_string(),
    };
    let named_datum = datum.filter(|_| !changed);
    // A named ellipsoid's flattening as published; else from its axes (PROJ keeps a and b).
    let rf = match known {
        Some(k) if !changed && text("rf").is_none() && text("b").is_none() => k.2,
        _ if (a - b).abs() < 1e-9 => 0.0,
        _ => (a / (a - b) * 1e9).round() / 1e9,
    };
    let datum_name = match named_datum {
        Some(d) => d.1.to_string(),
        None if changed => format!("Unknown based on {ellps_name} ellipsoid"),
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
    // A prime meridian by its longitude (`+pm=2.5`) or name (`+pm=paris`, which terrio-crs
    // resolves), in degrees east of Greenwich.
    let pm = match get("pm") {
        Some(pm) => pm,
        None => proj.prime_meridian,
    };
    if !pm.is_finite() {
        return None;
    }
    let pm_name = match text("pm").filter(|_| get("pm").is_none()) {
        Some(p) => p.get(..1).unwrap_or("").to_ascii_uppercase() + p.get(1..).unwrap_or(""),
        None if pm == 0.0 => "Greenwich".to_string(),
        None => "unnamed".to_string(),
    };
    let geog_name = match named_datum {
        Some(d) if d.0 == "WGS84" => "WGS 84".to_string(),
        _ => datum_name.trim_start_matches("Unknown based on ").to_string(),
    };
    let geogcs = |auth: &str| {
        format!(
            "GEOGCS[{},DATUM[{},SPHEROID[{},{},{}]{towgs84}],PRIMEM[{},{}],UNIT[\"degree\",0.0174532925199433]{auth}]",
            quoted(&geog_name),
            quoted(&datum_name),
            quoted(&ellps_name),
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

    // Hyperbolic Cassini-Soldner (EPSG:3139, Vanua Levu Grid) has no WKT 1 name: GDAL writes it
    // as WKT 2 only, and as `Cassini_Soldner` it would be read as the ordinary one.
    if terrio_crs::proj_boolean_parameters(def).is_ok_and(|p| p.iter().any(|(k, on)| k == "hyperbolic" && *on)) {
        return None;
    }
    // The projection and its parameters, in degrees and metres.
    let lat0 = get("lat_0").unwrap_or(0.0);
    let lon0 = get("lon_0").unwrap_or(0.0);
    let k0 = get("k_0").or(get("k")).unwrap_or(1.0);
    let to_m = proj.to_meter;
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
        // OGC's name, which the reader takes as EPSG's ellipsoidal method 9840, as PROJ computes
        // it.
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
        // west, which a `.prj` with these axes would contradict. PROJ fixes the azimuth and
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
        // Its standard parallel is the definition's, else 0, as PROJ takes a definition used as a
        // coordinate system and writes it (the map's preset gives Winkel's own, acos(2/π), about
        // 50.46°, which is also what the `proj` program takes for the bare projection).
        "wintri" => {
            let lat1 = get("lat_1").unwrap_or(0.0);
            ps = vec![("standard_parallel_1", lat1), ("central_meridian", lon0)];
            "Winkel_Tripel"
        }
        // Hotine's oblique Mercator by its azimuth, the general form terrio-crs projects (the
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
    /// WKT1 with EPSG's parameter names, and a `PROJCS` with two `UNIT`s: RustGIS refused the
    /// first ones and read the last by its first unit; each is read where PROJ 9.8.1 places it
    /// (pyproj 3.8.0, `Transformer.from_crs("EPSG:4326", CRS(wkt), always_xy=True)`).
    #[test]
    fn wkt1_with_epsg_parameter_names_or_two_units_is_read_as_proj_reads_it() {
        const WGS84: &str = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#;
        let conic = |method: &str| {
            format!(
                r#"PROJCS["c",{WGS84},PROJECTION["{method}"],PARAMETER["Latitude of false origin",40],PARAMETER["Longitude of false origin",-96],PARAMETER["Latitude of 1st standard parallel",29.5],PARAMETER["Latitude of 2nd standard parallel",45.5],PARAMETER["Easting at false origin",0],PARAMETER["Northing at false origin",0],UNIT["metre",1]]"#
            )
        };
        let cases = [
            (conic("Albers_Conic_Equal_Area"), (-90.0, 35.0), (542_742.271, -543_037.833)),
            (conic("Equidistant_Conic"), (-90.0, 35.0), (542_674.378, -537_687.494)),
            (
                format!(
                    r#"PROJCS["h",{WGS84},PROJECTION["Hotine_Oblique_Mercator_Azimuth_Center"],PARAMETER["Latitude of projection centre",40],PARAMETER["Longitude of projection centre",10],PARAMETER["Azimuth of initial line",30],PARAMETER["Angle from Rectified to Skew Grid",30],PARAMETER["Scale factor on initial line",1],PARAMETER["Easting at projection centre",0],PARAMETER["Northing at projection centre",0],UNIT["metre",1]]"#
                ),
                (11.0, 41.0),
                (84_136.654, 111_520.458),
            ),
            (
                format!(
                    r#"PROJCS["u",{WGS84},PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",9],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1],UNIT["US survey foot",0.304800609601219]]"#
                ),
                (10.0, 45.0),
                (578_815.303, 4_983_436.768),
            ),
        ];
        for (wkt, (lon, lat), (x, y)) in cases {
            let crs = crs_from_wkt(&wkt).unwrap_or_else(|e| panic!("{e}: {wkt}"));
            let p = crs.transformer().unwrap().from_lonlat(Coord::new(lon, lat)).unwrap();
            assert!((p.x - x).abs() < 0.01 && (p.y - y).abs() < 0.01, "{p:?}: {wkt}");
        }
    }

    #[test]
    fn sinusoidal_robinson_orthographic_and_mercator_variant_a() {
        let proj4 = |wkt: &str| crs_from_wkt(wkt).map(|c| c.custom_def().map(|d| d.proj4.clone()).unwrap_or_default());
        let esri = |method: &str, params: &str| format!(r#"PROJCS["x",{GEOG_4326},PROJECTION["{method}"],{params},UNIT["Meter",1.0]]"#);
        let common = r#"PARAMETER["False_Easting",100.0],PARAMETER["False_Northing",-200.0],PARAMETER["Central_Meridian",-75.0]"#;
        // Each is read as PROJ 9.8.1 places it (pyproj 3.8.0: `Transformer.from_crs("EPSG:4326",
        // wkt, always_xy=True).transform(lon, lat)`), and its PROJ string (for older releases)
        // places it alike.
        let reads = |wkt: &str, method: &str, at: (f64, f64), proj: Option<(f64, f64)>| {
            let crs = crs_from_wkt(wkt).unwrap_or_else(|e| panic!("{e}: {wkt}"));
            let def = crs.custom_def().unwrap().proj4.clone();
            assert!(def.starts_with(&format!("+proj={method} ")), "{def}");
            let at = Coord::new(at.0, at.1);
            let p = crs.transformer().unwrap().from_lonlat(at).unwrap();
            let q = crate::raster::crs::transformer_of(&def).unwrap().from_lonlat(at).unwrap();
            assert!((p.x - q.x).hypot(p.y - q.y) < 1e-3, "{def}: {p:?} {q:?}");
            if let Some((x, y)) = proj {
                assert!((p.x - x).hypot(p.y - y) < 1e-3, "{wkt}: {p:?}");
            }
            def
        };
        let at = (-70.0, 10.0);
        reads(&esri("Sinusoidal", common), "sinu", at, Some((548_296.820_340_765_6, 1_105_654.833_234_372)));
        reads(&esri("Robinson", common), "robin", at, Some((470_311.297_040_082_2, 1_069_319.160_993_024_5)));
        let merc = esri("Mercator_Variant_A", &format!(r#"{common},PARAMETER["Scale_Factor",0.997]"#));
        reads(&merc, "merc", at, Some((555_027.661_604_469_5, 1_107_940.677_543_667_6)));
        // Orthographic by its natural origin's names (as GDAL writes Esri WKT for the IAU's), or
        // Esri's centre: on a sphere.
        let sphere = GEOG_4326.replace("D_WGS_1984", "D_Sphere").replace("6378137.0,298.257223563", "6378137.0,0.0");
        for params in [
            r#"PARAMETER["Latitude of natural origin",40.0],PARAMETER["Longitude of natural origin",-100.0]"#,
            r#"PARAMETER["Latitude_Of_Center",40.0],PARAMETER["Longitude_Of_Center",-100.0]"#,
        ] {
            let wkt = esri("Orthographic", params).replace(GEOG_4326, &sphere);
            reads(&wkt, "ortho", (-95.0, 42.0), Some((413_107.719_031_127_2, 234_187.520_825_510_87)));
        }

        // WKT2: EPSG's Orthographic (9840) on an ellipsoid; Esri's names without an ID.
        let wkt2 = |method: &str, params: &str, ellipsoid: &str| {
            format!(
                r#"PROJCRS["x",BASEGEOGCRS["g",DATUM["d",ELLIPSOID["e",{ellipsoid},LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]]],CONVERSION["c",{method},{params}PARAMETER["False easting",0,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1]]]"#
            )
        };
        let origin = r#"PARAMETER["Latitude of natural origin",55,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",5,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],"#;
        let ellipsoid = "6378137,298.257223563";
        reads(
            &wkt2(r#"METHOD["Orthographic",ID["EPSG",9840]]"#, origin, ellipsoid),
            "ortho",
            (6.0, 54.0),
            Some((65_572.444_550_983_14, -110_839.880_416_567_04)),
        );
        reads(
            &wkt2(r#"METHOD["Robinson"]"#, origin, ellipsoid),
            "robin",
            (6.0, 54.0),
            Some((79_541.565_185_456_48, 5_737_067.559_377_016)),
        );
        // Mars's sphere: another body's, read as it is on the Earth.
        reads(&wkt2(r#"METHOD["Sinusoidal"]"#, origin, "3396190,0"), "sinu", (6.0, 54.0), None);
        // Mercator (Spherical), EPSG's 1026, on a sphere; refused on an ellipsoid, as is Esri's
        // (spherical) Orthographic.
        let spherical = r#"METHOD["Mercator (Spherical)",ID["EPSG",1026]]"#;
        reads(&wkt2(spherical, origin, "3396190,0"), "merc", (6.0, 54.0), None);
        let e = proj4(&wkt2(spherical, origin, "6378137,298.257223563")).unwrap_err();
        assert!(e.contains("Mercator (Spherical) projection on an ellipsoid isn't supported"), "{e}");
        // Equidistant Cylindrical (Spherical), EPSG's 1029: the same on a sphere, refused on an
        // ellipsoid rather than computed as the ellipsoidal method 1028.
        let eqc_spherical = r#"METHOD["Equidistant Cylindrical (Spherical)",ID["EPSG",1029]]"#;
        // Its latitude of natural origin (PROJ's `+lat_0`) is kept.
        let p = reads(&wkt2(eqc_spherical, origin, "3396190,0"), "eqc", (6.0, 54.0), None);
        assert!(p.contains("+lat_0=55 "), "{p}");
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
        // Makassar has no shift to WGS 84 in the file, but its datum's name gives one, which PROJ
        // 9.8.1 applies ("Makassar to WGS 84 (1)"; pyproj 3.8.0: `Transformer.from_crs(
        // "EPSG:4326", wkt, always_xy=True).transform(120, -3)`), and nothing is said.
        assert_eq!(note, None);
        let m = crs.transformer().unwrap().from_lonlat(Coord::new(120.0, -3.0)).unwrap();
        assert!((m.x - 5_009_477.817_221_181).abs() < 0.001 && (m.y - 568_973.538_929_259_6).abs() < 0.001, "{m:?}");
        // The projection itself (EPSG's guidance note 7-2), on a datum no one knows a shift of:
        // said, and longitude/latitude on that datum is what goes in.
        let unknown = wkt.replace("D_Makassar", "D_Unknown");
        let (crs, note) = read_wkt(&unknown).unwrap();
        assert!(note.is_some_and(|n| n.contains("D_Unknown")));
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
            // An unnamed ellipsoid by its axes and flattening (Mars's IAU 2015 ellipsoid), and by
            // its axes: the ellipsoid is the one written.
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

    /// A definition on the sphere of its ellipsoid's area (`+R_A`, as EPSG:2163's US National
    /// Atlas Equal Area) is written on that sphere, and read back places points where it does.
    #[test]
    fn an_authalic_sphere_is_written_as_the_sphere() {
        let def = "+proj=laea +R_A +lat_0=45 +lon_0=-100 +x_0=0 +y_0=0 +ellps=clrk66 +units=m +no_defs";
        let wkt = proj4_to_wkt(def, "US National Atlas").unwrap();
        assert!(wkt.contains(r#"SPHEROID["Authalic sphere of Clarke 1866",6370997.24"#) && wkt.contains(",0]"), "{wkt}");
        assert!((authalic_radius(6_378_206.4, 6_356_583.8) - 6_370_997.240_804).abs() < 1e-3);
        let (crs, back) = (Crs::parse(def).unwrap(), crs_from_wkt(&wkt).unwrap());
        let c = Coord::new(-80.0, 30.0);
        let (a, b) = (crs.transformer().unwrap().from_lonlat(c).unwrap(), back.transformer().unwrap().from_lonlat(c).unwrap());
        assert!((a.x - b.x).abs() < 0.001 && (a.y - b.y).abs() < 0.001, "{a:?} {b:?}\n{wkt}");
        // PROJ 9.8.1 (pyproj 3.8.0) for the definition, and for this WKT: `Transformer.from_crs(
        // "EPSG:4326", def, always_xy=True).transform(-80, 30)`. PROJ's own WKT1 for `+R_A` gives
        // the ellipsoid instead, which places points elsewhere.
        assert!((b.x - 1_921_494.528).abs() < 0.001 && (b.y + 1_439_433.375).abs() < 0.001, "{b:?}");
        // Switched off, it is the ellipsoid; with a radius given, the radius.
        for off in ["+R_A=false", "+R_A=0", "+R_A=off"] {
            let off = proj4_to_wkt(&def.replace("+R_A", off), "X").unwrap();
            assert!(off.contains(r#"SPHEROID["Clarke 1866",6378206.4,294.978698213898]"#), "{off}");
        }
        let radius = proj4_to_wkt(&def.replace("+R_A", "+R_A +R=6371000"), "X").unwrap();
        assert!(radius.contains(r#"SPHEROID["unnamed",6371000,0]"#), "{radius}");
        // On a named datum, and on a datum's ellipsoid reshaped to the sphere of its semi-major
        // axis (`+f=0`, Esri's spherical orthographic): each is placed where the definition
        // places it, not on the named datum's ellipsoid. PROJ 9.8.1 (pyproj 3.8.0) places
        // (-80°, 30°) there too.
        for (def, proj) in [
            ("+proj=laea +R_A +lat_0=45 +lon_0=-100 +datum=WGS84 +units=m +no_defs", (1_921_497.526, -1_439_435.621)),
            ("+proj=ortho +lat_0=45 +lon_0=-100 +datum=WGS84 +f=0 +units=m +no_defs", (1_889_192.270, -1_415_235.050)),
            ("+proj=ortho +lat_0=45 +lon_0=-100 +ellps=WGS84 +f=0 +units=m +no_defs", (1_889_192.270, -1_415_235.050)),
            // Another flattening than the named datum's.
            ("+proj=laea +lat_0=45 +lon_0=-100 +datum=WGS84 +rf=300 +units=m +no_defs", (1_924_738.550, -1_437_048.884)),
        ] {
            let wkt = proj4_to_wkt(def, "X").unwrap();
            assert!(!wkt.contains("WGS_1984") && !wkt.contains(r#"SPHEROID["WGS 84""#), "{def}: {wkt}");
            let (crs, back) = (Crs::parse(def).unwrap(), crs_from_wkt(&wkt).unwrap());
            let (a, b) = (crs.transformer().unwrap().from_lonlat(c).unwrap(), back.transformer().unwrap().from_lonlat(c).unwrap());
            assert!((a.x - b.x).abs() < 0.001 && (a.y - b.y).abs() < 0.001, "{def}: {a:?} {b:?}\n{wkt}");
            assert!((a.x - proj.0).abs() < 0.001 && (a.y - proj.1).abs() < 0.001, "{def}: {a:?} PROJ {proj:?}");
        }
        // A flattening equal to the named ellipsoid's keeps its name and datum.
        let same = proj4_to_wkt("+proj=laea +lat_0=45 +lon_0=-100 +datum=WGS84 +f=0.0033528106647474805 +units=m +no_defs", "X").unwrap();
        assert!(same.contains(r#"DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]"#), "{same}");
        // A definition without a unit is in metres.
        let wkt = proj4_to_wkt("+proj=tmerc +lon_0=9 +x_0=500000 +datum=WGS84 +no_defs", "X").unwrap();
        assert!(wkt.contains(r#"PARAMETER["false_easting",500000]"#) && wkt.contains(r#"UNIT["metre",1],AXIS"#), "{wkt}");
    }

    /// The world projections (Natural Earth, Winkel Tripel, Eckert IV) are written as WKT, on
    /// their datum, with their centre and offsets (#1191: a GeoPDF of a map drawn in one).
    #[test]
    fn world_projections_are_written() {
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
        // Winkel Tripel's standard parallel: the definition's (the map's preset gives Winkel's
        // own), else 0, as PROJ 9.8.1 writes a definition without one.
        let wkt = proj4_to_wkt("+proj=wintri +lon_0=0 +lat_1=50.459776252189805 +datum=WGS84 +units=m +no_defs", "W").unwrap();
        assert!(wkt.contains("PARAMETER[\"standard_parallel_1\",50.459776252189805]"), "{wkt}");
        let wkt = proj4_to_wkt("+proj=wintri +lon_0=0 +datum=WGS84 +units=m +no_defs", "W").unwrap();
        assert!(wkt.contains("PARAMETER[\"standard_parallel_1\",0]"), "{wkt}");
        // In degrees, minutes and seconds, as the definition is read.
        let wkt = proj4_to_wkt("+proj=wintri +lon_0=0 +lat_1=50d30' +datum=WGS84 +units=m +no_defs", "W").unwrap();
        assert!(wkt.contains("PARAMETER[\"standard_parallel_1\",50.5]"), "{wkt}");
        let dms = Crs::parse("+proj=wintri +lat_1=50d30' +datum=WGS84 +units=m").unwrap();
        let back = crs_from_wkt(&to_wkt(dms).unwrap()).unwrap().transformer().unwrap();
        let c = Coord::new(10.0, 40.0);
        let (a, b) = (dms.transformer().unwrap().from_lonlat(c).unwrap(), back.from_lonlat(c).unwrap());
        assert!((a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6, "{a:?} {b:?}");
        // A world projection has its ellipsoid and datum shift read, to name its datum.
        let ((a, _), shift) = earth_of("+proj=natearth +ellps=intl +towgs84=1,2,3 +units=m").unwrap();
        assert_eq!((a, shift), (6_378_388.0, [1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 0.0]));
        // A definition it can't read for another reason isn't read as longitude/latitude.
        assert!(earth_of("+proj=tmerc +k=abc +ellps=intl +towgs84=1,2,3").is_none());
        // A projection terrio-crs doesn't know has none.
        assert_eq!(proj4_to_wkt("+proj=nosuch +datum=WGS84 +units=m +no_defs", "X"), None);
    }

    /// Natural Earth, Winkel Tripel and Eckert IV are read from WKT as GDAL (WKT1), Esri and
    /// PROJ (WKT2) write them, and from their ESRI codes (a FileGDB feature class's), not as
    /// longitude/latitude: each places a point where PROJ 9.8.1 does, within a millimetre, and
    /// back, with Winkel Tripel's standard parallel the file's. RustGIS's own WKT for its presets
    /// reads back to the system it was written from (#1701).
    #[test]
    fn world_projections_are_read() {
        let places = |crs: Crs, what: &str, points: &[((f64, f64), (f64, f64))]| {
            let t = crs.transformer().unwrap_or_else(|e| panic!("{what}: {e}"));
            for &((lon, lat), (x, y)) in points {
                let m = t.from_lonlat(Coord::new(lon, lat)).unwrap();
                assert!((m.x - x).abs() < 1e-3 && (m.y - y).abs() < 1e-3, "{what} {lon},{lat}: {m:?}");
                let back = t.to_lonlat(Coord::new(x, y)).unwrap();
                assert!((back.x - lon).abs() < 1e-8 && (back.y - lat).abs() < 1e-8, "{what} {x},{y}: {back:?}");
            }
        };
        // Centred on 60°W, offset by 100 m and -200 m; Winkel Tripel's parallel at 40°N.
        for (wkt1, esri, wkt2, points) in [
            (
                r#"PROJCS["unknown",GEOGCS["unknown",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]],PROJECTION["Natural_Earth"],PARAMETER["central_meridian",-60],PARAMETER["false_easting",100],PARAMETER["false_northing",-200],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#,
                r#"PROJCS["unknown",GEOGCS["GCS_unknown",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Natural_Earth"],PARAMETER["False_Easting",100.0],PARAMETER["False_Northing",-200.0],PARAMETER["Central_Meridian",-60.0],UNIT["Meter",1.0]]"#,
                r#"PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]],ID["EPSG",6326]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["unknown",METHOD["Natural Earth"],PARAMETER["Longitude of natural origin",-60,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["False easting",100,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",-200,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]"#,
                [((10.0, 50.0), (5944996.452900967, 5606224.7555508325)), ((-120.0, -35.0), (-5473849.827720951, -3939478.50735193))],
            ),
            (
                r#"PROJCS["unknown",GEOGCS["unknown",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]],PROJECTION["Eckert_IV"],PARAMETER["central_meridian",-60],PARAMETER["false_easting",100],PARAMETER["false_northing",-200],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#,
                r#"PROJCS["unknown",GEOGCS["GCS_unknown",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Eckert_IV"],PARAMETER["False_Easting",100.0],PARAMETER["False_Northing",-200.0],PARAMETER["Central_Meridian",-60.0],UNIT["Meter",1.0]]"#,
                r#"PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]],ID["EPSG",6326]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["unknown",METHOD["Eckert IV"],PARAMETER["Longitude of natural origin",-60,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["False easting",100,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",-200,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]"#,
                [((10.0, 50.0), (5581773.315127191, 6071282.405990218)), ((-120.0, -35.0), (-5221045.868366751, -4438772.808673019))],
            ),
            (
                r#"PROJCS["unknown",GEOGCS["unknown",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]],PROJECTION["Winkel_Tripel"],PARAMETER["central_meridian",-60],PARAMETER["standard_parallel_1",40],PARAMETER["false_easting",100],PARAMETER["false_northing",-200],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#,
                r#"PROJCS["unknown",GEOGCS["GCS_unknown",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Winkel_Tripel"],PARAMETER["False_Easting",100.0],PARAMETER["False_Northing",-200.0],PARAMETER["Central_Meridian",-60.0],PARAMETER["Standard_Parallel_1",40.0],UNIT["Meter",1.0]]"#,
                r#"PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]],ID["EPSG",6326]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["unknown",METHOD["Winkel Tripel"],PARAMETER["Longitude of natural origin",-60,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Latitude of 1st standard parallel",40,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8823]],PARAMETER["False easting",100,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",-200,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]"#,
                [((10.0, 50.0), (5795783.3284553625, 5703113.4871610105)), ((-120.0, -35.0), (-5457165.906327649, -3978189.6516721374))],
            ),
        ] {
            for wkt in [wkt1, esri, wkt2] {
                let crs = crs_from_wkt(wkt).unwrap_or_else(|e| panic!("{e}: {wkt}"));
                assert!(matches!(crs, Crs::Custom(_)), "{wkt}");
                places(crs, wkt, &points);
            }
        }
        // World Natural Earth and Eckert IV on WGS 84, and on the spheres ESRI gives them.
        for (code, points) in [
            (54077, [((10.0, 50.0), (849270.9218429951, 5606424.7555508325)), ((-120.0, -35.0), (-10947899.655441903, -3939278.50735193))]),
            (54012, [((10.0, 50.0), (797381.9021610274, 6071482.405990218)), ((-120.0, -35.0), (-10442291.736733502, -4438572.808673019))]),
            (
                53077,
                [((10.0, 50.0), (848321.7736396515, 5600158.995300423)), ((-120.0, -35.0), (-10935664.243842268, -3934875.9556790087))],
            ),
            (
                53012,
                [((10.0, 50.0), (796489.6487278191, 6064688.5459756795)), ((-120.0, -35.0), (-10430607.033798292, -4433606.139858049))],
            ),
        ] {
            let crs = crate::filegdb::crs_of_code(code).unwrap_or_else(|| panic!("ESRI:{code}"));
            places(crs, &format!("ESRI:{code}"), &points);
        }
        // A Winkel Tripel without a standard parallel is at 0, as PROJ 9.8.1 takes it: its
        // definition, WKT1 without the parameter, and the WKT written from the definition.
        let bare = "+proj=wintri +lon_0=-60 +x_0=100 +y_0=-200 +datum=WGS84 +units=m +no_defs";
        let at_0 = [((10.0, 50.0), (6707316.7995641865, 5703113.4871610105)), ((-120.0, -35.0), (-6238480.310135213, -3978189.6516721374))];
        places(Crs::parse(bare).unwrap(), bare, &at_0);
        let no_parallel = r#"PROJCS["unknown",GEOGCS["unknown",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]],PROJECTION["Winkel_Tripel"],PARAMETER["central_meridian",-60],PARAMETER["false_easting",100],PARAMETER["false_northing",-200],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#;
        places(crs_from_wkt(no_parallel).unwrap(), no_parallel, &at_0);
        let written = proj4_to_wkt(bare, "W").unwrap();
        places(crs_from_wkt(&written).unwrap(), &written, &at_0);
        // The presets' WKT reads back to the presets: Winkel Tripel at Winkel's own parallel,
        // which its preset gives.
        for def in [
            "+proj=natearth +lon_0=0 +datum=WGS84 +units=m +no_defs",
            "+proj=wintri +lon_0=0 +lat_1=50.459776252189805 +datum=WGS84 +units=m +no_defs",
            "+proj=eck4 +lon_0=0 +datum=WGS84 +units=m +no_defs",
        ] {
            let back = crs_from_wkt(&proj4_to_wkt(def, "World").unwrap()).unwrap().transformer().unwrap();
            let direct = crate::raster::crs::transformer_of(def).unwrap();
            for (lon, lat) in [(10.0, 50.0), (-120.0, -35.0), (179.0, 80.0)] {
                let (a, b) = (back.from_lonlat(Coord::new(lon, lat)).unwrap(), direct.from_lonlat(Coord::new(lon, lat)).unwrap());
                assert!((a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6, "{def} {lon},{lat}: {a:?} {b:?}");
            }
        }
        // Axes pointing west are read as PROJ 9.8.1 reads them (pyproj 3.8.0:
        // `Transformer.from_crs("EPSG:4326", wkt, always_xy=True).transform(10, 50)`); a
        // standard parallel at a pole isn't a Winkel Tripel.
        let wkt1 = r#"PROJCS["unknown",GEOGCS["unknown",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]],PROJECTION["Natural_Earth"],PARAMETER["central_meridian",-60],PARAMETER["false_easting",100],PARAMETER["false_northing",-200],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#;
        let west = crs_from_wkt(&wkt1.replace(r#"AXIS["Easting",EAST]"#, r#"AXIS["Easting",WEST]"#)).unwrap();
        let p = west.transformer().unwrap().from_lonlat(Coord::new(10.0, 50.0)).unwrap();
        assert!((p.x + 5_944_996.452_900_967).hypot(p.y - 5_606_224.755_550_833) < 1e-3, "{p:?}");
        let wintri = r#"PROJCS["unknown",GEOGCS["unknown",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]],PROJECTION["Winkel_Tripel"],PARAMETER["central_meridian",-60],PARAMETER["standard_parallel_1",40],PARAMETER["false_easting",100],PARAMETER["false_northing",-200],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#;
        assert!(crs_from_wkt(&wintri.replace(r#""standard_parallel_1",40"#, r#""standard_parallel_1",90"#)).is_err());
        // None has a scale factor: one other than 1 is refused in WKT, as `+k` is in a definition.
        let scaled =
            wkt1.replace(r#"PARAMETER["central_meridian",-60],"#, r#"PARAMETER["central_meridian",-60],PARAMETER["scale_factor",0.5],"#);
        assert!(crs_from_wkt(&scaled).unwrap_err().contains("scale factor other than 1"));
        assert!(crs_from_wkt(&scaled.replace("0.5]", "1]")).is_ok());
        // PROJ 9.8.1 ignores a PROJ string's scale in Natural Earth (pyproj 3.8.0: 10°E 40°N is
        // 894104.51, 4501295.58 with and without `+k=0.5`): refused rather than dropped, a scale
        // of 1 is the projection itself.
        for name in ["natearth", "wintri", "eck4"] {
            let def = format!("+proj={name} +k=0.5 +datum=WGS84 +units=m +no_defs");
            assert!(Crs::parse(&def).and_then(Crs::transformer).unwrap_err().contains("scale factor"), "{name}");
            assert!(Crs::parse(&def.replace("+k=0.5", "+k_0=2")).and_then(Crs::transformer).is_err(), "{name}");
        }
        let one = Crs::parse("+proj=natearth +k=1 +datum=WGS84 +units=m +no_defs").unwrap().transformer().unwrap();
        let at = one.from_lonlat(Coord::new(10.0, 40.0)).unwrap();
        assert!((at.x - 894_104.506_383_310_9).abs() < 1e-6 && (at.y - 4_501_295.579_974_174).abs() < 1e-6, "{at:?}");
        // Natural Earth and Eckert IV have no standard parallel: a `+lat_1` is ignored, as in PROJ,
        // where Winkel Tripel refuses one at a pole. `+geoc` changes nothing on their sphere (PROJ
        // 9.9.0's `proj` gives the same position with and without it).
        for name in ["natearth", "eck4"] {
            let plain = Crs::parse(&format!("+proj={name} +lon_0=-60 +datum=WGS84 +units=m +no_defs")).unwrap().transformer().unwrap();
            for extra in ["+lat_1=95", "+geoc"] {
                let def = format!("+proj={name} +lon_0=-60 {extra} +datum=WGS84 +units=m +no_defs");
                let t = Crs::parse(&def).unwrap_or_else(|e| panic!("{def}: {e}")).transformer().unwrap();
                let (a, b) = (t.from_lonlat(Coord::new(10.0, 45.0)).unwrap(), plain.from_lonlat(Coord::new(10.0, 45.0)).unwrap());
                assert!((a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6, "{def}: {a:?} {b:?}");
            }
        }
        assert!(Crs::parse("+proj=wintri +lat_1=95 +datum=WGS84 +units=m +no_defs").is_err());
        // An offset that isn't a number would place every point nowhere.
        for bad in ["+x_0=inf", "+y_0=NaN", "+x_0=-inf"] {
            let def = format!("+proj=natearth {bad} +datum=WGS84 +units=m +no_defs");
            assert!(Crs::parse(&def).is_err(), "{def}");
        }
    }

    /// A world projection's parameter that isn't a finite number (`1e400`, which the reader
    /// leaves out) isn't taken as not given, a parallel of 0 or a centre of 0°, in WKT1 and WKT2
    /// (#1701).
    #[test]
    fn world_projections_refuse_a_parameter_that_isnt_a_number() {
        let wintri = r#"PROJCS["unknown",GEOGCS["unknown",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]],PROJECTION["Winkel_Tripel"],PARAMETER["central_meridian",-60],PARAMETER["standard_parallel_1",40],PARAMETER["false_easting",100],PARAMETER["false_northing",-200],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#;
        let natearth = wintri.replace("Winkel_Tripel", "Natural_Earth").replace(r#"PARAMETER["standard_parallel_1",40],"#, "");
        assert!(crs_from_wkt(wintri).is_ok() && crs_from_wkt(&natearth).is_ok());
        let unusable = [
            wintri.replace(r#""standard_parallel_1",40"#, r#""standard_parallel_1",1e400"#),
            wintri.replace(r#""central_meridian",-60"#, r#""central_meridian",1e400"#),
            natearth.replace(r#""central_meridian",-60"#, r#""central_meridian",-1e400"#),
            r#"PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]],ID["EPSG",6326]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["unknown",METHOD["Winkel Tripel"],PARAMETER["Longitude of natural origin",-60,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Latitude of 1st standard parallel",1e400,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8823]],PARAMETER["False easting",100,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",-200,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]"#.to_string(),
        ];
        for wkt in unusable {
            let e = crs_from_wkt(&wkt).unwrap_err();
            assert!(e.contains("isn't a finite number"), "{e}: {wkt}");
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
        // As the same WKT under its own name: both take the WKT's datum shift, which PROJ 9.8.1
        // doesn't for the code itself (`+datum=potsdam` is DHDN's transformations, 0.6 m apart).
        let soldner = format!("{}My_Soldner{}", &wkt[..start], &wkt[end..]);
        let soldner = crs_from_wkt(&soldner).unwrap_or_else(|e| panic!("{e}: {soldner}"));
        // Near the grid origin and a few degrees from it, where a spherical form would differ.
        for lonlat in [Coord::new(13.4, 52.4), Coord::new(17.5, 55.5)] {
            let a = soldner.transformer().unwrap().from_lonlat(lonlat).unwrap();
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
        // NAD 27 with no TOWGS84: read by its Esri name, and transformed as PROJ 9.8.1 does
        // (pyproj 3.8.0: `Transformer.from_crs(CRS(wkt), "EPSG:4326", always_xy=True)
        // .transform(-100, 40)`), with nothing to say.
        let nad27 = r#"GEOGCS["GCS_North_American_1927",DATUM["D_North_American_1927",SPHEROID["Clarke_1866",6378206.4,294.9786982]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        let (crs, note) = read_wkt(nad27).unwrap();
        assert!(crs.is_geographic());
        assert_eq!(note, None);
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(-100.0, 40.0)).unwrap();
        assert!((ll.x + 100.000_415_588_626_37).abs() < 1e-9 && (ll.y - 39.999_996_882_432_82).abs() < 1e-9, "{ll:?}");
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
        // NAD 27 has PROJ's transformations, as reading it says nothing.
        assert!(!crs_from_wkt(nad27).unwrap().lacks_datum_shift());
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
        // Brazil's older datums, without TOWGS84, are transformed by their Esri names as PROJ
        // 9.8.1 does (SAD69 to WGS 84 (1), Corrego Alegre 1970-72 to WGS 84 (4); pyproj 3.8.0 at
        // 47°W 15°S), with nothing to say.
        for (datum, spheroid, lon, lat) in [
            (
                "D_South_American_1969",
                r#"SPHEROID["GRS_1967_Truncated",6378160.0,298.25]"#,
                -47.000_433_384_536_755,
                -15.000_445_045_462_978,
            ),
            ("D_Corrego_Alegre", r#"SPHEROID["International_1924",6378388.0,297.0]"#, -47.000_334_064_824_415, -15.000_238_252_866_357),
        ] {
            let old = format!(r#"GEOGCS["GCS_old",DATUM["{datum}",{spheroid}],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#);
            let (crs, note) = read_wkt(&old).unwrap();
            assert_eq!(note, None, "{datum}");
            let ll = crs.transformer().unwrap().to_lonlat(Coord::new(-47.0, -15.0)).unwrap();
            assert!((ll.x - lon).abs() < 1e-9 && (ll.y - lat).abs() < 1e-9, "{datum}: {ll:?}");
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
    fn twd97_reads_as_wgs_84_without_a_note() {
        // The .prj of the Ministry of the Interior's county boundaries (TWD97 2020 realization).
        let geographic = r#"GEOGCS["GCS_TWD97[2020]",DATUM["D_TWD_1997",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert_eq!(read_wkt(geographic), Ok((Crs::LonLat, None)));
        // The EPSG datum name, as OGC WKT writes it.
        let ogc = geographic.replace("D_TWD_1997", "Taiwan_Datum_1997");
        assert_eq!(read_wkt(&ogc), Ok((Crs::LonLat, None)));
        // TWD97 / TM2 zone 121 (EPSG:3826), as ArcGIS writes it: no note either.
        let tm2 = r#"PROJCS["TWD_1997_TM_Taiwan",GEOGCS["GCS_TWD_1997",DATUM["D_TWD_1997",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",250000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",121.0],PARAMETER["Scale_Factor",0.9999],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        let (crs, note) = read_wkt(tm2).unwrap();
        assert_eq!(note, None);
        // Taipei 101, about (306 km, 2,770 km) in TM2.
        let lonlat = crs.transformer().unwrap().to_lonlat(Coord::new(306_000.0, 2_770_000.0)).unwrap();
        assert!((lonlat.x - 121.56).abs() < 0.01 && (lonlat.y - 25.03).abs() < 0.01, "{lonlat:?}");
        // TWD67, the older datum on another ellipsoid, still gets its note.
        let twd67 = r#"GEOGCS["GCS_TWD_1967",DATUM["D_TWD_1967",SPHEROID["GRS_1967_Truncated",6378160.0,298.25]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert!(read_wkt(twd67).unwrap().1.is_some_and(|n| n.contains("D_TWD_1967")));
        // A file that names TWD97 on another spheroid keeps it, and PROJ 9.8.1's TWD97 to WGS 84 (1)
        // (a null transformation) on that spheroid: (121.5, 25) is (121.5, 24.99936953266989).
        let other =
            geographic.replace(r#"SPHEROID["GRS_1980",6378137.0,298.257222101]"#, r#"SPHEROID["International_1924",6378388.0,297.0]"#);
        let (crs, note) = read_wkt(&other).unwrap();
        assert_eq!(note, None);
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(121.5, 25.0)).unwrap();
        assert!((ll.x - 121.5).abs() < 1e-9 && (ll.y - 24.999_369_532_669_89).abs() < 1e-9, "{ll:?}");
        let def = crs.custom_def().unwrap();
        assert!((def.proj4.contains("6378388") || def.proj4.contains("+ellps=intl")) && !def.proj4.contains("GRS80"), "{}", def.proj4);
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
            // The WKT lands where its code does, but for Monte Mario (Rome), as in PROJ 9.8.1: the
            // code's own transformation covers Sardinia only, so at Rome the code has none (the
            // prime meridian alone), while Esri's WKT, which names no code, takes Monte Mario's,
            // which covers Italy: 72 m apart.
            if code != 4806 {
                assert!(distance(actual, expected) < 0.01, "EPSG:{code}: {actual:?} vs {expected:?}");
            }
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
            // PROJ writes seven parameters.
            let seven = if shift.matches(',').count() == 2 { format!("{shift},0,0,0,0") } else { shift.to_string() };
            assert!(def.proj4.contains(&format!("+towgs84={seven} ")), "{}", def.proj4);
            assert!(note.is_none());
        }
        // The datum's name identifies it, as in PROJ 9.8.1: another name (or one with only a
        // ballpark) is said, and the ellipsoid alone doesn't take its transformation away
        // (pyproj 3.8.0: `Transformer.from_crs(CRS(wkt), "EPSG:4326", always_xy=True)` at the
        // corpus row's point).
        let f: Vec<&str> = line.split('\t').collect();
        let xy = Coord::new(f[1].parse().unwrap(), f[2].parse().unwrap());
        for (changed, said, lon, lat) in [
            (wkt.replace("D_Monte_Mario", "D_Local"), true, 9.190_346_978_072_384, 45.463_529_988_917_08),
            (wkt.replace("D_Monte_Mario", "D_Roma_1940"), true, 9.190_346_978_072_384, 45.463_529_988_917_08),
            (wkt.replace("6378388.0", "6378300.0"), false, 9.190_004_725_089_445, 45.464_827_873_511_766),
        ] {
            let (crs, note) = read_wkt(&changed).unwrap();
            assert_eq!(note.is_some(), said, "{changed}: {note:?}");
            let ll = crs.transformer().unwrap().to_lonlat(xy).unwrap();
            assert!((ll.x - lon).abs() < 1e-8 && (ll.y - lat).abs() < 1e-8, "{changed}: {ll:?}");
        }
        // A locally named projection with different false easting retains its parameters, and
        // Monte Mario's shift: the point 100 m east is Milan, as PROJ 9.8.1 places it (pyproj 3.8.0,
        // `Transformer.from_crs(CRS(wkt), "EPSG:4326", always_xy=True)`).
        let local = wkt.replace("Monte_Mario_Italy_1", "Local grid").replace("1500000.0", "1500100.0");
        let crs = crs_from_wkt(&local).unwrap();
        assert!(crs.custom_def().unwrap().proj4.contains("+x_0=1500100"));
        let fields: Vec<&str> = line.split('\t').collect();
        let xy = Coord::new(fields[1].parse::<f64>().unwrap() + 100.0, fields[2].parse().unwrap());
        let ll = crs.transformer().unwrap().to_lonlat(xy).unwrap();
        assert!(metres_apart(ll, Coord::new(9.190_000_007_881_74, 45.464_200_001_031_124)) < 0.01, "{ll:?}");
        // Geographic WKT uses the same fallback; explicit shifts still take precedence.
        let geog = parse(wkt).unwrap().child("GEOGCS").unwrap().clone();
        assert!(earth(Some(&geog)).contains("+towgs84=-104.1,-49.1,-9.9"));
        assert!(datum_note(Some(&geog)).is_none());
        // An altered azimuth isn't the Swiss special case: it reads as the oblique Mercator its
        // parameters say, never as Swiss. The Krovak mutations stay refused: PROJ fixes the
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

    /// An Esri datum name is read as PROJ 9.8.1 reads it (pyproj 3.8.0, network off,
    /// `Transformer.from_crs(CRS(wkt), "EPSG:4326", always_xy=True)`): on another ellipsoid it keeps
    /// its transformation (PRS92 to WGS 84 (1)), an unknown datum name gets only the ballpark PROJ
    /// takes from the system's name (PRS92's), which is said, and a `TOWGS84` in the file wins. SAD69, NAD27 and RGF93 have PROJ's
    /// transformations, with nothing to say.
    #[test]
    fn esri_datum_names_respect_the_file_definition() {
        let row = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with("3123\t")).unwrap();
        let fields: Vec<_> = row.split('\t').collect();
        let (wkt, xy) = (fields[5], Coord::new(fields[1].parse().unwrap(), fields[2].parse().unwrap()));
        let clarke = r#"SPHEROID["Clarke_1866",6378206.4,294.978698213898]"#;
        for (changed, (lon, lat), said) in [
            (
                wkt.replace(clarke, r#"SPHEROID["International_1924",6378388.0,297.0]"#),
                (121.000_000_003_276, 14.599_574_608_041_847),
                false,
            ),
            (wkt.replace("D_Philippine_Reference_System_1992", "D_Local"), (120.998_640_630_823_35, 14.601_505_031_570_84), true),
            (wkt.replace(clarke, &format!("{clarke},TOWGS84[1,2,3]")), (120.998_623_115_827_26, 14.600_482_514_809_487), false),
        ] {
            let (crs, note) = read_wkt(&changed).unwrap();
            assert_eq!(note.is_some(), said, "{changed}: {note:?}");
            let ll = crs.transformer().unwrap().to_lonlat(xy).unwrap();
            assert!((ll.x - lon).abs() < 1e-9 && (ll.y - lat).abs() < 1e-9, "{changed}: {ll:?}");
        }
        let geog = |datum: &str, spheroid: &str| {
            format!(r#"GEOGCS["GCS",DATUM["{datum}",SPHEROID[{spheroid}]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#)
        };
        for (wkt, at, (lon, lat)) in [
            (
                geog("D_South_American_1969", r#""GRS_1967_Truncated",6378160.0,298.25"#),
                (-47.0, -15.0),
                (-47.000_433_384_536_755, -15.000_445_045_462_978),
            ),
            (
                geog("D_North_American_1927", r#""Clarke_1866",6378206.4,294.978698213898"#),
                (-100.0, 40.0),
                (-100.000_415_588_626_37, 39.999_996_882_432_82),
            ),
            (geog("D_RGF_1993", r#""GRS_1980",6378137.0,298.257222101"#), (2.0, 46.0), (2.0, 46.0)),
        ] {
            let (crs, note) = read_wkt(&wkt).unwrap();
            assert_eq!(note, None, "{wkt}");
            let ll = crs.transformer().unwrap().to_lonlat(Coord::new(at.0, at.1)).unwrap();
            assert!((ll.x - lon).abs() < 1e-9 && (ll.y - lat).abs() < 1e-9, "{wkt}: {ll:?}");
        }
    }

    /// An Esri `.prj` on a national datum saves a PROJ string with the datum's bundled shift, so an
    /// older release that reads only that string still shifts it: as before for the datums RustGIS
    /// named itself (OSGB36, ED50 …), and by its EPSG system's shift for one known by its Esri name
    /// (PRS92, Pulkovo 1942), which it read without a shift before.
    #[test]
    fn a_national_datum_keeps_its_shift_in_the_saved_proj_string() {
        for code in [3003u16, 23032, 27700, 31467, 28992, 2100, 21781, 3123, 28407] {
            let row = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with(&format!("{code}\t"))).unwrap();
            let (crs, _) = read_wkt(row.split('\t').nth(5).unwrap()).unwrap();
            let proj4 = &crs.custom_def().unwrap().proj4;
            let shift = epsg_proj4(code).unwrap().split_whitespace().filter(|t| t.starts_with("+datum=") || t.starts_with("+towgs84="));
            for t in shift {
                assert!(proj4.split_whitespace().any(|p| p == t), "EPSG:{code}: {proj4} lacks {t}");
            }
            assert!(proj4.contains("+towgs84=") || proj4.contains("+datum="), "EPSG:{code}: {proj4}");
        }
    }

    /// A datum known only by its Esri name is the EPSG geographic system terrio-crs identifies,
    /// as PROJ 9.8.1 does (pyproj 3.8.0: `CRS(wkt).to_epsg()` of the bare `GEOGCS`); an unknown
    /// name is none.
    #[test]
    fn esri_datum_names_give_their_epsg_system() {
        for (datum, spheroid, code) in [
            ("D_Philippine_Reference_System_1992", r#""Clarke_1866",6378206.4,294.978698213898"#, Some(4683)),
            ("D_Pulkovo_1942", r#""Krasovsky_1940",6378245.0,298.3"#, Some(4284)),
            ("D_Tokyo", r#""Bessel_1841",6377397.155,299.1528128"#, Some(4301)),
            ("D_WGS_1972", r#""WGS_1972",6378135.0,298.26"#, Some(4322)),
            ("D_Beijing_1954", r#""Krasovsky_1940",6378245.0,298.3"#, Some(4214)),
            ("D_Arc_1960", r#""Clarke_1880_RGS",6378249.145,293.465"#, Some(4210)),
            ("D_Local", r#""Clarke_1866",6378206.4,294.978698213898"#, None),
        ] {
            let wkt =
                format!(r#"GEOGCS["GCS",DATUM["{datum}",SPHEROID[{spheroid}]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#);
            let root = tree(&wkt).unwrap();
            let node = geogcs(&root).and_then(|g| g.child("DATUM")).unwrap();
            assert_eq!(esri_datum_code(node, None), code, "{datum}");
        }
        // On its own prime meridian, with its geographic system (PROJ: `CRS(wkt).to_epsg()` 4801).
        let bern = r#"GEOGCS["GCS_Bern_1898_Bern",DATUM["CH1903_(Bern)",SPHEROID["Bessel_1841",6377397.155,299.1528128]],PRIMEM["Bern",7.43958333333333],UNIT["Degree",0.0174532925199433]]"#;
        let root = tree(bern).unwrap();
        let geog = geogcs(&root).unwrap();
        assert_eq!(esri_datum_code(geog.child("DATUM").unwrap(), Some(geog)), Some(4801));
        // A datum not known by its Esri name, identified by its system's name as PROJ does (GR96,
        // 4747).
        let greenland = r#"GEOGCS["GCS_Greenland_1996",DATUM["D_Greenland_1996",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        let root = tree(greenland).unwrap();
        let geog = geogcs(&root).unwrap();
        assert_eq!(esri_datum_code(geog.child("DATUM").unwrap(), Some(geog)), Some(4747));
    }

    /// Monte Mario (Rome), EPSG:4806, as GDAL writes it to a `.prj` (`Monte_Mario_(Rome)`) and as
    /// WKT2 names it (`Monte Mario (Rome)`), with the Rome prime meridian, placed at Rome as PROJ
    /// 9.8.1 places it (pyproj 3.8.0, network off, `Transformer.from_crs(CRS(wkt), "EPSG:4326",
    /// always_xy=True)`): the WKT2 names its code, whose own transformation covers Sardinia only,
    /// so at Rome it has none; the Esri WKT names none, and takes Monte Mario's, which covers
    /// Italy.
    #[test]
    fn monte_mario_rome_takes_the_monte_mario_shift() {
        let esri = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with("4806\t")).unwrap();
        let wkt2 = include_str!("wkt2_corpus.tsv").lines().find(|l| l.starts_with("EPSG:4806\t")).unwrap();
        let rome = Coord::new(0.044_066_666_666_665_026, 41.9028);
        for (row, at) in [(esri, Coord::new(12.496_213_644, 41.903_443_487)), (wkt2, Coord::new(12.4964, 41.9028))] {
            let wkt = row.split('\t').nth(5).unwrap();
            let (crs, note) = read_wkt(wkt).unwrap();
            assert!(note.is_none(), "{note:?}");
            let ll = crs.transformer().unwrap().to_lonlat(rome).unwrap();
            assert!(metres_apart(ll, at) < 0.01, "{ll:?} is {} m from {at:?}", metres_apart(ll, at));
        }
    }

    #[test]
    fn italian_datums_require_their_ellipsoid() {
        for datum in ["D_Rete_Dinamica_Nazionale_2008", "D_IGM_1995", "Istituto_Geografico_Militaire_1995"] {
            let geographic = GEOG_4326.replace("D_WGS_1984", datum).replace("6378137.0", "6378000.0");
            // PROJ 9.8.1 applies the datum's null transformation to WGS 84 on the file's
            // ellipsoid, nothing to say: (12.5, 41.9) is (12.5, 41.90000411064424).
            let (crs, note) = read_wkt(&geographic).unwrap();
            assert_eq!(note, None, "{datum}");
            let def = crs.custom_def().unwrap();
            assert!(def.proj4.contains("+a=6378000") && !def.proj4.contains("+towgs84="), "{}", def.proj4);
            let ll = crs.transformer().unwrap().to_lonlat(Coord::new(12.5, 41.9)).unwrap();
            assert!((ll.x - 12.5).abs() < 1e-9 && (ll.y - 41.900_004_110_644_24).abs() < 1e-9, "{datum}: {ll:?}");
        }
        // Projected, the same: PROJ 9.8.1 places (500000, 4982950.4001) at 9°E, and 45.00096764648024°N
        // (RDN2008, 6707) or 45.000967647557786°N (IGM95, 3064).
        for (code, lat) in [("6707", 45.000_967_646_480_24), ("3064", 45.000_967_647_557_786)] {
            let row = include_str!("esri_datum_corpus.tsv").lines().find(|l| l.starts_with(&format!("{code}\t"))).unwrap();
            let c: Vec<&str> = row.split('\t').collect();
            let projected = c[5].replace("6378137.0", "6378000.0");
            let (crs, note) = read_wkt(&projected).unwrap();
            assert_eq!(note, None, "{code}");
            let def = crs.custom_def().unwrap();
            assert!(def.proj4.contains("+a=6378000") && !def.proj4.contains("+towgs84="), "{}", def.proj4);
            let xy = Coord::new(c[1].parse().unwrap(), c[2].parse().unwrap());
            let ll = crs.transformer().unwrap().to_lonlat(xy).unwrap();
            assert!((ll.x - 9.0).abs() < 1e-9 && (ll.y - lat).abs() < 1e-9, "{code}: {ll:?}");
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
        let rows = corpus();
        assert_eq!(rows.len(), 84);
        for (code, [x, y, lon, lat], wkt) in rows {
            if let Some(n) = code.strip_prefix("EPSG:") {
                let with_id = format!("{},ID[\"EPSG\",{n}]]", wkt.strip_suffix(']').unwrap());
                let expected = Crs::from_epsg(n.parse().unwrap()).unwrap();
                assert_eq!(crs_from_wkt(&with_id), Ok(expected), "{code}");
                assert_eq!(Crs::parse(&with_id), Ok(expected), "{code}");
            }
            // Each is placed as PROJ 9.8.1 places it (the corpus's longitude and latitude).
            let crs = crs_from_wkt(wkt).unwrap_or_else(|e| panic!("{code}: {e}"));
            let ll = crs.transformer().unwrap().to_lonlat(Coord::new(x, y)).unwrap();
            let dlon = (ll.x - lon + 540.0).rem_euclid(360.0) - 180.0;
            let metres = (dlon * lat.to_radians().cos()).hypot(ll.y - lat) * 111_195.0;
            assert!(metres < 0.01, "{code}: {metres} m off, {:?}", crs.custom_def().map(|d| d.proj4.clone()));
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
    pub(crate) const WGS84_WKT2: &str = r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4326]]"#;

    /// OSGB 36 longitude/latitude (EPSG:4277), as PROJ writes its WKT2.
    const OSGB36_WKT2: &str = r#"GEOGCRS["OSGB36",DATUM["Ordnance Survey of Great Britain 1936",ELLIPSOID["Airy 1830",6377563.396,299.3249646,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4277]]"#;

    /// ED50 longitude/latitude (EPSG:4230), as PROJ writes its WKT2.
    const ED50_WKT2: &str = r#"GEOGCRS["ED50",DATUM["European Datum 1950",ELLIPSOID["International 1924",6378388,297,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4230]]"#;

    /// Longitude/latitude on a datum of the Bessel 1841 ellipsoid RustGIS doesn't know.
    const BESSEL_WKT2: &str = r#"GEOGCRS["Local",DATUM["Local datum",ELLIPSOID["Bessel 1841",6377397.155,299.1528128,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]"#;

    /// An example shift from [`BESSEL_WKT2`]'s datum, in the position vector convention.
    const BESSEL_SHIFT: [f64; 7] = [598.1, 73.7, 418.2, 0.202, 0.045, -2.455, 6.7];

    /// Whether two texts read alike: the same notes (or error), and systems that place `at` alike.
    fn reads_alike(a: &str, b: &str, at: Coord) -> bool {
        match (read_wkt(a), read_wkt(b)) {
            (Ok((p, pn)), Ok((q, qn))) => pn == qn && places_alike(p, q, at),
            (p, q) => p.is_err() && p.err() == q.err(),
        }
    }

    /// Whether two systems place the point `at` (longitude, latitude) alike, to a nanometre: two
    /// reads of a WKT each keep their own text, so they are equal systems but not equal `Crs`.
    fn places_alike(a: Crs, b: Crs, at: Coord) -> bool {
        let place = |c: Crs| c.transformer().ok().and_then(|t| t.from_lonlat(at));
        match (place(a), place(b)) {
            (Some(p), Some(q)) => (p.x - q.x).hypot(p.y - q.y) < 1e-9,
            _ => false,
        }
    }

    /// A `BOUNDCRS` from `source` to `target` by a position vector transformation of `shift`
    /// (metres, arc-seconds, parts per million; seven of them), or geocentric translations (three).
    /// British National Grid (EPSG:27700) in WKT1 with its `TOWGS84` (OSGB36's shift to WGS 84),
    /// as RustGIS wrote it from its PROJ definition: a file whose shift the tests change.
    pub(crate) fn bng_wkt1() -> String {
        def_wkt(epsg_proj4(27700).unwrap(), "EPSG:27700", Some(27700)).unwrap()
    }

    pub(crate) fn bound_crs(source: &str, target: &str, shift: &[f64]) -> String {
        let names =
            ["X-axis translation", "Y-axis translation", "Z-axis translation", "X-axis rotation", "Y-axis rotation", "Z-axis rotation"];
        let mut params: Vec<String> =
            shift.iter().zip(names).zip(8605..).map(|((v, name), code)| format!(r#"PARAMETER["{name}",{v},ID["EPSG",{code}]]"#)).collect();
        if let Some(ppm) = shift.get(6) {
            params.push(format!(r#"PARAMETER["Scale difference",{ppm},SCALEUNIT["parts per million",1E-06],ID["EPSG",8611]]"#));
        }
        format!(
            r#"BOUNDCRS[SOURCECRS[{source}],TARGETCRS[{target}],ABRIDGEDTRANSFORMATION["t",METHOD[{}],{}]]"#,
            // Three translations alone are geocentric translations, as PROJ 9.8.1 writes them: a
            // position vector transformation without its rotations and scale has no PROJ string.
            if shift.len() == 3 {
                r#""Geocentric translations (geog2D domain)",ID["EPSG",9603]"#
            } else {
                r#""Position Vector transformation (geog2D domain)",ID["EPSG",9606]"#
            },
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
            // The file's shift to ED50, then ED50's to WGS 84 as PROJ 9.8.1 picks it there: "ED50
            // to WGS 84 (17)" (France's, whose area reaches 10.38°E), 10°E 50°N on the Bessel datum.
            // pyproj 3.8.0, network off, with the scale written as unity (`1.0000067`), since PROJ
            // reads a bound transformation's parts per million as a ratio:
            // `Transformer.from_crs(CRS(wkt), "EPSG:4326", always_xy=True).transform(10, 50)`.
            PlacementCase {
                what: "a bound system to ED50",
                wkt: bound_crs(BESSEL_WKT2, ED50_WKT2, &BESSEL_SHIFT),
                xy: Coord::new(10.0, 50.0),
                lonlat: Coord::new(9.997_684_968_762_389, 49.998_866_814_066_59),
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

    /// A bound system whose transformation RustGIS can't apply, a grid (NTv2, as PROJ 9.8.1 writes
    /// `+nadgrids=`) or another without Helmert translations, is read as its source system with a
    /// note naming the transformation, and so is GDAL's WKT1 form of a grid
    /// (`EXTENSION["PROJ4_GRIDS",…]`), but not `@null`, which is no shift. One without a
    /// transformation, which ISO 19162 requires and PROJ refuses, is read as its source with a note
    /// saying so. A Helmert one is applied, without the note (#1678).
    /// UTM zone 17N on NAD 27, as PROJ 9.8.1 writes it in WKT2 without a code.
    pub(crate) const NAD27_UTM17: &str = r#"PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898,LENGTHUNIT["metre",1]],ID["EPSG",6267]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["UTM zone 17N",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",-81,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]],ID["EPSG",16017]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]"#;

    /// [`NAD27_UTM17`] bound to WGS 84 by the NTv2 grid `conus`: PROJ 9.8.1's WKT2 for
    /// `+proj=utm +zone=17 +datum=NAD27 +nadgrids=conus +type=crs`.
    pub(crate) const BOUND_BY_A_GRID: &str = r#"BOUNDCRS[SOURCECRS[PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898,LENGTHUNIT["metre",1]],ID["EPSG",6267]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["UTM zone 17N",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",-81,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]],ID["EPSG",16017]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["latitude",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["longitude",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4326]]],ABRIDGEDTRANSFORMATION["unknown to WGS84",METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","conus",ID["EPSG",8656]]]]"#;

    #[test]
    fn a_bound_system_says_when_its_transformation_isnt_applied() {
        // PROJ 9.8.1's WKT2 for `+proj=longlat +datum=NAD27 +nadgrids=ntv2_0.gsb +type=crs`, and
        // for UTM zone 17N on it with `+nadgrids=conus`.
        const NAD27: &str = r#"GEOGCRS["unknown",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898,LENGTHUNIT["metre",1]],ID["EPSG",6267]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]],CS[ellipsoidal,2],AXIS["longitude",east,ORDER[1],ANGLEUNIT["degree",0.0174532925199433,ID["EPSG",9122]]],AXIS["latitude",north,ORDER[2],ANGLEUNIT["degree",0.0174532925199433,ID["EPSG",9122]]]]"#;
        const UTM17: &str = NAD27_UTM17;
        const WGS84: &str = r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["latitude",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["longitude",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4326]]"#;
        let grid = |source: &str, file: &str| {
            format!(
                r#"BOUNDCRS[SOURCECRS[{source}],TARGETCRS[{WGS84}],ABRIDGEDTRANSFORMATION["unknown to WGS84",METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","{file}",ID["EPSG",8656]]]]"#
            )
        };
        // Read as the source alone. Alone, NAD 27 is transformed as PROJ 9.8.1 transforms it (by
        // its EPSG transformations, NAD27 to WGS 84 (3) and others), and nothing is said; bound,
        // the file gives one, and what's said is that it isn't applied.
        let as_source = |wkt: &str, source: &str, dropped: &str| {
            let (crs, note) = read_wkt(wkt).unwrap();
            let (alone, alone_note) = read_wkt(source).unwrap();
            // Each keeps its own WKT; both place points alike, the grid left out.
            let at = Coord::new(-79.0, 43.0);
            let (a, b) = (crs.transformer().unwrap().from_lonlat(at).unwrap(), alone.transformer().unwrap().from_lonlat(at).unwrap());
            assert!((a.x - b.x).hypot(a.y - b.y) < 1e-6, "{a:?} {b:?}: {wkt}");
            assert_eq!(alone_note, None);
            assert_eq!(note.as_deref(), Some(dropped), "{wkt}");
        };
        as_source(
            &grid(NAD27, "ntv2_0.gsb"),
            NAD27,
            "the file's NTv2 transformation to WGS 84 (ntv2_0.gsb) isn't applied, so positions may be off by as much as that shift",
        );
        as_source(
            &grid(UTM17, "conus"),
            UTM17,
            "the file's NTv2 transformation to WGS 84 (conus) isn't applied, so positions may be off by as much as that shift",
        );
        // Another method without Helmert translations: named, without a file.
        let offsets = grid(NAD27, "x").replace(
            r#"METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","x",ID["EPSG",8656]]"#,
            r#"METHOD["Geographic2D offsets",ID["EPSG",9619]],PARAMETER["Latitude offset",-0.5,ANGLEUNIT["arc-second",4.84813681109536E-06],ID["EPSG",8601]],PARAMETER["Longitude offset",1.5,ANGLEUNIT["arc-second",4.84813681109536E-06],ID["EPSG",8602]]"#,
        );
        assert!(offsets.contains("Geographic2D offsets"), "{offsets}");
        as_source(
            &offsets,
            NAD27,
            "the file's Geographic2D offsets transformation to WGS 84 isn't applied, so positions may be off by as much as that shift",
        );
        // GDAL's WKT1 of the first (PROJ 9.8.1's WKT1_GDAL): the grid in an extension. `@null`
        // is no shift, and says nothing.
        let wkt1 = r#"GEOGCS["unknown",DATUM["North_American_Datum_1927",SPHEROID["Clarke 1866",6378206.4,294.978698213898,AUTHORITY["EPSG","7008"]],EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"],AUTHORITY["EPSG","6267"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]]"#;
        let plain = wkt1.replace(r#"EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"],"#, "");
        assert_ne!(plain, wkt1);
        as_source(wkt1, &plain, "the file's grid shift (ntv2_0.gsb) isn't applied, so positions may be off by as much as that shift");
        let ((null, null_note), (bare, bare_note)) = (read_wkt(&wkt1.replace("ntv2_0.gsb", "@null")).unwrap(), read_wkt(&plain).unwrap());
        assert_eq!(null_note, bare_note);
        assert!(places_alike(null, bare, Coord::new(-79.0, 43.0)));
        // A name from the file is cut short in the notice.
        let long = "g".repeat(5000);
        let (_, note) = read_wkt(&grid(NAD27, &long)).unwrap();
        let note = note.unwrap();
        assert!(note.chars().count() < 250 && note.contains('…') && note.ends_with("as much as that shift"), "{note}");
        // No transformation (PROJ refuses the system): read as the source, as projected data refused
        // would be read as longitude/latitude, and said, with the source's own note if it has one.
        let untransformed = "the file's bound coordinate system has no transformation to WGS 84, so it is read as its source system";
        for source in [NAD27, UTM17] {
            let none = format!(r#"BOUNDCRS[SOURCECRS[{source}],TARGETCRS[{WGS84}]]"#);
            let (crs, note) = read_wkt(&none).unwrap();
            let (alone, alone_note) = read_wkt(source).unwrap();
            assert!(places_alike(crs, alone, Coord::new(-79.0, 43.0)), "{none}");
            let note = note.unwrap();
            assert!(note.ends_with(untransformed) && alone_note.as_deref().is_none_or(|a| note.contains(a)), "{note}");
        }
        // On WGS 84 there is nothing to convert, and nothing is said.
        let (crs, note) = read_wkt(&format!(r#"BOUNDCRS[SOURCECRS[{WGS84}],TARGETCRS[{WGS84}]]"#)).unwrap();
        assert_eq!((crs, note), (Crs::LonLat, None));
        // PROJ's placeholders: a ballpark offset is no known shift, so the source's datum note
        // stands alone; a null offset is a shift of none, and says nothing of a grid either.
        let method = r#"METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","x",ID["EPSG",8656]]"#;
        let (alone, alone_note) = read_wkt(NAD27).unwrap();
        for placeholder in ["Ballpark geographic offset", "Null geographic offset"] {
            let wkt = grid(NAD27, "x").replace(method, &format!(r#"METHOD["{placeholder}"]"#));
            assert_ne!(wkt, grid(NAD27, "x"));
            let (crs, note) = read_wkt(&wkt).unwrap();
            let note = note.unwrap_or_default();
            assert!(!note.contains("isn't applied") && !note.contains("has no transformation"), "{placeholder}: {note}");
            if placeholder.starts_with("Ballpark") {
                assert_eq!((!note.is_empty()).then_some(note), alone_note.clone(), "{placeholder}");
                assert!(places_alike(crs, alone, Coord::new(-79.0, 43.0)), "{placeholder}");
            } else {
                assert!(!note.contains("has no conversion"), "{placeholder}: {note}");
            }
        }
        // GDAL's other form of a grid: `+nadgrids=` in the system's `PROJ4` extension. Said as the
        // `PROJ4_GRIDS` one is, in place of the datum note; `@null` says nothing.
        let utm_wkt1 = r#"PROJCS["NAD27 / UTM zone 17N",GEOGCS["NAD27",DATUM["North_American_Datum_1927",SPHEROID["Clarke 1866",6378206.4,294.978698213898]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-81],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1]]"#;
        let with = |grids: &str| {
            utm_wkt1.replace(
                r#"UNIT["metre",1]]"#,
                &format!(r#"UNIT["metre",1],EXTENSION["PROJ4","+proj=utm +zone=17 +datum=NAD27 +nadgrids={grids} +units=m +no_defs"]]"#),
            )
        };
        let (plain_crs, _) = read_wkt(utm_wkt1).unwrap();
        let (crs, note) = read_wkt(&with("conus")).unwrap();
        assert_eq!(note.as_deref(), Some("the file's grid shift (conus) isn't applied, so positions may be off by as much as that shift"));
        assert!(places_alike(crs, plain_crs, Coord::new(-79.0, 43.0)));
        // `@null` says nothing more either, but is a shift of none, as PROJ 9.8.1 places it
        // (pyproj 3.8.0, network off: `Transformer.from_crs("EPSG:4326", wkt, always_xy=True)
        // .transform(-79, 43)`), and the plain system takes PROJ's NAD27 transformation.
        let ((null, null_note), (bare, bare_note)) = (read_wkt(&with("@null")).unwrap(), read_wkt(utm_wkt1).unwrap());
        assert_eq!(null_note, bare_note);
        let at = Coord::new(-79.0, 43.0);
        for (crs, x, y) in [(null, 663_023.669_508_077, 4_762_540.718_029_167), (bare, 663_000.991_695_212_7, 4_762_540.581_581_737)] {
            let t = crs.transformer().unwrap();
            let p = t.from_lonlat(at).unwrap_or_else(|| {
                panic!(
                    "{x}: {:?} {:?} back {:?}",
                    crs.custom_def().map(|d| d.proj4.clone()),
                    t.from_lonlat_unchecked(at),
                    t.from_lonlat_unchecked(at).and_then(|o| t.to_lonlat(o))
                )
            });
            assert!((p.x - x).hypot(p.y - y) < 1e-3, "{p:?}");
        }
        // An optional grid (`@`) is named without PROJ's mark.
        let (_, note) = read_wkt(&with("@conus,@null")).unwrap();
        assert_eq!(note.as_deref(), Some("the file's grid shift (conus) isn't applied, so positions may be off by as much as that shift"));
        // A system read otherwise (by `Crs::parse`, typed into a command, or by a GeoPackage
        // row's EPSG code) says it as the definition's; one without a grid, or a code, says
        // nothing.
        assert_eq!(grid(UTM17, "conus"), BOUND_BY_A_GRID);
        let said = "the definition's NTv2 transformation to WGS 84 (conus) isn't applied, so positions may be off by as much as that shift";
        assert_eq!(unapplied_note(BOUND_BY_A_GRID, Crs::parse(BOUND_BY_A_GRID).unwrap()).as_deref(), Some(said));
        assert_eq!(typed_crs_note(BOUND_BY_A_GRID, Crs::parse(BOUND_BY_A_GRID).unwrap()).as_deref(), Some(said));
        // What those paths said before: nothing.
        assert_eq!(projection_compatibility_note(BOUND_BY_A_GRID, Crs::parse(BOUND_BY_A_GRID).unwrap()), None);
        let none = format!(r#"BOUNDCRS[SOURCECRS[{UTM17}],TARGETCRS[{WGS84}]]"#);
        assert_eq!(
            unapplied_note(&none, Crs::parse(&none).unwrap()).as_deref(),
            Some(untransformed.replace("the file's", "the definition's").as_str())
        );
        for clean in [UTM17, "EPSG:32617"] {
            assert_eq!(unapplied_note(clean, Crs::parse(clean).unwrap()), None, "{clean}");
            assert_eq!(typed_crs_note(clean, Crs::parse(clean).unwrap()), None, "{clean}");
        }
        // A Helmert transformation is applied, and says nothing of it.
        let (_, note) = read_wkt(&bound_crs(OSGB36_WKT2, WGS84_WKT2, &[375.0, -111.0, 431.0])).unwrap();
        assert!(!note.as_deref().unwrap_or("").contains("isn't applied"), "{note:?}");
        // The marks `bound` leaves can't come from a file: an extension or a node of a like name
        // is read as any other it doesn't know, in WKT1 or WKT2, and the datum note stays.
        for forged in [
            r#"EXTENSION["RUSTGIS_UNAPPLIED","a forged grid"]"#,
            r#"EXTENSION["RUSTGIS_UNTRANSFORMED","Mars"]"#,
            r#"EXTENSION["rustgis:unapplied","a forged grid"]"#,
            r#"RUSTGIS_UNAPPLIED["a forged grid"]"#,
            r#"rustgis_untransformed["Mars"]"#,
        ] {
            let in_wkt1 = wkt1.replace(r#"EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"]"#, forged);
            assert!(reads_alike(&in_wkt1, &plain, Coord::new(-79.0, 43.0)), "{in_wkt1}");
            let in_wkt2 = NAD27.replace(r#"ID["EPSG",6267]],PRIMEM"#, &format!(r#"ID["EPSG",6267],{forged}],PRIMEM"#));
            assert_ne!(in_wkt2, NAD27);
            assert!(reads_alike(&in_wkt2, NAD27, Coord::new(-79.0, 43.0)), "{in_wkt2}");
        }
        // Nor can a mark's own name be written: the parser reads no name with a colon.
        assert!(read_wkt(&wkt1.replace(r#"EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"]"#, r#"rustgis:unapplied["x"]"#)).is_err());
        // A projected source without a datum is read on WGS 84, as RustGIS read it before, though
        // PROJ 9.8.1 refuses it ("Missing DATUM or ENSEMBLE node"), and the note says so. Bound,
        // it is read as the source alone, with the bound system's note after it.
        let datum = r#"DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898,LENGTHUNIT["metre",1]],ID["EPSG",6267]],"#;
        let no_datum = UTM17.replace(datum, "");
        assert_ne!(no_datum, UTM17);
        let (alone, alone_note) = read_wkt(&no_datum).unwrap();
        let on_wgs84 = Crs::parse("+proj=utm +zone=17 +datum=WGS84 +units=m +no_defs").unwrap();
        assert!(places_alike(alone, on_wgs84, Coord::new(-79.0, 43.0)), "{:?}", alone.custom_def());
        let said =
            "the coordinate system unknown has no datum and is read on WGS 84, so positions may be off by up to a few hundred metres";
        assert_eq!(alone_note.as_deref(), Some(said));
        for (bound, then) in [
            (format!(r#"BOUNDCRS[SOURCECRS[{no_datum}],TARGETCRS[{WGS84}]]"#), untransformed),
            (
                grid(&no_datum, "x.gsb"),
                "the file's NTv2 transformation to WGS 84 (x.gsb) isn't applied, so positions may be off by as much as that shift",
            ),
        ] {
            let (crs, note) = read_wkt(&bound).unwrap_or_else(|e| panic!("{e}: {bound}"));
            assert!(places_alike(crs, alone, Coord::new(-79.0, 43.0)), "{bound}");
            assert_eq!(note, Some(format!("{said}; {then}")), "{bound}");
        }
        // A geographic one is still read as WGS 84 longitude/latitude, as before, saying so.
        let no_datum = NAD27.replace(datum, "");
        assert_ne!(no_datum, NAD27);
        let (crs, note) = read_wkt(&no_datum).unwrap();
        assert_eq!(crs, Crs::LonLat);
        assert!(note.is_some_and(|n| n.contains("has no datum and is read on WGS 84")));
        // By its code, EPSG:4326 exactly: nothing said.
        let coded = r#"GEOGCS["WGS 84",PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4326"]]"#;
        assert_eq!(read_wkt(coded).unwrap(), (Crs::LonLat, None));
        // With its own prime meridian, on WGS 84 from that meridian, as before, saying so; in
        // grads, refused, as before.
        let paris = r#"GEOGCS["x",PRIMEM["Paris",2.33722917],UNIT["degree",0.0174532925199433]]"#;
        let (crs, note) = read_wkt(paris).unwrap();
        assert!(note.is_some_and(|n| n.contains("has no datum and is read on WGS 84")));
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(1.0, 45.0)).unwrap();
        // PROJ 9.8.1 with a WGS 84 DATUM added: 3.3372291666666665, 45 (Paris as 2.5969213 grads).
        assert!((ll.x - 3.337_229_166_666_666_5).abs() < 1e-12 && (ll.y - 45.0).abs() < 1e-12, "{ll:?}");
        assert!(read_wkt(&paris.replace(r#"UNIT["degree",0.0174532925199433]"#, r#"UNIT["grad",0.015707963267949]"#)).is_err());
    }

    /// A system read from WKT whose most accurate transformation needs a grid RustGIS doesn't have
    /// (NAD 27's in the United States) says so once used, as an EPSG code does.
    #[test]
    fn a_wkt_system_says_its_missing_grid() {
        let crs = crs_from_wkt(NAD27_UTM17).unwrap();
        assert!(matches!(crs, Crs::Custom(_)));
        crs.transformer().unwrap().from_lonlat(Coord::new(-81.0, 35.0)).unwrap();
        let note = super::super::crs::grid_note(crs).unwrap_or_default();
        assert!(note.contains("needs a grid RustGIS doesn't have"), "{note:?}");
    }

    /// A `.prj` saved with a byte order mark (Windows), or space around it, reads as without.
    /// So does one written over several lines: the same system.
    #[test]
    fn a_byte_order_mark_is_read_past() {
        let utm = NAD27_UTM17.replace(r#"PROJCRS["unknown""#, r#"PROJCRS["mine""#).replace("UTM zone 17N", "mine");
        let plain = read_wkt(&utm).unwrap();
        assert!(matches!(plain.0, Crs::Custom(_)));
        assert_eq!(read_wkt(&format!("\u{feff}{utm}\r\n")), Ok(plain.clone()));
        assert_eq!(read_wkt(&utm.replace("],", "],\r\n    ")), Ok(plain.clone()));
        assert_eq!(read_wkt_over_code(&format!("\u{feff}{utm}"), 26717), read_wkt_over_code(&utm, 26717));
        let esri = r#"PROJCS["Lambert",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Lambert_Conformal_Conic"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",10.0],PARAMETER["Standard_Parallel_1",45.0],PARAMETER["Standard_Parallel_2",50.0],PARAMETER["Latitude_Of_Origin",47.0],UNIT["Meter",1.0]]"#;
        assert_eq!(read_wkt(&format!("\u{feff}{esri}")), read_wkt(esri));
        assert!(read_wkt(esri).is_ok());
    }

    /// The issue's case (#1678): a Shapefile whose `.prj` is NAD 27 bound to WGS 84 by an NTv2 grid,
    /// as PROJ 9.8.1 writes it, says when it's read that the grid isn't applied.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_prj_bound_by_a_grid_says_it_isnt_applied() {
        let prj = r#"BOUNDCRS[SOURCECRS[GEOGCRS["unknown",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898,LENGTHUNIT["metre",1]],ID["EPSG",6267]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]],CS[ellipsoidal,2],AXIS["longitude",east,ORDER[1],ANGLEUNIT["degree",0.0174532925199433,ID["EPSG",9122]]],AXIS["latitude",north,ORDER[2],ANGLEUNIT["degree",0.0174532925199433,ID["EPSG",9122]]]]],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["latitude",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["longitude",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4326]]],ABRIDGEDTRANSFORMATION["unknown to WGS84",METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","ntv2_0.gsb",ID["EPSG",8656]]]]"#;
        let dir = std::env::temp_dir().join(format!("rustgis-bound-grid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ds = crate::Dataset::new("nad27", crate::GeometryType::Point, vec![]);
        ds.push(Some(crate::Geometry::Point(Coord::new(-79.0, 43.0))), vec![]);
        let shp = dir.join("nad27.shp");
        crate::io::write_path(&ds, &shp).unwrap();
        std::fs::write(shp.with_extension("prj"), prj).unwrap();
        let (_, report) = crate::io::read_path(&shp).unwrap();
        let said = "the .prj file: the file's NTv2 transformation to WGS 84 (ntv2_0.gsb) isn't applied, so positions may be off by as much as that shift";
        assert!(report.messages.iter().any(|m| m == said), "{:?}", report.messages);
        assert!(!report.messages.iter().any(|m| m.contains("has no conversion to WGS 84 in the file")), "{:?}", report.messages);
        std::fs::remove_dir_all(&dir).ok();
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
        assert!(read_wkt(wkt).unwrap().1.unwrap().contains("PROJ 9.7 and earlier"));
    }

    /// An equidistant cylindrical WKT whose semi-major axis isn't WGS 84's, under WGS 84's datum
    /// name, is read with its own axis, as PROJ 9.8.1 reads it (pyproj 3.8.0:
    /// `Transformer.from_crs("EPSG:4326", wkt, always_xy=True).transform(10, 50)`), so there is no
    /// other axis to report. On an ellipsoid, the formula note stays.
    #[test]
    fn eqc_with_its_own_radius_is_read_with_it() {
        let rows = corpus();
        let wkt = rows.iter().find(|r| r.0 == "EPSG:4087").unwrap().2.replace("6378137", "6371000");
        let near_sphere = wkt.replace("298.257223563", "10000000000000");
        for (wkt, note, x, y) in [
            (&wkt, Some(ELLIPSOIDAL_EQC), 1_111_949.266_445_587_4, 5_534_646.951_385_601),
            (&near_sphere, None, 1_111_949.266_445_587_4, 5_559_746.332_227_188),
        ] {
            let (crs, notice) = read_wkt(wkt).unwrap();
            assert_eq!(notice.as_deref(), note);
            let p = crs.transformer().unwrap().from_lonlat(Coord::new(10.0, 50.0)).unwrap();
            assert!((p.x - x).hypot(p.y - y) < 1e-3, "{p:?}");
        }
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
    /// origin of 52 grads put the point 578 km north (#1692). PROJ identifies all three as
    /// EPSG:27572 and applies "NTF to WGS 84 (1)": `Transformer.from_crs(CRS(wkt), "EPSG:4326",
    /// always_xy=True).transform(549826.63, 2192990.71)` (pyproj 3.8.0, network off).
    #[test]
    fn wkt1_angles_are_in_the_geographic_systems_unit() {
        const GDAL: &str = r#"PROJCS["NTF (Paris) / Lambert zone II",GEOGCS["NTF (Paris)",DATUM["Nouvelle_Triangulation_Francaise_Paris",SPHEROID["Clarke 1880 (IGN)",6378249.2,293.466021293627,AUTHORITY["EPSG","7011"]],AUTHORITY["EPSG","6807"]],PRIMEM["Paris",2.33722917,AUTHORITY["EPSG","8903"]],UNIT["grad",0.0157079632679489,AUTHORITY["EPSG","9105"]],AUTHORITY["EPSG","4807"]],PROJECTION["Lambert_Conformal_Conic_1SP"],PARAMETER["latitude_of_origin",52],PARAMETER["central_meridian",0],PARAMETER["scale_factor",0.99987742],PARAMETER["false_easting",600000],PARAMETER["false_northing",2200000],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#;
        const ESRI: &str = r#"PROJCS["NTF_Paris_Lambert_Zone_II",GEOGCS["GCS_NTF_Paris",DATUM["Nouvelle_Triangulation_Francaise_(Paris)",SPHEROID["Clarke_1880_IGN",6378249.2,293.466021293627]],PRIMEM["Paris",2.33722917],UNIT["Grad",0.0157079632679489]],PROJECTION["Lambert_Conformal_Conic"],PARAMETER["False_Easting",600000.0],PARAMETER["False_Northing",2200000.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",52.0],PARAMETER["Scale_Factor",0.99987742],PARAMETER["Latitude_Of_Origin",52.0],UNIT["Meter",1.0]]"#;
        const WKT2: &str = r#"PROJCRS["NTF (Paris) / Lambert zone II",BASEGEOGCRS["NTF (Paris)",DATUM["Nouvelle Triangulation Francaise (Paris)",ELLIPSOID["Clarke 1880 (IGN)",6378249.2,293.466021293627,LENGTHUNIT["metre",1]]],PRIMEM["Paris",2.5969213,ANGLEUNIT["grad",0.0157079632679489]],ID["EPSG",4807]],CONVERSION["Lambert zone II",METHOD["Lambert Conic Conformal (1SP)",ID["EPSG",9801]],PARAMETER["Latitude of natural origin",52,ANGLEUNIT["grad",0.0157079632679489],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",0,ANGLEUNIT["grad",0.0157079632679489],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.99987742,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",600000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",2200000,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["easting (X)",east,ORDER[1],LENGTHUNIT["metre",1]],AXIS["northing (Y)",north,ORDER[2],LENGTHUNIT["metre",1]]]"#;
        let xy = Coord::new(549_826.63, 2_192_990.71);
        let ntf = Coord::new(1.679_999_997, 46.734_999_981);
        // The prime meridian is in that unit too, as PROJ reads it, but for Paris at 2.33722917,
        // the degrees GDAL and Esri write: Paris in grads (2.5969213, as WKT1 defines it) is the
        // same place, and another meridian at 2.33722917 is 2.33722917 grads, 0.233722917° west
        // of Paris. PROJ's positions for each, as above.
        let paris_in_grads = GDAL.replace(r#"PRIMEM["Paris",2.33722917"#, r#"PRIMEM["Paris",2.5969213"#);
        let another = GDAL.replace(r#"PRIMEM["Paris""#, r#"PRIMEM["Another""#);
        let west = Coord::new(1.446_268_029, 46.734_998_500);
        // A central meridian of 10 grads is 9° east of Paris's (PROJ 9.8.1 places the point there).
        let ten = GDAL.replace(r#"PARAMETER["central_meridian",0]"#, r#"PARAMETER["central_meridian",10]"#);
        let east = Coord::new(10.680_356_166, 46.735_042_705);
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

    /// A geographic system in grads (NTF (Paris), EPSG:4807, as GDAL writes it without its axes)
    /// has latitudes up to 100 grads, placed as PROJ 9.8.1 places them (pyproj 3.8.0, network
    /// off, `Transformer.from_crs(CRS(wkt), "EPSG:4326", always_xy=True)`): 95 grads is 85.5°N.
    /// Past 100 grads is no place.
    #[test]
    fn a_latitude_in_grads_is_checked_against_the_pole_in_grads() {
        let wkt = r#"GEOGCS["NTF (Paris)",DATUM["Nouvelle_Triangulation_Francaise_Paris",SPHEROID["Clarke 1880 (IGN)",6378249.2,293.466021293627,AUTHORITY["EPSG","7011"]],AUTHORITY["EPSG","6807"]],PRIMEM["Paris",2.33722917,AUTHORITY["EPSG","8903"]],UNIT["grad",0.0157079632679489,AUTHORITY["EPSG","9105"]]]"#;
        let t = read_wkt(wkt).unwrap().0.transformer().unwrap();
        for (lat, want) in [
            (95.0, Coord::new(3.231_474_345_668_997, 85.501_260_988_238_16)),
            (99.9, Coord::new(2.944_914_606_011_862_3, 89.911_525_507_638_56)),
        ] {
            let c = t.to_lonlat(Coord::new(1.0, lat)).unwrap_or_else(|| panic!("{lat} grads"));
            assert!((c.x - want.x).abs() < 1e-9 && (c.y - want.y).abs() < 1e-9, "{lat} grads: {c:?}");
        }
        assert!(t.to_lonlat(Coord::new(1.0, 100.5)).is_none());
        assert!((super::angle_unit_degrees(wkt) - 0.9).abs() < 1e-12);
        // Of a bound system's source, not its prime meridian's degrees.
        let bound = r#"BOUNDCRS[SOURCECRS[GEOGCRS["NTF (Paris)",DATUM["x",ELLIPSOID["Clarke 1880 (IGN)",6378249.2,293.466021293627]],PRIMEM["Paris",2.33722917,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["lon",east],AXIS["lat",north],ANGLEUNIT["grad",0.0157079632679489]]],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]],ABRIDGEDTRANSFORMATION["t",METHOD["Geocentric translations",ID["EPSG",9603]],PARAMETER["X-axis translation",-168],PARAMETER["Y-axis translation",-60],PARAMETER["Z-axis translation",320]]]"#;
        assert!((super::angle_unit_degrees(bound) - 0.9).abs() < 1e-12);
    }

    /// A WGS 84 code with a shift of its own along the axis through (0, 0), which moves nothing
    /// there: PROJ 9.8.1 applies no shift from WGS 84 to WGS 84 anywhere (pyproj 3.8.0,
    /// `Transformer.from_crs(CRS(wkt), "EPSG:4326", always_xy=True).transform(0, 45)` is (0, 45)),
    /// so it is WGS 84 longitude/latitude.
    #[test]
    fn a_wgs84_code_with_a_shift_of_its_own_is_read_as_proj_reads_it() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563],TOWGS84[100,0,0,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4326"]]"#;
        let (crs, _) = read_wkt(wkt).unwrap();
        let at = crs.transformer().unwrap().to_lonlat(Coord::new(0.0, 45.0)).unwrap();
        assert!(at.x.abs() < 1e-9 && (at.y - 45.0).abs() < 1e-9, "{at:?}");
    }

    /// A projection terrio-crs reads but whose parameters it refuses is said so, with the reason,
    /// not as an unsupported projection; one it doesn't read is unsupported, as before.
    #[test]
    fn a_refused_parameter_is_not_an_unsupported_projection() {
        let tm = r#"PROJCS["x",GEOGCS["x",DATUM["D_x",SPHEROID["S",6378206.4,294.978698213898]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",95],UNIT["metre",1]]"#;
        let e = read_wkt(tm).unwrap_err();
        assert!(e.starts_with("the coordinate system x can't be read: ") && e.contains("lat_0"), "{e}");
        let e = read_wkt(&tm.replace("Transverse_Mercator", "Foo_Bar")).unwrap_err();
        assert_eq!(e, "the Foo_Bar projection isn't supported");
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
        let p = proj4(&scaled).unwrap();
        assert!(p.contains("+k=0.9999 ") || p.contains("+k_0=0.9999 "), "{p}");
        // The false easting and northing add to the westing and southing at the natural origin
        // (EPSG's method 9808: a false westing and southing). With 1000 m and 2000 m, the origin,
        // 19°E on the equator, is at 1000 m, 2000 m, and the point 1000 m and 2000 m further on.
        // (PROJ 9.8.1 refuses this method with offsets other than 0.)
        let offset = GDAL
            .replace(r#""false_easting",0]"#, r#""false_easting",1000]"#)
            .replace(r#""false_northing",0]"#, r#""false_northing",2000]"#);
        assert!(offset.contains(r#""false_easting",1000]"#) && offset.contains(r#""false_northing",2000]"#), "{offset}");
        let offset_crs = crs_from_wkt(&offset).unwrap();
        // PROJ writes it no PROJ string; it is saved with the one RustGIS wrote before, for older
        // releases, which places the points alike.
        let legacy = Crs::parse(&offset_crs.custom_def().unwrap().proj4).unwrap();
        assert!(places_alike(offset_crs, legacy, lonlat), "{}", offset_crs.custom_def().unwrap().proj4);
        // In feet, the offsets are feet: PROJ takes `+x_0` and `+y_0` in metres. So in WKT2 with
        // a parameter's own unit.
        let feet = offset.replace(r#"UNIT["metre",1,AUTHORITY["EPSG","9001"]]]"#, r#"UNIT["foot",0.3048,AUTHORITY["EPSG","9002"]]]"#);
        let own_unit = WKT2
            .replace(r#""False easting",0,LENGTHUNIT["metre",1]"#, r#""False easting",1000,LENGTHUNIT["foot",0.3048]"#)
            .replace(r#""False northing",0,LENGTHUNIT["metre",1]"#, r#""False northing",2000,LENGTHUNIT["foot",0.3048]"#);
        for wkt in [&feet, &own_unit] {
            assert!(wkt.contains(r#"foot",0.3048"#), "{wkt}");
            let crs = crs_from_wkt(wkt).unwrap();
            let def = &crs.custom_def().unwrap().proj4;
            assert!(def.contains("+x_0=-304.8") && def.contains("+y_0=-609.6"), "{def}");
            assert!(places_alike(crs, Crs::parse(def).unwrap(), lonlat), "{def}");
        }
        let t = offset_crs.transformer().unwrap();
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
        let at =
            |wkt: &str, xy: Coord| crs_from_wkt(wkt).unwrap_or_else(|e| panic!("{e}: {wkt}")).transformer().unwrap().to_lonlat(xy).unwrap();
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
        // Web Mercator, Sinusoidal and equidistant cylindrical with axes pointing west are read,
        // as PROJ 9.8.1 places them (pyproj 3.8.0: `Transformer.from_crs("EPSG:4326", wkt,
        // always_xy=True).transform(10, 50)`).
        let web = ESRI_3857.replace(r#"UNIT["Meter",1.0]]"#, r#"UNIT["Meter",1.0],AXIS["X",WEST],AXIS["Y",NORTH]]"#);
        let sinusoidal = format!(
            r#"PROJCS["x",{GEOG_4326},PROJECTION["Sinusoidal"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],UNIT["Meter",1.0],AXIS["X",WEST],AXIS["Y",NORTH]]"#
        );
        let eqc = sinusoidal.replace(r#"PROJECTION["Sinusoidal"]"#, r#"PROJECTION["Equidistant_Cylindrical"]"#);
        for (wkt, x, y) in [
            (&web, -1_113_194.907_932_735_7, 6_446_275.841_017_161),
            (&sinusoidal, -716_957.536_160_03, 5_540_847.041_684_148),
            (&eqc, -1_113_194.907_932_735_7, 5_540_847.041_684_148),
        ] {
            let p = crs_from_wkt(wkt).unwrap().transformer().unwrap().from_lonlat(Coord::new(10.0, 50.0)).unwrap();
            assert!((p.x - x).hypot(p.y - y) < 1e-3, "{wkt}: {p:?}");
        }
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
        // Equidistant cylindrical on a sphere, pointing west: x is the arc west of the meridian.
        let sphere = eqc.replace(
            r#"DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]]"#,
            r#"DATUM["D_Sphere",SPHEROID["Sphere",6371000.0,0.0]]"#,
        );
        assert_ne!(sphere, eqc);
        let p = crs_from_wkt(&sphere).unwrap().transformer().unwrap().from_lonlat(Coord::new(10.0, 50.0)).unwrap();
        let (x, y) = (-6_371_000.0 * 10f64.to_radians(), 6_371_000.0 * 50f64.to_radians());
        assert!((p.x - x).hypot(p.y - y) < 1e-3, "{p:?}");
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
        // ED50 by its code, and by its datum's name (the reference is `placement_cases`', PROJ
        // 9.8.1's). Read as if to WGS 84, the point would be 82 m east.
        let ed50 = Coord::new(9.997_684_968_762_389, 49.998_866_814_066_59);
        let by_name = ED50_WKT2.replace(r#",ID["EPSG",4230]"#, "");
        for target in [ED50_WKT2, &by_name] {
            assert!(metres_apart(at(&bound_crs(BESSEL_WKT2, target, &BESSEL_SHIFT)), ed50) < 0.001, "{target}");
        }
        let to_wgs84 = at(&bound_crs(BESSEL_WKT2, WGS84_WKT2, &BESSEL_SHIFT));
        assert!(metres_apart(to_wgs84, ed50) > 80.0, "{to_wgs84:?}");
        // To OSGB 36: at 10°E, outside the area of OSGB 36's transformations, PROJ 9.8.1 goes on
        // to WGS 84 without one (pyproj 3.8.0, as for `placement_cases`).
        let osgb36 = Coord::new(9.998_813_925_294_643, 49.998_164_502_181_31);
        let ll = at(&bound_crs(BESSEL_WKT2, OSGB36_WKT2, &BESSEL_SHIFT));
        assert!(metres_apart(ll, osgb36) < 0.001, "{} m off", metres_apart(ll, osgb36));
        // A target with a shift of its own (ED50 with a `TOWGS84`) is a bound system itself, which
        // PROJ 9.8.1 can't go on through: it places the point where it is ("Ballpark geographic
        // offset from Local to WGS 84"), and so does terrio-crs (opengeos/terrio#1012, item 12,
        // will say so).
        let ed50_shifted = r#"GEOGCS["ED50",DATUM["European_Datum_1950",SPHEROID["International 1924",6378388,297],TOWGS84[-80,-90,-110,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4230"]]"#;
        let ll = at(&bound_crs(BESSEL_WKT2, ed50_shifted, &BESSEL_SHIFT));
        assert!(metres_apart(ll, Coord::new(10.0, 50.0)) < 0.001, "{ll:?}");
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
        // So is one whose EPSG code is bundled on GRS 1980 with a zero shift (as GDAL 3.13.3
        // writes it), as matching a file's shift to its code takes it: NAD83(2011), whose datum
        // isn't known by its name.
        let nad83_2011 = r#"GEOGCRS["NAD83(2011)",DATUM["NAD83 (National Spatial Reference System 2011)",ELLIPSOID["GRS 1980",6378137,298.257222101,LENGTHUNIT["metre",1]]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",6318]]"#;
        assert_eq!(epsg_proj4(6318), Some("+proj=longlat +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +no_defs"));
        let ll = at(&bound_crs(BESSEL_WKT2, nad83_2011, &BESSEL_SHIFT));
        assert!(metres_apart(ll, to_wgs84) < 0.001, "{ll:?}");
        // A three-parameter shift to WGS 84, as PROJ 9.8.1 writes it: seven parameters.
        let def = crs_from_wkt(&bound_crs(BESSEL_WKT2, WGS84_WKT2, &[1.0, 2.0, 3.0])).unwrap().custom_def().unwrap();
        assert!(def.proj4.contains("+towgs84=1,2,3,0,0,0,0 "), "{}", def.proj4);
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
        assert!(def.proj4.starts_with("+proj=tmerc +lat_0=49") && def.proj4.contains("+towgs84=375,-111,431,0,0,0,0 "), "{}", def.proj4);
        // A WKT1 source with a `TOWGS84` of its own: the bound transformation replaces it.
        let bng_wkt1 = bng_wkt1();
        assert!(bng_wkt1.contains("TOWGS84[446.448,"), "{bng_wkt1}");
        // PROJ 9.8.1 writes the inner one as its PROJ string, 4 m from where it places points; it is
        // saved, for older releases, with the bound one, as RustGIS read it before.
        let over_wkt1 = crs_from_wkt(&bound_crs(&bng_wkt1, WGS84_WKT2, &[375.0, -111.0, 431.0])).unwrap();
        let saved = &over_wkt1.custom_def().unwrap().proj4;
        assert!(saved.contains("+towgs84=375,-111,431,0,0,0,0 ") && !saved.contains("446.448"), "{saved}");
        let over_wkt2 = crs_from_wkt(&bound_crs(&bng, WGS84_WKT2, &[375.0, -111.0, 431.0])).unwrap();
        assert!(places_alike(over_wkt1, over_wkt2, Coord::new(-3.5, 55.38)));
        // Krovak North Orientated with either shift: the file's, where PROJ 9.8.1 places it
        // (pyproj 3.8.0, network off, `Transformer.from_crs("EPSG:4326", CRS(wkt),
        // always_xy=True).transform(15.4, 49.8)`), metres from EPSG:5514's own transformation.
        let krovak = format!(r#"{},ID["EPSG",5514]]"#, rows.iter().find(|r| r.0 == "EPSG:5514").unwrap().2.strip_suffix(']').unwrap());
        for (shift, at) in [([589.0, 76.0, 480.0], (-677_313.886, -1_083_840.369)), ([570.8, 85.7, 462.8], (-677_328.002, -1_083_838.950))]
        {
            let (crs, note) = read_wkt(&bound_crs(&krovak, WGS84_WKT2, &shift)).unwrap();
            assert!(matches!(crs, Crs::Custom(_)) && note.is_none(), "{crs:?} {note:?}");
            let p = crs.transformer().unwrap().from_lonlat(Coord::new(15.4, 49.8)).unwrap();
            assert!((p.x - at.0).abs() < 0.01 && (p.y - at.1).abs() < 0.01, "{shift:?}: {p:?}");
        }
        // A system that can't be read from its parameters (a method no one knows) is read by its
        // code, with the code's shift instead of the file's, said.
        let krovak = krovak.replace(r#"METHOD["Krovak (North Orientated)",ID["EPSG",1041]]"#, r#"METHOD["Unheard of"]"#);
        assert!(krovak.contains("Unheard of"));
        let (crs, note) = read_wkt(&bound_crs(&krovak, WGS84_WKT2, &[570.8, 85.7, 462.8])).unwrap();
        assert_eq!(crs, Crs::Epsg(5514));
        assert_eq!(
            note.as_deref(),
            Some("the file's own shift to WGS 84 is set aside for EPSG:5514's, because the Unheard of projection isn't supported")
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
                "the file's own shift to WGS 84 is set aside, because the Unheard of projection isn't supported, and EPSG:4267 is read without it, so positions may be off by as much as that shift"
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
        let wkt1 = bng_wkt1.clone();
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
        // The same shift rounded otherwise, as older GDAL releases wrote Hong Kong 1980's, is the
        // same. (GDAL 3 and RustGIS write its `.prj` without a shift.)
        let spheroid = r#"SPHEROID["International 1924",6378388,297,AUTHORITY["EPSG","7022"]]"#;
        let hk = to_wkt(Crs::Epsg(4611)).unwrap();
        assert!(hk.contains(spheroid) && !hk.contains("TOWGS84"), "{hk}");
        let bundled = "TOWGS84[-162.619,-276.959,-161.764,0.067753,-2.243649,-1.158827,-1.094246]";
        for shift in [bundled, "TOWGS84[-162.619,-276.959,-161.764,0.067753,-2.243648,-1.158828,-1.094246]"] {
            let with = hk.replace(spheroid, &format!("{spheroid},{shift}"));
            assert_eq!(crs_from_wkt(&with), Ok(Crs::Epsg(4611)), "{with}");
        }
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

    /// An EPSG code that comes with a WKT (a GeoPackage row's) is used as the WKT's own `AUTHORITY`
    /// would be (#1677): when the WKT gives no shift, or the code's own.
    #[test]
    fn a_code_beside_a_wkt_is_used_when_the_wkt_gives_no_other_shift() {
        let bng = bng_wkt1();
        let own = "TOWGS84[446.448,-125.157,542.06,0.15,0.247,0.842,-20.489]";
        assert!(bng.contains(own), "{bng}");
        // The code's own shift, or none: the code is used.
        assert_eq!(read_wkt_over_code(&bng, 27700), None);
        let unshifted = bng.replace(&format!(",{own}"), "");
        assert_ne!(unshifted, bng);
        assert_eq!(read_wkt_over_code(&unshifted, 27700), None);
        // The code stands in for the WKT's own, also one that names another system.
        let mislabelled = bng.replace(r#"AUTHORITY["EPSG","27700"]]"#, r#"AUTHORITY["EPSG","4277"]]"#);
        assert_ne!(mislabelled, bng);
        // Read from its parameters, as PROJ 9.8.1 reads it (no code; pyproj 3.8.0
        // `Transformer.from_crs(CRS(wkt), "EPSG:4326", always_xy=True).transform(400000, 300000)`
        // is (-2.001433115511, 52.597792430834)): not as the geographic system its root names.
        let (crs, _) = read_wkt(&mislabelled).unwrap();
        assert!(matches!(crs, Crs::Custom(_)), "{crs:?}");
        let ll = crs.transformer().unwrap().to_lonlat(Coord::new(400_000.0, 300_000.0)).unwrap();
        assert!((ll.x + 2.001_433_115_511).abs() < 1e-9 && (ll.y - 52.597_792_430_834).abs() < 1e-9, "{ll:?}");
        assert_eq!(read_wkt_over_code(&mislabelled, 27700), None);
        // A grid isn't a shift to compare: the code is used, and its caller says the grid isn't
        // applied ([`unapplied_note`]). Text that isn't WKT leaves the code too.
        assert_eq!(read_wkt_over_code(BOUND_BY_A_GRID, 26717), None);
        assert_eq!(read_wkt_over_code("PROJCS[", 27700), None);
    }

    /// A WKT that gives another shift than the code beside it, a `TOWGS84` or a `BOUNDCRS`'s, is
    /// read in place of the code, as a `.prj` is read in place of its own `AUTHORITY` (#1677).
    #[test]
    fn a_wkt_with_another_shift_is_read_in_place_of_the_code_beside_it() {
        let bng = bng_wkt1();
        let own = "TOWGS84[446.448,-125.157,542.06,0.15,0.247,0.842,-20.489]";
        assert!(bng.contains(own), "{bng}");
        // Another shift is the WKT's, as in a `.prj`.
        let other = bng.replace(own, "TOWGS84[375,-111,431,0,0,0,0]");
        let read = read_wkt_over_code(&other, 27700).unwrap();
        assert_eq!(read, read_wkt(&other));
        let (crs, note) = read.unwrap();
        assert!(crs.custom_def().is_some_and(|d| d.proj4.contains("+towgs84=375,-111,431,0,0,0,0")) && note.is_none(), "{crs:?} {note:?}");
        // So is a `BOUNDCRS`'s, of a WKT2 without a code of its own.
        let rows = corpus();
        let bng2 = rows.iter().find(|r| r.0 == "EPSG:27700").unwrap().2;
        assert!(!bng2.contains(r#"ID["EPSG",27700]"#), "{bng2}");
        let osgb = epsg_proj4(4277).and_then(earth_of).unwrap().1;
        assert_eq!(read_wkt_over_code(&bound_crs(bng2, WGS84_WKT2, &osgb), 27700), None);
        let (crs, note) = read_wkt_over_code(&bound_crs(bng2, WGS84_WKT2, &[375.0, -111.0, 431.0]), 27700).unwrap().unwrap();
        assert!(crs.custom_def().is_some_and(|d| d.proj4.contains("+towgs84=375,-111,431,0,0,0,0 ")) && note.is_none(), "{crs:?} {note:?}");
        // A shift of zero where the code has one is the WKT's, said.
        let zero = bng.replace(own, "TOWGS84[0,0,0,0,0,0,0]");
        let (crs, note) = read_wkt_over_code(&zero, 27700).unwrap().unwrap();
        assert!(matches!(crs, Crs::Custom(_)), "{crs:?}");
        assert_eq!(
            note.as_deref(),
            Some("the file gives a shift of zero to WGS 84, which is used in place of EPSG:27700's transformation")
        );
        // A system that can't be read from its parameters (a method no one knows) is read by the
        // code, with the code's shift in place of the WKT's, said.
        let krovak = rows.iter().find(|r| r.0 == "EPSG:5514").unwrap().2;
        let unheard = krovak.replace(r#"METHOD["Krovak (North Orientated)",ID["EPSG",1041]]"#, r#"METHOD["Unheard of"]"#);
        assert_ne!(unheard, krovak);
        let (crs, note) = read_wkt_over_code(&bound_crs(&unheard, WGS84_WKT2, &[570.8, 85.7, 462.8]), 5514).unwrap().unwrap();
        assert_eq!(crs, Crs::Epsg(5514));
        assert_eq!(
            note.as_deref(),
            Some("the file's own shift to WGS 84 is set aside for EPSG:5514's, because the Unheard of projection isn't supported")
        );
        // Krovak North Orientated itself is read with the file's shift, as PROJ 9.8.1 reads it.
        let (crs, note) = read_wkt_over_code(&bound_crs(krovak, WGS84_WKT2, &[570.8, 85.7, 462.8]), 5514).unwrap().unwrap();
        assert!(matches!(crs, Crs::Custom(_)) && note.is_none(), "{crs:?} {note:?}");
    }

    /// A `BOUNDCRS` that can't be read with its transformation leaves the code beside it, and says
    /// that the shift it gives is set aside (#1677). Alone, as in a `.prj`, the same text is
    /// refused. Text that can't be read and gives no transformation leaves the code with nothing
    /// to say.
    #[test]
    fn a_bound_wkt_that_cant_be_read_leaves_the_code_beside_it_and_says_so() {
        let rows = corpus();
        let wkt2 = |code: &str| rows.iter().find(|r| r.0 == code).unwrap().2;
        // A target whose shift to WGS 84 isn't known: NAD 27's is a grid.
        let nad27 = wkt2("EPSG:4267");
        let bound = bound_crs(wkt2("EPSG:27700"), nad27, &[375.0, -111.0, 431.0]);
        let why = "the shift from NAD27 to WGS 84 isn't known";
        assert_eq!(read_wkt(&bound), Err(why.to_string()));
        assert_eq!(
            read_wkt_over_code(&bound, 27700),
            Some(Ok((Crs::Epsg(27700), Some(format!("the file's own shift to WGS 84 is set aside for EPSG:27700's, because {why}")))))
        );
        // A code without a shift of its own (NAD 27's) is read without the text's.
        let lost = bound_crs(BESSEL_WKT2, nad27, &BESSEL_SHIFT);
        assert_eq!(
            read_wkt_over_code(&lost, 4267),
            Some(Ok((
                Crs::Epsg(4267),
                Some(format!(
                    "the file's own shift to WGS 84 is set aside, because {why}, and EPSG:4267 is read without it, so positions may be off by as much as that shift"
                ))
            )))
        );
        // The same systems bound to WGS 84 are read with their shift, with nothing to say.
        assert!(matches!(
            read_wkt_over_code(&bound_crs(wkt2("EPSG:27700"), WGS84_WKT2, &[375.0, -111.0, 431.0]), 27700),
            Some(Ok((_, None)))
        ));
        // So is a bound system that is a compound system's horizontal part.
        let compound = format!(r#"COMPOUNDCRS["with heights",{bound},VERTCRS["heights"]]"#);
        assert_eq!(read_wkt(&compound), Err(why.to_string()));
        assert!(matches!(read_wkt_over_code(&compound, 27700), Some(Ok((Crs::Epsg(27700), Some(note)))) if note.ends_with(why)));
        // Nothing to set aside: a bound system that gives no transformation, and text cut short,
        // which isn't WKT.
        let no_source = format!("BOUNDCRS[TARGETCRS[{WGS84_WKT2}]]");
        assert!(read_wkt(&no_source).is_err());
        assert_eq!(read_wkt_over_code(&no_source, 27700), None);
        let cut = bound.strip_suffix("]]").unwrap();
        assert_eq!(read_wkt_over_code(cut, 27700), None);
        // Nor is there in text that only holds a transformation, bound to no system: one alone, or
        // in a compound system without a horizontal part, which can't be read.
        let alone = r#"ABRIDGEDTRANSFORMATION["t",METHOD["Geocentric translations"],PARAMETER["X-axis translation",375]]"#;
        assert_eq!(read_wkt_over_code(alone, 27700), None);
        let unbound = format!(r#"COMPOUNDCRS["no horizontal part",{alone}]"#);
        assert_eq!(read_wkt(&unbound), Err("the compound coordinate system has no horizontal part".into()));
        assert_eq!(read_wkt_over_code(&unbound, 27700), None);
    }

    /// Another body's WKT is read, or refused, whatever the EPSG code beside it: EPSG's codes are
    /// the Earth's (#1677).
    #[test]
    fn another_bodys_wkt_is_read_in_place_of_the_code_beside_it() {
        let mars = r#"GEOGCS["Mars_2015_Ographic",DATUM["D_Mars_2015",SPHEROID["Mars_2015",3396190.0,169.894447223612]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert_eq!(read_wkt_over_code(mars, 4326), Some(Err(planetary::OGRAPHIC.to_string())));
        let ocentric = mars.replace("Mars_2015_Ographic", "Mars_2015_Ocentric").replace("169.894447223612", "0");
        assert!(matches!(read_wkt_over_code(&ocentric, 4326), Some(Ok((crs, Some(_)))) if crs.is_planetary()));
        // Longitudes counted west in WKT2, which can't be parsed into WKT1.
        let west = r#"GEOGCRS["Mars (2015) / Ographic",DATUM["Mars (2015)",ELLIPSOID["Mars (2015)",3396190,169.894447223612,LENGTHUNIT["metre",1]]],PRIMEM["Reference Meridian",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",west,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["IAU",49901,2015]]"#;
        assert!(tree(west).is_err());
        assert!(matches!(read_wkt_over_code(west, 4326), Some(Err(_))));
    }

    /// Web Mercator's codes with a shift of zero, as GDAL writes them, are Web Mercator. Read from
    /// their parameters, as Mercator on an ellipsoid, points would be 21 km off.
    #[test]
    fn projected_systems_refuse_present_unusable_units_and_radii() {
        for original in [ESRI_32631, ESRI_3857] {
            for (from, to) in [
                (r#"UNIT["Meter",1.0]]"#, r#"UNIT["Meter",1e400]]"#),
                (r#"UNIT["Meter",1.0]]"#, r#"UNIT["Meter"]]"#),
                ("6378137.0,298.257223563", "1e400,298.257223563"),
            ] {
                let malformed = original.replace(from, to);
                assert_ne!(malformed, original);
                assert!(crs_from_wkt(&malformed).is_err(), "{:?}", crs_from_wkt(&malformed));
            }
        }
        let wkt2 = include_str!("wkt2_corpus.tsv")
            .lines()
            .find(|l| l.contains("Popular Visualisation Pseudo Mercator"))
            .and_then(|l| l.rsplit('\t').next())
            .unwrap();
        let mut parsed = parse(wkt2).unwrap();
        // Remove the projected authority: this checks its supplied definition, not trusted EPSG.
        parsed.items.retain(|i| !matches!(i, Item::Node(n) if n.name.eq_ignore_ascii_case("ID")));
        let unit = cs_unit(&parsed, &LENGTH_UNIT).unwrap();
        let title = unit.title().to_string();
        for numbers in [vec![], vec![Item::Num(f64::INFINITY)]] {
            let mut malformed = parsed.clone();
            for item in &mut malformed.items {
                if let Item::Node(n) = item {
                    if n.name.eq_ignore_ascii_case("LENGTHUNIT") {
                        *n = Node::new("LENGTHUNIT", &title, numbers.clone());
                    } else if n.name.eq_ignore_ascii_case("AXIS") {
                        for nested in &mut n.items {
                            if let Item::Node(unit) = nested
                                && unit.name.eq_ignore_ascii_case("LENGTHUNIT")
                            {
                                *unit = Node::new("LENGTHUNIT", &title, numbers.clone());
                            }
                        }
                    }
                }
            }
            let e = projected(&projected2(&malformed).unwrap(), "").unwrap_err();
            assert!(e.contains("unit or datum radius that isn't a finite number"), "{e}");
        }
    }

    /// Nodes are taken out with the comma before them, in one pass however many there are: a
    /// `.prj` past terrio-crs's size cap with many `TOWGS84` nodes doesn't stall the read.
    #[test]
    fn nodes_are_taken_out_in_one_pass() {
        assert_eq!(without_nodes("A[1,TOWGS84[1,2,3], B[2], towgs84[4]]", "TOWGS84["), Some(("A[1, B[2]]".to_string(), true)));
        assert_eq!(without_nodes("A[1]", "TOWGS84["), Some(("A[1]".to_string(), false)));
        assert_eq!(without_nodes("A[1,TOWGS84[1,2", "TOWGS84["), None);
        let many = format!("BOUNDCRS[{}]", ",TOWGS84[0,0,0]".repeat(200_000));
        assert_eq!(without_nodes(&many, "TOWGS84["), Some(("BOUNDCRS[]".to_string(), true)));
    }

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
        // Another shift: GDAL's `PROJ4` extension (`+nadgrids=@null` on Web Mercator's sphere) is
        // what PROJ 9.8.1 reads in place of the WKT, so it stays Web Mercator (pyproj 3.8.0 places
        // it as EPSG:3857); without the extension, the shift is the file's own.
        let shifted = google.replace("TOWGS84[0,0,0,0,0,0,0]", "TOWGS84[1,2,3,0,0,0,0]");
        assert_eq!(crs_from_wkt(&shifted), Ok(Crs::WebMercator));
        let shifted = without_proj4_extension(&shifted).unwrap();
        assert!(matches!(crs_from_wkt(&shifted), Ok(Crs::Custom(_))), "{:?}", crs_from_wkt(&shifted));
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
            (r#"UNIT["Meter",1.0]]"#, r#"UNIT["Meter",1e400]]"#),
            (r#"UNIT["Meter",1.0]]"#, r#"UNIT["Meter"]]"#),
            ("6378137.0,298.257223563", "1e400,298.257223563"),
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
        // PROJ 9.8.1's string for it (pyproj 3.8.0 `CRS(wkt).to_proj4()`), position-vector rotations.
        assert!(def.proj4.contains("+towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.48"), "{}", def.proj4);
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
        // As PROJ 9.8.1 writes it (`-20.4889999999569`, the scale difference in ppm unrounded).
        assert!(def.proj4.contains("+towgs84=446.448,-125.157,542.06,-0.15,-0.247,-0.842,-20.4889999999569 "), "{}", def.proj4);
        // Every coordinate frame method, by its EPSG code, turns the other way; position vector
        // methods don't.
        // The time-dependent ones (1056, 1057) without rates are PROJ's coordinate frame shift, and
        // PROJ 9.8.1 writes no PROJ string for them; the time-specific one (1066) without an epoch
        // PROJ refuses ("Input is not a transformation").
        let method = |code: u32| {
            wkt.replace(r#""Position Vector transformation (geog2D domain)",ID["EPSG",9606]"#, &format!(r#""m",ID["EPSG",{code}]"#))
        };
        let frame_place = crs_from_wkt(&frame).unwrap();
        for (code, sign) in FRAME_METHODS.iter().map(|c| (*c, "-")).chain([(9606, ""), (1033, ""), (1037, "")]) {
            // RustGIS reads it as before, as the coordinate frame shift, the epoch left out.
            if code == 1066 {
                let crs = crs_from_wkt(&method(code)).unwrap();
                assert!(places_alike(crs, frame_place, Coord::new(-0.128_7, 51.504)), "{code}");
                // So with space before the brackets and keywords in small letters, as the parser
                // allows.
                let spaced = method(code).replace("ABRIDGEDTRANSFORMATION[", "abridgedTransformation [\n ").replace("METHOD[", "Method [");
                let crs = crs_from_wkt(&spaced).unwrap_or_else(|e| panic!("{e}"));
                assert!(places_alike(crs, frame_place, Coord::new(-0.128_7, 51.504)), "{spaced}");
                continue;
            }
            let crs = crs_from_wkt(&method(code)).unwrap();
            let def = crs.custom_def().unwrap();
            // PROJ writes no PROJ string for these: saved for older releases with the coordinate
            // frame shift's, as RustGIS read them before.
            if matches!(code, 1056 | 1057) {
                assert!(def.proj4.contains(",-0.15,-0.247,-0.842,"), "{code}: {}", def.proj4);
                assert!(places_alike(crs, frame_place, Coord::new(-0.128_7, 51.504)), "{code}");
                continue;
            }
            // The full rotation matrix: PROJ places Big Ben a few 1e-10° from the frame shift, but its
            // `+towgs84` is the small-angle form, so no PROJ string is kept.
            if matches!(code, 1133 | 1140) {
                assert!(def.proj4.contains(",-0.15,-0.247,-0.842,"), "{code}: {}", def.proj4);
                let ll = crs.transformer().unwrap().to_lonlat(Coord::new(530_000.0, 180_000.0)).unwrap();
                assert!((ll.x + 0.128_717_983_262_614_4).abs() < 1e-11 && (ll.y - 51.504_128_007_185_58).abs() < 1e-11, "{code}: {ll:?}");
                continue;
            }
            assert!(def.proj4.contains(&format!(",{sign}0.15,{sign}0.247,{sign}0.842,")), "{code}: {}", def.proj4);
        }
        // A three-parameter shift, in a unit of its own.
        let three = r#"BOUNDCRS[SOURCECRS[GEOGCRS["NAD27",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898]],PRIMEM["Greenwich",0],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]],ABRIDGEDTRANSFORMATION["NAD27 to WGS 84",METHOD["Geocentric translations (geog2D domain)",ID["EPSG",9603]],PARAMETER["X-axis translation",-8,ID["EPSG",8605]],PARAMETER["Y-axis translation",16000,LENGTHUNIT["centimetre",0.01],ID["EPSG",8606]],PARAMETER["Z-axis translation",176,ID["EPSG",8607]]]]"#;
        // A unitless scale is a factor, as PROJ 9.8.1 reads it; parts per million by their unit.
        // (PROJ 9.8.1 writes 7 for 1.000007, terrio-crs 7.00000000009027.)
        for (scale, ppm) in [
            ("1.000007", 7.0),
            (r#"1.000007,SCALEUNIT["unity",1]"#, 7.0),
            (r#"7,SCALEUNIT["parts per million",1E-06]"#, 7.0),
            ("0.995", -5000.0),
            ("1.0", 0.0),
        ] {
            let seven = wkt.replace("0.999979511", scale);
            let def = crs_from_wkt(&seven).unwrap().custom_def().unwrap();
            let written = def.proj4.split_once("0.842,").and_then(|(_, r)| r.split(' ').next()?.parse::<f64>().ok());
            assert!(written.is_some_and(|w| (w - ppm).abs() < 1e-6), "{scale}: {}", def.proj4);
        }
        // Rotations without a scale, or the reverse: the missing ones are none, as PROJ 9.8.1
        // places them, which writes no PROJ string for them; saved, for older releases, with
        // them as 0, as RustGIS read them before.
        let big_ben = Coord::new(-0.128_35, 51.504);
        for (missing, zero) in [
            (r#",PARAMETER["Scale difference",0.999979511,ID["EPSG",8611]]"#, wkt.replace("0.999979511", "1.0")),
            (
                r#",PARAMETER["Y-axis rotation",0.247,ID["EPSG",8609]]"#,
                wkt.replace(r#"PARAMETER["Y-axis rotation",0.247"#, r#"PARAMETER["Y-axis rotation",0"#),
            ),
        ] {
            let crs = crs_from_wkt(&wkt.replace(missing, "")).unwrap();
            let saved = Crs::parse(&crs.custom_def().unwrap().proj4).unwrap();
            assert!(places_alike(crs, crs_from_wkt(&zero).unwrap(), big_ben), "{missing}");
            assert!(places_alike(crs, saved, big_ben), "{missing}: {}", crs.custom_def().unwrap().proj4);
        }
        let (crs, note) = read_wkt(three).unwrap();
        assert!(
            note.is_none() && crs.custom_def().is_some_and(|d| d.proj4.contains("+towgs84=-8,160,176,0,0,0,0 ")),
            "{:?}",
            crs.custom_def()
        );
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
    fn hyperbolic_cassini_gets_no_wkt1() {
        // The switch on, bare or written out, has no WKT 1 name; off, it is the ordinary Cassini.
        for on in ["+hyperbolic", "+hyperbolic=true", "+hyperbolic=1"] {
            assert_eq!(proj4_to_wkt(&format!("+proj=cass +lat_0=10 +lon_0=170 +ellps=WGS84 {on} +units=m"), "x"), None, "{on}");
        }
        for off in ["", "+hyperbolic=false"] {
            let wkt = proj4_to_wkt(&format!("+proj=cass +lat_0=10 +lon_0=170 +ellps=WGS84 {off} +units=m"), "x");
            assert!(wkt.is_some_and(|w| w.contains("Cassini_Soldner")), "{off}");
        }
    }

    #[test]
    fn every_system_east_and_north_gets_a_prj() {
        // Each EPSG code terrio-crs reads is written, and read back by its code, except a
        // geocentric system, a grid shift, axes other than east and north (Krovak pointing south
        // and west, EPSG:5513 and 2065, checked below), NAD27 Michigan (6200-6202, 6966) and
        // Modified Krovak (5224, 5225), Colombia Urban (6244–6275), hyperbolic Cassini (3139) and
        // the others checked below, which WKT 1 can't hold, and Laborde (EPSG:8441), which WKT
        // isn't read or written in yet. NAD27's are written too (as GDAL does, without a shift).
        let mut written = 0;
        for code in 1..=u16::MAX {
            let Some(def) = epsg_proj4(code) else { continue };
            if earth_params(def).is_none()
                || ["+proj=geocent", "+nadgrids", "+axis=", "+proj=labrd", "+proj=mod_krovak", "+proj=col_urban", "+hyperbolic"]
                    .iter()
                    .any(|k| def.contains(k))
                || matches!(
                    code,
                    5513 | 2065
                        | 6200
                        | 6201
                        | 6202
                        | 6966
                        | 6989
                        | 9013
                        | 10635
                        | 10735
                        | 32600
                        | 32700
                        | 3993
                        | 9549
                        | 10258
                        | 10262
                        | 10266
                )
            {
                continue;
            }
            let crs = Crs::from_epsg(u32::from(code)).unwrap();
            let wkt = to_wkt(crs).unwrap_or_else(|| panic!("EPSG:{code}: {def}"));
            // A compound code RustGIS's own text doesn't place as its transformer does (EPSG:7410,
            // 7400, 7954) is written as GDAL writes it, a `COMPD_CS`, read as its horizontal part.
            match crs_from_wkt(&wkt) {
                Ok(back) if back == crs => {}
                Ok(back) if wkt.starts_with("COMPD_CS[") => {
                    let horizontal = format!(r#"AUTHORITY["EPSG","{}"]],VERT_CS["#, back.epsg());
                    assert!(wkt.contains(&horizontal), "EPSG:{code}: {back:?}: {wkt}");
                }
                back => panic!("EPSG:{code}: {back:?}: {wkt}"),
            }
            // Beside its code, as the GeoPackage writer puts it, the code is used.
            assert_eq!(read_wkt_over_code(&wkt, u32::from(code)), None, "{wkt}");
            written += 1;
        }
        assert!(written > 5000, "{written}");
        assert!(to_wkt(Crs::from_epsg(26717).unwrap()).unwrap().contains(r#"DATUM["North_American_Datum_1927",SPHEROID["Clarke 1866""#));
        // Krovak pointing south and west (EPSG:5513, 2065 on Ferro) as GDAL 3.10.3 writes it
        // (`gdalsrsinfo -o wkt1`), read back by its code.
        for code in [5513, 2065] {
            let crs = Crs::from_epsg(code).unwrap();
            let wkt = to_wkt(crs).unwrap_or_else(|| panic!("EPSG:{code}"));
            assert!(wkt.starts_with(r#"PROJCS["S-JTSK"#), "{wkt}");
            assert_eq!(crs_from_wkt(&wkt), Ok(crs), "{wkt}");
        }
        // NAD27 Michigan (Lambert Conic Conformal (2SP Michigan)), Modified Krovak (EPSG:5224,
        // 5225), Colombia Urban (6247), hyperbolic Cassini (3139), the Guam projection (3993),
        // Lambert Conic Conformal (1SP variant B) (9549, 10258, 10262, 10266: as the plain 1SP,
        // 89–111 km off), geographic 3D systems whose 2D text isn't placed as the code because of
        // their shift to WGS 84 (6989, 9013, 10635, 10735; TWD97, EPSG:3823, is written as its 2D
        // system) and the UTM grid systems (32600, 32700): no WKT 1. GDAL 3.13.3 writes none either.
        for code in
            [6200, 6201, 6202, 6966, 5224, 5225, 6247, 3139, 3993, 9549, 10258, 10262, 10266, 6989, 9013, 10635, 10735, 32600, 32700]
        {
            assert_eq!(to_wkt(Crs::from_epsg(code).unwrap()), None, "EPSG:{code}");
        }
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
            (27572, (2.35, 48.86), "PRIMEM[\"Paris\",2.337229"),
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
            // A definition naming its datum (`+datum=potsdam`) takes that datum's transformations,
            // as PROJ 9.8.1 does, where the WKT read back keeps the datum's TOWGS84: about 0.6 m
            // apart in Berlin.
            let named = epsg_proj4(code).is_some_and(|d| d.contains("+datum=") && !d.contains("+datum=WGS84"));
            let tolerance = match (crs.is_geographic(), named) {
                (true, _) => 1e-8,
                (false, true) => 1.0,
                (false, false) => 0.01,
            };
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
            // Deprecated, with its own definition as PROJ keeps it (no longer 29101's).
            (29100, (-54.0, -10.0), (5_000_000.000, 8_894_141.426)),
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
        // Mars's sphere, however the PROJ string writes it.
        let def = crs.custom_def().unwrap().proj4.clone();
        assert!(def.starts_with("+proj=longlat ") && planetary::proj4_axes(&def) == Some((3_396_190.0, 3_396_190.0)), "{def}");
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

    /// #1866: a name from the file is cut short in every notice and error it is put in. A notice
    /// over 16 KiB isn't shown at all (`diagnostics::redact` omits it whole), so a name left whole
    /// would hide what the notice says; cut, the notice keeps it.
    #[test]
    fn a_long_name_from_the_file_is_cut_short_in_what_is_said() {
        use crate::raster::planetary;
        let long = "x".repeat(20_000);
        let said = |m: &str, ends: &str| {
            let head: String = m.chars().take(200).collect();
            assert!(m.chars().count() < 400 && m.contains('…') && m.ends_with(ends), "{head}");
            assert!(!crate::diagnostics::Redacted::new(m).as_str().contains("omitted"), "{head}");
        };
        // A datum without a shift to WGS 84.
        let datum = format!(
            r#"GEOGCS["g",DATUM["{long}",SPHEROID["Clarke 1880",6378249.145,293.465]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#
        );
        said(&read_wkt(&datum).unwrap().1.unwrap(), "positions may be off by up to a few hundred metres");
        // A bound system's target whose shift isn't known, and one whose zero shift may be a
        // placeholder.
        let unknown = BESSEL_WKT2.replace("Local", &long);
        said(&crs_from_wkt(&bound_crs(BESSEL_WKT2, &unknown, &BESSEL_SHIFT)).unwrap_err(), "to WGS 84 isn't known");
        let ed50_zero = format!(
            r#"GEOGCS["{long}",DATUM["European_Datum_1950",SPHEROID["International 1924",6378388,297],TOWGS84[0,0,0,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#
        );
        said(&crs_from_wkt(&bound_crs(BESSEL_WKT2, &ed50_zero, &BESSEL_SHIFT)).unwrap_err(), "where its datum has one");
        // An ellipsoid without its axis, a projection, an angle unit and a kind of system this
        // doesn't read.
        let no_axis = format!(
            r#"GEOGCRS["g",DATUM["d",ELLIPSOID["{long}"]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]"#
        );
        said(&read_wkt(&no_axis).unwrap_err(), "has no semi-major axis");
        let projection = format!(
            r#"PROJCS["p",GEOGCS["g",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["{long}"],UNIT["metre",1]]"#
        );
        said(&read_wkt(&projection).unwrap_err(), "projection isn't supported");
        // A world projection's name with a long run of spaces in it is still read as its method,
        // which refuses a scale factor and a parameter that isn't a finite number.
        let natural_earth =
            projection.replace(&format!(r#"PROJECTION["{long}"]"#), &format!(r#"PROJECTION["Natural{}Earth"]"#, " ".repeat(20_000)));
        assert_ne!(natural_earth, projection);
        for parameter in [r#"PARAMETER["scale_factor",0.5]"#, r#"PARAMETER["central_meridian",1e400]"#] {
            let refused = natural_earth.replace(r#",UNIT["metre",1]]"#, &format!(r#",{parameter},UNIT["metre",1]]"#));
            assert_ne!(refused, natural_earth);
            said(&read_wkt(&refused).unwrap_err(), "isn't supported");
        }
        let kind = format!(
            r#"GEOGCRS["g",DATUM["d",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[{long},2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433]]"#
        );
        said(&read_wkt(&kind).unwrap_err(), "coordinate systems aren't supported");
        said(&read_wkt(&format!(r#"{long}["g"]"#)).unwrap_err(), "coordinate systems aren't supported");
        // Text that can't be parsed.
        said(&read_wkt(&long).unwrap_err(), "…");
        said(&read_wkt(&format!("{long}[?]")).unwrap_err(), "…");
        said(&read_wkt(&format!(r#"{long}["a" "b"]"#)).unwrap_err(), "…");
        said(&read_wkt(&format!(r#"GEOGCS["g",DATUM["d",SPHEROID["s",1{}]]]"#, "e".repeat(20_000))).unwrap_err(), "isn't a number");
        // Another body's system: read, refused, kept by none, and exported. Its radius is this
        // test's own: a custom system is shared by its definition, so one another test reads
        // first would keep that test's name.
        let mars = format!(
            r#"GEOGCS["{long}",DATUM["D_Mars",SPHEROID["Mars",3396190.1866,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#
        );
        let (crs, note) = read_wkt(&mars).unwrap();
        said(&note.unwrap(), "as if on the Earth");
        said(&planetary::refused(&long, planetary::PROJECTED), "it would land in the wrong place");
        said(&planetary::code_note(&long), "as if on the Earth");
        said(&planetary::export_error(crs, Crs::WebMercator).unwrap(), "or as longitude/latitude");
    }
}
