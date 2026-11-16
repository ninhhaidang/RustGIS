//! A polygon layer's stacked fills (#882 piece 5): drawn bottom first inside each polygon in place
//! of its symbol's single fill, as QGIS's symbol layers stack. Each fills the polygon with its
//! colour, hatches it with parallel lines (QGIS's line pattern fill, and a simple fill's hatched
//! brush styles), dots it with markers on a grid (QGIS's point pattern fill), shades it from one
//! colour to another across it (QGIS's gradient fill: linear, radial or conical) or from its edge
//! inwards (QGIS's shapeburst fill). Sizes are in points, as a symbol's, so a pattern keeps its
//! size on screen at every scale.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::color::Rgba;
use crate::symbology::Marker;

/// The most fills one polygon symbol stacks.
pub const MAX_FILLS: usize = 8;
/// How far apart a pattern's lines or markers may be (points).
pub const PATTERN_SPACING: std::ops::RangeInclusive<f32> = 1.0..=500.0;
/// The widest hatch line (points).
pub const MAX_HATCH_WIDTH: f32 = 50.0;
/// The largest pattern marker (points).
pub const MAX_DOT_SIZE: f32 = 200.0;
/// How far a pattern may be shifted each way (points).
pub const MAX_PATTERN_OFFSET: f32 = 500.0;
/// The most colour stops a gradient has between its two colours.
pub const MAX_STOPS: usize = 16;
/// How far a gradient's reference points may lie outside the feature's box (in boxes).
pub const MAX_GRADIENT_POINT: f32 = 10.0;
/// The deepest a shapeburst shades inwards from the edge (points).
pub const MAX_BURST_DISTANCE: f32 = 1000.0;
/// The widest a shapeburst's shading is blurred (pixels at 96 dpi, as QGIS's 0 to 17).
pub const MAX_BURST_BLUR: u8 = 17;

/// One fill of a polygon layer's stacked symbol (`Layer::fill_layers`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FillLayer {
    /// Its colour (a pattern's lines' or markers'); `None` takes the feature's fill colour from
    /// the renderer, so the fills follow Unique Values and Graduated Colors classes.
    #[serde(default)]
    pub color: Option<Rgba>,
    #[serde(default)]
    pub pattern: FillPattern,
}

/// Whether any of `fills` draws an image as its dots' marker that `f` holds true for.
pub fn fills_have_pictures(fills: &[FillLayer], f: impl Fn(crate::symbology::Picture) -> bool) -> bool {
    fills.iter().any(|fl| matches!(fl.pattern, FillPattern::Dots { marker: Marker::Picture(p), .. } if f(p)))
}

