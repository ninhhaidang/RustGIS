//! Symbols, renderers (single symbol, unique values, graduated colours, graduated symbols) and
//! labeling. A renderer maps each feature to a [`Symbol`] and describes its legend.

use std::collections::BTreeMap;

use rustgis_data::value::PastClasses;
use rustgis_data::{Dataset, FieldType, GeometryType};
use serde::{Deserialize, Serialize};

use crate::color::{Ramp, Rgba};
use crate::fills::FillLayer;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Marker {
    #[default]
    Circle,
    Square,
    Triangle,
    Diamond,
    Star,
    /// A character of a font, as QGIS's font markers draw one: the size is the font's em.
    Char(Glyph),
}

impl Marker {
    /// The shapes (every marker but [`Marker::Char`]).
    pub const ALL: [Marker; 5] = [Marker::Circle, Marker::Square, Marker::Triangle, Marker::Diamond, Marker::Star];
    pub fn label(self) -> &'static str {
        match self {
            Marker::Circle => "Circle",
            Marker::Square => "Square",
            Marker::Triangle => "Triangle",
            Marker::Diamond => "Diamond",
            Marker::Star => "Star",
            Marker::Char(_) => "Character",
        }
    }
}

/// A character marker's character, and the installed family it is drawn from (empty: the label
/// font). A family the machine lacks draws in the label font, as a label's does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Glyph {
    pub ch: char,
    #[serde(default, skip_serializing_if = "FontName::is_default")]
    pub font: FontName,
}

impl Glyph {
    pub fn new(ch: char, font: &str) -> Glyph {
        Glyph { ch, font: FontName::new(font) }
    }
}

/// The most font names kept (each is kept for the rest of the run); one past them is the label font.
const MAX_FONT_NAMES: usize = 4096;

/// A font family's name, kept once for the rest of the run so a symbol naming one stays `Copy`.
/// Empty for the label font. Names are kept as a label's are ([`font_name`]), and past
/// [`MAX_FONT_NAMES`] different ones (from a crafted file) the rest are the label font.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontName(&'static str);

impl FontName {
    pub fn new(name: &str) -> FontName {
        use std::collections::HashSet;
        use std::sync::{Mutex, OnceLock};
        static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
        let Some(name) = font_name(name) else { return FontName::default() };
        let name = name.as_str();
        let mut names = NAMES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if let Some(kept) = names.get(name) {
            return FontName(kept);
        }
        if names.len() >= MAX_FONT_NAMES {
            return FontName::default();
        }
        let kept: &'static str = Box::leak(name.to_owned().into_boxed_str());
        names.insert(kept);
        FontName(kept)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }

    pub fn is_default(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for FontName {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0)
    }
}

impl<'de> Deserialize<'de> for FontName {
    /// Not a name (a crafted file): the label font, and the rest still reads.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(de_font(d)?.map_or_else(FontName::default, |n| FontName::new(&n)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub fill: Rgba,
    pub stroke: Rgba,
    /// Outline / line width in points.
    pub stroke_width: f32,
    /// Marker size in points.
    pub size: f32,
    pub marker: Marker,
}

/// Default palette for new layers (cycled by layer id).
const LAYER_COLORS: [[u8; 3]; 8] = [
    [0x4A, 0x90, 0xD9],
    [0xE0, 0x7B, 0x39],
    [0x5B, 0xB0, 0x6E],
    [0xC2, 0x55, 0x9B],
    [0xE6, 0xB8, 0x3A],
    [0x7D, 0x6B, 0xD1],
    [0x3C, 0xB1, 0xB8],
    [0xB9, 0x5F, 0x4A],
];

impl Symbol {
    /// Whether anything is drawn with this symbol for a `g` feature: a line its stroke, an area
    /// or point its fill or an outline with a width (a point of size 0 nothing).
    pub fn draws(&self, g: GeometryType) -> bool {
        match g {
            GeometryType::Polyline => self.stroke.a() > 0,
            GeometryType::Point if self.size <= 0.0 => false,
            _ => self.fill.a() > 0 || (self.stroke.a() > 0 && self.stroke_width > 0.0),
        }
    }

    pub fn default_for(g: GeometryType, seed: u32) -> Symbol {
        let c = LAYER_COLORS.get(seed as usize % LAYER_COLORS.len()).copied().unwrap_or([0x4A, 0x90, 0xD9]);
        let base = Rgba::rgb(c[0], c[1], c[2]);
        match g {
            GeometryType::Point => {
                Symbol { fill: base, stroke: Rgba::rgb(255, 255, 255), stroke_width: 1.0, size: 7.0, marker: Marker::Circle }
            }
            GeometryType::Polyline => Symbol { fill: base, stroke: base, stroke_width: 1.6, size: 0.0, marker: Marker::Circle },
            _ => Symbol { fill: base.with_alpha(170), stroke: base.shade(0.6), stroke_width: 0.7, size: 0.0, marker: Marker::Circle },
        }
    }

    /// The seed whose `g` default this symbol is, untouched: a layer keeps its default when it is
    /// copied or added from a layer file under another id, so any seed's default counts.
    pub fn default_seed(&self, g: GeometryType) -> Option<u32> {
        (0..LAYER_COLORS.len() as u32).find(|&seed| *self == Symbol::default_for(g, seed))
    }

    pub fn with_fill(mut self, c: Rgba, g: GeometryType) -> Symbol {
        match g {
            GeometryType::Polyline => {
                self.fill = c;
                self.stroke = c;
            }
            GeometryType::Polygon => {
                self.fill = c.with_alpha(self.fill.a().max(150));
                self.stroke = c.shade(0.6);
            }
            _ => self.fill = c,
        }
        self
    }

    /// The colours a ramp gives a class, taken from `src`: the fill and the outline (a point's too,
    /// which a ramp never sets, so it is one picked by hand). A polygon keeps its own fill opacity.
    fn with_colors_of(mut self, src: &Symbol, g: GeometryType) -> Symbol {
        match g {
            GeometryType::Polyline => {
                self.fill = src.fill;
                self.stroke = src.stroke;
            }
            GeometryType::Polygon => {
                self.fill = src.fill.with_alpha(self.fill.a());
                self.stroke = src.stroke;
            }
            _ => {
                self.fill = src.fill;
                self.stroke = src.stroke;
            }
        }
        self
    }

    /// Ramp colours change hue, not a polygon's existing fill opacity (even below 150).
    fn with_ramp_fill(self, c: Rgba, g: GeometryType) -> Symbol {
        let mut out = self.with_fill(c, g);
        if g == GeometryType::Polygon {
            out.fill = c.with_alpha(self.fill.a());
        }
        out
    }
}

/// How a stroke's open ends are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LineCap {
    #[default]
    Round,
    Butt,
    Square,
}

impl LineCap {
    pub const ALL: [LineCap; 3] = [LineCap::Round, LineCap::Butt, LineCap::Square];
    pub fn code(self) -> &'static str {
        match self {
            LineCap::Round => "round",
            LineCap::Butt => "butt",
            LineCap::Square => "square",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            LineCap::Round => "Round",
            LineCap::Butt => "Flat",
            LineCap::Square => "Square",
        }
    }
    pub fn from_code(s: &str) -> Option<LineCap> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("flat") {
            return Some(LineCap::Butt);
        }
        LineCap::ALL.into_iter().find(|c| c.code().eq_ignore_ascii_case(s))
    }
}

/// How a stroke turns at a vertex.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LineJoin {
    #[default]
    Round,
    Miter,
    Bevel,
}

impl LineJoin {
    pub const ALL: [LineJoin; 3] = [LineJoin::Round, LineJoin::Miter, LineJoin::Bevel];
    pub fn code(self) -> &'static str {
        match self {
            LineJoin::Round => "round",
            LineJoin::Miter => "miter",
            LineJoin::Bevel => "bevel",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            LineJoin::Round => "Round",
            LineJoin::Miter => "Miter",
            LineJoin::Bevel => "Bevel",
        }
    }
    pub fn from_code(s: &str) -> Option<LineJoin> {
        let s = s.trim();
        LineJoin::ALL.into_iter().find(|j| j.code().eq_ignore_ascii_case(s))
    }
}

/// The most strokes one line symbol stacks.
pub const MAX_STROKES: usize = 16;
/// The widest stroke and the farthest offset, in points.
pub const MAX_STROKE_WIDTH: f32 = 50.0;
pub const MAX_STROKE_OFFSET: f32 = 100.0;
/// The most steps a dash pattern has, and the longest step, in points.
pub const MAX_DASH_STEPS: usize = 8;
pub const MAX_DASH_STEP: f32 = 500.0;

/// A stroke's dash pattern: lengths in points, dash and gap in turn, starting with a dash (as
/// SVG's `stroke-dasharray`: an odd number of steps is repeated to make it even). Written to a
/// project as that list, `[4, 2]`. Built only through [`Dash::new`], so its steps are always
/// in range and add up to more than nothing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(into = "Vec<f32>")]
pub struct Dash {
    steps: [f32; MAX_DASH_STEPS],
    len: u8,
}

impl Dash {
    /// The pattern of `steps` (at most [`MAX_DASH_STEPS`], each 0 to [`MAX_DASH_STEP`]), or
    /// `None` for none: no steps, a step that isn't a number from 0 to [`MAX_DASH_STEP`], too
    /// many, or all of them 0.
    pub fn new(steps: &[f32]) -> Option<Dash> {
        let ok = |v: &f32| v.is_finite() && (0.0..=MAX_DASH_STEP).contains(v);
        if steps.is_empty() || steps.len() > MAX_DASH_STEPS || !steps.iter().all(ok) || steps.iter().sum::<f32>() <= 0.0 {
            return None;
        }
        let mut d = Dash { steps: [0.0; MAX_DASH_STEPS], len: u8::try_from(steps.len()).ok()? };
        d.steps.get_mut(..steps.len())?.copy_from_slice(steps);
        Some(d)
    }

    /// Its steps, as given.
    pub fn steps(&self) -> &[f32] {
        self.steps.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// Its steps made even (an odd number twice over), as a renderer takes them.
    pub fn even_steps(&self) -> Vec<f32> {
        let s = self.steps();
        if s.len().is_multiple_of(2) { s.to_vec() } else { s.iter().chain(s).copied().collect() }
    }

    /// One round of the pattern, in points.
    pub fn period(&self) -> f32 {
        self.even_steps().iter().sum()
    }
}

/// Reads a stroke's pattern from a project, brought into range: at most [`MAX_DASH_STEPS`] steps,
/// each clamped to 0 to [`MAX_DASH_STEP`] (one too large for a number, infinite, the longest);
/// one left without length is `None`, solid (a project file is hostile input).
fn de_dash<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Dash>, D::Error> {
    let Some(v) = Option::<Vec<f32>>::deserialize(d)? else { return Ok(None) };
    let steps: Vec<f32> = v.iter().take(MAX_DASH_STEPS).map(|x| if x.is_nan() { 0.0 } else { x.clamp(0.0, MAX_DASH_STEP) }).collect();
    Ok(Dash::new(&steps))
}

impl From<Dash> for Vec<f32> {
    fn from(d: Dash) -> Vec<f32> {
        d.steps().to_vec()
    }
}

/// One stroke of a line layer's stacked symbol (`Layer::line_strokes`). Widths and offsets are
/// in points, so they keep their size on screen at every scale; the geometry is left as it is.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineStroke {
    /// The colour; `None` takes the feature's line colour from the renderer, so the strokes
    /// follow Unique Values and Graduated Colors classes.
    #[serde(default)]
    pub color: Option<Rgba>,
    /// The width in points; `None` takes the feature's line width from the renderer.
    #[serde(default)]
    pub width: Option<f32>,
    /// The sideways shift in points: positive to the right of the line's direction (the order
    /// of its vertices), negative to the left. Two copies of a line drawn in opposite
    /// directions land on opposite sides.
    #[serde(default)]
    pub offset: f32,
    #[serde(default)]
    pub cap: LineCap,
    #[serde(default)]
    pub join: LineJoin,
    /// When the layer draws its strokes in levels (`Layer::stroke_levels`), the level this one
    /// draws at: every feature's strokes of one level go down before any of the next, so casings
    /// join under the cores. `None` is its place in the stack, the bottom one 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    /// Dashed: the pattern along the line, in points; `None` is a solid line.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_dash")]
    pub dash: Option<Dash>,
    /// Markers or hash marks along the line instead of the stroke: in the stroke's colour, the
    /// width their outline's (a hash mark's line's), shifted sideways by its offset. Cap, join
    /// and dash don't apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markers: Option<LineMarkers>,
}

/// Whether a stack of fills (a polygon's) or strokes (a line's or a point's) hides the colors a
/// color scheme sets on a symbol (#1355): the stack is drawn for the geometry (a polygon's stacked
/// strokes and a line's stacked fills are not) and every layer of it sets a color of its own. A layer
/// without one takes the symbol's (`FillLayer::color`, `LineStroke::color`), so the class still shows
/// its color through it: a road's black casing over a core that follows the class. An empty stack is
/// no stack. Used for a class's own stack and for the layer's.
pub fn stack_hides_symbol(g: GeometryType, fills: &[FillLayer], strokes: &[LineStroke]) -> bool {
    match g {
        GeometryType::Polygon => !fills.is_empty() && fills.iter().all(|f| f.color.is_some()),
        _ => !strokes.is_empty() && strokes.iter().all(|s| s.color.is_some()),
    }
}

/// What [`Renderer::reverse_ramp`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RampReversal {
    /// How many classes changed colors with another: 0 when fewer than two classes show the scheme.
    pub reversed: usize,
    /// How many classes were left as they were, their own stack hiding their symbol.
    pub kept: usize,
    /// How many of the `reversed` classes have no stack of their own, so they are drawn from the layer's.
    pub on_layer_stack: usize,
}

impl Default for LineStroke {
    fn default() -> Self {
        LineStroke {
            color: None,
            width: None,
            offset: 0.0,
            cap: LineCap::Round,
            join: LineJoin::Round,
            level: None,
            dash: None,
            markers: None,
        }
    }
}

/// The largest marker or hash mark along a line, in points.
pub const MAX_LINE_MARKER_SIZE: f32 = 100.0;
/// The shortest and longest interval between markers along a line, in points.
pub const MIN_MARKER_INTERVAL: f32 = 0.5;
pub const MAX_MARKER_INTERVAL: f32 = 10_000.0;

/// Where a stroke's markers go on each part of a line, several places at once. Written to a
/// project as their codes, `["first", "last"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Placements(u8);

impl Placements {
    /// At every interval along the line, from its start.
    pub const INTERVAL: Placements = Placements(1);
    pub const VERTICES: Placements = Placements(1 << 1);
    /// Every vertex but the first and the last.
    pub const INNER_VERTICES: Placements = Placements(1 << 2);
    pub const FIRST: Placements = Placements(1 << 3);
    pub const LAST: Placements = Placements(1 << 4);
    /// Halfway along the line.
    pub const MIDDLE: Placements = Placements(1 << 5);
    /// Halfway along each segment.
    pub const SEGMENT_MIDDLES: Placements = Placements(1 << 6);
    pub const NONE: Placements = Placements(0);

    const CODES: [(Placements, &'static str); 7] = [
        (Placements::INTERVAL, "interval"),
        (Placements::VERTICES, "vertices"),
        (Placements::INNER_VERTICES, "innerVertices"),
        (Placements::FIRST, "first"),
        (Placements::LAST, "last"),
        (Placements::MIDDLE, "middle"),
        (Placements::SEGMENT_MIDDLES, "segmentMiddles"),
    ];

    pub fn has(self, p: Placements) -> bool {
        self.0 & p.0 != 0
    }

    pub fn with(self, p: Placements) -> Placements {
        Placements(self.0 | p.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Its codes, in a fixed order.
    pub fn codes(self) -> Vec<&'static str> {
        Self::CODES.iter().filter(|(p, _)| self.has(*p)).map(|(_, c)| *c).collect()
    }

    /// The placement a code names ("first", "middle", …), whatever its case.
    pub fn from_code(code: &str) -> Option<Placements> {
        Self::CODES.iter().find(|(_, c)| c.eq_ignore_ascii_case(code.trim())).map(|(p, _)| *p)
    }

    /// Every code there is, for messages.
    pub fn all_codes() -> String {
        Self::CODES.iter().map(|(_, c)| format!("\"{c}\"")).collect::<Vec<_>>().join(", ")
    }
}

impl Default for Placements {
    fn default() -> Self {
        Placements::INTERVAL
    }
}

impl Serialize for Placements {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.codes().serialize(s)
    }
}

/// Read from a project leniently: codes it doesn't know are left out, and none at all is the
/// default (at every interval).
impl<'de> Deserialize<'de> for Placements {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let codes: Vec<String> = Vec::deserialize(d)?;
        let p = codes.iter().take(16).filter_map(|c| Placements::from_code(c)).fold(Placements::NONE, Placements::with);
        Ok(if p.is_empty() { Placements::default() } else { p })
    }
}

/// A point layer's markers turned by a field's angle ([`crate::Layer::marker_rotation`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkerRotation {
    /// The field the angle is read from, in degrees.
    pub field: String,
    /// The angle runs counterclockwise (as in mathematics), not clockwise (as on a compass).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub counterclockwise: bool,
}

/// Reads a layer's marker rotation leniently: one that isn't (a crafted file's) is none, and the rest
/// of the layer still reads. A field name is at most [`MAX_FONT_NAME`]·2 characters.
pub fn de_marker_rotation<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<MarkerRotation>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Read {
        Rotation(MarkerRotation),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Read>::deserialize(d)? {
        Some(Read::Rotation(r)) if !r.field.is_empty() && r.field.chars().count() <= 2 * MAX_FONT_NAME => Some(r),
        _ => None,
    })
}

impl MarkerRotation {
    /// Row `row`'s turn in degrees clockwise: 0 where its value isn't a number (or the field is gone).
    pub fn turn(&self, ds: &Dataset, row: usize) -> f32 {
        let Some(v) = ds.field_index(&self.field).and_then(|fi| ds.value(row, fi).as_f64()).filter(|v| v.is_finite()) else {
            return 0.0;
        };
        let a = v.rem_euclid(360.0) as f32;
        if self.counterclockwise { -a } else { a }
    }
}

/// What a stroke draws along a line instead of a line: a marker, or a hash mark across it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineMark {
    Marker(Marker),
    Hash,
}

impl Default for LineMark {
    fn default() -> Self {
        LineMark::Marker(Marker::Circle)
    }
}

/// Markers or hash marks along a line (a [`LineStroke::markers`]), as QGIS's marker and hashed
/// lines draw them. Lengths are in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineMarkers {
    #[serde(default)]
    pub mark: LineMark,
    /// A marker's size, or a hash mark's length.
    #[serde(default = "LineMarkers::default_size")]
    pub size: f32,
    #[serde(default)]
    pub at: Placements,
    /// The distance between two markers placed at every interval.
    #[serde(default = "LineMarkers::default_interval")]
    pub interval: f32,
    /// How far along the line the markers move: from its start (at every interval, the first
    /// vertex), back from its end (the last vertex), or on from the middle.
    #[serde(default)]
    pub offset_along: f32,
    /// Turned with the line, so a marker's top faces left of the line's direction; otherwise
    /// upright.
    #[serde(default = "LineMarkers::yes")]
    pub rotate: bool,
    /// The stretch of line, centred on a marker placed at an interval, whose direction it turns
    /// with: smoother on a line of many short bends. 0 is the direction of the segment it sits on.
    #[serde(default)]
    pub average_angle: f32,
    /// Turned further, in degrees clockwise. A hash mark at 0 crosses the line square.
    #[serde(default)]
    pub angle: f32,
    /// A marker's outline colour; `None` draws none. Its width is the stroke's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<Rgba>,
}

impl Default for LineMarkers {
    fn default() -> Self {
        LineMarkers {
            mark: LineMark::default(),
            size: Self::default_size(),
            at: Placements::default(),
            interval: Self::default_interval(),
            offset_along: 0.0,
            rotate: true,
            average_angle: 0.0,
            angle: 0.0,
            outline: None,
        }
    }
}

impl LineMarkers {
    fn default_size() -> f32 {
        4.0
    }

    fn default_interval() -> f32 {
        12.0
    }

    fn yes() -> bool {
        true
    }

    /// Brought into range: a size from 0 to [`MAX_LINE_MARKER_SIZE`], an interval from
    /// [`MIN_MARKER_INTERVAL`] to [`MAX_MARKER_INTERVAL`], an offset along the line and an
    /// averaging stretch within the longest interval, an angle within a turn.
    pub fn clamped(mut self) -> LineMarkers {
        // Too large for an f32 (infinite once read) is the nearest bound, as a large value is.
        let fit = |v: f32, lo: f32, hi: f32, or: f32| if v.is_nan() { or } else { v.clamp(lo, hi) };
        self.size = fit(self.size, 0.0, MAX_LINE_MARKER_SIZE, Self::default_size());
        self.interval = fit(self.interval, MIN_MARKER_INTERVAL, MAX_MARKER_INTERVAL, Self::default_interval());
        self.offset_along = fit(self.offset_along, -MAX_MARKER_INTERVAL, MAX_MARKER_INTERVAL, 0.0);
        self.average_angle = fit(self.average_angle, 0.0, MAX_MARKER_INTERVAL, 0.0);
        self.angle = if self.angle.is_finite() { self.angle.rem_euclid(360.0) } else { 0.0 };
        self
    }
}

impl LineStroke {
    /// The level it draws at as the `i`th stroke of its stack (see [`LineStroke::level`]).
    pub fn level_at(&self, i: usize) -> u8 {
        self.level.unwrap_or_else(|| u8::try_from(i).unwrap_or(u8::MAX))
    }

    /// The colour and width it draws a feature whose renderer symbol is `s` with.
    pub fn resolve(&self, s: &Symbol) -> (Rgba, f32) {
        (self.color.unwrap_or(s.stroke), self.width.unwrap_or(s.stroke_width))
    }

    /// Brought into range: finite, width 0 to [`MAX_STROKE_WIDTH`], offset within
    /// ±[`MAX_STROKE_OFFSET`].
    pub fn clamped(mut self) -> LineStroke {
        self.width = self.width.map(|w| if w.is_finite() { w.clamp(0.0, MAX_STROKE_WIDTH) } else { 1.0 });
        self.offset = if self.offset.is_finite() { self.offset.clamp(-MAX_STROKE_OFFSET, MAX_STROKE_OFFSET) } else { 0.0 };
        self.markers = self.markers.map(LineMarkers::clamped);
        self
    }
}

/// Reads a layer's strokes as the command would leave them: at most [`MAX_STROKES`], each
/// [`LineStroke::clamped`]. A project file is hostile input, and every stroke is drawn per
/// feature per frame.
pub fn de_strokes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<LineStroke>, D::Error> {
    let mut v: Vec<LineStroke> = Vec::deserialize(d)?;
    v.truncate(MAX_STROKES);
    Ok(v.into_iter().map(LineStroke::clamped).collect())
}

/// Reads a renderer's per-class strokes as [`de_strokes`] reads a layer's: each class's at most
/// [`MAX_STROKES`], each [`LineStroke::clamped`]. A class left with none is dropped.
pub fn de_class_strokes<'de, D, K>(d: D) -> Result<BTreeMap<K, Vec<LineStroke>>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: Deserialize<'de> + Ord,
{
    let m: BTreeMap<K, Vec<LineStroke>> = BTreeMap::deserialize(d)?;
    Ok(m.into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, mut v)| {
            v.truncate(MAX_STROKES);
            (k, v.into_iter().map(LineStroke::clamped).collect())
        })
        .collect())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClassMethod {
    #[default]
    NaturalBreaks,
    EqualInterval,
    Quantile,
    /// Class widths that grow by one ratio, for values that pile up at the low end.
    GeometricInterval,
    /// Classes one standard deviation wide about the mean.
    StandardDeviation,
    /// Class widths that grow by one step (1, 2, 3… units), for more detail among the low values.
    ArithmeticProgression,
    /// Splits at the mean, then at the mean of the values above it, while they stay a minority: for
    /// a few very large values among many small ones. It sets its own number of classes, at most
    /// the number asked.
    HeadTailBreaks,
    /// Bounds typed by hand, kept as they are (see [`Renderer::graduated_manual`]).
    Manual,
}

