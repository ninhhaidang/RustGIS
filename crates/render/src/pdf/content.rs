//! A [`Drawing`](crate::vector::Drawing)'s items as a page's content stream: paths, glyph runs
//! (in the document's [`Fonts`]), pictures, gradients (shadings, their transparency a soft mask)
//! and groups (clipped, or transparency groups for an opacity or blend mode).
//!
//! The drawing's pixels (y down) are mapped onto the page once, at the start; every item is
//! written in them. Colour, transparency and line settings are written only when they change.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;

use rustgis_map::effects::Blend;
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Path, PathSegment, Transform};

use super::Doc;
use super::fonts::Fonts;
use crate::vector::{Brush, Clip, GlyphRun, Group, Image, Item, Stroke};

/// Writes the content of a page `size` (drawing pixels) shown at `page` (points); its content
/// stream's and resource dictionary's object numbers.
pub(super) fn page(doc: &mut Doc, fonts: &mut Fonts, size: (f32, f32), page: (f64, f64), items: &[Item]) -> (usize, usize) {
    let (contents, resources) = (doc.reserve(), doc.reserve());
    let mut w = Writer { doc, fonts, res: Resources::default(), resources };
    let mut out = Vec::new();
    // Drawing pixels, y down from the top, to points, y up from the bottom.
    let (sx, sy) = (page.0 / f64::from(size.0), page.1 / f64::from(size.1));
    let _ = writeln!(out, "{} 0 0 {} 0 {} cm", fnum(sx), fnum(-sy), fnum(page.1));
    w.items(&mut out, &mut State::default(), items);
    w.doc.stream(contents, "", &out, false);
    let dict = w.res.dict();
    w.doc.put(resources, dict.as_bytes());
    (contents, resources)
}

/// What a page (and the groups on it) refers to by name.
#[derive(Default)]
struct Resources {
    fonts: Vec<usize>,
    /// Graphics states by their entries, and the entries in order.
    states: HashMap<String, usize>,
    state_list: Vec<String>,
    xobjects: Vec<usize>,
    shadings: Vec<String>,
}

impl Resources {
    /// The resource dictionary naming everything the page's content refers to.
    fn dict(&self) -> String {
        let mut s = String::from("<<");
        if !self.fonts.is_empty() {
            s.push_str(" /Font <<");
            for id in &self.fonts {
                let _ = write!(s, " /T{id} {id} 0 R");
            }
            s.push_str(" >>");
        }
        if !self.state_list.is_empty() {
            s.push_str(" /ExtGState <<");
            for (i, g) in self.state_list.iter().enumerate() {
                let _ = write!(s, " /G{i} << {g} >>");
            }
            s.push_str(" >>");
        }
        if !self.xobjects.is_empty() {
            s.push_str(" /XObject <<");
            for id in &self.xobjects {
                let _ = write!(s, " /X{id} {id} 0 R");
            }
            s.push_str(" >>");
        }
        if !self.shadings.is_empty() {
            s.push_str(" /Shading <<");
            for (i, sh) in self.shadings.iter().enumerate() {
                let _ = write!(s, " /S{i} {sh}");
            }
            s.push_str(" >>");
        }
        s.push_str(" >>");
        s
    }

    /// The name of the graphics state with `entries`.
    fn state(&mut self, entries: String) -> usize {
        if let Some(&i) = self.states.get(&entries) {
            return i;
        }
        self.state_list.push(entries.clone());
        self.states.insert(entries, self.state_list.len() - 1);
        self.state_list.len() - 1
    }
}

/// What the content has set so far (`None`: not known, so written before it's used).
#[derive(Clone, Default)]
struct State {
    fill: Option<[u16; 3]>,
    stroke: Option<[u16; 3]>,
    /// The graphics state (alpha) in force.
    gs: Option<usize>,
    width: Option<f32>,
    cap: Option<LineCap>,
    join: Option<LineJoin>,
    miter: Option<f32>,
}

struct Writer<'a> {
    doc: &'a mut Doc,
    fonts: &'a mut Fonts,
    res: Resources,
    resources: usize,
}