/// What a fill draws inside its polygon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FillPattern {
    /// The whole polygon in its colour.
    #[default]
    Solid,
    /// Parallel lines `spacing` points apart and `width` points wide, at `angle` degrees
    /// anticlockwise from east, shifted `offset` points across them.
    Hatch {
        #[serde(default)]
        angle: f32,
        #[serde(default = "hatch_spacing")]
        spacing: f32,
        #[serde(default = "hatch_width")]
        width: f32,
        #[serde(default)]
        offset: f32,
    },
    /// Markers `size` points across on a grid `spacing` points apart (across, down), every other
    /// row shifted `displacement[0]` across and every other column `displacement[1]` down (QGIS's
    /// displacement), the grid shifted `offset`; outlined in `outline`, `outline_width` wide.
    Dots {
        #[serde(default)]
        marker: Marker,
        #[serde(default = "dot_size")]
        size: f32,
        #[serde(default)]
        outline: Option<Rgba>,
        #[serde(default)]
        outline_width: f32,
        #[serde(default = "dot_spacing")]
        spacing: [f32; 2],
        #[serde(default)]
        displacement: [f32; 2],
        #[serde(default)]
        offset: [f32; 2],
    },
    /// From the fill's colour at `from` to `color2` at `to` (through `stops`), straight across
    /// (linear), outwards from `from` as far as `to` (radial), or round `from` anticlockwise from
    /// the direction of `to` (conical); `from` and `to` are in the feature's box (0 to 1 across
    /// and down) or the map view's (`in_view`), the gradient turned `angle` degrees anticlockwise
    /// round its box's middle, and past `to` padded, reflected or repeated (`spread`).
    Gradient {
        #[serde(default)]
        shape: GradientShape,
        #[serde(default = "white")]
        color2: Rgba,
        #[serde(default)]
        stops: Stops,
        /// Each colour held until the next stop, not blended.
        #[serde(default)]
        discrete: bool,
        #[serde(default = "gradient_from")]
        from: GradientPoint,
        #[serde(default = "gradient_to")]
        to: GradientPoint,
        #[serde(default)]
        spread: Spread,
        #[serde(default)]
        angle: f32,
        #[serde(default)]
        in_view: bool,
    },
    /// From the fill's colour at the polygon's edge to `color2` (through `stops`) `distance`
    /// points in, or at the point deepest inside (`None`: over the whole shape); blurred `blur`
    /// pixels, its holes' edges left out (`ignore_rings`) or shaded from as from the outer one.
    Shapeburst {
        #[serde(default = "white")]
        color2: Rgba,
        #[serde(default)]
        stops: Stops,
        #[serde(default)]
        discrete: bool,
        #[serde(default)]
        distance: Option<f32>,
        #[serde(default)]
        blur: u8,
        #[serde(default)]
        ignore_rings: bool,
    },
}

/// How a gradient runs from its first colour to its second.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GradientShape {
    /// Straight across, from one point to the other.
    #[default]
    Linear,
    /// In rings round the first point, as far out as the second.
    Radial,
    /// Round the first point, anticlockwise from the direction of the second.
    Conical,
}

/// What a gradient draws past its second point (or before its first).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Spread {
    /// Its end colours.
    #[default]
    Pad,
    /// Back and forth.
    Reflect,
    /// Over again.
    Repeat,
}

/// One of a gradient's points: a place in its box (0 to 1 across and down), or the feature's
/// centroid.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GradientPoint {
    At([f32; 2]),
    Centroid(Centroid),
}

/// The feature's centroid, written `"centroid"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Centroid {
    Centroid,
}

impl GradientPoint {
    pub const CENTROID: GradientPoint = GradientPoint::Centroid(Centroid::Centroid);

    fn clamped(self) -> GradientPoint {
        match self {
            GradientPoint::At(p) => GradientPoint::At(p.map(|v| within(v, -MAX_GRADIENT_POINT, MAX_GRADIENT_POINT, 0.5))),
            c => c,
        }
    }
}

/// A colour of a gradient at `at` (0 to 1) along it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub at: f32,
    pub color: Rgba,
}

/// A gradient's colours between its two ends, by where they are along it: at most
/// [`MAX_STOPS`], in order, each within 0 to 1 (written as a list).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "Vec<Stop>", into = "Vec<Stop>")]
pub struct Stops {
    len: u8,
    list: [Stop; MAX_STOPS],
}

impl Default for Stops {
    fn default() -> Stops {
        Stops { len: 0, list: [Stop { at: 0.0, color: Rgba([0; 4]) }; MAX_STOPS] }
    }
}

impl Stops {
    /// `stops` in order along the gradient, each within 0 to 1 (one of no number left out), the
    /// first [`MAX_STOPS`] kept.
    pub fn new(stops: impl IntoIterator<Item = Stop>) -> Stops {
        let mut v: Vec<Stop> = stops.into_iter().filter(|s| s.at.is_finite()).take(MAX_STOPS).collect();
        for s in &mut v {
            s.at = s.at.clamp(0.0, 1.0);
        }
        // Stable: stops at one place keep their order, a hard step.
        v.sort_by(|a, b| a.at.total_cmp(&b.at));
        let mut out = Stops::default();
        for (slot, s) in out.list.iter_mut().zip(&v) {
            *slot = *s;
        }
        out.len = v.len().min(MAX_STOPS) as u8;
        out
    }

