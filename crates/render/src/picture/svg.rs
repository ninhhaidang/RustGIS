//! SVG images for picture markers: read within bounds, what drawing one costs worked out before
//! it is drawn, and drawn once into a pixmap.
//!
//! An SVG's cost isn't its size in bytes or pixels. Measured at 700 px a side, a few kilobytes
//! can take minutes: 500 thick strokes dashed every 0.01 units took 366 s, 200 blurs 79 s, 20
//! turbulences 8 s, 20,000 half-transparent squares 129 s, `<use>` nested to 10⁵ elements 0.2 s
//! just to read, and a path of 20,000 segments used 400 times 0.6 s to read. Reading one turns
//! each arc into cubic curves, more the larger its radius ([`cubics`]): a 24-byte circle of radius
//! 10³⁸ came to 9.6 million points, and a path's arc whose radius its ends stretch never finished.
//! So what an SVG comes to is counted before it is read ([`elements`]), and it is taken only when
//! what it draws is worked out first ([`work`]), from what the rasterizer does: the pixels each
//! shape covers and the rows its edges cross, each dash and each segment of a stroke, each layer a
//! group with opacity, a clip or a mask draws into. A unit is a pixel blended, about 13 ns, the
//! dearest the rasterizer spends on one. The measured cases all came within ~13 ns a unit; an icon
//! is a few hundred thousand units.
//!
//! Filters are refused outright: they cost per pixel by kind and parameter (a blur's radius, a
//! turbulence's octaves, a morphology's, a convolution's), which a marker's icon rarely needs and
//! a count can't bound without modelling each one. So is text (no fonts are loaded for it) and an
//! image inside the SVG or one it links to (it would pass the image caps, or read a file): drawn
//! without them, an icon would lose them unsaid. So is animation, which only its first frame would
//! show, and anything it takes from another file (a `<use>`, a paint or a style sheet): no file
//! is read.

use resvg::usvg;
use usvg::roxmltree;

/// Why an SVG isn't taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// It isn't an SVG that reads.
    NotSvg,
    /// It has more to draw than the cap given, or more elements than [`ELEMENTS`].
    TooMuch,
    /// It has more XML nodes than [`XML_NODES`], or nests deeper than [`DEPTH`].
    TooNested,
    /// A shape's radius is in em or ex: font sizes, which no font is loaded to give, so it may be
    /// any size.
    FontRadius,
    /// It has filters, text, images, animation or links to other files, which aren't drawn (the
    /// module says why).
    Undrawn,
}

/// The most elements an SVG may have once its `<use>`s and markers are counted where they repeat:
/// reading takes some 2 µs an element, so at most ~20 ms. Icons have tens to hundreds.
pub(super) const ELEMENTS: u64 = 10_000;
/// The most points an SVG's shapes may come to once read, counted where they repeat, as
/// [`ELEMENTS`]: reading works out each path's bounds and its stroke's, some 70 ns a point (a path
/// of 20,000 segments used 400 times took 0.6 s), so at most ~15 ms. Icons have hundreds to a few
/// thousand.
pub(super) const POINTS: u64 = 200_000;
/// The most cubic curves one arc may come to once read ([`cubics`]): an icon's arcs come to a few
/// (a radius of 10⁶ comes to 16 a whole turn, 10¹² to 150), and the reader moves each of an arc's
/// curves across all the others still to go (svgtypes' `SimplifyingPathParser`), which grows as
/// their square: one of millions never finishes.
const ARC_CUBICS: f64 = 200.0;
/// The most steps matching the style sheets' rules to the elements may take ([`css_steps`]):
/// each element is tried against each rule, and a selector's descendant (`a b`) against every
/// element above it, again for each descendant before it, so a few of them deep down come to
/// millions (`x g g g g g g rect` 40 levels down took 0.5 s). A step is some 11 ns (measured: a
/// class rule on an element, two steps, 22 ns), so ~11 ms: some 500,000 rules tried on elements,
/// 200 class rules on 2,500 elements. An attribute's value matched costs a step each 64 bytes too,
/// and each node walked back past to a sibling one. Icons take thousands of steps.
const CSS_STEPS: u64 = 1_000_000;
/// The most XML nodes (elements, text, comments) read before giving up, a bound on reading the
/// text itself: [`ELEMENTS`] and the text and spaces between them.
pub(super) const XML_NODES: u32 = 4 * ELEMENTS as u32;
/// The deepest nesting read (usvg's own bound is 1,024): icons nest a few levels, and the walks
/// here recurse as deep.
pub(super) const DEPTH: u32 = 64;
/// What each element and each segment costs whatever its size, in units.
const NODE: u64 = 100;
/// What a gradient's stops are divided by to cost units a pixel: past two, tiny-skia goes through
/// them all at each pixel (its `gradient` stage), some 0.7 ns a stop (measured: 1,000 stops
/// 695 ns a pixel, 100 stops 93 ns, 2 stops 21 ns), so each stop costs 1/16 of a unit a pixel.
const STOPS_PER_UNIT: u64 = 16;

/// SVG text `text` read into a tree: within [`XML_NODES`], [`ELEMENTS`], [`POINTS`] and
/// [`CSS_STEPS`], its root an `<svg>`, with no images resolved and no files read.
pub(super) fn read(text: &str) -> Result<usvg::Tree, Refusal> {
    let xml = roxmltree::ParsingOptions { allow_dtd: true, nodes_limit: XML_NODES, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, xml).map_err(|e| match e {
        roxmltree::Error::NodesLimitReached => Refusal::TooNested,
        _ => Refusal::NotSvg,
    })?;
    if doc.root_element().tag_name().name() != "svg" {
        return Err(Refusal::NotSvg);
    }
    if doc.descendants().any(undrawn) {
        return Err(Refusal::Undrawn);
    }
    if doc.descendants().any(font_radius) {
        return Err(Refusal::FontRadius);
    }
    with_sheet(&doc, |sheet| {
        elements(&doc, sheet)?;
        if nested_markers(&doc, sheet) { Err(Refusal::TooMuch) } else { Ok(()) }
    })?;
    let opt = usvg::Options {
        resources_dir: None,
        max_depth: DEPTH,
        image_href_resolver: usvg::ImageHrefResolver { resolve_data: Box::new(|_, _, _| None), resolve_string: Box::new(|_, _| None) },
        ..Default::default()
    };
    usvg::Tree::from_xmltree(&doc, &opt).map_err(|e| match e {
        usvg::Error::ElementsLimitReached => Refusal::TooMuch,
        _ => Refusal::NotSvg,
    })
}

/// Whether shape `n` has a radius in em or ex other than 0 ([`Refusal::FontRadius`]), of those
/// usvg reads for it: a circle's `r`, an ellipse's or a rectangle's `rx` and `ry`.
fn font_radius(n: roxmltree::Node) -> bool {
    use svgtypes::LengthUnit::{Em, Ex};
    let radii: &[&str] = match n.tag_name().name() {
        "circle" => &["r"],
        "ellipse" | "rect" => &["rx", "ry"],
        _ => return false,
    };
    radii
        .iter()
        .filter_map(|a| n.attribute(*a))
        .any(|v| v.trim().parse::<svgtypes::Length>().is_ok_and(|l| matches!(l.unit, Em | Ex) && l.number != 0.0))
}

/// Whether `n` is something that isn't drawn: text, an image, animation, or what an SVG takes from
/// another file (a `<use>`, a gradient or pattern it inherits, a paint, a clip path or a style
/// sheet's `@import` or `<?xml-stylesheet?>` from elsewhere, which usvg never reads); an HTML
/// object too, but for one in a `<switch>` that needs an extension, which usvg passes over for what
/// follows it (as an editor's SVG keeps its own data).
fn undrawn(n: roxmltree::Node) -> bool {
    if n.is_pi() {
        return n.pi().is_some_and(|pi| pi.target == "xml-stylesheet");
    }
    match n.tag_name().name() {
        "text" | "image" => return true,
        // The still image is drawn: what moves would be lost.
        "animate" | "animateColor" | "animateMotion" | "animateTransform" | "discard" | "set" => return true,
        "style" => {
            let sheet = sheet_of(n);
            // An at-rule's name is any case; `@keyframes` moves what a still image can't, with a
            // vendor's prefix too (`@-webkit-keyframes`).
            let lower = sheet.to_ascii_lowercase();
            let keyframes = lower.split('@').skip(1).any(|r| {
                let name = r.strip_prefix('-').and_then(|v| v.split_once('-')).map_or(r, |(_, rest)| rest);
                name.starts_with("keyframes")
            });
            return lower.contains("@import") || keyframes || elsewhere(&sheet);
        }
        "foreignObject" => {
            return !(n.parent_element().is_some_and(|p| p.has_tag_name("switch")) && n.has_attribute("requiredExtensions"));
        }
        _ => {}
    }
    n.attributes().any(|a| {
        let v = a.value().trim();
        // A link's address is where it leads, not something it draws.
        (a.name() == "href" && !n.has_tag_name("a") && !v.is_empty() && !v.starts_with('#')) || elsewhere(v)
    })
}