impl Writer<'_> {
    /// Writes `items` in order.
    fn items(&mut self, out: &mut Vec<u8>, st: &mut State, items: &[Item]) {
        let mut rest = items.iter().peekable();
        while let Some(item) = rest.next() {
            // An opaque fill then an opaque stroke of the same path (a polygon and its outline): the
            // path written once, filled and stroked together. (A see-through pair stays apart: a
            // shape filled and stroked together composites as one, unlike two drawn in turn.)
            if let Item::Fill { path, brush: Brush::Solid(fill), rule, transform } = item
                && let Some(Item::Stroke { path: outline, color, stroke, transform: at }) = rest.peek()
                && outline == path
                && at == transform
                && fill.is_opaque()
                && color.is_opaque()
            {
                self.fill_and_stroke(out, st, path, *fill, *rule, (*color, stroke), *transform);
                rest.next();
                continue;
            }
            match item {
                Item::Fill { path, brush, rule, transform } => self.fill(out, st, path, brush, *rule, *transform),
                Item::Stroke { path, color, stroke, transform } => self.stroke(out, st, path, *color, stroke, *transform),
                Item::Glyphs(run) => self.glyphs(out, st, run),
                Item::Image(image) => self.image(out, st, image),
                Item::Group(g) => self.group(out, st, g),
            }
        }
    }

    /// Fills `path` (moved by `t`): in a colour, or clipped to it and covered with a shading, its
    /// varying transparency a soft mask.
    fn fill(&mut self, out: &mut Vec<u8>, st: &mut State, path: &Path, brush: &Brush, rule: FillRule, t: Transform) {
        let even_odd = rule == FillRule::EvenOdd;
        match brush {
            Brush::Solid(c) => {
                self.alpha(out, st, c.alpha());
                set_color(out, &mut st.fill, *c, "rg");
                let saved = (!t.is_identity()).then(|| begin(out, st, t));
                path_ops(out, path);
                out.extend_from_slice(if even_odd { b"f*\n" } else { b"f\n" });
                end(out, st, saved);
            }
            Brush::Linear { stops, transform, .. } | Brush::Radial { stops, transform, .. } => {
                let stops = clean_stops(stops);
                let saved = begin(out, st, t);
                path_ops(out, path);
                out.extend_from_slice(if even_odd { b"W* n\n" } else { b"W n\n" });
                if !transform.is_identity() {
                    write_matrix(out, *transform);
                    out.extend_from_slice(b" cm\n");
                }
                let coords = match brush {
                    Brush::Linear { start, end, .. } => {
                        format!("/ShadingType 2 /Coords [{} {} {} {}]", fnum(start.0), fnum(start.1), fnum(end.0), fnum(end.1))
                    }
                    Brush::Radial { start, r0, end, r1, .. } => format!(
                        "/ShadingType 3 /Coords [{} {} {} {} {} {}]",
                        fnum(start.0),
                        fnum(start.1),
                        fnum(r0.max(0.0)),
                        fnum(end.0),
                        fnum(end.1),
                        fnum(r1.max(0.0))
                    ),
                    Brush::Solid(_) => String::new(),
                };
                let alphas: Vec<f32> = stops.iter().map(|s| s.1.alpha()).collect();
                if alphas.windows(2).all(|w| (w[0] - w[1]).abs() < 1.0 / 512.0) {
                    self.alpha(out, st, alphas.first().copied().unwrap_or(1.0));
                } else {
                    // Varying transparency: a soft mask, the same gradient in grey with the alphas
                    // as its levels, set up in the gradient's space.
                    // The mask's box reaches far past any page in the gradient's space; the
                    // clip already bounds what shows.
                    self.alpha(out, st, 1.0);
                    let mask = self.doc.reserve();
                    let grey = format!("<< {coords} /ColorSpace /DeviceGray /Function {} /Extend [true true] >>", function(&stops, true));
                    let dict = format!(
                        "/Type /XObject /Subtype /Form /BBox [-1000000 -1000000 1000000 1000000] /Group << /S /Transparency /CS /DeviceGray >> /Resources << /Shading << /S0 {grey} >> >>"
                    );
                    self.doc.stream(mask, &dict, b"/S0 sh", false);
                    let g = self.res.state(format!("/SMask << /Type /Mask /S /Luminosity /G {mask} 0 R >>"));
                    let _ = writeln!(out, "/G{g} gs");
                }
                self.res
                    .shadings
                    .push(format!("<< {coords} /ColorSpace /DeviceRGB /Function {} /Extend [true true] >>", function(&stops, false)));
                let _ = writeln!(out, "/S{} sh", self.res.shadings.len() - 1);
                end(out, st, Some(saved));
            }
        }
    }

    /// Fills `path` with `fill` and strokes it with `stroke` in one, both opaque.
    #[allow(clippy::too_many_arguments)]
    fn fill_and_stroke(
        &mut self,
        out: &mut Vec<u8>,
        st: &mut State,
        path: &Path,
        fill: Color,
        rule: FillRule,
        (c, s): (Color, &Stroke),
        t: Transform,
    ) {
        self.alpha(out, st, 1.0);
        set_color(out, &mut st.fill, fill, "rg");
        set_color(out, &mut st.stroke, c, "RG");
        stroke_state(out, st, s);
        let saved = (!t.is_identity()).then(|| begin(out, st, t));
        path_ops(out, path);
        out.extend_from_slice(if rule == FillRule::EvenOdd { b"B*\n" } else { b"B\n" });
        end(out, st, saved);
    }

    /// Strokes `path` (moved by `t`) in colour `c` with the style `s`.
    fn stroke(&mut self, out: &mut Vec<u8>, st: &mut State, path: &Path, c: Color, s: &Stroke, t: Transform) {
        self.alpha(out, st, c.alpha());
        set_color(out, &mut st.stroke, c, "RG");
        stroke_state(out, st, s);
        let saved = (!t.is_identity()).then(|| begin(out, st, t));
        path_ops(out, path);
        out.extend_from_slice(b"S\n");
        end(out, st, saved);
    }

    /// Sets a run of glyphs in the document's Type 3 fonts, each at its pen position.
    fn glyphs(&mut self, out: &mut Vec<u8>, st: &mut State, run: &GlyphRun) {
        let mut placed = Vec::with_capacity(run.glyphs.len());
        for g in &run.glyphs {
            if let Some(p) = self.fonts.place(self.doc, run.font, g.id, g.ch) {
                placed.push((g.x, p));
            }
        }
        if placed.is_empty() {
            return;
        }
        self.alpha(out, st, run.color.alpha());
        set_color(out, &mut st.fill, run.color, "rg");
        out.extend_from_slice(b"BT\n");
        // Glyph space is y up: flipped into the run's y-down space.
        write_matrix(out, run.transform.pre_scale(1.0, -1.0));
        out.extend_from_slice(b" Tm\n");
        let size = run.size;
        let (mut font, mut pen) = (None, 0.0f32);
        for (x, p) in placed {
            if font != Some(p.font) {
                if font.is_some() {
                    out.extend_from_slice(b"] TJ\n");
                }
                if !self.res.fonts.contains(&p.font) {
                    self.res.fonts.push(p.font);
                }
                let _ = write!(out, "/T{} ", p.font);
                num(out, size, 3);
                out.extend_from_slice(b" Tf\n[");
                font = Some(p.font);
            }
            // TJ moves back by thousandths of the em: from where the pen is to the glyph's place.
            let shift = (pen - x) / size * 1000.0;
            if shift.abs() >= 0.05 {
                num(out, shift, 1);
            }
            let _ = write!(out, "<{:02X}>", p.code);
            pen = x + p.advance * size;
        }
        out.extend_from_slice(b"] TJ\nET\n");
    }

    /// Places a picture: an image object (with its alpha as a soft mask) drawn where it lies.
    fn image(&mut self, out: &mut Vec<u8>, st: &mut State, image: &Image) {
        let id = self.doc.reserve();
        let mut dict = format!(
            "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8{}",
            image.width,
            image.height,
            if image.smooth { " /Interpolate true" } else { "" }
        );
        if let Some(alpha) = &image.alpha_z {
            let mask = self.doc.reserve();
            self.doc.stream(
                mask,
                &format!(
                    "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8",
                    image.width, image.height
                ),
                alpha,
                true,
            );
            let _ = write!(dict, " /SMask {mask} 0 R");
        }
        self.doc.stream(id, &dict, &image.rgb_z, true);
        self.res.xobjects.push(id);
        self.alpha(out, st, image.opacity);
        // The unit square (an image's space, its first row at the top) onto its pixels, then on.
        let (w, h) = (image.width as f32, image.height as f32);
        let saved = begin(out, st, image.transform.pre_concat(Transform::from_row(w, 0.0, 0.0, -h, 0.0, h)));
        let _ = writeln!(out, "/X{id} Do");
        end(out, st, Some(saved));
    }

    /// Writes a group inside its clip, moved by its transform: as a transparency group when it has
    /// an opacity or a blend mode, else straight into the content.
    fn group(&mut self, out: &mut Vec<u8>, st: &mut State, g: &Group) {
        let saved = st.clone();
        out.extend_from_slice(b"q\n");
        if let Some(clip) = &g.clip {
            clip_ops(out, clip);
        }
        // A transform that folds its items flat leaves nothing to see.
        if g.transform.invert().is_none() {
            out.extend_from_slice(b"Q\n");
            return;
        }
        if !g.transform.is_identity() {
            write_matrix(out, g.transform);
            out.extend_from_slice(b" cm\n");
        }
        let composite = g.opacity < 0.999 || g.blend != Blend::Normal;
        if composite {
            // A transparency group, isolated (drawn on transparency, as the renderer draws a
            // layer offscreen), composited with the group's opacity and blend mode.
            let form = self.doc.reserve();
            let mut inner = Vec::new();
            self.items(&mut inner, &mut State::default(), &g.items);
            // Room for anything the group's items can draw, in their own space: the page seen
            // through every transform above the group is not known here.
            let dict = format!(
                "/Type /XObject /Subtype /Form /BBox [-1000000 -1000000 1000000 1000000] /Group << /S /Transparency /CS /DeviceRGB /I true >> /Resources {} 0 R",
                self.resources
            );
            self.doc.stream(form, &dict, &inner, false);
            self.res.xobjects.push(form);
            let mut entries = String::new();
            let a = fnum(g.opacity);
            let _ = write!(entries, "/ca {a} /CA {a}");
            if g.blend != Blend::Normal {
                let _ = write!(entries, " /BM /{}", blend_name(g.blend));
            }
            let gs = self.res.state(entries);
            let _ = writeln!(out, "/G{gs} gs /X{form} Do");
        } else {
            self.items(out, st, &g.items);
        }
        out.extend_from_slice(b"Q\n");
        *st = saved;
    }

    /// Sets the fill and stroke alpha to `a` where it isn't already.
    fn alpha(&mut self, out: &mut Vec<u8>, st: &mut State, a: f32) {
        let a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 };
        let v = fnum(a);
        let gs = self.res.state(format!("/ca {v} /CA {v}"));
        if st.gs != Some(gs) {
            let _ = writeln!(out, "/G{gs} gs");
            st.gs = Some(gs);
        }
    }
}