impl ClassMethod {
    pub const ALL: [ClassMethod; 8] = [
        ClassMethod::NaturalBreaks,
        ClassMethod::EqualInterval,
        ClassMethod::Quantile,
        ClassMethod::GeometricInterval,
        ClassMethod::StandardDeviation,
        ClassMethod::ArithmeticProgression,
        ClassMethod::HeadTailBreaks,
        ClassMethod::Manual,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ClassMethod::NaturalBreaks => "Natural Breaks (Jenks)",
            ClassMethod::EqualInterval => "Equal Interval",
            ClassMethod::Quantile => "Quantile",
            ClassMethod::GeometricInterval => "Geometric Interval",
            ClassMethod::StandardDeviation => "Standard Deviation",
            ClassMethod::ArithmeticProgression => "Arithmetic Progression",
            ClassMethod::HeadTailBreaks => "Head/Tail Breaks",
            ClassMethod::Manual => "Manual",
        }
    }
    pub fn from_name(s: &str) -> Option<ClassMethod> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "naturalbreaks" | "natural_breaks" | "jenks" | "natural breaks" => ClassMethod::NaturalBreaks,
            "equalinterval" | "equal_interval" | "equal interval" => ClassMethod::EqualInterval,
            "quantile" => ClassMethod::Quantile,
            "geometricinterval" | "geometric_interval" | "geometric interval" | "geometric" => ClassMethod::GeometricInterval,
            "standarddeviation" | "standard_deviation" | "standard deviation" | "stddev" => ClassMethod::StandardDeviation,
            "arithmeticprogression" | "arithmetic_progression" | "arithmetic progression" | "arithmetic" => {
                ClassMethod::ArithmeticProgression
            }
            "headtailbreaks" | "head_tail_breaks" | "head/tail breaks" | "head/tail" | "headtail" => ClassMethod::HeadTailBreaks,
            "manual" => ClassMethod::Manual,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Renderer {
    Single(Symbol),
    Unique {
        field: String,
        classes: Vec<(String, Symbol)>,
        other: Symbol,
        ramp: Ramp,
        /// Legend labels set by hand, by class value; a class without one shows its value. A value
        /// listed twice (a style import can do that) shares one label, as it shares one drawn
        /// symbol: features take their value's first class (`symbol_for`).
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        class_labels: BTreeMap<String, String>,
        /// The legend label of `other`, when set by hand (otherwise `<all other values>`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        other_label: Option<String>,
        /// A line layer's stacked strokes for one class, by class value: drawn for that class's
        /// features instead of the layer's `line_strokes`. A class without its own takes the layer's.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "de_class_strokes")]
        class_strokes: BTreeMap<String, Vec<LineStroke>>,
        /// The stacked strokes of `other`; empty takes the layer's.
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "de_strokes")]
        other_strokes: Vec<LineStroke>,
        /// A polygon layer's stacked fills for one class, by class value: drawn for that class's
        /// features instead of the layer's `fill_layers`. A class without its own takes the layer's.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "crate::fills::de_class_fills")]
        class_fills: BTreeMap<String, Vec<FillLayer>>,
        /// The stacked fills of `other`; empty takes the layer's.
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::fills::de_fills")]
        other_fills: Vec<FillLayer>,
        /// Unique Values by expression: a feature's class value is this expression's (in the
        /// language of [`rustgis_data::query`]) in place of `field`'s, which is then empty. The
        /// text is kept and compiled against the layer's fields where it is used
        /// ([`Renderer::classer`]); where it doesn't read, or gives null, a feature is `other`.
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_expression")]
        expression: Option<String>,
    },
    /// Upper bounds of each class (ascending) and one symbol per class.
    Graduated {
        field: String,
        breaks: Vec<f64>,
        symbols: Vec<Symbol>,
        ramp: Ramp,
        method: ClassMethod,
        min: f64,
        /// The classes asked for: there can be fewer breaks, as Standard Deviation, Head/Tail
        /// Breaks and repeated bounds give fewer. 0 (a project from before it was kept) is as many
        /// as there are breaks; see [`Renderer::classes_asked`].
        #[serde(default)]
        classes: usize,
        /// Legend labels set by hand, by class index (0 is the lowest class); a class without one
        /// shows its range.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        class_labels: BTreeMap<usize, String>,
        /// A line layer's stacked strokes for one class, by class index, as Unique Values'.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "de_class_strokes")]
        class_strokes: BTreeMap<usize, Vec<LineStroke>>,
        /// A polygon layer's stacked fills for one class, by class index, as Unique Values'.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "crate::fills::de_class_fills")]
        class_fills: BTreeMap<usize, Vec<FillLayer>>,
    },
    /// Marker size grows with the value.
    Proportional {
        field: String,
        base: Symbol,
        min_size: f32,
        max_size: f32,
        min: f64,
        max: f64,
    },
}

/// The legend label of a Unique Values renderer's fallback class.
pub const OTHER_VALUES: &str = "<all other values>";

/// The most features × classes a legend of Unique Values by expression classes its features for
/// (it is worked out as the Layers pane is drawn): past it, every class is listed.
pub const MAX_LEGEND_STEPS: usize = 2_000_000;

/// One legend row.
#[derive(Clone, Debug, PartialEq)]
pub struct LegendEntry {
    pub label: String,
    pub symbol: Symbol,
    /// The stacked strokes its class has of its own; empty, it takes the layer's `line_strokes`.
    pub strokes: Vec<LineStroke>,
    /// The Unique Values fallback row (`<all other values>`), told apart from a class whatever
    /// its label: a legend can leave it out without touching a class that bears the same name.
    pub fallback: bool,
}

impl LegendEntry {
    /// A class's row, with the stacked strokes it has of its own, if any.
    fn class(label: String, symbol: Symbol, strokes: Option<&Vec<LineStroke>>) -> Self {
        LegendEntry { label, symbol, strokes: strokes.cloned().unwrap_or_default(), fallback: false }
    }
}

/// Most unique-value classes created automatically.
pub const MAX_UNIQUE: usize = 64;
/// The most classes by an expression: a QGIS rule-based style's rules (one class each), kept when
/// its classes are made again from the expression.
pub const MAX_EXPRESSION_CLASSES: usize = 200;

/// The colour of a new Unique Values renderer's `<all other values>` class.
pub const OTHER_FILL: Rgba = Rgba::rgb(0xB0, 0xB0, 0xB0);

/// The longest legend label set by hand, in characters.
pub const MAX_LABEL: usize = 200;

/// One class of a Unique Values or Graduated Colors renderer, as a command names it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClassRef {
    /// A Unique Values class, by its value.
    Value(String),
    /// A Graduated Colors class, by index (0 is the lowest).
    Index(usize),
    /// The Unique Values fallback, `<all other values>`.
    Other,
}

/// Reads a class expression leniently: text up to [`rustgis_data::query::MAX_LEN`], blank or
/// anything else none.
fn de_expression<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) if !s.trim().is_empty() && s.chars().count() <= rustgis_data::query::MAX_LEN => Some(s),
        _ => None,
    })
}

/// A renderer's class expression ([`Renderer::Unique`]'s `expression`) compiled against a
/// dataset's fields for one drawing, with the map scale it reads (`@map_scale`); nothing for a
/// renderer that classes by a field. Built by [`Renderer::classer`].
#[derive(Clone, Debug, Default)]
pub struct Classer {
    expr: Option<rustgis_data::query::Expr>,
    env: rustgis_data::query::Env,
}

/// The class a feature falls in ([`Renderer::class_of`]): a place in the classes, or Unique
/// Values' `<all other values>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Class(usize),
    Other,
}

fn no_class(i: usize, n: usize) -> String {
    format!("no class {i}: this renderer has {n} (0 to {})", n.saturating_sub(1))
}

/// Why a class reference doesn't fit the renderer, as [`Renderer::set_class_style`] says it.
fn class_mismatch(r: &Renderer) -> String {
    match r {
        Renderer::Unique { .. } => "Unique Values classes are named by their value, not by number".into(),
        Renderer::Graduated { .. } => "Graduated Colors classes are numbered from 0, the lowest".into(),
        _ => "only Unique Values and Graduated Colors have classes to style".into(),
    }
}

/// What [`Renderer::set_class_style`] changes in a class; `None` leaves that part as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassStyle {
    pub color: Option<Rgba>,
    /// For RGB-only command input, keep the polygon's legacy translucency instead of setting alpha.
    pub preserve_fill_alpha: bool,
    pub outline: Option<Rgba>,
    /// `Some("")` clears a label set by hand, so the class shows its value or range again.
    pub label: Option<String>,
}