/// Whether a marker may be drawn on a shape inside another marker: usvg draws it on each of that
/// marker's vertices wherever that marker is (its `convert_children` of a marker), which
/// [`elements`] doesn't count. A shape there takes a marker by inheritance: from itself or what
/// holds it inside the marker, from the marker and what holds it, from a style sheet (whose reach
/// isn't read here), or, through a `<use>` in the marker, from anything the copy it makes holds or
/// that holds it. So any of those naming a marker counts. One marker can only nest itself, a cycle
/// usvg drops (it follows a `<use>`'s copy too).
fn nested_markers(doc: &roxmltree::Document, sheet: &simplecss::StyleSheet) -> bool {
    let markers: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("marker")).collect();
    if markers.len() < 2 {
        return false;
    }
    let uses = markers.iter().any(|m| m.descendants().any(|n| n.has_tag_name("use")));
    sheet_sets_marker(sheet)
        || markers.iter().any(|m| m.ancestors().any(names_marker) || m.descendants().any(names_marker))
        || (uses && doc.descendants().any(names_marker))
}

/// Whether element `n` sets a marker on what it draws and holds, by attribute or in its `style`.
fn names_marker(n: roxmltree::Node) -> bool {
    n.attributes().any(|a| {
        sets_marker(a.name(), a.value())
            || (a.name() == "style" && simplecss::DeclarationTokenizer::from(a.value()).any(|d| sets_marker(d.name, d.value)))
    })
}

/// Whether property `name` of `value` sets a marker: a marker property naming one (`url(#id)`),
/// read as usvg reads it; `none` sets none.
fn sets_marker(name: &str, value: &str) -> bool {
    is_marker(name) && svgtypes::FuncIRI::from_str(value).is_ok()
}

/// Whether style sheet `sheet` sets a marker, on whatever its selectors reach.
fn sheet_sets_marker(sheet: &simplecss::StyleSheet) -> bool {
    sheet.rules.iter().any(|r| r.declarations.iter().any(|d| sets_marker(d.name, d.value)))
}

/// The ids of the markers a marker property names (`url(#id)`), by attribute, in a `style` or in
/// style sheet `sheet`, read as usvg reads them: no other is drawn.
fn named_markers(doc: &roxmltree::Document, sheet: &simplecss::StyleSheet) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    let mut take = |v: &str| {
        if let Ok(iri) = svgtypes::FuncIRI::from_str(v) {
            ids.insert(iri.0.to_string());
        }
    };
    for a in doc.descendants().flat_map(|n| n.attributes()) {
        if is_marker(a.name()) {
            take(a.value());
        } else if a.name() == "style" {
            simplecss::DeclarationTokenizer::from(a.value()).filter(|d| is_marker(d.name)).for_each(|d| take(d.value));
        }
    }
    sheet.rules.iter().flat_map(|r| &r.declarations).filter(|d| is_marker(d.name)).for_each(|d| take(d.value));
    ids
}

/// Whether property `name` sets a marker, read as usvg reads a style's declarations (with
/// simplecss: past comments and spaces before the colon); ASCII case aside, to be safe (usvg
/// takes only lower case).
fn is_marker(name: &str) -> bool {
    ["marker", "marker-start", "marker-mid", "marker-end"].iter().any(|m| name.eq_ignore_ascii_case(m))
}

/// The steps matching `sheet`'s rules to element `n` takes, as usvg matches them before reading
/// it (each rule tried with simplecss's own matcher, each step counted), on top of the `spent`
/// that matching other elements took; `None` past [`CSS_STEPS`] in all, where it is stopped.
fn css_steps(n: roxmltree::Node, sheet: &simplecss::StyleSheet, spent: u64) -> Option<u64> {
    let steps = std::cell::Cell::new(spent);
    for rule in &sheet.rules {
        steps.set(steps.get() + 1);
        rule.selector.matches(&Stepped(n, &steps));
        if steps.get() > CSS_STEPS {
            return None;
        }
    }
    Some(steps.get() - spent)
}

/// An element as simplecss matches it, each step it takes counted in the cell; past
/// [`CSS_STEPS`] it has nothing above or before it and matches nothing, so the matching ends.
struct Stepped<'a, 'input>(roxmltree::Node<'a, 'input>, &'a std::cell::Cell<u64>);

impl<'a, 'input> Stepped<'a, 'input> {
    /// Counts a step: whether it is still within [`CSS_STEPS`].
    fn step(&self) -> bool {
        self.steps(1)
    }

    /// Counts `n` steps: whether they are still within [`CSS_STEPS`].
    fn steps(&self, n: u64) -> bool {
        self.1.set(self.1.get().saturating_add(n));
        self.1.get() <= CSS_STEPS
    }

    /// The element `to` gives, a step away; none past [`CSS_STEPS`].
    fn hop(&self, to: impl FnOnce(roxmltree::Node<'a, 'input>) -> Option<roxmltree::Node<'a, 'input>>) -> Option<Self> {
        if !self.step() {
            return None;
        }
        to(self.0).map(|n| Stepped(n, self.1))
    }
}

impl simplecss::Element for Stepped<'_, '_> {
    fn parent_element(&self) -> Option<Self> {
        self.hop(|n| n.parent_element())
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        // Each node walked back past is a step: comments between siblings are walked too.
        let mut back = self.0.prev_sibling();
        while let Some(n) = back {
            if !self.step() {
                return None;
            }
            if n.is_element() {
                return Some(Stepped(n, self.1));
            }
            back = n.prev_sibling();
        }
        None
    }

    fn has_local_name(&self, name: &str) -> bool {
        self.step() && self.0.tag_name().name() == name
    }

    fn attribute_matches(&self, name: &str, operator: simplecss::AttributeOperator) -> bool {
        // Matched along its value, a step each 64 bytes, before it is read.
        match self.0.attribute(name) {
            Some(v) => self.steps(1 + v.len() as u64 / 64) && operator.matches(v),
            None => {
                self.step();
                false
            }
        }
    }

    fn pseudo_class_matches(&self, class: simplecss::PseudoClass) -> bool {
        self.step() && matches!(class, simplecss::PseudoClass::FirstChild) && self.prev_sibling_element().is_none()
    }
}

/// `f` of `doc`'s style sheets, read once as usvg reads them (`resolve_css`), every `<style>`'s
/// text taken (usvg leaves out one of a `type` other than CSS): more, never less.
fn with_sheet<R>(doc: &roxmltree::Document, f: impl FnOnce(&simplecss::StyleSheet) -> R) -> R {
    let texts: Vec<String> = doc.descendants().filter(|n| n.has_tag_name("style")).map(sheet_of).collect();
    let mut sheet = simplecss::StyleSheet::new();
    for text in &texts {
        sheet.parse_more(text);
    }
    f(&sheet)
}

/// Style element `n`'s sheet: its text, comments left out, as usvg reads it (`style_texts`).
fn sheet_of(n: roxmltree::Node) -> String {
    n.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect()
}

/// Whether `v` names something by `url(…)` that isn't one of the SVG's own (`#id`).
fn elsewhere(v: &str) -> bool {
    // A browser takes `URL(` too, and would fetch what it names.
    v.to_ascii_lowercase().split("url(").skip(1).any(|r| !r.trim_start().trim_start_matches(['"', '\'']).starts_with('#'))
}

/// The elements `doc` comes to once each `<use>` is counted as what it repeats and each marker as
/// often as the vertices it can sit on. Refused nested past [`DEPTH`] ([`Refusal::TooNested`]);
/// past [`ELEMENTS`], its shapes' points ([`points_of`]) counted so past [`POINTS`], matched to its
/// style sheets past [`CSS_STEPS`] ([`css_steps`]) or where a `<use>` repeats itself (usvg drops
/// those, but counting it is no use) as too much ([`Refusal::TooMuch`]).
fn elements(doc: &roxmltree::Document, sheet: &simplecss::StyleSheet) -> Result<u64, Refusal> {
    // The first element of an id, as usvg takes it: counting another would count what isn't drawn.
    let mut ids = std::collections::HashMap::new();
    for n in doc.descendants() {
        if let Some(id) = n.attribute("id") {
            ids.entry(id).or_insert(n);
        }
    }
    let mut counter = Counter { ids, seen: std::collections::HashMap::new(), viewport: viewport(doc), sheet, matched: 0, deep: false };
    let refused = |c: &Counter| if c.deep { Refusal::TooNested } else { Refusal::TooMuch };
    let all = counter.count(doc.root_element(), 0).ok_or_else(|| refused(&counter))?;
    // A marker sits on the vertices (`marker-mid`) and both ends of the shapes that set it, by
    // attribute or style, or that inherit it from what holds them or from the `<use>` that copies
    // them; a style sheet may set it on any shape. So each marker, its elements and their points,
    // is counted as if on all of those vertices: none for a marker nothing names.
    let vertices = if sheet_sets_marker(sheet) { all.points } else { all.marked };
    let named = named_markers(doc, sheet);
    let used = |m: &roxmltree::Node| m.has_tag_name("marker") && m.attribute("id").is_some_and(|id| named.contains(id));
    let (mut markers, mut marked) = (0u64, 0u64);
    for m in doc.descendants().filter(used) {
        let c = counter.count(m, 0).ok_or_else(|| refused(&counter))?;
        (markers, marked) = (markers.saturating_add(c.elements), marked.saturating_add(c.points));
    }
    let total = all.elements.saturating_add(markers.saturating_mul(vertices));
    let points = all.points.saturating_add(marked.saturating_mul(vertices));
    (total <= ELEMENTS && points <= POINTS).then_some(total).ok_or(Refusal::TooMuch)
}

/// What an element and what's under it come to ([`elements`]).
#[derive(Clone, Copy)]
struct Counted {
    elements: u64,
    /// Its shapes' points ([`points_of`]).
    points: u64,
    /// The points of those that set a marker or inherit one from what holds them here.
    marked: u64,
    /// The levels under it, `<use>`s counted as levels.
    height: u32,
    /// The steps matching the style sheets to it and what's under it takes ([`css_steps`]), each
    /// `<use>`'s copy again: usvg reads what a `<use>` repeats again for each, rules and all.
    steps: u64,
}

/// [`elements`]'s count of a document.
struct Counter<'a, 's> {
    ids: std::collections::HashMap<&'a str, roxmltree::Node<'a, 'a>>,
    /// Each element counted, by node (`None` marks one being counted: a cycle).
    seen: std::collections::HashMap<roxmltree::NodeId, Option<Counted>>,
    /// What a length in % may come to ([`viewport`]).
    viewport: f64,
    /// The style sheets.
    sheet: &'s simplecss::StyleSheet<'s>,
    /// The steps the matching here has taken so far, each element once ([`css_steps`]).
    matched: u64,
    /// Whether a count stopped for nesting past [`DEPTH`] ([`Refusal::TooNested`]).
    deep: bool,
}

impl<'a> Counter<'a, '_> {
    /// What element `n`, `depth` levels down, comes to; `None` past [`ELEMENTS`], [`DEPTH`] or
    /// [`CSS_STEPS`], or for a `<use>` that repeats itself. Each element is counted once and its
    /// count kept, but its levels are checked again wherever it is reached: a `<use>` may reach it
    /// deeper.
    fn count(&mut self, n: roxmltree::Node<'a, 'a>, depth: u32) -> Option<Counted> {
        if depth > DEPTH {
            self.deep = true;
            return None;
        }
        match self.seen.get(&n.id()) {
            Some(Some(done)) => {
                self.deep |= depth + done.height > DEPTH;
                return (depth + done.height <= DEPTH).then_some(*done);
            }
            Some(None) => return None,
            None => {}
        }
        // Left `None` when the count fails too: every `None` ends in a refusal.
        self.seen.insert(n.id(), None);
        // A plain `href` before an XLink one, as usvg takes them (SVG 2).
        let named = |ns: Option<&str>| n.attributes().find(|a| a.name() == "href" && a.namespace() == ns).map(|a| a.value());
        let href = named(None).or_else(|| named(Some("http://www.w3.org/1999/xlink")));
        // What a `<use>` repeats counts as if it were under it, its link read as usvg reads it.
        let used = n
            .has_tag_name("use")
            .then(|| href.and_then(|h| svgtypes::IRI::from_str(h).ok()).and_then(|iri| self.ids.get(iri.0).copied()))
            .flatten();
        let steps = css_steps(n, self.sheet, self.matched)?;
        self.matched = self.matched.saturating_add(steps);
        let mut total = Counted { elements: 1, points: points_of(n, self.viewport), marked: 0, height: 0, steps };
        for under in n.children().filter(|c| c.is_element()).chain(used) {
            let c = self.count(under, depth + 1)?;
            total.elements = total.elements.saturating_add(c.elements);
            total.points = total.points.saturating_add(c.points);
            total.marked = total.marked.saturating_add(c.marked);
            total.height = total.height.max(c.height + 1);
            total.steps = total.steps.saturating_add(c.steps);
            if total.elements > ELEMENTS || total.steps > CSS_STEPS {
                return None;
            }
        }
        // What sets a marker sets it on all it draws and holds.
        if names_marker(n) {
            total.marked = total.points;
        }
        self.seen.insert(n.id(), Some(total));
        Some(total)
    }
}

