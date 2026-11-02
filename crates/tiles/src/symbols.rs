//! A style's symbol layers as labels: where each feature is labelled (`symbol-placement`), and
//! the text and icon it is labelled with, for the renderer to place with its own labels.
//!
//! As MapLibre does: `point` placement labels a point at each of its points, a line at its first
//! vertex and a polygon at its pole of inaccessibility; `line` placement repeats the label every
//! `symbol-spacing` along a line where it bends no more than `text-max-angle`, and `line-center`
//! puts one at the middle of the line. A label whose anchor is outside its own tile is left to
//! the tile it is in, so a feature cut across tiles is labelled once.

use std::sync::Arc;

use tiny_skia::Pixmap;

use crate::cache::Payload;
use crate::draw::{Frame, Item, clip_rect, to_px};
use crate::mvt::GeomKind;
use crate::sprite::Sprite;
use crate::style::{FeatureCtx, Rgba, StyleLayer, SymbolPlacement, Val};

/// Most labels a draw collects.
pub(crate) const MAX_LABELS: usize = 8000;

/// A label from a symbol layer, in logical points, for the renderer to place with its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub x: f32,
    pub y: f32,
    /// `text-size`: the font's size in logical pixels (CSS pixels, as MapLibre's).
    pub size: f32,
    pub color: Rgba,
    pub halo: Option<Rgba>,
    pub bold: bool,
    /// A font stack naming an italic face (`text-font`).
    pub italic: bool,
    /// `text-letter-spacing`, in ems.
    pub letter_spacing: f32,
    /// `text-max-width` in ems: lines are wrapped at spaces to about that width. 0 for none (a
    /// label along a line is never wrapped).
    pub max_width: f32,
    /// How the lines of a wrapped label line up (`text-justify`): -0.5 left, 0 centred, 0.5
    /// right.
    pub justify: f32,
    /// Higher places first: later style layers first, then by `symbol-sort-key` (lower first).
    pub priority: f64,
    /// Where the text sits against the point, in text-box sizes (`text-anchor`): `[0.5, 0]`
    /// puts its left edge on the point, `[0, 0.5]` its top edge.
    pub anchor: [f32; 2],
    /// How far the text is moved, in ems (`text-offset`).
    pub offset: [f32; 2],
    /// Turned with the map (a label along a line): the text's angle in radians, clockwise from
    /// the screen's x axis, kept upright. `None` is upright on the screen.
    pub angle: Option<f32>,
    /// The icon drawn at the point (`icon-image`). A label may have an icon and no text.
    pub icon: Option<LabelIcon>,
    pub collide: Collide,
}

/// An icon from the style's sprite, in logical points.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelIcon {
    pub image: Arc<Pixmap>,
    pub width: f32,
    pub height: f32,
    /// The icon's centre, from the point (`icon-offset`).
    pub dx: f32,
    pub dy: f32,
    /// Turned with the map (`icon-rotation-alignment`, an arrow along a line), in radians
    /// clockwise, `icon-rotate` included; `None` is upright on the screen (`icon-rotate` aside,
    /// which is then its angle).
    pub angle: Option<f32>,
}

/// How a label collides with the others (MapLibre's `text-padding`, `*-optional`,
/// `*-allow-overlap`, `*-ignore-placement`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Collide {
    /// Room kept clear round the text and the icon, in points.
    pub text_padding: f32,
    pub icon_padding: f32,
    /// The icon may show without its text (`text-optional`), the text without its icon
    /// (`icon-optional`).
    pub text_optional: bool,
    pub icon_optional: bool,
    /// Shown even over other labels.
    pub text_allow_overlap: bool,
    pub icon_allow_overlap: bool,
    /// Others may be placed over it.
    pub text_ignore_placement: bool,
    pub icon_ignore_placement: bool,
}

impl Default for Collide {
    fn default() -> Self {
        Collide {
            text_padding: 2.0,
            icon_padding: 2.0,
            text_optional: false,
            icon_optional: false,
            text_allow_overlap: false,
            icon_allow_overlap: false,
            text_ignore_placement: false,
            icon_ignore_placement: false,
        }
    }
}

/// Keeps the `n` labels placed first (highest [`Label::priority`]), in no particular order.
pub(crate) fn keep_first(labels: &mut Vec<Label>, n: usize) {
    keep_highest(labels, n, |l| l.priority);
}

/// Keeps the `n` items of highest `priority`, in no particular order.
fn keep_highest<T>(items: &mut Vec<T>, n: usize, priority: impl Fn(&T) -> f64) {
    if items.len() > n {
        items.select_nth_unstable_by(n, |a, b| priority(b).total_cmp(&priority(a)));
        items.truncate(n);
    }
}

/// How early a symbol layer's labels are placed: later layers first, as in MapLibre (styles
/// list their most important labels last, like Liberty's POIs by rank and then its cities).
/// Each layer's labels are ordered within it by their sort key, which is kept to ±1e5.
pub(crate) fn placement_rank(layer_index: usize) -> f64 {
    layer_index as f64 * 1e6
}

