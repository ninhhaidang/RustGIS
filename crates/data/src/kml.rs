//! KML and KMZ: placemarks with their names, descriptions, extended data, folders and styles.
//!
//! A document becomes one layer per kind of geometry it holds (points, lines, polygons). Each
//! feature keeps its folder path and the id of its style; the styles (line and fill colours,
//! widths, icon colours) are returned beside the layer so the map can draw them. Ground overlays
//! (georeferenced images) are counted in a note, not drawn.
//!
//! The file is hostile: no DTD is read (so no entity expansion), the node count is capped, nesting
//! is bounded, and a malformed placemark is skipped and counted.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use roxmltree::{Node, ParsingOptions};
use rustgis_geom::Coord;

use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

/// Most extended-data columns (distinct names) a document gets.
const MAX_KEYS: usize = 256;
/// Deepest nesting of folders and multi-geometries followed.
const MAX_DEPTH: usize = 64;

/// How a KML style draws: colours as RGBA.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    /// The line (and polygon outline) colour.
    pub stroke: Option<[u8; 4]>,
    pub width: Option<f32>,
    /// The polygon fill; `None` when the style says not to fill.
    pub fill: Option<[u8; 4]>,
    /// Whether polygons are filled (`<fill>0</fill>` turns it off).
    pub filled: bool,
    /// Whether polygons are outlined (`<outline>0</outline>` turns it off).
    pub outlined: bool,
    /// The point icon's colour tint.
    pub icon: Option<[u8; 4]>,
    pub scale: Option<f32>,
}

/// A layer read from a KML document: its features, what was skipped, and the styles its features'
/// `style` field names.
pub struct Layer {
    pub dataset: Dataset,
    pub report: ReadReport,
    pub styles: Vec<(String, Style)>,
}

/// Reads a KML document, or the main document of a KMZ (`doc.kml`, else the first `.kml` in it).
pub fn read(file_name: &str, bytes: &[u8]) -> Result<Vec<Layer>> {
    let path = std::path::Path::new(file_name);
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("layer").to_string();
    let is_kmz =
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("kmz")) || bytes.starts_with(b"PK\x03\x04");
    let owned;
    let text_bytes: &[u8] = if is_kmz {
        // Inflated within the size every computer reads (a KML or KMZ is read as it is added, on
        // the UI thread in the desktop app, so it stays there, #2408).
        let budget = usize::try_from(crate::io::BYTES_FLOOR).unwrap_or(usize::MAX);
        let mut entries = crate::zip::read_matching_within(bytes, |n| n.to_ascii_lowercase().ends_with(".kml"), budget)
            .map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
        // doc.kml by convention, else the shallowest .kml.
        entries.sort_by_key(|(n, _)| (!n.eq_ignore_ascii_case("doc.kml"), n.matches('/').count(), n.clone()));
        owned = entries
            .into_iter()
            .next()
            .map(|(_, b)| b)
            .ok_or_else(|| DataError::Format(format!("{file_name}: no .kml document in the archive")))?;
        &owned
    } else {
        bytes
    };
    let text = std::str::from_utf8(text_bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")))?;
    let text = text.trim_start_matches('\u{feff}');
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
    let doc = crate::xml::parse(text, opts).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    let mut r = Reader {
        styles: HashMap::new(),
        maps: HashMap::new(),
        inline: HashMap::new(),
        dropped_keys: 0,
        duplicate_entries: 0,
        duplicate_placemarks: 0,
        rows: Vec::new(),
        keys: Vec::new(),
        skipped: 0,
        geometry_losses: GeometryLosses::default(),
    };
    r.collect_styles(doc.root());
    r.walk(doc.root(), &mut Vec::new(), 0);
    Ok(r.layers(&name))
}

/// A ground overlay: a picture laid over a longitude/latitude box.
#[derive(Clone, Debug, PartialEq)]
pub struct GroundOverlay {
    pub name: String,
    /// The picture's address, as the document gives it (a path inside a KMZ, a file beside the
    /// KML, or a web address).
    pub href: String,
    /// West, south, east, north.
    pub bbox: [f64; 4],
    /// Degrees counter-clockwise about the box's centre.
    pub rotation: f64,
}

/// A document's ground overlays: those it lays out (with a KMZ's pictures; other documents name
/// theirs by `href` only), how many can't be drawn (a `gx:LatLonQuad`, no picture, a bad box), and
/// how many of a KMZ's are left out because their pictures' copies would pass the archive's budget.
pub type Overlays = (Vec<(GroundOverlay, Option<Vec<u8>>)>, usize, usize);

/// The ground overlays a document lays out ([`Overlays`]).
pub fn ground_overlays(file_name: &str, bytes: &[u8]) -> Result<Overlays> {
    // As [`read`] inflates it: within the size every computer reads.
    ground_overlays_within(file_name, bytes, usize::try_from(crate::io::BYTES_FLOOR).unwrap_or(usize::MAX))
}

/// [`ground_overlays`] with a KMZ expanded, and its pictures copied, within `budget` bytes.
fn ground_overlays_within(file_name: &str, bytes: &[u8], budget: usize) -> Result<Overlays> {
    let is_kmz = std::path::Path::new(file_name).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("kmz"))
        || bytes.starts_with(b"PK\x03\x04");
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let owned;
    let (doc_bytes, doc_dir): (&[u8], String) = if is_kmz {
        entries = crate::zip::read_matching_within(bytes, |_| true, budget).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
        let mut docs: Vec<&(String, Vec<u8>)> = entries.iter().filter(|(n, _)| n.to_ascii_lowercase().ends_with(".kml")).collect();
        docs.sort_by_key(|(n, _)| (!n.eq_ignore_ascii_case("doc.kml"), n.matches('/').count(), n.clone()));
        let (n, b) = docs.first().ok_or_else(|| DataError::Format(format!("{file_name}: no .kml document in the archive")))?;
        owned = b.clone();
        (&owned, n.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default())
    } else {
        (bytes, String::new())
    };
    let text = std::str::from_utf8(doc_bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")))?;
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
    let doc = crate::xml::parse(text.trim_start_matches('\u{feff}'), opts).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    let mut out = Vec::new();
    let mut undrawn = 0usize;
    // The pictures handed back are copies, one for each overlay naming one: together within the
    // budget the archive was expanded within, less the archive and the document's copy (held
    // while they are copied), so many overlays of one large picture aren't many copies of it. Past that an
    // overlay is left out, and counted.
    let held: usize = entries.iter().map(|(_, b)| b.len()).sum::<usize>() + if is_kmz { doc_bytes.len() } else { 0 };
    let mut pictures_left = budget.saturating_sub(held);
    let mut over_budget = 0usize;
    let all = doc.descendants().filter(|n| n.is_element() && local(n) == "GroundOverlay").count();
    // Past the first 256, overlays are counted as not drawn.
    undrawn += all.saturating_sub(256);
    for g in doc.descendants().filter(|n| n.is_element() && local(n) == "GroundOverlay").take(256) {
        let href = g.descendants().find(|n| n.is_element() && local(n) == "Icon").and_then(|i| child_text(&i, "href"));
        let bbox = g.descendants().find(|n| n.is_element() && local(n) == "LatLonBox").and_then(|b| {
            let v = |k: &str| child_text(&b, k).and_then(|t| t.trim().parse::<f64>().ok()).filter(|x| x.is_finite());
            let (n, s, e, w) = (v("north")?, v("south")?, v("east")?, v("west")?);
            Some(([w, s, e, n], v("rotation").unwrap_or(0.0)))
        });
        let (Some(href), Some(([w, s, e, n], rotation))) = (href, bbox) else {
            undrawn += 1;
            continue;
        };
        if !(w < e
            && s < n
            && (-180.0..=180.0).contains(&w)
            && (-180.0..=180.0).contains(&e)
            && (-90.0..=90.0).contains(&s)
            && (-90.0..=90.0).contains(&n))
        {
            undrawn += 1;
            continue;
        }
        // A KMZ's picture, by its path relative to the document.
        let found = is_kmz.then(|| {
            let want = format!("{doc_dir}{}", href.trim_start_matches("./"));
            entries
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(&want) || n.eq_ignore_ascii_case(href.trim_start_matches("./")))
                .map(|(_, b)| b)
        });
        if let Some(Some(b)) = found {
            let Some(left) = pictures_left.checked_sub(b.len()) else {
                over_budget += 1;
                continue;
            };
            pictures_left = left;
        }
        let picture = found.map(|b| b.cloned());
        let name = child_text(&g, "name").unwrap_or_else(|| "Ground overlay".into());
        out.push((GroundOverlay { name, href, bbox: [w, s, e, n], rotation }, picture.flatten()));
    }
    Ok((out, undrawn, over_budget))
}