impl Renderer {
    pub fn kind_label(&self) -> &'static str {
        match self {
            Renderer::Single(_) => "Single Symbol",
            Renderer::Unique { .. } => "Unique Values",
            Renderer::Graduated { .. } => "Graduated Colors",
            Renderer::Proportional { .. } => "Proportional Symbols",
        }
    }

    /// The field it classes or sizes by; `None` for a single symbol and for Unique Values by
    /// expression ([`Renderer::expression`]).
    pub fn field(&self) -> Option<&str> {
        match self {
            Renderer::Single(_) | Renderer::Unique { expression: Some(_), .. } => None,
            Renderer::Unique { field, .. } | Renderer::Graduated { field, .. } | Renderer::Proportional { field, .. } => Some(field),
        }
    }

    /// The expression Unique Values class by, in place of a field.
    pub fn expression(&self) -> Option<&str> {
        match self {
            Renderer::Unique { expression: Some(e), .. } => Some(e),
            _ => None,
        }
    }

    /// Its class expression ([`Renderer::expression`]) compiled against `ds` for drawing at the
    /// map scale 1:`map_scale` (none: no map, as in an export of the features).
    pub fn classer(&self, ds: &Dataset, map_scale: Option<f64>) -> Classer {
        let Some(text) = self.expression() else { return Classer::default() };
        Classer {
            expr: rustgis_data::query::parse_fields(text, ds).ok(),
            env: rustgis_data::query::Env { map_scale: map_scale.filter(|s| s.is_finite() && *s > 0.0) },
        }
    }

    /// The first symbol (the single symbol, or a representative).
    pub fn base_symbol(&self) -> Symbol {
        match self {
            Renderer::Single(s) => *s,
            Renderer::Unique { other, classes, .. } => classes.first().map(|c| c.1).unwrap_or(*other),
            Renderer::Graduated { symbols, .. } => symbols.last().copied().unwrap_or(Symbol::default_for(GeometryType::Polygon, 0)),
            Renderer::Proportional { base, .. } => *base,
        }
    }

    /// Builds a unique-values renderer on `field`, from the rows `rows` keeps (a row past its end
    /// counts, so `&[]` keeps them all).
    pub fn unique(ds: &Dataset, rows: &[bool], field: &str, ramp: Ramp, base: Symbol) -> Result<Renderer, String> {
        Self::unique_counted(ds, rows, field, ramp, base).map(|(r, _)| r)
    }

    /// [`Renderer::unique`], and how many kept features with a geometry have a value past the
    /// first [`MAX_UNIQUE`] (by number when every value reads as one, else by text), drawn as
    /// `<all other values>`.
    pub fn unique_counted(ds: &Dataset, rows: &[bool], field: &str, ramp: Ramp, base: Symbol) -> Result<(Renderer, PastClasses), String> {
        let fi = ds.field_index(field).ok_or_else(|| format!("no field named '{field}'"))?;
        let (vals, rest) = ds.unique_values_counted(fi, rows, MAX_UNIQUE);
        let n = vals.len();
        let classes = vals.into_iter().enumerate().map(|(k, v)| (v, base.with_ramp_fill(ramp.pick(k, n), ds.geometry_type))).collect();
        let other = base.with_ramp_fill(OTHER_FILL, ds.geometry_type);
        let renderer = Renderer::Unique {
            field: ds.fields.get(fi).map(|f| f.name.clone()).unwrap_or_default(),
            classes,
            other,
            ramp,
            class_labels: BTreeMap::new(),
            other_label: None,
            class_strokes: BTreeMap::new(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        Ok((renderer, rest))
    }

    /// Builds a Unique Values renderer classing by an expression's values over the rows `rows`
    /// keeps (a row past its end counts), at most [`MAX_UNIQUE`] of them in order; a row whose
    /// value is null is `other`. The values are those it gives without a map (`@map_scale` null):
    /// a value it gives only at some scales is a class once it is given then (as a style import
    /// makes them), else drawn as `other`. The text is checked against `ds`'s fields.
    pub fn unique_by(ds: &Dataset, rows: &[bool], expression: &str, ramp: Ramp, base: Symbol) -> Result<Renderer, String> {
        Self::unique_by_counted(ds, rows, expression, ramp, base).map(|(r, _)| r)
    }

    /// [`Renderer::unique_by`], and how many kept features with a geometry have a value past the
    /// first [`MAX_UNIQUE`] (by number when every value reads as one, else by text), drawn as
    /// `<all other values>`.
    pub fn unique_by_counted(
        ds: &Dataset,
        rows: &[bool],
        expression: &str,
        ramp: Ramp,
        base: Symbol,
    ) -> Result<(Renderer, PastClasses), String> {
        Self::unique_by_up_to(ds, rows, expression, ramp, base, MAX_UNIQUE)
    }

    /// [`Renderer::unique_by_counted`] keeping up to `max` values (at most
    /// [`MAX_EXPRESSION_CLASSES`]), and how many features have a value past them.
    pub fn unique_by_up_to(
        ds: &Dataset,
        rows: &[bool],
        expression: &str,
        ramp: Ramp,
        base: Symbol,
        max: usize,
    ) -> Result<(Renderer, PastClasses), String> {
        let max = max.clamp(1, MAX_EXPRESSION_CLASSES);
        let text = expression.trim();
        if text.chars().count() > rustgis_data::query::MAX_LEN {
            return Err(format!("the expression is longer than {} characters", rustgis_data::query::MAX_LEN));
        }
        let e = rustgis_data::query::parse_fields(text, ds)?;
        if rustgis_data::query::uses_area(&e)
            && let Some(message) = rustgis_data::ambiguous_area_message(
                ds.features
                    .iter()
                    .enumerate()
                    .filter(|(row, _)| rows.get(*row).copied().unwrap_or(true))
                    .filter_map(|(_, feature)| feature.geometry.as_ref()),
            )
        {
            return Err(message);
        }
        let env = rustgis_data::query::Env::default();
        // The first values by text, and while every value reads as a number, by number too: the
        // order is known only once every feature is classed (`rustgis_data::value::numeric_classes`).
        let mut by_text: FirstValues<String> = FirstValues::new(max);
        let mut by_number: FirstValues<rustgis_data::value::ClassText> = FirstValues::new(max);
        let mut text_seen = false;
        for r in (0..ds.features.len()).filter(|r| rows.get(*r).copied().unwrap_or(true)) {
            let v = match rustgis_data::query::value(&e, ds, r, &env) {
                rustgis_data::Value::Null => continue,
                rustgis_data::Value::Text(t) => t,
                v => v.to_string(),
            };
            // (A feature without a geometry draws nothing, so it isn't counted.)
            let drawn = usize::from(ds.features.get(r).is_some_and(|f| f.geometry.is_some()));
            // Read as a number once.
            let read = rustgis_data::value::ClassText::new(v);
            text_seen = text_seen || read.orders_as_text();
            if !text_seen {
                by_number.add(read.clone(), drawn);
            }
            by_text.add(read.into_text(), drawn);
        }
        // (Numbers come before the empty text and `<Null>`, so with any number, one is kept.)
        let numeric = !text_seen && rustgis_data::value::ClassText::numeric(by_number.kept.keys());
        let (vals, features): (Vec<String>, usize) = if numeric {
            (by_number.kept.into_keys().map(rustgis_data::value::ClassText::into_text).collect(), by_number.rest)
        } else {
            (by_text.kept.into_keys().collect(), by_text.rest)
        };
        let rest = PastClasses { features, by_number: numeric };
        let n = vals.len();
        let classes = vals.into_iter().enumerate().map(|(k, v)| (v, base.with_ramp_fill(ramp.pick(k, n), ds.geometry_type))).collect();
        Ok((
            Renderer::Unique {
                field: String::new(),
                classes,
                other: base.with_ramp_fill(OTHER_FILL, ds.geometry_type),
                ramp,
                class_labels: BTreeMap::new(),
                other_label: None,
                class_strokes: BTreeMap::new(),
                other_strokes: Vec::new(),
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: Some(text.to_string()),
            },
            rest,
        ))
    }

    /// Builds a graduated-colours renderer on a numeric `field`, classing the rows `rows` keeps (a
    /// row past its end counts, so `&[]` keeps them all).
    pub fn graduated(
        ds: &Dataset,
        rows: &[bool],
        field: &str,
        classes: usize,
        method: ClassMethod,
        ramp: Ramp,
        base: Symbol,
    ) -> Result<Renderer, String> {
        let k = classes.clamp(1, 12);
        Self::graduated_by(ds, rows, field, method, ramp, base, |vals| Ok(class_breaks(vals, k, method))).map(|mut r| {
            if let Renderer::Graduated { classes, .. } = &mut r {
                *classes = k;
            }
            r
        })
    }

    /// Builds a graduated-colours renderer on the class bounds typed by hand: the classes' upper
    /// bounds, sorted and each once, closed by the field's largest value when they stop short of
    /// it, so every feature has a class. At most 12 classes.
    pub fn graduated_manual(
        ds: &Dataset,
        rows: &[bool],
        field: &str,
        breaks: &[f64],
        ramp: Ramp,
        base: Symbol,
    ) -> Result<Renderer, String> {
        Self::graduated_by(ds, rows, field, ClassMethod::Manual, ramp, base, |vals| {
            let mut b: Vec<f64> = breaks.iter().copied().filter(|v| v.is_finite()).collect();
            if b.is_empty() {
                return Err("manual breaks need at least one number".into());
            }
            b.sort_by(|a, b| a.total_cmp(b));
            b.dedup();
            if let Some(&hi) = vals.last()
                && b.last().is_some_and(|&l| l < hi)
            {
                b.push(hi);
            }
            if b.len() > 12 {
                return Err(format!("{} classes; Graduated Colors takes at most 12", b.len()));
            }
            Ok(b)
        })
    }

    /// A graduated renderer on `field`, with the bounds `breaks` works out from the field's sorted
    /// values.
    fn graduated_by(
        ds: &Dataset,
        rows: &[bool],
        field: &str,
        method: ClassMethod,
        ramp: Ramp,
        base: Symbol,
        breaks: impl FnOnce(&[f64]) -> Result<Vec<f64>, String>,
    ) -> Result<Renderer, String> {
        let fi = ds.field_index(field).ok_or_else(|| format!("no field named '{field}'"))?;
        if !ds.fields.get(fi).is_some_and(|f| f.ty.is_numeric()) {
            return Err(format!("'{field}' is not a numeric field"));
        }
        let mut vals: Vec<f64> = ds.kept_features(rows).filter_map(|f| f.attrs.get(fi).and_then(|v| v.as_f64())).collect();
        if vals.is_empty() {
            return Err(format!("'{field}' has no numeric values"));
        }
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let breaks = breaks(&vals)?;
        let n = breaks.len();
        let symbols = (0..n).map(|i| base.with_ramp_fill(ramp.pick(i, n), ds.geometry_type)).collect();
        let min = vals.first().copied().unwrap_or(0.0);
        Ok(Renderer::Graduated {
            field: ds.fields.get(fi).map(|f| f.name.clone()).unwrap_or_default(),
            breaks,
            symbols,
            ramp,
            method,
            min,
            classes: n,
            class_labels: BTreeMap::new(),
            class_strokes: BTreeMap::new(),
            class_fills: Default::default(),
        })
    }

    /// A Graduated Colors renderer's classes as asked, which can be more than it has (see
    /// `classes`); 0 for other renderers.
    pub fn classes_asked(&self) -> usize {
        match self {
            Renderer::Graduated { classes, breaks, .. } => {
                if *classes > 0 {
                    *classes
                } else {
                    breaks.len()
                }
            }
            _ => 0,
        }
    }

    /// Builds a proportional-symbols renderer on a numeric `field`, sized over the rows `rows` keeps
    /// (a row past its end counts, so `&[]` keeps them all).
    pub fn proportional(ds: &Dataset, rows: &[bool], field: &str, base: Symbol) -> Result<Renderer, String> {
        let fi = ds.field_index(field).ok_or_else(|| format!("no field named '{field}'"))?;
        if !ds.fields.get(fi).is_some_and(|f| f.ty.is_numeric()) {
            return Err(format!("'{field}' is not a numeric field"));
        }
        let (min, max) = ds.numeric_range(fi, rows).ok_or_else(|| format!("'{field}' has no numeric values"))?;
        let mut base = base;
        if base.size == 0.0 {
            base.size = 6.0;
        }
        Ok(Renderer::Proportional {
            field: ds.fields.get(fi).map(|f| f.name.clone()).unwrap_or_default(),
            base,
            min_size: 4.0,
            max_size: 28.0,
            min,
            max,
        })
    }

    /// The class a feature row falls in: a Unique Values class's place in `classes` (its value's
    /// first class) or `<all other values>`, a Graduated Colors class's index; `None` for the other
    /// renderers, and for a Graduated Colors row without a number.
    fn class_of(&self, ds: &Dataset, row: usize) -> Option<Slot> {
        self.class_in(ds, row, &self.classer(ds, None))
    }

    /// [`Renderer::class_of`] with its class expression compiled (`c`, from [`Renderer::classer`]).
    fn class_in(&self, ds: &Dataset, row: usize, c: &Classer) -> Option<Slot> {
        match self {
            Renderer::Unique { expression: Some(_), classes, .. } => {
                let Some(e) = &c.expr else { return Some(Slot::Other) };
                let printed;
                let v = match rustgis_data::query::value(e, ds, row, &c.env) {
                    rustgis_data::Value::Null => return Some(Slot::Other),
                    rustgis_data::Value::Text(t) => {
                        printed = t;
                        printed.as_str()
                    }
                    v => {
                        printed = v.to_string();
                        printed.as_str()
                    }
                };
                Some(classes.iter().position(|(k, _)| k == v).map_or(Slot::Other, Slot::Class))
            }
            Renderer::Unique { field, classes, .. } => {
                let Some(fi) = ds.field_index(field) else { return Some(Slot::Other) };
                // Text compares as it is; other values as they print.
                let printed;
                let v = match ds.value(row, fi) {
                    rustgis_data::Value::Text(t) => t.as_str(),
                    v => {
                        printed = v.to_string();
                        printed.as_str()
                    }
                };
                Some(classes.iter().position(|(k, _)| k == v).map_or(Slot::Other, Slot::Class))
            }
            Renderer::Graduated { field, breaks, .. } => {
                let v = ds.field_index(field).and_then(|fi| ds.value(row, fi).as_f64())?;
                Some(Slot::Class(breaks.iter().position(|b| v <= *b).unwrap_or(breaks.len().saturating_sub(1))))
            }
            Renderer::Single(_) | Renderer::Proportional { .. } => None,
        }
    }

    /// The stacked strokes a line feature row's class has of its own (`class_strokes`), or `None`
    /// when its class has none (the layer's `line_strokes` apply).
    pub fn strokes_for(&self, ds: &Dataset, row: usize) -> Option<&[LineStroke]> {
        self.strokes_in(self.class_of(ds, row))
    }

    /// The symbol a feature row draws with ([`Renderer::symbol_for`]) and its class's own stacked
    /// strokes ([`Renderer::strokes_for`]), the feature classed once for both.
    pub fn style_for(&self, ds: &Dataset, row: usize) -> (Symbol, Option<&[LineStroke]>) {
        let slot = self.class_of(ds, row);
        (self.symbol_in(ds, row, slot), self.strokes_in(slot))
    }

    fn strokes_in(&self, slot: Option<Slot>) -> Option<&[LineStroke]> {
        let strokes = match (self, slot?) {
            (Renderer::Unique { classes, class_strokes, .. }, Slot::Class(i)) => class_strokes.get(&classes.get(i)?.0)?,
            (Renderer::Unique { other_strokes, .. }, Slot::Other) => other_strokes,
            (Renderer::Graduated { class_strokes, .. }, Slot::Class(i)) => class_strokes.get(&i)?,
            _ => return None,
        };
        (!strokes.is_empty()).then_some(strokes.as_slice())
    }

    /// The stacked fills a polygon feature row's class has of its own (`class_fills`), or `None`
    /// when its class has none (the layer's `fill_layers` apply).
    pub fn fills_for(&self, ds: &Dataset, row: usize) -> Option<&[FillLayer]> {
        self.fills_in(self.class_of(ds, row))
    }

    /// The symbol, class strokes and class fills of a feature row ([`Renderer::style_for`],
    /// [`Renderer::fills_for`]), the feature classed once for all three.
    pub fn style_fills_for(&self, ds: &Dataset, row: usize) -> (Symbol, Option<&[LineStroke]>, Option<&[FillLayer]>) {
        let slot = self.class_of(ds, row);
        (self.symbol_in(ds, row, slot), self.strokes_in(slot), self.fills_in(slot))
    }

    /// The place in `classes` of the Unique Values class a feature row falls in (`c` from
    /// [`Renderer::classer`]); `None` for `<all other values>` and the other renderers.
    pub fn unique_class(&self, ds: &Dataset, row: usize, c: &Classer) -> Option<usize> {
        match (self, self.class_in(ds, row, c)) {
            (Renderer::Unique { .. }, Some(Slot::Class(i))) => Some(i),
            _ => None,
        }
    }

    /// [`Renderer::style_fills_for`] with its class expression compiled ([`Renderer::classer`]),
    /// for drawing many features.
    pub fn style_fills_with(&self, ds: &Dataset, row: usize, c: &Classer) -> (Symbol, Option<&[LineStroke]>, Option<&[FillLayer]>) {
        let slot = self.class_in(ds, row, c);
        (self.symbol_in(ds, row, slot), self.strokes_in(slot), self.fills_in(slot))
    }

    fn fills_in(&self, slot: Option<Slot>) -> Option<&[FillLayer]> {
        let fills = match (self, slot?) {
            (Renderer::Unique { classes, class_fills, .. }, Slot::Class(i)) => class_fills.get(&classes.get(i)?.0)?,
            (Renderer::Unique { other_fills, .. }, Slot::Other) => other_fills,
            (Renderer::Graduated { class_fills, .. }, Slot::Class(i)) => class_fills.get(&i)?,
            _ => return None,
        };
        (!fills.is_empty()).then_some(fills.as_slice())
    }

    /// Whether any class has stacked fills of its own.
    pub fn has_class_fills(&self) -> bool {
        match self {
            Renderer::Unique { class_fills, other_fills, .. } => !class_fills.is_empty() || !other_fills.is_empty(),
            Renderer::Graduated { class_fills, .. } => !class_fills.is_empty(),
            Renderer::Single(_) | Renderer::Proportional { .. } => false,
        }
    }

    /// Whether any class has stacked strokes of its own.
    pub fn has_class_strokes(&self) -> bool {
        match self {
            Renderer::Unique { class_strokes, other_strokes, .. } => !class_strokes.is_empty() || !other_strokes.is_empty(),
            Renderer::Graduated { class_strokes, .. } => !class_strokes.is_empty(),
            Renderer::Single(_) | Renderer::Proportional { .. } => false,
        }
    }

    /// One class's own stacked fills (empty: it takes the layer's), as [`Renderer::class_strokes`].
    pub fn class_fills(&self, class: &ClassRef) -> Result<&[FillLayer], String> {
        let none: &[FillLayer] = &[];
        match (self, class) {
            (Renderer::Unique { classes, class_fills, .. }, ClassRef::Value(v)) => {
                if !classes.iter().any(|(k, _)| k == v) {
                    return Err(format!("no class with the value '{v}'"));
                }
                Ok(class_fills.get(v).map_or(none, Vec::as_slice))
            }
            (Renderer::Unique { other_fills, .. }, ClassRef::Other) => Ok(other_fills),
            (Renderer::Graduated { symbols, class_fills, .. }, ClassRef::Index(i)) => {
                if *i >= symbols.len() {
                    return Err(no_class(*i, symbols.len()));
                }
                Ok(class_fills.get(i).map_or(none, Vec::as_slice))
            }
            _ => Err(class_mismatch(self)),
        }
    }

    /// One class's own stacked strokes (empty when it takes the layer's).
    pub fn class_strokes(&self, class: &ClassRef) -> Result<&[LineStroke], String> {
        let none: &[LineStroke] = &[];
        match (self, class) {
            (Renderer::Unique { classes, class_strokes, .. }, ClassRef::Value(v)) => {
                if !classes.iter().any(|(k, _)| k == v) {
                    return Err(format!("no class with the value '{v}'"));
                }
                Ok(class_strokes.get(v).map_or(none, Vec::as_slice))
            }
            (Renderer::Unique { other_strokes, .. }, ClassRef::Other) => Ok(other_strokes),
            (Renderer::Graduated { symbols, class_strokes, .. }, ClassRef::Index(i)) => {
                if *i >= symbols.len() {
                    return Err(no_class(*i, symbols.len()));
                }
                Ok(class_strokes.get(i).map_or(none, Vec::as_slice))
            }
            _ => Err(class_mismatch(self)),
        }
    }

    /// Sets one class's stacked fills (empty: the layer's again), as [`Renderer::set_class_strokes`]
    /// does strokes.
    pub fn set_class_fills(&mut self, class: &ClassRef, fills: Vec<FillLayer>) -> Result<(), String> {
        use crate::fills::MAX_FILLS;
        if fills.len() > MAX_FILLS {
            return Err(format!("at most {MAX_FILLS} fills"));
        }
        let fills: Vec<FillLayer> = fills.into_iter().map(FillLayer::clamped).collect();
        match (&mut *self, class) {
            (Renderer::Unique { classes, class_fills, .. }, ClassRef::Value(v)) => {
                if !classes.iter().any(|(k, _)| k == v) {
                    return Err(format!("no class with the value '{v}'"));
                }
                if fills.is_empty() {
                    class_fills.remove(v);
                } else {
                    class_fills.insert(v.clone(), fills);
                }
            }
            (Renderer::Unique { other_fills, .. }, ClassRef::Other) => *other_fills = fills,
            (Renderer::Graduated { symbols, class_fills, .. }, ClassRef::Index(i)) => {
                if *i >= symbols.len() {
                    return Err(no_class(*i, symbols.len()));
                }
                if fills.is_empty() {
                    class_fills.remove(i);
                } else {
                    class_fills.insert(*i, fills);
                }
            }
            _ => return Err(class_mismatch(self)),
        }
        Ok(())
    }

    /// Sets one class's own stacked strokes, at most [`MAX_STROKES`], each brought into range;
    /// none goes back to the layer's.
    pub fn set_class_strokes(&mut self, class: &ClassRef, strokes: Vec<LineStroke>) -> Result<(), String> {
        if strokes.len() > MAX_STROKES {
            return Err(format!("at most {MAX_STROKES} strokes"));
        }
        let strokes: Vec<LineStroke> = strokes.into_iter().map(LineStroke::clamped).collect();
        match (&mut *self, class) {
            (Renderer::Unique { classes, class_strokes, .. }, ClassRef::Value(v)) => {
                if !classes.iter().any(|(k, _)| k == v) {
                    return Err(format!("no class with the value '{v}'"));
                }
                if strokes.is_empty() {
                    class_strokes.remove(v);
                } else {
                    class_strokes.insert(v.clone(), strokes);
                }
            }
            (Renderer::Unique { other_strokes, .. }, ClassRef::Other) => *other_strokes = strokes,
            (Renderer::Graduated { symbols, class_strokes, .. }, ClassRef::Index(i)) => {
                if *i >= symbols.len() {
                    return Err(no_class(*i, symbols.len()));
                }
                if strokes.is_empty() {
                    class_strokes.remove(i);
                } else {
                    class_strokes.insert(*i, strokes);
                }
            }
            _ => return Err(class_mismatch(self)),
        }
        Ok(())
    }

    /// Symbol for a feature row.
    pub fn symbol_for(&self, ds: &Dataset, row: usize) -> Symbol {
        self.symbol_in(ds, row, self.class_of(ds, row))
    }

    /// The symbol for a feature row in class `slot` ([`Renderer::class_of`]).
    fn symbol_in(&self, ds: &Dataset, row: usize, slot: Option<Slot>) -> Symbol {
        match self {
            Renderer::Single(s) => *s,
            Renderer::Unique { classes, other, .. } => match slot {
                Some(Slot::Class(i)) => classes.get(i).map_or(*other, |c| c.1),
                _ => *other,
            },
            Renderer::Graduated { symbols, .. } => {
                let fallback = symbols.first().copied().unwrap_or(Symbol::default_for(ds.geometry_type, 0));
                let Some(Slot::Class(i)) = slot else {
                    return Symbol { fill: Rgba::new(0xC8, 0xC8, 0xC8, 140), ..fallback };
                };
                symbols.get(i).copied().unwrap_or(fallback)
            }
            Renderer::Proportional { field, base, min_size, max_size, min, max } => {
                let Some(v) = ds.field_index(field).and_then(|fi| ds.value(row, fi).as_f64()) else { return *base };
                let t = if max > min { ((v - min) / (max - min)).clamp(0.0, 1.0) } else { 0.5 };
                // Area-proportional (Flannery-free square root scaling).
                Symbol { size: min_size + (max_size - min_size) * t.sqrt() as f32, ..*base }
            }
        }
    }

    /// The legend for the features `rows` keeps (a row past its end counts, so `&[]` keeps them
    /// all): a Unique Values class only when some kept feature has it, and `<all other values>`
    /// only when some kept feature matches no class and its symbol is not fully transparent (#207).
    /// The classes may come from a style file or from before a definition query changed, so they
    /// can name values no kept feature has. The other renderers list their classes as they are.
    pub fn legend_for(&self, ds: &Dataset, rows: &[bool]) -> Vec<LegendEntry> {
        self.legend_at(ds, rows, None)
    }

    /// [`Renderer::legend_for`] for the map drawn at 1:`map_scale` (none: no map), which Unique
    /// Values by an expression reading `@map_scale` class by. One whose expression would take
    /// more than [`MAX_LEGEND_STEPS`] to class every kept feature lists all its classes.
    pub fn legend_at(&self, ds: &Dataset, rows: &[bool], map_scale: Option<f64>) -> Vec<LegendEntry> {
        let Renderer::Unique {
            field,
            classes,
            other,
            class_labels,
            other_label,
            expression,
            other_fills,
            other_strokes,
            class_strokes,
            ..
        } = self
        else {
            return self.legend();
        };
        // A fallback that draws nothing has nothing to stand for in a legend: its own symbol as
        // `Symbol::draws` has it (as a web style does), else its stacked strokes (on lines and
        // points) or stacked fills (on areas); a point of size 0 draws none of them.
        let polygon = ds.geometry_type == GeometryType::Polygon;
        let strokes = !polygon
            && other_strokes.iter().any(|s| s.color.unwrap_or(other.stroke).a() > 0 && s.width.unwrap_or(other.stroke_width) > 0.0);
        let fills = polygon && other_fills.iter().any(|f| f.draws(other.fill));
        let sized = ds.geometry_type != GeometryType::Point || other.size > 0.0;
        let visible = other.draws(ds.geometry_type) || (sized && (strokes || fills));
        if expression.is_some() && ds.features.len().saturating_mul(classes.len().max(1)) > MAX_LEGEND_STEPS {
            let mut all = self.legend();
            all.retain(|e| visible || !e.fallback);
            return all;
        }
        // A value matches its first class, as in `symbol_for`.
        let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (i, (k, _)) in classes.iter().enumerate() {
            index.entry(k.as_str()).or_insert(i);
        }
        let fi = ds.field_index(field);
        // By expression: each feature classed as it is drawn.
        let classer = expression.as_ref().map(|_| self.classer(ds, map_scale));
        let mut used = vec![false; classes.len()];
        let mut other_used = false;
        // Distinct classes still unseen: once every class and "other" are in, the rest can't add any.
        let mut unseen = index.len() + 1;
        // One buffer for every value's text, rather than a string per feature.
        let mut text = String::new();
        for r in (0..ds.features.len()).filter(|r| rows.get(*r).copied().unwrap_or(true)) {
            let class = if let Some(c) = &classer {
                match self.class_in(ds, r, c) {
                    Some(Slot::Class(i)) => classes.get(i).and_then(|(k, _)| index.get(k.as_str()).copied()),
                    _ => None,
                }
            } else {
                fi.and_then(|fi| {
                    use std::fmt::Write;
                    text.clear();
                    write!(text, "{}", ds.value(r, fi)).ok()?;
                    index.get(text.as_str()).copied()
                })
            };
            let seen = match class.and_then(|i| used.get_mut(i)) {
                Some(u) => std::mem::replace(u, true),
                None => std::mem::replace(&mut other_used, true),
            };
            if !seen {
                unseen -= 1;
                if unseen == 0 {
                    break;
                }
            }
        }
        let mut v: Vec<LegendEntry> = self
            .class_order()
            .into_iter()
            .filter(|i| used.get(*i).copied().unwrap_or(false))
            .filter_map(|i| classes.get(i))
            .map(|(k, s)| LegendEntry::class(unique_label(class_labels, k), *s, class_strokes.get(k)))
            .collect();
        if other_used && visible {
            v.push(LegendEntry {
                label: other_label.clone().unwrap_or_else(|| OTHER_VALUES.into()),
                symbol: *other,
                strokes: other_strokes.clone(),
                fallback: true,
            });
        }
        v
    }

    /// The order `legend()` lists the Unique Values classes in: their indices in `classes`, by
    /// value when [`Renderer::classes_by_number`] (9 before 10, a class renamed by hand in its
    /// value's place), else by label without regard to case. The sort is stable, so a repeated
    /// value keeps its first class first. Empty for the other renderers, whose legend follows
    /// their classes.
    pub fn class_order(&self) -> Vec<usize> {
        let Renderer::Unique { classes, class_labels, .. } = self else { return Vec::new() };
        let mut order: Vec<usize> = (0..classes.len()).collect();
        if self.classes_by_number() {
            // (Each value read as a number once; the sort is stable.)
            order.sort_by_cached_key(|i| rustgis_data::value::ClassText::new(classes.get(*i).map(|(k, _)| k.clone()).unwrap_or_default()));
        } else {
            order.sort_by_cached_key(|i| classes.get(*i).map(|(k, _)| unique_label(class_labels, k).to_lowercase()));
        }
        order
    }

    /// Whether Unique Values' classes are ordered by number: when their values read as numbers
    /// ([`rustgis_data::value::numeric_classes`]), as a numeric field's do. False for the other
    /// renderers.
    pub fn classes_by_number(&self) -> bool {
        let Renderer::Unique { classes, .. } = self else { return false };
        rustgis_data::value::numeric_classes(classes.iter().map(|(k, _)| k.as_str()))
    }

    /// Every row of its legend, whatever the features: the classes, then a Unique Values'
    /// `<all other values>`; none for a single symbol.
    pub fn legend(&self) -> Vec<LegendEntry> {
        match self {
            Renderer::Single(_) => Vec::new(),
            Renderer::Unique { classes, other, class_labels, other_label, class_strokes, other_strokes, .. } => {
                // Keep symbols attached to their classes in `class_order` and the fallback at the end.
                let mut v: Vec<LegendEntry> = self
                    .class_order()
                    .into_iter()
                    .filter_map(|i| classes.get(i))
                    .map(|(k, s)| LegendEntry::class(unique_label(class_labels, k), *s, class_strokes.get(k)))
                    .collect();
                v.push(LegendEntry {
                    label: other_label.clone().unwrap_or_else(|| OTHER_VALUES.into()),
                    symbol: *other,
                    strokes: other_strokes.clone(),
                    fallback: true,
                });
                v
            }
            Renderer::Graduated { breaks, symbols, min, class_labels, class_strokes, .. } => {
                let mut lo = *min;
                breaks
                    .iter()
                    .zip(symbols)
                    .enumerate()
                    .map(|(i, (b, s))| {
                        let label = class_labels.get(&i).cloned().unwrap_or_else(|| range_label(lo, *b));
                        lo = *b;
                        LegendEntry::class(label, *s, class_strokes.get(&i))
                    })
                    .collect()
            }
            Renderer::Proportional { base, min_size, max_size, min, max, .. } => vec![
                LegendEntry { label: fmt_num(*min), symbol: Symbol { size: *min_size, ..*base }, strokes: Vec::new(), fallback: false },
                LegendEntry {
                    label: fmt_num((min + max) / 2.0),
                    symbol: Symbol { size: min_size + (max_size - min_size) * 0.707, ..*base },
                    strokes: Vec::new(),
                    fallback: false,
                },
                LegendEntry { label: fmt_num(*max), symbol: Symbol { size: *max_size, ..*base }, strokes: Vec::new(), fallback: false },
            ],
        }
    }

    /// The classes whose colors a ramp gives, in the order the legend lists them, and how many
    /// more the ramp can't recolor: a class whose own stack hides its symbol's colors. The stack
    /// that is drawn depends on the geometry (a polygon's fills; a line's strokes or a point's
    /// marks; see the drawing in `rustgis-render`, which doesn't draw a polygon's stacked strokes
    /// or a line's stacked fills), an empty stack is no stack, and a layer of a stack without a
    /// color of its own takes the symbol's (`FillLayer::color`, `LineStroke::color`), so the
    /// class still shows its ramp color through it: a road's black casing over a core that follows
    /// the class. Only a stack whose every layer sets its own color hides the symbol. Only the
    /// first entry of a Unique Values value is drawn, so a repeated value counts once. They are
    /// indices into `classes` (Unique Values) or `symbols` (Graduated Colors). `None` for the
    /// other renderers.
    fn ramp_slots(&self, g: GeometryType) -> Option<(Vec<usize>, usize, usize)> {
        // Whether a class's own stacks hide its symbol's colors, for the geometry the layer draws.
        let replaced = |fills: Option<&Vec<FillLayer>>, strokes: Option<&Vec<LineStroke>>| {
            stack_hides_symbol(g, fills.map_or(&[][..], Vec::as_slice), strokes.map_or(&[][..], Vec::as_slice))
        };
        // A class's own stack, of the kind the layer draws, replaces the layer's whenever it isn't empty.
        let bare = |fills: Option<&Vec<FillLayer>>, strokes: Option<&Vec<LineStroke>>| {
            if g == GeometryType::Polygon { fills.is_none_or(|f| f.is_empty()) } else { strokes.is_none_or(|s| s.is_empty()) }
        };
        let (mut slots, mut kept, mut on_layer) = (Vec::new(), 0, 0);
        match self {
            Renderer::Unique { classes, class_fills, class_strokes, .. } => {
                let mut seen = std::collections::HashSet::new();
                for i in self.class_order() {
                    let Some((value, _)) = classes.get(i) else { continue };
                    if !seen.insert(value.as_str()) {
                        continue;
                    }
                    if replaced(class_fills.get(value), class_strokes.get(value)) {
                        kept += 1;
                    } else {
                        slots.push(i);
                        on_layer += usize::from(bare(class_fills.get(value), class_strokes.get(value)));
                    }
                }
            }
            Renderer::Graduated { symbols, class_fills, class_strokes, .. } => {
                for i in 0..symbols.len() {
                    if replaced(class_fills.get(&i), class_strokes.get(&i)) {
                        kept += 1;
                    } else {
                        slots.push(i);
                        on_layer += usize::from(bare(class_fills.get(&i), class_strokes.get(&i)));
                    }
                }
            }
            _ => return None,
        }
        Some((slots, kept, on_layer))
    }

    fn class_symbol_mut(&mut self, i: usize) -> Option<&mut Symbol> {
        match self {
            Renderer::Unique { classes, .. } => classes.get_mut(i).map(|c| &mut c.1),
            Renderer::Graduated { symbols, .. } => symbols.get_mut(i),
            _ => None,
        }
    }

    /// Reverses the colours of the classes (#1355), in the order the legend lists them: the first
    /// class takes the last one's colour, and so on. Colours edited by hand travel with their
    /// class's old position, and a polygon fill keeps its own opacity. Applied twice it gives the
    /// original back. A class whose own stack hides its symbol (every layer of its fills, for a
    /// polygon, or strokes, for a line or a point, sets a color of its own), and a repeat of a Unique
    /// Values value (which no feature draws), keep theirs and take no part in the reversal. The
    /// `<all other values>` symbol is no ramp color and stays. Says how many classes changed places
    /// and how many were kept for their own stacks, or `None` for a renderer without classes
    /// coloured from a ramp.
    pub fn reverse_ramp(&mut self, g: GeometryType) -> Option<RampReversal> {
        let (slots, kept, on_layer_stack) = self.ramp_slots(g)?;
        let old: Vec<Symbol> = slots.iter().filter_map(|&i| self.class_symbol_mut(i).map(|s| *s)).collect();
        for (pos, &i) in slots.iter().enumerate() {
            let from = old.len().checked_sub(1 + pos).and_then(|k| old.get(k));
            if let (Some(sym), Some(from)) = (self.class_symbol_mut(i), from) {
                *sym = sym.with_colors_of(from, g);
            }
        }
        // One class, or none, has no other to change places with.
        let moved = slots.len() > 1;
        Some(RampReversal { reversed: if moved { slots.len() } else { 0 }, kept, on_layer_stack: if moved { on_layer_stack } else { 0 } })
    }

    /// Recolours every class from a ramp, preserving each polygon fill's opacity.
    pub fn apply_ramp(&mut self, ramp: Ramp, g: GeometryType) {
        match self {
            Renderer::Unique { classes, ramp: r, .. } => {
                let n = classes.len();
                for (k, c) in classes.iter_mut().enumerate() {
                    c.1 = c.1.with_ramp_fill(ramp.pick(k, n), g);
                }
                *r = ramp;
            }
            Renderer::Graduated { symbols, ramp: r, .. } => {
                let n = symbols.len();
                for (k, s) in symbols.iter_mut().enumerate() {
                    *s = s.with_ramp_fill(ramp.pick(k, n), g);
                }
                *r = ramp;
            }
            _ => {}
        }
    }

    /// Sets one class's colour, outline colour and legend label (#314). A line class takes the
    /// colour as its line colour; a polygon class gets a matching outline unless `outline` names
    /// one. Colours include their alpha unless `preserve_fill_alpha` requests the polygon's
    /// legacy translucency for RGB-only input. Returns the class's legend row as it now is.
    pub fn set_class_style(&mut self, class: &ClassRef, style: &ClassStyle, g: GeometryType) -> Result<LegendEntry, String> {
        let label = style.label.as_deref().map(normalize_label);
        let restyle = |s: &mut Symbol| {
            if let Some(c) = style.color {
                *s = s.with_fill(c, g);
                if g == GeometryType::Polygon && !style.preserve_fill_alpha {
                    s.fill = c;
                }
            }
            if let Some(c) = style.outline {
                s.stroke = c;
            }
        };
        // The class's row in `legend()`, which lists the classes and then `other`. Unique Values
        // rows are sorted by label, so that class's row is built here rather than looked up.
        let row = match (&mut *self, class) {
            (Renderer::Unique { classes, class_labels, class_strokes, .. }, ClassRef::Value(v)) => {
                let i = classes.iter().position(|(k, _)| k == v).ok_or_else(|| format!("no class with the value '{v}'"))?;
                if let Some((k, s)) = classes.get_mut(i) {
                    restyle(s);
                    match label {
                        // Empty, or the value itself: the class shows its value, with no label of its own.
                        Some(t) if t.is_empty() || t == *k => {
                            class_labels.remove(k.as_str());
                        }
                        Some(t) => {
                            class_labels.insert(k.clone(), t);
                        }
                        None => {}
                    }
                }
                return classes
                    .get(i)
                    .map(|(k, s)| LegendEntry::class(unique_label(class_labels, k), *s, class_strokes.get(k)))
                    .ok_or_else(|| "the class has no legend row".into());
            }
            (Renderer::Unique { classes, other, other_label, .. }, ClassRef::Other) => {
                restyle(other);
                if let Some(t) = label {
                    *other_label = (!t.is_empty() && t != OTHER_VALUES).then_some(t);
                }
                classes.len()
            }
            (Renderer::Graduated { symbols, class_labels, breaks, min, .. }, ClassRef::Index(i)) => {
                // The range the class shows when it has no label of its own.
                let lo = if *i == 0 { Some(*min) } else { breaks.get(*i - 1).copied() };
                let range = lo.zip(breaks.get(*i).copied()).map(|(lo, hi)| range_label(lo, hi));
                let n = symbols.len();
                let s = symbols.get_mut(*i).ok_or_else(|| format!("no class {i}: this renderer has {n} (0 to {})", n.saturating_sub(1)))?;
                restyle(s);
                match label {
                    Some(t) if t.is_empty() || Some(&t) == range.as_ref() => {
                        class_labels.remove(i);
                    }
                    Some(t) => {
                        class_labels.insert(*i, t);
                    }
                    None => {}
                }
                *i
            }
            (Renderer::Unique { .. }, ClassRef::Index(_)) => {
                return Err("Unique Values classes are named by their value, not by number".into());
            }
            (Renderer::Graduated { .. }, _) => return Err("Graduated Colors classes are numbered from 0, the lowest".into()),
            _ => return Err("only Unique Values and Graduated Colors have classes to style".into()),
        };
        self.legend().into_iter().nth(row).ok_or_else(|| "the class has no legend row".into())
    }

    /// Carries hand-made class edits over from `old` when the symbology is applied again (#314).
    /// On the same field (or expression), a class that is still there keeps its legend label (a Graduated Colors
    /// class keeps its edits only while the ranges are the same: they belong to a range). With the
    /// same colour scheme too, it keeps the colour and outline set by hand: those that aren't what
    /// the scheme gave it at its place in `old` (see [`carry_class`]). The other classes take the
    /// new scheme's colours at their new places, on the new base symbol. A different scheme
    /// recolours every class (picking a scheme is asking for its colours); a different field
    /// starts fresh.
    pub fn carry_edits_from(&mut self, old: &Renderer, g: GeometryType) {
        match (self, old) {
            (
                Renderer::Unique {
                    field,
                    classes,
                    other,
                    ramp,
                    class_labels,
                    other_label,
                    class_strokes,
                    other_strokes,
                    class_fills,
                    other_fills,
                    expression,
                },
                Renderer::Unique {
                    field: of,
                    classes: oc,
                    other: oo,
                    ramp: or,
                    class_labels: ol,
                    other_label: ool,
                    class_strokes: ost,
                    other_strokes: oos,
                    class_fills: ocf,
                    other_fills: oof,
                    expression: oe,
                },
            ) if field == of && expression == oe => {
                let same_scheme = ramp == or;
                let n = oc.len();
                // Strokes and fills are drawn by hand, as labels are: a class keeps them whatever
                // the scheme.
                other_strokes.clone_from(oos);
                other_fills.clone_from(oof);
                for (k, s) in classes.iter_mut() {
                    if let Some(t) = ol.get(k.as_str()) {
                        class_labels.insert(k.clone(), t.clone());
                    }
                    if let Some(st) = ost.get(k.as_str()) {
                        class_strokes.insert(k.clone(), st.clone());
                    }
                    if let Some(f) = ocf.get(k.as_str()) {
                        class_fills.insert(k.clone(), f.clone());
                    }
                    if same_scheme
                        && let Some(j) = oc.iter().position(|(ok, _)| ok == k)
                        && let Some((_, os)) = oc.get(j)
                    {
                        *s = carry_class(*s, os, or.pick(j, n), g);
                    }
                }
                other_label.clone_from(ool);
                if same_scheme {
                    *other = carry_class(*other, oo, OTHER_FILL, g);
                }
            }
            (
                Renderer::Graduated { field, breaks, symbols, ramp, min, class_labels, class_strokes, class_fills, .. },
                Renderer::Graduated {
                    field: of,
                    breaks: ob,
                    symbols: os,
                    ramp: or,
                    min: om,
                    class_labels: ol,
                    class_strokes: ost,
                    class_fills: ocf,
                    ..
                },
            ) if field == of => {
                // A Graduated Colors edit belongs to a range: it stays only while the ranges do.
                if breaks != ob || min != om {
                    return;
                }
                class_labels.clone_from(ol);
                class_strokes.clone_from(ost);
                class_fills.clone_from(ocf);
                if ramp == or && symbols.len() == os.len() {
                    let n = os.len();
                    for (i, (s, o)) in symbols.iter_mut().zip(os).enumerate() {
                        *s = carry_class(*s, o, or.pick(i, n), g);
                    }
                }
            }
            _ => {}
        }
    }

    /// Applies `f` to every symbol.
    pub fn map_symbols(&mut self, mut f: impl FnMut(&mut Symbol)) {
        match self {
            Renderer::Single(s) => f(s),
            Renderer::Unique { classes, other, .. } => {
                classes.iter_mut().for_each(|c| f(&mut c.1));
                f(other);
            }
            Renderer::Graduated { symbols, .. } => symbols.iter_mut().for_each(f),
            Renderer::Proportional { base, .. } => f(base),
        }
    }

    /// Whether any symbol is a character of a font ([`Marker::Char`]).
    pub fn has_char_markers(&self) -> bool {
        let char = |s: &Symbol| matches!(s.marker, Marker::Char(_));
        match self {
            Renderer::Single(s) | Renderer::Proportional { base: s, .. } => char(s),
            Renderer::Unique { classes, other, .. } => char(other) || classes.iter().any(|c| char(&c.1)),
            Renderer::Graduated { symbols, .. } => symbols.iter().any(char),
        }
    }
}

/// `new` with the hand-made parts of `old`, a class whose scheme colour was `scheme`: the fill
/// when it isn't that colour, and the outline when it isn't the one [`Symbol::with_fill`] derives
/// (a polygon's shaded fill, a line's own colour; for points, the new base symbol's outline).
/// Polygon hue edits compare without alpha because ramps keep polygon opacity. That opacity
/// carries over independently, including alpha-only edits; point and line colours compare whole.
/// Outlines compare whole.
fn carry_class(new: Symbol, old: &Symbol, scheme: Rgba, g: GeometryType) -> Symbol {
    let rgb = |c: Rgba| (c.r(), c.g(), c.b());
    let mut s = new;
    let edited = if g == GeometryType::Polygon { rgb(old.fill) != rgb(scheme) } else { old.fill != scheme };
    if edited {
        let c = if g == GeometryType::Polygon { Rgba::rgb(old.fill.r(), old.fill.g(), old.fill.b()) } else { old.fill };
        s = s.with_fill(c, g);
    }
    if g == GeometryType::Polygon {
        s.fill = s.fill.with_alpha(old.fill.a());
    }
    let derived = match g {
        GeometryType::Polygon => Rgba::rgb(old.fill.r(), old.fill.g(), old.fill.b()).shade(0.6),
        GeometryType::Polyline => old.fill,
        _ => new.stroke,
    };
    if old.stroke != derived {
        s.stroke = old.stroke;
    }
    s
}

/// A legend label as [`Renderer::set_class_style`] keeps it: one line (control characters such as
/// line breaks become spaces), trimmed, cut to [`MAX_LABEL`] characters and trimmed again (a cut
/// can end in a space). An editor compares its text this way to tell whether it changed.
pub fn normalize_label(t: &str) -> String {
    let t: String = t.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    t.trim().chars().take(MAX_LABEL).collect::<String>().trim_end().to_string()
}

/// The first values in a map's order, never more than `max` kept, each with how many features
/// with a geometry have it, and how many have a value past them. A value put out of the first
/// can't come back (any later one is past the last kept), so its features and every later one
/// past the last kept are counted as the rest.
struct FirstValues<K: Ord> {
    kept: BTreeMap<K, usize>,
    rest: usize,
    max: usize,
}

impl<K: Ord> FirstValues<K> {
    fn new(max: usize) -> Self {
        FirstValues { kept: BTreeMap::new(), rest: 0, max }
    }

    fn add(&mut self, v: K, drawn: usize) {
        if self.kept.len() >= self.max && self.kept.last_key_value().is_some_and(|(last, _)| v > *last) {
            self.rest += drawn;
            return;
        }
        *self.kept.entry(v).or_insert(0) += drawn;
        if self.kept.len() > self.max {
            self.rest += self.kept.pop_last().map_or(0, |(_, n)| n);
        }
    }
}

/// A Unique Values class's legend label: the one set by hand, or its value.
fn unique_label(class_labels: &BTreeMap<String, String>, value: &str) -> String {
    class_labels.get(value).cloned().unwrap_or_else(|| value.to_string())
}

/// A Graduated Colors class's legend label when it has none of its own: its range.
fn range_label(lo: f64, hi: f64) -> String {
    format!("{} – {}", fmt_num(lo), fmt_num(hi))
}

/// Compact number formatting for legends: 1.2M, 35K, 0.42.
pub fn fmt_num(v: f64) -> String {
    let a = v.abs();
    if (a / 1e5).round() >= 10_000.0 {
        format!("{:.1}B", v / 1e9)
    } else if (a / 1e3).round() >= 1_000.0 {
        format!("{:.1}M", v / 1e6)
    } else if a.round() >= 1e4 {
        format!("{:.0}K", v / 1e3)
    } else if (a * 100.0).round() >= 10_000.0 || v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

/// Upper class bounds for sorted values.
pub fn class_breaks(sorted: &[f64], k: usize, method: ClassMethod) -> Vec<f64> {
    let n = sorted.len();
    let (Some(&lo), Some(&hi)) = (sorted.first(), sorted.last()) else { return Vec::new() };
    if k <= 1 || hi <= lo {
        return vec![hi];
    }
    let mut b = match method {
        // Manual bounds are typed, not worked out: asked to work them out, it starts from equal intervals.
        ClassMethod::EqualInterval | ClassMethod::Manual => (1..=k).map(|i| lo + (hi - lo) * i as f64 / k as f64).collect::<Vec<_>>(),
        ClassMethod::Quantile => (1..=k).map(|i| sorted.get(((n * i) / k).saturating_sub(1).min(n - 1)).copied().unwrap_or(hi)).collect(),
        ClassMethod::NaturalBreaks => jenks(sorted, k),
        ClassMethod::GeometricInterval => {
            // Positive values: each bound is the one before times (hi / lo)^(1/k), worked out on
            // logarithms. Values from zero or below are taken as if the minimum sat at a hundredth
            // of the range, which makes the ratio 101^(1/k); each bound is then a share of the way
            // from lo to hi. Neither adds large values, so the bounds stay finite and in order.
            let range = hi - lo;
            (1..=k)
                .map(|i| {
                    let f = i as f64 / k as f64;
                    let v = if lo > 0.0 {
                        (lo.ln() + (hi.ln() - lo.ln()) * f).exp()
                    } else {
                        let t = (101f64.powf(f) - 1.0) / 100.0;
                        if range.is_finite() { lo + range * t } else { lo * (1.0 - t) + hi * t }
                    };
                    v.clamp(lo, hi)
                })
                .collect()
        }
        ClassMethod::StandardDeviation => {
            // On values scaled by the largest magnitude, so the squares can't overflow.
            let s = lo.abs().max(hi.abs());
            let mean_s = sorted.iter().map(|v| v / s).sum::<f64>() / n as f64;
            let mean = mean_s * s;
            let sd = s * (sorted.iter().map(|v| (v / s - mean_s).powi(2)).sum::<f64>() / n as f64).sqrt();
            // k − 1 inner bounds one deviation apart, centred on the mean. A bound with no value
            // above the one before it is dropped, so no class is empty.
            let inner = (1..k).map(|i| mean + sd * (i as f64 - k as f64 / 2.0)).filter(|&v| v > lo && v < hi);
            let mut b = Vec::new();
            let mut below = 0;
            for v in inner.chain(std::iter::once(hi)) {
                let upto = sorted.partition_point(|x| *x <= v);
                if upto > below {
                    b.push(v);
                    below = upto;
                }
            }
            b
        }
        ClassMethod::ArithmeticProgression => {
            // Widths of 1, 2… k units, which add up to the range: the i-th bound is i(i + 1)/2 units
            // above lo. Taken as a share of the way from lo to hi, so an infinite range can't overflow.
            let range = hi - lo;
            let units = k.saturating_mul(k.saturating_add(1)) as f64 / 2.0;
            (1..=k)
                .map(|i| {
                    let t = (i.saturating_mul(i.saturating_add(1)) as f64 / 2.0 / units).min(1.0);
                    let v = if range.is_finite() { lo + range * t } else { lo * (1.0 - t) + hi * t };
                    v.clamp(lo, hi)
                })
                .collect()
        }
        ClassMethod::HeadTailBreaks => head_tail(sorted, k),
    };
    // Only bounds that are the same value: any tolerance merges distinct values somewhere (tiny
    // ones under an absolute one, large ones under a relative one). Repeated bounds come from
    // repeated values (quantiles, Jenks), so they are equal exactly.
    b.dedup();
    if let Some(l) = b.last_mut() {
        *l = hi;
    }
    b
}

/// Head/tail breaks (Jiang, 2013): a bound at the mean, then at the mean of the head (the values
/// above it), and so on while the head holds under 40% of the values it was taken from, for at
/// most k classes. The last bound is the maximum.
fn head_tail(sorted: &[f64], k: usize) -> Vec<f64> {
    let mut b = Vec::new();
    let mut slice = sorted;
    while b.len() + 1 < k && slice.len() > 1 {
        // On values scaled by the largest magnitude (the slice is sorted), so neither a sum of large
        // values nor large values of opposite signs can overflow.
        let (Some(&first), Some(&last)) = (slice.first(), slice.last()) else { break };
        let s = first.abs().max(last.abs());
        let mean = if s > 0.0 { s * (slice.iter().map(|v| v / s).sum::<f64>() / slice.len() as f64) } else { 0.0 };
        let tail = slice.partition_point(|x| *x <= mean);
        // No value at or below the mean (or none above it): no split.
        if tail == 0 {
            break;
        }
        let Some(head) = slice.get(tail..).filter(|h| !h.is_empty()) else { break };
        b.push(mean);
        if head.len() as f64 >= 0.4 * slice.len() as f64 {
            break;
        }
        slice = head;
    }
    b.extend(sorted.last());
    b
}

/// Jenks natural breaks (Fisher's dynamic programming), on a sample of at most 1000 values.
fn jenks(sorted: &[f64], k: usize) -> Vec<f64> {
    let data: Vec<f64> = if sorted.len() > 1000 {
        (0..1000).filter_map(|i| sorted.get(i * sorted.len() / 1000)).copied().collect()
    } else {
        sorted.to_vec()
    };
    let n = data.len();
    let k = k.min(n);
    if k < 2 {
        return vec![data.last().copied().unwrap_or(0.0)];
    }
    // lower[i][j]: first index of class j for data[0..=i]; var[i][j]: within-class variance.
    let mut lower = vec![vec![0usize; k + 1]; n + 1];
    let mut var = vec![vec![f64::INFINITY; k + 1]; n + 1];
    for j in 1..=k {
        lower[1][j] = 1;
        var[1][j] = 0.0;
    }
    for l in 2..=n {
        let (mut s1, mut s2, mut w) = (0.0, 0.0, 0.0);
        let mut v = 0.0;
        for m in 1..=l {
            let i3 = l - m + 1;
            let val = data[i3 - 1];
            s2 += val * val;
            s1 += val;
            w += 1.0;
            v = s2 - (s1 * s1) / w;
            let i4 = i3 - 1;
            if i4 != 0 {
                for j in 2..=k {
                    if var[l][j] >= v + var[i4][j - 1] {
                        lower[l][j] = i3;
                        var[l][j] = v + var[i4][j - 1];
                    }
                }
            }
        }
        lower[l][1] = 1;
        var[l][1] = v;
    }
    let mut breaks = vec![0.0; k];
    breaks[k - 1] = data[n - 1];
    let mut kk = n;
    for j in (2..=k).rev() {
        let id = lower[kk][j].saturating_sub(2);
        breaks[j - 2] = data[id];
        kk = lower[kk][j].saturating_sub(1).max(1);
    }
    breaks
}

/// The label sizes `labels.style` accepts, in points (Labeling › Text Symbol).
pub const LABEL_SIZE: std::ops::RangeInclusive<f64> = 4.0..=72.0;

/// The most label classes a layer keeps besides its Default class.
pub const MAX_LABEL_CLASSES: usize = 64;

/// The name the Default class goes by until it is renamed.
pub const DEFAULT_LABEL_CLASS: &str = "Default";

/// The longest label class name kept (characters).
pub const MAX_LABEL_CLASS_NAME: usize = 100;

/// A label class name as it is kept: trimmed and cut to [`MAX_LABEL_CLASS_NAME`] characters (not
/// ending in the space it was cut at). Empty when there's no name.
pub fn label_class_name(name: &str) -> String {
    let name: String = name.trim().chars().take(MAX_LABEL_CLASS_NAME).collect();
    name.trim_end().to_string()
}

/// Label settings for a layer: on or off, and its label classes (#618). As in ArcGIS Pro, each
/// class labels every feature its SQL clause matches, so a feature two classes match gets a label
/// from each (#732); `classes` come first and `default` last, the order their labels are placed
/// in when they compete for room. Saved as before: the Default class's settings sit beside `enabled`
/// and the other classes are listed only when there are any, so a project without them keeps
/// its old form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "LabelingFile")]
pub struct Labeling {
    pub enabled: bool,
    #[serde(flatten)]
    pub default: LabelClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<LabelClass>,
}

/// A layer's labels as a file holds them, read into what the commands would leave (see `From`).
#[derive(Deserialize)]
struct LabelingFile {
    enabled: bool,
    #[serde(flatten)]
    default: LabelClass,
    #[serde(default)]
    classes: Vec<LabelClass>,
}

impl From<LabelingFile> for Labeling {
    /// A project file is hostile input, and every feature drawn is matched against the classes:
    /// names are kept as the commands keep them ([`label_class_name`]; Default's is "Default"
    /// when it has none), and at most [`MAX_LABEL_CLASSES`] classes, each named, by a name no
    /// class before it nor Default has (ASCII case aside). The others are dropped, since commands
    /// and annotation classes (`"CLASS" = 'name'`) tell classes apart by name.
    fn from(mut f: LabelingFile) -> Self {
        f.default.name = label_class_name(&f.default.name);
        if f.default.name.is_empty() {
            f.default.name = DEFAULT_LABEL_CLASS.into();
        }
        let mut classes: Vec<LabelClass> = Vec::new();
        for mut c in f.classes {
            if classes.len() >= MAX_LABEL_CLASSES {
                break;
            }
            c.name = label_class_name(&c.name);
            let same = |n: &str| n.eq_ignore_ascii_case(&c.name);
            if !c.name.is_empty() && !same(&f.default.name) && !classes.iter().any(|o| same(&o.name)) {
                classes.push(c);
            }
        }
        Labeling { enabled: f.enabled, default: f.default, classes }
    }
}

/// One class of a layer's labels: the features it labels, the field it labels them with, its text
/// symbol, placement and visibility range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LabelClass {
    #[serde(default = "default_class_name", skip_serializing_if = "is_default_class_name")]
    pub name: String,
    /// A hidden class labels none of its features (the other classes still label theirs).
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub visible: bool,
    /// The SQL `WHERE` clause choosing its features (the definition query's language); none, or
    /// blank, takes every feature. One that no longer reads (a field since removed) takes none.
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub where_: Option<String>,
    pub field: String,
    pub size: f32,
    pub color: Rgba,
    pub halo: bool,
    pub bold: bool,
    /// Labels draw only when zoomed in beyond 1:`min_scale`, if set.
    #[serde(default)]
    pub min_scale: Option<f64>,
    /// Labels draw only when zoomed out beyond 1:`max_scale`, if set.
    #[serde(default)]
    pub max_scale: Option<f64>,
    /// Labels draw only when the viewer is at least this far away (metres), if set.
    #[serde(default)]
    pub closest: Option<f64>,
    /// Labels draw only when the viewer is at most this far away (metres), if set.
    #[serde(default)]
    pub farthest: Option<f64>,
    /// Where a label sits relative to its feature.
    #[serde(default)]
    pub placement: LabelPlacement,
    /// A label taken by another tries nearby positions before it is left out (#1011):
    /// round a point ([`LabelPlacement::alternatives`]), or along a non-repeated line.
    /// Off by default: placed
    /// elsewhere, a label may take the place another has today.
    #[serde(default, deserialize_with = "de_flag", skip_serializing_if = "std::ops::Not::not")]
    pub other_positions: bool,
    /// Slanted (an italic face where the font has one).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    /// The installed font family the labels are drawn in, by name ("Liberation Sans Narrow"), as
    /// QGIS names a text format's; none is the label font. A machine without the family draws
    /// them in the label font too.
    #[serde(default, deserialize_with = "de_font", skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// The halo's colour, as QGIS's buffer has one; none picks one that stands out from the text
    /// (white behind dark text, dark behind light).
    #[serde(default, deserialize_with = "de_halo_color", skip_serializing_if = "Option::is_none")]
    pub halo_color: Option<Rgba>,
    /// How far the halo reaches beyond the letters (points, within [`HALO_WIDTH`]), as QGIS's
    /// buffer size; none is RustGIS's thin halo.
    #[serde(default, deserialize_with = "de_halo_width", skip_serializing_if = "Option::is_none")]
    pub halo_width: Option<f32>,
    /// Space added between letters (points, within a layout text's range), as QGIS's letter
    /// spacing; negative draws them tighter.
    #[serde(default, deserialize_with = "de_letter_spacing", skip_serializing_if = "is_zero")]
    pub spacing: f32,
    /// How far the label is moved from where its placement puts it (points, right and down, each
    /// within [`LABEL_OFFSET`]), as QGIS's offset from a point; a label along a line isn't moved.
    #[serde(default, deserialize_with = "de_label_offset", skip_serializing_if = "is_no_offset")]
    pub offset: [f32; 2],
    /// A shape drawn behind the text, as QGIS's label background; none draws the text alone.
    #[serde(default, deserialize_with = "de_background", skip_serializing_if = "Option::is_none")]
    pub background: Option<LabelBackground>,
    /// Breaks the text into lines wherever this occurs, left out (QGIS's wrap character, a
    /// character or a few: a space puts each word on a line of its own); none breaks none.
    #[serde(default, deserialize_with = "de_wrap_char", skip_serializing_if = "Option::is_none")]
    pub wrap_char: Option<String>,
    /// Breaks lines longer than this many characters between their words, as QGIS's automatic
    /// wrap length (at most [`MAX_WRAP_LENGTH`]); 0 breaks none.
    #[serde(default, deserialize_with = "de_wrap_length", skip_serializing_if = "is_zero_u32")]
    pub wrap_length: u32,
    /// How a label's lines line up: left, as labels always have, centred or right.
    #[serde(default, skip_serializing_if = "is_left")]
    pub line_align: crate::layout::TextAlign,
    /// Along a line, a label every this many points (within [`LABEL_REPEAT`]), spaced out from
    /// the one at its middle, as QGIS's repeat distance; none is the one at its middle.
    #[serde(default, deserialize_with = "de_repeat", skip_serializing_if = "Option::is_none")]
    pub repeat: Option<f32>,
    /// Along a line, how far the label sits from it (points, within [`LABEL_OFFSET`], to the
    /// text's nearest edge), as QGIS's distance from a line: above it as the text reads when
    /// positive, below when negative, on it at 0.
    #[serde(default, deserialize_with = "de_line_offset", skip_serializing_if = "is_zero")]
    pub line_offset: f32,
    /// Every label drawn, over the others where they crowd, rather than some left out.
    #[serde(default, deserialize_with = "de_flag", skip_serializing_if = "std::ops::Not::not")]
    pub show_all: bool,
    /// Callouts: a label crowded off its spot moves to a free one nearby, a line drawn from its
    /// feature to it at most this long (points, within [`CALLOUT_REACH`]), as QGIS's callouts;
    /// none keeps every label in its spot. Labels along lines aren't moved.
    #[serde(default, deserialize_with = "de_callout", skip_serializing_if = "Option::is_none")]
    pub callout: Option<f32>,
    /// Labels moved by hand (the Move Label tool, as QGIS's): each feature's, by its id, drawn
    /// where it was put, a callout's line back to the feature when the class has callouts. At
    /// most [`MAX_MOVED_LABELS`], kept in order of id (so a label's is found without a scan).
    #[serde(default, deserialize_with = "de_moved", skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<MovedLabel>,
    /// Its text, size, colour and angle per feature, each by an expression (none set: the field,
    /// size and colour above alone).
    #[serde(default, skip_serializing_if = "crate::label_by::LabelBy::is_empty", deserialize_with = "crate::label_by::de_label_by")]
    pub by: crate::label_by::LabelBy,
}

/// A label moved by hand: its feature's id, and where its text is centred, in the layer's
/// coordinates (longitude and latitude), so it stays put on the map as the view moves.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MovedLabel {
    pub id: u64,
    pub at: [f64; 2],
}