/// Collects the labels `layer` puts on the features of `it` into `out`, for a `view`-sized
/// pixmap (device pixels) at `frame`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect(
    out: &mut Vec<Label>,
    it: &Item,
    layer: &StyleLayer,
    frame: &Frame,
    zoom: f32,
    layer_rank: f64,
    sheet: Option<&Sprite>,
    view: (f32, f32),
) {
    let Payload::Vector(tile) = &*it.payload else { return };
    let Some(src_layer) = layer.source_layer.as_deref() else { return };
    let Some(l) = tile.layers.iter().find(|l| l.name == src_layer) else { return };
    let Some(clip) = clip_rect(it, frame) else { return };
    let ppp = frame.ppp.max(0.1);
    // An anchor counts in its own tile's square; and only near the screen: a tile reaches far
    // past the view when zoomed in, and labels collected there would only use up the allowance.
    // Those just off screen are kept, as the text of one can reach into view.
    let margin = 64.0 * ppp;
    let tile_box = [clip.left(), clip.top(), clip.right(), clip.bottom()];
    let near = [tile_box[0].max(-margin), tile_box[1].max(-margin), tile_box[2].min(view.0 + margin), tile_box[3].min(view.1 + margin)];
    if near[0] >= near[2] || near[1] >= near[3] {
        return;
    }
    let to = to_px(it, l.extent, frame);
    let zctx = FeatureCtx::zoom_only(zoom);
    let placement = layer.symbol_placement(&zctx);
    // Spacing is in the pixels of a tile shown at its own zoom (512 to a tile): a tile shown
    // between zooms is stretched by up to 2.
    let stretch = 2f32.powf(zoom.rem_euclid(1.0));
    for f in &l.features {
        let ctx = FeatureCtx { zoom, kind: f.kind, id: f.id, props: &f.props, images: None };
        if !layer.matches(&ctx) {
            continue;
        }
        let Some(base) = label_of(layer, &ctx, sheet, layer_rank, placement) else { continue };
        let lines = || f.geometry.iter().map(|g| g.iter().map(&to).collect::<Vec<(f32, f32)>>());
        let mut anchors: Vec<((f32, f32), Option<f32>)> = Vec::new();
        match (placement, f.kind) {
            (SymbolPlacement::Point, GeomKind::Point) => anchors.extend(f.geometry.iter().flatten().map(|p| (to(p), None))),
            (SymbolPlacement::Point, GeomKind::LineString) => {
                anchors.extend(f.geometry.iter().filter_map(|g| g.first()).map(|p| (to(p), None)))
            }
            (SymbolPlacement::Point, GeomKind::Polygon) => {
                for poly in polygons(lines().collect()) {
                    // The pole lies inside the outer ring's box: a polygon whose box misses the
                    // part of the tile near the screen gets no label there, so isn't searched.
                    let (mut lo, mut hi) = ((f32::INFINITY, f32::INFINITY), (f32::NEG_INFINITY, f32::NEG_INFINITY));
                    for &(x, y) in poly.first().into_iter().flatten() {
                        (lo, hi) = ((lo.0.min(x), lo.1.min(y)), (hi.0.max(x), hi.1.max(y)));
                    }
                    if !(lo.0 <= near[2] && hi.0 >= near[0] && lo.1 <= near[3] && hi.1 >= near[1]) {
                        continue;
                    }
                    if let Some(p) = pole_of_inaccessibility(&poly, ppp.max(1.0)) {
                        anchors.push((p, None));
                    }
                }
            }
            (SymbolPlacement::Line | SymbolPlacement::LineCenter, GeomKind::LineString | GeomKind::Polygon) => {
                let s = (layer.layout_value("symbol-spacing", &ctx).and_then(|v| v.as_f64()).unwrap_or(250.0) as f32).clamp(1.0, 10_000.0);
                let max_angle =
                    (layer.layout_value("text-max-angle", &ctx).and_then(|v| v.as_f64()).unwrap_or(45.0) as f32).clamp(0.0, 360.0);
                let rule = LineRule {
                    spacing: s * stretch * ppp,
                    length: label_length(&base) * ppp,
                    max_angle: max_angle.to_radians(),
                    center: placement == SymbolPlacement::LineCenter,
                };
                for line in lines() {
                    anchors.extend(line_anchors(&line, &rule, tile_box).into_iter().map(|(p, a)| (p, Some(a))));
                }
            }
            _ => {}
        }
        for ((x, y), along) in anchors {
            if !(inside(x, y, tile_box) && inside(x, y, near)) {
                continue;
            }
            let mut label = Label { x: x / ppp, y: y / ppp, ..base.clone() };
            if let Some(a) = along {
                if text_turns(layer, &ctx, placement) {
                    label.angle = Some(upright(a));
                }
                if let Some(i) = &mut label.icon {
                    // An icon turned with the map keeps the line's direction (an arrow points
                    // along it), unless it is kept upright.
                    if let Some(rotate) = i.angle {
                        let a = a + rotate;
                        i.angle = Some(if flag(layer, "icon-keep-upright", &ctx) { upright(a) } else { a });
                    }
                }
            }
            out.push(label);
            // Bounded however many features there are, keeping those placed first: style layers
            // are collected in order, the most important last, so a cap on arrival would keep the
            // least important.
            if out.len() >= 2 * MAX_LABELS {
                keep_first(out, MAX_LABELS);
            }
        }
    }
}