    pub fn as_slice(&self) -> &[Stop] {
        self.list.get(..usize::from(self.len)).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl From<Vec<Stop>> for Stops {
    fn from(v: Vec<Stop>) -> Stops {
        Stops::new(v)
    }
}

impl From<Stops> for Vec<Stop> {
    fn from(s: Stops) -> Vec<Stop> {
        s.as_slice().to_vec()
    }
}

/// A gradient's or shapeburst's colours along it, `first` at 0 and `last` at 1 with `stops`
/// between, each held until the next where `discrete`.
pub fn ramp(first: Rgba, last: Rgba, stops: &Stops, discrete: bool) -> Vec<Stop> {
    let mut v = Vec::with_capacity(2 * (stops.as_slice().len() + 2));
    v.push(Stop { at: 0.0, color: first });
    v.extend_from_slice(stops.as_slice());
    v.push(Stop { at: 1.0, color: last });
    if !discrete {
        return v;
    }
    // A step at each stop: the colour before it held up to it.
    let mut out = Vec::with_capacity(2 * v.len());
    for w in v.windows(2) {
        if let [a, b] = w {
            out.push(*a);
            out.push(Stop { at: b.at, color: a.color });
        }
    }
    if let Some(l) = v.last() {
        out.push(*l);
    }
    out
}

/// The colour at `t` (0 to 1) along `ramp` (from [`ramp`]), blended between its stops.
pub fn color_at(ramp: &[Stop], t: f32) -> Rgba {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    let Some(first) = ramp.first() else { return Rgba([0; 4]) };
    let mut prev = *first;
    for s in ramp {
        if t < s.at {
            let span = s.at - prev.at;
            let k = if span > 0.0 { (t - prev.at) / span } else { 1.0 };
            let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * k).round().clamp(0.0, 255.0) as u8;
            let (a, b) = (prev.color.0, s.color.0);
            return Rgba([mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2]), mix(a[3], b[3])]);
        }
        prev = *s;
    }
    prev.color
}

fn white() -> Rgba {
    Rgba([255; 4])
}

/// QGIS's: from the top middle of the box to its bottom middle.
fn gradient_from() -> GradientPoint {
    GradientPoint::At([0.5, 0.0])
}

fn gradient_to() -> GradientPoint {
    GradientPoint::At([0.5, 1.0])
}

fn hatch_spacing() -> f32 {
    8.0
}

fn hatch_width() -> f32 {
    1.0
}

fn dot_size() -> f32 {
    3.0
}

fn dot_spacing() -> [f32; 2] {
    [8.0, 8.0]
}

/// `v` within `lo..=hi`; one of no number is `none`.
fn within(v: f32, lo: f32, hi: f32, none: f32) -> f32 {
    // An infinity to its bound; only NaN, which has no side, to `none`.
    if v.is_nan() { none } else { v.clamp(lo, hi) }
}

impl FillLayer {
    /// A fill in the feature's colour, hatched `angle` degrees `spacing` points apart.
    pub fn hatch(angle: f32, spacing: f32, width: f32) -> FillLayer {
        FillLayer { color: None, pattern: FillPattern::Hatch { angle, spacing, width, offset: 0.0 } }.clamped()
    }