/// One placemark, before it's put in a layer.
struct Row {
    geometry: Geometry,
    name: Option<String>,
    description: Option<String>,
    folder: String,
    style: Option<String>,
    data: Vec<(String, String)>,
    /// Its geometry's coordinates had altitudes, which are dropped.
    zm: bool,
}

struct Reader {
    styles: HashMap<String, Style>,
    /// StyleMap id → the style its "normal" pair names.
    maps: HashMap<String, String>,
    /// Inline styles, given keys `#inline-1`, `#inline-2`… (equal ones share one; no document id
    /// holds a `#`, so they can't clash).
    inline: HashMap<String, usize>,
    rows: Vec<Row>,
    /// Extended data keys, in the order first seen.
    keys: Vec<String>,
    /// Extended data entries past the column cap.
    dropped_keys: usize,
    duplicate_entries: usize,
    duplicate_placemarks: usize,
    skipped: usize,
    geometry_losses: GeometryLosses,
}

fn local<'a>(n: &Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

fn child<'a, 'i>(n: &Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && local(c) == name)
}

/// The text and CDATA children joined, keeping description whitespace as data.
fn raw_text_of(n: &Node) -> String {
    n.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect()
}

/// The text of an element (its text and CDATA children joined), trimmed.
fn text_of(n: &Node) -> String {
    raw_text_of(n).trim().to_string()
}

fn data_text(n: &Node, key: &str) -> String {
    if key == "description" { raw_text_of(n) } else { text_of(n) }
}

fn child_text(n: &Node, name: &str) -> Option<String> {
    child(n, name).map(|c| text_of(&c)).filter(|t| !t.is_empty())
}

/// A KML colour, `aabbggrr` in hex, as RGBA.
fn color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(s.get(..8)?, 16).ok()?;
    let [a, b, g, r] = v.to_be_bytes();
    Some([r, g, b, a])
}

fn style_of(n: &Node) -> Style {
    let mut s = Style { filled: true, outlined: true, ..Style::default() };
    if let Some(l) = child(n, "LineStyle") {
        s.stroke = child_text(&l, "color").and_then(|c| color(&c));
        s.width = child_text(&l, "width").and_then(|w| w.parse::<f32>().ok()).filter(|w| w.is_finite()).map(|w| w.clamp(0.0, 50.0));
    }
    if let Some(p) = child(n, "PolyStyle") {
        s.fill = child_text(&p, "color").and_then(|c| color(&c));
        s.filled = child_text(&p, "fill").is_none_or(|f| f != "0");
        s.outlined = child_text(&p, "outline").is_none_or(|f| f != "0");
    }
    if let Some(i) = child(n, "IconStyle") {
        s.icon = child_text(&i, "color").and_then(|c| color(&c));
        s.scale = child_text(&i, "scale").and_then(|w| w.parse::<f32>().ok()).filter(|w| w.is_finite()).map(|w| w.clamp(0.1, 10.0));
    }
    s
}

/// Coordinates: `lon,lat[,alt]` tuples separated by white space; malformed ones are dropped.
/// `unreadable` counts rejected tuples. `zm` is set when a tuple kept had an altitude other than 0 (KML's default), which is dropped.
fn coords(s: &str, zm: &Cell<bool>, unreadable: &Cell<usize>) -> Vec<Coord> {
    s.split_whitespace()
        .filter_map(|t| {
            let parsed = (|| {
                let mut it = t.split(',');
                let x = it.next()?.trim().parse::<f64>().ok()?;
                let y = it.next()?.trim().parse::<f64>().ok()?;
                if !crate::io::lon_lat_in_range(x, y) {
                    return None;
                }
                if it.next().and_then(|z| z.trim().parse::<f64>().ok()).is_some_and(|z| z != 0.0) {
                    zm.set(true);
                }
                Some(Coord::new(x, y))
            })();
            if parsed.is_none() {
                unreadable.set(unreadable.get() + 1);
            }
            parsed
        })
        .collect()
}

/// Whether reading keeps a placemark that [`crate::interop::to_kml`] writes with geometry `g`:
/// one with a point in range ([`coords`]), a line part of two such points, or a polygon whose
/// outer ring has three ([`geometry`]). It is read into the layer of `g`'s kind.
pub(crate) fn keeps(g: &Geometry) -> bool {
    let kept = |c: &&Coord| crate::io::lon_lat_in_range(c.x, c.y);
    match g {
        Geometry::Point(c) => kept(&c),
        Geometry::MultiPoint(v) => v.iter().any(|c| kept(&c)),
        Geometry::Line(parts) => parts.iter().any(|p| p.iter().filter(kept).count() >= 2),
        Geometry::Polygon(polys) => polys.iter().any(|rings| rings.first().is_some_and(|r| r.iter().filter(kept).count() >= 3)),
    }
}

/// The vertices of a placemark reading keeps ([`keeps`]) that don't read back with it: those
/// outside longitude/latitude ([`coords`]), and every vertex of a line part left with fewer than
/// two, a ring with fewer than three, or a polygon whose outer ring is dropped ([`geometry`]).
pub(crate) fn lost_vertices(g: &Geometry) -> usize {
    let in_range = |p: &[Coord]| p.iter().filter(|c| crate::io::lon_lat_in_range(c.x, c.y)).count();
    // A part reading keeps loses its vertices out of range; one it drops, all of them.
    let part = |p: &[Coord], min: usize| {
        let n = in_range(p);
        if n >= min { p.len().saturating_sub(n) } else { p.len() }
    };
    match g {
        Geometry::Point(_) => 0,
        Geometry::MultiPoint(v) => v.len().saturating_sub(in_range(v)),
        Geometry::Line(parts) => parts.iter().map(|p| part(p, 2)).sum(),
        Geometry::Polygon(polys) => polys
            .iter()
            .map(|rings| match rings.split_first() {
                Some((outer, inner)) if in_range(outer) >= 3 => part(outer, 3) + inner.iter().map(|r| part(r, 3)).sum::<usize>(),
                _ => rings.iter().map(Vec::len).sum(),
            })
            .sum(),
    }
}

/// Discards counted at their parsing seam, aggregated once per document.
#[derive(Default)]
struct GeometryLosses {
    unreadable: Cell<usize>,
    holes: usize,
    short_lines: usize,
    excess_points: usize,
    polygons: usize,
}

impl GeometryLosses {
    /// Adds a placemark's losses without repeating the document's notices per layer.
    fn add(&mut self, other: &Self) {
        self.unreadable.set(self.unreadable.get() + other.unreadable.get());
        self.holes += other.holes;
        self.short_lines += other.short_lines;
        self.excess_points += other.excess_points;
        self.polygons += other.polygons;
    }

    /// Emits one counted notice per kind, only for nonzero totals.
    fn report(&self, name: &str, report: &mut ReadReport) {
        use crate::loss::LossCode::{DroppedGeometry, DroppedRing};
        // Each is a part left out of a kept placemark's geometry.
        for (count, one, many, code) in [
            (self.unreadable.get(), "unreadable KML coordinate tuple", "unreadable KML coordinate tuples", DroppedGeometry),
            (self.holes, "unreadable KML polygon hole", "unreadable KML polygon holes", DroppedRing),
            (
                self.short_lines,
                "KML line part with fewer than two usable coordinates",
                "KML line parts with fewer than two usable coordinates",
                DroppedGeometry,
            ),
            (self.excess_points, "excess KML Point coordinate tuple", "excess KML Point coordinate tuples", DroppedGeometry),
            (self.polygons, "KML polygon without a usable outer ring", "KML polygons without a usable outer ring", DroppedGeometry),
        ] {
            if count > 0 {
                let (subject, verb) = if count == 1 { (one, "was") } else { (many, "were") };
                report.messages.push(crate::loss::Note::loss(code, format!("{name}: {count} {subject} {verb} left out")));
            }
        }
    }
}