/// Most labels a class keeps moved by hand.
pub const MAX_MOVED_LABELS: usize = 10_000;

/// Whether `at` is a place a label can be moved to: a longitude within ±540° (a view's worth
/// past the antimeridian either way) and a latitude within ±90°.
pub fn is_label_place(at: [f64; 2]) -> bool {
    let [x, y] = at;
    x.is_finite() && y.is_finite() && x.abs() <= 540.0 && y.abs() <= 90.0
}

/// A project file's moved labels: those with a place ([`is_label_place`]), one a feature (the
/// last wins), at most [`MAX_MOVED_LABELS`], in order of id.
fn de_moved<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<MovedLabel>, D::Error> {
    /// Reads one entry at a time, retaining only the capped map of valid label positions.
    struct Moves;
    impl<'de> serde::de::Visitor<'de> for Moves {
        type Value = Vec<MovedLabel>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an array of moved labels")
        }

        /// Buffers only one entry and the capped, deduplicated set of valid positions.
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut by_id = std::collections::BTreeMap::new();
            while let Some(value) = seq.next_element::<serde_json::Value>()? {
                if let Ok(m) = serde_json::from_value::<MovedLabel>(value)
                    && is_label_place(m.at)
                    && (by_id.len() < MAX_MOVED_LABELS || by_id.contains_key(&m.id))
                {
                    by_id.insert(m.id, m.at);
                }
            }
            Ok(by_id.into_iter().map(|(id, at)| MovedLabel { id, at }).collect())
        }

        /// Skips a damaged object without allocating it or rejecting the surrounding class.
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?.is_some() {}
            Ok(Vec::new())
        }

        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }
    d.deserialize_any(Moves)
}

impl LabelClass {
    /// Where feature `id`'s label was moved to by hand, if it was.
    pub fn moved_to(&self, id: u64) -> Option<[f64; 2]> {
        self.moved.binary_search_by_key(&id, |m| m.id).ok().and_then(|i| self.moved.get(i)).map(|m| m.at)
    }

    /// Keeps feature `id`'s label at `at` (longitude, latitude), or where its class puts it
    /// again with none. False when `at` isn't a place ([`is_label_place`]), or the class already
    /// keeps the most.
    pub fn move_label(&mut self, id: u64, at: Option<[f64; 2]>) -> bool {
        let found = self.moved.binary_search_by_key(&id, |m| m.id);
        match (at, found) {
            (None, Ok(i)) => {
                self.moved.remove(i);
                true
            }
            (None, Err(_)) => true,
            (Some(at), _) if !is_label_place(at) => false,
            (Some(at), Ok(i)) => {
                if let Some(m) = self.moved.get_mut(i) {
                    m.at = at;
                }
                true
            }
            (Some(at), Err(i)) if self.moved.len() < MAX_MOVED_LABELS => {
                self.moved.insert(i, MovedLabel { id, at });
                true
            }
            (Some(_), Err(_)) => false,
        }
    }
}

/// How long a callout's line may be (points): from about a letter to a few centimetres of paper.
pub const CALLOUT_REACH: std::ops::RangeInclusive<f64> = 5.0..=500.0;
/// A callout's longest line when none is given (points).
pub const DEFAULT_CALLOUT: f32 = 40.0;

/// A callout's longest line as a class keeps it, within [`CALLOUT_REACH`]; none for no number.
pub fn callout_reach(v: f64) -> Option<f32> {
    v.is_finite().then(|| v.clamp(*CALLOUT_REACH.start(), *CALLOUT_REACH.end()) as f32)
}

/// Reads positive finite lengths within the supported range; damaged and disabled values stay off.
fn de_callout<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    // (0 turns callouts off, as the command has it; a negative isn't a length either.)
    Ok(lenient::<D, f64>(d)?.filter(|v| *v > 0.0).and_then(callout_reach))
}

/// The longest wrap character kept (characters).
pub const MAX_WRAP_CHAR: usize = 8;
/// The longest line length wrapping keeps to (characters).
pub const MAX_WRAP_LENGTH: u32 = 500;
/// How far apart repeated labels along a line may be (points): from about a word to a few metres
/// of paper.
pub const LABEL_REPEAT: std::ops::RangeInclusive<f64> = 10.0..=10_000.0;

/// A wrap character as a class keeps it: at most [`MAX_WRAP_CHAR`] characters, line breaks left
/// out, none when empty (a space is one: it is a character to break at).
pub fn wrap_char(c: &str) -> Option<String> {
    let c: String = c.chars().filter(|c| *c != '\n' && *c != '\r').take(MAX_WRAP_CHAR).collect();
    (!c.is_empty()).then_some(c)
}

fn de_wrap_char<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(lenient::<D, String>(d)?.and_then(|c| wrap_char(&c)))
}

fn de_wrap_length<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    Ok(lenient::<D, f64>(d)?.filter(|v| v.is_finite() && *v > 0.0).map_or(0, |v| (v.min(f64::from(MAX_WRAP_LENGTH))) as u32))
}

/// A repeat distance, within [`LABEL_REPEAT`]; one that isn't a positive number is none.
pub fn label_repeat(v: f64) -> Option<f32> {
    (v.is_finite() && v > 0.0).then(|| v.clamp(*LABEL_REPEAT.start(), *LABEL_REPEAT.end()) as f32)
}

fn de_repeat<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    Ok(lenient::<D, f64>(d)?.and_then(label_repeat))
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_left(a: &crate::layout::TextAlign) -> bool {
    *a == crate::layout::TextAlign::Left
}