/// The largest a viewport of `doc` may be, which a length in % is a part of: its view boxes and the
/// sizes of its `<svg>`s, `<symbol>`s and `<use>`s, as usvg takes them for one (100 a side
/// without). The root's size in % is of 100 a side; any other in %, em or ex may be any size, the
/// largest an f32 holds.
fn viewport(doc: &roxmltree::Document) -> f64 {
    let root = doc.root_element();
    let mut largest = 100.0f64;
    for n in doc.descendants().filter(|n| n.is_element()) {
        if let Some(b) = n.attribute("viewBox").and_then(|v| v.parse::<svgtypes::ViewBox>().ok()) {
            largest = largest.max(b.w.abs()).max(b.h.abs());
        }
        if matches!(n.tag_name().name(), "svg" | "symbol" | "use") {
            let of = if n == root { 100.0 } else { f64::from(f32::MAX) };
            for side in ["width", "height"].iter().filter_map(|&a| n.attribute(a)) {
                largest = largest.max(length(side, of));
            }
        }
    }
    largest
}

/// The points shape element `n` comes to once read, both ends counted (a marker may sit on each):
/// what each segment of its path data holds, an arc as the curves it is divided into
/// ([`cubics`]); a circle, an ellipse and a rounded rectangle as four quarter arcs, a radius in %
/// as that part of `viewport`; a polyline's pairs of numbers as svgtypes reads them. `u64::MAX`
/// for an arc past [`ARC_CUBICS`].
fn points_of(n: roxmltree::Node, viewport: f64) -> u64 {
    let corners = |names: &[&str]| {
        // usvg resolves lengths in f32: a radius past it isn't drawn.
        let radius = names.iter().filter_map(|&a| n.attribute(a)).fold(0.0f64, |r, v| r.max(length(v, viewport)));
        let c = cubics(radius, std::f64::consts::FRAC_PI_2);
        if c > ARC_CUBICS { u64::MAX } else { 4 * 3 * c as u64 + 5 }
    };
    let points = match n.tag_name().name() {
        "path" => n.attribute("d").map_or(0, path_points),
        "polyline" | "polygon" => n.attribute("points").map_or(0, |p| svgtypes::PointsParser::from(p).count() as u64),
        "line" => 0,
        "circle" => corners(&["r"]),
        "ellipse" => corners(&["rx", "ry"]),
        "rect" if n.has_attribute("rx") || n.has_attribute("ry") => corners(&["rx", "ry"]),
        "rect" => 4,
        _ => return 0,
    };
    points.saturating_add(2)
}

/// Length `v` in user units at most, as usvg resolves it (96 dpi), one in % a part of `of`: the
/// largest an f32 holds for one relative to a font (whose size may be any), 0 for 0 in any unit
/// and for one that doesn't read (usvg takes none).
fn length(v: &str, of: f64) -> f64 {
    use svgtypes::LengthUnit::*;
    let Ok(l) = v.trim().parse::<svgtypes::Length>() else { return 0.0 };
    if l.number == 0.0 {
        return 0.0;
    }
    let per = match l.unit {
        None | Px => 1.0,
        In => 96.0,
        Cm => 96.0 / 2.54,
        Mm => 96.0 / 25.4,
        Pt => 96.0 / 72.0,
        Pc => 16.0,
        Percent => of / 100.0,
        Em | Ex => return f64::from(f32::MAX),
    };
    (l.number.abs() * per).min(f64::from(f32::MAX))
}

/// The points path data `d` comes to once read, as svgtypes' `SimplifyingPathParser` builds it for
/// usvg: each arc from where the one before ends, divided as kurbo divides it.
fn path_points(d: &str) -> u64 {
    use svgtypes::PathSegment::*;
    let (mut at, mut start) = (kurbo::Point::ZERO, kurbo::Point::ZERO);
    let mut points = 0u64;
    for seg in svgtypes::PathParser::from(d) {
        // usvg stops at the first error, as here.
        let Ok(seg) = seg else { break };
        let to = |abs: bool, x: f64, y: f64| if abs { kurbo::Point::new(x, y) } else { kurbo::Point::new(at.x + x, at.y + y) };
        let (end, n) = match seg {
            MoveTo { abs, x, y } => {
                start = to(abs, x, y);
                (start, 1)
            }
            LineTo { abs, x, y } => (to(abs, x, y), 1),
            HorizontalLineTo { abs, x } => (kurbo::Point::new(if abs { x } else { at.x + x }, at.y), 1),
            VerticalLineTo { abs, y } => (kurbo::Point::new(at.x, if abs { y } else { at.y + y }), 1),
            CurveTo { abs, x, y, .. } | SmoothCurveTo { abs, x, y, .. } => (to(abs, x, y), 3),
            Quadratic { abs, x, y, .. } | SmoothQuadratic { abs, x, y } => (to(abs, x, y), 2),
            EllipticalArc { abs, rx, ry, x_axis_rotation, large_arc, sweep, x, y } => {
                let end = to(abs, x, y);
                let arc = kurbo::SvgArc {
                    from: at,
                    to: end,
                    radii: kurbo::Vec2::new(rx, ry),
                    x_rotation: x_axis_rotation.to_radians(),
                    large_arc,
                    sweep,
                };
                match kurbo::Arc::from_svg_arc(&arc) {
                    Some(arc) => {
                        let c = cubics(arc.radii.x.max(arc.radii.y), arc.sweep_angle);
                        // Not finite (NaN) for ends or radii past what a float holds.
                        if c.is_nan() || c > ARC_CUBICS {
                            return u64::MAX;
                        }
                        (end, 3 * c as u64)
                    }
                    // A straight line instead.
                    None => (end, 1),
                }
            }
            ClosePath { .. } => (start, 1),
        };
        at = end;
        points = points.saturating_add(n);
    }
    points
}

