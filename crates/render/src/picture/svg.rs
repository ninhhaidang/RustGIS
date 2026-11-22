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
//! what it draws is worked out first ([`work`]),
//! from what the rasterizer does: the pixels each shape covers and the rows its edges cross, each
//! dash and each segment of a stroke, each layer a group with opacity, a clip or a mask draws
//! into. A unit is a pixel blended, about 13 ns, the dearest the rasterizer spends on one. The
//! measured cases all came within ~13 ns a unit; an icon is a few hundred thousand units.
//!
//! Filters are refused outright: they cost per pixel by kind and parameter (a blur's radius, a
//! turbulence's octaves, a morphology's, a convolution's), which a marker's icon rarely needs and
//! a count can't bound without modelling each one. So is text (no fonts are loaded for it) and an
//! image inside the SVG or one it links to (it would pass the image caps, or read a file): drawn
//! without them, an icon would lose them unsaid. So is animation (the still image is drawn) and
//! anything it takes from another file (a `<use>`, a paint or a style sheet): no file is read.

use resvg::usvg;
use usvg::roxmltree;

/// Why an SVG isn't taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// It isn't an SVG that reads.
    NotSvg,
    /// It has more to draw than the cap given, or more elements than [`ELEMENTS`].
    TooMuch,
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
/// (a radius of 10⁶ to 13), and the reader moves each of an arc's curves across all the others
/// still to go (svgtypes' `SimplifyingPathParser`), so one of millions never finishes.
const ARC_CUBICS: f64 = 1_000.0;
/// The most XML nodes (elements, text, comments) read before giving up, a bound on reading the
/// text itself: [`ELEMENTS`] and the text and spaces between them.
const XML_NODES: u32 = 4 * ELEMENTS as u32;
/// The deepest nesting read (usvg's own bound is 1,024): icons nest a few levels, and the walks
/// here recurse as deep.
const DEPTH: u32 = 64;
/// What each element and each segment costs whatever its size, in units.
const NODE: u64 = 100;