/// Sets the line width, cap, join and mitre limit of `s` where they aren't already.
fn stroke_state(out: &mut Vec<u8>, st: &mut State, s: &Stroke) {
    if st.width != Some(s.width) {
        num(out, s.width, 3);
        out.extend_from_slice(b" w\n");
        st.width = Some(s.width);
    }
    if st.cap != Some(s.cap) {
        let _ = writeln!(
            out,
            "{} J",
            match s.cap {
                LineCap::Butt => 0,
                LineCap::Round => 1,
                LineCap::Square => 2,
            }
        );
        st.cap = Some(s.cap);
    }
    if st.join != Some(s.join) {
        let _ = writeln!(
            out,
            "{} j",
            match s.join {
                LineJoin::Miter | LineJoin::MiterClip => 0,
                LineJoin::Round => 1,
                LineJoin::Bevel => 2,
            }
        );
        st.join = Some(s.join);
    }
    if matches!(s.join, LineJoin::Miter | LineJoin::MiterClip) && st.miter != Some(s.miter_limit) {
        num(out, s.miter_limit.max(1.0), 3);
        out.extend_from_slice(b" M\n");
        st.miter = Some(s.miter_limit);
    }
}

/// Saves the graphics state and applies `t`; what was known before, for [`end`].
fn begin(out: &mut Vec<u8>, st: &State, t: Transform) -> State {
    out.extend_from_slice(b"q\n");
    if !t.is_identity() {
        write_matrix(out, t);
        out.extend_from_slice(b" cm\n");
    }
    st.clone()
}