/// The cubic curves kurbo divides an arc of `radius` sweeping `sweep` radians into at usvg's
/// tolerance (0.1 user units): `kurbo::Arc::append_iter`'s count, one more for where an end
/// rounded differently may take it. Not finite for a radius that isn't.
fn cubics(radius: f64, sweep: f64) -> f64 {
    let n_err = (1.1163 * radius / 0.1).powf(1.0 / 6.0).max(3.999_999);
    (n_err * sweep.abs() / std::f64::consts::TAU).ceil() + 1.0
}

/// What drawing `tree` with `t` into a `w` × `h` pixmap costs, in units ([`self`]'s module says
/// what one is); [`Refusal::TooMuch`] as soon as it passes `cap`, [`Refusal::Undrawn`] for one with
/// filters.
pub(super) fn work(tree: &usvg::Tree, (w, h): (u32, u32), t: tiny_skia::Transform, cap: u64) -> Result<u64, Refusal> {
    let (w, h) = (w as f32, h as f32);
    let canvas = tiny_skia::Rect::from_xywh(0.0, 0.0, w, h);
    // resvg clips a layer to the canvas grown by twice its size on each side.
    let reach = tiny_skia::Rect::from_ltrb(-2.0 * w, -2.0 * h, 3.0 * w, 3.0 * h).ok_or(Refusal::TooMuch)?;
    let mut cost = Cost { units: 0, cap, reach, clipping: false };
    // The pixmap made and cleared, and laid down as the marker's image.
    cost.add(pixels(canvas) / 4)?;
    cost.group(tree.root(), t, canvas, 0)?;
    Ok(cost.units)
}

/// The pixels a rectangle covers; 0 for none (one that doesn't overlap what it was cut to).
fn pixels(r: Option<tiny_skia::Rect>) -> u64 {
    r.map_or(0, |r| (f64::from(r.width()) * f64::from(r.height())).min(u64::MAX as f64) as u64)
}

/// `r` cut to `to`, where both are.
fn cut(r: Option<tiny_skia::Rect>, to: Option<tiny_skia::Rect>) -> Option<tiny_skia::Rect> {
    r?.intersect(&to?)
}

/// What a drawing has cost so far, against its cap.
struct Cost {
    units: u64,
    cap: u64,
    /// The farthest a layer reaches.
    reach: tiny_skia::Rect,
    /// Inside a clip path, where resvg draws each group of its own into a pixmap the clip's size.
    clipping: bool,
}

impl Cost {
    fn add(&mut self, units: u64) -> Result<(), Refusal> {
        self.units = self.units.saturating_add(units);
        if self.units > self.cap { Err(Refusal::TooMuch) } else { Ok(()) }
    }

    /// Group `g` drawn with `t` into `onto` (the pixmap it lands in, in pixels; `None` for one
    /// that is drawn nowhere). The only place nesting is bounded: every walk down comes here.
    fn group(&mut self, g: &usvg::Group, t: tiny_skia::Transform, onto: Option<tiny_skia::Rect>, depth: u32) -> Result<(), Refusal> {
        if depth > DEPTH {
            return Err(Refusal::TooNested);
        }
        if !g.filters().is_empty() {
            return Err(Refusal::Undrawn);
        }
        let t = t.pre_concat(g.transform());
        let onto = if g.should_isolate() {
            // A layer of its own: made, drawn into and laid onto what's under it; in a clip path,
            // the clip's whole pixmap (resvg's `clip_group`).
            let layer = if self.clipping {
                onto
            } else {
                let Some(bounds) = g.layer_bounding_box().and_then(|b| b.transform(t)) else { return Ok(()) };
                cut(bounds.to_rect().outset(2.0, 2.0), Some(self.reach))
            };
            let area = pixels(layer);
            self.add(NODE.saturating_add(area.saturating_mul(2)))?;
            // A clip path: a pixmap the layer's size, filled, drawn into and made a mask; a
            // mask: a pixmap and an alpha mask the layer's size, drawn into and applied. Each
            // with its own clip path or mask the same again.
            let mut clip = g.clip_path();
            while let Some(c) = clip {
                self.add(NODE.saturating_add(area.saturating_mul(4)))?;
                let outer = std::mem::replace(&mut self.clipping, true);
                let drawn = self.group(c.root(), t.pre_concat(c.transform()), layer, depth + 1);
                self.clipping = outer;
                drawn?;
                clip = c.clip_path();
            }
            let mut mask = g.mask();
            while let Some(m) = mask {
                self.add(NODE.saturating_add(area.saturating_mul(5)))?;
                self.group(m.root(), t, layer, depth + 1)?;
                mask = m.mask();
            }
            layer
        } else {
            self.add(NODE)?;
            onto
        };
        for node in g.children() {
            match node {
                usvg::Node::Group(child) => self.group(child, t, onto, depth + 1)?,
                usvg::Node::Path(path) => self.path(path, t, onto, depth)?,
                // Neither is drawn: images aren't resolved, text has no fonts.
                usvg::Node::Image(_) | usvg::Node::Text(_) => self.add(NODE)?,
            }
        }
        Ok(())
    }

    /// A path's fill and stroke drawn with `t` into `onto`.
    fn path(&mut self, path: &usvg::Path, t: tiny_skia::Transform, onto: Option<tiny_skia::Rect>, depth: u32) -> Result<(), Refusal> {
        self.add(NODE)?;
        if !path.is_visible() {
            return Ok(());
        }
        let shape = Shape::of(path.data(), t);
        let bounds = path.data().bounds().transform(t);
        if let Some(fill) = path.fill() {
            // Every row of its bounds within the pixmap, every row each edge crosses, and the
            // segments themselves.
            let covered = pixels(cut(bounds, onto));
            let blended = blended(fill.paint(), fill.opacity(), covered);
            self.add(blended.saturating_add(shape.rows).saturating_add(shape.segments.saturating_mul(NODE)))?;
            self.paint(fill.paint(), t, covered, depth)?;
        }
        if let Some(stroke) = path.stroke() {
            // Each piece (a segment, or a dash) is outlined and filled: its length by its width,
            // and the rows of its outline's edges. A piece far off the pixmap still costs its
            // outline, so the segments are counted whatever their place.
            let pieces = match stroke.dasharray() {
                Some(dashes) => {
                    let period: f32 = dashes.iter().sum();
                    let per = if period > 0.0 && period.is_finite() { f64::from(shape.user_length) / f64::from(period) } else { 0.0 };
                    ((per * dashes.len() as f64).ceil().min(u64::MAX as f64) as u64).max(shape.segments)
                }
                None => shape.segments,
            };
            // Its width in pixels (by the larger axis), with the pixel either side its edges touch.
            let (sx, sy) = t.get_scale();
            let reach = f64::from((stroke.width().get() * sx.max(sy)).max(0.0)) + 2.0;
            let area = blended(stroke.paint(), stroke.opacity(), (f64::from(shape.length) * reach).min(u64::MAX as f64) as u64);
            // An outline's edges cross its length and, at each piece's ends and joins, its width
            // in rows (measured: a dash costs some four times its width).
            let rows = (2.0 * f64::from(shape.length) + 4.0 * pieces as f64 * reach).min(u64::MAX as f64) as u64;
            self.add(area.saturating_add(rows).saturating_add(pieces.saturating_mul(NODE)))?;
            let covered = pixels(cut(bounds.and_then(|b| b.outset(reach as f32, reach as f32)), onto));
            self.paint(stroke.paint(), t, covered, depth)?;
        }
        Ok(())
    }

    /// What `paint` adds over a plain colour across `covered` pixels: a gradient computed at each
    /// (through each of its stops past two), a pattern's tile drawn (as often as it is used) and
    /// sampled at each.
    fn paint(&mut self, paint: &usvg::Paint, t: tiny_skia::Transform, covered: u64, depth: u32) -> Result<(), Refusal> {
        match paint {
            usvg::Paint::Color(_) => Ok(()),
            // Past two stops, each of them at each pixel too ([`STOPS_PER_UNIT`]).
            usvg::Paint::LinearGradient(g) => self.add(gradient(covered, g.stops().len())),
            usvg::Paint::RadialGradient(g) => self.add(gradient(covered, g.stops().len())),
            usvg::Paint::Pattern(pattern) => {
                let (sx, sy) = t.pre_concat(pattern.transform()).get_scale();
                let rect = pattern.rect();
                let (tw, th) = ((rect.width() * sx).round(), (rect.height() * sy).round());
                let tile = tiny_skia::Rect::from_xywh(0.0, 0.0, tw, th);
                self.add(covered.saturating_mul(2).saturating_add(pixels(tile)))?;
                let at = tiny_skia::Transform::from_scale(tw / rect.width(), th / rect.height());
                self.group(pattern.root(), at, tile, depth + 1)
            }
        }
    }
}

/// What a gradient of `stops` costs computed across `covered` pixels ([`STOPS_PER_UNIT`]).
fn gradient(covered: u64, stops: usize) -> u64 {
    let through = if stops > 2 { covered.saturating_mul(stops as u64) / STOPS_PER_UNIT } else { 0 };
    covered.saturating_add(through)
}