/// SVG text `text` read into a tree: within [`XML_NODES`], [`ELEMENTS`] and [`POINTS`], its root
/// an `<svg>`, with no images resolved and no files read.
pub(super) fn read(text: &str) -> Result<usvg::Tree, Refusal> {
    let xml = roxmltree::ParsingOptions { allow_dtd: true, nodes_limit: XML_NODES, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, xml).map_err(|e| match e {
        roxmltree::Error::NodesLimitReached => Refusal::TooMuch,
        _ => Refusal::NotSvg,
    })?;
    if doc.root_element().tag_name().name() != "svg" {
        return Err(Refusal::NotSvg);
    }
    if doc.descendants().any(undrawn) {
        return Err(Refusal::Undrawn);
    }
    if elements(&doc).is_none() || nested_markers(&doc) {
        return Err(Refusal::TooMuch);
    }
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
fn nested_markers(doc: &roxmltree::Document) -> bool {
    let markers: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("marker")).collect();
    if markers.len() < 2 {
        return false;
    }
    let carries = |n: roxmltree::Node| {
        n.attributes().any(|a| {
            matches!(a.name(), "marker" | "marker-start" | "marker-mid" | "marker-end")
                || (a.name() == "style" && a.value().to_ascii_lowercase().contains("marker"))
        })
    };
    let sheet = doc.descendants().filter(|n| n.has_tag_name("style")).any(|n| sheet_of(n).to_ascii_lowercase().contains("marker"));
    let uses = markers.iter().any(|m| m.descendants().any(|n| n.has_tag_name("use")));
    sheet || markers.iter().any(|m| m.ancestors().any(carries) || m.descendants().any(carries)) || (uses && doc.descendants().any(carries))
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
/// often as the vertices it can sit on; `None` past [`ELEMENTS`], its shapes' points ([`points_of`])
/// counted so past [`POINTS`], nested past [`DEPTH`] or where a `<use>` repeats itself (usvg drops
/// those, but counting it is no use).
fn elements(doc: &roxmltree::Document) -> Option<u64> {
    // The first element of an id, as usvg takes it: counting another would count what isn't drawn.
    let mut ids = std::collections::HashMap::new();
    for n in doc.descendants() {
        if let Some(id) = n.attribute("id") {
            ids.entry(id).or_insert(n);
        }
    }
    let mut seen = std::collections::HashMap::new();
    let (elements, vertices) = count(doc.root_element(), &ids, &mut seen, 0)?;
    // Markers may sit on every vertex (`marker-mid`) and both ends of every shape, here or through
    // a style sheet, so each marker, its elements and their points, is counted as if on all of them.
    let (markers, marked) = doc.descendants().filter(|n| n.has_tag_name("marker")).try_fold((0u64, 0u64), |sum, m| {
        let (e, p) = count(m, &ids, &mut seen, 0)?;
        Some((sum.0.saturating_add(e), sum.1.saturating_add(p)))
    })?;
    let total = elements.saturating_add(markers.saturating_mul(vertices));
    let points = vertices.saturating_add(marked.saturating_mul(vertices));
    (total <= ELEMENTS && points <= POINTS).then_some(total)
}

/// [`elements`] of the element `n` and what's under it, with the points of its shapes, memoized in
/// `seen` by node (`None` there marks one being counted: a cycle).
fn count<'a>(
    n: roxmltree::Node<'a, 'a>,
    ids: &std::collections::HashMap<&str, roxmltree::Node<'a, 'a>>,
    seen: &mut std::collections::HashMap<roxmltree::NodeId, Option<(u64, u64)>>,
    depth: u32,
) -> Option<(u64, u64)> {
    if depth > DEPTH {
        return None;
    }
    match seen.get(&n.id()) {
        Some(Some(done)) => return Some(*done),
        Some(None) => return None,
        None => {}
    }
    seen.insert(n.id(), None);
    // A plain `href` before an XLink one, as usvg takes them (SVG 2).
    let named = |ns: Option<&str>| n.attributes().find(|a| a.name() == "href" && a.namespace() == ns).map(|a| a.value());
    let href = named(None).or_else(|| named(Some("http://www.w3.org/1999/xlink")));
    // What a `<use>` repeats counts as if it were under it, its link read as usvg reads it.
    let used = n.has_tag_name("use").then(|| href.and_then(|h| svgtypes::IRI::from_str(h).ok()).and_then(|iri| ids.get(iri.0))).flatten();
    let mut total = (1u64, points_of(n));
    for under in n.children().filter(|c| c.is_element()).chain(used.copied()) {
        let (e, v) = count(under, ids, seen, depth + 1)?;
        total = (total.0.saturating_add(e), total.1.saturating_add(v));
        if total.0 > ELEMENTS {
            return None;
        }
    }
    seen.insert(n.id(), Some(total));
    Some(total)
}

/// The points shape element `n` comes to once read, both ends counted (a marker may sit on each):
/// what each segment of its path data holds, an arc as the curves it is divided into
/// ([`cubics`]); a circle, an ellipse and a rounded rectangle as four quarter arcs; at most one a
/// pair of a polyline's runs of digits (`1.5` and `.1.2` are two numbers, `1e5` one). `u64::MAX`
/// for an arc past [`ARC_CUBICS`].
fn points_of(n: roxmltree::Node) -> u64 {
    let corners = |names: &[&str]| {
        // usvg resolves lengths in f32: a radius past it isn't drawn, one in em, ex or % (of the
        // font or the viewport, which may be any size) is counted as the largest it may come to.
        let radius = names.iter().filter_map(|&a| n.attribute(a)).fold(0.0f64, |r, v| r.max(length(v)));
        let c = cubics(radius, std::f64::consts::FRAC_PI_2);
        if c > ARC_CUBICS { u64::MAX } else { 4 * 3 * c as u64 + 5 }
    };
    let points = match n.tag_name().name() {
        "path" => n.attribute("d").map_or(0, path_points),
        "polyline" | "polygon" => {
            n.attribute("points").map_or(0, |p| p.split(|c: char| !c.is_ascii_digit()).filter(|t| !t.is_empty()).count() as u64 / 2)
        }
        "line" => 0,
        "circle" => corners(&["r"]),
        "ellipse" => corners(&["rx", "ry"]),
        "rect" if n.has_attribute("rx") || n.has_attribute("ry") => corners(&["rx", "ry"]),
        "rect" => 4,
        _ => return 0,
    };
    points.saturating_add(2)
}