/// Restores what [`begin`] saved.
fn end(out: &mut Vec<u8>, st: &mut State, saved: Option<State>) {
    if let Some(s) = saved {
        out.extend_from_slice(b"Q\n");
        *st = s;
    }
}

/// Clips what follows to `clip`'s path.
fn clip_ops(out: &mut Vec<u8>, clip: &Clip) {
    let Some(path) = clip.path.clone().transform(clip.transform) else { return };
    path_ops(out, &path);
    out.extend_from_slice(if clip.rule == FillRule::EvenOdd { b"W* n\n" } else { b"W n\n" });
}

/// Sets the fill (`rg`) or stroke (`RG`) colour where it isn't already.
fn set_color(out: &mut Vec<u8>, known: &mut Option<[u16; 3]>, c: Color, op: &str) {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 1000.0).round() as u16;
    let key = [q(c.red()), q(c.green()), q(c.blue())];
    if *known != Some(key) {
        for v in key {
            num(out, f32::from(v) / 1000.0, 3);
            out.push(b' ');
        }
        out.extend_from_slice(op.as_bytes());
        out.push(b'\n');
        *known = Some(key);
    }
}

/// `p`'s segments as path operators; quadratic curves as the cubics they equal.
fn path_ops(out: &mut Vec<u8>, p: &Path) {
    let mut at = tiny_skia::Point::zero();
    let pt = |out: &mut Vec<u8>, p: tiny_skia::Point| {
        num(out, p.x, 1);
        out.push(b' ');
        num(out, p.y, 1);
        out.push(b' ');
    };
    for seg in p.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                pt(out, p);
                out.extend_from_slice(b"m\n");
                at = p;
            }
            PathSegment::LineTo(p) => {
                pt(out, p);
                out.extend_from_slice(b"l\n");
                at = p;
            }
            PathSegment::QuadTo(k, p) => {
                let lerp = |a: tiny_skia::Point| tiny_skia::Point::from_xy(a.x + (k.x - a.x) * 2.0 / 3.0, a.y + (k.y - a.y) * 2.0 / 3.0);
                pt(out, lerp(at));
                pt(out, lerp(p));
                pt(out, p);
                out.extend_from_slice(b"c\n");
                at = p;
            }
            PathSegment::CubicTo(a, b, p) => {
                pt(out, a);
                pt(out, b);
                pt(out, p);
                out.extend_from_slice(b"c\n");
                at = p;
            }
            PathSegment::Close => out.extend_from_slice(b"h\n"),
        }
    }
}

