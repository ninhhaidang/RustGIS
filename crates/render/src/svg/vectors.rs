//! A recorded [`Drawing`](crate::vector::Drawing)'s items as SVG: paths with their fill and
//! stroke, gradients and clips in `<defs>`, pictures as PNG, groups with their opacity and blend
//! mode, and glyphs as their outlines, each defined once and placed with `<use>` (a run keeps its
//! text as an `aria-label`).

use std::collections::HashMap;
use std::fmt::Write as _;

use ab_glyph::{Font, OutlineCurve};
use base64::Engine;
use rustgis_map::effects::Blend;
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Path, PathSegment, Transform};

use crate::vector::{Brush, GlyphRun, Group, Image, Item, Stroke};

/// What the items refer to, written once into the document's `<defs>`.
#[derive(Default)]
pub(super) struct Defs {
    pub text: String,
    count: usize,
    /// Glyph outlines by face (its `FontRef`'s address) and glyph id: their ids.
    glyphs: HashMap<(usize, u16), Option<String>>,
}

impl Defs {
    /// A new id for a definition of `kind` (a gradient, a clip).
    fn id(&mut self, kind: &str) -> String {
        self.count += 1;
        format!("{kind}{}", self.count)
    }
}

/// Writes `items` into `out`, in the drawing's own space (the caller's group places it); `None`
/// if a picture among them can't be written (an unusable result, not a gap left in the page).
pub(super) fn items(out: &mut String, defs: &mut Defs, items: &[Item]) -> Option<()> {
    let mut rest = items.iter().peekable();
    while let Some(item) = rest.next() {
        // An opaque fill then an opaque stroke of the same path: one element, filled and stroked.
        if let Item::Fill { path, brush: Brush::Solid(fill), rule, transform } = item
            && let Some(Item::Stroke { path: outline, color, stroke, transform: at }) = rest.peek()
            && outline == path
            && at == transform
            && fill.is_opaque()
            && color.is_opaque()
        {
            let paint = format!("{}{}{}", fill_attrs(*fill, *rule), stroke_attrs(*color, stroke), transform_attr(*transform));
            path_element(out, path, &paint);
            rest.next();
            continue;
        }
        match item {
            Item::Fill { path, brush, rule, transform } => {
                let paint = match brush {
                    Brush::Solid(c) => fill_attrs(*c, *rule),
                    _ => format!(r#" fill="url(#{})"{}"#, gradient(defs, brush), rule_attr(*rule)),
                };
                path_element(out, path, &format!(r#"{paint} stroke="none"{}"#, transform_attr(*transform)));
            }
            Item::Stroke { path, color, stroke, transform } => {
                path_element(out, path, &format!(r#" fill="none"{}{}"#, stroke_attrs(*color, stroke), transform_attr(*transform)));
            }
            Item::Glyphs(run) => glyphs(out, defs, run),
            Item::Image(image) => picture(out, image)?,
            Item::Group(g) => group(out, defs, g)?,
        }
    }
    Some(())
}

/// Writes a `<path>` of `path` with `attrs` (its paint and transform).
fn path_element(out: &mut String, path: &Path, attrs: &str) {
    let _ = writeln!(out, r#"<path d="{}"{attrs}/>"#, path_data(path));
}

/// A path's `d`: its segments, in the drawing's pixels.
fn path_data(p: &Path) -> String {
    let mut d = String::new();
    for seg in p.segments() {
        match seg {
            PathSegment::MoveTo(a) => {
                let _ = write!(d, "M{} {}", num(a.x), num(a.y));
            }
            PathSegment::LineTo(a) => {
                let _ = write!(d, "L{} {}", num(a.x), num(a.y));
            }
            PathSegment::QuadTo(k, a) => {
                let _ = write!(d, "Q{} {} {} {}", num(k.x), num(k.y), num(a.x), num(a.y));
            }
            PathSegment::CubicTo(k1, k2, a) => {
                let _ = write!(d, "C{} {} {} {} {} {}", num(k1.x), num(k1.y), num(k2.x), num(k2.y), num(a.x), num(a.y));
            }
            PathSegment::Close => d.push('Z'),
        }
    }
    d
}

/// ` fill-rule="evenodd"` for the even-odd rule; nothing for the default, nonzero.
fn rule_attr(rule: FillRule) -> &'static str {
    if rule == FillRule::EvenOdd { r#" fill-rule="evenodd""# } else { "" }
}

/// ` fill="#rrggbb"` with its opacity where it isn't opaque, and its rule.
fn fill_attrs(c: Color, rule: FillRule) -> String {
    format!("{}{}", paint("fill", c), rule_attr(rule))
}

/// A stroke's colour and style.
fn stroke_attrs(c: Color, s: &Stroke) -> String {
    let cap = match s.cap {
        LineCap::Butt => "",
        LineCap::Round => r#" stroke-linecap="round""#,
        LineCap::Square => r#" stroke-linecap="square""#,
    };
    let join = match s.join {
        LineJoin::Miter | LineJoin::MiterClip => format!(r#" stroke-miterlimit="{}""#, num(s.miter_limit.max(1.0))),
        LineJoin::Round => r#" stroke-linejoin="round""#.into(),
        LineJoin::Bevel => r#" stroke-linejoin="bevel""#.into(),
    };
    format!(r#"{} stroke-width="{}"{cap}{join}"#, paint("stroke", c), num(s.width))
}

/// ` attr="#rrggbb"`, with `attr-opacity` where it isn't opaque.
fn paint(attr: &str, c: Color) -> String {
    let u = c.to_color_u8();
    let mut s = format!(r##" {attr}="#{:02x}{:02x}{:02x}""##, u.red(), u.green(), u.blue());
    if u.alpha() < 255 {
        let _ = write!(s, r#" {attr}-opacity="{}""#, num(c.alpha()));
    }
    s
}

/// ` transform="matrix(…)"`, or nothing for no transform.
fn transform_attr(t: Transform) -> String {
    if t.is_identity() { String::new() } else { format!(r#" transform="{}""#, matrix(t)) }
}

/// `matrix(…)` of `t`: tiny-skia's (sx, ky, kx, sy, tx, ty) are SVG's (a, b, c, d, e, f).
pub(super) fn matrix(t: Transform) -> String {
    format!("matrix({} {} {} {} {} {})", fine(t.sx), fine(t.ky), fine(t.kx), fine(t.sy), num(t.tx), num(t.ty))
}

/// A gradient brush in `<defs>`, in user space; its id.
fn gradient(defs: &mut Defs, brush: &Brush) -> String {
    let id = defs.id("gradient");
    let stops = |s: &[(f32, Color)]| {
        let mut out = String::new();
        for (t, c) in s {
            let u = c.to_color_u8();
            let _ = write!(
                out,
                r##"<stop offset="{}" stop-color="#{:02x}{:02x}{:02x}"{}/>"##,
                num(t.clamp(0.0, 1.0)),
                u.red(),
                u.green(),
                u.blue(),
                if u.alpha() < 255 { format!(r#" stop-opacity="{}""#, num(c.alpha())) } else { String::new() }
            );
        }
        out
    };
    match brush {
        Brush::Linear { start, end, stops: s, transform } => {
            let _ = writeln!(
                defs.text,
                r#"<linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}" gradientTransform="{}">{}</linearGradient>"#,
                num(start.0),
                num(start.1),
                num(end.0),
                num(end.1),
                matrix(*transform),
                stops(s)
            );
        }
        Brush::Radial { start, r0, end, r1, stops: s, transform } => {
            let _ = writeln!(
                defs.text,
                r#"<radialGradient id="{id}" gradientUnits="userSpaceOnUse" fx="{}" fy="{}"{} cx="{}" cy="{}" r="{}" gradientTransform="{}">{}</radialGradient>"#,
                num(start.0),
                num(start.1),
                // (A focal radius is SVG 2's: written only where there is one.)
                if *r0 > 0.0 { format!(r#" fr="{}""#, num(*r0)) } else { String::new() },
                num(end.0),
                num(end.1),
                num(r1.max(0.0)),
                matrix(*transform),
                stops(s)
            );
        }
        Brush::Solid(_) => {}
    }
    id
}

/// A run of glyphs: each glyph's outline (defined once, in font units) placed with `<use>`, the
/// run's text kept as its label.
fn glyphs(out: &mut String, defs: &mut Defs, run: &GlyphRun) {
    let upm = run.font.units_per_em().filter(|u| u.is_finite() && *u > 0.0).unwrap_or(1000.0);
    let face = std::ptr::from_ref(run.font) as usize;
    let k = run.size / upm;
    let text: String = run.glyphs.iter().map(|g| g.ch).collect();
    let _ = writeln!(out, r#"<g aria-label="{}"{}{}>"#, super::esc(&text), paint("fill", run.color), transform_attr(run.transform));
    for g in &run.glyphs {
        let id = match defs.glyphs.get(&(face, g.id.0)) {
            Some(id) => id.clone(),
            None => {
                let id = outline_data(run.font, g.id).map(|d| {
                    let id = format!("glyph{}", defs.glyphs.len() + 1);
                    let _ = writeln!(defs.text, r#"<path id="{id}" d="{d}"/>"#);
                    id
                });
                defs.glyphs.insert((face, g.id.0), id.clone());
                id
            }
        };
        // Spaces have no outline: nothing to place.
        if let Some(id) = id {
            // (Eight decimals: a small size over a large em is a small scale.)
            let _ =
                writeln!(out, r##"<use xlink:href="#{id}" transform="matrix({} 0 0 {} {} 0)"/>"##, trimmed(k, 8), trimmed(-k, 8), num(g.x));
        }
    }
    out.push_str("</g>\n");
}

/// A glyph's outline in font units (y up), as path data; `None` for one with none (a space).
fn outline_data(font: &ab_glyph::FontRef<'static>, id: ab_glyph::GlyphId) -> Option<String> {
    let o = font.outline(id)?;
    let mut d = String::new();
    let mut last = None;
    for c in &o.curves {
        let (start, end) = match c {
            OutlineCurve::Line(a, b) | OutlineCurve::Quad(a, _, b) | OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
        };
        if last != Some(start) {
            if last.is_some() {
                d.push('Z');
            }
            let _ = write!(d, "M{} {}", num(start.x), num(start.y));
        }
        let _ = match c {
            OutlineCurve::Line(_, b) => write!(d, "L{} {}", num(b.x), num(b.y)),
            OutlineCurve::Quad(_, k, b) => write!(d, "Q{} {} {} {}", num(k.x), num(k.y), num(b.x), num(b.y)),
            OutlineCurve::Cubic(_, k1, k2, b) => {
                write!(d, "C{} {} {} {} {} {}", num(k1.x), num(k1.y), num(k2.x), num(k2.y), num(b.x), num(b.y))
            }
        };
        last = Some(end);
    }
    if last.is_some() {
        d.push('Z');
    }
    (!d.is_empty()).then_some(d)
}

/// A picture, as PNG, stretched over its pixels where it lies; `None` if it can't be read back or
/// encoded.
fn picture(out: &mut String, image: &Image) -> Option<()> {
    let png = image.pixmap()?.encode_png().ok()?;
    let _ = writeln!(
        out,
        r#"<image width="{}" height="{}" preserveAspectRatio="none"{}{}{} xlink:href="data:image/png;base64,{}"/>"#,
        image.width,
        image.height,
        if image.opacity < 1.0 { format!(r#" opacity="{}""#, num(image.opacity)) } else { String::new() },
        if image.smooth { "" } else { r#" image-rendering="optimizeSpeed""# },
        transform_attr(image.transform),
        base64::engine::general_purpose::STANDARD.encode(png)
    );
    Some(())
}

/// A group: inside its clip (in the outer space), moved by its transform, composited with its
/// opacity and blend mode as one isolated layer; `None` as [`items`] gives it.
fn group(out: &mut String, defs: &mut Defs, g: &Group) -> Option<()> {
    let mut open = 0;
    if let Some(clip) = &g.clip {
        let id = defs.id("clip");
        let _ = writeln!(
            defs.text,
            r#"<clipPath id="{id}" clipPathUnits="userSpaceOnUse"><path d="{}"{}{}/></clipPath>"#,
            path_data(&clip.path),
            if clip.rule == FillRule::EvenOdd { r#" clip-rule="evenodd""# } else { "" },
            transform_attr(clip.transform)
        );
        let _ = writeln!(out, r#"<g clip-path="url(#{id})">"#);
        open += 1;
    }
    let mut attrs = transform_attr(g.transform);
    if g.opacity < 0.999 {
        let _ = write!(attrs, r#" opacity="{}""#, num(g.opacity));
    }
    // Every group isolated, as the PDF's transparency groups are and the pixels' offscreen
    // layers: a blend inside it reaches nothing beneath it.
    if g.blend != Blend::Normal {
        let _ = write!(attrs, r#" style="mix-blend-mode:{};isolation:isolate""#, blend_name(g.blend));
    } else {
        attrs.push_str(r#" style="isolation:isolate""#);
    }
    let _ = writeln!(out, "<g{attrs}>");
    items(out, defs, &g.items)?;
    out.push_str("</g>\n");
    for _ in 0..open {
        out.push_str("</g>\n");
    }
    Some(())
}

/// The CSS `mix-blend-mode` of a blend mode.
fn blend_name(b: Blend) -> &'static str {
    match b {
        Blend::Normal => "normal",
        Blend::Multiply => "multiply",
        Blend::Screen => "screen",
        Blend::Overlay => "overlay",
        Blend::Darken => "darken",
        Blend::Lighten => "lighten",
        Blend::Difference => "difference",
    }
}

/// A coordinate: at most two decimals (a hundredth of a pixel), none trailing.
fn num(v: f32) -> String {
    trimmed(v, 2)
}

/// A scale or turn: six decimals.
pub(super) fn fine(v: f32) -> String {
    trimmed(v, 6)
}

/// `v` with at most `places` decimals and no trailing zeros (0 for a value that isn't finite).
fn trimmed(v: f32, places: usize) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let t = format!("{v:.places$}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t == "-0" { "0".into() } else { t.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{Clip, Drawing, Glyph, GlyphRun};
    use tiny_skia::{PathBuilder, Rect};

    fn rect(x: f32) -> Path {
        PathBuilder::from_rect(Rect::from_xywh(x, 10.0, 20.0, 20.0).unwrap())
    }

    #[test]
    fn a_drawing_becomes_svg_elements_and_defs() {
        let mut d = Drawing::new(200.0, 100.0);
        let red = Color::from_rgba8(200, 0, 0, 255);
        // Filled and outlined: one element.
        d.fill(rect(0.0), Brush::Solid(red), FillRule::EvenOdd, Transform::identity());
        d.stroke(
            rect(0.0),
            Color::BLACK,
            Stroke { width: 2.0, cap: LineCap::Round, join: LineJoin::Round, miter_limit: 4.0 },
            Transform::identity(),
        );
        let stops = vec![(0.0, red), (1.0, Color::from_rgba8(0, 0, 255, 128))];
        d.fill(
            rect(30.0),
            Brush::Linear { start: (30.0, 0.0), end: (50.0, 0.0), stops, transform: Transform::identity() },
            FillRule::Winding,
            Transform::identity(),
        );
        let font = crate::text::label_font().unwrap();
        let o = font.glyph_id('o');
        let glyphs =
            vec![Glyph { id: o, x: 0.0, ch: 'o' }, Glyph { id: font.glyph_id(' '), x: 6.0, ch: ' ' }, Glyph { id: o, x: 9.0, ch: 'o' }];
        d.glyphs(GlyphRun { font, size: 12.0, color: Color::BLACK, transform: Transform::from_translate(60.0, 40.0), glyphs });
        let mut pic = tiny_skia::Pixmap::new(2, 2).unwrap();
        pic.fill(Color::from_rgba8(0, 128, 0, 255));
        d.image(pic.as_ref(), Transform::from_scale(5.0, 5.0), 0.5, false);
        let clip = Clip { path: rect(100.0), rule: FillRule::Winding, transform: Transform::identity() };
        d.begin_group(0.6, Blend::Multiply, Some(clip));
        d.fill(rect(95.0), Brush::Solid(red), FillRule::Winding, Transform::identity());
        d.end_group();
        let (_, _, list) = d.finish();
        let mut defs = Defs::default();
        let mut svg = String::new();
        items(&mut svg, &mut defs, &list).unwrap();
        assert!(svg.starts_with(r##"<path d="M0 10L20 10L20 30L0 30Z" fill="#c80000" fill-rule="evenodd" stroke="#000000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"##), "{svg}");
        assert!(
            svg.contains(r#"fill="url(#gradient1)""#)
                && defs.text.contains(r#"<linearGradient id="gradient1" gradientUnits="userSpaceOnUse" x1="30" y1="0" x2="50" y2="0""#)
        );
        assert!(defs.text.contains(r##"stop-color="#0000ff" stop-opacity="0.5""##));
        // The two o's share one outline; the space has none to place.
        assert_eq!(
            (defs.text.matches("<path id=\"glyph").count(), svg.matches("<use xlink:href=\"#glyph1\"").count()),
            (1, 2),
            "{}",
            defs.text
        );
        assert!(svg.contains(r##"<g aria-label="o o" fill="#000000" transform="matrix(1 0 0 1 60 40)">"##));
        assert!(svg.contains(
            r#"opacity="0.5" image-rendering="optimizeSpeed" transform="matrix(5 0 0 5 0 0)" xlink:href="data:image/png;base64,"#
        ));
        assert!(
            svg.contains(r#"<g clip-path="url(#clip2)">"#)
                && svg.contains(r#"opacity="0.6" style="mix-blend-mode:multiply;isolation:isolate">"#),
            "{svg}"
        );
        assert!(defs.text.contains(r#"<clipPath id="clip2" clipPathUnits="userSpaceOnUse"><path d="M100 10"#));
    }

    #[test]
    fn a_picture_that_cant_be_read_back_fails_the_writing() {
        let mut d = Drawing::new(10.0, 10.0);
        let mut pic = tiny_skia::Pixmap::new(2, 2).unwrap();
        pic.fill(Color::BLACK);
        d.image(pic.as_ref(), Transform::identity(), 1.0, true);
        let (_, _, mut list) = d.finish();
        if let Some(Item::Image(image)) = list.first_mut() {
            image.rgb_z = vec![1, 2, 3];
        }
        // An unusable result is an error, not a page with a gap in it.
        assert!(items(&mut String::new(), &mut Defs::default(), &list).is_none());
    }
}