/// The label `layer` gives a feature, before it is put anywhere; `None` when it has neither
/// text nor icon.
fn label_of(layer: &StyleLayer, ctx: &FeatureCtx, sheet: Option<&Sprite>, layer_rank: f64, placement: SymbolPlacement) -> Option<Label> {
    // Lines are kept: the renderer drops those its font can't draw (a name in another script).
    let text: String = layer.text_field(ctx).map(|t| t.trim().chars().take(200).collect()).unwrap_or_default();
    let icon = sheet.and_then(|s| icon(s, layer, ctx, placement));
    if text.is_empty() && icon.is_none() {
        return None;
    }
    let size = layer.text_size(ctx).clamp(1.0, 96.0);
    let halo = layer.text_halo_color(ctx).filter(|c| c[3] > 0 && layer.text_halo_width(ctx) > 0.0);
    let font = layer.text_font(ctx);
    let bold = font.iter().any(|f| f.contains("Bold") || f.contains("Semibold") || f.contains("Medium") || f.contains("Black"));
    let italic = font.iter().any(|f| f.contains("Italic") || f.contains("Oblique"));
    let sort = f64::from(layer.symbol_sort_key(ctx).unwrap_or(0.0)).clamp(-1e5, 1e5);
    let num = |name: &str, default: f32, lo: f32, hi: f32| {
        layer.layout_value(name, ctx).and_then(|v| v.as_f64()).map(|v| v as f32).filter(|v| v.is_finite()).unwrap_or(default).clamp(lo, hi)
    };
    let anchor = text_anchor(layer, ctx);
    let line = placement != SymbolPlacement::Point;
    let justify = match layer.layout_value("text-justify", ctx).as_ref().and_then(Val::as_str) {
        Some("left") => -0.5,
        Some("right") => 0.5,
        // `auto` lines up with the side the text is anchored at.
        Some("auto") => -anchor[0],
        _ => 0.0,
    };
    let collide = Collide {
        text_padding: num("text-padding", 2.0, 0.0, 100.0),
        icon_padding: num("icon-padding", 2.0, 0.0, 100.0),
        text_optional: flag(layer, "text-optional", ctx),
        icon_optional: flag(layer, "icon-optional", ctx),
        text_allow_overlap: flag(layer, "text-allow-overlap", ctx),
        icon_allow_overlap: flag(layer, "icon-allow-overlap", ctx),
        text_ignore_placement: flag(layer, "text-ignore-placement", ctx),
        icon_ignore_placement: flag(layer, "icon-ignore-placement", ctx),
    };
    Some(Label {
        text,
        x: 0.0,
        y: 0.0,
        size,
        color: layer.text_color(ctx).unwrap_or([0, 0, 0, 255]),
        halo,
        bold,
        italic,
        letter_spacing: num("text-letter-spacing", 0.0, -1.0, 5.0),
        max_width: if line { 0.0 } else { num("text-max-width", 10.0, 0.0, 1000.0) },
        justify,
        // Within a layer a lower sort key places first.
        priority: layer_rank - sort,
        anchor: if line { [0.0, 0.0] } else { anchor },
        offset: if line { [0.0, 0.0] } else { pair(layer.layout_value("text-offset", ctx)).unwrap_or([0.0, 0.0]) },
        angle: None,
        icon,
        collide,
    })
}

/// A boolean layout property (false when absent).
fn flag(layer: &StyleLayer, name: &str, ctx: &FeatureCtx) -> bool {
    layer.layout_value(name, ctx) == Some(Val::Bool(true))
}

/// Whether a label's text turns with the line it is along (`text-rotation-alignment` `map`, or
/// `auto` for line placement), rather than staying upright on the screen.
fn text_turns(layer: &StyleLayer, ctx: &FeatureCtx, placement: SymbolPlacement) -> bool {
    match layer.layout_value("text-rotation-alignment", ctx).as_ref().and_then(Val::as_str) {
        Some("viewport") => false,
        Some("map") => true,
        _ => placement != SymbolPlacement::Point,
    }
}

/// An angle (radians) turned half round when it would read upside down: within (-90°, 90°].
pub fn upright(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    if !a.is_finite() {
        return 0.0;
    }
    let a = (a + PI).rem_euclid(TAU) - PI;
    if a > PI / 2.0 + 1e-4 {
        a - PI
    } else if a <= -PI / 2.0 + 1e-4 {
        a + PI
    } else {
        a
    }
}

/// About how long a label is along its line, in logical points: its text at an average letter
/// width (the renderer measures it with its font when placing it), or its icon when it has no
/// text.
fn label_length(l: &Label) -> f32 {
    let em = l.size;
    let text: f32 = l
        .text
        .chars()
        .map(|c| match c {
            ' ' | 'i' | 'l' | 'I' | 'j' | 't' | 'f' | '.' | ',' | '\'' | '-' => 0.3,
            'm' | 'w' | 'M' | 'W' => 0.85,
            c if c as u32 >= 0x2E80 => 1.0,
            c if c.is_uppercase() => 0.65,
            _ => 0.55,
        })
        .sum::<f32>()
        * em
        * if l.bold { 1.05 } else { 1.0 }
        + l.letter_spacing * em * l.text.chars().count() as f32;
    let icon = l.icon.as_ref().map_or(0.0, |i| i.width);
    text.max(icon).max(1.0)
}