/// `text` broken into lines as `wrap_char` and `wrap_length` say: at each wrap character (left
/// out), then each line longer than `wrap_length` characters between its words (a word longer
/// than that keeps a line of its own). Lines the text already has stay lines. A line that fits keeps
/// its spacing; a longer one is rebuilt from its words, so its extra spaces are dropped.
pub fn wrapped(text: &str, wrap_char: Option<&str>, wrap_length: u32) -> String {
    let broken = match wrap_char.filter(|c| !c.is_empty()) {
        Some(c) => text.replace(c, "\n"),
        None => text.to_string(),
    };
    if wrap_length == 0 {
        return broken;
    }
    let most = wrap_length as usize;
    let mut out: Vec<String> = Vec::new();
    for line in broken.split('\n') {
        // A line that fits stays as it is, as QGIS's word wrap keeps it.
        if line.chars().count() <= most {
            out.push(line.to_string());
            continue;
        }
        let mut current = String::new();
        for word in line.split_whitespace() {
            let (have, add) = (current.chars().count(), word.chars().count());
            if have > 0 && have + 1 + add > most {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        out.push(current);
    }
    out.join("\n")
}

/// How far a label's offset moves it each way (points).
pub const LABEL_OFFSET: std::ops::RangeInclusive<f64> = -200.0..=200.0;

/// A rectangle behind a label's text, as QGIS's rectangle background: the text's block grown by
/// its padding, filled and outlined, its corners rounded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LabelBackground {
    /// Its fill; none leaves it see-through.
    #[serde(deserialize_with = "de_halo_color")]
    pub fill: Option<Rgba>,
    /// Its outline's colour; none draws no outline.
    #[serde(deserialize_with = "de_halo_color")]
    pub stroke: Option<Rgba>,
    /// Its outline's width (points, within [`BACKGROUND_STROKE`]).
    #[serde(deserialize_with = "de_background_stroke")]
    pub stroke_width: f32,
    /// How far it reaches past the text's block, across and up and down (points, each within
    /// [`BACKGROUND_PADDING`]).
    #[serde(deserialize_with = "de_background_padding")]
    pub padding: [f32; 2],
    /// Its corners' radius (points, within [`BACKGROUND_PADDING`]); 0 is square.
    #[serde(deserialize_with = "de_background_radius")]
    pub radius: f32,
}

impl Default for LabelBackground {
    /// White, with a thin dark outline (SMV25's road numbers): what Background turns on.
    fn default() -> Self {
        LabelBackground {
            fill: Some(Rgba::rgb(255, 255, 255)),
            stroke: Some(Rgba::rgb(38, 38, 38)),
            stroke_width: 0.3,
            padding: [1.0, 0.5],
            radius: 0.0,
        }
    }
}

impl LabelBackground {
    /// Kept within its ranges (a command's or a crafted file's numbers).
    pub fn checked(self) -> LabelBackground {
        // (One past f32's range, infinite, is the most; none of no number.)
        let within = |v: f32, r: &std::ops::RangeInclusive<f64>| if v.is_nan() { 0.0 } else { v.clamp(*r.start() as f32, *r.end() as f32) };
        LabelBackground {
            stroke_width: within(self.stroke_width, &BACKGROUND_STROKE),
            padding: self.padding.map(|p| within(p, &BACKGROUND_PADDING)),
            radius: within(self.radius, &BACKGROUND_PADDING),
            ..self
        }
    }

    /// Grown `factor` times, as its label is by a reference scale.
    pub fn scaled(&self, factor: f32) -> LabelBackground {
        LabelBackground {
            stroke_width: self.stroke_width * factor,
            padding: self.padding.map(|p| p * factor),
            radius: self.radius * factor,
            ..self.clone()
        }
    }

    /// Whether it draws anything: a fill or an outline that shows.
    pub fn draws(&self) -> bool {
        self.fill.is_some_and(|f| f.a() > 0) || (self.stroke.is_some_and(|s| s.a() > 0) && self.stroke_width > 0.0)
    }
}

/// The widths a label background's outline takes (points).
pub const BACKGROUND_STROKE: std::ops::RangeInclusive<f64> = 0.0..=10.0;
/// The paddings and corner radii a label background takes (points).
pub const BACKGROUND_PADDING: std::ops::RangeInclusive<f64> = 0.0..=50.0;

/// A label's offset, each way within [`LABEL_OFFSET`]; one that isn't two numbers is none.
fn de_label_offset<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[f32; 2], D::Error> {
    let within = |v: f64| if v.is_finite() { v.clamp(*LABEL_OFFSET.start(), *LABEL_OFFSET.end()) as f32 } else { 0.0 };
    Ok(lenient::<D, [f64; 2]>(d)?.map_or([0.0; 2], |o| o.map(within)))
}

/// A label's distance from its line, within [`LABEL_OFFSET`]; one that isn't a number is none.
pub fn label_line_offset(v: f64) -> f32 {
    if v.is_finite() { v.clamp(*LABEL_OFFSET.start(), *LABEL_OFFSET.end()) as f32 } else { 0.0 }
}

fn de_line_offset<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(lenient::<D, f64>(d)?.map_or(0.0, label_line_offset))
}

fn is_no_offset(o: &[f32; 2]) -> bool {
    *o == [0.0; 2]
}

/// A switch that is on only when it says `true`.
fn de_flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(lenient::<D, bool>(d)? == Some(true))
}

/// A label's background, kept within its ranges; one that isn't a background is none.
fn de_background<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<LabelBackground>, D::Error> {
    Ok(lenient::<D, LabelBackground>(d)?.map(LabelBackground::checked))
}

fn de_background_stroke<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(lenient::<D, f64>(d)?
        .filter(|v| v.is_finite())
        .map_or(0.0, |v| v.clamp(*BACKGROUND_STROKE.start(), *BACKGROUND_STROKE.end()) as f32))
}

fn de_background_padding<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[f32; 2], D::Error> {
    let within = |v: f64| if v.is_finite() { v.clamp(*BACKGROUND_PADDING.start(), *BACKGROUND_PADDING.end()) as f32 } else { 0.0 };
    Ok(lenient::<D, [f64; 2]>(d)?.map_or([0.0; 2], |p| p.map(within)))
}

fn de_background_radius<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(lenient::<D, f64>(d)?
        .filter(|v| v.is_finite())
        .map_or(0.0, |v| v.clamp(*BACKGROUND_PADDING.start(), *BACKGROUND_PADDING.end()) as f32))
}

/// The widths a label's halo takes (points).
pub const HALO_WIDTH: std::ops::RangeInclusive<f64> = 0.0..=10.0;

/// A label's halo width as `labels.style` would leave it: within [`HALO_WIDTH`]; one that isn't a
/// number (a crafted file's) is none, the thin halo.
fn de_halo_width<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    Ok(lenient::<D, f64>(d)?.filter(|w| w.is_finite()).map(|w| w.clamp(*HALO_WIDTH.start(), *HALO_WIDTH.end()) as f32))
}

/// A label's halo colour; one that isn't a colour is none, the automatic one.
fn de_halo_color<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Rgba>, D::Error> {
    lenient::<D, Rgba>(d)
}

/// A label's letter spacing, within a layout text's range; one that isn't a number is none.
fn de_letter_spacing<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    use crate::layout::{MAX_LETTER_SPACING, MIN_LETTER_SPACING};
    Ok(lenient::<D, f64>(d)?.filter(|s| s.is_finite()).map_or(0.0, |s| s.clamp(MIN_LETTER_SPACING, MAX_LETTER_SPACING) as f32))
}

/// A value of type `T`, or none for anything else, so a damaged setting doesn't make the project
/// unreadable.
fn lenient<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Read<T> {
        Value(T),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Read<T>>::deserialize(d)? {
        Some(Read::Value(v)) => Some(v),
        Some(Read::Other(_)) | None => None,
    })
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// A halo that stands out from text of colour `text`: white behind dark text, dark behind light.
pub fn auto_halo(text: Rgba) -> Rgba {
    let lum = 0.299 * text.r() as f32 + 0.587 * text.g() as f32 + 0.114 * text.b() as f32;
    if lum < 140.0 { Rgba::new(255, 255, 255, 225) } else { Rgba::new(20, 22, 26, 210) }
}

/// The longest font family name kept (characters).
pub const MAX_FONT_NAME: usize = 128;

/// A label font's family name as it is kept: trimmed, at most [`MAX_FONT_NAME`] characters, and
/// none when blank.
pub fn font_name(name: &str) -> Option<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_FONT_NAME).collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// A label class's font as a project has it, kept as [`font_name`] keeps it: a name, or none for
/// anything else (a number, a list), so a damaged font doesn't make the project unreadable.
pub(crate) fn de_font<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Font {
        Name(String),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Font>::deserialize(d)? {
        Some(Font::Name(n)) => font_name(&n),
        Some(Font::Other(_)) | None => None,
    })
}

fn default_class_name() -> String {
    DEFAULT_LABEL_CLASS.into()
}

fn is_default_class_name(n: &str) -> bool {
    n == DEFAULT_LABEL_CLASS
}

fn yes() -> bool {
    true
}

fn is_yes(b: &bool) -> bool {
    *b
}

/// Where labels sit relative to their feature's label point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelPlacement {
    /// Above points, centred on lines and polygons.
    #[default]
    Auto,
    Center,
    Above,
    Below,
    Left,
    Right,
    AboveRight,
    AboveLeft,
    BelowRight,
    BelowLeft,
    /// Along a line, straight, turned to its direction across the text, at its middle; points
    /// and polygons are labelled as `Auto`.
    Parallel,
    /// Along a line, each character turned with it where it bends, at its middle; points and
    /// polygons are labelled as `Auto`.
    Curved,
}

impl LabelPlacement {
    /// Whether a line is labelled along it ([`Parallel`](Self::Parallel) or
    /// [`Curved`](Self::Curved)).
    pub fn along_line(self) -> bool {
        matches!(self, LabelPlacement::Parallel | LabelPlacement::Curved)
    }

    pub const ALL: [LabelPlacement; 12] = [
        LabelPlacement::Auto,
        LabelPlacement::Center,
        LabelPlacement::Above,
        LabelPlacement::Below,
        LabelPlacement::Left,
        LabelPlacement::Right,
        LabelPlacement::AboveRight,
        LabelPlacement::AboveLeft,
        LabelPlacement::BelowRight,
        LabelPlacement::BelowLeft,
        LabelPlacement::Parallel,
        LabelPlacement::Curved,
    ];
    pub fn code(self) -> &'static str {
        match self {
            LabelPlacement::Auto => "auto",
            LabelPlacement::Center => "center",
            LabelPlacement::Above => "above",
            LabelPlacement::Below => "below",
            LabelPlacement::Left => "left",
            LabelPlacement::Right => "right",
            LabelPlacement::AboveRight => "aboveRight",
            LabelPlacement::AboveLeft => "aboveLeft",
            LabelPlacement::BelowRight => "belowRight",
            LabelPlacement::BelowLeft => "belowLeft",
            LabelPlacement::Parallel => "parallel",
            LabelPlacement::Curved => "curved",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            LabelPlacement::Auto => "Automatic",
            LabelPlacement::Center => "Centered",
            LabelPlacement::Above => "Above",
            LabelPlacement::Below => "Below",
            LabelPlacement::Left => "Left",
            LabelPlacement::Right => "Right",
            LabelPlacement::AboveRight => "Above Right",
            LabelPlacement::AboveLeft => "Above Left",
            LabelPlacement::BelowRight => "Below Right",
            LabelPlacement::BelowLeft => "Below Left",
            LabelPlacement::Parallel => "Parallel",
            LabelPlacement::Curved => "Curved",
        }
    }
    pub fn from_code(s: &str) -> Option<LabelPlacement> {
        LabelPlacement::ALL.into_iter().find(|p| p.code().eq_ignore_ascii_case(s.trim()))
    }
    /// The label centre's offset from the label point, in points, for a label `w` wide and `h`
    /// tall next to a marker of size `marker` (0 for lines and polygons).
    pub fn offset(self, w: f32, h: f32, marker: f32, is_point: bool) -> (f32, f32) {
        let gap = marker / 2.0 + 2.0;
        let (dx, dy) = (w / 2.0 + gap, h / 2.0 + gap);
        match self {
            LabelPlacement::Auto | LabelPlacement::Parallel | LabelPlacement::Curved if is_point => (0.0, -(marker / 2.0 + h * 0.6)),
            LabelPlacement::Auto | LabelPlacement::Center | LabelPlacement::Parallel | LabelPlacement::Curved => (0.0, 0.0),
            LabelPlacement::Above => (0.0, -dy),
            LabelPlacement::Below => (0.0, dy),
            LabelPlacement::Left => (-dx, 0.0),
            LabelPlacement::Right => (dx, 0.0),
            LabelPlacement::AboveRight => (dx, -dy),
            LabelPlacement::AboveLeft => (-dx, -dy),
            LabelPlacement::BelowRight => (dx, dy),
            LabelPlacement::BelowLeft => (-dx, dy),
        }
    }

    /// The places round a point a label tries after this one, when [`LabelClass::other_positions`]
    /// is on: in QGIS's cartographic order (its `DEFAULT_PLACEMENT_ORDER`), corners first, this
    /// one left out. Line alternatives are generated along their geometry by the renderer.
    pub fn alternatives(self) -> impl Iterator<Item = LabelPlacement> {
        [
            LabelPlacement::AboveRight,
            LabelPlacement::AboveLeft,
            LabelPlacement::BelowRight,
            LabelPlacement::BelowLeft,
            LabelPlacement::Right,
            LabelPlacement::Left,
            LabelPlacement::Above,
            LabelPlacement::Below,
        ]
        .into_iter()
        .filter(move |p| *p != self)
    }
}

impl LabelClass {
    /// The Default class: on the first text field that looks like a name.
    pub fn default_for(ds: &Dataset) -> LabelClass {
        let field = ds
            .fields
            .iter()
            .find(|f| f.ty == FieldType::Text && f.name.to_ascii_lowercase().contains("name"))
            .or_else(|| ds.fields.iter().find(|f| f.ty == FieldType::Text))
            .or_else(|| ds.fields.first())
            .map(|f| f.name.clone())
            .unwrap_or_default();
        LabelClass {
            name: default_class_name(),
            visible: true,
            where_: None,
            field,
            size: 10.0,
            color: Rgba::rgb(0x2B, 0x2B, 0x2B),
            halo: true,
            bold: false,
            min_scale: None,
            max_scale: None,
            closest: None,
            farthest: None,
            placement: LabelPlacement::Auto,
            other_positions: false,
            italic: false,
            font: None,
            halo_color: None,
            halo_width: None,
            spacing: 0.0,
            offset: [0.0; 2],
            background: None,
            wrap_char: None,
            wrap_length: 0,
            line_align: crate::layout::TextAlign::Left,
            repeat: None,
            line_offset: 0.0,
            show_all: false,
            callout: None,
            moved: Vec::new(),
            by: Default::default(),
        }
    }

    /// `text` broken into lines as this class wraps it ([`wrapped`]).
    pub fn wrap(&self, text: &str) -> String {
        wrapped(text, self.wrap_char.as_deref(), self.wrap_length)
    }

    /// The halo's colour behind text of colour `text`: its own, else one that stands out.
    pub fn halo_color_for(&self, text: Rgba) -> Rgba {
        self.halo_color.unwrap_or_else(|| auto_halo(text))
    }

    /// Whether labels draw at a scale denominator and viewing distance (metres).
    pub fn in_range(&self, scale: f64, distance: f64) -> bool {
        self.min_scale.is_none_or(|m| scale <= m)
            && self.max_scale.is_none_or(|m| scale >= m)
            && self.closest.is_none_or(|d| distance >= d)
            && self.farthest.is_none_or(|d| distance <= d)
    }

    /// Its clause, read against `ds`: `Some(None)` takes every feature, `Some(Some(e))` those
    /// matching `e`, and `None` (a clause that doesn't read) takes none.
    pub fn query(&self, ds: &Dataset) -> Option<Option<rustgis_data::query::Expr>> {
        match self.where_.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
            None => Some(None),
            Some(w) => rustgis_data::query::parse_fields(w, ds).ok().map(Some),
        }
    }
}

/// A label on the map, as (layer id, feature id, label class in `Labeling::in_order`, repeat): a
/// feature has a label for each class labelling it (#732), and along a line each repeat is one of
/// its own (0 for the one at the line's middle, and for every label not repeated). The feature is
/// named by its full-width id, not its row, so a lock holds through edits that move rows (#1046)
/// and distinct 64-bit ids stay distinct on wasm32 (#1135).
pub type LabelKey = (u32, u64, usize, usize);

/// A layer's label classes read against its data, in `Labeling::in_order`: every class whose
/// clause takes a row labels it.
pub struct LabelMatcher<'a> {
    classes: Vec<(&'a LabelClass, Option<Option<rustgis_data::query::Expr>>)>,
    env: rustgis_data::query::Env,
}

impl<'a> LabelMatcher<'a> {
    /// `map_scale`: the scale (1:N) the labels are drawn at, which a clause may read
    /// (`@map_scale`); none: no map.
    pub fn new(labels: &'a Labeling, ds: &Dataset, map_scale: Option<f64>) -> Self {
        let env = rustgis_data::query::Env { map_scale: map_scale.filter(|s| s.is_finite() && *s > 0.0) };
        LabelMatcher { classes: labels.in_order().map(|c| (c, c.query(ds))).collect(), env }
    }

    /// The classes whose clause takes `row`, each with its place in `Labeling::in_order`.
    pub fn classes_of(&self, ds: &Dataset, row: usize) -> impl Iterator<Item = (usize, &'a LabelClass)> {
        self.classes.iter().enumerate().filter_map(move |(i, (c, q))| match q {
            Some(None) => Some((i, *c)),
            Some(Some(e)) if rustgis_data::query::matches(e, ds, row, &self.env) => Some((i, *c)),
            _ => None,
        })
    }
}

impl Labeling {
    /// The classes in the order a feature is matched: the listed ones, then Default. At most
    /// `MAX_LABEL_CLASSES` of them count (a project file is read with no more), also for a
    /// `Labeling` built in code, since every feature drawn is matched against them.
    pub fn in_order(&self) -> impl Iterator<Item = &LabelClass> {
        self.classes.iter().take(MAX_LABEL_CLASSES).chain(std::iter::once(&self.default))
    }

    /// The class named `name` (ASCII case aside), Default included.
    pub fn class(&self, name: &str) -> Option<&LabelClass> {
        self.in_order().find(|c| c.name.eq_ignore_ascii_case(name.trim()))
    }