/// `t` as a PDF matrix (`a b c d e f`): tiny-skia's (sx, ky, kx, sy, tx, ty) are the same six.
fn write_matrix(out: &mut Vec<u8>, t: Transform) {
    for (i, v) in [t.sx, t.ky, t.kx, t.sy, t.tx, t.ty].into_iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        num(out, v, 5);
    }
}

/// The stops, offsets clamped to 0..=1 and never going back, with one at each end.
fn clean_stops(stops: &[(f32, Color)]) -> Vec<(f32, Color)> {
    let mut out: Vec<(f32, Color)> = Vec::with_capacity(stops.len() + 2);
    // (A ramp holds at most 2 × (MAX_STOPS + 2) stops: none is left out.)
    for &(t, c) in stops {
        // On the grid the offsets are written to (six decimals), so two that would print alike
        // are drawn apart below.
        let t = if t.is_finite() { (t.clamp(0.0, 1.0) * 1e6).round() / 1e6 } else { 0.0 };
        let t = out.last().map_or(t, |l| t.max(l.0));
        out.push((t, c));
    }
    match (out.first().copied(), out.last().copied()) {
        (Some(first), Some(last)) => {
            if first.0 > 0.0 {
                out.insert(0, (0.0, first.1));
            }
            if last.0 < 1.0 {
                out.push((1.0, last.1));
            }
        }
        _ => out = vec![(0.0, Color::BLACK), (1.0, Color::BLACK)],
    }
    if let [only] = out.as_slice() {
        let c = only.1;
        out.push((1.0, c));
    }
    // A function's bounds must rise: stops at one offset (a discrete ramp's steps, its last colour
    // at 1) are drawn apart by a hair, each earlier one moved back, so a step stays a step and
    // what pads past the end takes the last colour, as the raster gradient does.
    let mut next = f32::INFINITY;
    for stop in out.iter_mut().rev() {
        if stop.0 >= next {
            stop.0 = (next - STOP_GAP).max(0.0);
        }
        next = stop.0;
    }
    // A first stop sharing its offset with the next holds no width: left out, so no bound lands
    // on the domain's start (the pass above can bring one back to 0 beside the next).
    while out.len() > 2 && out.first().map(|f| f.0) == out.get(1).map(|s| s.0) {
        out.remove(0);
    }
    out
}

/// How far apart [`clean_stops`] draws stops at one offset.
const STOP_GAP: f32 = 1e-5;

/// The function from 0..=1 to the stops' colours (or, `alpha`, their alphas), interpolated
/// linearly between them.
fn function(stops: &[(f32, Color)], alpha: bool) -> String {
    let c = |c: Color| if alpha { fnum(c.alpha()) } else { format!("{} {} {}", fnum(c.red()), fnum(c.green()), fnum(c.blue())) };
    let pieces: Vec<String> =
        stops.windows(2).map(|w| format!("<< /FunctionType 2 /Domain [0 1] /C0 [{}] /C1 [{}] /N 1 >>", c(w[0].1), c(w[1].1))).collect();
    if pieces.len() == 1 {
        return pieces.into_iter().next().unwrap_or_default();
    }
    let bounds: Vec<String> = stops.iter().skip(1).take(stops.len().saturating_sub(2)).map(|s| fnum(s.0)).collect();
    let encode = vec!["0 1"; pieces.len()].join(" ");
    format!("<< /FunctionType 3 /Domain [0 1] /Functions [{}] /Bounds [{}] /Encode [{encode}] >>", pieces.join(" "), bounds.join(" "))
}