/// What `covered` pixels painted with `paint` at `opacity` cost: a plain opaque colour is copied
/// rather than blended (measured: some 0.3 ns a pixel against 9), so a sixteenth.
fn blended(paint: &usvg::Paint, opacity: usvg::Opacity, covered: u64) -> u64 {
    if matches!(paint, usvg::Paint::Color(_)) && opacity == usvg::Opacity::ONE { covered / 16 } else { covered }
}

/// A path's measures along its segments.
struct Shape {
    /// Its segments' lengths along their control points in pixels, at least as long as the curves.
    length: f32,
    /// The same before transforming, which its dashes are laid along.
    user_length: f32,
    /// The rows its edges cross.
    rows: u64,
    segments: u64,
}

impl Shape {
    fn of(data: &tiny_skia::Path, t: tiny_skia::Transform) -> Shape {
        use tiny_skia::PathSegment::*;
        let mut s = Shape { length: 0.0, user_length: 0.0, rows: 0, segments: 0 };
        // Where the path is (and where its contour started), before and after transforming.
        let both = |p: tiny_skia::Point| {
            let mut q = p;
            t.map_point(&mut q);
            (p, q)
        };
        let origin = (tiny_skia::Point::zero(), tiny_skia::Point::zero());
        let (mut start, mut at) = (origin, origin);
        for seg in data.segments() {
            let points: &[tiny_skia::Point] = match seg {
                MoveTo(p) => {
                    (start, at) = (both(p), both(p));
                    continue;
                }
                LineTo(p) => &[p],
                QuadTo(c, p) => &[c, p],
                CubicTo(c1, c2, p) => &[c1, c2, p],
                Close => &[start.0],
            };
            for &p in points {
                let next = both(p);
                s.length += at.1.distance(next.1);
                s.user_length += at.0.distance(next.0);
                s.rows = s.rows.saturating_add(((next.1.y - at.1.y).abs().ceil() as u64).saturating_add(1));
                at = next;
            }
            s.segments += 1;
        }
        s
    }
}