/// Length `v` in user units at most, as usvg resolves it (96 dpi): the largest an f32 holds for one
/// relative to a font or a viewport, 0 for one that doesn't read (usvg takes none).
fn length(v: &str) -> f64 {
    use svgtypes::LengthUnit::*;
    let Ok(l) = v.trim().parse::<svgtypes::Length>() else { return 0.0 };
    let per = match l.unit {
        None | Px => 1.0,
        In => 96.0,
        Cm => 96.0 / 2.54,
        Mm => 96.0 / 25.4,
        Pt => 96.0 / 72.0,
        Pc => 16.0,
        Em | Ex | Percent => return f64::from(f32::MAX),
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
            return Err(Refusal::TooMuch);
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

    /// What `paint` adds over a plain colour across `covered` pixels: a gradient computed at each,
    /// a pattern's tile drawn (as often as it is used) and sampled at each.
    fn paint(&mut self, paint: &usvg::Paint, t: tiny_skia::Transform, covered: u64, depth: u32) -> Result<(), Refusal> {
        match paint {
            usvg::Paint::Color(_) => Ok(()),
            usvg::Paint::LinearGradient(_) | usvg::Paint::RadialGradient(_) => self.add(covered),
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

    /// What `body` costs drawn 100 × 100 pixels, a pixel a unit of its view.
    fn cost(body: &str) -> Result<u64, Refusal> {
        work(&read(&svg(body))?, (100, 100), tiny_skia::Transform::identity(), u64::MAX)
    }

    /// `<use>`s count as what they repeat, nested; a marker as often as the vertices it may sit
    /// on; one that repeats itself isn't counted at all.
    #[test]
    fn an_svg_counts_its_elements_where_they_repeat() {
        let doc = |body: &str| roxmltree::Document::parse(&svg(body)).map(|d| elements(&d)).unwrap();
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
        // A marker of two elements on the points of a path of four segments and of the circle (its
        // four quarter arcs of two curves at most, a move, four lines), both ends of each counted.
        assert_eq!(
            doc(r#"<marker id="m"><circle r="1"/></marker><path d="M0 0 L1 1 L2 0 L3 1"/>"#),
            Some(4 + 2 * ((4 + 2) + (4 * 3 * 2 + 5 + 2)))
        );
        let zigzag = format!("M0 0{}", " L1 1".repeat(5000));
        assert_eq!(doc(&format!(r#"<marker id="m"><circle r="1"/></marker><path d="{zigzag}"/>"#)), None);
        // A marker's points are copied on each vertex too: few elements, past the points.
        let half = format!("M0 0{}", " L1 1".repeat(2490));
        assert_eq!(doc(&format!(r#"<marker id="m"><path d="{half}"/></marker><path d="{half}" marker-mid="url(#m)"/>"#)), None);
        // A rect without rounded corners is a path of four corners, which a marker sits on too.
        let rects = r#"<rect width="1" height="1"/>"#.repeat(1000);
        assert_eq!(doc(&format!(r#"<marker id="m">{rects}</marker>{rects}"#)), None);
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
            (points_of(shape), read(&text).map(|t| drawn(t.root())))
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
        ] {
            let (points, drawn) = counted(body);
            let drawn = drawn.unwrap_or_else(|why| panic!("{body}: {why:?}"));
            assert!(points >= drawn && points < POINTS, "{body}: {points} counted, {drawn} drawn");
        }
        for body in [
            r#"<circle r="1e38"/>"#,
            r#"<path d="M0 0A1e30 0.00002 0 1 1 1e30 1e30" stroke="black"/>"#,
            r#"<path d="M0 0A1e300 1e300 0 1 1 1 1"/>"#,
            r#"<circle r="50%"/>"#,
            r#"<rect width="9" height="9" rx="2em"/>"#,
        ] {
            let started = std::time::Instant::now();
            assert_eq!((counted(body).0, read(&svg(body)).err()), (u64::MAX, Some(Refusal::TooMuch)), "{body}");
            assert!(started.elapsed() < std::time::Duration::from_secs(1), "{body}");
        }
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
        assert_eq!(read(&svg(&"<!-- -->".repeat(XML_NODES as usize))).err(), Some(Refusal::TooMuch));
        let deep = format!("{}{}", "<g>".repeat(2000), "</g>".repeat(2000));
        assert_eq!(read(&svg(&deep)).err(), Some(Refusal::TooMuch));
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
}