    /// The class named `name` (ASCII case aside), Default included, to change.
    pub fn class_mut(&mut self, name: &str) -> Option<&mut LabelClass> {
        let name = name.trim();
        if self.default.name.eq_ignore_ascii_case(name) {
            return Some(&mut self.default);
        }
        self.classes.iter_mut().take(MAX_LABEL_CLASSES).find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Turns the labels off and starts the Default class again on `ds`'s first name-like field
    /// (`LabelClass::default_for`), keeping its name, clause and visibility: its field was
    /// deleted. (A service refresh keeps Default's text symbol instead, and clears its field.)
    pub fn restart_default(&mut self, ds: &Dataset) {
        self.enabled = false;
        let old = std::mem::replace(&mut self.default, LabelClass::default_for(ds));
        (self.default.name, self.default.visible, self.default.where_) = (old.name, old.visible, old.where_);
    }

    /// Points every class on a field `ds` doesn't have at the Default class's field, or the first
    /// name-like one when Default's is gone too (a field deleted, a style from another layer, a
    /// service's fields changed). The class is kept, with its clause and text symbol.
    pub fn repoint_missing_fields(&mut self, ds: &Dataset) {
        // Default's field, or (it is gone too, cleared by a refresh) the first name-like one, so
        // a class isn't left on no field.
        let fallback = match ds.field_index(&self.default.field) {
            Some(_) => self.default.field.clone(),
            None => LabelClass::default_for(ds).field,
        };
        // (A class whose text is an expression that still reads keeps its own, which it doesn't
        // label from.)
        let by_text = |c: &LabelClass| c.by.text.as_deref().is_some_and(|t| rustgis_data::query::parse_fields(t, ds).is_ok());
        for c in self.classes.iter_mut().filter(|c| ds.field_index(&c.field).is_none() && !by_text(c)) {
            c.field.clone_from(&fallback);
        }
    }

    /// Off, on the first text field that looks like a name, with only its Default class.
    pub fn default_for(ds: &Dataset) -> Labeling {
        Labeling { enabled: false, default: LabelClass::default_for(ds), classes: Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Field, Value};

    /// Labels saved before label classes (#618) read as the Default class alone, and are written
    /// back the same, so older versions still read a project without classes.
    #[test]
    fn labels_without_classes_keep_their_saved_form() {
        let old = serde_json::json!({
            "enabled": true, "field": "name", "size": 9.0, "color": [10, 20, 30, 255], "halo": false, "bold": true,
            "min_scale": 500000.0, "max_scale": null, "closest": null, "farthest": null, "placement": "AboveRight"
        });
        let l: Labeling = serde_json::from_value(old.clone()).unwrap();
        assert!(l.enabled && l.classes.is_empty());
        assert_eq!((l.default.name.as_str(), l.default.visible, &l.default.where_), (DEFAULT_LABEL_CLASS, true, &None));
        assert_eq!((l.default.field.as_str(), l.default.size, l.default.placement), ("name", 9.0, LabelPlacement::AboveRight));
        assert_eq!(serde_json::to_value(&l).unwrap(), old);
        // With classes, they are listed after the Default class's settings, and read back.
        let mut with = l.clone();
        with.classes.push(LabelClass { name: "Capitals".into(), where_: Some("capital = 1".into()), visible: false, ..l.default.clone() });
        let v = serde_json::to_value(&with).unwrap();
        assert_eq!(v["field"], "name");
        assert_eq!(v["classes"][0]["where"], "capital = 1");
        assert_eq!(serde_json::from_value::<Labeling>(v).unwrap(), with);
    }

    /// A label font is kept by its family name, trimmed and bounded (a project file is hostile
    /// input); a blank one is the label font, and isn't written.
    #[test]
    fn a_label_font_is_kept_by_name() {
        let read = |font: serde_json::Value| {
            serde_json::from_value::<Labeling>(serde_json::json!({
                "enabled": true, "field": "name", "size": 9.0, "color": [0, 0, 0, 255], "halo": false, "bold": false, "font": font
            }))
            .unwrap()
        };
        let l = read("  Liberation Sans Narrow \n".into());
        assert_eq!(l.default.font.as_deref(), Some("Liberation Sans Narrow"));
        assert_eq!(serde_json::to_value(&l).unwrap()["font"], "Liberation Sans Narrow");
        // Not a name (a number, a list, an object): no font, and the rest of the labels still read.
        for blank in [
            serde_json::json!("  "),
            serde_json::Value::Null,
            serde_json::json!(7),
            serde_json::json!(["Arial"]),
            serde_json::json!({"a": 1}),
        ] {
            let l = read(blank);
            assert_eq!(l.default.font, None);
            assert!(serde_json::to_value(&l).unwrap().get("font").is_none());
        }
        assert_eq!(read("x".repeat(10_000).into()).default.font.map(|f| f.chars().count()), Some(MAX_FONT_NAME));
    }

    /// A halo's colour and width and the letter spacing are kept; a label saved before them, or
    /// one without them, is written as it always was; a crafted value reads as none (or within its
    /// range) and the rest of the labels still read.
    #[test]
    fn a_labels_halo_and_letter_spacing_are_kept_and_old_labels_save_as_before() {
        let base = serde_json::json!({"enabled": true, "field": "name", "size": 9.0, "color": [0, 0, 0, 255], "halo": true, "bold": false});
        let read = |extra: serde_json::Value| {
            let mut v = base.clone();
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            serde_json::from_value::<Labeling>(v).unwrap()
        };
        let old = read(serde_json::json!({}));
        assert_eq!((old.default.halo_color, old.default.halo_width, old.default.spacing), (None, None, 0.0));
        let saved = serde_json::to_value(&old).unwrap();
        for k in ["halo_color", "halo_width", "spacing"] {
            assert!(saved.get(k).is_none(), "{k} written for a label without it");
        }
        let l = read(serde_json::json!({"halo_color": [255, 238, 0, 255], "halo_width": 1.42, "spacing": 0.5}));
        assert_eq!((l.default.halo_color, l.default.halo_width, l.default.spacing), (Some(Rgba::rgb(255, 238, 0)), Some(1.42), 0.5));
        assert_eq!(serde_json::from_value::<Labeling>(serde_json::to_value(&l).unwrap()).unwrap(), l);
        let junk = read(serde_json::json!({"halo_color": "pink", "halo_width": "wide", "spacing": [1]}));
        assert_eq!((junk.default.halo_color, junk.default.halo_width, junk.default.spacing), (None, None, 0.0));
        assert_eq!(junk.default.field, "name");
        let far = read(serde_json::json!({"halo_width": 1e30, "spacing": -1e30}));
        assert_eq!((far.default.halo_width, far.default.spacing), (Some(10.0), -5.0));
        assert_eq!(read(serde_json::json!({"halo_width": -3})).default.halo_width, Some(0.0));
        // The automatic colour is the one labels always had.
        assert_eq!(l.default.halo_color_for(Rgba::rgb(0, 0, 0)), Rgba::rgb(255, 238, 0));
        assert_eq!(old.default.halo_color_for(Rgba::rgb(0, 0, 0)), Rgba::new(255, 255, 255, 225));
        assert_eq!(old.default.halo_color_for(Rgba::rgb(255, 255, 255)), Rgba::new(20, 22, 26, 210));
    }

    /// Wrapping breaks a text at its wrap character and between words past its length; with the
    /// lines' alignment and a repeat along lines it is kept, a label without them is written as it
    /// always was, and a crafted value reads as none (or within its range).
    #[test]
    fn a_labels_wrapping_and_repeat_are_kept_and_old_labels_save_as_before() {
        // At each wrap character (left out), then between words past the length.
        assert_eq!(wrapped("Winterthur Altstadt", Some(" "), 0), "Winterthur\nAltstadt");
        assert_eq!(wrapped("Zürich/Winterthur", Some("/"), 0), "Zürich\nWinterthur");
        assert_eq!(wrapped("the quick brown fox jumps", None, 10), "the quick\nbrown fox\njumps");
        assert_eq!(wrapped("Kantonsspital Winterthur", None, 5), "Kantonsspital\nWinterthur", "a long word keeps its own line");
        assert_eq!(wrapped("Höhe über Meer", None, 9), "Höhe über\nMeer", "characters, not bytes");
        assert_eq!(wrapped("a\nb c d", None, 3), "a\nb c\nd", "lines it has stay lines");
        assert_eq!(wrapped(" Ober  Winterthur", None, 20), " Ober  Winterthur", "a line that fits stays as it is");
        assert_eq!(wrapped("unchanged text", None, 0), "unchanged text");
        let base = serde_json::json!({"enabled": true, "field": "name", "size": 9.0, "color": [0, 0, 0, 255], "halo": true, "bold": false});
        let read = |extra: serde_json::Value| {
            let mut v = base.clone();
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            serde_json::from_value::<Labeling>(v).unwrap()
        };
        let old = read(serde_json::json!({}));
        let c = &old.default;
        assert_eq!((c.wrap_char.as_deref(), c.wrap_length, c.line_align, c.repeat), (None, 0, crate::layout::TextAlign::Left, None));
        let saved = serde_json::to_value(&old).unwrap();
        for k in ["wrap_char", "wrap_length", "line_align", "repeat"] {
            assert!(saved.get(k).is_none(), "{k} written for a label without it");
        }
        let l = read(serde_json::json!({"wrap_char": " ", "wrap_length": 12, "line_align": "Center", "repeat": 85.0}));
        let c = &l.default;
        assert_eq!(
            (c.wrap_char.as_deref(), c.wrap_length, c.line_align, c.repeat),
            (Some(" "), 12, crate::layout::TextAlign::Center, Some(85.0))
        );
        assert_eq!(serde_json::from_value::<Labeling>(serde_json::to_value(&l).unwrap()).unwrap(), l);
        assert_eq!(c.wrap("Ober Winterthur"), "Ober\nWinterthur");
        // Crafted: none, or within range.
        let junk = read(serde_json::json!({"wrap_char": 7, "wrap_length": "long", "repeat": "often"}));
        let c = &junk.default;
        assert_eq!((c.wrap_char.as_deref(), c.wrap_length, c.repeat, c.field.as_str()), (None, 0, None, "name"));
        let far = read(serde_json::json!({"wrap_char": "\n\n".to_string() + &"x".repeat(50), "wrap_length": 1e30, "repeat": 1e30}));
        let c = &far.default;
        assert_eq!(
            (c.wrap_char.as_ref().map(|w| w.chars().count()), c.wrap_length, c.repeat),
            (Some(MAX_WRAP_CHAR), MAX_WRAP_LENGTH, Some(10_000.0))
        );
        assert_eq!((label_repeat(0.5), label_repeat(-1.0), label_repeat(f64::NAN)), (Some(10.0), None, None));
        assert_eq!(wrap_char(""), None);
        assert_eq!(wrap_char("\n"), None, "a line break isn't a character to break at");
    }

    /// An offset and a background are kept; a label without them is written as it always was; a
    /// crafted value reads as none (or within its range) and the rest of the labels still read.
    #[test]
    fn a_labels_offset_and_background_are_kept_and_old_labels_save_as_before() {
        let base = serde_json::json!({"enabled": true, "field": "name", "size": 9.0, "color": [0, 0, 0, 255], "halo": true, "bold": false});
        let read = |extra: serde_json::Value| {
            let mut v = base.clone();
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            serde_json::from_value::<Labeling>(v).unwrap()
        };
        let old = read(serde_json::json!({}));
        assert_eq!((old.default.offset, old.default.background.as_ref()), ([0.0; 2], None));
        let saved = serde_json::to_value(&old).unwrap();
        for k in ["offset", "background"] {
            assert!(saved.get(k).is_none(), "{k} written for a label without it");
        }
        let bg = serde_json::json!({"fill": [255, 255, 255, 255], "stroke": null, "stroke_width": 0.28, "padding": [1.22, 0.28], "radius": 0.71});
        let l = read(serde_json::json!({"offset": [5.67, -2.83], "background": bg}));
        assert_eq!(l.default.offset, [5.67, -2.83]);
        let want =
            LabelBackground { fill: Some(Rgba::rgb(255, 255, 255)), stroke: None, stroke_width: 0.28, padding: [1.22, 0.28], radius: 0.71 };
        assert_eq!(l.default.background.as_ref(), Some(&want));
        assert_eq!(serde_json::from_value::<Labeling>(serde_json::to_value(&l).unwrap()).unwrap(), l);
        // What a background leaves out is the default's.
        let part = read(serde_json::json!({"background": {"radius": 2}}));
        assert_eq!(part.default.background, Some(LabelBackground { radius: 2.0, ..LabelBackground::default() }));
        // Crafted values: none, or within their ranges.
        let junk = read(serde_json::json!({"offset": "far", "background": "box"}));
        assert_eq!((junk.default.offset, junk.default.background.as_ref(), junk.default.field.as_str()), ([0.0; 2], None, "name"));
        let far = read(
            serde_json::json!({"offset": [1e30, -1e30], "background": {"fill": "pink", "stroke_width": 99, "padding": [-1, 1e9], "radius": "round"}}),
        );
        assert_eq!(far.default.offset, [200.0, -200.0]);
        let b = far.default.background.unwrap();
        assert_eq!((b.fill, b.stroke_width, b.padding, b.radius), (None, 10.0, [0.0, 50.0], 0.0));
        assert!(read(serde_json::json!({"offset": [1]})).default.offset == [0.0; 2]);
        // A background shows with a fill or an outline that shows.
        assert!(LabelBackground::default().draws());
        assert!(!LabelBackground { fill: None, stroke: None, ..LabelBackground::default() }.draws());
        assert!(!LabelBackground { fill: None, stroke_width: 0.0, ..LabelBackground::default() }.draws());
        assert!(!LabelBackground { fill: Some(Rgba::new(0, 0, 0, 0)), stroke: None, ..LabelBackground::default() }.draws());
        // Grown with its label.
        let g = want.scaled(2.0);
        assert_eq!((g.stroke_width, g.padding, g.radius, g.fill), (0.56, [2.44, 0.56], 1.42, want.fill));
    }

    /// Going round the point is off for a label saved before it, and written only when on; a
    /// crafted value is off and the rest of the labels still read (#1011).
    #[test]
    fn other_positions_are_kept_and_old_labels_save_as_before() {
        let base = serde_json::json!({"enabled": true, "field": "name", "size": 9.0, "color": [0, 0, 0, 255], "halo": true, "bold": false});
        let read = |extra: serde_json::Value| {
            let mut v = base.clone();
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            serde_json::from_value::<Labeling>(v).unwrap()
        };
        let old = read(serde_json::json!({}));
        assert!(!old.default.other_positions);
        assert!(serde_json::to_value(&old).unwrap().get("other_positions").is_none());
        let on = read(serde_json::json!({"other_positions": true}));
        assert!(on.default.other_positions);
        assert_eq!(serde_json::from_value::<Labeling>(serde_json::to_value(&on).unwrap()).unwrap(), on);
        let junk = read(serde_json::json!({"other_positions": "yes"}));
        assert!(!junk.default.other_positions);
        assert_eq!(junk.default.field, "name");
    }

    /// A label saved before the distance from a line sits on it and saves as before; a distance
    /// is kept either way, held to its range, and one that isn't a number is none (#1187).
    #[test]
    fn a_line_offset_is_kept_and_old_labels_save_as_before() {
        let base = serde_json::json!({"enabled": true, "field": "name", "size": 9.0, "color": [0, 0, 0, 255], "halo": true, "bold": false});
        let read = |extra: serde_json::Value| {
            let mut v = base.clone();
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            serde_json::from_value::<Labeling>(v).unwrap()
        };
        let old = read(serde_json::json!({}));
        assert_eq!(old.default.line_offset, 0.0);
        assert!(serde_json::to_value(&old).unwrap().get("line_offset").is_none());
        for (given, kept) in [(4.5, 4.5), (-3.0, -3.0), (900.0, 200.0), (-900.0, -200.0)] {
            let l = read(serde_json::json!({"line_offset": given}));
            assert_eq!(l.default.line_offset, kept, "{given}");
            assert_eq!(serde_json::from_value::<Labeling>(serde_json::to_value(&l).unwrap()).unwrap(), l);
        }
        let junk = read(serde_json::json!({"line_offset": "above"}));
        assert_eq!(junk.default.line_offset, 0.0);
        assert_eq!(junk.default.field, "name");
    }

    /// The places a label tries after its own: QGIS's cartographic order, its own left out.
    #[test]
    fn label_alternatives_go_round_the_point() {
        use LabelPlacement::*;
        let all = [AboveRight, AboveLeft, BelowRight, BelowLeft, Right, Left, Above, Below];
        for p in [Auto, Center, Parallel, Curved] {
            assert_eq!(p.alternatives().collect::<Vec<_>>(), all, "{p:?}");
        }
        assert_eq!(BelowRight.alternatives().collect::<Vec<_>>(), [AboveRight, AboveLeft, BelowLeft, Right, Left, Above, Below]);
        for p in all {
            assert_eq!(p.alternatives().count(), 7);
            assert!(!p.alternatives().any(|a| a == p));
        }
    }

    /// The shapes are written as they always were; a character with its family, which a crafted
    /// file can't make long or many, or stop the rest from reading.
    #[test]
    fn character_markers_keep_their_font_and_shapes_their_saved_form() {
        for m in Marker::ALL {
            assert_eq!(serde_json::to_value(m).unwrap(), serde_json::json!(m.label()));
            assert_eq!(serde_json::from_value::<Marker>(serde_json::json!(m.label())).unwrap(), m);
        }
        let old = serde_json::json!({"fill": [1, 2, 3, 255], "stroke": [0, 0, 0, 255], "stroke_width": 1.0, "size": 7.0, "marker": "Star"});
        let s: Symbol = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(s).unwrap(), old);

        let g = Marker::Char(Glyph::new('@', " DKM_Symbole "));
        let v = serde_json::to_value(g).unwrap();
        assert_eq!(v, serde_json::json!({"Char": {"ch": "@", "font": "DKM_Symbole"}}));
        assert_eq!(serde_json::from_value::<Marker>(v).unwrap(), g);
        // The label font's character isn't written a family.
        assert_eq!(serde_json::to_value(Glyph::new('A', "")).unwrap(), serde_json::json!({"ch": "A"}));
        for junk in [serde_json::json!(7), serde_json::json!(["Arial"]), serde_json::json!({"a": 1}), serde_json::Value::Null] {
            let read: Marker = serde_json::from_value(serde_json::json!({"Char": {"ch": "x", "font": junk}})).unwrap();
            assert_eq!(read, Marker::Char(Glyph::new('x', "")));
        }
        let long: Marker = serde_json::from_value(serde_json::json!({"Char": {"ch": "x", "font": "y".repeat(10_000)}})).unwrap();
        let Marker::Char(g) = long else { panic!() };
        assert_eq!(g.font.as_str().chars().count(), MAX_FONT_NAME);
        // Names are kept once each: the same name is the same one.
        assert!(std::ptr::eq(FontName::new("Some Family").as_str(), FontName::new("Some Family ").as_str()));
    }

    /// A project file is read with at most `MAX_LABEL_CLASSES` classes, and only that many count
    /// in a `Labeling` built with more. A class on
    /// a field the data doesn't have is pointed at Default's, and kept.
    #[test]
    fn classes_are_bounded_and_follow_missing_fields() {
        let ds = Dataset::new("t", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        let mut l = Labeling::default_for(&ds);
        let class = |i: usize| LabelClass { name: format!("c{i}"), field: "gone".into(), ..LabelClass::default_for(&ds) };
        l.classes = (0..MAX_LABEL_CLASSES + 5).map(class).collect();
        // A project file listing more is read with the first ones.
        let read: Labeling = serde_json::from_value(serde_json::to_value(&l).unwrap()).unwrap();
        assert_eq!(read.classes.len(), MAX_LABEL_CLASSES);
        assert_eq!(read.classes.last().map(|c| c.name.clone()), Some(format!("c{}", MAX_LABEL_CLASSES - 1)));
        // And with one class to a name, as the commands keep them: none blank, nor named as one
        // before it or Default.
        let mut v = serde_json::to_value(Labeling::default_for(&ds)).unwrap();
        // A class saved without a name (its name is Default's, which is skipped) too.
        let named = |n: Option<&str>| {
            let mut c = serde_json::to_value(LabelClass::default_for(&ds)).unwrap();
            if let Some(n) = n {
                c["name"] = n.into();
            }
            c
        };
        let long = format!("{} tail", "b".repeat(99));
        let names = [Some(" A "), Some("a"), Some("  "), Some("default"), None, Some(long.as_str())];
        v["classes"] = names.into_iter().map(named).collect();
        let read: Labeling = serde_json::from_value(v.clone()).unwrap();
        let b = "b".repeat(99);
        assert_eq!(read.classes.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["A", b.as_str()]);
        // Default's name is kept the same way, and is Default's when blank.
        v["name"] = "   ".into();
        let read: Labeling = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(read.default.name, DEFAULT_LABEL_CLASS);
        v["name"] = " Rest ".into();
        assert_eq!(serde_json::from_value::<Labeling>(v).unwrap().default.name, "Rest");
        assert_eq!(l.in_order().count(), MAX_LABEL_CLASSES + 1, "the counted classes, then Default");
        assert_eq!(l.in_order().last().map(|c| c.name.as_str()), Some(DEFAULT_LABEL_CLASS));
        assert!(l.class(&format!("c{}", MAX_LABEL_CLASSES + 1)).is_none() && l.class_mut("c70").is_none());
        l.repoint_missing_fields(&ds);
        assert!(l.classes.iter().all(|c| c.field == "name"));
        // With Default's field gone too (a refresh clears it), they take the first name-like one.
        l.default.field.clear();
        l.classes[0].field = "gone".into();
        l.repoint_missing_fields(&ds);
        assert_eq!(l.classes[0].field, "name");
        // A class labelling by a text expression keeps its own field while that reads, and is
        // repointed like the rest once it doesn't.
        for (text, field) in [("upper(name)", "gone"), ("upper(gone)", "name")] {
            l.classes[0].field = "gone".into();
            l.classes[0].by.text = Some(text.into());
            l.repoint_missing_fields(&ds);
            assert_eq!(l.classes[0].field, field, "{text}");
        }
        l.classes[0].by.text = None;
        // Default on a field that's gone starts again on a name-like one, keeping its own settings.
        (l.enabled, l.default.name, l.default.visible, l.default.where_) = (true, "Rest".into(), false, Some("name = 'x'".into()));
        l.default.field = "gone".into();
        l.restart_default(&ds);
        assert!(!l.enabled);
        assert_eq!(
            (l.default.field.as_str(), l.default.name.as_str(), l.default.visible, l.default.where_.as_deref()),
            ("name", "Rest", false, Some("name = 'x'"))
        );
    }

    #[test]
    fn a_feature_takes_every_class_that_matches() {
        let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("pop", FieldType::Integer)]);
        for pop in [5, 50, 500] {
            ds.push(None, vec![Value::Int(pop)]);
        }
        let mut l = Labeling::default_for(&ds);
        let class = |name: &str, w: &str| LabelClass { name: name.into(), where_: Some(w.into()), ..LabelClass::default_for(&ds) };
        l.classes = vec![class("Big", "pop > 100"), class("Medium", "pop > 10"), class("Broken", "nope = 1")];
        let names = |l: &Labeling| -> Vec<Vec<(usize, String)>> {
            let m = LabelMatcher::new(l, &ds, None);
            (0..3).map(|r| m.classes_of(&ds, r).map(|(i, c)| (i, c.name.clone())).collect()).collect()
        };
        let (big, medium, default) = ((0, "Big".to_string()), (1, "Medium".to_string()), (3, "Default".to_string()));
        assert_eq!(
            names(&l),
            [vec![default.clone()], vec![medium.clone(), default.clone()], vec![big.clone(), medium.clone(), default]],
            "500 is Big, Medium and Default; a clause that doesn't read takes nothing"
        );
        // A Default class with a clause labels only what it matches.
        l.default.where_ = Some("pop = 50".into());
        assert_eq!(names(&l), [vec![], vec![medium.clone(), (3, "Default".to_string())], vec![big, medium]]);
        assert_eq!(l.class("medium").map(|c| c.name.as_str()), Some("Medium"), "names match ASCII case aside");
    }