/// The parts of a geometry element: (points, lines, polygons), MultiGeometry flattened.
#[derive(Default)]
struct Parts {
    points: Vec<Coord>,
    lines: Vec<Vec<Coord>>,
    polygons: Vec<Vec<Vec<Coord>>>,
    /// An altitude other than 0 was dropped from the points, lines or polygons.
    zm: [Cell<bool>; 3],
    losses: GeometryLosses,
}

/// Adds a geometry element's parts to `out` (MultiGeometry flattened, to `MAX_DEPTH`), marking
/// altitudes and counting discarded coordinate tuples, holes and line parts.
fn geometry(n: &Node, out: &mut Parts, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match local(n) {
        // Altitudes count only for the parts kept.
        "Point" => {
            // Keep the first usable tuple and its altitude; count each tuple discarded.
            let mut first = None;
            if let Some(c) = child_text(n, "coordinates") {
                for t in c.split_whitespace() {
                    let alt = Cell::new(false);
                    if let Some(p) = coords(t, &alt, &out.losses.unreadable).into_iter().next() {
                        if first.is_none() {
                            first = Some((p, alt.get()));
                        } else {
                            out.losses.excess_points += 1;
                        }
                    }
                }
            }
            if let Some((p, alt)) = first {
                out.zm[0].set(out.zm[0].get() || alt);
                out.points.push(p);
            }
        }
        "LineString" | "LinearRing" => {
            let alt = Cell::new(false);
            let text = child_text(n, "coordinates");
            let c = text.as_deref().map(|c| coords(c, &alt, &out.losses.unreadable)).unwrap_or_default();
            if c.len() >= 2 {
                out.zm[1].set(out.zm[1].get() || alt.get());
                out.lines.push(c);
            } else if text.is_some() {
                out.losses.short_lines += 1;
            }
        }
        "Polygon" => {
            let ring = |b: Node| {
                let text = child(&b, "LinearRing").and_then(|r| child_text(&r, "coordinates"))?;
                let alt = Cell::new(false);
                Some((coords(&text, &alt, &out.losses.unreadable), alt.get()))
            };
            let outer = child(n, "outerBoundaryIs").and_then(ring);
            let mut has_coordinates = outer.is_some();
            let mut kept = outer.filter(|(r, _)| r.len() >= 3).map(|(r, alt)| (vec![r], alt));
            // Read every hole even if the outer ring failed. Valid holes lost with their
            // polygon are covered by the polygon count, not called unreadable holes.
            for inner in n.children().filter(|c| c.is_element() && local(c) == "innerBoundaryIs") {
                if let Some((r, a)) = ring(inner) {
                    has_coordinates = true;
                    if r.len() < 3 {
                        out.losses.holes += 1;
                    } else if let Some((rings, alt)) = &mut kept {
                        *alt |= a;
                        rings.push(r);
                    }
                }
            }
            if let Some((rings, alt)) = kept {
                out.zm[2].set(out.zm[2].get() || alt);
                out.polygons.push(rings);
            } else if has_coordinates {
                out.losses.polygons += 1;
            }
        }
        // A track: gx:coord elements of "lon lat alt".
        "Track" => {
            let mut alt = false;
            let mut has_coordinates = false;
            let mut c = Vec::new();
            for node in n.children().filter(|c| c.is_element() && local(c) == "coord") {
                let t = text_of(&node);
                if t.is_empty() {
                    continue;
                }
                has_coordinates = true;
                let parsed = (|| {
                    let mut it = t.split_whitespace();
                    let x = it.next()?.parse::<f64>().ok()?;
                    let y = it.next()?.parse::<f64>().ok()?;
                    // A track keeps any finite coordinate, without the longitude/latitude range check.
                    if !(x.is_finite() && y.is_finite()) {
                        return None;
                    }
                    alt |= it.next().and_then(|z| z.parse::<f64>().ok()).is_some_and(|z| z != 0.0);
                    Some(Coord::new(x, y))
                })();
                if let Some(p) = parsed {
                    c.push(p);
                } else {
                    out.losses.unreadable.set(out.losses.unreadable.get() + 1);
                }
            }
            if c.len() >= 2 {
                out.zm[1].set(out.zm[1].get() || alt);
                out.lines.push(c);
            } else if has_coordinates {
                out.losses.short_lines += 1;
            }
        }
        "MultiGeometry" | "MultiTrack" => {
            for c in n.children().filter(|c| c.is_element()) {
                geometry(&c, out, depth + 1);
            }
        }
        _ => {}
    }
}

impl Reader {
    /// Styles and style maps anywhere in the document, by id.
    fn collect_styles(&mut self, root: Node) {
        for n in root.descendants().filter(|n| n.is_element()) {
            let Some(id) = n.attribute("id") else { continue };
            match local(&n) {
                "Style" => {
                    self.styles.insert(id.to_string(), style_of(&n));
                }
                "StyleMap" => {
                    let normal = n
                        .children()
                        .filter(|p| p.is_element() && local(p) == "Pair")
                        .find(|p| child_text(p, "key").as_deref() == Some("normal"));
                    if let Some(url) = normal.and_then(|p| child_text(&p, "styleUrl")) {
                        self.maps.insert(id.to_string(), url.trim_start_matches('#').to_string());
                    } else if let Some(s) = normal.and_then(|p| child(&p, "Style")) {
                        // A key no document id can be (ids can't hold '#').
                        let key = format!("#{id}-normal");
                        self.styles.insert(key.clone(), style_of(&s));
                        self.maps.insert(id.to_string(), key);
                    }
                }
                _ => {}
            }
        }
    }

    /// The style id a placemark draws with: its inline style, else its styleUrl (through a style
    /// map). Only ids defined in this document count.
    fn style_for(&mut self, pm: &Node) -> Option<String> {
        if let Some(s) = child(pm, "Style") {
            let s = style_of(&s);
            // Equal styles share a key, found by their description (not a scan of every style).
            let sig = format!("{s:?}");
            let n = self.inline.len();
            let i = *self.inline.entry(sig).or_insert(n);
            // A key no document id can be (ids can't hold '#').
            let id = format!("#inline-{}", i + 1);
            self.styles.insert(id.clone(), s);
            return Some(id);
        }
        let url = child_text(pm, "styleUrl")?;
        // Only references within the document (`#id`); a style in another file isn't read.
        let id = url.strip_prefix('#')?.to_string();
        let mut id = id;
        // A style map points to a style (bounded, in case maps point at each other).
        for _ in 0..4 {
            match self.maps.get(&id) {
                Some(next) => id = next.clone(),
                None => break,
            }
        }
        self.styles.contains_key(&id).then_some(id)
    }

    fn walk(&mut self, n: Node, folders: &mut Vec<String>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for c in n.children().filter(|c| c.is_element()) {
            match local(&c) {
                "Folder" | "Document" => {
                    let named = local(&c) == "Folder";
                    if named {
                        folders.push(child_text(&c, "name").unwrap_or_default());
                    }
                    self.walk(c, folders, depth + 1);
                    if named {
                        folders.pop();
                    }
                }
                "Placemark" => self.placemark(&c, folders),
                // Pictures, read by `ground_overlays`.
                "GroundOverlay" => {}
                "kml" => self.walk(c, folders, depth + 1),
                _ => {}
            }
        }
    }