    /// Kept as the command would leave it: sizes within their ranges, an angle within a turn,
    /// a displacement within a cell. A project file is hostile input, and every fill is drawn per
    /// feature per frame.
    pub fn clamped(self) -> FillLayer {
        let pattern = match self.pattern {
            FillPattern::Solid => FillPattern::Solid,
            FillPattern::Hatch { angle, spacing, width, offset } => FillPattern::Hatch {
                angle: if angle.is_finite() { angle.rem_euclid(360.0) } else { 0.0 },
                spacing: within(spacing, *PATTERN_SPACING.start(), *PATTERN_SPACING.end(), hatch_spacing()),
                width: within(width, 0.0, MAX_HATCH_WIDTH, hatch_width()),
                offset: within(offset, -MAX_PATTERN_OFFSET, MAX_PATTERN_OFFSET, 0.0),
            },
            FillPattern::Dots { marker, size, outline, outline_width, spacing, displacement, offset } => {
                let spacing = spacing.map(|s| within(s, *PATTERN_SPACING.start(), *PATTERN_SPACING.end(), dot_spacing()[0]));
                FillPattern::Dots {
                    marker,
                    size: within(size, 0.0, MAX_DOT_SIZE, dot_size()),
                    outline,
                    outline_width: within(outline_width, 0.0, MAX_HATCH_WIDTH, 0.0),
                    displacement: [0, 1].map(|i| {
                        let cell = spacing.get(i).copied().unwrap_or(1.0);
                        displacement.get(i).map_or(0.0, |d| if d.is_finite() { d.rem_euclid(cell) } else { 0.0 })
                    }),
                    offset: offset.map(|o| within(o, -MAX_PATTERN_OFFSET, MAX_PATTERN_OFFSET, 0.0)),
                    spacing,
                }
            }
            FillPattern::Gradient { shape, color2, stops, discrete, from, to, spread, angle, in_view } => FillPattern::Gradient {
                shape,
                color2,
                stops,
                discrete,
                from: from.clamped(),
                to: to.clamped(),
                spread,
                angle: if angle.is_finite() { angle.rem_euclid(360.0) } else { 0.0 },
                in_view,
            },
            FillPattern::Shapeburst { color2, stops, discrete, distance, blur, ignore_rings } => FillPattern::Shapeburst {
                color2,
                stops,
                discrete,
                distance: distance.filter(|d| d.is_finite()).map(|d| d.clamp(0.0, MAX_BURST_DISTANCE)),
                blur: blur.min(MAX_BURST_BLUR),
                ignore_rings,
            },
        };
        FillLayer { pattern, ..self }
    }

    /// Whether it draws anything in a feature of fill `feature`: a colour that shows, and for a
    /// pattern, lines or markers of some size.
    pub fn draws(&self, feature: Rgba) -> bool {
        let shown = self.color.unwrap_or(feature).a() > 0;
        match self.pattern {
            FillPattern::Solid => shown,
            FillPattern::Hatch { width, .. } => shown && width > 0.0,
            // An image shows whatever its colour, as a point's does.
            FillPattern::Dots { marker, size, outline, outline_width, .. } => {
                let image = matches!(marker, Marker::Picture(p) if !p.is_default());
                size > 0.0 && (image || shown || (outline.is_some_and(|o| o.a() > 0) && outline_width > 0.0))
            }
            FillPattern::Gradient { color2, stops, .. } | FillPattern::Shapeburst { color2, stops, .. } => {
                shown || color2.a() > 0 || stops.as_slice().iter().any(|s| s.color.a() > 0)
            }
        }
    }
}

/// A layer's fills read leniently: at most [`MAX_FILLS`], each [`FillLayer::clamped`]; one that
/// isn't a fill is left out, and a value that isn't a list is none, so a damaged entry doesn't
/// make the project unreadable.
pub fn de_fills<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<FillLayer>, D::Error> {
    Ok(fills_of(serde_json::Value::deserialize(d)?))
}

fn fills_of(v: serde_json::Value) -> Vec<FillLayer> {
    let serde_json::Value::Array(list) = v else { return Vec::new() };
    list.into_iter().filter_map(|f| serde_json::from_value::<FillLayer>(f).ok()).take(MAX_FILLS).map(FillLayer::clamped).collect()
}