/// `tree` drawn with `t` into a `w` × `h` pixmap.
pub(super) fn draw(tree: &usvg::Tree, (w, h): (u32, u32), t: tiny_skia::Transform) -> Option<tiny_skia::Pixmap> {
    let mut pm = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(tree, t, &mut pm.as_mut());
    Some(pm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svg(body: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 100 100">{body}</svg>"#)
    }

    /// [`elements`] of `body`.
    fn element_count(body: &str) -> Option<u64> {
        roxmltree::Document::parse(&svg(body)).map(|d| with_sheet(&d, |sheet| elements(&d, sheet).ok())).unwrap()
    }

    /// What `body` costs drawn 100 × 100 pixels, a pixel a unit of its view.
    fn cost(body: &str) -> Result<u64, Refusal> {
        work(&read(&svg(body))?, (100, 100), tiny_skia::Transform::identity(), u64::MAX)
    }

    /// `<use>`s count as what they repeat, nested; a marker as often as the vertices it may sit
    /// on; one that repeats itself isn't counted at all.
    #[test]
    fn an_svg_counts_its_elements_where_they_repeat() {
        let doc = element_count;
        assert_eq!(doc(r#"<rect width="1" height="1"/>"#), Some(2));
        // With both, a `<use>` repeats what its plain `href` names, as usvg takes it (SVG 2).
        let both = r##"<g id="c"/><g id="b"><rect/><rect/><rect/></g><use xlink:href="#c" href="#b"/>"##;
        assert_eq!(doc(both), doc(r##"<g id="c"/><g id="b"><rect/><rect/><rect/></g><use href="#b"/>"##));
        // A rect used ten times by a group used ten times: the svg, the defs, the rect, the group
        // with its ten uses each with the rect (21), and ten uses of that (220).
        let nested = r##"<defs><rect id="r" width="1" height="1"/><g id="g">{uses}</g></defs>{top}"##
            .replace("{uses}", &r##"<use href="#r"/>"##.repeat(10))
            .replace("{top}", &r##"<use xlink:href="#g"/>"##.repeat(10));
        assert_eq!(doc(&nested), Some(1 + 1 + 1 + 21 + 220));
        let bomb =
            (1..=5).map(|l| format!(r##"<g id="l{l}">{}</g>"##, format!(r##"<use href="#l{}"/>"##, l - 1).repeat(10))).collect::<String>();
        assert_eq!(doc(&format!(r##"<rect id="l0" width="1" height="1"/>{bomb}<use href="#l5"/>"##)), None);
        assert_eq!(doc(r##"<g id="a"><use href="#a"/></g>"##), None, "a use of itself");
        // A marker of two elements on the points of the path of four segments that names it, both
        // ends counted; not on the circle inside it, which names none.
        let four = "M0 0 L1 1 L2 0 L3 1";
        assert_eq!(
            doc(&format!(r#"<marker id="m"><circle r="1"/></marker><path d="{four}" marker-mid="url(#m)"/>"#)),
            Some(4 + 2 * (4 + 2))
        );
        // A marker nothing names isn't drawn, however many points the shapes have (#2713).
        let zigzag = format!("M0 0{}", " L1 1".repeat(5000));
        assert_eq!(doc(&format!(r#"<marker id="m"><circle r="1"/></marker><path d="{zigzag}"/>"#)), Some(4));
        // A marker is counted on the vertices of the shapes that name it, by attribute or style,
        // themselves or what holds them (it is inherited), not on the others.
        let named = |on: &str| doc(&format!(r#"<marker id="m"><circle r="1"/></marker>{on}<path d="{zigzag}"/>"#));
        assert_eq!(named(&format!(r#"<path d="{four}" marker-end="url(#m)"/>"#)), Some(5 + 2 * 6));
        assert_eq!(named(&format!(r#"<g marker-start="url(#m)"><path d="{four}"/></g>"#)), Some(6 + 2 * 6));
        assert_eq!(named(&format!(r#"<g style="marker: url(#m)"><path d="{four}"/></g>"#)), Some(6 + 2 * 6));
        // On a `<use>`, the copy it makes carries it.
        assert_eq!(named(&format!(r##"<defs><path id="p" d="{four}"/></defs><use href="#p" marker-mid="url(#m)"/>"##)), Some(8 + 2 * 6));
        // A marker nothing names is on no vertex, even beside one that is set (#2713): its elements
        // count once.
        let unused = format!(r#"<marker id="u">{}</marker>"#, r#"<rect width="1" height="1"/>"#.repeat(3000));
        let path = format!("M0 0{}", " L1 1".repeat(2000));
        let beside = format!(r#"{unused}<marker id="m"><circle r="1"/></marker><path d="{path}" marker-mid="url(#m)"/>"#);
        assert_eq!(doc(&beside), Some(3005 + 2 * 2003));
        // A sheet's `marker: none` sets no marker, so it reaches no vertex (#2713).
        let big = format!(
            r#"<marker id="m"><circle r="1"/></marker><path d="M0 0 L1 1" marker-mid="url(#m)"/><path d="M0 0{}"/>"#,
            " L1 1".repeat(5000)
        );
        let styled = |rule: &str| doc(&format!("<style>path {{ {rule} }}</style>{big}"));
        assert_eq!(styled("marker: none"), styled("fill: red"));
        assert!(styled("fill: red").is_some());
        // Nor does `marker-end="none"` on a shape, by attribute or in its `style`.
        let none = |set: &str| doc(&big.replacen(r#"<path d="M0 0 L"#, &format!(r#"<path {set} d="M0 0 L"#), 2));
        assert_eq!(none(r#"marker-end="none""#), none(""));
        assert_eq!(none(r#"style="marker: none""#), none(""));
        // On the big path, past the elements.
        assert_eq!(doc(&format!(r#"<marker id="m"><circle r="1"/></marker><path d="{zigzag}" marker-mid="url(#m)"/>"#)), None);
        // A style sheet that sets a marker may reach any shape: every vertex counts.
        let sheet = |rule: &str| doc(&format!(r#"<style>{rule}</style><marker id="m"><circle r="1"/></marker><path d="{zigzag}"/>"#));
        assert_eq!(sheet("path { marker-mid: url(#m) }"), None);
        assert_eq!(sheet("path { MARKER : url(#m) }"), None);
        // Read as the reader reads it: past a comment before the colon, after a `*`.
        assert_eq!(sheet("path { marker-mid/* */: url(#m) }"), None);
        assert_eq!(sheet("path { *marker-end: url(#m) }"), None);
        assert_eq!(named(&format!(r#"<g style="marker-start /**/ : url(#m)"><path d="{four}"/></g>"#)), Some(6 + 2 * 6));
        // One that only names a class like a marker sets none (#2713).
        assert_eq!(sheet(".marker-dot { fill: red }"), Some(5));
        // A marker's points are copied on each vertex too: few elements, past the points.
        let half = format!("M0 0{}", " L1 1".repeat(2490));
        assert_eq!(doc(&format!(r#"<marker id="m"><path d="{half}"/></marker><path d="{half}" marker-mid="url(#m)"/>"#)), None);
        // A rect without rounded corners is a path of four corners, which a marker sits on too.
        let rects = r#"<rect width="1" height="1"/>"#.repeat(1000);
        assert_eq!(doc(&format!(r#"<marker id="m">{rects}</marker>{rects}"#)), Some(2002));
        let marked = r#"<rect width="1" height="1" marker-mid="url(#m)"/>"#.repeat(1000);
        assert_eq!(read(&svg(&format!(r#"<marker id="m">{rects}</marker>{marked}"#))).err(), Some(Refusal::TooMuch));
        let deep = format!("{}{}", "<g>".repeat(DEPTH as usize + 1), "</g>".repeat(DEPTH as usize + 1));
        assert_eq!(doc(&deep), None);
        // A path of 20,000 segments draws; used ten times its numbers are counted ten times, past
        // what reading takes.
        let long = format!(r#"<defs><path id="p" d="M0 0{}"/></defs>"#, " L1 1".repeat(20_000));
        assert!(doc(&format!(r##"{long}<use href="#p"/>"##)).is_some());
        assert_eq!(doc(&format!(r##"{long}{}"##, r##"<use href="#p"/>"##.repeat(10))), None);
        // usvg reads a link past the spaces around it.
        assert_eq!(doc(&format!(r##"{long}{}"##, r##"<use href=" #p "/>"##.repeat(10))), None);
        // Only ASCII spaces: an id ending in a no-break space keeps it, as svgtypes reads it.
        let nbsp = format!(r#"<defs><path id="p&#xA0;" d="M0 0{}"/></defs>"#, " L1 1".repeat(20_000));
        assert_eq!(doc(&format!(r##"{nbsp}{}"##, r##"<use href="#p&#xA0;"/>"##.repeat(10))), None);
        assert!(doc(&format!(r##"{long}<use href=" #p "/>"##)).is_some());
    }

    /// An arc is counted as the curves usvg divides it into, never fewer: a path's, its radii
    /// stretched to reach its ends as the reader stretches them, and a circle's, an ellipse's and a
    /// rounded rectangle's. One past [`ARC_CUBICS`] is refused before it is read, quickly: a
    /// 24-byte circle of radius 10³⁸ came to 9.6 million points, and the path's arc below to so
    /// many that reading it never finished.
    #[test]
    fn an_svgs_arcs_are_counted_as_drawn() {
        fn drawn(g: &usvg::Group) -> u64 {
            g.children()
                .iter()
                .map(|n| match n {
                    usvg::Node::Group(g) => drawn(g),
                    usvg::Node::Path(p) => p.data().points().len() as u64,
                    _ => 0,
                })
                .sum()
        }
        let counted = |body: &str| {
            let text = svg(body);
            let doc = roxmltree::Document::parse(&text).unwrap();
            let shape = doc.descendants().find(|n| ["path", "circle", "ellipse", "rect"].contains(&n.tag_name().name())).unwrap();
            (points_of(shape, viewport(&doc)), read(&text).map(|t| drawn(t.root())))
        };
        for body in [
            r#"<path d="M0 0A1 1 0 1 1 1 1"/>"#,
            r#"<path d="M0 0a50 50 0 1 0 1 1 30 10 45 0 1 9 9z"/>"#,
            r#"<path d="M10 10A0.001 5 0 1 1 90 90" stroke="black"/>"#,
            r#"<path d="M0 0A1e9 1e9 0 1 1 1 0" stroke="black"/>"#,
            r#"<circle r="10"/>"#,
            r#"<circle r="1e12"/>"#,
            r#"<ellipse rx="3in" ry="1"/>"#,
            r#"<rect width="80" height="80" rx="1e15"/>"#,
            // Quadratic curves, smooth ones too, as the points usvg builds of them.
            r#"<path d="M0 0Q5 9 10 0T20 0t10 0q5 5 10 0"/>"#,
            // A radius in % of the view box (100 a side), one of 0 in any unit (#2713).
            r#"<circle r="50%"/>"#,
            r#"<ellipse rx="40%" ry="1e3%"/>"#,
            r#"<rect width="80" height="80" rx="10%"/>"#,
            r#"<rect width="80" height="80" rx="0em" ry="0ex"/>"#,
            r#"<circle r="0%"/>"#,
        ] {
            let (points, drawn) = counted(body);
            let drawn = drawn.unwrap_or_else(|why| panic!("{body}: {why:?}"));
            assert!(points >= drawn && points < POINTS, "{body}: {points} counted, {drawn} drawn");
        }
        // A % radius in a viewport whose size isn't known (a nested one in %) counts as the
        // largest it may come to, as one in em or ex does.
        let nested = svg(r#"<svg width="50%" height="50%"><circle r="50%"/></svg>"#);
        assert_eq!(read(&nested).err(), Some(Refusal::TooMuch));
        // Against a nested viewport of its own size.
        let large = svg(r#"<svg viewBox="0 0 1e30 1e30"><circle r="50%"/></svg>"#);
        assert_eq!(read(&large).err(), Some(Refusal::TooMuch));
        for body in [
            r#"<circle r="1e38"/>"#,
            r#"<path d="M0 0A1e30 0.00002 0 1 1 1e30 1e30" stroke="black"/>"#,
            r#"<path d="M0 0A1e300 1e300 0 1 1 1 1"/>"#,
            r#"<circle r="1e30%"/>"#,
            // Past [`ARC_CUBICS`]: some 220 curves to an arc nearly round (#2713).
            r#"<path d="M0 0A1e13 1e13 0 1 1 1 0" stroke="black"/>"#,
        ] {
            let started = std::time::Instant::now();
            assert_eq!((counted(body).0, read(&svg(body)).err()), (u64::MAX, Some(Refusal::TooMuch)), "{body}");
            assert!(started.elapsed() < std::time::Duration::from_secs(1), "{body}");
        }
        // A radius in em or ex, a font's size, is refused as such (#2713).
        let em = r#"<rect width="9" height="9" rx="2em"/>"#;
        assert_eq!((counted(em).0, read(&svg(em)).err()), (u64::MAX, Some(Refusal::FontRadius)));
    }

    /// What isn't an SVG, or reads past its bounds, is refused while it is read: an entity loop,
    /// more XML than [`XML_NODES`], nesting past [`DEPTH`].
    #[test]
    fn an_svg_is_read_within_its_bounds() {
        assert!(read(&svg(r#"<rect width="1" height="1"/>"#)).is_ok());
        assert_eq!(read("<html/>").err(), Some(Refusal::NotSvg));
        assert_eq!(read("not xml").err(), Some(Refusal::NotSvg));
        let laughs = r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;"><!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;"><!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;"><!ENTITY e "&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;"><!ENTITY f "&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;"><!ENTITY g "&f;&f;&f;&f;&f;&f;&f;&f;&f;&f;">]><svg xmlns="http://www.w3.org/2000/svg"><desc>&g;</desc></svg>"#;
        assert_eq!(read(laughs).err(), Some(Refusal::NotSvg));
        // The same in an attribute's value, quickly.
        let started = std::time::Instant::now();
        assert_eq!(read(&laughs.replace("<desc>&g;</desc>", r#"<path d="&g;"/>"#)).err(), Some(Refusal::NotSvg));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(read(&svg(&"<!-- -->".repeat(XML_NODES as usize))).err(), Some(Refusal::TooNested));
        let deep = format!("{}{}", "<g>".repeat(2000), "</g>".repeat(2000));
        assert_eq!(read(&svg(&deep)).err(), Some(Refusal::TooNested));
        // A marker on a shape inside another marker is drawn on each of that marker's vertices
        // too, which isn't counted: refused, by attribute, inherited or from a style sheet.
        let zigzag = "M0 0L1 1L2 0L3 1";
        for nested in [
            format!(r#"<marker id="b"><circle r="1"/></marker><marker id="a"><path d="{zigzag}" marker-mid="url(#b)"/></marker>"#),
            format!(
                r#"<marker id="b"><circle r="1"/></marker><marker id="a"><g style="marker-mid: url(#b)"><path d="{zigzag}"/></g></marker>"#
            ),
            format!(
                r#"<style>path {{ marker-mid: url(#b) }}</style><marker id="b"><circle r="1"/></marker><marker id="a"><path d="{zigzag}"/></marker>"#
            ),
            format!(
                r#"<style>path {{ marker-mid/* */: url(#b) }}</style><marker id="b"><circle r="1"/></marker><marker id="a"><path d="{zigzag}"/></marker>"#
            ),
            // A shape a `<use>` inside a marker repeats, with a marker of its own or one inherited
            // from that marker.
            format!(
                r##"<marker id="b"><circle r="1"/></marker><path id="p" d="{zigzag}" marker-mid="url(#b)"/><marker id="a"><use href="#p"/></marker>"##
            ),
            format!(
                r##"<marker id="b"><circle r="1"/></marker><path id="p" d="{zigzag}"/><marker id="a" marker-mid="url(#b)"><use href="#p"/></marker>"##
            ),
        ] {
            let body = format!(r#"{nested}<path d="{zigzag}" marker-mid="url(#a)"/>"#);
            assert_eq!(read(&svg(&body)).err(), Some(Refusal::TooMuch), "{body}");
        }
        // A `<use>` passes its marker to the copy it makes, with no shape naming one.
        let used = format!(
            r##"<defs><path id="p" d="{zigzag}"/><path id="q" d="{zigzag}"/></defs><marker id="b"><circle r="1"/></marker><marker id="a"><use href="#p" marker-mid="url(#b)"/></marker><use href="#q" marker-mid="url(#a)"/>"##
        );
        assert_eq!(read(&svg(&used)).err(), Some(Refusal::TooMuch));
        // Two markers side by side, or one with a style sheet (it can only nest itself, which
        // usvg drops), are read.
        let side = format!(
            r#"<marker id="a"><circle r="1"/></marker><marker id="b"><circle r="1"/></marker><path d="{zigzag}" marker-start="url(#a)" marker-end="url(#b)"/>"#
        );
        assert!(read(&svg(&side)).is_ok());
        // With a style sheet that only names a class like a marker, they are read too (#2713).
        let classed = format!(r#"<style>.marker-dot {{ fill: red }}</style>{side}"#);
        assert!(read(&svg(&classed)).is_ok());
        let one =
            format!(r#"<style>path {{ marker-mid: url(#a) }}</style><marker id="a"><path d="M0 0L1 1"/></marker><path d="{zigzag}"/>"#);
        assert!(read(&svg(&one)).is_ok());
        // The same through a `<use>` (usvg follows its copy and drops the cycle), quickly.
        let started = std::time::Instant::now();
        let cycle = format!(
            r##"<path id="p" d="{zigzag}" marker-mid="url(#a)"/><marker id="a"><use href="#p"/></marker><path d="{zigzag}" marker-mid="url(#a)"/>"##
        );
        assert!(read(&svg(&cycle)).is_ok());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    /// Filters are refused, wherever the group with them is.
    #[test]
    fn an_svg_with_filters_is_refused() {
        let blur = r#"<filter id="f"><feGaussianBlur stdDeviation="3"/></filter>"#;
        assert_eq!(cost(&format!(r#"{blur}<rect width="9" height="9" filter="url(#f)"/>"#)), Err(Refusal::Undrawn));
        assert_eq!(
            cost(&format!(r#"{blur}<g><g opacity="0.5"><rect width="9" height="9" filter="url(#f)"/></g></g>"#)),
            Err(Refusal::Undrawn)
        );
    }

    /// The cost follows what the rasterizer does: a blended pixel a unit and a plain opaque one a
    /// sixteenth, each dash of a stroke, each layer's pixels.
    #[test]
    fn an_svgs_cost_follows_what_it_draws() {
        let base = cost("").unwrap();
        assert_eq!(base, 100 * 100 / 4 + NODE, "the pixmap and the root");
        let opaque = cost(r##"<rect width="100" height="100" fill="#a00"/>"##).unwrap() - base;
        let half = cost(r##"<rect width="100" height="100" fill="#a00" fill-opacity="0.5"/>"##).unwrap() - base;
        // Its 10,000 pixels, a sixteenth of them opaque; the rest the same: its node, 4 segments
        // and the rows of its two upright edges (each 100 + 1) and two flat ones (1 each).
        let rest = NODE + 4 * NODE + 2 * 101 + 2;
        assert_eq!((opaque, half), (10_000 / 16 + rest, 10_000 + rest));
        // A rect four times as large off the pixmap costs no more pixels.
        let off = cost(r##"<rect x="-50" y="-50" width="200" height="200" fill="#a00" fill-opacity="0.5"/>"##).unwrap() - base;
        assert_eq!(off, 10_000 + NODE + 4 * NODE + 2 * 201 + 2);
        // A layer (a group with opacity) adds its pixels twice, grown 2 px a side.
        let layered = cost(r##"<g opacity="0.5"><rect width="10" height="10" fill="#a00"/></g>"##).unwrap();
        let plain = cost(r##"<g><rect width="10" height="10" fill="#a00"/></g>"##).unwrap();
        assert_eq!(layered - plain, 2 * 14 * 14);
        // Dashes: finer ones cost more, as many more as there are.
        let dashed = |every: f32| {
            cost(&format!(r##"<rect width="100" height="100" fill="none" stroke="#000" stroke-dasharray="{every} {every}"/>"##)).unwrap()
        };
        assert!(dashed(0.5) > dashed(5.0) * 5);
        // A gradient is computed at each pixel it covers; a pattern's tile drawn and sampled.
        let gradient = cost(r#"<linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient><rect width="100" height="100" fill="url(#g)"/>"#).unwrap();
        assert!(gradient - base >= 2 * 10_000);
        let tile = |side: u32| {
            cost(&format!(r#"<pattern id="p" width="{side}" height="{side}" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern><rect width="100" height="100" fill="url(#p)"/>"#)).unwrap()
        };
        assert_eq!(tile(1000) - tile(10), 1000 * 1000 - 10 * 10);
        // Inside a clip path, resvg draws a shape with a clip path of its own into a pixmap the
        // enclosing clip's size (`clip_group`), and clips that the same: however small the shape.
        let clipped = |n: usize| {
            let inner = r#"<rect width="1" height="1" clip-path="url(#s)"/>"#.repeat(n);
            cost(&format!(r#"<clipPath id="s"><rect width="1" height="1"/></clipPath><clipPath id="c"><rect width="100" height="100"/>{inner}</clipPath><g clip-path="url(#c)"><rect width="100" height="100"/></g>"#)).unwrap()
        };
        assert!(clipped(10) - clipped(1) >= 9 * 6 * 10_000, "{} {}", clipped(10), clipped(1));
    }

    /// Working out the cost stops as soon as it passes the cap.
    #[test]
    fn an_svgs_cost_stops_at_its_cap() {
        let tree = read(&svg(&r##"<rect width="100" height="100" fill="#a00" fill-opacity="0.5"/>"##.repeat(100))).unwrap();
        assert!(work(&tree, (100, 100), tiny_skia::Transform::identity(), u64::MAX).is_ok_and(|u| u > 100 * 10_000));
        assert_eq!(work(&tree, (100, 100), tiny_skia::Transform::identity(), 50_000), Err(Refusal::TooMuch));
        // An edge past what a count of rows holds (10²⁰ down, drawn 256 times larger) counts as
        // all of them, refused rather than overflowing.
        let tall = read(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><path d="M0 0 L0 1e20"/></svg>"#).unwrap();
        assert_eq!(
            work(&tall, (256, 256), tiny_skia::Transform::from_scale(256.0, 256.0), crate::picture::MARKER_PIXELS),
            Err(Refusal::TooMuch)
        );
    }

    /// Text, images and what an SVG takes from other files aren't drawn, so an SVG with them is
    /// refused rather than drawn without them, and no file it names is read; an editor's own data
    /// in a `<switch>` that needs an extension passes, and so do a link's address and what an
    /// SVG names of its own. Ids resolve to their first element, as usvg resolves them.
    #[test]
    fn an_svg_with_what_isnt_drawn_is_refused() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
        for undrawn in [
            r#"<image href="/etc/hosts" width="100" height="100"/>"#.to_string(),
            format!(r#"<image xlink:href="data:image/png;base64,{png}" width="100" height="100"/>"#),
            r#"<text x="1" y="9">Pin</text>"#.to_string(),
            r#"<foreignObject width="9" height="9"/>"#.to_string(),
            // usvg takes the first child of a switch that needs no extension: one drawing nothing.
            r#"<switch><foreignObject width="9" height="9"/><rect width="9" height="9"/></switch>"#.to_string(),
            r#"<use href="pins.svg#pin"/>"#.to_string(),
            r#"<use xlink:href="data:image/svg+xml,%3Csvg/%3E"/>"#.to_string(),
            r#"<rect width="9" height="9" fill="url(paints.svg#red)"/>"#.to_string(),
            r#"<rect width="9" height="9" style="fill: url( 'paints.svg#red' )"/>"#.to_string(),
            r#"<linearGradient id="g" href="paints.svg#base"/><rect width="9" height="9" fill="url(#g)"/>"#.to_string(),
            r#"<style>@import "pins.css";</style><rect width="9" height="9"/>"#.to_string(),
            r#"<style>@IMPORT "pins.css";</style><rect width="9" height="9"/>"#.to_string(),
            r#"<rect width="9" height="9" fill="URL(paints.svg#red)"/>"#.to_string(),
            r#"<style>@keyframes s { to { opacity: 0 } } rect { animation: s 1s }</style><rect width="9" height="9"/>"#.to_string(),
            r#"<style>@-webkit-keyframes s { to { opacity: 0 } } rect { -webkit-animation: s 1s }</style><rect width="9" height="9"/>"#
                .to_string(),
            r#"<style>@-moz-keyframes s { to { opacity: 0 } } rect { -moz-animation: s 1s }</style><rect width="9" height="9"/>"#
                .to_string(),
            r#"<style>rect { fill: url(paints.svg#red) }</style><rect width="9" height="9"/>"#.to_string(),
        ] {
            assert_eq!(read(&svg(&undrawn)).err(), Some(Refusal::Undrawn), "{undrawn}");
        }
        // Animation is lost on a still image; a style sheet split by a comment is read whole.
        for moving in [
            r#"<rect width="9" height="9"><animate attributeName="x" to="5" dur="1s"/></rect>"#,
            r#"<g><set attributeName="opacity" to="0"/></g>"#,
        ] {
            assert_eq!(read(&svg(moving)).err(), Some(Refusal::Undrawn), "{moving}");
        }
        assert_eq!(read(&svg(r#"<style>@im<!-- -->port "pins.css";</style>"#)).err(), Some(Refusal::Undrawn));
        let linked = r#"<?xml-stylesheet href="pins.css"?><svg xmlns="http://www.w3.org/2000/svg"><rect width="9" height="9"/></svg>"#;
        assert_eq!(read(linked).err(), Some(Refusal::Undrawn));
        // A class named after keyframes is no animation.
        assert!(read(&svg(r#"<style>.no-keyframes { fill: red }</style><rect class="no-keyframes" width="9" height="9"/>"#)).is_ok());
        let switched = r#"<switch><foreignObject requiredExtensions="http://ns.example/editor" width="1" height="1"/><g><rect width="9" height="9"/></g></switch>"#;
        assert!(read(&svg(switched)).is_ok());
        let own = r##"<linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
            <style>rect { stroke: url("#g") }</style>
            <a href="https://example.org/pin"><rect id="r" width="9" height="9" fill="url(#g)"/></a>
            <use href="#r" x="1"/>"##;
        assert!(read(&svg(own)).is_ok());
        // A large group and a small rect sharing an id: the group, first, is what a use repeats.
        let big = format!(r#"<g id="x">{}</g>"#, r#"<rect width="1" height="1"/>"#.repeat(2000));
        let twice = format!(r##"<defs>{big}<rect id="x" width="1" height="1"/></defs>{}"##, r##"<use href="#x"/>"##.repeat(5));
        assert_eq!(read(&svg(&twice)).err(), Some(Refusal::TooMuch));
    }

    /// A polyline's points are its numbers as the reader takes them, two to a point (#2713).
    #[test]
    fn a_polylines_points_are_its_pairs_of_numbers() {
        let points = |tag: &str, list: &str| {
            let text = svg(&format!(r#"<{tag} points="{list}"/>"#));
            let doc = roxmltree::Document::parse(&text).unwrap();
            points_of(doc.descendants().find(|n| n.has_tag_name(tag)).unwrap(), viewport(&doc))
        };
        assert_eq!(points("polyline", "12.345,67.891 10.5,20.25 30.125,40.5"), 3 + 2);
        assert_eq!(points("polygon", "1e5,2 -3.5-4 .5.5"), 3 + 2);
        assert_eq!(points("polyline", ""), 2);
    }

    /// Matching a style sheet's rules to the elements is bounded, by the steps the matcher takes:
    /// many rules on many elements, or a selector of many descendants deep down (#2713).
    #[test]
    fn a_style_sheet_is_matched_within_its_bound() {
        let rects = |n: usize| (0..n).map(|i| format!(r#"<rect class="c{i}" width="1" height="1"/>"#)).collect::<String>();
        let rules = |n: usize| (0..n).map(|i| format!(".c{i} {{ fill: red }}")).collect::<String>();
        assert!(read(&svg(&format!("<style>{}</style>{}", rules(20), rects(2000)))).is_ok());
        assert_eq!(read(&svg(&format!("<style>{}</style>{}", rules(2000), rects(2000)))).err(), Some(Refusal::TooMuch));
        // usvg reads what a `<use>` repeats again for each `<use>`, and matches the rules again: a
        // rect used 1,000 times, under a sheet of 1,000 rules.
        let used = format!(
            r##"<defs><rect id="r" class="c0" width="1" height="1"/><g id="g">{}</g></defs>{}"##,
            r##"<use href="#r"/>"##.repeat(10),
            r##"<use href="#g"/>"##.repeat(100)
        );
        assert_eq!(read(&svg(&format!("<style>{}</style>{used}", rules(1000)))).err(), Some(Refusal::TooMuch));
        assert!(read(&svg(&format!("<style>{}</style>{used}", rules(20)))).is_ok());
        // Each descendant in a selector tries every element above: a few of them, 40 levels down.
        let deep = format!("{}<rect width=\"1\" height=\"1\"/>{}", "<g>".repeat(40), "</g>".repeat(40));
        assert!(read(&svg(&format!("<style>g rect {{ fill: red }}</style>{deep}"))).is_ok());
        assert_eq!(read(&svg(&format!("<style>x g g g g g g rect {{ fill: red }}</style>{deep}"))).err(), Some(Refusal::TooMuch));
    }

    /// Matching stops once the steps taken for all the elements matched so far pass
    /// [`CSS_STEPS`], not one element's: a selector of four descendants on 60 nested groups
    /// takes each of them at most C(60, 4) steps, under the cap, and all of them C(61, 5) (#2713).
    #[test]
    fn style_sheet_matching_stops_at_its_cap() {
        let deep = format!("{}{}", "<g>".repeat(60), "</g>".repeat(60));
        let text = svg(&format!("<style>y g g g g {{ fill: red }}</style>{deep}"));
        let doc = roxmltree::Document::parse(&text).unwrap();
        with_sheet(&doc, |sheet| {
            let (ids, seen) = (std::collections::HashMap::new(), std::collections::HashMap::new());
            let mut counter = Counter { ids, seen, viewport: 100.0, sheet, matched: 0, deep: false };
            assert!(counter.count(doc.root_element(), 0).is_none());
            assert!(counter.matched <= CSS_STEPS + 1, "{} steps", counter.matched);
        });
    }

    /// The siblings walked back past, comments included, are steps too (#2713): 100 rules on an
    /// element after 30,000 comments walk 3,000,000 nodes.
    #[test]
    fn style_sheet_matching_counts_the_siblings_it_walks() {
        let rules = "rect:first-child { fill: red } ".repeat(100);
        let after = |what: &str| element_count(&format!("<style>{rules}</style><g>{}<rect/></g>", what.repeat(30_000)));
        assert_eq!(after(""), Some(4));
        assert_eq!(after("<!---->"), None);
    }

    /// An attribute is matched along its value: a long one costs a step each 64 bytes (#2713).
    #[test]
    fn style_sheet_matching_counts_the_bytes_it_reads() {
        let rules = (0..200).map(|i| format!(".c{i} {{ fill: red }}")).collect::<String>();
        let class = |bytes: usize| element_count(&format!(r#"<style>{rules}</style><rect class="{}"/>"#, "a".repeat(bytes)));
        assert_eq!(class(10), Some(3));
        assert_eq!(class(500_000), None);
    }

    /// A gradient of more than two stops goes through them all at each pixel it covers.
    #[test]
    fn a_gradients_stops_are_counted() {
        let painted = |stops: usize| {
            let stops = (0..stops).map(|i| format!(r#"<stop offset="{}" stop-color="red"/>"#, i as f32 / stops as f32)).collect::<String>();
            cost(&format!(r#"<linearGradient id="g">{stops}</linearGradient><rect width="100" height="100" fill="url(#g)"/>"#)).unwrap()
        };
        assert!(painted(1000) - painted(2) >= 10_000 * 1000 / STOPS_PER_UNIT, "{} {}", painted(1000), painted(2));
    }

    /// A subtree counted once where it is shallow is counted again where a `<use>` reaches it
    /// deeper, so nesting past [`DEPTH`] is refused while counting, never by the reader (#2713).
    #[test]
    fn a_used_subtree_is_bounded_where_it_is_deep() {
        let tree = format!(r#"<g id="t">{}<rect width="1" height="1"/>{}</g>"#, "<g>".repeat(40), "</g>".repeat(40));
        let deep = format!(r##"{}<use href="#t"/>{}"##, "<g>".repeat(40), "</g>".repeat(40));
        let doc = element_count;
        assert!(doc(&tree).is_some());
        assert_eq!(doc(&format!("{tree}{deep}")), None);
        assert_eq!(read(&svg(&format!("{tree}{deep}"))).err(), Some(Refusal::TooNested));
    }

    /// Geometry at the ends of what a float holds is drawn without panicking, when what it costs
    /// lets it be drawn at all.
    #[test]
    fn hostile_geometry_is_drawn_without_panicking() {
        let at = tiny_skia::Transform::identity();
        let mut drawn = 0;
        for body in [
            r#"<path d="M-3e38 -3e38L3e38 3e38L-3e38 3e38z" fill="red" stroke="blue" stroke-width="3e38"/>"#,
            r#"<path d="M0 0L100 0L0 100" fill="none" stroke="red" stroke-width="1e30" stroke-miterlimit="1e30"/>"#,
            r#"<path d="M0 0L100 100" stroke="red" stroke-dasharray="1e-30 1e-30"/>"#,
            r#"<path d="M0 0L100 100" stroke="red" stroke-dasharray="3e38 1e-30" stroke-dashoffset="3e38"/>"#,
            r#"<g transform="matrix(3e38 0 0 3e38 -3e38 -3e38)"><rect width="1" height="1"/></g>"#,
            r#"<g transform="scale(1e-38)"><circle r="1e38" stroke="red"/></g>"#,
            r#"<g transform="skewX(89.9999)"><rect width="100" height="100" stroke="red"/></g>"#,
        ] {
            let Ok(tree) = read(&svg(body)) else { continue };
            if work(&tree, (100, 100), at, crate::picture::MARKER_PIXELS).is_ok() {
                let _ = draw(&tree, (100, 100), at);
                drawn += 1;
            }
        }
        assert!(drawn > 0, "none was drawn, so none was tested");
    }
}