    fn placemark(&mut self, pm: &Node, folders: &[String]) {
        let mut parts = Parts::default();
        for g in pm.children().filter(|c| c.is_element()) {
            geometry(&g, &mut parts, 0);
        }
        // A placemark left out whole is said as one; only a kept one's parts left out are its
        // geometry's loss.
        let losses = std::mem::take(&mut parts.losses);
        let mut data = Vec::new();
        let mut seen = HashSet::new();
        let mut duplicates = 0usize;
        if let Some(ext) = child(pm, "ExtendedData") {
            for d in ext.descendants().filter(|d| d.is_element()) {
                let pair = match local(&d) {
                    "Data" => {
                        d.attribute("name").map(|k| (k.to_string(), child(&d, "value").map(|v| data_text(&v, k)).unwrap_or_default()))
                    }
                    "SimpleData" => d.attribute("name").map(|k| (k.to_string(), data_text(&d, k))),
                    _ => None,
                };
                if let Some((k, v)) = pair.filter(|(k, _)| !k.trim().is_empty()) {
                    // The attribute reader uses the first entry for a key. Count and discard
                    // later entries here, including duplicates across Data and SimpleData and
                    // names past the column cap.
                    if !seen.insert(k.clone()) {
                        duplicates += 1;
                        continue;
                    }
                    if !self.keys.contains(&k) {
                        // At most MAX_KEYS columns of extended data; the rest are dropped.
                        if self.keys.len() >= MAX_KEYS {
                            self.dropped_keys += 1;
                            continue;
                        }
                        self.keys.push(k.clone());
                    }
                    data.push((k, v));
                }
            }
        }
        let style = self.style_for(pm);
        let folder = folders.iter().filter(|f| !f.is_empty()).cloned().collect::<Vec<_>>().join(" / ");
        let name = child_text(pm, "name");
        let description = child(pm, "description").map(|n| raw_text_of(&n));
        // A placemark mixing kinds (a MultiGeometry of a point and a line) gives one feature per
        // kind, each in its layer.
        let mut any = false;
        let mut push = |geometry: Geometry, zm: &Cell<bool>, rows: &mut Vec<Row>| {
            any = true;
            rows.push(Row {
                geometry,
                name: name.clone(),
                description: description.clone(),
                folder: folder.clone(),
                style: style.clone(),
                data: data.clone(),
                zm: zm.get(),
            });
        };
        match parts.points.as_slice() {
            [] => {}
            [p] => push(Geometry::Point(*p), &parts.zm[0], &mut self.rows),
            _ => push(Geometry::MultiPoint(parts.points), &parts.zm[0], &mut self.rows),
        }
        if !parts.lines.is_empty() {
            push(Geometry::Line(parts.lines), &parts.zm[1], &mut self.rows);
        }
        if !parts.polygons.is_empty() {
            push(Geometry::Polygon(parts.polygons), &parts.zm[2], &mut self.rows);
        }
        if any {
            self.geometry_losses.add(&losses);
        }
        if !any {
            self.skipped += 1;
        } else if duplicates > 0 {
            self.duplicate_entries += duplicates;
            self.duplicate_placemarks += 1;
        }
    }