    #[test]
    fn fmt_num_switches_form_when_it_rounds_up() {
        assert_eq!(fmt_num(0.42), "0.42");
        assert_eq!(fmt_num(42.0), "42");
        assert_eq!(fmt_num(99.994), "99.99");
        assert_eq!(fmt_num(99.995), "100");
        assert_eq!(fmt_num(99.996), "100");
        assert_eq!(fmt_num(9_999.4), "9999");
        assert_eq!(fmt_num(9_999.5), "10K");
        assert_eq!(fmt_num(9_999.6), "10K");
        assert_eq!(fmt_num(35_000.0), "35K");
        assert_eq!(fmt_num(999_400.0), "999K");
        assert_eq!(fmt_num(999_500.0), "1.0M");
        assert_eq!(fmt_num(999_600.0), "1.0M");
        assert_eq!(fmt_num(1_000_000.0), "1.0M");
        assert_eq!(fmt_num(1_234_567.0), "1.2M");
        assert_eq!(fmt_num(999_940_000.0), "999.9M");
        assert_eq!(fmt_num(999_950_000.0), "1.0B");
        assert_eq!(fmt_num(999_960_000.0), "1.0B");
        assert_eq!(fmt_num(-999_600.0), "-1.0M");
    }
    #[test]
    fn the_legend_lists_only_classes_kept_features_have() {
        let mut d = Dataset::new("t", GeometryType::Polygon, vec![Field::new("g", FieldType::Text)]);
        for g in ["a", "b", "a", "z"] {
            d.push(None, vec![Value::Text(g.into())]);
        }
        let s = |c| Symbol::default_for(GeometryType::Polygon, c);
        // "c" is no feature's value; "z" matches no class.
        let r = Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".into(), s(0)), ("b".into(), s(1)), ("c".into(), s(2))],
            other: s(3),
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let labels = |rows: &[bool]| r.legend_for(&d, rows).into_iter().map(|e| e.label).collect::<Vec<_>>();
        assert_eq!(labels(&[]), ["a", "b", "<all other values>"]);
        assert_eq!(labels(&[true, false, true, false]), ["a"], "<all other values> only when used");
        assert_eq!(labels(&[false, true]), ["a", "b", "<all other values>"], "rows past the end (a, z) count as kept");
        assert_eq!(labels(&[false, false, false, true]), ["<all other values>"]);
        assert!(labels(&[false; 4]).is_empty());
        // The full legend is unchanged.
        assert_eq!(r.legend().len(), 4);
        // A class listed twice: the first matches, and the scan still stops once all are seen.
        let twice = Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".into(), s(0)), ("a".into(), s(1)), ("b".into(), s(2))],
            other: s(3),
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let shown = twice.legend_for(&d, &[]);
        assert_eq!(shown.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["a", "b", "<all other values>"]);
        assert_eq!(shown[0].symbol, s(0));
        // A field the data no longer has: every feature is "other".
        let gone = Renderer::Unique {
            field: "nope".into(),
            classes: vec![("a".into(), s(0))],
            other: s(3),
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        assert_eq!(gone.legend_for(&d, &[]).len(), 1);
        // The fallback row is marked as such, whatever a class is called.
        let named = Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".into(), s(0)), (OTHER_VALUES.into(), s(1))],
            other: s(3),
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let mut d2 = d.clone();
        d2.push(None, vec![Value::Text(OTHER_VALUES.into())]);
        let rows = named.legend_for(&d2, &[]);
        assert_eq!(
            rows.iter().map(|e| (e.label.as_str(), e.fallback)).collect::<Vec<_>>(),
            [(OTHER_VALUES, false), ("a", false), (OTHER_VALUES, true)]
        );
        // Other renderers list their classes as they are.
        assert_eq!(Renderer::Single(s(0)).legend_for(&d, &[false; 4]), Vec::new());
    }

    #[test]
    fn a_transparent_fallback_is_left_out_of_the_legend() {
        let mut d = Dataset::new("t", GeometryType::Polygon, vec![Field::new("g", FieldType::Text)]);
        for g in ["a", "z"] {
            d.push(None, vec![Value::Text(g.into())]);
        }
        let s = |c| Symbol::default_for(GeometryType::Polygon, c);
        let mut other = s(3);
        other.fill = other.fill.with_alpha(0);
        other.stroke = other.stroke.with_alpha(0);
        let r = Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".into(), s(0))],
            other,
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let labels = |r: &Renderer, rows: &[bool]| r.legend_for(&d, rows).into_iter().map(|e| e.label).collect::<Vec<_>>();
        assert_eq!(labels(&r, &[]), ["a"], "z falls in the invisible fallback");
        assert!(labels(&r, &[false, true]).is_empty(), "only the invisible fallback is used");
        // A visible stroke alone still stands for the features.
        let mut stroked = other;
        stroked.stroke = stroked.stroke.with_alpha(255);
        let r2 = Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".into(), s(0))],
            other: stroked,
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        assert_eq!(labels(&r2, &[]), ["a", OTHER_VALUES]);
        // A line's fill draws nothing: a fallback with only a fill is invisible on lines.
        let mut lines = Dataset::new("l", GeometryType::Polyline, vec![Field::new("g", FieldType::Text)]);
        lines.push(None, vec![Value::Text("z".into())]);
        let mut filled = other;
        filled.fill = filled.fill.with_alpha(255);
        let r3 = Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".into(), s(0))],
            other: filled,
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        assert!(r3.legend_for(&lines, &[]).is_empty());
        assert_eq!(labels(&r3, &[]), ["a", OTHER_VALUES], "a polygon fill still shows");
        // An outline without a width draws nothing, nor does a point of size 0 (as a web style has it).
        let with_other = |r: &Renderer, o: Symbol| match r.clone() {
            Renderer::Unique { field, classes, ramp, class_labels, other_label, class_strokes, class_fills, expression, .. } => {
                Renderer::Unique {
                    field,
                    classes,
                    other: o,
                    ramp,
                    class_labels,
                    other_label,
                    class_strokes,
                    other_strokes: Vec::new(),
                    class_fills,
                    other_fills: Vec::new(),
                    expression,
                }
            }
            r => r,
        };
        let mut hairline = stroked;
        hairline.stroke_width = 0.0;
        assert_eq!(labels(&with_other(&r, hairline), &[]), ["a"]);
        // A line's stroke draws whatever its width.
        assert!(!with_other(&r, hairline).legend_for(&lines, &[]).is_empty());
        let mut points = Dataset::new("p", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
        points.push(None, vec![Value::Text("z".into())]);
        let mut dot = Symbol::default_for(GeometryType::Point, 3);
        assert!(!with_other(&r, dot).legend_for(&points, &[]).is_empty(), "a sized point shows");
        dot.size = 0.0;
        assert!(with_other(&r, dot).legend_for(&points, &[]).is_empty());
        // The full legend (the symbology editor's list) keeps it.
        assert_eq!(r.legend().len(), 2);
    }

    /// Each row keeps its class's strokes, its class stored in either order, in both renderers
    /// with classes.
    #[test]
    fn a_legend_row_has_its_class_own_strokes() {
        let mut d = Dataset::new("t", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
        for g in ["a", "b", "z"] {
            d.push(None, vec![Value::Text(g.into())]);
        }
        let s = |c| Symbol::default_for(GeometryType::Point, c);
        let stroke = |width| LineStroke { width: Some(width), ..LineStroke::default() };
        // Stored in either order, the rows come by label and each keeps its class's strokes.
        let (a, b) = (("a".to_string(), s(0)), ("b".to_string(), s(1)));
        for classes in [vec![a.clone(), b.clone()], vec![b, a]] {
            let r = Renderer::Unique {
                field: "g".into(),
                classes,
                other: s(2),
                ramp: Ramp::Ember,
                class_labels: Default::default(),
                other_label: None,
                class_strokes: [("a".to_string(), vec![stroke(1.0)])].into_iter().collect(),
                other_strokes: vec![stroke(2.0)],
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: None,
            };
            // The whole list and the classes of the features kept.
            for rows in [r.legend(), r.legend_for(&d, &[])] {
                let strokes: Vec<_> = rows.iter().map(|e| e.strokes.clone()).collect();
                assert_eq!(strokes, [vec![stroke(1.0)], vec![], vec![stroke(2.0)]]);
            }
        }
        let g = Renderer::Graduated {
            field: "v".into(),
            breaks: vec![1.0, 2.0],
            symbols: vec![s(0), s(1)],
            ramp: Ramp::Ember,
            method: ClassMethod::Quantile,
            min: 0.0,
            classes: 2,
            class_labels: Default::default(),
            class_strokes: [(1, vec![stroke(1.0)])].into_iter().collect(),
            class_fills: Default::default(),
        };
        let strokes: Vec<_> = g.legend().into_iter().map(|e| e.strokes).collect();
        assert_eq!(strokes, [vec![], vec![stroke(1.0)]]);
    }

    #[test]
    fn categorical_children_sort_case_insensitively_without_changing_symbols() {
        let names = ["Zulu", "alpha", "Bravo", "ALPHA", "éclair", "École"];
        let mut data = Dataset::new("categories", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        let classes: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                data.push(None, vec![Value::Text((*name).into())]);
                ((*name).into(), Symbol::default_for(GeometryType::Point, i as u32))
            })
            .collect();
        let renderer = Renderer::Unique {
            field: "name".into(),
            classes: classes.clone(),
            other: Symbol::default_for(GeometryType::Point, 99),
            ramp: Ramp::Ember,
            class_labels: BTreeMap::new(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let legend = renderer.legend();
        assert_eq!(
            legend.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(),
            ["alpha", "ALPHA", "Bravo", "Zulu", "éclair", "École", "<all other values>"]
        );
        for (i, (name, symbol)) in classes.iter().enumerate() {
            assert_eq!(legend.iter().find(|e| &e.label == name).unwrap().symbol, *symbol);
            assert_eq!(renderer.symbol_for(&data, i), *symbol);
        }
        let filtered = renderer.legend_for(&data, &[true, false, true, true, false, false]);
        assert_eq!(filtered.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["ALPHA", "Bravo", "Zulu"]);
        for entry in &filtered {
            assert_eq!(entry.symbol, classes.iter().find(|(name, _)| name == &entry.label).unwrap().1);
            assert!(!entry.fallback);
        }
        data.push(None, vec![Value::Text("unclassified".into())]);
        assert_eq!(renderer.legend_for(&data, &[]), legend);
    }

    #[test]
    fn breaks() {
        let v: Vec<f64> = vec![1.0, 2.0, 3.0, 10.0, 11.0, 12.0, 50.0, 51.0, 52.0];
        let j = class_breaks(&v, 3, ClassMethod::NaturalBreaks);
        assert_eq!(j, vec![3.0, 12.0, 52.0]);
        assert_eq!(class_breaks(&v, 3, ClassMethod::EqualInterval).len(), 3);
        assert_eq!(class_breaks(&[], 3, ClassMethod::Quantile), Vec::<f64>::new());
        assert_eq!(class_breaks(&[5.0, 5.0], 4, ClassMethod::NaturalBreaks), vec![5.0]);
    }

    fn close(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9 * y.abs().max(1.0))
    }

    #[test]
    fn geometric_interval_breaks_grow_by_one_ratio() {
        // Positive values: each bound is the one before times (hi / lo)^(1/k).
        let v = [1.0, 5.0, 40.0, 300.0, 1000.0];
        let b = class_breaks(&v, 3, ClassMethod::GeometricInterval);
        assert!(close(&b, &[10.0, 100.0, 1000.0]), "{b:?}");
        // From zero (or below), the values are shifted so the minimum sits at a hundredth of the
        // range: the widths still grow by one ratio, and the last bound is the maximum.
        let b = class_breaks(&[-50.0, 0.0, 50.0], 2, ClassMethod::GeometricInterval);
        assert!(close(&b, &[-51.0 + 101f64.sqrt(), 50.0]), "{b:?}");
        let b = class_breaks(&[0.0, 100.0], 4, ClassMethod::GeometricInterval);
        let widths: Vec<f64> = std::iter::once(b[0]).chain(b.windows(2).map(|w| w[1] - w[0])).collect();
        assert!(widths.windows(2).all(|w| w[1] > w[0]), "{b:?}");
        assert_eq!(b.len(), 4);
        assert_eq!(b.last(), Some(&100.0));
        // Far from zero the shift is not added and taken away again, so it can't round off.
        let b = class_breaks(&[-1e16, -9_999_999_999_999_998.0], 2, ClassMethod::GeometricInterval);
        assert!(b.iter().all(|v| v.is_finite()), "{b:?}");
        assert_eq!(b.last(), Some(&-9_999_999_999_999_998.0));
        // Near the largest float nothing overflows: the bounds stay finite and in order.
        for v in [[1e308, 1.1e308], [-1e308, 1e308], [1e-308, 1e308]] {
            let b = class_breaks(&v, 2, ClassMethod::GeometricInterval);
            assert!(b.len() == 2 && b.iter().all(|x| x.is_finite()) && b[0] >= v[0] && b[0] < b[1], "{v:?}: {b:?}");
        }
    }

    #[test]
    fn standard_deviation_breaks_are_one_deviation_wide_about_the_mean() {
        // 0..=100: mean 50, population deviation sqrt(850).
        let v: Vec<f64> = (0..=100).map(f64::from).collect();
        let sd = 850f64.sqrt();
        // An even count puts a bound on the mean; an odd one centres a class on it.
        let b = class_breaks(&v, 4, ClassMethod::StandardDeviation);
        assert!(close(&b, &[50.0 - sd, 50.0, 50.0 + sd, 100.0]), "{b:?}");
        let b = class_breaks(&v, 3, ClassMethod::StandardDeviation);
        assert!(close(&b, &[50.0 - sd / 2.0, 50.0 + sd / 2.0, 100.0]), "{b:?}");
        // Bounds past the data are dropped rather than leaving empty classes: 8 classes of one
        // deviation would reach ±4 deviations, but the data spans less than ±2.
        let b = class_breaks(&v, 8, ClassMethod::StandardDeviation);
        assert!(close(&b, &[50.0 - sd, 50.0, 50.0 + sd, 100.0]), "{b:?}");
        // A bound with no value above the one before it goes too: mean 4, deviation sqrt(24), so
        // (4, 4 + sqrt(24)] would hold nothing between the zeros and the tens.
        let b = class_breaks(&[0.0, 0.0, 0.0, 10.0, 10.0], 4, ClassMethod::StandardDeviation);
        assert!(close(&b, &[4.0, 10.0]), "{b:?}");
        // Large values: the squared deviations would overflow without scaling.
        let b = class_breaks(&[1e160, 2e160], 2, ClassMethod::StandardDeviation);
        assert!(close(&b, &[1.5e160, 2e160]), "{b:?}");
        let b = class_breaks(&[-1e308, 1e308], 2, ClassMethod::StandardDeviation);
        assert!(close(&b, &[0.0, 1e308]), "{b:?}");
    }

    #[test]
    fn arithmetic_progression_widths_grow_by_one_step() {
        // 0..=100 in 4 classes: widths of 1, 2, 3 and 4 tenths of the range.
        let v: Vec<f64> = (0..=100).map(f64::from).collect();
        let b = class_breaks(&v, 4, ClassMethod::ArithmeticProgression);
        assert!(close(&b, &[10.0, 30.0, 60.0, 100.0]), "{b:?}");
        // Across the whole float range nothing overflows.
        let b = class_breaks(&[-1e308, 1e308], 3, ClassMethod::ArithmeticProgression);
        assert!(b.len() == 3 && b.iter().all(|x| x.is_finite()) && b.windows(2).all(|w| w[0] < w[1]), "{b:?}");
    }

    #[test]
    fn head_tail_breaks_follow_the_heavy_tail() {
        // 1..=10 and 100: the mean is 155/11 and the head above it is [100] alone: one split.
        let mut v: Vec<f64> = (1..=10).map(f64::from).collect();
        v.push(100.0);
        let b = class_breaks(&v, 5, ClassMethod::HeadTailBreaks);
        assert!(close(&b, &[155.0 / 11.0, 100.0]), "{b:?}");
        // A power law: each head is a small share of the values before it, so it goes on, up to
        // the classes asked.
        let v: Vec<f64> = (1..=1000).map(|i| 1000.0 / f64::from(i)).rev().collect();
        let b = class_breaks(&v, 4, ClassMethod::HeadTailBreaks);
        assert_eq!(b.len(), 4, "{b:?}");
        assert!(b.windows(2).all(|w| w[0] < w[1]) && b.last() == Some(&1000.0), "{b:?}");
        let few = class_breaks(&v, 2, ClassMethod::HeadTailBreaks);
        assert_eq!(few.len(), 2, "{few:?}");
        // Evenly spread values: the head above the mean is half of them, so it stops at one split.
        let v: Vec<f64> = (0..=100).map(f64::from).collect();
        let b = class_breaks(&v, 6, ClassMethod::HeadTailBreaks);
        assert!(close(&b, &[50.0, 100.0]), "{b:?}");
        // Large values: the mean can't overflow.
        let b = class_breaks(&[1e308, 1e308, 1.5e308], 3, ClassMethod::HeadTailBreaks);
        assert!(b.iter().all(|x| x.is_finite()) && b.len() == 2, "{b:?}");
        // Nor with large values of opposite signs.
        for v in [[-1.7e308, 1.7e308, 1.7e308], [-1e308, -1e308, 1e308]] {
            let b = class_breaks(&v, 3, ClassMethod::HeadTailBreaks);
            assert!(b.len() == 2 && b.iter().all(|x| x.is_finite()) && b[0] > v[0] && b[0] < b[1], "{v:?}: {b:?}");
        }
    }

    #[test]
    fn close_values_keep_their_classes() {
        // Bounds closer than f64::EPSILON in absolute terms are still distinct when the values
        // themselves are that small.
        for m in ClassMethod::ALL {
            let b = class_breaks(&[0.0, 1e-18], 2, m);
            assert!(b.len() == 2 && b[0] >= 0.0 && b[0] < b[1], "{m:?}: {b:?}");
        }
        // And close values far from zero keep theirs too.
        let b = class_breaks(&[1e16, 1e16 + 2.0], 2, ClassMethod::Quantile);
        assert_eq!(b, vec![1e16, 1e16 + 2.0]);
    }

    #[test]
    fn every_method_is_listed_named_and_read_back() {
        for m in ClassMethod::ALL {
            let name = format!("{m:?}");
            let camel = format!("{}{}", name[..1].to_ascii_lowercase(), &name[1..]);
            assert_eq!(ClassMethod::from_name(&camel), Some(m), "{camel}");
            assert_eq!(ClassMethod::from_name(&m.label().replace(" (Jenks)", "")), Some(m), "{}", m.label());
        }
        assert_eq!(ClassMethod::from_name("stddev"), Some(ClassMethod::StandardDeviation));
        assert_eq!(ClassMethod::from_name("geometric"), Some(ClassMethod::GeometricInterval));
        assert_eq!(ClassMethod::from_name("arithmetic"), Some(ClassMethod::ArithmeticProgression));
        assert_eq!(ClassMethod::from_name("headTail"), Some(ClassMethod::HeadTailBreaks));
    }

    #[test]
    fn graduated_keeps_the_classes_asked() {
        // Head/Tail Breaks finds 2 classes in evenly spread values; the 5 asked are kept, so the
        // next method or class count starts from them.
        let mut d = Dataset::new("t", GeometryType::Polygon, vec![Field::new("v", FieldType::Double)]);
        for v in 0..=100 {
            d.push(None, vec![Value::Float(f64::from(v))]);
        }
        let base = Symbol::default_for(GeometryType::Polygon, 0);
        let r = Renderer::graduated(&d, &[], "v", 5, ClassMethod::HeadTailBreaks, Ramp::Ember, base).unwrap();
        let Renderer::Graduated { breaks, .. } = &r else { panic!("graduated renderer expected") };
        assert_eq!((breaks.len(), r.classes_asked()), (2, 5));
        let back: Renderer = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.classes_asked(), 5);
        // Manual bounds ask for as many classes as they make.
        let m = Renderer::graduated_manual(&d, &[], "v", &[10.0, 20.0], Ramp::Ember, base).unwrap();
        assert_eq!(m.classes_asked(), 3);
        // A project from before the count was kept asks for as many classes as it has.
        let mut old = serde_json::to_value(&r).unwrap();
        old["Graduated"].as_object_mut().unwrap().remove("classes");
        let old: Renderer = serde_json::from_value(old).unwrap();
        assert_eq!(old.classes_asked(), 2);
    }

    #[test]
    fn graduated_symbols_by_value() {
        let mut d = Dataset::new("t", GeometryType::Polygon, vec![Field::new("v", FieldType::Double)]);
        for v in [1.0, 2.0, 50.0, 60.0] {
            d.push(None, vec![Value::Float(v)]);
        }
        d.push(None, vec![Value::Null]);
        let r =
            Renderer::graduated(&d, &[], "v", 2, ClassMethod::NaturalBreaks, Ramp::Ember, Symbol::default_for(GeometryType::Polygon, 0))
                .unwrap();
        assert_ne!(r.symbol_for(&d, 0).fill, r.symbol_for(&d, 3).fill);
        assert_eq!(r.legend().len(), 2);
        let _ = r.symbol_for(&d, 4);
        let _ = r.symbol_for(&d, 999);
        assert!(
            Renderer::graduated(&d, &[], "nope", 2, ClassMethod::Quantile, Ramp::Ember, Symbol::default_for(GeometryType::Polygon, 0))
                .is_err()
        );
    }

    #[test]
    fn unique_values_by_an_expression_count_what_is_past_their_classes() {
        let base = Symbol::default_for(GeometryType::Point, 0);
        let mut d = Dataset::new("p", GeometryType::Point, vec![rustgis_data::Field::new("n", rustgis_data::FieldType::Integer)]);
        // 100 values, each on two features, in no order: the first 64 (by number) are kept.
        for i in (0..100i64).rev().chain(0..100) {
            d.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![rustgis_data::Value::Int(i)]);
        }
        let (r, rest) = Renderer::unique_by_counted(&d, &[], "n", Ramp::Harbour, base).unwrap();
        let Renderer::Unique { classes, .. } = &r else { panic!() };
        let all: Vec<String> = (0..100).map(|i: i64| i.to_string()).collect();
        assert_eq!(classes.iter().map(|c| c.0.clone()).collect::<Vec<_>>(), all[..MAX_UNIQUE]);
        assert_eq!(rest, PastClasses { features: 2 * (100 - MAX_UNIQUE), by_number: true });
        // Text the expression makes of them is kept in text order.
        let (r, rest) = Renderer::unique_by_counted(&d, &[], "'k' || n", Ramp::Harbour, base).unwrap();
        let Renderer::Unique { classes, .. } = &r else { panic!() };
        let mut all: Vec<String> = (0..100).map(|i: i64| format!("k{i}")).collect();
        all.sort();
        assert_eq!(classes.iter().map(|c| c.0.clone()).collect::<Vec<_>>(), all[..MAX_UNIQUE]);
        assert_eq!(rest, PastClasses { features: 2 * (100 - MAX_UNIQUE), by_number: false });
        // One text among the numbers, given last: the first are taken in text order, though every
        // value before it read as a number, and though those kept are all numbers.
        let (r, rest) = Renderer::unique_by_counted(&d, &[], "CASE WHEN n = 99 THEN 'x' ELSE n END", Ramp::Harbour, base).unwrap();
        let Renderer::Unique { classes, .. } = &r else { panic!() };
        let mut all: Vec<String> = (0..99).map(|i: i64| i.to_string()).chain(["x".to_string()]).collect();
        all.sort();
        assert_eq!(classes.iter().map(|c| c.0.clone()).collect::<Vec<_>>(), all[..MAX_UNIQUE]);
        assert_eq!(rest, PastClasses { features: 2 * (100 - MAX_UNIQUE), by_number: false });
        let (_, rest) = Renderer::unique_by_counted(&d, &[], "n % 10", Ramp::Harbour, base).unwrap();
        assert_eq!(rest, PastClasses { features: 0, by_number: true });
        // A feature without a geometry draws nothing, so it isn't counted.
        d.push(None, vec![rustgis_data::Value::Int(99)]);
        let (_, rest) = Renderer::unique_by_counted(&d, &[], "n", Ramp::Harbour, base).unwrap();
        assert_eq!(rest.features, 2 * (100 - MAX_UNIQUE));
    }

    /// #1340: Unique Values on a numeric field, by the field or by an expression, class and list
    /// their values in numeric order; on a text field, in text order and by label, as before.
    #[test]
    fn unique_values_of_numbers_are_in_numeric_order() {
        let point = || Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0)));
        let base = Symbol::default_for(GeometryType::Point, 0);
        let fields = vec![Field::new("i", FieldType::Integer), Field::new("r", FieldType::Double), Field::new("t", FieldType::Text)];
        let mut d = Dataset::new("t", GeometryType::Point, fields);
        for (i, r, t) in [(10153, 10.0, "b"), (1741, 2.5, "A"), (9627, -1.0, "10"), (9, 10.0, "9")] {
            d.push(point(), vec![Value::Int(i), Value::Float(r), Value::Text(t.into())]);
        }
        let values = |r: &Renderer| match r {
            Renderer::Unique { classes, .. } => classes.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
            _ => panic!("{r:?}"),
        };
        let labels = |l: Vec<LegendEntry>| l.into_iter().map(|e| e.label).collect::<Vec<_>>();
        for (by, expected) in [("i", ["9", "1741", "9627", "10153"]), ("r", ["-1", "2.5", "10", ""])] {
            let expected: Vec<&str> = expected.into_iter().filter(|v| !v.is_empty()).collect();
            let by_field = Renderer::unique(&d, &[], by, Ramp::Harbour, base).unwrap();
            let by_expression = Renderer::unique_by(&d, &[], by, Ramp::Harbour, base).unwrap();
            for r in [&by_field, &by_expression] {
                assert_eq!(values(r), expected, "{by}");
                assert!(r.classes_by_number());
                let mut legend = expected.clone();
                legend.push(OTHER_VALUES);
                assert_eq!(labels(r.legend()), legend, "{by}");
                assert_eq!(labels(r.legend_for(&d, &[])), expected, "{by}: what is drawn");
            }
        }
        // A text field: text order, and the legend by label without regard to case.
        let r = Renderer::unique(&d, &[], "t", Ramp::Harbour, base).unwrap();
        assert_eq!(values(&r), ["10", "9", "A", "b"]);
        assert!(!r.classes_by_number());
        assert_eq!(labels(r.legend_for(&d, &[])), ["10", "9", "A", "b"]);
        // Nulls are a class after the numbers.
        d.push(point(), vec![Value::Null]);
        let r = Renderer::unique(&d, &[], "i", Ramp::Harbour, base).unwrap();
        assert_eq!(values(&r), ["9", "1741", "9627", "10153", "<Null>"]);
        assert_eq!(labels(r.legend_for(&d, &[])), ["9", "1741", "9627", "10153", "<Null>"]);
    }

    /// A class of numbers renamed by hand keeps its value's place in the legend; classes saved in
    /// text order (a project from before) are listed in numeric order too.
    #[test]
    fn a_renamed_number_class_keeps_its_place() {
        let s = |c| Symbol::default_for(GeometryType::Polygon, c);
        let mut r = unique(&[("10", s(0)), ("100", s(1)), ("9", s(2))], s(3), Ramp::Ember);
        assert_eq!(r.class_order(), [2, 0, 1]);
        let labels = |r: &Renderer| r.legend().into_iter().map(|e| e.label).collect::<Vec<_>>();
        assert_eq!(labels(&r), ["9", "10", "100", OTHER_VALUES]);
        let style = ClassStyle { label: Some("Arable".into()), ..Default::default() };
        r.set_class_style(&ClassRef::Value("100".into()), &style, GeometryType::Polygon).unwrap();
        assert_eq!(labels(&r), ["9", "10", "Arable", OTHER_VALUES]);
        // Each row keeps its class's own symbol.
        let fills: Vec<Rgba> = r.legend().iter().map(|e| e.symbol.fill).collect();
        assert_eq!(fills, [s(2).fill, s(0).fill, s(1).fill, s(3).fill]);
        // A repeated value keeps its first class first.
        let r = unique(&[("10", s(0)), ("9", s(1)), ("10", s(2))], s(3), Ramp::Ember);
        assert_eq!(r.class_order(), [1, 0, 2]);
        // With text among them, by label as before.
        let r = unique(&[("10", s(0)), ("b", s(1)), ("9", s(2)), ("A", s(4))], s(3), Ramp::Ember);
        assert_eq!(labels(&r), ["10", "9", "A", "b", OTHER_VALUES]);
    }

    /// Past [`MAX_UNIQUE`] values, the classes are the smallest numbers and the features past
    /// them are counted; with fewer, none are.
    #[test]
    fn unique_values_of_a_field_count_what_is_past_their_classes() {
        let base = Symbol::default_for(GeometryType::Point, 0);
        let mut d = Dataset::new("p", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        for i in (0..100i64).rev().chain(0..100) {
            d.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![Value::Int(i)]);
        }
        let (r, rest) = Renderer::unique_counted(&d, &[], "n", Ramp::Harbour, base).unwrap();
        let Renderer::Unique { classes, .. } = &r else { panic!() };
        let first: Vec<String> = (0..MAX_UNIQUE).map(|i| i.to_string()).collect();
        assert_eq!(classes.iter().map(|c| c.0.clone()).collect::<Vec<_>>(), first);
        assert_eq!(rest, PastClasses { features: 2 * (100 - MAX_UNIQUE), by_number: true });
        // The same as by the expression of the field.
        assert_eq!(Renderer::unique_by_counted(&d, &[], "n", Ramp::Harbour, base).unwrap().1, rest);
        // A feature without a geometry draws nothing, so it isn't counted.
        d.push(None, vec![Value::Int(99)]);
        assert_eq!(Renderer::unique_counted(&d, &[], "n", Ramp::Harbour, base).unwrap().1, rest);
        let rows: Vec<bool> = (0..d.features.len()).map(|r| r < 100).collect();
        assert_eq!(Renderer::unique_counted(&d, &rows, "n", Ramp::Harbour, base).unwrap().1.features, 100 - MAX_UNIQUE);
        let few: Vec<bool> = (0..d.features.len()).map(|r| r < MAX_UNIQUE).collect();
        assert_eq!(Renderer::unique_counted(&d, &few, "n", Ramp::Harbour, base).unwrap().1.features, 0);
    }

    /// The rest is in the legend when its stacked fills or strokes draw, though its own symbol
    /// doesn't.
    #[test]
    fn the_rests_stacked_fills_and_strokes_put_it_in_the_legend() {
        for g in [GeometryType::Polygon, GeometryType::Polyline] {
            let mut d = Dataset::new("p", g, vec![]);
            let shape = match g {
                GeometryType::Polygon => rustgis_data::Geometry::Polygon(vec![vec![vec![
                    rustgis_geom::Coord::new(0.0, 0.0),
                    rustgis_geom::Coord::new(1.0, 0.0),
                    rustgis_geom::Coord::new(0.0, 1.0),
                    rustgis_geom::Coord::new(0.0, 0.0),
                ]]]),
                _ => rustgis_data::Geometry::Line(vec![vec![rustgis_geom::Coord::new(0.0, 0.0), rustgis_geom::Coord::new(1.0, 1.0)]]),
            };
            d.push(Some(shape), vec![]);
            let mut r = Renderer::unique_by(&d, &[], "NULL", Ramp::Harbour, Symbol::default_for(g, 0)).unwrap();
            let Renderer::Unique { other, .. } = &mut r else { panic!() };
            (other.fill, other.stroke) = (Rgba::new(0, 0, 0, 0), Rgba::new(0, 0, 0, 0));
            let rest = |r: &Renderer| r.legend_for(&d, &[]).iter().any(|e| e.fallback);
            assert!(!rest(&r), "{g:?}: nothing draws");
            let Renderer::Unique { other_fills, other_strokes, .. } = &mut r else { panic!() };
            match g {
                GeometryType::Polygon => {
                    other_fills.push(crate::fills::FillLayer { color: Some(Rgba::rgb(1, 2, 3)), ..Default::default() })
                }
                _ => other_strokes.push(LineStroke { color: Some(Rgba::rgb(1, 2, 3)), width: Some(2.0), ..Default::default() }),
            }
            assert!(rest(&r), "{g:?}: the stacked one draws");
        }
    }

    /// Label clauses read the scale the labels are drawn at.
    #[test]
    fn label_clauses_read_the_drawing_scale() {
        let mut d = Dataset::new("p", GeometryType::Point, vec![]);
        d.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![]);
        let mut l = Labeling::default_for(&d);
        l.default.where_ = Some("@map_scale > 1000".into());
        assert_eq!(LabelMatcher::new(&l, &d, Some(5_000.0)).classes_of(&d, 0).count(), 1);
        assert_eq!(LabelMatcher::new(&l, &d, Some(500.0)).classes_of(&d, 0).count(), 0);
        assert_eq!(LabelMatcher::new(&l, &d, None).classes_of(&d, 0).count(), 0);
    }

    #[test]
    fn unique_values_class_by_an_expression() {
        let mut d =
            Dataset::new("t", GeometryType::Point, vec![Field::new("pop", FieldType::Integer), Field::new("name", FieldType::Text)]);
        for (pop, name) in [(5, "a"), (500, "b"), (50_000, "c"), (7, "d")] {
            d.push(None, vec![Value::Int(pop), Value::Text(name.into())]);
        }
        d.push(None, vec![Value::Null, Value::Text("e".into())]);
        let base = Symbol::default_for(GeometryType::Point, 0);
        let e = "CASE WHEN pop >= 1000 THEN 'city' WHEN pop >= 100 THEN 'town' WHEN pop < 100 THEN 'village' END";
        let r = Renderer::unique_by(&d, &[], e, Ramp::Harbour, base).unwrap();
        let Renderer::Unique { field, classes, expression, .. } = &r else { panic!() };
        assert!(field.is_empty());
        assert_eq!(expression.as_deref(), Some(e));
        assert_eq!(classes.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["city", "town", "village"], "a null value isn't a class");
        assert_eq!((r.field(), r.expression()), (None, Some(e)));
        let c = r.classer(&d, None);
        let class: Vec<Option<usize>> = (0..5).map(|row| r.unique_class(&d, row, &c)).collect();
        assert_eq!(class, [Some(2), Some(1), Some(0), Some(2), None]);
        assert_eq!(r.symbol_for(&d, 1), classes[1].1, "one row at a time too");
        // The legend lists the classes some kept feature has, and the rest for the null.
        let labels = |rows: &[bool]| r.legend_for(&d, rows).into_iter().map(|e| e.label).collect::<Vec<_>>();
        assert_eq!(labels(&[]), ["city", "town", "village", OTHER_VALUES]);
        assert_eq!(labels(&[true, false, false, true, false]), ["village"]);
        // Numbers and values that print: as they print.
        let r = Renderer::unique_by(&d, &[], "pop % 2", Ramp::Harbour, base).unwrap();
        let Renderer::Unique { classes, .. } = &r else { panic!() };
        assert_eq!(classes.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["0", "1"]);
        // What doesn't read is refused when it's set, and classes nothing where it no longer reads.
        assert!(Renderer::unique_by(&d, &[], "nope + 1", Ramp::Harbour, base).unwrap_err().contains("nope"));
        assert!(Renderer::unique_by(&d, &[], &"1".repeat(rustgis_data::query::MAX_LEN + 1), Ramp::Harbour, base).is_err());
        let other = Dataset::new("t", GeometryType::Point, vec![Field::new("x", FieldType::Integer)]);
        let mut other = other;
        other.push(None, vec![Value::Int(1)]);
        assert_eq!(r.unique_class(&other, 0, &r.classer(&other, None)), None);
        // A renderer by a field has no expression to compile, and classes as before.
        let by_field = Renderer::unique(&d, &[], "name", Ramp::Harbour, base).unwrap();
        assert_eq!((by_field.field(), by_field.expression()), (Some("name"), None));
        assert_eq!(by_field.unique_class(&d, 1, &by_field.classer(&d, None)), Some(1));
    }

    #[test]
    fn a_class_expression_reads_the_map_scale_and_saves_as_text() {
        let mut d = Dataset::new("t", GeometryType::Point, vec![Field::new("pop", FieldType::Integer)]);
        d.push(None, vec![Value::Int(5)]);
        let base = Symbol::default_for(GeometryType::Point, 0);
        let e = "CASE WHEN @map_scale IS NULL OR @map_scale < 10000 THEN 'near' ELSE 'far' END";
        let r = Renderer::unique_by(&d, &[], e, Ramp::Harbour, base).unwrap();
        let Renderer::Unique { classes, .. } = &r else { panic!() };
        let key = |scale| r.unique_class(&d, 0, &r.classer(&d, scale)).and_then(|i| classes.get(i)).map(|c| c.0.as_str());
        assert_eq!((key(None), key(Some(5000.0)), key(Some(50_000.0))), (Some("near"), Some("near"), None), "'far' isn't a class");
        // Saved as its text; read back the same, and junk read as none.
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["Unique"]["expression"], e);
        assert_eq!(serde_json::from_value::<Renderer>(json.clone()).unwrap(), r);
        for junk in [serde_json::json!(5), serde_json::json!("  "), serde_json::json!("x".repeat(rustgis_data::query::MAX_LEN + 1))] {
            let mut j = json.clone();
            j["Unique"]["expression"] = junk;
            assert_eq!(serde_json::from_value::<Renderer>(j).unwrap().expression(), None);
        }
        // A renderer from before: no expression key, and none written.
        let by_field = Renderer::unique(&d, &[], "pop", Ramp::Harbour, base).unwrap();
        let json = serde_json::to_value(&by_field).unwrap();
        assert!(json["Unique"].get("expression").is_none());
        // Edits carry over on the same expression only.
        let mut again = Renderer::unique_by(&d, &[], e, Ramp::Harbour, base).unwrap();
        let mut edited = r.clone();
        if let Renderer::Unique { class_labels, .. } = &mut edited {
            class_labels.insert("near".into(), "Near".into());
        }
        again.carry_edits_from(&edited, GeometryType::Point);
        assert_eq!(again.legend()[0].label, "Near");
        let mut other = Renderer::unique_by(&d, &[], "'near'", Ramp::Harbour, base).unwrap();
        other.carry_edits_from(&edited, GeometryType::Point);
        assert_eq!(other.legend()[0].label, "near");
    }

    #[test]
    fn classes_come_from_the_kept_rows() {
        let mut d = Dataset::new("t", GeometryType::Polygon, vec![Field::new("v", FieldType::Double)]);
        for v in [1.0, 2.0, 3.0, 1000.0] {
            d.push(None, vec![Value::Float(v)]);
        }
        let base = Symbol::default_for(GeometryType::Polygon, 0);
        let rows = [true, true, true, false];
        let Ok(Renderer::Graduated { breaks, min, .. }) =
            Renderer::graduated(&d, &rows, "v", 2, ClassMethod::EqualInterval, Ramp::Ember, base)
        else {
            panic!("graduated renderer expected")
        };
        assert_eq!(breaks.last().copied(), Some(3.0));
        assert_eq!(min, 1.0);
        // A short row list keeps the rows past its end.
        let Ok(Renderer::Graduated { breaks, .. }) =
            Renderer::graduated(&d, &[false], "v", 2, ClassMethod::EqualInterval, Ramp::Ember, base)
        else {
            panic!("graduated renderer expected")
        };
        assert_eq!(breaks.last().copied(), Some(1000.0));
        assert!(Renderer::graduated(&d, &[false; 4], "v", 2, ClassMethod::Quantile, Ramp::Ember, base).is_err());
        let Ok(Renderer::Unique { classes, .. }) = Renderer::unique(&d, &[false, true, false, true], "v", Ramp::Harbour, base) else {
            panic!("unique renderer expected")
        };
        let keys: Vec<&str> = classes.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["2", "1000"]);
    }

    fn unique(classes: &[(&str, Symbol)], other: Symbol, ramp: Ramp) -> Renderer {
        Renderer::Unique {
            field: "g".into(),
            classes: classes.iter().map(|(k, s)| (k.to_string(), *s)).collect(),
            other,
            ramp,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        }
    }

    fn graduated(breaks: &[f64], symbols: &[Symbol], ramp: Ramp) -> Renderer {
        Renderer::Graduated {
            field: "v".into(),
            breaks: breaks.to_vec(),
            symbols: symbols.to_vec(),
            ramp,
            method: ClassMethod::Quantile,
            min: 0.0,
            classes: breaks.len(),
            class_labels: Default::default(),
            class_strokes: Default::default(),
            class_fills: Default::default(),
        }
    }

    fn labels_of(r: &Renderer) -> Vec<String> {
        r.legend().into_iter().map(|e| e.label).collect()
    }

    #[test]
    fn a_class_takes_its_own_colour_outline_and_label() {
        let g = GeometryType::Polygon;
        let s = |c| Symbol::default_for(g, c);
        let (red, blue) = (Rgba::rgb(0xC0, 0x30, 0x20), Rgba::rgb(0, 0, 0xFF));
        let mut r = unique(&[("a", s(0)), ("b", s(1))], s(3), Ramp::Ember);
        let style =
            |color, outline, label: Option<&str>| ClassStyle { color, outline, label: label.map(str::to_string), ..Default::default() };
        let e = r.set_class_style(&ClassRef::Value("b".into()), &style(Some(red), None, Some("  Bee ")), g).unwrap();
        assert_eq!(e.label, "Bee");
        assert_eq!((e.symbol.fill, e.symbol.stroke, e.fallback), (red, red.shade(0.6), false));
        assert_eq!(labels_of(&r), ["a", "Bee", OTHER_VALUES]);
        // The fallback class, with an outline alone: the fill stays.
        let e = r.set_class_style(&ClassRef::Other, &style(None, Some(blue), Some("Others")), g).unwrap();
        assert!(e.fallback);
        assert_eq!((e.symbol.stroke, e.symbol.fill), (blue, s(3).fill));
        // The legend of what is drawn shows the labels too.
        let mut d = Dataset::new("t", g, vec![Field::new("g", FieldType::Text)]);
        for v in ["b", "z"] {
            d.push(None, vec![Value::Text(v.into())]);
        }
        assert_eq!(r.legend_for(&d, &[]).into_iter().map(|e| e.label).collect::<Vec<_>>(), ["Bee", "Others"]);
        // A label that is the value itself (or <all other values>) is no label of its own.
        r.set_class_style(&ClassRef::Value("b".into()), &style(None, None, Some("b")), g).unwrap();
        assert!(matches!(&r, Renderer::Unique { class_labels, .. } if class_labels.is_empty()));
        r.set_class_style(&ClassRef::Value("b".into()), &style(None, None, Some("Bee")), g).unwrap();
        r.set_class_style(&ClassRef::Other, &style(None, None, Some(OTHER_VALUES)), g).unwrap();
        assert!(matches!(&r, Renderer::Unique { other_label: None, .. }));
        r.set_class_style(&ClassRef::Other, &style(None, None, Some("Others")), g).unwrap();
        // An empty (or blank) label goes back to the value.
        r.set_class_style(&ClassRef::Value("b".into()), &style(None, None, Some("")), g).unwrap();
        r.set_class_style(&ClassRef::Other, &style(None, None, Some("  ")), g).unwrap();
        assert_eq!(labels_of(&r), ["a", "b", OTHER_VALUES]);
        // A class that isn't there, or named the wrong way, is refused and changes nothing.
        let before = r.clone();
        assert!(r.set_class_style(&ClassRef::Value("nope".into()), &style(Some(red), None, None), g).is_err());
        assert!(r.set_class_style(&ClassRef::Index(0), &style(Some(red), None, None), g).is_err());
        assert_eq!(r, before);
        assert!(Renderer::Single(s(0)).set_class_style(&ClassRef::Other, &style(Some(red), None, None), g).is_err());
        // A line class takes the colour as its line colour.
        let line = Symbol::default_for(GeometryType::Polyline, 0);
        let mut lines = unique(&[("a", line)], line, Ramp::Ember);
        let e = lines.set_class_style(&ClassRef::Value("a".into()), &style(Some(red), None, None), GeometryType::Polyline).unwrap();
        assert_eq!(e.symbol.stroke, red);
        // Graduated Colors: by number, and the label replaces the range.
        let mut gr = graduated(&[10.0, 20.0], &[s(0), s(1)], Ramp::Ember);
        let e = gr.set_class_style(&ClassRef::Index(1), &style(Some(red), None, Some("High")), g).unwrap();
        assert_eq!((e.label.as_str(), e.symbol.fill, e.symbol.stroke), ("High", red, red.shade(0.6)));
        assert_eq!(labels_of(&gr), ["0 – 10", "High"]);
        gr.set_class_style(&ClassRef::Index(0), &style(None, None, Some("0 – 10")), g).unwrap();
        assert!(matches!(&gr, Renderer::Graduated { class_labels, .. } if !class_labels.contains_key(&0)), "the range itself is no label");
        assert!(gr.set_class_style(&ClassRef::Index(2), &style(None, None, Some("x")), g).is_err());
        assert!(gr.set_class_style(&ClassRef::Other, &style(None, None, Some("x")), g).is_err());
        // A very long label is cut.
        let e = gr.set_class_style(&ClassRef::Index(0), &style(None, None, Some(&"x".repeat(500))), g).unwrap();
        assert_eq!(e.label.chars().count(), MAX_LABEL);
        // A label is one line.
        let e = gr.set_class_style(&ClassRef::Index(0), &style(None, None, Some("Low\nincome\t")), g).unwrap();
        assert_eq!(e.label, "Low income");
        // A cut that lands on a space doesn't leave it at the end.
        let long = format!("{} {}", "x".repeat(MAX_LABEL - 1), "y".repeat(10));
        let e = gr.set_class_style(&ClassRef::Index(0), &style(None, None, Some(&long)), g).unwrap();
        assert_eq!(e.label, "x".repeat(MAX_LABEL - 1));
    }

    #[test]
    fn labels_are_moved_by_hand_and_put_back() {
        let ds = rustgis_data::Dataset::new(
            "d",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("name", rustgis_data::FieldType::Text)],
        );
        let mut c = LabelClass::default_for(&ds);
        assert!(c.move_label(7, Some([10.0, 20.0])) && c.move_label(8, Some([1.0, 2.0])));
        assert!(c.move_label(7, Some([11.0, 21.0])), "moved again, the same feature's");
        assert_eq!((c.moved_to(7), c.moved_to(8), c.moved.len()), (Some([11.0, 21.0]), Some([1.0, 2.0]), 2));
        assert!(!c.move_label(9, Some([f64::NAN, 0.0])), "not a place");
        assert!(!c.move_label(9, Some([1000.0, 0.0])) && !c.move_label(9, Some([0.0, 95.0])), "off the map");
        assert!(c.move_label(2, Some([0.0, 0.0])) && c.moved.windows(2).all(|w| w[0].id < w[1].id), "kept in order of id");
        assert!(c.move_label(2, None) && c.move_label(2, None), "putting back one not moved is fine");
        assert!(c.move_label(8, None) && c.moved_to(8).is_none(), "put back");
        // Read from a file: bad entries dropped, the rest kept, a feature's last place wins.
        let mut v = serde_json::to_value(&c).unwrap();
        v["moved"] = serde_json::json!([{"id": 1, "at": [1, 1]}, {"id": "x"}, 7, {"id": 1, "at": [2, 2]}, {"id": 3, "at": [3, 3]}, {"id": 0, "at": [1000, 95]}]);
        let read: LabelClass = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(read.moved, vec![MovedLabel { id: 1, at: [2.0, 2.0] }, MovedLabel { id: 3, at: [3.0, 3.0] }]);
        // Out of order in the file: put in order, so each is still found.
        v["moved"] = serde_json::json!([{"id": 9, "at": [9, 9]}, {"id": 4, "at": [4, 4]}]);
        let read: LabelClass = serde_json::from_value(v.clone()).unwrap();
        assert_eq!((read.moved_to(4), read.moved_to(9)), (Some([4.0, 4.0]), Some([9.0, 9.0])));
        // A huge list reads quickly, and keeps the first the file lists, up to the most kept.
        let many: Vec<_> = (0..50_000u64).rev().map(|i| serde_json::json!({"id": i, "at": [0, 0]})).collect();
        v["moved"] = serde_json::Value::Array(many);
        let read: LabelClass = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(read.moved.len(), MAX_MOVED_LABELS);
        v["moved"] = serde_json::json!("nonsense");
        assert!(serde_json::from_value::<LabelClass>(v.clone()).unwrap().moved.is_empty(), "the rest still reads");
        v["show_all"] = serde_json::json!("yes");
        v["moved"] = serde_json::json!({"damaged": [1, 2, 3]});
        let read: LabelClass = serde_json::from_str(&v.to_string()).unwrap();
        assert!(!read.show_all && read.moved.is_empty(), "damaged optional fields leave the class usable");
        // None moved: none saved.
        let none = serde_json::to_value(LabelClass::default_for(&ds)).unwrap();
        assert!(none.get("moved").is_none());
    }

    #[test]
    fn class_labels_are_saved_and_older_renderers_still_load() {
        let g = GeometryType::Point;
        let s = |c| Symbol::default_for(g, c);
        // A renderer nobody edited saves as before #314, so older versions still read it.
        let mut r = unique(&[("a", s(0))], s(1), Ramp::Ember);
        let j = serde_json::to_string(&r).unwrap();
        assert!(!j.contains("class_labels") && !j.contains("other_label"), "{j}");
        assert_eq!(serde_json::from_str::<Renderer>(&j).unwrap(), r);
        r.set_class_style(&ClassRef::Value("a".into()), &ClassStyle { label: Some("A".into()), ..Default::default() }, g).unwrap();
        r.set_class_style(&ClassRef::Other, &ClassStyle { label: Some("Rest".into()), ..Default::default() }, g).unwrap();
        assert_eq!(serde_json::from_str::<Renderer>(&serde_json::to_string(&r).unwrap()).unwrap(), r);
        // Class numbers round-trip as JSON object keys.
        let mut gr = graduated(&[1.0, 2.0], &[s(0), s(1)], Ramp::Ember);
        gr.set_class_style(&ClassRef::Index(1), &ClassStyle { label: Some("Two".into()), ..Default::default() }, g).unwrap();
        assert_eq!(serde_json::from_str::<Renderer>(&serde_json::to_string(&gr).unwrap()).unwrap(), gr);
    }

    #[test]
    fn applying_the_symbology_again_keeps_hand_edits() {
        let g = GeometryType::Polygon;
        let base = Symbol::default_for(g, 0);
        // Classes coloured by their scheme, as Renderer::unique and Renderer::graduated make them.
        let ramped = |keys: &[&str], ramp: Ramp| {
            let n = keys.len();
            let classes: Vec<(&str, Symbol)> = keys.iter().enumerate().map(|(k, v)| (*v, base.with_fill(ramp.pick(k, n), g))).collect();
            unique(&classes, base.with_fill(OTHER_FILL, g), ramp)
        };
        let (red, blue) = (Rgba::rgb(0xC0, 0x30, 0x20), Rgba::rgb(0, 0, 0xFF));
        let edit = |r: &mut Renderer, c: ClassRef, color: Option<Rgba>, outline: Option<Rgba>, label: Option<&str>| {
            r.set_class_style(&c, &ClassStyle { color, outline, label: label.map(str::to_string), ..Default::default() }, g).unwrap();
        };
        let mut old = ramped(&["a", "b", "c"], Ramp::Ember);
        edit(&mut old, ClassRef::Value("b".into()), Some(red), None, Some("Bee"));
        edit(&mut old, ClassRef::Value("c".into()), None, Some(blue), None);
        edit(&mut old, ClassRef::Other, Some(red), None, Some("Rest"));
        // Same field and scheme, "a" gone and "d" new: only the hand edits carry over. "c" keeps
        // its outline but takes the scheme's colour at its new place, not the one it had.
        let fresh = ramped(&["b", "c", "d"], Ramp::Ember);
        let mut again = fresh.clone();
        again.carry_edits_from(&old, g);
        let (Renderer::Unique { classes, other, .. }, Renderer::Unique { classes: fc, .. }) = (&again, &fresh) else {
            panic!("unique renderers expected")
        };
        assert_eq!((classes[0].1.fill, classes[0].1.stroke), (red, red.shade(0.6)), "b keeps its colour");
        assert_eq!((classes[1].1.fill, classes[1].1.stroke), (fc[1].1.fill, blue), "c keeps its outline only");
        assert_eq!(classes[2].1, fc[2].1, "d is new");
        assert_eq!((other.fill, other.stroke), (red, red.shade(0.6)));
        assert_eq!(labels_of(&again), ["Bee", "c", "d", "Rest"]);
        // A polygon class's own transparency, and an outline that differs only in it, carry over too.
        let mut tinted = ramped(&["a"], Ramp::Ember);
        if let Renderer::Unique { classes, .. } = &mut tinted {
            classes[0].1.fill = red.with_alpha(80);
            classes[0].1.stroke = red.shade(0.6).with_alpha(100);
        }
        let mut kept = ramped(&["a"], Ramp::Ember);
        kept.carry_edits_from(&tinted, g);
        let (Renderer::Unique { classes: kc, .. }, Renderer::Unique { classes: tc, .. }) = (&kept, &tinted) else {
            panic!("unique renderers expected")
        };
        assert_eq!((kc[0].1.fill, kc[0].1.stroke), (tc[0].1.fill, tc[0].1.stroke));
        // An unedited renderer applied again is the fresh one.
        let mut plain = fresh.clone();
        plain.carry_edits_from(&ramped(&["b", "c"], Ramp::Ember), g);
        assert_eq!(plain, fresh);
        // Another scheme recolours but keeps the labels; another field starts fresh.
        let fresh = ramped(&["b"], Ramp::Harbour);
        let mut recoloured = fresh.clone();
        recoloured.carry_edits_from(&old, g);
        let (Renderer::Unique { classes, other, .. }, Renderer::Unique { classes: fc, other: fo, .. }) = (&recoloured, &fresh) else {
            panic!("unique renderers expected")
        };
        assert_eq!((classes, other), (fc, fo));
        assert_eq!(labels_of(&recoloured), ["Bee", "Rest"]);
        let mut other_field = ramped(&["b"], Ramp::Ember);
        if let Renderer::Unique { field, .. } = &mut other_field {
            *field = "h".into();
        }
        let fresh = other_field.clone();
        other_field.carry_edits_from(&old, g);
        assert_eq!(other_field, fresh);
        // Graduated Colors: edits by number while the count and scheme stay; labels while the ranges do.
        let ramped = |breaks: &[f64], ramp: Ramp| {
            let n = breaks.len();
            graduated(breaks, &(0..n).map(|i| base.with_fill(ramp.pick(i, n), g)).collect::<Vec<_>>(), ramp)
        };
        let mut gold = ramped(&[10.0, 20.0], Ramp::Ember);
        edit(&mut gold, ClassRef::Index(0), Some(red), None, Some("Low"));
        let mut same = ramped(&[10.0, 20.0], Ramp::Ember);
        same.carry_edits_from(&gold, g);
        assert_eq!(same, gold);
        let fresh = ramped(&[12.0, 20.0], Ramp::Ember);
        let mut moved = fresh.clone();
        moved.carry_edits_from(&gold, g);
        let (Renderer::Graduated { symbols, .. }, Renderer::Graduated { symbols: fs, .. }) = (&moved, &fresh) else {
            panic!("graduated renderers expected")
        };
        assert_eq!((symbols[0], symbols[1]), (fs[0], fs[1]), "the range moved: its colour goes");
        assert_eq!(labels_of(&moved), ["0 – 12", "12 – 20"], "and its label too");
        let fresh = ramped(&[10.0, 15.0, 20.0], Ramp::Ember);
        let mut more = fresh.clone();
        more.carry_edits_from(&gold, g);
        assert_eq!(more, fresh, "another number of classes starts fresh");
    }

    fn unique_with(values: &[(&str, u8)]) -> Renderer {
        let sym = |v: u8| Symbol { fill: Rgba::rgb(v, 0, 0), ..Symbol::default_for(GeometryType::Point, 0) };
        Renderer::Unique {
            field: "f".into(),
            classes: values.iter().map(|(k, v)| (k.to_string(), sym(*v))).collect(),
            other: sym(0),
            ramp: Ramp::Greys,
            class_labels: BTreeMap::new(),
            other_label: None,
            class_strokes: BTreeMap::new(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        }
    }

    /// #1355: Invert Color Ramp reverses the colours along the legend's order, which is not the
    /// order the classes are stored in; a value repeated in the classes isn't drawn, so it takes
    /// no part; and a class with a stack of its own keeps its colours without shifting the rest.
    #[test]
    fn reverse_ramp_follows_the_legend_and_skips_what_is_not_drawn() {
        let reds = |r: &Renderer| match r {
            Renderer::Unique { classes, .. } => classes.iter().map(|c| (c.0.clone(), c.1.fill.r())).collect::<Vec<_>>(),
            _ => vec![],
        };
        let g = GeometryType::Point;
        // Stored 10, 100, 9: the legend lists 9, 10, 100 (numbers), so 9 and 100 swap colours.
        let mut r = unique_with(&[("10", 1), ("100", 2), ("9", 3)]);
        assert_eq!(r.reverse_ramp(g), Some(RampReversal { reversed: 3, kept: 0, on_layer_stack: 3 }));
        assert_eq!(reds(&r), vec![("10".into(), 1), ("100".into(), 3), ("9".into(), 2)]);
        assert!(matches!(&r, Renderer::Unique { other, .. } if other.fill.r() == 0), "<all other values> is no ramp color");
        r.reverse_ramp(g);
        assert_eq!(reds(&r), vec![("10".into(), 1), ("100".into(), 2), ("9".into(), 3)], "twice is the original");
        // A repeat of a value is not drawn: it stays, and the drawn entries reverse among themselves.
        let mut r = unique_with(&[("a", 1), ("b", 2), ("a", 9), ("c", 3)]);
        r.reverse_ramp(g);
        assert_eq!(reds(&r), vec![("a".into(), 3), ("b".into(), 2), ("a".into(), 9), ("c".into(), 1)]);
        // A class with a stack of its own is kept, counted, and the others reverse among themselves.
        let mut r = unique_with(&[("a", 1), ("b", 2), ("c", 3), ("d", 4)]);
        if let Renderer::Unique { class_strokes, .. } = &mut r {
            class_strokes.insert("a".into(), vec![LineStroke { color: Some(Rgba::rgb(9, 9, 9)), ..LineStroke::default() }]);
        }
        assert_eq!(r.reverse_ramp(g), Some(RampReversal { reversed: 3, kept: 1, on_layer_stack: 3 }));
        assert_eq!(reds(&r), vec![("a".into(), 1), ("b".into(), 4), ("c".into(), 3), ("d".into(), 2)]);
        // A point's hand-picked outline travels with its fill.
        let mut r = unique_with(&[("a", 1), ("b", 2)]);
        if let Renderer::Unique { classes, .. } = &mut r
            && let Some(c) = classes.first_mut()
        {
            c.1.stroke = Rgba::rgb(200, 0, 0);
        }
        r.reverse_ramp(g);
        let strokes = |r: &Renderer| match r {
            Renderer::Unique { classes, .. } => classes.iter().map(|c| (c.1.fill.r(), c.1.stroke.r())).collect::<Vec<_>>(),
            _ => vec![],
        };
        assert_eq!(strokes(&r), vec![(2, 255), (1, 200)]);
        r.reverse_ramp(g);
        assert_eq!(strokes(&r), vec![(1, 200), (2, 255)], "twice is the original");
        // A lone class has no other to change places with.
        assert_eq!(unique_with(&[("a", 1)]).reverse_ramp(g), Some(RampReversal { reversed: 0, kept: 0, on_layer_stack: 0 }));
        // Nothing to reverse for a single symbol.
        assert_eq!(Renderer::Single(Symbol::default_for(g, 0)).reverse_ramp(g), None);
    }

    /// #1355: which stack replaces a class's symbol depends on what the layer draws: a polygon's
    /// own fills, a line's or a point's own strokes. The other kind is not drawn, and an empty
    /// stack is no stack, so those classes take part in the reversal.
    #[test]
    fn reverse_ramp_keeps_only_the_classes_whose_stack_is_drawn() {
        let reds = |r: &Renderer| match r {
            Renderer::Unique { classes, .. } => classes.iter().map(|c| c.1.fill.r()).collect::<Vec<_>>(),
            _ => vec![],
        };
        let with = |fills: bool, strokes: bool, empty: bool| {
            let mut r = unique_with(&[("a", 1), ("b", 2), ("c", 3)]);
            if let Renderer::Unique { class_fills, class_strokes, .. } = &mut r {
                let own = Some(Rgba::rgb(9, 9, 9));
                if fills {
                    class_fills.insert("a".into(), if empty { vec![] } else { vec![FillLayer { color: own, ..FillLayer::default() }] });
                }
                if strokes {
                    class_strokes.insert("a".into(), if empty { vec![] } else { vec![LineStroke { color: own, ..LineStroke::default() }] });
                }
            }
            r
        };
        let (kept, reversed) = (vec![1, 3, 2], vec![3, 2, 1]);
        // (stacks of class a, geometry) -> the classes that reverse, and how many are kept.
        for (fills, strokes, empty, g, take_part) in [
            (true, false, false, GeometryType::Polygon, false),
            (false, true, false, GeometryType::Polygon, true),
            (false, true, false, GeometryType::Polyline, false),
            (true, false, false, GeometryType::Polyline, true),
            (false, true, false, GeometryType::Point, false),
            (true, false, false, GeometryType::Point, true),
            (true, true, true, GeometryType::Polygon, true),
            (true, true, true, GeometryType::Polyline, true),
        ] {
            let mut r = with(fills, strokes, empty);
            let n = r.reverse_ramp(g).map(|x| x.kept);
            let case = format!("fills {fills}, strokes {strokes}, empty {empty}, {g:?}");
            if take_part {
                assert_eq!(n, Some(0), "{case}");
                assert_eq!(reds(&r), reversed, "{case}");
            } else {
                assert_eq!(n, Some(1), "{case}");
                assert_eq!(reds(&r), kept, "{case}");
            }
        }
    }

    /// #1355: a stack layer without a color of its own takes the symbol's, so the class still shows
    /// its ramp color through it and takes part in the reversal: a road's black casing over a core
    /// that follows the class. Only a stack whose every layer has its own color hides the symbol.
    #[test]
    fn reverse_ramp_reverses_the_classes_a_stack_inherits_from() {
        let reds = |r: &Renderer| match r {
            Renderer::Unique { classes, .. } => classes.iter().map(|c| c.1.fill.r()).collect::<Vec<_>>(),
            _ => vec![],
        };
        let own = Some(Rgba::rgb(9, 9, 9));
        let casing = LineStroke { color: own, width: Some(6.0), ..LineStroke::default() };
        let core = LineStroke::default();
        let (solid_own, solid_inherit) = (FillLayer { color: own, ..FillLayer::default() }, FillLayer::default());
        for (stack_of_strokes, g, hides) in [
            (vec![casing], GeometryType::Polyline, true),
            (vec![casing, core], GeometryType::Polyline, false),
            (vec![core], GeometryType::Polyline, false),
            (vec![casing], GeometryType::Point, true),
            (vec![casing, core], GeometryType::Point, false),
        ] {
            let mut r = unique_with(&[("a", 1), ("b", 2), ("c", 3)]);
            if let Renderer::Unique { class_strokes, .. } = &mut r {
                class_strokes.insert("a".into(), stack_of_strokes.clone());
            }
            let n = r.reverse_ramp(g).map(|x| x.kept);
            let case = format!("{} strokes, {g:?}", stack_of_strokes.len());
            assert_eq!((n, reds(&r)), if hides { (Some(1), vec![1, 3, 2]) } else { (Some(0), vec![3, 2, 1]) }, "{case}");
        }
        for (fills, hides) in [(vec![solid_own], true), (vec![solid_own, solid_inherit], false), (vec![solid_inherit], false)] {
            let mut r = unique_with(&[("a", 1), ("b", 2), ("c", 3)]);
            if let Renderer::Unique { class_fills, .. } = &mut r {
                class_fills.insert("a".into(), fills.clone());
            }
            let n = r.reverse_ramp(GeometryType::Polygon).map(|x| x.kept);
            let case = format!("{} fills", fills.len());
            assert_eq!((n, reds(&r)), if hides { (Some(1), vec![1, 3, 2]) } else { (Some(0), vec![3, 2, 1]) }, "{case}");
        }
    }
}