/// An icon for a label (`icon-image`), when the sprite has it, sized by `icon-size` and moved by
/// `icon-offset`; an SDF icon takes `icon-color`. Along a line, an icon turned with the map
/// (`icon-rotation-alignment` `map`, or `auto`) gets an angle, `icon-rotate` to begin with.
fn icon(sheet: &Sprite, layer: &StyleLayer, ctx: &FeatureCtx, placement: SymbolPlacement) -> Option<LabelIcon> {
    let name = layer.layout_value("icon-image", &FeatureCtx { images: Some(sheet), ..*ctx })?;
    let name = name.as_str().filter(|n| !n.is_empty())?;
    let img = sheet.image(name, layer.paint_value("icon-color", ctx).and_then(|c| c.as_color()))?;
    let k = layer.layout_value("icon-size", ctx).and_then(|v| v.as_f64()).filter(|v| v.is_finite()).unwrap_or(1.0).clamp(0.0, 8.0) as f32;
    let (w, h) = (img.pixmap.width() as f32 / img.ratio * k, img.pixmap.height() as f32 / img.ratio * k);
    if !(w >= 0.5 && h >= 0.5) {
        return None;
    }
    let [dx, dy] = pair(layer.layout_value("icon-offset", ctx)).unwrap_or([0.0, 0.0]);
    let rotate =
        layer.layout_value("icon-rotate", ctx).and_then(|v| v.as_f64()).filter(|v| v.is_finite()).unwrap_or(0.0).rem_euclid(360.0) as f32;
    let turns = match layer.layout_value("icon-rotation-alignment", ctx).as_ref().and_then(Val::as_str) {
        Some("viewport") => false,
        Some("map") => true,
        _ => placement != SymbolPlacement::Point,
    };
    let angle = (turns && placement != SymbolPlacement::Point).then_some(rotate.to_radians());
    Some(LabelIcon { image: img.pixmap, width: w, height: h, dx: dx * k, dy: dy * k, angle })
}

/// Two finite numbers (`[x, y]`), clamped to a sane range.
pub(crate) fn pair(v: Option<Val>) -> Option<[f32; 2]> {
    let Val::Arr(a) = v? else { return None };
    let n = |i: usize| a.get(i).and_then(Val::as_f64).filter(|v| v.is_finite()).map(|v| v.clamp(-1000.0, 1000.0) as f32);
    Some([n(0)?, n(1)?])
}

/// `text-anchor` as the text box's shift from the point, in box sizes.
fn text_anchor(layer: &StyleLayer, ctx: &FeatureCtx) -> [f32; 2] {
    let v = layer.layout_value("text-anchor", ctx);
    let (x, y) = match v.as_ref().and_then(Val::as_str).unwrap_or("center") {
        "left" => (0.5, 0.0),
        "right" => (-0.5, 0.0),
        "top" => (0.0, 0.5),
        "bottom" => (0.0, -0.5),
        "top-left" => (0.5, 0.5),
        "top-right" => (-0.5, 0.5),
        "bottom-left" => (0.5, -0.5),
        "bottom-right" => (-0.5, -0.5),
        _ => (0.0, 0.0),
    };
    [x, y]
}

fn inside(x: f32, y: f32, b: [f32; 4]) -> bool {
    x >= b[0] && x < b[2] && y >= b[1] && y < b[3]
}

/// How labels go along a line, in device pixels and radians.
struct LineRule {
    /// Between one label and the next (`symbol-spacing`).
    spacing: f32,
    /// A label's length along the line.
    length: f32,
    /// The most the line may turn across a label (`text-max-angle`).
    max_angle: f32,
    /// One label at the middle of the line (`line-center`).
    center: bool,
}

/// Where labels go along a line (in pixels): each place and the line's direction across the
/// label there (radians, clockwise from the x axis; not yet kept upright). Lines shorter than a
/// label get none, nor do places where the line bends more than the rule allows. Labels repeat
/// every `spacing` from half a label in; a line coming in from past the tile's square (`tile`)
/// starts half a spacing in instead, as it does in the neighbouring tile.
fn line_anchors(line: &[(f32, f32)], rule: &LineRule, tile: [f32; 4]) -> Vec<((f32, f32), f32)> {
    let pts: Vec<(f32, f32)> = line.iter().copied().filter(|p| p.0.is_finite() && p.1.is_finite()).collect();
    // Distance along the line at each vertex.
    let mut along = Vec::with_capacity(pts.len());
    let mut d = 0.0f32;
    for (i, p) in pts.iter().enumerate() {
        if let Some(q) = i.checked_sub(1).and_then(|j| pts.get(j)) {
            d += (p.0 - q.0).hypot(p.1 - q.1);
        }
        along.push(d);
    }
    let total = d;
    let half = rule.length / 2.0;
    if pts.len() < 2 || !(total.is_finite() && total >= rule.length) || rule.length <= 0.0 {
        return Vec::new();
    }
    let try_at = |at: f32| -> Option<((f32, f32), f32)> {
        if at - half < 0.0 || at + half > total {
            return None;
        }
        if turning(&pts, &along, at - half, at + half) > rule.max_angle {
            return None;
        }
        let (a, b, p) = (point_at(&pts, &along, at - half)?, point_at(&pts, &along, at + half)?, point_at(&pts, &along, at)?);
        let angle = (b.1 - a.1).atan2(b.0 - a.0);
        angle.is_finite().then_some((p, angle))
    };
    if rule.center {
        return try_at(total / 2.0).into_iter().collect();
    }
    // A spacing at least a label and a quarter more, so labels never touch.
    let spacing = rule.spacing.max(rule.length + rule.spacing / 4.0).max(1.0);
    let continued = pts.first().is_some_and(|p| !inside(p.0, p.1, tile));
    let start = if continued { (spacing / 2.0) % spacing } else { (half + rule.length * 0.1) % spacing };
    let mut out = Vec::new();
    let mut at = start;
    // At most a few thousand along one line.
    let mut n = 0;
    while at <= total && n < 4000 {
        if let Some(a) = try_at(at) {
            out.push(a);
        }
        at += spacing;
        n += 1;
    }
    // A line with room for one label, but no repeat landing where it fits, gets one in its middle.
    if out.is_empty() && !continued {
        out.extend(try_at(total / 2.0));
    }
    out
}