/// A renderer's per-class fills read as [`de_fills`] reads a layer's; a class left with none is
/// dropped, and a class that doesn't read is left out.
pub fn de_class_fills<'de, D, K>(d: D) -> Result<BTreeMap<K, Vec<FillLayer>>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: serde::de::DeserializeOwned + Ord,
{
    let serde_json::Value::Object(m) = serde_json::Value::deserialize(d)? else { return Ok(BTreeMap::new()) };
    Ok(m.into_iter()
        .filter_map(|(k, v)| {
            let key: K = serde_json::from_value(serde_json::Value::String(k.clone())).or_else(|_| serde_json::from_str(&k)).ok()?;
            let fills = fills_of(v);
            (!fills.is_empty()).then_some((key, fills))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fills are kept; crafted ones are held within their ranges, and ones that aren't fills are
    /// left out without the rest failing to read.
    #[test]
    fn fills_read_leniently_and_within_their_ranges() {
        let hatch = FillLayer::hatch(45.0, 4.0, 0.5);
        let dots = FillLayer {
            color: Some(Rgba::rgb(0, 0, 255)),
            pattern: FillPattern::Dots {
                marker: Marker::Circle,
                size: 2.0,
                outline: None,
                outline_width: 0.0,
                spacing: [6.0, 6.0],
                displacement: [3.0, 0.0],
                offset: [1.0, -1.0],
            },
        };
        let v = serde_json::to_value([FillLayer::default(), hatch, dots]).unwrap();
        assert_eq!(
            v[1],
            serde_json::json!({"color": null, "pattern": {"kind": "hatch", "angle": 45.0, "spacing": 4.0, "width": 0.5, "offset": 0.0}})
        );
        assert_eq!(fills_of(v), vec![FillLayer::default(), hatch, dots]);
        // Damaged: not fills, too many, sizes of no number or far too large.
        let crafted = serde_json::json!([
            7,
            {"pattern": {"kind": "stripes"}},
            {"pattern": {"kind": "hatch", "angle": -90, "spacing": 1e30, "width": -1, "offset": -1e30}},
            {"pattern": {"kind": "dots", "size": 1e9, "spacing": [0, 1e9], "displacement": [99, -1], "offset": [1e9, 0]}}
        ]);
        let read = fills_of(crafted);
        assert_eq!(read.len(), 2, "{read:?}");
        assert_eq!(read[0].pattern, FillPattern::Hatch { angle: 270.0, spacing: 500.0, width: 0.0, offset: -MAX_PATTERN_OFFSET });
        let FillPattern::Dots { size, spacing, displacement, offset, .. } = read[1].pattern else { panic!("{read:?}") };
        assert_eq!((size, spacing, offset), (MAX_DOT_SIZE, [1.0, 500.0], [MAX_PATTERN_OFFSET, 0.0]));
        assert_eq!(displacement, [0.0, 499.0], "within a cell");
        assert_eq!(fills_of(serde_json::json!((0..20).map(|_| serde_json::json!({})).collect::<Vec<_>>())).len(), MAX_FILLS);
        assert!(fills_of(serde_json::json!("solid")).is_empty());
        // What draws: a colour that shows, and a pattern of some size.
        let feature = Rgba::rgb(200, 100, 0);
        assert!(FillLayer::default().draws(feature));
        assert!(!FillLayer::default().draws(Rgba::new(0, 0, 0, 0)));
        assert!(!FillLayer::hatch(0.0, 4.0, 0.0).draws(feature));
    }

    /// A gradient's point out of range is held at the nearest bound, an infinite one too; only
    /// NaN, which has no side, goes to the middle.
    #[test]
    fn gradient_points_out_of_range_are_held_at_their_bound() {
        let held = GradientPoint::At([f32::INFINITY, f32::NEG_INFINITY]).clamped();
        assert_eq!(held, GradientPoint::At([MAX_GRADIENT_POINT, -MAX_GRADIENT_POINT]));
        assert_eq!(GradientPoint::At([f32::NAN, 1e9]).clamped(), GradientPoint::At([0.5, MAX_GRADIENT_POINT]));
    }

    /// A gradient's colours along it, blended or held, its points written as QGIS's or the
    /// centroid; stops kept in order, within 0 to 1, at most [`MAX_STOPS`]; crafted numbers held.
    #[test]
    fn gradients_read_and_shade_along_their_stops() {
        let (red, green, blue) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 255, 0), Rgba::rgb(0, 0, 255));
        let stops = Stops::new([Stop { at: 0.5, color: green }]);
        let ramp = ramp(red, blue, &stops, false);
        assert_eq!(color_at(&ramp, 0.0), red);
        assert_eq!(color_at(&ramp, 0.25), Rgba::rgb(128, 128, 0));
        assert_eq!(color_at(&ramp, 0.5), green);
        assert_eq!(color_at(&ramp, 1.0), blue);
        assert_eq!(color_at(&ramp, f32::NAN), red);
        let held = super::ramp(red, blue, &stops, true);
        assert_eq!((color_at(&held, 0.49), color_at(&held, 0.51), color_at(&held, 1.0)), (red, green, blue));
        let g = FillLayer {
            color: Some(red),
            pattern: FillPattern::Gradient {
                shape: GradientShape::Conical,
                color2: blue,
                stops,
                discrete: false,
                from: GradientPoint::CENTROID,
                to: GradientPoint::At([1.0, 0.5]),
                spread: Spread::Repeat,
                angle: 30.0,
                in_view: false,
            },
        };
        let v = serde_json::to_value([g]).unwrap();
        assert_eq!(v[0]["pattern"]["from"], serde_json::json!("centroid"));
        assert_eq!(v[0]["pattern"]["stops"], serde_json::json!([{"at": 0.5, "color": [0, 255, 0, 255]}]));
        assert_eq!(fills_of(v), vec![g]);
        // Defaults: QGIS's, top middle to bottom middle, to white.
        let read = fills_of(serde_json::json!([{"pattern": {"kind": "gradient"}}, {"pattern": {"kind": "shapeburst"}}]));
        assert!(matches!(
            read[0].pattern,
            FillPattern::Gradient { from: GradientPoint::At([0.5, 0.0]), to: GradientPoint::At([0.5, 1.0]), .. }
        ));
        assert!(matches!(read[1].pattern, FillPattern::Shapeburst { distance: None, blur: 0, color2: Rgba([255, 255, 255, 255]), .. }));
        // Crafted: points far out, too many stops out of order and range, a blur and depth too large.
        let many: Vec<_> = (0..40).map(|i| serde_json::json!({"at": 2.0 - f64::from(i) / 10.0, "color": [0, 0, 0, 255]})).collect();
        let read = fills_of(serde_json::json!([
            {"pattern": {"kind": "gradient", "from": [1e30, -1e30], "angle": -30, "stops": many}},
            {"pattern": {"kind": "shapeburst", "distance": 1e30, "blur": 200}},
            {"pattern": {"kind": "gradient", "from": "middle"}}
        ]));
        assert_eq!(read.len(), 2, "{read:?}");
        let FillPattern::Gradient { from, angle, stops, .. } = read[0].pattern else { panic!("{read:?}") };
        assert_eq!((from, angle), (GradientPoint::At([MAX_GRADIENT_POINT, -MAX_GRADIENT_POINT]), 330.0));
        let at: Vec<f32> = stops.as_slice().iter().map(|s| s.at).collect();
        assert_eq!(at.len(), MAX_STOPS);
        assert!(at.windows(2).all(|w| w[0] <= w[1]) && at.iter().all(|a| (0.0..=1.0).contains(a)), "{at:?}");
        assert!(matches!(read[1].pattern, FillPattern::Shapeburst { distance: Some(MAX_BURST_DISTANCE), blur: MAX_BURST_BLUR, .. }));
        // What draws: either colour showing.
        let clear = Rgba::new(0, 0, 0, 0);
        let burst = |c2| FillLayer {
            color: Some(clear),
            pattern: FillPattern::Shapeburst {
                color2: c2,
                stops: Stops::default(),
                discrete: false,
                distance: None,
                blur: 0,
                ignore_rings: false,
            },
        };
        assert!(burst(blue).draws(clear) && !burst(clear).draws(clear));
    }
}