/// The PDF name of a blend mode.
fn blend_name(b: Blend) -> &'static str {
    match b {
        Blend::Normal => "Normal",
        Blend::Multiply => "Multiply",
        Blend::Screen => "Screen",
        Blend::Overlay => "Overlay",
        Blend::Darken => "Darken",
        Blend::Lighten => "Lighten",
        Blend::Difference => "Difference",
    }
}

/// `v` with at most `places` decimals and no trailing zeros (0 for a value that isn't finite).
pub(super) fn num(out: &mut Vec<u8>, v: impl Into<f64>, places: u32) {
    let v: f64 = v.into();
    let k = 10i64.pow(places.min(9));
    let v = if v.is_finite() { v } else { 0.0 };
    let mut n = (v * k as f64).round().clamp(-1e17, 1e17) as i64;
    if n < 0 {
        out.push(b'-');
        n = -n;
    }
    let _ = write!(out, "{}", n / k);
    let (mut frac, mut digits) = (n % k, places.min(9) as usize);
    if frac != 0 {
        while frac % 10 == 0 {
            frac /= 10;
            digits -= 1;
        }
        let _ = write!(out, ".{frac:0digits$}");
    }
}

/// [`num`] with six decimals, as text.
fn fnum(v: impl Into<f64>) -> String {
    let mut out = Vec::new();
    num(&mut out, v, 6);
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::{Page, tests::objects, write};
    use crate::vector::{Drawing, Glyph};
    use ab_glyph::Font;
    use tiny_skia::{PathBuilder, Pixmap, Rect};

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Path {
        PathBuilder::from_rect(Rect::from_xywh(x, y, w, h).unwrap())
    }

    /// A page with one of everything: fills, a rounded stroke, text, a picture, gradients (one
    /// fading) and a multiplied, clipped group.
    pub(crate) fn sampler() -> Drawing {
        let mut d = Drawing::new(400.0, 300.0);
        d.fill(rect(10.0, 10.0, 120.0, 80.0), Brush::Solid(Color::from_rgba8(30, 120, 200, 255)), FillRule::Winding, Transform::identity());
        d.fill(rect(60.0, 40.0, 120.0, 80.0), Brush::Solid(Color::from_rgba8(220, 40, 40, 128)), FillRule::EvenOdd, Transform::identity());
        let mut pb = PathBuilder::new();
        pb.move_to(10.0, 150.0);
        pb.quad_to(100.0, 100.0, 190.0, 150.0);
        let round = Stroke { width: 3.0, cap: LineCap::Round, join: LineJoin::Round, miter_limit: 4.0 };
        d.stroke(pb.finish().unwrap(), Color::BLACK, round, Transform::identity());
        let font = crate::text::label_font().unwrap();
        let (mut glyphs, mut x) = (Vec::new(), 0.0);
        for ch in "Shelby County".chars() {
            let id = font.glyph_id(ch);
            glyphs.push(Glyph { id, x, ch });
            x += font.h_advance_unscaled(id) / font.units_per_em().unwrap() * 24.0;
        }
        d.glyphs(GlyphRun {
            font,
            size: 24.0,
            color: Color::from_rgba8(20, 20, 20, 255),
            transform: Transform::from_translate(210.0, 40.0),
            glyphs,
        });
        let mut pic = Pixmap::new(4, 4).unwrap();
        pic.fill(Color::from_rgba8(0, 160, 0, 200));
        d.image(pic.as_ref(), Transform::from_row(10.0, 0.0, 0.0, 10.0, 210.0, 60.0), 1.0, false);
        let stops = vec![(0.0, Color::from_rgba8(255, 200, 0, 255)), (1.0, Color::from_rgba8(120, 0, 160, 255))];
        d.fill(
            rect(210.0, 120.0, 170.0, 60.0),
            Brush::Linear { start: (210.0, 0.0), end: (380.0, 0.0), stops, transform: Transform::identity() },
            FillRule::Winding,
            Transform::identity(),
        );
        let fade = vec![(0.0, Color::from_rgba8(0, 0, 0, 255)), (1.0, Color::from_rgba8(0, 0, 0, 0))];
        d.fill(
            rect(210.0, 190.0, 170.0, 100.0),
            Brush::Radial { start: (295.0, 240.0), r0: 0.0, end: (295.0, 240.0), r1: 50.0, stops: fade, transform: Transform::identity() },
            FillRule::Winding,
            Transform::identity(),
        );
        let clip = crate::vector::Clip { path: rect(10.0, 180.0, 100.0, 100.0), rule: FillRule::Winding, transform: Transform::identity() };
        d.begin_group(0.8, Blend::Multiply, Some(clip));
        d.fill(rect(0.0, 170.0, 190.0, 120.0), Brush::Solid(Color::from_rgba8(250, 220, 0, 255)), FillRule::Winding, Transform::identity());
        d.end_group();
        d
    }

    #[test]
    fn a_drawing_becomes_page_content() {
        let pdf = write(&[Page::drawing(sampler(), 100.0, 75.0)], "Sampler");
        let objs = objects(&pdf);
        let content = objs
            .iter()
            .find(|(d, data)| !d.contains("/Type") && data.starts_with(b"0."))
            .map(|o| String::from_utf8_lossy(&o.1).into_owned())
            .unwrap();
        // Pixels onto the page: 100 mm is 283.46 pt across 400 px.
        assert!(content.starts_with("0.708661 0 0 -0.708661 0 212.598425 cm\n"), "{content}");
        for op in ["\nf\n", "\nf*\n", "1 J\n", " c\n", "\nS\n", "BT\n", " Tf\n", "] TJ\nET\n", " Do\n", " sh\n", "W n\n"] {
            assert!(content.contains(op), "{op:?} in {content}");
        }
        let all: String = objs.iter().map(|o| o.0.as_str()).collect();
        assert!(all.contains("/Subtype /Type3") && all.contains("/ToUnicode") && all.contains("/SMask"));
        assert!(all.contains("/BM /Multiply") && all.contains("/S /Transparency /CS /DeviceRGB /I true"));
        assert!(all.contains("/ShadingType 2") && all.contains("/ShadingType 3") && all.contains("/S /Luminosity"));
        // The half-transparent fill and the group's opacity.
        assert!(all.contains("/ca 0.501961 /CA 0.501961") && all.contains("/ca 0.8 /CA 0.8 /BM /Multiply"));
    }

    #[test]
    fn unchanged_settings_are_not_written_again() {
        let mut d = Drawing::new(100.0, 100.0);
        for i in 0..50 {
            d.fill(rect(i as f32, 0.0, 1.0, 1.0), Brush::Solid(Color::BLACK), FillRule::Winding, Transform::identity());
            d.stroke(rect(i as f32, 2.0, 1.0, 1.0), Color::WHITE, Stroke::default(), Transform::identity());
        }
        let pdf = write(&[Page::drawing(d, 10.0, 10.0)], "");
        let content = objects(&pdf).into_iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|c| c.contains("\nf\n")).unwrap();
        assert_eq!((content.matches(" rg\n").count(), content.matches(" RG\n").count(), content.matches(" gs\n").count()), (1, 1, 1));
        assert_eq!((content.matches(" w\n").count(), content.matches("\nf\n").count(), content.matches("\nS\n").count()), (1, 50, 50));
    }

    #[test]
    fn an_opaque_outlined_shape_is_written_once_and_a_see_through_one_twice() {
        let outlined = |fill: Color| {
            let mut d = Drawing::new(100.0, 100.0);
            d.fill(rect(10.0, 10.0, 30.0, 30.0), Brush::Solid(fill), FillRule::EvenOdd, Transform::identity());
            d.stroke(rect(10.0, 10.0, 30.0, 30.0), Color::BLACK, Stroke::default(), Transform::identity());
            let pdf = write(&[Page::drawing(d, 10.0, 10.0)], "");
            objects(&pdf).into_iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|c| c.contains(" m\n")).unwrap()
        };
        let opaque = outlined(Color::from_rgba8(200, 0, 0, 255));
        assert_eq!((opaque.matches(" m\n").count(), opaque.matches("\nB*\n").count()), (1, 1), "{opaque}");
        // Filled and stroked together, a see-through shape would knock its fill out under the stroke.
        let clear = outlined(Color::from_rgba8(200, 0, 0, 128));
        assert_eq!((clear.matches(" m\n").count(), clear.matches("\nf*\n").count(), clear.matches("\nS\n").count()), (2, 1, 1), "{clear}");
    }

    #[test]
    fn text_is_positioned_by_its_pens_and_maps_back_to_its_characters() {
        let font = crate::text::label_font().unwrap();
        let upm = font.units_per_em().unwrap();
        let (a, v) = (font.glyph_id('A'), font.glyph_id('V'));
        // V set 5 px further than A's advance (at a 20 px em, 250 thousandths).
        let x = font.h_advance_unscaled(a) / upm * 20.0 + 5.0;
        let mut d = Drawing::new(100.0, 100.0);
        let glyphs = vec![Glyph { id: a, x: 0.0, ch: 'A' }, Glyph { id: v, x, ch: 'V' }];
        d.glyphs(GlyphRun { font, size: 20.0, color: Color::BLACK, transform: Transform::from_translate(10.0, 50.0), glyphs });
        let objs = objects(&write(&[Page::drawing(d, 10.0, 10.0)], ""));
        let content = objs.iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|c| c.contains("TJ")).unwrap();
        assert!(content.contains("1 0 0 -1 10 50 Tm\n") && content.contains("[<00>-250<01>] TJ"), "{content}");
        let cmap = objs.iter().map(|o| String::from_utf8_lossy(&o.1).into_owned()).find(|c| c.contains("beginbfchar")).unwrap();
        assert!(cmap.contains("<00> <0041>") && cmap.contains("<01> <0056>"));
    }

    #[test]
    fn stops_are_cleaned_and_numbers_written_plainly() {
        let red = Color::from_rgba8(255, 0, 0, 255);
        assert_eq!(clean_stops(&[(0.4, red)]).iter().map(|s| s.0).collect::<Vec<_>>(), vec![0.0, 0.4, 1.0]);
        // Stops at one offset are drawn apart (earlier ones moved back), so the bounds rise.
        let apart: Vec<f32> = clean_stops(&[(0.6, red), (f32::NAN, red), (0.2, red)]).iter().map(|s| s.0).collect();
        assert!(apart.windows(2).all(|w| w[0] < w[1]) && apart[3] == 0.6 && apart[1] > 0.599, "{apart:?}");
        assert_eq!(clean_stops(&[]).len(), 2);
        let f = function(&clean_stops(&[(0.0, red), (0.5, Color::BLACK), (1.0, red)]), false);
        assert!(f.starts_with("<< /FunctionType 3") && f.contains("/Bounds [0.5]") && f.contains("/Encode [0 1 0 1]"), "{f}");
        // A discrete ramp's last step (two stops at 1): past the end, its last colour.
        let blue = Color::from_rgba8(0, 0, 255, 255);
        let f = function(&clean_stops(&[(0.0, red), (0.5, red), (0.5, Color::BLACK), (1.0, Color::BLACK), (1.0, blue)]), false);
        assert!(f.contains("/C1 [0 0 1] /N 1 >>] /Bounds"), "the last piece ends in blue: {f}");
        // Two stops that would print alike are drawn apart all the same.
        let alike = function(&clean_stops(&[(0.0, red), (0.5, red), (0.500_000_1, blue), (1.0, blue)]), false);
        let b: Vec<f32> =
            alike.split("/Bounds [").nth(1).unwrap().split(']').next().unwrap().split(' ').map(|v| v.parse().unwrap()).collect();
        assert!(b.windows(2).all(|w| w[0] < w[1]), "{alike}");
        // Stops crowded at the start: moving them apart brings one back to 0 beside the first,
        // which is then left out, so no bound lands on the domain's start.
        let crowded = function(&clean_stops(&[(0.0, red), (1e-6, Color::BLACK), (1e-6, blue), (1.0, red)]), false);
        let b: Vec<f32> =
            crowded.split("/Bounds [").nth(1).unwrap().split(']').next().unwrap().split(' ').map(|v| v.parse().unwrap()).collect();
        assert!(b.first().is_some_and(|f| *f > 0.0) && b.windows(2).all(|w| w[0] < w[1]), "{crowded}");
        // Two stops at 0 (a stop of the user's at the start): the first, of no width, left out.
        let start: Vec<f32> = clean_stops(&[(0.0, red), (0.0, Color::BLACK), (0.5, blue)]).iter().map(|s| s.0).collect();
        assert_eq!(start, vec![0.0, 0.5, 1.0]);
        let bounds: Vec<f32> =
            f.split("/Bounds [").nth(1).unwrap().split(']').next().unwrap().split(' ').map(|v| v.parse().unwrap()).collect();
        assert!(bounds.windows(2).all(|w| w[0] < w[1]), "{bounds:?}");
        let s = |v: f32, p: u32| {
            let mut o = Vec::new();
            num(&mut o, v, p);
            String::from_utf8(o).unwrap()
        };
        assert_eq!(
            (s(1.5, 2), s(-0.004, 2), s(2.0, 3), s(-12.3456, 3), s(f32::NAN, 2), s(0.05, 1)),
            ("1.5".into(), "0".into(), "2".into(), "-12.346".into(), "0".into(), "0.1".into())
        );
    }
}