/// The point at distance `d` along a line whose vertices are at the distances `along`.
fn point_at(pts: &[(f32, f32)], along: &[f32], d: f32) -> Option<(f32, f32)> {
    let i = along.partition_point(|&a| a < d).max(1);
    let (a, b) = (pts.get(i - 1)?, pts.get(i).or(pts.last())?);
    let (da, db) = (*along.get(i - 1)?, *along.get(i).or(along.last())?);
    let t = if db > da { ((d - da) / (db - da)).clamp(0.0, 1.0) } else { 0.0 };
    Some((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t))
}

/// How much a line turns (radians, every bend counted) between the distances `from` and `to`.
fn turning(pts: &[(f32, f32)], along: &[f32], from: f32, to: f32) -> f32 {
    let mut sum = 0.0;
    for i in 1..pts.len().saturating_sub(1) {
        let Some(&d) = along.get(i) else { continue };
        if d <= from || d >= to {
            continue;
        }
        let (Some(a), Some(b), Some(c)) = (pts.get(i - 1), pts.get(i), pts.get(i + 1)) else { continue };
        let (u, v) = ((b.0 - a.0, b.1 - a.1), (c.0 - b.0, c.1 - b.1));
        if u.0.hypot(u.1) < 1e-6 || v.0.hypot(v.1) < 1e-6 {
            continue;
        }
        let turn = (u.0 * v.1 - u.1 * v.0).atan2(u.0 * v.0 + u.1 * v.1).abs();
        if turn.is_finite() {
            sum += turn;
        }
    }
    sum
}

/// A tile polygon's rings grouped into polygons: each outer ring with the holes after it. The
/// outer rings wind one way and the holes the other (the first ring's way is the outer one's).
fn polygons(rings: Vec<Vec<(f32, f32)>>) -> Vec<Vec<Vec<(f32, f32)>>> {
    let mut out: Vec<Vec<Vec<(f32, f32)>>> = Vec::new();
    let mut outer_sign = 0.0f32;
    for r in rings.into_iter().take(10_000) {
        let a = signed_area(&r);
        if a == 0.0 || !a.is_finite() {
            continue;
        }
        if outer_sign == 0.0 {
            outer_sign = a.signum();
        }
        match out.last_mut() {
            Some(p) if a.signum() != outer_sign => p.push(r),
            _ if a.signum() == outer_sign => out.push(vec![r]),
            _ => {}
        }
    }
    out
}

fn signed_area(r: &[(f32, f32)]) -> f32 {
    let mut s = 0.0;
    for (i, a) in r.iter().enumerate() {
        let Some(b) = r.get((i + 1) % r.len()) else { continue };
        s += a.0 * b.1 - b.0 * a.1;
    }
    s / 2.0
}

/// The point inside a polygon (outer ring, then holes) farthest from its edges, to within
/// `precision` (after Mapbox's polylabel, ISC; see `INCLUDED` in `xtask/src/notices.rs`): where a
/// label sits best in an area. `None` for a polygon with no area.
pub(crate) fn pole_of_inaccessibility(poly: &[Vec<(f32, f32)>], precision: f32) -> Option<(f32, f32)> {
    let outer = poly.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in outer.iter().filter(|p| p.0.is_finite() && p.1.is_finite()) {
        (x0, y0, x1, y1) = (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1));
    }
    let (w, h) = (x1 - x0, y1 - y0);
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let cell = w.min(h);
    // A label needs its point to about a pixel, or a two-hundredth of the polygon when that is
    // coarser: a large polygon isn't searched to a fraction of a pixel.
    let precision = precision.max(w.max(h) / 200.0).max(1e-3);
    let make = |c: (f32, f32), h: f32| {
        let d = signed_distance(c, poly);
        Cell { c, h, d, max: d + h * std::f32::consts::SQRT_2 }
    };
    let mut queue: std::collections::BinaryHeap<Cell> = std::collections::BinaryHeap::new();
    let h0 = cell / 2.0;
    let (nx, ny) = (((w / cell).ceil() as usize).min(64), ((h / cell).ceil() as usize).min(64));
    for i in 0..nx {
        for j in 0..ny {
            queue.push(make((x0 + h0 + i as f32 * cell, y0 + h0 + j as f32 * cell), h0));
        }
    }
    // The centroid, and the box's middle, to begin from.
    let mut best = make(centroid(outer).unwrap_or((x0 + w / 2.0, y0 + h / 2.0)), 0.0);
    let middle = make((x0 + w / 2.0, y0 + h / 2.0), 0.0);
    if middle.d > best.d {
        best = middle;
    }
    let mut steps = 0;
    // The cell that could hold the farthest point first.
    while let Some(c) = queue.pop() {
        steps += 1;
        if c.d > best.d {
            best = Cell { c: c.c, h: 0.0, d: c.d, max: c.d };
        }
        if c.max - best.d <= precision {
            continue;
        }
        if steps > 1_000 {
            break;
        }
        let h = c.h / 2.0;
        for (dx, dy) in [(-h, -h), (h, -h), (-h, h), (h, h)] {
            queue.push(make((c.c.0 + dx, c.c.1 + dy), h));
        }
    }
    (best.d > 0.0).then_some(best.c)
}

/// A cell of [`pole_of_inaccessibility`]'s search: its centre, half its side, the distance from
/// its centre to the polygon (signed: negative outside) and the most any point in it can have.
struct Cell {
    c: (f32, f32),
    h: f32,
    d: f32,
    max: f32,
}

impl PartialEq for Cell {
    fn eq(&self, o: &Self) -> bool {
        self.max.total_cmp(&o.max).is_eq()
    }
}
impl Eq for Cell {}
impl PartialOrd for Cell {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Cell {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.max.total_cmp(&o.max)
    }
}