    /// The layers: one per kind of geometry, in points, lines, polygons order.
    fn layers(self, name: &str) -> Vec<Layer> {
        let kinds = [(GeometryType::Point, "points"), (GeometryType::Polyline, "lines"), (GeometryType::Polygon, "polygons")];
        let present: Vec<_> = kinds.iter().filter(|(k, _)| self.rows.iter().any(|r| r.geometry.kind() == *k)).collect();
        let mut fields = vec![
            Field::new("name", FieldType::Text),
            Field::new("description", FieldType::Text),
            Field::new("folder", FieldType::Text),
            Field::new("style", FieldType::Text),
        ];
        // Each extended data key's column, as (key, column name). A key named like one of those,
        // ignoring case, is left out when it only repeats that column (an exported name field
        // does), else kept under a free name.
        let mut taken: Vec<Field> =
            fields.iter().cloned().chain(self.keys.iter().map(|k| Field::new(k.clone(), FieldType::Text))).collect();
        let mut keys: Vec<(String, String)> = Vec::new();
        let mut renamed: Vec<(String, String)> = Vec::new();
        for k in &self.keys {
            let Some(i) = fields.iter().position(|f| f.name.eq_ignore_ascii_case(k)) else {
                keys.push((k.clone(), k.clone()));
                continue;
            };
            let repeats = |r: &Row| {
                let fixed = match i {
                    0 => r.name.as_deref(),
                    1 => r.description.as_deref(),
                    2 => Some(r.folder.as_str()),
                    _ => r.style.as_deref(),
                };
                r.data.iter().find(|(dk, _)| dk == k).is_none_or(|(_, v)| v == fixed.unwrap_or_default())
            };
            if (i != 1 || k == "description") && self.rows.iter().all(repeats) {
                continue;
            }
            let column = crate::io::free_column_name(&taken, k);
            taken.push(Field::new(column.clone(), FieldType::Text));
            renamed.push((k.clone(), column.clone()));
            keys.push((k.clone(), column));
        }
        fields.extend(keys.iter().map(|(_, c)| Field::new(c.clone(), FieldType::Text)));
        let mut out = Vec::new();
        for (i, (kind, label)) in present.iter().enumerate() {
            let title = if present.len() == 1 { name.to_string() } else { format!("{name} {label}") };
            let mut ds = Dataset::new(title, *kind, fields.clone());
            let mut used: Vec<String> = Vec::new();
            let mut zm_rows = 0usize;
            for r in self.rows.iter().filter(|r| r.geometry.kind() == *kind) {
                let mut attrs = vec![
                    r.name.clone().map(Value::Text).unwrap_or_default(),
                    r.description.clone().map(Value::Text).unwrap_or_default(),
                    if r.folder.is_empty() { Value::Null } else { Value::Text(r.folder.clone()) },
                    r.style.clone().map(Value::Text).unwrap_or_default(),
                ];
                attrs.extend(
                    keys.iter()
                        .map(|(k, _)| r.data.iter().find(|(dk, _)| dk == k).map(|(_, v)| Value::Text(v.clone())).unwrap_or_default()),
                );
                if let Some(s) = &r.style
                    && !used.contains(s)
                {
                    used.push(s.clone());
                }
                ds.push_next(Some(r.geometry.clone()), attrs);
                zm_rows += usize::from(r.zm);
            }
            // Extended data is typed; names, descriptions, folders and style ids stay text.
            crate::io::type_text_columns(&mut ds, &["name", "description", "folder", "style"]);
            let mut report = ReadReport::default();
            // The notes go with the first layer only.
            if i == 0 {
                self.geometry_losses.report(name, &mut report);
                report.skipped = self.skipped;
                if self.skipped > 0 {
                    report.messages.push(crate::loss::Note::loss(
                        crate::loss::LossCode::DroppedFeature,
                        format!("{name}: {} placemarks without a geometry RustGIS draws were skipped", self.skipped),
                    ));
                }
                if self.duplicate_entries > 0 {
                    report.messages.push(crate::loss::Note::loss(
                        crate::loss::LossCode::DroppedValue,
                        format!(
                            "{name}: {} repeated ExtendedData entries in {} placemarks were left out",
                            self.duplicate_entries, self.duplicate_placemarks
                        ),
                    ));
                }
                if self.dropped_keys > 0 {
                    report.messages.push(crate::loss::Note::loss(
                        crate::loss::LossCode::DroppedOverLimit,
                        format!("{name}: extended data past {MAX_KEYS} distinct names was left out"),
                    ));
                }
                if !renamed.is_empty() {
                    let listed: Vec<String> = renamed
                        .iter()
                        .take(5)
                        .map(|(k, c)| format!("'{}' → '{}'", crate::io::dbf_notice_name(k), crate::io::dbf_notice_name(c)))
                        .collect();
                    let more = if renamed.len() > 5 { format!(" and {} more", renamed.len() - 5) } else { String::new() };
                    let count = if renamed.len() == 1 {
                        "1 extended data name was".to_string()
                    } else {
                        format!("{} extended data names were", renamed.len())
                    };
                    report.messages.push(format!(
                        "{name}: {count} renamed, as RustGIS gives every placemark a name, description, folder and style column: {}{more}",
                        listed.join(", ")
                    ));
                }
            }
            if zm_rows > 0 {
                ds.zm_dropped = true;
                report.messages.push(crate::loss::Note::loss(
                    crate::loss::LossCode::DroppedZm,
                    format!("{}: {}", ds.name, crate::zm_note(Some(zm_rows as u64))),
                ));
            }
            let styles = used.into_iter().filter_map(|id| self.styles.get(&id).map(|s| (id, *s))).collect();
            out.push(Layer { dataset: ds, report, styles });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retained point makes discarded sibling geometry observable in both KML containers.
    fn mixed_geometry_layers(geometry: &str, ext: &str) -> Vec<Layer> {
        let xml = format!(
            r#"<kml xmlns:gx="http://www.google.com/kml/ext/2.2"><Placemark><name>kept</name><MultiGeometry><Point><coordinates>1,2</coordinates></Point>{geometry}</MultiGeometry></Placemark></kml>"#
        );
        let bytes = if ext == "kmz" { crate::zip::write(&[("doc.kml".into(), xml.into_bytes())]).unwrap() } else { xml.into_bytes() };
        read(&format!("a.{ext}"), &bytes).unwrap()
    }

    #[test]
    fn xml_loss_polygon_without_outer_ring_counts_its_rings() {
        let geometry = r#"<Polygon><outerBoundaryIs><LinearRing><coordinates>0,0,8 bad</coordinates></LinearRing></outerBoundaryIs><innerBoundaryIs><LinearRing><coordinates>1,1,99 2,1 2,2</coordinates></LinearRing></innerBoundaryIs><innerBoundaryIs><LinearRing><coordinates>bad</coordinates></LinearRing></innerBoundaryIs></Polygon><Polygon><innerBoundaryIs><LinearRing><coordinates>1,1 2,1 2,2</coordinates></LinearRing></innerBoundaryIs></Polygon>"#;
        for ext in ["kml", "kmz"] {
            let layers = mixed_geometry_layers(geometry, ext);
            assert_eq!(layers.len(), 1);
            assert_eq!(layers[0].dataset.features.len(), 1);
            assert_eq!(layers[0].dataset.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 2.0))));
            assert_eq!(layers[0].dataset.features[0].attrs[0], Value::Text("kept".into()));
            assert!(!layers[0].dataset.zm_dropped, "discarded rings do not set a retained point's altitude flag");
            assert_eq!(
                layers[0].report.messages.texts().collect::<Vec<_>>(),
                [
                    "a: 2 unreadable KML coordinate tuples were left out",
                    "a: 1 unreadable KML polygon hole was left out",
                    "a: 2 KML polygons without a usable outer ring were left out"
                ]
            );
            use crate::loss::LossCode::{DroppedGeometry, DroppedRing};
            let codes: Vec<_> = layers[0].report.messages.iter().map(|m| m.loss).collect();
            assert_eq!(codes, [Some(DroppedGeometry), Some(DroppedRing), Some(DroppedGeometry)]);
            for clean in [
                "<Polygon/>",
                "<Polygon><outerBoundaryIs><LinearRing><coordinates> </coordinates></LinearRing></outerBoundaryIs><innerBoundaryIs/><innerBoundaryIs><LinearRing><coordinates> </coordinates></LinearRing></innerBoundaryIs></Polygon>",
                "<Polygon><outerBoundaryIs><LinearRing><coordinates>0,0 4,0 4,4 0,0</coordinates></LinearRing></outerBoundaryIs><innerBoundaryIs><LinearRing><coordinates>1,1 2,1 2,2</coordinates></LinearRing></innerBoundaryIs></Polygon>",
            ] {
                assert!(mixed_geometry_layers(clean, ext).iter().all(|l| l.report.messages.is_empty()), "{clean}");
            }
        }
    }

    #[test]
    fn xml_loss_track_counts_nonempty_bad_coordinates() {
        let geometry = "<gx:Track><gx:coord>0 0</gx:coord><gx:coord>bad 1</gx:coord><gx:coord>2 2</gx:coord></gx:Track><gx:Track><gx:coord>3 3 99</gx:coord><gx:coord> </gx:coord></gx:Track><gx:Track><gx:coord>bad</gx:coord></gx:Track>";
        for ext in ["kml", "kmz"] {
            let layers = mixed_geometry_layers(geometry, ext);
            assert_eq!(layers.len(), 2);
            assert_eq!(layers[0].dataset.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 2.0))));
            assert_eq!(
                layers[1].dataset.features[0].geometry,
                Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(2.0, 2.0)]]))
            );
            assert!(layers.iter().all(|l| !l.dataset.zm_dropped && l.dataset.features[0].attrs[0] == Value::Text("kept".into())));
            assert_eq!(
                layers[0].report.messages.texts().collect::<Vec<_>>(),
                [
                    "a: 2 unreadable KML coordinate tuples were left out",
                    "a: 2 KML line parts with fewer than two usable coordinates were left out"
                ]
            );
            assert!(layers[1].report.messages.is_empty());
            let clean = mixed_geometry_layers(
                "<gx:Track/><gx:Track><gx:coord> </gx:coord><gx:coord/></gx:Track><gx:Track><gx:coord>200 100</gx:coord><gx:coord>201 101</gx:coord></gx:Track>",
                ext,
            );
            assert!(
                clean.iter().all(|l| l.report.messages.is_empty()),
                "empty tracks stay quiet; finite track coordinates keep their existing range policy"
            );
            assert_eq!(
                clean[1].dataset.features[0].geometry,
                Some(Geometry::Line(vec![vec![Coord::new(200.0, 100.0), Coord::new(201.0, 101.0)]]))
            );
        }
    }

    /// A placemark left out whole is said once, as one: the parts of it that couldn't be read
    /// aren't also a kept placemark's geometry loss.
    #[test]
    fn a_placemark_left_out_whole_is_said_once() {
        let layers = read(
            "a.kml",
            br#"<kml><Placemark><LineString><coordinates>x,y 1,2</coordinates></LineString></Placemark><Placemark><Point><coordinates>1,2</coordinates></Point></Placemark></kml>"#,
        )
        .unwrap();
        let said: Vec<_> = layers.iter().flat_map(|l| l.report.messages.iter().map(|m| (m.as_str(), m.loss))).collect();
        assert_eq!(said, [("a: 1 placemarks without a geometry RustGIS draws were skipped", Some(crate::loss::LossCode::DroppedFeature))]);
    }

    #[test]
    fn xml_loss_partial_geometry_losses_are_counted() {
        let xml = br#"<kml><Placemark><name>kept</name><MultiGeometry>
<Point><coordinates>1,2 3,4 5,6</coordinates></Point>
<LineString><coordinates>0,0 bad 2,2</coordinates></LineString>
<LineString><coordinates>8,8</coordinates></LineString>
<Polygon><outerBoundaryIs><LinearRing><coordinates>0,0 4,0 4,4 0,0</coordinates></LinearRing></outerBoundaryIs>
<innerBoundaryIs><LinearRing><coordinates>1,1 x,y</coordinates></LinearRing></innerBoundaryIs>
</Polygon></MultiGeometry></Placemark></kml>"#;
        for ext in ["kml", "kmz"] {
            let bytes = if ext == "kmz" { crate::zip::write(&[("doc.kml".into(), xml.to_vec())]).unwrap() } else { xml.to_vec() };
            let layers = read(&format!("a.{ext}"), &bytes).unwrap();
            assert_eq!(layers.len(), 3);
            assert_eq!(layers[0].dataset.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 2.0))));
            assert_eq!(
                layers[1].dataset.features[0].geometry,
                Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(2.0, 2.0)]]))
            );
            assert_eq!(
                layers[2].dataset.features[0].geometry,
                Some(Geometry::Polygon(vec![vec![vec![
                    Coord::new(0.0, 0.0),
                    Coord::new(4.0, 0.0),
                    Coord::new(4.0, 4.0),
                    Coord::new(0.0, 0.0)
                ]]]))
            );
            assert!(layers.iter().all(|l| l.dataset.features[0].attrs[0] == Value::Text("kept".into())));
            let messages: Vec<_> = layers.iter().flat_map(|l| l.report.messages.iter().map(|m| m.as_str())).collect();
            assert_eq!(
                messages,
                [
                    "a: 2 unreadable KML coordinate tuples were left out",
                    "a: 1 unreadable KML polygon hole was left out",
                    "a: 1 KML line part with fewer than two usable coordinates was left out",
                    "a: 2 excess KML Point coordinate tuples were left out"
                ]
            );
            use crate::loss::LossCode::{DroppedGeometry, DroppedRing};
            let codes: Vec<_> = layers.iter().flat_map(|l| l.report.messages.iter().map(|m| m.loss)).collect();
            assert_eq!(codes, [Some(DroppedGeometry), Some(DroppedRing), Some(DroppedGeometry), Some(DroppedGeometry)]);
        }
        let clean = read("a.kml", br#"<kml><Placemark><MultiGeometry><Point><coordinates>1,2</coordinates></Point><LineString/><LineString><coordinates> </coordinates></LineString><Polygon><outerBoundaryIs><LinearRing><coordinates>0,0 4,0 4,4 0,0</coordinates></LinearRing></outerBoundaryIs><innerBoundaryIs/><innerBoundaryIs><LinearRing><coordinates> </coordinates></LinearRing></innerBoundaryIs></Polygon></MultiGeometry></Placemark></kml>"#).unwrap();
        assert!(clean[0].report.messages.is_empty());
    }

    /// A KMZ's overlays of one picture are copied within the archive's budget; those past it are
    /// left out and counted apart from those that can't be drawn.
    #[test]
    fn overlay_pictures_are_copied_within_the_budget() {
        let overlay = r#"<GroundOverlay><Icon><href>p.png</href></Icon><LatLonBox><north>1</north><south>0</south><east>1</east><west>0</west></LatLonBox></GroundOverlay>"#;
        let doc = format!(r#"<kml xmlns="http://www.opengis.net/kml/2.2"><Document>{}</Document></kml>"#, overlay.repeat(5));
        let kmz = crate::zip::write(&[("doc.kml".to_string(), doc.clone().into_bytes()), ("p.png".to_string(), vec![7; 1000])]).unwrap();
        // Room for the archive, the document's copy and two and a half copies of the picture:
        // two are made.
        let max = (2 * doc.len() + 1000 + 2500) as u64;
        let (out, undrawn, over_budget) = ground_overlays_within("a.kmz", &kmz, max as usize).unwrap();
        assert_eq!((out.len(), undrawn, over_budget), (2, 0, 3));
        assert!(out.iter().all(|(_, p)| p.as_deref().is_some_and(|p| p.len() == 1000)));
        let (out, _, over_budget) = ground_overlays("a.kmz", &kmz).unwrap();
        assert_eq!((out.len(), over_budget), (5, 0));
    }

    const DOC: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2" xmlns:gx="http://www.google.com/kml/ext/2.2">
<Document><name>Trip</name>
  <Style id="red"><LineStyle><color>ff0000ff</color><width>3</width></LineStyle><PolyStyle><color>7f00ff00</color><outline>0</outline></PolyStyle></Style>
  <Style id="pin-n"><IconStyle><color>ffff0000</color><scale>1.5</scale></IconStyle></Style>
  <StyleMap id="pin"><Pair><key>normal</key><styleUrl>#pin-n</styleUrl></Pair><Pair><key>highlight</key><styleUrl>#red</styleUrl></Pair></StyleMap>
  <Folder><name>Day 1</name>
    <Placemark><name>Start &amp; end</name><styleUrl>#pin</styleUrl><description><![CDATA[<b>Café</b>]]></description>
      <ExtendedData><Data name="rating"><value>5</value></Data><SchemaData><SimpleData name="kind">cafe</SimpleData></SchemaData></ExtendedData>
      <Point><coordinates>2.35,48.85,30</coordinates></Point></Placemark>
    <Folder><name>Walk</name>
      <Placemark><name>Route</name><styleUrl>#red</styleUrl><LineString><coordinates>2.35,48.85 2.36,48.86
        2.37,48.87</coordinates></LineString></Placemark>
    </Folder>
  </Folder>
  <Placemark><name>Park</name><Style><PolyStyle><color>ff00aa00</color></PolyStyle></Style>
    <MultiGeometry><Polygon><outerBoundaryIs><LinearRing><coordinates>0,0 1,0 1,1 0,0</coordinates></LinearRing></outerBoundaryIs>
      <innerBoundaryIs><LinearRing><coordinates>0.1,0.1 0.2,0.1 0.2,0.2 0.1,0.1</coordinates></LinearRing></innerBoundaryIs></Polygon>
      <Point><coordinates>0.5,0.5</coordinates></Point></MultiGeometry></Placemark>
  <Placemark><name>Track</name><gx:Track><when>2020-01-01T00:00:00Z</when><gx:coord>1 2 3</gx:coord><gx:coord>1.1 2.1 3</gx:coord></gx:Track></Placemark>
  <Placemark><name>Nothing</name></Placemark>
  <GroundOverlay><Icon><href>map.png</href></Icon></GroundOverlay>
</Document></kml>"##;

    #[test]
    fn duplicate_extended_data_is_counted_once_per_document() {
        let xml = r#"<kml><Document>
<Placemark><ExtendedData><Data name="n"><value>0</value></Data><Data name="n"><value>5</value></Data>
<SchemaData><SimpleData name="n">9</SimpleData><SimpleData name="kind">first</SimpleData>
<SimpleData name="kind">later</SimpleData></SchemaData></ExtendedData>
<MultiGeometry><Point><coordinates>1,2</coordinates></Point><LineString><coordinates>1,2 2,3</coordinates></LineString></MultiGeometry></Placemark>
<Placemark><ExtendedData><Data name="n"><value>7</value></Data><Data name="n"><value>8</value></Data></ExtendedData>
<Point><coordinates>3,4</coordinates></Point></Placemark>
</Document></kml>"#;
        let layers = read("duplicates.kml", xml.as_bytes()).unwrap();
        assert_eq!(layers.len(), 2);
        let messages: Vec<_> = layers.iter().flat_map(|l| &l.report.messages).collect();
        assert_eq!(
            messages.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
            ["duplicates: 4 repeated ExtendedData entries in 2 placemarks were left out"]
        );
        assert_eq!(messages[0].loss, Some(crate::loss::LossCode::DroppedValue));
        for layer in &layers {
            let ds = &layer.dataset;
            assert_eq!(ds.value(0, ds.field_index("n").unwrap()), &Value::Int(0));
            assert_eq!(ds.value(0, ds.field_index("kind").unwrap()), &Value::Text("first".into()));
        }
        assert_eq!(layers[0].dataset.value(1, layers[0].dataset.field_index("n").unwrap()), &Value::Int(7));
    }

    #[test]
    fn repeated_names_past_the_column_cap_are_counted() {
        let mut first = String::from("<Placemark><ExtendedData>");
        for i in 0..MAX_KEYS {
            first.push_str(&format!("<Data name=\"k{i}\"><value>1</value></Data>"));
        }
        first.push_str("</ExtendedData><Point><coordinates>1,2</coordinates></Point></Placemark>");
        let over = |extra: &str| {
            format!(
                "<kml><Document>{first}<Placemark><ExtendedData>{extra}</ExtendedData>\
                 <Point><coordinates>3,4</coordinates></Point></Placemark></Document></kml>"
            )
        };
        let twice = over("<Data name=\"extra\"><value>1</value></Data><Data name=\"extra\"><value>2</value></Data>");
        let layers = read("cap.kml", twice.as_bytes()).unwrap();
        let messages: Vec<_> = layers.iter().flat_map(|l| &l.report.messages).collect();
        assert!(messages.iter().any(|m| m.contains("1 repeated ExtendedData entries in 1 placemarks")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("distinct names")), "the cap is still said: {messages:?}");
        assert_eq!(layers[0].dataset.fields.len() - layers[0].dataset.field_index("k0").unwrap(), MAX_KEYS);
        // Once past the cap is the cap only, not a repeat.
        let once = over("<Data name=\"extra\"><value>1</value></Data>");
        let layers = read("cap.kml", once.as_bytes()).unwrap();
        let messages: Vec<_> = layers.iter().flat_map(|l| &l.report.messages).collect();
        assert!(!messages.iter().any(|m| m.contains("repeated")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("distinct names")), "{messages:?}");
    }

    #[test]
    fn distinct_extended_data_keys_across_placemarks_are_clean() {
        let xml = r#"<kml><Document><Placemark><ExtendedData><Data name="n"><value>0</value></Data>
<SchemaData><SimpleData name="kind">cafe</SimpleData></SchemaData></ExtendedData><Point><coordinates>1,2</coordinates></Point></Placemark>
<Placemark><ExtendedData><Data name="n"><value>5</value></Data></ExtendedData><Point><coordinates>3,4</coordinates></Point></Placemark>
</Document></kml>"#;
        let layers = read("clean.kml", xml.as_bytes()).unwrap();
        assert!(layers.iter().all(|l| l.report.messages.is_empty()));
        assert_eq!(layers[0].dataset.features.len(), 2);
    }

    #[test]
    fn digits_a_number_cannot_hold_stay_text() {
        let doc = r#"<kml><Document><Placemark><ExtendedData><Data name="id"><value>12345678901234567891</value></Data>
            </ExtendedData><Point><coordinates>1,2</coordinates></Point></Placemark></Document></kml>"#;
        let layers = read("ids.kml", doc.as_bytes()).unwrap();
        let ds = &layers[0].dataset;
        let id = ds.field_index("id").unwrap();
        assert_eq!(ds.value(0, id), &Value::Text("12345678901234567891".into()));
    }

    #[test]
    fn reads_placemarks_folders_data_and_styles() {
        let layers = read("trip.kml", DOC.as_bytes()).unwrap();
        let names: Vec<&str> = layers.iter().map(|l| l.dataset.name.as_str()).collect();
        assert_eq!(names, ["trip points", "trip lines", "trip polygons"]);
        let pts = &layers[0];
        let ds = &pts.dataset;
        assert_eq!(ds.features.len(), 2, "the start and the park's point");
        let f = |n: &str| ds.field_index(n).unwrap();
        assert_eq!(ds.value(0, f("name")), &Value::Text("Start & end".into()));
        assert_eq!(ds.value(0, f("description")), &Value::Text("<b>Café</b>".into()));
        assert_eq!(ds.value(0, f("folder")), &Value::Text("Day 1".into()));
        assert_eq!(ds.value(0, f("rating")), &Value::Int(5), "a whole-number column is typed");
        assert_eq!(ds.value(0, f("kind")), &Value::Text("cafe".into()));
        // The style map resolves to its normal style.
        assert_eq!(ds.value(0, f("style")), &Value::Text("pin-n".into()));
        let pin = pts.styles.iter().find(|(id, _)| id == "pin-n").unwrap().1;
        assert_eq!(pin.icon, Some([0, 0, 255, 255]));
        assert_eq!(pin.scale, Some(1.5));
        assert_eq!(pts.report.skipped, 1);
        // Altitudes other than 0 (the start's 30 m, the track's) are dropped and said; the polygons
        // have none.
        assert!(layers[0].dataset.zm_dropped && layers[1].dataset.zm_dropped && !layers[2].dataset.zm_dropped);
        let said = format!("trip lines: {}", crate::zm_note(Some(1)));
        assert!(layers[1].report.messages.says(&said), "{:?}", layers[1].report.messages);
        // The ground overlay has no box: it can't be laid out.
        let (overlays, undrawn, over_budget) = ground_overlays("t.kml", DOC.as_bytes()).unwrap();
        assert_eq!((overlays.len(), undrawn, over_budget), (0, 1, 0));

        let lines = &layers[1].dataset;
        assert_eq!(lines.features.len(), 2, "the route and the track");
        assert_eq!(lines.value(0, lines.field_index("folder").unwrap()), &Value::Text("Day 1 / Walk".into()));
        let red = layers[1].styles.iter().find(|(id, _)| id == "red").unwrap().1;
        assert_eq!((red.stroke, red.width), (Some([255, 0, 0, 255]), Some(3.0)));
        assert!(matches!(&lines.features[1].geometry, Some(Geometry::Line(p)) if p[0].len() == 2));

        let polys = &layers[2];
        assert!(matches!(&polys.dataset.features[0].geometry, Some(Geometry::Polygon(p)) if p[0].len() == 2));
        let style = polys.dataset.value(0, polys.dataset.field_index("style").unwrap()).to_string();
        assert_eq!(style, "#inline-1");
        assert_eq!(polys.styles[0].1.fill, Some([0, 170, 0, 255]));
    }

    #[test]
    fn exported_text_descriptions_keep_their_column_on_repeated_kml_and_kmz_round_trips() {
        let descriptions = [
            Some("<b>Café & \"river\"</b>\nSecond line"),
            Some("]]> <plain> & text"),
            Some("a\rb"),
            Some("a\r\nb"),
            Some("a\u{7f}b\u{85}c\u{9f}"),
            Some(" padded \n"),
            Some("\t  \r\n "),
            Some(""),
            None,
        ];
        for ext in ["kml", "kmz"] {
            let mut ds = Dataset::new(
                "places",
                GeometryType::Point,
                vec![Field::new("name", FieldType::Text), Field::new("description", FieldType::Text)],
            );
            for (row, description) in descriptions.iter().enumerate() {
                ds.push(
                    Some(Geometry::Point(Coord::new(row as f64, 2.0))),
                    vec![Value::Text(format!("Place {row}")), description.map(|v| Value::Text(v.into())).unwrap_or_default()],
                );
            }
            for _ in 0..3 {
                let xml = crate::interop::to_kml(&ds, Some(0)).unwrap();
                let doc = roxmltree::Document::parse(&xml).unwrap();
                let placemarks: Vec<_> =
                    doc.descendants().filter(|n| n.has_tag_name(("http://www.opengis.net/kml/2.2", "Placemark"))).collect();
                for (pm, expected) in placemarks.iter().zip(descriptions) {
                    assert_eq!(child(pm, "description").map(|n| raw_text_of(&n)).as_deref(), expected);
                }
                let (bytes, skipped) = crate::io::write_bytes(&ds, ext).unwrap();
                assert_eq!(skipped, 0);
                let layers = read(&format!("places.{ext}"), &bytes).unwrap();
                assert_eq!(layers.len(), 1);
                let layer = &layers[0];
                assert!(layer.report.messages.is_empty(), "{ext}: {:?}", layer.report.messages);
                let back = &layer.dataset;
                assert!(back.field_index("description_1").is_none());
                let fi = back.field_index("description").unwrap();
                for (row, expected) in descriptions.iter().enumerate() {
                    let expected = expected.map(|v| Value::Text(v.into())).unwrap_or_default();
                    assert_eq!(back.value(row, fi), &expected, "{ext}, row {row}");
                }
                ds = back.clone();
            }
        }
    }

    /// Names, field names and values are held as they are or refused with a count, like a
    /// description: never cleaned without a word, and never written as XML that can't be read.
    #[test]
    fn unrepresentable_characters_elsewhere_are_refused_and_legal_ones_kept() {
        let layer = |name: &str, value: &str| {
            let mut ds =
                Dataset::new("places", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("note", FieldType::Text)]);
            ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text(name.into()), Value::Text(value.into())]);
            ds
        };
        let error = crate::interop::to_kml(&layer("ok", "a\u{1}b\u{ffff}c"), Some(0)).unwrap_err().to_string();
        assert!(error.contains("field 'note': 2 character(s)") && error.contains("U+0001"), "{error}");
        let error = crate::interop::to_kml(&layer("a\u{fffe}", "ok"), Some(0)).unwrap_err().to_string();
        assert!(error.contains("'name': 1 character(s)") && error.contains("U+FFFE"), "{error}");
        let error = crate::interop::to_gpx(&layer("a\u{1}", "ok"), Some(0)).unwrap_err().to_string();
        assert!(error.starts_with("GPX: places") && error.contains("name: 1 character(s)"), "{error}");
        let mut bad_field = layer("ok", "ok");
        bad_field.fields[1].name = "n\u{2}".into();
        assert!(crate::interop::to_kml(&bad_field, Some(0)).unwrap_err().to_string().contains("field name"));
        // Characters XML 1.0 holds (DEL, C1 controls) are kept, and the file reads back.
        let legal = "a\u{7f}b\u{85}c";
        let xml = crate::interop::to_kml(&layer(legal, legal), Some(0)).unwrap();
        roxmltree::Document::parse(&xml).unwrap();
        let back = read("places.kml", xml.as_bytes()).unwrap();
        let ds = &back[0].dataset;
        assert_eq!(ds.value(0, ds.field_index("note").unwrap()), &Value::Text(legal.into()));
        roxmltree::Document::parse(&crate::interop::to_gpx(&layer(legal, legal), Some(0)).unwrap()).unwrap();
    }

    #[test]
    fn unrepresentable_description_characters_are_refused_with_their_count_and_location() {
        for bad in ['\0', '\u{b}', '\u{1b}', '\u{1f}', '\u{fffe}', '\u{ffff}'] {
            let mut ds = Dataset::new("places", GeometryType::Point, vec![Field::new("description", FieldType::Text)]);
            ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text(format!("a{bad}b{bad}c"))]);
            let error = crate::interop::to_kml(&ds, None).unwrap_err().to_string();
            assert!(error.contains("places") && error.contains(&format!("feature id {}", ds.features[0].id)), "{error}");
            assert!(error.contains("description: 2 character(s)") && error.contains(&format!("U+{:04X}", bad as u32)), "{error}");
            for ext in ["kml", "kmz"] {
                assert_eq!(crate::io::write_bytes(&ds, ext).unwrap_err().to_string(), error);
            }
        }
    }

    #[test]
    fn description_copies_keep_whitespace_in_data_and_simple_data() {
        for copy in [
            "<Data name=\"description\"><value> padded &#13;\n</value></Data>",
            "<SchemaData><SimpleData name=\"description\"> padded &#13;\n</SimpleData></SchemaData>",
        ] {
            let xml = format!(
                "<kml><Placemark><description> padded &#13;\n</description><ExtendedData>{copy}</ExtendedData><Point><coordinates>1,2</coordinates></Point></Placemark></kml>"
            );
            let layers = read("spaces.kml", xml.as_bytes()).unwrap();
            let layer = &layers[0];
            assert!(layer.report.messages.is_empty(), "{:?}", layer.report.messages);
            let ds = &layer.dataset;
            assert_eq!(ds.value(0, ds.field_index("description").unwrap()), &Value::Text(" padded \r\n".into()));
            assert!(ds.field_index("description_1").is_none());
        }
    }

    #[test]
    fn exported_case_variant_descriptions_stay_distinct_from_the_canonical_column() {
        for variant in ["Description", "DESCRIPTION"] {
            for other in ["Keep case", "Another value"] {
                for ext in ["kml", "kmz"] {
                    let mut ds = Dataset::new(
                        "places",
                        GeometryType::Point,
                        vec![
                            Field::new("name", FieldType::Text),
                            Field::new("description", FieldType::Text),
                            Field::new(variant, FieldType::Text),
                        ],
                    );
                    ds.push(
                        Some(Geometry::Point(Coord::new(1.0, 2.0))),
                        vec![Value::Text("Place".into()), Value::Text("Keep case".into()), Value::Text(other.into())],
                    );
                    let (bytes, _) = crate::io::write_bytes(&ds, ext).unwrap();
                    let layers = read(&format!("places.{ext}"), &bytes).unwrap();
                    let layer = &layers[0];
                    let back = &layer.dataset;
                    assert_eq!(back.value(0, back.field_index("description").unwrap()), &Value::Text("Keep case".into()));
                    assert_eq!(back.value(0, back.field_index(&format!("{variant}_1")).unwrap()), &Value::Text(other.into()));
                    assert_eq!(layer.report.messages.len(), 1, "{:?}", layer.report.messages);
                    assert!(layer.report.messages[0].contains(&format!("'{variant}' → '{variant}_1'")), "{:?}", layer.report.messages);
                }
            }
        }
    }

    #[test]
    fn exported_other_reserved_columns_still_report_their_collisions() {
        for (name, ty, value) in [
            ("description", FieldType::Double, Value::Float(2.5)),
            ("Description", FieldType::Text, Value::Text("Keep case".into())),
            ("folder", FieldType::Text, Value::Text("My folder".into())),
            ("style", FieldType::Text, Value::Text("My style".into())),
        ] {
            let mut ds = Dataset::new("places", GeometryType::Point, vec![Field::new(name, ty)]);
            ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![value.clone()]);
            let xml = crate::interop::to_kml(&ds, None).unwrap();
            assert!(!xml.contains("<description>"), "{xml}");
            let layers = read("places.kml", xml.as_bytes()).unwrap();
            let layer = &layers[0];
            let fi = layer.dataset.field_index(&format!("{name}_1")).unwrap();
            assert_eq!(layer.dataset.value(0, fi), &value);
            assert_eq!(layer.report.messages.len(), 1, "{:?}", layer.report.messages);
            assert!(layer.report.messages[0].contains("renamed"), "{:?}", layer.report.messages);
        }
    }

    #[test]
    fn extended_data_named_like_a_fixed_column_is_kept_under_a_free_name() {
        // `name` becomes the placemark's name and, like every field, extended data too; `NAME`
        // clashes with the name column ignoring case but holds something else.
        let mut ds =
            Dataset::new("t", GeometryType::Point, vec![Field::new("name", FieldType::Double), Field::new("NAME", FieldType::Integer)]);
        ds.push_next(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Float(0.0), Value::Int(1)]);
        let kml = crate::interop::to_kml(&ds, crate::io::name_field(&ds)).unwrap();
        let layers = read("t.kml", kml.as_bytes()).unwrap();
        let (back, report) = (&layers[0].dataset, &layers[0].report);
        let names: Vec<&str> = back.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["name", "description", "folder", "style", "NAME_1"], "the copy of the name isn't a column of its own");
        assert_eq!(back.value(0, 0), &Value::Text("0".into()));
        assert_eq!(back.value(0, 4), &Value::Int(1));
        assert_eq!(
            report.messages,
            [
                "t: 1 extended data name was renamed, as RustGIS gives every placemark a name, description, folder and style column: 'NAME' → 'NAME_1'"
            ]
        );

        // Without a clash, nothing is said.
        let layers = read("trip.kml", DOC.as_bytes()).unwrap();
        assert!(layers[0].report.messages.iter().all(|m| !m.contains("renamed")), "{:?}", layers[0].report.messages);
    }

    #[test]
    fn kmz_and_hostile_input() {
        let kmz = crate::zip::write(&[("files/x.png".into(), vec![1, 2]), ("doc.kml".into(), DOC.as_bytes().to_vec())]).unwrap();
        assert_eq!(read("trip.kmz", &kmz).unwrap().len(), 3);
        // An entity bomb's DTD is refused.
        let bomb = r#"<?xml version="1.0"?><!DOCTYPE kml [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;">]><kml><Document><name>&b;</name></Document></kml>"#;
        assert!(read("b.kml", bomb.as_bytes()).is_err());
        assert!(read("x.kml", b"<kml><Placemark>").is_err());
        assert!(read("x.kml", &[0xff, 0xfe, 0x00]).is_err());
        assert!(read("x.kmz", b"PK\x03\x04junk").is_err());
        // Deep nesting is bounded, not followed to a stack overflow.
        let deep = format!(
            "<kml>{}<Placemark><Point><coordinates>1,2</coordinates></Point></Placemark>{}</kml>",
            "<Folder>".repeat(500),
            "</Folder>".repeat(500)
        );
        let _ = read("deep.kml", deep.as_bytes());
        for n in (0..DOC.len()).step_by(53) {
            let _ = read("t.kml", &DOC.as_bytes()[..n]);
        }
        // A placemark named with digits keeps its name as text; coordinates off the globe drop.
        let named = r#"<kml><Placemark><name>42</name><Point><coordinates>1,2</coordinates></Point></Placemark><Placemark><name>x</name><Point><coordinates>500,2</coordinates></Point></Placemark></kml>"#;
        let l = read("n.kml", named.as_bytes()).unwrap();
        assert_eq!(l[0].dataset.value(0, 0), &Value::Text("42".into()));
        assert_eq!(l[0].dataset.features.len(), 1);
        // An empty document has no layers.
        assert!(read("e.kml", b"<kml><Document/></kml>").unwrap().is_empty());
    }
}