/// The centroid of a ring.
fn centroid(r: &[(f32, f32)]) -> Option<(f32, f32)> {
    let (mut cx, mut cy, mut a) = (0.0f64, 0.0f64, 0.0f64);
    for (i, p) in r.iter().enumerate() {
        let q = r.get((i + 1) % r.len())?;
        let f = f64::from(p.0) * f64::from(q.1) - f64::from(q.0) * f64::from(p.1);
        cx += (f64::from(p.0) + f64::from(q.0)) * f;
        cy += (f64::from(p.1) + f64::from(q.1)) * f;
        a += f * 3.0;
    }
    (a.abs() > 1e-12).then(|| ((cx / a) as f32, (cy / a) as f32))
}

/// The distance from `p` to a polygon's edges: positive inside, negative outside.
fn signed_distance(p: (f32, f32), poly: &[Vec<(f32, f32)>]) -> f32 {
    let mut inside = false;
    let mut best = f32::INFINITY;
    for ring in poly {
        let n = ring.len();
        for i in 0..n {
            let (Some(&a), Some(&b)) = (ring.get(i), ring.get((i + n - 1) % n)) else { continue };
            if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
                inside = !inside;
            }
            best = best.min(segment_distance(p, a, b));
        }
    }
    if !best.is_finite() {
        return f32::NEG_INFINITY;
    }
    if inside { best } else { -best }
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::tests::{item, sheet, world_frame};
    use crate::mvt::{Feature, Value};
    use std::f32::consts::PI;

    fn feature(kind: GeomKind, geometry: Vec<Vec<[f32; 2]>>, name: &str) -> Feature {
        let props = vec![("name".into(), Value::String(name.into())), ("class".into(), Value::String("dot".into()))];
        Feature { id: None, kind, geometry, props }
    }

    fn point(name: &str) -> Feature {
        feature(GeomKind::Point, vec![vec![[2048.0, 2048.0]]], name)
    }

    fn symbol(layout: serde_json::Value) -> StyleLayer {
        StyleLayer::parse(&serde_json::json!({"id": "a", "type": "symbol", "source": "s", "source-layer": "l", "layout": layout,
            "paint": {"icon-color": "#0000ff"}}))
        .unwrap()
    }

    /// The whole world (tile 0/0/0) 4096 pixels across, at zoom 3.
    fn big_frame() -> Frame {
        let half = crate::template::HALF_WORLD;
        Frame { left: -half, top: half, m_per_px: 2.0 * half / 4096.0, ppp: 1.0 }
    }

    fn labels(features: Vec<Feature>, layer: &StyleLayer, frame: &Frame, view: f32) -> Vec<Label> {
        let mut out = Vec::new();
        let zoom = crate::template::style_zoom(frame.m_per_px);
        collect(&mut out, &item(features), layer, frame, zoom, 1.0, Some(&sheet()), (view, view));
        out
    }

    #[test]
    fn an_image_expression_chooses_the_icon() {
        let layer = symbol(serde_json::json!({"icon-image": ["coalesce", ["image", "nope"], ["image", "red"]]}));
        let out = labels(vec![point("")], &layer, &world_frame(), 64.0);
        assert_eq!(out.len(), 1, "an image the sprite has is drawn");
        assert!(out[0].icon.is_some());
        // None of them there: no icon, no label.
        let layer = symbol(serde_json::json!({"icon-image": ["image", "nope"]}));
        assert!(labels(vec![point("")], &layer, &world_frame(), 64.0).is_empty());
    }

    #[test]
    fn labels_points_with_icons() {
        let layer = symbol(serde_json::json!({"text-field": "{name}", "icon-image": "{class}", "icon-size": 2, "text-anchor": "left",
            "text-offset": [1, 0]}));
        let out = labels(vec![point("A"), point("")], &layer, &world_frame(), 64.0);
        assert_eq!(out.len(), 2, "an icon without text is a label too");
        let a = &out[0];
        assert_eq!((a.text.as_str(), a.anchor, a.offset, a.angle), ("A", [0.5, 0.0], [1.0, 0.0], None));
        let icon = a.icon.as_ref().unwrap();
        // `{class}` names the image; icon-size doubles it; the SDF icon takes icon-color.
        assert_eq!((icon.width, icon.height, icon.angle), (8.0, 8.0, None));
        let px = icon.image.pixel(1, 1).unwrap();
        assert_eq!((px.red(), px.blue()), (0, 255));
        assert!(out[1].text.is_empty() && out[1].icon.is_some());
    }

    #[test]
    fn later_layers_place_first_then_lower_sort_keys() {
        let layer = |sort: f64| symbol(serde_json::json!({"text-field": "{name}", "symbol-sort-key": sort}));
        let at = |sort: f64, rank: usize| {
            let mut out = Vec::new();
            collect(&mut out, &item(vec![point("Here")]), &layer(sort), &world_frame(), 0.0, placement_rank(rank), None, (64.0, 64.0));
            out[0].priority
        };
        // The same point labelled by style layers 3 and 7: layer 7's label wins, even with a
        // worse sort key (which only orders labels within a layer).
        assert!(at(50.0, 7) > at(0.0, 3));
        // Within a layer, the lower key first, however far up the style the layer is (an f32
        // priority lost keys a step apart past a few layers).
        assert!(at(1.0, 300) > at(2.0, 300));
        assert!(at(-99_999.0, 300) < at(-99_999.0, 301) && at(99_999.0, 301) > at(-99_999.0, 300));
    }

    #[test]
    fn carries_the_text_style() {
        let layer = symbol(serde_json::json!({"text-field": "{name}", "text-size": 12, "text-letter-spacing": 0.2,
            "text-font": ["Noto Sans Italic"], "text-justify": "left", "text-padding": 5, "text-optional": true,
            "icon-allow-overlap": true, "text-ignore-placement": true}));
        let l = &labels(vec![point("Lake")], &layer, &world_frame(), 64.0)[0];
        // text-size is the font's size in CSS pixels, passed on as it is.
        assert_eq!((l.size, l.letter_spacing, l.italic, l.bold, l.justify), (12.0, 0.2, true, false, -0.5));
        // text-max-width defaults to 10 ems.
        assert_eq!(l.max_width, 10.0);
        let c = l.collide;
        assert_eq!((c.text_padding, c.icon_padding), (5.0, 2.0));
        assert!(c.text_optional && !c.icon_optional && c.icon_allow_overlap && !c.text_allow_overlap && c.text_ignore_placement);
    }

    #[test]
    fn points_on_lines_and_polygons() {
        let layer = symbol(serde_json::json!({"text-field": "{name}"}));
        // A line is labelled at its first vertex.
        let line = feature(GeomKind::LineString, vec![vec![[1000.0, 1000.0], [3000.0, 1000.0]]], "Road");
        let out = labels(vec![line], &layer, &big_frame(), 4096.0);
        assert_eq!(out.iter().map(|l| (l.x, l.y)).collect::<Vec<_>>(), [(1000.0, 1000.0)]);
        // A U-shaped polygon, at its pole of inaccessibility, inside it (its centroid is in the
        // gap between the arms).
        let u = [
            [0.0, 0.0],
            [3000.0, 0.0],
            [3000.0, 3000.0],
            [2000.0, 3000.0],
            [2000.0, 1000.0],
            [1000.0, 1000.0],
            [1000.0, 3000.0],
            [0.0, 3000.0],
        ];
        let mut ring: Vec<[f32; 2]> = u.iter().map(|p: &[f32; 2]| [p[0] + 500.0, p[1] + 500.0]).collect();
        ring.push(ring[0]);
        let out = labels(vec![feature(GeomKind::Polygon, vec![ring.clone()], "Park")], &layer, &big_frame(), 4096.0);
        assert_eq!(out.len(), 1);
        let rings = vec![ring.iter().map(|p| (p[0], p[1])).collect::<Vec<_>>()];
        assert!(signed_distance((out[0].x, out[0].y), &rings) > 400.0, "at {},{}", out[0].x, out[0].y);
        // A hole keeps the label off it.
        let square = |a: f32, b: f32| vec![[a, a], [b, a], [b, b], [a, b], [a, a]];
        let mut hole = square(1500.0, 2500.0);
        hole.reverse();
        let out = labels(vec![feature(GeomKind::Polygon, vec![square(500.0, 3500.0), hole], "Ring")], &layer, &big_frame(), 4096.0);
        let (x, y) = (out[0].x, out[0].y);
        assert!(!(1500.0..=2500.0).contains(&x) || !(1500.0..=2500.0).contains(&y), "in the hole at {x},{y}");
    }

    #[test]
    fn repeats_labels_along_lines() {
        let layer = symbol(serde_json::json!({"text-field": "{name}", "symbol-placement": "line", "text-size": 16, "text-max-width": 3}));
        let road = |pts: Vec<[f32; 2]>| feature(GeomKind::LineString, vec![pts], "Main Street");
        let out = labels(vec![road(vec![[0.0, 2048.0], [4096.0, 2048.0]])], &layer, &big_frame(), 4096.0);
        // Every 250 pixels (the default spacing) along it, never wrapped, turned with the line.
        assert!(out.len() >= 14, "{} labels", out.len());
        for w in out.windows(2) {
            assert!((w[1].x - w[0].x - 250.0).abs() < 1.0);
        }
        assert!(out.iter().all(|l| l.angle == Some(0.0) && l.max_width == 0.0 && l.y == 2048.0));
        // Drawn right to left it still reads left to right; at a slant it is turned to it.
        let back = labels(vec![road(vec![[4000.0, 2000.0], [100.0, 2000.0]])], &layer, &big_frame(), 4096.0);
        assert!(back.iter().all(|l| l.angle.is_some_and(|a| a.abs() < 1e-4)));
        let down = labels(vec![road(vec![[4000.0, 100.0], [100.0, 4000.0]])], &layer, &big_frame(), 4096.0);
        assert!(down.iter().all(|l| l.angle.is_some_and(|a| (a + PI / 4.0).abs() < 1e-3)), "{:?}", down[0].angle);
        // A line shorter than its label has none.
        assert!(labels(vec![road(vec![[100.0, 100.0], [140.0, 100.0]])], &layer, &big_frame(), 4096.0).is_empty());
        // Nor does a zigzag bending more than 45° across every label.
        let zigzag: Vec<[f32; 2]> = (0..100).map(|i| [100.0 + i as f32 * 30.0, if i % 2 == 0 { 100.0 } else { 130.0 }]).collect();
        assert!(labels(vec![road(zigzag)], &layer, &big_frame(), 4096.0).is_empty());
        // Only on the screen (and a little past it).
        let near = labels(vec![road(vec![[0.0, 500.0], [4096.0, 500.0]])], &layer, &big_frame(), 1000.0);
        assert!(!near.is_empty() && near.iter().all(|l| l.x < 1064.0), "{:?}", near.iter().map(|l| l.x).collect::<Vec<_>>());
    }

    #[test]
    fn labels_the_middle_of_a_line_once() {
        let layer = symbol(serde_json::json!({"text-field": "{name}", "symbol-placement": "line-center"}));
        let river = feature(GeomKind::LineString, vec![vec![[100.0, 1000.0], [2100.0, 1000.0], [2100.0, 3000.0]]], "River");
        // Its middle is at a right-angled bend: no label there.
        assert!(labels(vec![river], &layer, &big_frame(), 4096.0).is_empty());
        let river = feature(GeomKind::LineString, vec![vec![[100.0, 1000.0], [2100.0, 1000.0], [2100.0, 1200.0]]], "River");
        let out = labels(vec![river], &layer, &big_frame(), 4096.0);
        assert_eq!(out.len(), 1);
        assert!((out[0].x - 1200.0).abs() < 1.0 && out[0].angle == Some(0.0));
    }

    #[test]
    fn line_labels_stay_in_their_tile() {
        // A line running on past the tile's square (into its buffer) is labelled only inside it.
        let layer = symbol(serde_json::json!({"text-field": "{name}", "symbol-placement": "line"}));
        let half = crate::template::HALF_WORLD;
        // Tile 1/0/0 (the north-west quarter) 2048 pixels across, the view the whole world.
        let at = crate::template::TileId { z: 1, x: 0, y: 0 }.extent();
        let tile = crate::mvt::Tile {
            layers: vec![crate::mvt::Layer {
                name: "l".into(),
                extent: 4096,
                features: vec![feature(GeomKind::LineString, vec![vec![[-400.0, 2048.0], [4500.0, 2048.0]]], "Main Street")],
            }],
        };
        let it = Item { payload: Arc::new(Payload::Vector(tile)), at, z: 1, clip: at };
        let frame = Frame { left: -half, top: half, m_per_px: 2.0 * half / 4096.0, ppp: 1.0 };
        let mut out = Vec::new();
        collect(&mut out, &it, &layer, &frame, 3.0, 1.0, None, (4096.0, 4096.0));
        assert!(!out.is_empty() && out.iter().all(|l| (0.0..2048.0).contains(&l.x)));
        // Coming in from past the tile, the first is half a spacing in.
        assert!((out[0].x - (-200.0 + 125.0)).rem_euclid(250.0) < 1.0, "first at {}", out[0].x);
    }

    #[test]
    fn arrows_along_lines_keep_their_direction() {
        let layer = symbol(serde_json::json!({"icon-image": "red", "symbol-placement": "line", "icon-rotate": 180}));
        let road = feature(GeomKind::LineString, vec![vec![[3000.0, 1000.0], [1000.0, 1000.0]]], "");
        let out = labels(vec![road], &layer, &big_frame(), 4096.0);
        assert!(!out.is_empty());
        // Drawn right to left (π), turned round again by icon-rotate: pointing right.
        let a = out[0].icon.as_ref().and_then(|i| i.angle).unwrap();
        assert!((a.rem_euclid(2.0 * PI)).min(2.0 * PI - a.rem_euclid(2.0 * PI)) < 1e-3, "{a}");
        // A shield stays upright.
        let shield = symbol(serde_json::json!({"icon-image": "red", "text-field": "{name}", "symbol-placement": "line",
            "text-rotation-alignment": "viewport", "icon-rotation-alignment": "viewport"}));
        let road = feature(GeomKind::LineString, vec![vec![[1000.0, 1000.0], [3000.0, 3000.0]]], "I 95");
        let out = labels(vec![road], &shield, &big_frame(), 4096.0);
        assert!(!out.is_empty() && out.iter().all(|l| l.angle.is_none() && l.icon.as_ref().is_some_and(|i| i.angle.is_none())));
    }

    #[test]
    fn keeps_text_upright() {
        for deg in [-180.0f32, -135.0, -91.0, -45.0, 0.0, 45.0, 89.0, 90.0, 135.0, 180.0, 400.0] {
            let a = upright(deg.to_radians());
            assert!(a > -PI / 2.0 - 1e-3 && a <= PI / 2.0 + 1e-3, "{deg} → {a}");
            // The same line, either way along it.
            assert!(((a - deg.to_radians()).rem_euclid(PI)).min(PI - (a - deg.to_radians()).rem_euclid(PI)) < 1e-3);
        }
        assert_eq!(upright(f32::NAN), 0.0);
    }

    #[test]
    fn the_cap_keeps_the_labels_placed_first() {
        let mut kept: Vec<f64> = (0..10).map(f64::from).collect();
        keep_highest(&mut kept, 3, |p| *p);
        kept.sort_by(f64::total_cmp);
        assert_eq!(kept, vec![7.0, 8.0, 9.0], "the last layers' labels, placed first, are kept");
    }

    #[test]
    fn hostile_geometry_is_safe() {
        let layer =
            symbol(serde_json::json!({"text-field": "{name}", "symbol-placement": "line", "symbol-spacing": -5, "text-max-angle": 1e9}));
        let bad = vec![
            feature(GeomKind::LineString, vec![vec![], vec![[f32::NAN, 0.0], [1.0, f32::INFINITY]], vec![[5.0, 5.0]; 3]], "x"),
            feature(GeomKind::Polygon, vec![vec![[0.0, 0.0]; 4], vec![]], "x"),
        ];
        let _ = labels(bad.clone(), &layer, &big_frame(), 4096.0);
        let _ = labels(bad, &symbol(serde_json::json!({"text-field": "{name}"})), &big_frame(), 4096.0);
        assert!(pole_of_inaccessibility(&[], 1.0).is_none());
        let _ = pole_of_inaccessibility(&[vec![(0.0, 0.0), (1e30, 1e30), (f32::NAN, 0.0)]], 0.0);
    }
}
