//! Page layouts: a page (in millimetres) holding map frames and the surrounds that explain them
//! (north arrows, scale bars, legends, text, pictures, tables, charts), exported as images or PDF.
//!
//! Element positions are in millimetres from the page's top-left corner. A map frame shows a map
//! of the project at its own centre and scale, independent of the map view.

use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};

use crate::raster::{Colorizer, Normalizer, RasterLayer, RasterMask, RasterRenderer, RasterStyle};
use crate::{Camera, Layer, Map, Rgba};

/// Logical points per millimetre (96 points per inch).
pub const PT_PER_MM: f64 = 96.0 / 25.4;
/// Metres of paper per logical point.
pub const PAPER_M_PER_PT: f64 = 0.0254 / 96.0;

/// The largest a map frame's scale can be: 2:1, larger than life for finds drawings (#902). Even
/// there its camera ([`frame_camera`]) stays above the map's closest zoom
/// ([`crate::camera::MIN_RES`]) anywhere on Web Mercator.
pub const MIN_FRAME_SCALE: f64 = 0.5;
/// The smallest: the whole world many times over.
pub const MAX_FRAME_SCALE: f64 = 1e10;

/// The tokens a layout's text expands besides the map's ([`crate::graphics::TOKENS`], from its
/// first map frame), with what each shows.
pub const PAGE_TOKENS: &[(&str, &str)] =
    &[("{page}", "Page number"), ("{pages}", "Number of pages"), ("{pageName}", "Page name"), ("{layout}", "Layout name")];

/// The smallest element side, in mm: thin enough for a hairline rule (a 0.05 mm rectangle).
pub const MIN_ELEMENT_MM: f64 = 0.05;

/// The largest element id a layout keeps (imported ones above it are renumbered).
const MAX_ID: u32 = 1_000_000;

/// Page sizes: (id, label, width mm, height mm), portrait.
pub const PAGES: &[(&str, &str, f64, f64)] = &[
    ("letter", "Letter", 215.9, 279.4),
    ("legal", "Legal", 215.9, 355.6),
    ("tabloid", "Tabloid", 279.4, 431.8),
    ("a5", "A5", 148.0, 210.0),
    ("a4", "A4", 210.0, 297.0),
    ("a3", "A3", 297.0, 420.0),
    ("a2", "A2", 420.0, 594.0),
    ("a1", "A1", 594.0, 841.0),
    ("a0", "A0", 841.0, 1189.0),
];

/// The outline of a map frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum FrameShape {
    #[default]
    Rectangle,
    Circle,
    Ellipse,
    /// A polygon in the frame's unit square (0..1 from its top-left), drawn by Polygon or Lasso.
    Polygon(Vec<(f64, f64)>),
}

impl FrameShape {
    /// The outline as points in the frame's unit square.
    pub fn outline(&self) -> Vec<(f64, f64)> {
        match self {
            FrameShape::Rectangle => vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            FrameShape::Circle | FrameShape::Ellipse => (0..96)
                .map(|i| {
                    let t = i as f64 / 96.0 * std::f64::consts::TAU;
                    (0.5 + 0.5 * t.cos(), 0.5 + 0.5 * t.sin())
                })
                .collect(),
            FrameShape::Polygon(p) => p.clone(),
        }
    }
}

/// A grid drawn over a map frame, labelled at the frame's edges: a graticule of latitudes and
/// longitudes, or (as ArcGIS Pro's measured grid) eastings and northings in a projected
/// coordinate system.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    /// Between lines: degrees for a graticule, the coordinate system's units (metres) for a
    /// measured grid; 0 picks a step from the frame's extent.
    #[serde(default)]
    pub step: f64,
    pub color: Rgba,
    #[serde(default = "yes")]
    pub labels: bool,
    /// Lines across the frame, ticks along its edges (the neatline), or both.
    #[serde(default)]
    pub style: GridStyle,
    /// Length of the edge ticks, in mm.
    #[serde(default = "tick_length")]
    pub tick_length: f64,
    /// Width of the lines and ticks, in points.
    #[serde(default = "grid_width")]
    pub width: f64,
    #[serde(default)]
    pub format: CoordFormat,
    /// Label with a hemisphere letter (`112.5°W`); off, with a sign (`-112.5°`).
    #[serde(default = "yes")]
    pub hemisphere: bool,
    /// The edges that carry labels: longitudes along the top and bottom, latitudes along the left
    /// and right.
    #[serde(default)]
    pub edges: GridEdges,
    /// Vertical runs the left and right labels along their edge.
    #[serde(default)]
    pub label_orientation: Orientation,
    /// A graticule, or a measured grid.
    #[serde(default, skip_serializing_if = "GridKind::is_graticule")]
    pub kind: GridKind,
    /// A measured grid's coordinate system (`EPSG:25832`, WKT or a PROJ string): none is the map's
    /// own when it is projected, else the UTM zone (WGS 84) at the frame's centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crs: Option<String>,
    /// A measured grid's labels shortened as topographic maps print them: the kilometres' last two
    /// digits (`43` for 543,000 m); off, the whole value.
    #[serde(default, skip_serializing_if = "is_false")]
    pub short: bool,
}

/// What a grid's lines are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GridKind {
    /// Latitudes and longitudes.
    #[default]
    Graticule,
    /// Eastings and northings in a projected coordinate system, its units apart.
    Measured,
}

impl GridKind {
    pub const ALL: [GridKind; 2] = [GridKind::Graticule, GridKind::Measured];

    pub fn code(self) -> &'static str {
        match self {
            GridKind::Graticule => "graticule",
            GridKind::Measured => "measured",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GridKind::Graticule => "Graticule",
            GridKind::Measured => "Measured grid",
        }
    }

    pub fn from_name(s: &str) -> Option<GridKind> {
        GridKind::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }

    fn is_graticule(&self) -> bool {
        *self == GridKind::Graticule
    }
}

/// The widest step of a measured grid, in its units.
pub const MAX_MEASURED_STEP: f64 = 10_000_000.0;
/// The narrowest step of a measured grid, in its units (a metre).
pub const MIN_MEASURED_STEP: f64 = 1.0;
/// The longest coordinate system a measured grid keeps (a WKT's characters, counted as such).
pub const MAX_GRID_CRS: usize = 4096;
/// The widest step of a graticule, in degrees.
pub const MAX_GRATICULE_STEP: f64 = 90.0;

/// The WGS 84 UTM zone's EPSG code at a point (32601–32660 north of the Equator, 32701–32760
/// south): a measured grid's coordinate system when neither it nor the map names one.
pub fn utm_epsg(lon: f64, lat: f64) -> u32 {
    // (A longitude past ±180°, on a copy of the world, wraps round.)
    let zone = if lon.is_finite() { ((lon + 180.0).rem_euclid(360.0) / 6.0).floor().clamp(0.0, 59.0) as u32 + 1 } else { 31 };
    if lat < 0.0 { 32_700 + zone } else { 32_600 + zone }
}

impl Default for Grid {
    fn default() -> Grid {
        Grid {
            step: 0.0,
            color: Rgba::new(0x40, 0x48, 0x55, 150),
            labels: true,
            style: GridStyle::default(),
            tick_length: tick_length(),
            width: grid_width(),
            format: CoordFormat::default(),
            hemisphere: true,
            edges: GridEdges::default(),
            label_orientation: Orientation::default(),
            kind: GridKind::Graticule,
            crs: None,
            short: false,
        }
    }
}

/// The longest edge tick a grid draws, in mm.
pub const MAX_TICK_MM: f64 = 20.0;
/// The widest grid line, in points.
pub const MAX_GRID_WIDTH: f64 = 10.0;

fn tick_length() -> f64 {
    2.0
}

fn grid_width() -> f64 {
    0.6
}

impl Grid {
    /// A graticule label: `v` degrees of longitude (`lon`) or latitude, on lines `step` apart.
    ///
    /// The Equator and prime meridian are `0°` (neither hemisphere), and so is the antimeridian
    /// (`180°`). Decimal degrees show as many decimals as the step needs; degrees, minutes and
    /// seconds leave out the minutes and seconds the step doesn't need.
    pub fn label(&self, v: f64, lon: bool, step: f64) -> String {
        // A stepped loop accumulates `+= step`, so 0 and ±180 can be off by rounding error.
        let eps = step.abs().max(1e-9) * 1e-6;
        let a = v.abs();
        let d = match self.format {
            CoordFormat::Decimal => format!("{}°", decimal(a, step)),
            CoordFormat::Dms => dms(a, step),
        };
        let unsigned = a < eps || (a - 180.0).abs() < eps;
        if unsigned {
            d
        } else if self.hemisphere {
            let letter = match (lon, v > 0.0) {
                (true, true) => 'E',
                (true, false) => 'W',
                (false, true) => 'N',
                (false, false) => 'S',
            };
            format!("{d}{letter}")
        } else if v < 0.0 {
            format!("-{d}")
        } else {
            d
        }
    }
}

impl Grid {
    /// A measured grid's label: easting or northing `v` on lines `step` apart, whole (`543000`),
    /// or [`short`](Grid::short) its kilometres' last two digits (`43`, `43.5` for a 500 m step).
    pub fn measured_label(&self, v: f64, step: f64) -> String {
        // Off by rounding error from the stepped loop: put back on the step.
        let v = if step > 0.0 { (v / step).round() * step } else { v };
        // (Short labels of a step of 100 km or more would all read 0: whole ones.)
        if self.short && step < 100_000.0 {
            let km = (v / 1000.0).rem_euclid(100.0);
            // (99.9999… after rounding error is 0 again.)
            let km = if (km - 100.0).abs() < 1e-6 { 0.0 } else { km };
            return decimal(km, step / 1000.0);
        }
        let s = decimal(v.abs(), step);
        if v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') { format!("-{s}") } else { s }
    }
}

/// `a` with the fewest decimals (up to 4) that show a multiple of `step` exactly.
fn decimal(a: f64, step: f64) -> String {
    let places = (0..4usize).find(|&n| {
        let k = step * 10f64.powi(n as i32);
        (k - k.round()).abs() < 1e-6
    });
    format!("{a:.prec$}", prec = places.unwrap_or(4))
}

/// `a` degrees as `12°30′15″`, leaving out the minutes and seconds `step` doesn't need.
fn dms(a: f64, step: f64) -> String {
    let secs = (a * 3600.0).round();
    if !secs.is_finite() {
        return "0°".into();
    }
    let secs = secs as u64;
    let (d, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let step_secs = (step.abs() * 3600.0).round() as u64;
    if step_secs.is_multiple_of(3600) && m == 0 && s == 0 {
        format!("{d}°")
    } else if step_secs.is_multiple_of(60) && s == 0 {
        format!("{d}°{m:02}′")
    } else {
        format!("{d}°{m:02}′{s:02}″")
    }
}

/// How a grid is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridStyle {
    /// Lines across the frame.
    #[default]
    Lines,
    /// Short ticks along the frame's edges only.
    Ticks,
    /// Lines across the frame and ticks along its edges.
    LinesAndTicks,
}

impl GridStyle {
    pub const ALL: [GridStyle; 3] = [GridStyle::Lines, GridStyle::Ticks, GridStyle::LinesAndTicks];

    pub fn code(self) -> &'static str {
        match self {
            GridStyle::Lines => "lines",
            GridStyle::Ticks => "ticks",
            GridStyle::LinesAndTicks => "linesAndTicks",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GridStyle::Lines => "Full lines",
            GridStyle::Ticks => "Ticks only",
            GridStyle::LinesAndTicks => "Lines and ticks",
        }
    }

    pub fn from_name(s: &str) -> Option<GridStyle> {
        GridStyle::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }

    pub fn lines(self) -> bool {
        self != GridStyle::Ticks
    }

    pub fn ticks(self) -> bool {
        self != GridStyle::Lines
    }
}

/// How grid labels write a coordinate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoordFormat {
    /// Decimal degrees: `112.5°`.
    #[default]
    Decimal,
    /// Degrees, minutes and seconds: `112°30′`.
    Dms,
}

impl CoordFormat {
    pub fn from_name(s: &str) -> Option<CoordFormat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "decimal" | "dd" => Some(CoordFormat::Decimal),
            "dms" => Some(CoordFormat::Dms),
            _ => None,
        }
    }
}

/// The edges of a map frame that carry grid labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridEdges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl Default for GridEdges {
    /// The bottom and left edges, as grids were labelled before edges could be picked.
    fn default() -> GridEdges {
        GridEdges { top: false, bottom: true, left: true, right: false }
    }
}

/// Where a text element's lines sit across it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub const ALL: [TextAlign; 3] = [TextAlign::Left, TextAlign::Center, TextAlign::Right];

    pub fn code(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TextAlign::Left => "Left",
            TextAlign::Center => "Center",
            TextAlign::Right => "Right",
        }
    }

    pub fn from_name(s: &str) -> Option<TextAlign> {
        let s = s.trim();
        TextAlign::ALL
            .into_iter()
            .find(|v| v.code().eq_ignore_ascii_case(s))
            .or(s.eq_ignore_ascii_case("centre").then_some(TextAlign::Center))
    }
}

/// The family a text element is drawn in (#517): a generic one, not a file, so a project opens
/// anywhere and each machine draws it in a face it has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontFamily {
    /// The interface's sans-serif. Saved as the other layout enums are (`Sans`, as `TextAlign`
    /// saves `Center`); the command's lowercase names are read too, so a layout written by hand
    /// with `"font": "sans"` imports.
    #[default]
    #[serde(alias = "sans", alias = "sans-serif")]
    Sans,
    /// An installed serif (Georgia, Times, Noto Serif…); the sans where there is none.
    #[serde(alias = "serif")]
    Serif,
}

impl FontFamily {
    pub const ALL: [FontFamily; 2] = [FontFamily::Sans, FontFamily::Serif];

    pub fn code(self) -> &'static str {
        match self {
            FontFamily::Sans => "sans",
            FontFamily::Serif => "serif",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FontFamily::Sans => "Sans-serif",
            FontFamily::Serif => "Serif",
        }
    }

    /// `sans` or `serif` (`sans-serif` too), in any case.
    pub fn from_name(s: &str) -> Option<FontFamily> {
        let s = s.trim();
        FontFamily::ALL
            .into_iter()
            .find(|v| v.code().eq_ignore_ascii_case(s))
            .or(s.eq_ignore_ascii_case("sans-serif").then_some(FontFamily::Sans))
    }
}

fn is_sans(f: &FontFamily) -> bool {
    *f == FontFamily::Sans
}

/// The most letter spacing a text element takes, either way (points).
pub const MAX_LETTER_SPACING: f64 = 50.0;
pub const MIN_LETTER_SPACING: f64 = -5.0;

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_left(a: &TextAlign) -> bool {
    *a == TextAlign::Left
}

/// The most layers a legend leaves out.
pub const MAX_LEGEND_EXCLUDED: usize = 1000;

/// Which way a line element runs across its box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineDir {
    /// Across the middle of the box, left to right.
    #[default]
    Horizontal,
    /// Down the middle of the box.
    Vertical,
    /// From the top-left corner to the bottom-right.
    Falling,
    /// From the bottom-left corner to the top-right.
    Rising,
}

impl LineDir {
    pub const ALL: [LineDir; 4] = [LineDir::Horizontal, LineDir::Vertical, LineDir::Falling, LineDir::Rising];

    pub fn code(self) -> &'static str {
        match self {
            LineDir::Horizontal => "horizontal",
            LineDir::Vertical => "vertical",
            LineDir::Falling => "falling",
            LineDir::Rising => "rising",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LineDir::Horizontal => "Horizontal",
            LineDir::Vertical => "Vertical",
            LineDir::Falling => "Diagonal ↘",
            LineDir::Rising => "Diagonal ↗",
        }
    }

    pub fn from_name(s: &str) -> Option<LineDir> {
        LineDir::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }

    /// The line's ends in a box (x, y, w, h).
    pub fn ends(self, (x, y, w, h): (f64, f64, f64, f64)) -> [(f64, f64); 2] {
        match self {
            LineDir::Horizontal => [(x, y + h / 2.0), (x + w, y + h / 2.0)],
            LineDir::Vertical => [(x + w / 2.0, y), (x + w / 2.0, y + h)],
            LineDir::Falling => [(x, y), (x + w, y + h)],
            LineDir::Rising => [(x, y + h), (x + w, y)],
        }
    }
}

/// Where [`Layout::arrange`] moves an element in the drawing order, in which later elements are
/// drawn over earlier ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    /// Over every other element.
    Front,
    /// Over the element above it.
    Forward,
    /// Under the element below it.
    Backward,
    /// Under every other element.
    Back,
}

impl Arrange {
    pub const ALL: [Arrange; 4] = [Arrange::Front, Arrange::Forward, Arrange::Backward, Arrange::Back];

    pub fn code(self) -> &'static str {
        match self {
            Arrange::Front => "front",
            Arrange::Forward => "forward",
            Arrange::Backward => "backward",
            Arrange::Back => "back",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Arrange::Front => "Bring to Front",
            Arrange::Forward => "Bring Forward",
            Arrange::Backward => "Send Backward",
            Arrange::Back => "Send to Back",
        }
    }

    pub fn from_name(s: &str) -> Option<Arrange> {
        Arrange::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// Which ends of a line element carry an arrowhead: of the ends [`LineDir::ends`] gives, the
/// start is the first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineHead {
    /// A plain line.
    #[default]
    None,
    /// An arrow pointing at its end.
    End,
    /// An arrow pointing at its start.
    Start,
    /// A head at both ends.
    Both,
}

impl LineHead {
    pub const ALL: [LineHead; 4] = [LineHead::None, LineHead::End, LineHead::Start, LineHead::Both];

    pub fn code(self) -> &'static str {
        match self {
            LineHead::None => "none",
            LineHead::End => "end",
            LineHead::Start => "start",
            LineHead::Both => "both",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LineHead::None => "None",
            LineHead::End => "At the end",
            LineHead::Start => "At the start",
            LineHead::Both => "Both ends",
        }
    }

    pub fn from_name(s: &str) -> Option<LineHead> {
        LineHead::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// How far across (mm) a level or upright line's box is, to pick it up by.
pub const LINE_BOX_MM: f64 = 6.0;

/// A line's direction in degrees anticlockwise from pointing right (90 points up the page), from
/// its tail to its tip: towards its end, unless only its start has a head.
pub fn line_angle(r: (f64, f64, f64, f64), dir: LineDir, head: LineHead) -> f64 {
    let [a, b] = dir.ends(r);
    let (tail, tip) = if head == LineHead::Start { (b, a) } else { (a, b) };
    // The page's y grows downwards.
    (tail.1 - tip.1).atan2(tip.0 - tail.0).to_degrees()
}

/// A line's length (mm) in its box.
pub fn line_length(r: (f64, f64, f64, f64), dir: LineDir) -> f64 {
    let [a, b] = dir.ends(r);
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// The box, direction and heads that draw a line `length` mm long at `angle` degrees (as
/// [`line_angle`]) centred on `center`. A single head moves to the end it must be at to keep
/// pointing that way; a nearly level or upright line gets a box [`LINE_BOX_MM`] across.
pub fn line_at_angle(center: (f64, f64), length: f64, angle: f64, head: LineHead) -> ((f64, f64, f64, f64), LineDir, LineHead) {
    let length = finite_or(length, 10.0).clamp(MIN_ELEMENT_MM, 5000.0);
    let a = finite_or(angle, 0.0).to_radians();
    // From tail to tip, on the page.
    let (dx, dy) = (length * a.cos(), -length * a.sin());
    const LEVEL: f64 = 0.25;
    // `forward`: the tip is at the end the direction draws last.
    let (dir, forward) = if dy.abs() < LEVEL {
        (LineDir::Horizontal, dx >= 0.0)
    } else if dx.abs() < LEVEL {
        (LineDir::Vertical, dy >= 0.0)
    } else if (dx > 0.0) == (dy > 0.0) {
        (LineDir::Falling, dx > 0.0)
    } else {
        (LineDir::Rising, dx > 0.0)
    };
    let head = match head {
        LineHead::End | LineHead::Start => {
            if forward {
                LineHead::End
            } else {
                LineHead::Start
            }
        }
        h => h,
    };
    let (w, h) = match dir {
        LineDir::Horizontal => (dx.abs(), LINE_BOX_MM),
        LineDir::Vertical => (LINE_BOX_MM, dy.abs()),
        LineDir::Falling | LineDir::Rising => (dx.abs(), dy.abs()),
    };
    let (w, h) = (w.max(MIN_ELEMENT_MM), h.max(MIN_ELEMENT_MM));
    ((center.0 - w / 2.0, center.1 - h / 2.0, w, h), dir, head)
}

/// A north arrow's look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArrowStyle {
    /// A split arrowhead, one half filled.
    #[default]
    Classic,
    /// A four-point compass rose.
    Compass,
    /// A plain filled triangle.
    Triangle,
    /// An eight-point star.
    Star,
}

impl ArrowStyle {
    pub const ALL: [ArrowStyle; 4] = [ArrowStyle::Classic, ArrowStyle::Compass, ArrowStyle::Triangle, ArrowStyle::Star];

    pub fn code(self) -> &'static str {
        match self {
            ArrowStyle::Classic => "classic",
            ArrowStyle::Compass => "compass",
            ArrowStyle::Triangle => "triangle",
            ArrowStyle::Star => "star",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ArrowStyle::Classic => "Classic arrow",
            ArrowStyle::Compass => "Compass rose",
            ArrowStyle::Triangle => "Triangle",
            ArrowStyle::Star => "Star",
        }
    }

    pub fn from_name(s: &str) -> Option<ArrowStyle> {
        ArrowStyle::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// The units a scale bar counts in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleUnits {
    /// Millimetres, centimetres, metres or kilometres, whichever reads better at the frame's scale.
    #[default]
    Auto,
    Kilometers,
    Meters,
    Miles,
    Feet,
}

impl ScaleUnits {
    pub const ALL: [ScaleUnits; 5] = [ScaleUnits::Auto, ScaleUnits::Kilometers, ScaleUnits::Meters, ScaleUnits::Miles, ScaleUnits::Feet];

    pub fn code(self) -> &'static str {
        match self {
            ScaleUnits::Auto => "auto",
            ScaleUnits::Kilometers => "kilometers",
            ScaleUnits::Meters => "meters",
            ScaleUnits::Miles => "miles",
            ScaleUnits::Feet => "feet",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScaleUnits::Auto => "Automatic (mm to km)",
            ScaleUnits::Kilometers => "Kilometers",
            ScaleUnits::Meters => "Meters",
            ScaleUnits::Miles => "Miles",
            ScaleUnits::Feet => "Feet",
        }
    }

    pub fn from_name(s: &str) -> Option<ScaleUnits> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(ScaleUnits::Auto),
            "kilometers" | "kilometres" | "km" => Some(ScaleUnits::Kilometers),
            "meters" | "metres" | "m" => Some(ScaleUnits::Meters),
            "miles" | "mi" => Some(ScaleUnits::Miles),
            "feet" | "ft" => Some(ScaleUnits::Feet),
            _ => None,
        }
    }

    /// The unit to count `total_m` metres in, and its metres and abbreviation.
    pub fn resolve(self, total_m: f64) -> (f64, &'static str) {
        match self {
            ScaleUnits::Auto if total_m >= 1000.0 => (1000.0, "km"),
            // A plan of a find, larger than life (#902), counts in what's drawn on it.
            ScaleUnits::Auto if total_m < 0.01 => (0.001, "mm"),
            ScaleUnits::Auto if total_m < 1.0 => (0.01, "cm"),
            ScaleUnits::Auto | ScaleUnits::Meters => (1.0, "m"),
            ScaleUnits::Kilometers => (1000.0, "km"),
            ScaleUnits::Miles => (1609.344, "mi"),
            ScaleUnits::Feet => (0.3048, "ft"),
        }
    }
}

/// A scale bar's look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleBarStyle {
    /// A bar of alternating dark and light divisions.
    #[default]
    Alternating,
    /// A line stepping up and down at each division.
    Stepped,
    /// A single line with a tick at each division.
    Line,
}

impl ScaleBarStyle {
    pub const ALL: [ScaleBarStyle; 3] = [ScaleBarStyle::Alternating, ScaleBarStyle::Stepped, ScaleBarStyle::Line];

    pub fn code(self) -> &'static str {
        match self {
            ScaleBarStyle::Alternating => "alternating",
            ScaleBarStyle::Stepped => "stepped",
            ScaleBarStyle::Line => "line",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScaleBarStyle::Alternating => "Alternating bar",
            ScaleBarStyle::Stepped => "Stepped line",
            ScaleBarStyle::Line => "Single line",
        }
    }

    pub fn from_name(s: &str) -> Option<ScaleBarStyle> {
        ScaleBarStyle::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// The most divisions a scale bar has.
pub const MAX_SCALE_DIVISIONS: usize = 10;

fn divisions() -> usize {
    4
}

fn ink() -> Rgba {
    Rgba::rgb(0x22, 0x22, 0x22)
}

fn white() -> Rgba {
    Rgba::rgb(0xFF, 0xFF, 0xFF)
}

fn yes() -> bool {
    true
}

/// Which way a colorbar runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

/// Which side of a colorbar its tick labels sit on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TickSide {
    /// Below a horizontal bar, right of a vertical one.
    #[default]
    End,
    /// Above a horizontal bar, left of a vertical one.
    Start,
}

impl TickSide {
    /// A side by name: `below`/`right`/`end` or `above`/`left`/`start`.
    pub fn from_name(s: &str) -> Option<TickSide> {
        match s.trim().to_ascii_lowercase().as_str() {
            "below" | "right" | "end" | "bottom" => Some(TickSide::End),
            "above" | "left" | "start" | "top" => Some(TickSide::Start),
            _ => None,
        }
    }
}

/// Labelled values a colorbar shows at most.
pub const MAX_COLORBAR_TICKS: usize = 21;

fn ticks() -> usize {
    5
}

/// What an element is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ElementKind {
    MapFrame {
        /// The map shown (by name).
        map: String,
        /// Centre (longitude/latitude) and scale denominator on paper.
        center: Coord,
        scale: f64,
        #[serde(default)]
        shape: FrameShape,
        #[serde(default)]
        grid: Option<Grid>,
        /// Another frame whose extent this one outlines (an extent indicator).
        #[serde(default)]
        extent_of: Option<u32>,
        /// The extent indicator's outline colour and width (points): red and 2 unless set.
        #[serde(default = "indicator_color", skip_serializing_if = "is_indicator_color")]
        extent_color: Rgba,
        #[serde(default = "indicator_width", skip_serializing_if = "is_indicator_width")]
        extent_width: f64,
        /// The map turned to a bearing (degrees clockwise from north) and tilted to a pitch
        /// (degrees from looking straight down, 0 to 85), as the map view is.
        #[serde(default, skip_serializing_if = "is_zero")]
        bearing: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        pitch: f64,
    },
    NorthArrow {
        frame: u32,
        #[serde(default)]
        style: ArrowStyle,
        /// The filled parts.
        #[serde(default = "ink")]
        fill: Rgba,
        /// The outline and the `N`.
        #[serde(default = "ink")]
        outline: Rgba,
    },
    ScaleBar {
        frame: u32,
        #[serde(default)]
        units: ScaleUnits,
        /// Divisions along the bar, 1 to [`MAX_SCALE_DIVISIONS`].
        #[serde(default = "divisions")]
        divisions: usize,
        #[serde(default)]
        style: ScaleBarStyle,
        /// The lines, the alternating bar's dark divisions and the labels.
        #[serde(default = "ink")]
        color: Rgba,
        /// The alternating bar's light divisions.
        #[serde(default = "white")]
        fill: Rgba,
    },
    Legend {
        frame: u32,
        title: String,
        /// List every class of every visible layer. Off (the default), the legend shows what the
        /// frame draws: classes some drawn feature has, `<all other values>` only when used, and
        /// no layer that draws nothing at the frame's scale (#207).
        #[serde(default)]
        all_classes: bool,
        /// Leave `<all other values>` out of the legend, in either mode. Only the legend changes:
        /// the features it stands for are drawn as before.
        #[serde(default)]
        hide_other: bool,
        /// Layers (by name, any case) the legend leaves out though the map draws them: a mask, a
        /// highlight, a basemap-like layer. Empty, it lists every layer it would otherwise.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude: Vec<String>,
    },
    /// The colour scale of a raster layer in a frame's map: a gradient bar for a stretch, steps
    /// for classes, a block per value for unique values and colour tables (#326).
    Colorbar {
        frame: u32,
        /// The raster layer (by name); `None` is the top visible raster of the frame's map.
        #[serde(default)]
        layer: Option<String>,
        /// Drawn beside the bar, e.g. the units: `Elevation (m)`.
        #[serde(default)]
        title: String,
        #[serde(default)]
        orientation: Orientation,
        /// Labelled values along a continuous bar, the ends included (2 labels the minimum and
        /// maximum). Classes and unique values label their own breaks and values.
        #[serde(default = "ticks")]
        ticks: usize,
        #[serde(default)]
        tick_side: TickSide,
        /// Run the other way: low values on the right of a horizontal bar (left by default), at
        /// the top of a vertical one (the bottom by default).
        #[serde(default)]
        reverse: bool,
    },
    /// Text; dynamic tokens (`{page}`, `{date}`, `{scale}`…) expand when drawn.
    Text {
        text: String,
        size: f64,
        color: Rgba,
        #[serde(default)]
        bold: bool,
        /// Slanted, as water and ocean names are.
        #[serde(default, skip_serializing_if = "is_false")]
        italic: bool,
        /// Where each line sits across the element.
        #[serde(default, skip_serializing_if = "is_left")]
        align: TextAlign,
        /// Space added between letters, in points (spaced-out ocean names); negative tightens.
        #[serde(default, skip_serializing_if = "is_zero")]
        letter_spacing: f64,
        /// Sans-serif or serif; left out when sans, so layouts saved before keep their bytes. With
        /// `font_name`, the family drawn where that one isn't installed.
        #[serde(default, skip_serializing_if = "is_sans")]
        font: FontFamily,
        /// An installed font family, by the name the font gives it ("Garamond", #762): drawn in
        /// it where it is installed, else in `font`. Left out when none; kept as a label's font is
        /// (trimmed, bounded, none when not a name).
        #[serde(default, deserialize_with = "crate::symbology::de_font", skip_serializing_if = "Option::is_none")]
        font_name: Option<String>,
    },
    Picture {
        /// Base64 PNG or JPEG.
        image: String,
    },
    /// The attribute table of a layer in a frame's map.
    Table {
        frame: u32,
        layer: String,
        #[serde(default = "rows")]
        rows: usize,
    },
    /// A bar chart of a numeric field of a layer in a frame's map, one bar per feature.
    Chart {
        frame: u32,
        layer: String,
        field: String,
        /// Bars are labelled with this field, if set.
        #[serde(default)]
        label: Option<String>,
    },
    /// A rectangle drawn on the page (a neatline or a backdrop). `width` is the outline's, in
    /// points (0: none).
    Rectangle {
        fill: Rgba,
        outline: Rgba,
        width: f64,
        /// Its corners rounded by this much, in millimetres (0: square corners).
        #[serde(default, skip_serializing_if = "is_zero")]
        radius: f64,
    },
    /// An ellipse filling the element's box: a circle in a square one.
    Ellipse { fill: Rgba, outline: Rgba, width: f64 },
    /// A straight line across the element's box, `width` points wide.
    Line {
        color: Rgba,
        width: f64,
        #[serde(default)]
        dir: LineDir,
        /// Arrowheads: none makes it a plain line.
        #[serde(default)]
        head: LineHead,
    },
}

fn rows() -> usize {
    12
}

impl ElementKind {
    /// A text element in the plain style: upright, left-aligned, normally spaced.
    pub fn text(text: impl Into<String>, size: f64, color: Rgba, bold: bool) -> ElementKind {
        ElementKind::Text {
            text: text.into(),
            size,
            color,
            bold,
            italic: false,
            align: TextAlign::Left,
            letter_spacing: 0.0,
            font: FontFamily::Sans,
            font_name: None,
        }
    }

    /// A north arrow for `frame`, in the default style.
    pub fn north_arrow(frame: u32) -> ElementKind {
        ElementKind::NorthArrow { frame, style: ArrowStyle::default(), fill: ink(), outline: ink() }
    }

    /// A scale bar for `frame`, in the default style.
    pub fn scale_bar(frame: u32) -> ElementKind {
        ElementKind::ScaleBar {
            frame,
            units: ScaleUnits::default(),
            divisions: divisions(),
            style: ScaleBarStyle::default(),
            color: ink(),
            fill: white(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            ElementKind::MapFrame { .. } => "mapFrame",
            ElementKind::NorthArrow { .. } => "northArrow",
            ElementKind::ScaleBar { .. } => "scaleBar",
            ElementKind::Legend { .. } => "legend",
            ElementKind::Colorbar { .. } => "colorbar",
            ElementKind::Text { .. } => "text",
            ElementKind::Picture { .. } => "picture",
            ElementKind::Table { .. } => "table",
            ElementKind::Chart { .. } => "chart",
            ElementKind::Rectangle { .. } => "rectangle",
            ElementKind::Ellipse { .. } => "ellipse",
            ElementKind::Line { .. } => "line",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            ElementKind::MapFrame { .. } => "Map Frame",
            ElementKind::NorthArrow { .. } => "North Arrow",
            ElementKind::ScaleBar { .. } => "Scale Bar",
            ElementKind::Legend { .. } => "Legend",
            ElementKind::Colorbar { .. } => "Colorbar",
            ElementKind::Text { .. } => "Text",
            ElementKind::Picture { .. } => "Picture",
            ElementKind::Table { .. } => "Table Frame",
            ElementKind::Chart { .. } => "Chart Frame",
            ElementKind::Rectangle { .. } => "Rectangle",
            ElementKind::Ellipse { .. } => "Ellipse",
            ElementKind::Line { .. } => "Line",
        }
    }
    /// The map frame this element belongs to.
    pub fn frame(&self) -> Option<u32> {
        match self {
            ElementKind::NorthArrow { frame, .. }
            | ElementKind::ScaleBar { frame, .. }
            | ElementKind::Legend { frame, .. }
            | ElementKind::Colorbar { frame, .. }
            | ElementKind::Table { frame, .. }
            | ElementKind::Chart { frame, .. } => Some(*frame),
            _ => None,
        }
    }
}

/// An element on the page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub id: u32,
    pub name: String,
    /// Top-left corner and size, in millimetres.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Locked, it can be selected but not moved, resized, deleted or edited until unlocked.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    pub kind: ElementKind,
}

impl Element {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

/// A map series: one page per feature of an index layer, the main frame zoomed to each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MapSeries {
    /// The frame that follows the pages.
    pub frame: u32,
    /// The index layer, in the frame's map.
    pub layer: String,
    /// The field naming each page (`{pageName}`); the row number when empty.
    #[serde(default)]
    pub name_field: String,
    /// Space around each page's feature, as a fraction of its extent.
    #[serde(default = "margin")]
    pub margin: f64,
    /// The page shown (0-based row of the index layer).
    #[serde(default)]
    pub page: usize,
}

fn margin() -> f64 {
    0.1
}

/// Most guides a layout keeps in each direction.
pub const MAX_GUIDES: usize = 64;

/// Ruler guides for lining elements up: never printed or exported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Guides {
    /// Vertical guides, at these distances from the page's left edge (mm).
    #[serde(default)]
    pub x: Vec<f64>,
    /// Horizontal guides, at these distances from the page's top edge (mm).
    #[serde(default)]
    pub y: Vec<f64>,
}

impl Guides {
    pub fn is_empty(&self) -> bool {
        self.x.is_empty() && self.y.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub name: String,
    pub width: f64,
    pub height: f64,
    pub elements: Vec<Element>,
    #[serde(default)]
    pub series: Option<MapSeries>,
    #[serde(default, skip_serializing_if = "Guides::is_empty")]
    pub guides: Guides,
    /// Numbers its surrounds print (graticule, scale bar and colorbar labels, `{scale}`) take a
    /// decimal comma and a point between thousands (`0,05`, `1:6.300.000`), as maps in Portuguese,
    /// Spanish, French or German do; off, the other way round.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decimal_comma: bool,
    #[serde(default)]
    next_id: u32,
}

impl Layout {
    pub fn new(name: impl Into<String>, width: f64, height: f64) -> Self {
        Layout {
            name: name.into(),
            width,
            height,
            elements: Vec::new(),
            series: None,
            guides: Guides::default(),
            decimal_comma: false,
            next_id: 1,
        }
    }

    /// Adds an element and returns its id.
    pub fn add(&mut self, name: impl Into<String>, (x, y, w, h): (f64, f64, f64, f64), kind: ElementKind) -> u32 {
        let id = self.next_id.max(self.elements.iter().map(|e| e.id).max().unwrap_or(0).saturating_add(1));
        self.next_id = id.saturating_add(1);
        // Templates on tiny pages can compute sizes near zero.
        let (w, h) = (finite_or(w, 1.0).clamp(MIN_ELEMENT_MM, 5000.0), finite_or(h, 1.0).clamp(MIN_ELEMENT_MM, 5000.0));
        self.elements.push(Element { id, name: name.into(), x, y, w, h, visible: true, locked: false, kind });
        id
    }

    pub fn element(&self, id: u32) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }
    pub fn element_mut(&mut self, id: u32) -> Option<&mut Element> {
        self.elements.iter_mut().find(|e| e.id == id)
    }

    /// The first map frame.
    pub fn first_frame(&self) -> Option<u32> {
        self.elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).map(|e| e.id)
    }

    /// The topmost element at a page point (mm).
    pub fn hit(&self, x: f64, y: f64) -> Option<u32> {
        self.elements.iter().rev().find(|e| e.visible && e.contains(x, y)).map(|e| e.id)
    }

    /// Where element `id` is drawn among the others, and how many there are: 0 is drawn first,
    /// under the rest, and the last over them.
    pub fn draw_order(&self, id: u32) -> Option<(usize, usize)> {
        Some((self.elements.iter().position(|e| e.id == id)?, self.elements.len()))
    }

    /// Moves element `id` in the drawing order: over every other, one place up, one down, or
    /// under every other. Its new place, or None when there's no such element.
    pub fn arrange(&mut self, id: u32, to: Arrange) -> Option<usize> {
        let (i, n) = self.draw_order(id)?;
        let last = n - 1;
        let j = match to {
            Arrange::Front => last,
            Arrange::Forward => (i + 1).min(last),
            Arrange::Backward => i.saturating_sub(1),
            Arrange::Back => 0,
        };
        let e = self.elements.remove(i);
        self.elements.insert(j, e);
        Some(j)
    }

    /// The locked element that removing `id` would take with it: the element itself, or one of
    /// the surrounds that belong to it.
    pub fn locked_in_removal(&self, id: u32) -> Option<&Element> {
        self.elements.iter().find(|e| e.locked && (e.id == id || e.kind.frame() == Some(id)))
    }

    /// Removes an element and the surrounds that belong to it.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.elements.len();
        self.elements.retain(|e| e.id != id && e.kind.frame() != Some(id));
        for e in &mut self.elements {
            if let ElementKind::MapFrame { extent_of, .. } = &mut e.kind
                && *extent_of == Some(id)
            {
                *extent_of = None;
            }
        }
        if self.series.as_ref().is_some_and(|s| s.frame == id) {
            self.series = None;
        }
        self.elements.len() != before
    }

    /// Keeps the page and elements sane after loading or editing (layout files are untrusted):
    /// sizes in range, unique element ids, and no references to missing frames.
    pub fn clamp(&mut self) {
        self.width = finite_or(self.width, 210.0).clamp(20.0, 5000.0);
        self.height = finite_or(self.height, 297.0).clamp(20.0, 5000.0);
        // The same limits `layout.guides` sets: the view walks these lists every frame.
        for list in [&mut self.guides.x, &mut self.guides.y] {
            list.retain(|v| v.is_finite() && v.abs() <= 5000.0);
            list.truncate(MAX_GUIDES);
        }
        self.elements.truncate(500);
        let mut seen = std::collections::HashSet::new();
        let mut next = self.elements.iter().map(|e| e.id).filter(|id| *id <= MAX_ID).max().unwrap_or(0).saturating_add(1);
        for e in &mut self.elements {
            // Ids stay well below u32::MAX so new elements never run out of numbers. A repeated id
            // (only a hand-edited file has one) is renumbered; references to it keep meaning the
            // first element with that id, as they did when the file was written.
            if e.id == 0 || e.id > MAX_ID || !seen.insert(e.id) {
                e.id = next;
                seen.insert(next);
                next = next.saturating_add(1);
            }
            e.w = finite_or(e.w, 20.0).clamp(MIN_ELEMENT_MM, 5000.0);
            e.h = finite_or(e.h, 20.0).clamp(MIN_ELEMENT_MM, 5000.0);
            e.x = finite_or(e.x, 0.0).clamp(-5000.0, 5000.0);
            e.y = finite_or(e.y, 0.0).clamp(-5000.0, 5000.0);
            e.name = e.name.chars().take(200).collect();
            match &mut e.kind {
                ElementKind::MapFrame { center, scale, shape, grid, bearing, pitch, extent_width, .. } => {
                    *scale = finite_or(*scale, 50_000_000.0).clamp(MIN_FRAME_SCALE, MAX_FRAME_SCALE);
                    *extent_width = finite_or(*extent_width, INDICATOR_WIDTH).clamp(MIN_INDICATOR_WIDTH, MAX_INDICATOR_WIDTH);
                    *bearing = crate::camera::normalize_bearing(*bearing);
                    *pitch = crate::camera::normalize_pitch(*pitch);
                    if !center.is_finite() {
                        *center = Coord::new(0.0, 0.0);
                    }
                    center.x = center.x.clamp(-540.0, 540.0);
                    center.y = center.y.clamp(-85.0, 85.0);
                    if let FrameShape::Polygon(p) = shape {
                        p.retain(|(u, v)| u.is_finite() && v.is_finite());
                        p.truncate(10_000);
                        p.iter_mut().for_each(|(u, v)| (*u, *v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)));
                        if p.len() < 3 {
                            *shape = FrameShape::Rectangle;
                        }
                    }
                    if let Some(g) = grid {
                        // A graticule's step in degrees, a measured grid's in its system's units.
                        let step = finite_or(g.step, 0.0);
                        g.step = match g.kind {
                            GridKind::Graticule => step.clamp(0.0, MAX_GRATICULE_STEP),
                            GridKind::Measured if step <= 0.0 => 0.0,
                            GridKind::Measured => step.clamp(MIN_MEASURED_STEP, MAX_MEASURED_STEP),
                        };
                        g.tick_length = finite_or(g.tick_length, tick_length()).clamp(0.0, MAX_TICK_MM);
                        g.width = finite_or(g.width, grid_width()).clamp(0.0, MAX_GRID_WIDTH);
                    }
                }
                ElementKind::Legend { exclude, .. } => {
                    // Names stay whole: a layer's own name isn't cut, and a cut one would no
                    // longer match it.
                    exclude.truncate(MAX_LEGEND_EXCLUDED);
                }
                ElementKind::Text { text, size, letter_spacing, .. } => {
                    *size = finite_or(*size, 12.0).clamp(*crate::graphics::FONT_SIZE.start(), *crate::graphics::FONT_SIZE.end());
                    *letter_spacing = finite_or(*letter_spacing, 0.0).clamp(MIN_LETTER_SPACING, MAX_LETTER_SPACING);
                    text.truncate(text.char_indices().nth(5000).map_or(text.len(), |(i, _)| i));
                }
                ElementKind::Table { rows, .. } => *rows = (*rows).clamp(1, 500),
                ElementKind::ScaleBar { divisions, .. } => *divisions = (*divisions).clamp(1, MAX_SCALE_DIVISIONS),
                ElementKind::Rectangle { width, .. } | ElementKind::Ellipse { width, .. } => {
                    *width = finite_or(*width, 1.0).clamp(0.0, 50.0)
                }
                ElementKind::Line { width, .. } => *width = finite_or(*width, 1.0).clamp(0.1, 50.0),
                _ => {}
            }
        }
        self.next_id = next;
        // References must point at map frames that exist.
        let frames: std::collections::HashSet<u32> =
            self.elements.iter().filter(|e| matches!(e.kind, ElementKind::MapFrame { .. })).map(|e| e.id).collect();
        self.elements.retain(|e| e.kind.frame().is_none_or(|f| frames.contains(&f)));
        for e in &mut self.elements {
            if let ElementKind::MapFrame { extent_of, .. } = &mut e.kind
                && extent_of.is_some_and(|o| !frames.contains(&o) || o == e.id)
            {
                *extent_of = None;
            }
        }
        if let Some(s) = &mut self.series {
            if !frames.contains(&s.frame) {
                self.series = None;
            } else {
                s.margin = finite_or(s.margin, 0.1).clamp(0.0, 5.0);
            }
        }
    }
}

fn finite_or(v: f64, d: f64) -> f64 {
    if v.is_finite() { v } else { d }
}

/// The extent indicator's colour and width (points) until they are set.
pub const INDICATOR_COLOR: Rgba = Rgba::rgb(0xD0, 0x1C, 0x1C);
pub const INDICATOR_WIDTH: f64 = 2.0;
/// The narrowest and widest an extent indicator's outline may be (points); to hide it, remove it.
pub const MIN_INDICATOR_WIDTH: f64 = 0.1;
pub const MAX_INDICATOR_WIDTH: f64 = 20.0;

fn indicator_color() -> Rgba {
    INDICATOR_COLOR
}

fn is_indicator_color(c: &Rgba) -> bool {
    *c == INDICATOR_COLOR
}

fn indicator_width() -> f64 {
    INDICATOR_WIDTH
}

fn is_indicator_width(w: &f64) -> bool {
    *w == INDICATOR_WIDTH
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// The camera that shows a frame's centre at its paper scale in `map`'s projection, turned to
/// `bearing` and tilted to `pitch` (degrees). The globe takes its camera in Web Mercator, whose
/// ground scale at the centre it matches, so it is handled like the flat map, and turns and
/// tilts like it. The frame's own bearing and pitch apply, whichever way the map in view is
/// turned or tilted.
pub fn frame_camera(map: &Map, center: Coord, scale: f64, bearing: f64, pitch: f64) -> Camera {
    let mut cam = Camera { bearing: crate::camera::normalize_bearing(bearing), pitch: crate::camera::normalize_pitch(pitch), ..map.camera };
    if let Some(c) = map.to_map(center) {
        cam.center = c;
    }
    let per = map.ground_per_unit(cam.center).max(1e-9);
    cam.res = scale * PAPER_M_PER_PT / per;
    cam.clamp();
    cam
}

/// The scale that fits a lon/lat extent into a frame `w_mm`×`h_mm` with a margin, and its centre.
pub fn fit_scale(map: &Map, e: &rustgis_geom::Extent, w_mm: f64, h_mm: f64) -> Option<(Coord, f64)> {
    if e.is_empty() || w_mm <= 0.0 || h_mm <= 0.0 {
        return None;
    }
    let flat = map.flat();
    let mut ext = rustgis_geom::Extent::EMPTY;
    for c in
        [Coord::new(e.min_x, e.min_y), Coord::new(e.max_x, e.min_y), Coord::new(e.max_x, e.max_y), Coord::new(e.min_x, e.max_y), e.center()]
    {
        if let Some(p) = flat.forward_checked(c) {
            ext.expand(p);
        }
    }
    if ext.is_empty() {
        return None;
    }
    let center = flat.inverse(ext.center())?;
    let per = (flat.ground_per_unit(ext.center()) * map.body().ratio()).max(1e-9);
    // A lone point gets a metre around it; anything larger fits as it is, down to a find a
    // millimetre across (#902). Ground metres, whatever the projection's units.
    let floor_m = if ext.width() > 0.0 || ext.height() > 0.0 { 1e-3 } else { 1.0 };
    let (gw, gh) = ((ext.width() * per).max(floor_m) * 1.1, (ext.height() * per).max(floor_m) * 1.1);
    let scale = (gw / (w_mm / 1000.0)).max(gh / (h_mm / 1000.0));
    Some((center, scale.clamp(MIN_FRAME_SCALE, MAX_FRAME_SCALE)))
}

/// What a colorbar draws for a raster layer.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorScale {
    /// A continuous ramp from `lo` to `hi`, its colours evenly spaced over the values.
    Ramp { lo: f64, hi: f64, colors: Vec<Rgba> },
    /// Classes, each bounded by two of `edges` (one more than there are colours).
    Steps { edges: Vec<f64>, colors: Vec<Rgba> },
    /// A colour per value.
    Values(Vec<(f64, Rgba)>),
}

/// Most values a colorbar gives a block of their own (unique values, colour tables).
pub const MAX_COLORBAR_VALUES: usize = 64;

/// Colours sampled along a continuous colorbar.
const RAMP_SAMPLES: usize = 256;

/// The raster layer a colorbar shows: `layer` by name, or the top visible raster of the map.
pub fn colorbar_layer<'a>(map: &'a Map, layer: Option<&str>) -> Option<&'a Layer> {
    match layer {
        Some(n) => map.layer_by_name(n).filter(|l| l.is_raster()),
        None => map.layers.iter().find(|l| l.visible && l.is_raster()),
    }
}

/// A raster layer's colour scale as the map draws it (stretch, enhancement and all), or `None`
/// when it has none: RGB, a hillshade without a tint, or pixels that couldn't be read.
pub fn color_scale(rl: &RasterLayer) -> Option<ColorScale> {
    let r = rl.data.as_ref()?;
    let mask = &rl.style.mask;
    // Coloured without the mask, so a hidden value leaves no gap in the bar.
    let style = RasterStyle { mask: RasterMask::default(), ..rl.style.clone() };
    let masked = rl.masked_stats();
    let view_stats = masked.as_deref().map(Vec::as_slice);
    let band = style.renderer.bands().first().copied().unwrap_or(0);
    let stats = view_stats.and_then(|s| s.first()).or_else(|| r.bands.get(band).map(|b| &b.stats))?;
    let colorizer = Colorizer::new(&style, r, view_stats);
    let shade = matches!(style.renderer, RasterRenderer::ShadedRelief { .. }).then_some(255);
    let at = |v: f64| colorizer.color(&[v as f32], shade).map(Rgba);
    match &style.renderer {
        RasterRenderer::Stretch { .. } | RasterRenderer::ShadedRelief { ramp: Some(_), .. } => {
            let n = Normalizer::new(style.stretch, stats, &r.data_type);
            let (lo, hi) = (n.lo, n.hi);
            let colors = (0..RAMP_SAMPLES)
                .map(|i| at(lo + (hi - lo) * i as f64 / (RAMP_SAMPLES - 1) as f64).unwrap_or(Rgba::new(0, 0, 0, 0)))
                .collect();
            Some(ColorScale::Ramp { lo, hi, colors })
        }
        RasterRenderer::Classify { breaks, ramp, .. } => {
            let first = *breaks.first()?;
            let mut edges = vec![if stats.min.is_finite() { stats.min.min(first) } else { first }];
            edges.extend(breaks.iter().copied());
            // Each class's colour from a value inside it, so the enhancement applies as on the map.
            let colors = edges
                .windows(2)
                .enumerate()
                .map(|(i, w)| {
                    match w {
                        [a, b] if a < b => at((a + b) / 2.0),
                        [_, b] => at(*b),
                        _ => None,
                    }
                    .unwrap_or_else(|| ramp.pick(i, breaks.len()))
                })
                .collect();
            Some(ColorScale::Steps { edges, colors })
        }
        RasterRenderer::UniqueValues { values, .. } => {
            let v: Vec<(f64, Rgba)> = values
                .iter()
                .filter(|(v, _)| !mask.hides(*v as f32))
                .take(MAX_COLORBAR_VALUES)
                .map(|(v, c)| (*v, at(*v).unwrap_or(*c)))
                .collect();
            (!v.is_empty()).then_some(ColorScale::Values(v))
        }
        RasterRenderer::Colormap => {
            let cats = r.bands.first().and_then(|b| b.stats.categories.as_ref())?;
            let v: Vec<(f64, Rgba)> =
                cats.iter().filter(|v| !mask.hides(**v as f32)).filter_map(|v| at(*v).map(|c| (*v, c))).take(MAX_COLORBAR_VALUES).collect();
            (!v.is_empty()).then_some(ColorScale::Values(v))
        }
        RasterRenderer::Rgb { .. } | RasterRenderer::ShadedRelief { ramp: None, .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_are_arranged_in_the_drawing_order() {
        let mut l = Layout::new("L", 100.0, 100.0);
        let rect = || ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 0.0, radius: 0.0 };
        let ids: Vec<u32> = (0..4).map(|i| l.add(format!("E{i}"), (10.0, 10.0, 50.0, 50.0), rect())).collect();
        let order = |l: &Layout| l.elements.iter().map(|e| e.id).collect::<Vec<_>>();
        // All four overlap: the one drawn last is the one a click picks.
        assert_eq!(l.hit(20.0, 20.0), Some(ids[3]));
        assert_eq!(l.arrange(ids[0], Arrange::Front), Some(3));
        assert_eq!(order(&l), [ids[1], ids[2], ids[3], ids[0]]);
        assert_eq!(l.hit(20.0, 20.0), Some(ids[0]), "brought to the front, it's on top");
        assert_eq!(l.arrange(ids[0], Arrange::Backward), Some(2));
        assert_eq!(l.arrange(ids[0], Arrange::Back), Some(0));
        assert_eq!(l.arrange(ids[0], Arrange::Forward), Some(1));
        assert_eq!(order(&l), [ids[1], ids[0], ids[2], ids[3]]);
        // At an end, a step that way stays put.
        assert_eq!(l.arrange(ids[3], Arrange::Forward), Some(3));
        assert_eq!(l.arrange(ids[1], Arrange::Backward), Some(0));
        assert_eq!(l.arrange(999, Arrange::Front), None);
        assert_eq!(l.draw_order(ids[0]), Some((1, 4)));
        assert_eq!(Arrange::from_name(" Back "), Some(Arrange::Back));
    }

    #[test]
    fn lines_turn_to_any_angle_and_keep_their_length() {
        // Within half a degree: lines within a quarter millimetre of level or upright are made so.
        let near = |a: f64, b: f64| ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs() < 0.5;
        for head in LineHead::ALL {
            for angle in (-180..=180).step_by(5).map(f64::from).chain([0.1, 89.9, 179.95]) {
                let (r, dir, h) = line_at_angle((100.0, 80.0), 40.0, angle, head);
                // Centred where asked; Both and None stay; one head moves to keep pointing.
                assert!(((r.0 + r.2 / 2.0) - 100.0).abs() < 1e-9 && ((r.1 + r.3 / 2.0) - 80.0).abs() < 1e-9);
                assert_eq!(h == LineHead::None || h == LineHead::Both, head == LineHead::None || head == LineHead::Both);
                assert!((line_length(r, dir) - 40.0).abs() < 0.3, "{angle}°: {}", line_length(r, dir));
                // Read back as it was set: plain lines and two-headed ones have no way to point, so
                // either way along them will do.
                let back = line_angle(r, dir, h);
                let pointed = matches!(head, LineHead::End | LineHead::Start);
                assert!(near(back, angle) || (!pointed && near(back, angle + 180.0)), "{head:?} {angle}° came back {back}°");
            }
        }
        // Level and upright lines keep a box to pick them up by.
        assert_eq!(line_at_angle((50.0, 50.0), 30.0, 0.0, LineHead::End).0, (35.0, 47.0, 30.0, LINE_BOX_MM));
        assert_eq!(line_at_angle((50.0, 50.0), 30.0, 90.0, LineHead::End).1, LineDir::Vertical);
        // Pointing left or down puts the single head on the start.
        assert_eq!(line_at_angle((0.0, 0.0), 10.0, 180.0, LineHead::End).2, LineHead::Start);
        assert_eq!(line_at_angle((0.0, 0.0), 10.0, -90.0, LineHead::Start).2, LineHead::End);
        // Junk in: a usable line out.
        let (r, ..) = line_at_angle((0.0, 0.0), f64::NAN, f64::INFINITY, LineHead::End);
        assert!(r.2.is_finite() && r.3.is_finite() && r.2 > 0.0);
    }

    #[test]
    fn elements_add_hit_and_remove_with_surrounds() {
        let mut l = Layout::new("Layout", 297.0, 210.0);
        let f = l.add(
            "Map Frame",
            (10.0, 10.0, 200.0, 150.0),
            ElementKind::MapFrame {
                map: "Map".into(),
                center: Coord::new(0.0, 0.0),
                scale: 1e8,
                shape: FrameShape::Rectangle,
                grid: None,
                extent_of: None,
                extent_color: INDICATOR_COLOR,
                extent_width: INDICATOR_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        let n = l.add("North Arrow", (215.0, 10.0, 15.0, 20.0), ElementKind::north_arrow(f));
        let t = l.add("Title", (20.0, 165.0, 100.0, 12.0), ElementKind::text("World", 18.0, Rgba::rgb(0, 0, 0), true));
        assert_eq!(l.hit(50.0, 50.0), Some(f));
        assert_eq!(l.hit(220.0, 15.0), Some(n));
        assert_eq!(l.hit(290.0, 200.0), None);
        assert!(l.remove(f));
        assert!(l.element(n).is_none(), "the frame's north arrow goes with it");
        assert!(l.element(t).is_some());
    }

    #[test]
    fn hostile_layouts_are_repaired() {
        let text = r#"{"name":"x","width":1e300,"height":-5,"elements":[
            {"id":3,"name":"a","x":0,"y":0,"w":1e9,"h":10,"kind":{"MapFrame":{"map":"M","center":{"x":1e300,"y":200},"scale":-1,"shape":{"Polygon":[[0,0],[1,1]]},"extent_of":3,"extent_width":1e9}}},
            {"id":3,"name":"b","x":0,"y":0,"w":10,"h":10,"kind":{"Text":{"text":"t","size":1e9,"color":[0,0,0,255],"letter_spacing":-1e9}}},
            {"id":9,"name":"c","x":0,"y":0,"w":10,"h":10,"kind":{"Legend":{"frame":42,"title":"L"}}}],
            "series":{"frame":77,"layer":"x"}}"#;
        let mut l: Layout = serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}"));
        l.clamp();
        let ids: Vec<u32> = l.elements.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 2, "the legend of a missing frame goes");
        assert_ne!(ids[0], ids[1], "duplicate ids are renumbered");
        assert!(l.width <= 5000.0 && l.height >= 20.0 && l.series.is_none());
        match &l.elements[0].kind {
            ElementKind::MapFrame { scale, shape, extent_of, center, extent_width, .. } => {
                assert!(*scale == MIN_FRAME_SCALE && *shape == FrameShape::Rectangle && extent_of.is_none() && center.is_finite());
                assert_eq!(*extent_width, MAX_INDICATOR_WIDTH);
            }
            _ => panic!("frame expected"),
        }
        assert!(
            matches!(&l.elements[1].kind, ElementKind::Text { size, letter_spacing, .. } if *size <= 400.0 && *letter_spacing == MIN_LETTER_SPACING)
        );
        let id = l.add("n", (0.0, 0.0, 1.0, 1.0), ElementKind::north_arrow(ids[0]));
        assert!(!ids.contains(&id));
        // An id at u32::MAX is renumbered, and adding after it doesn't overflow.
        let mut big: Layout = serde_json::from_str(r#"{"name":"b","width":100,"height":100,"elements":[{"id":4294967295,"name":"t","x":0,"y":0,"w":5,"h":5,"kind":{"Text":{"text":"t","size":10,"color":[0,0,0,255]}}}]}"#).unwrap_or_else(|e| panic!("{e}"));
        big.clamp();
        assert!(big.elements[0].id <= MAX_ID);
        let a = big.add(
            "a",
            (0.0, 0.0, 1.0, 1.0),
            ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 1.0, radius: 0.0 },
        );
        assert!(a != big.elements[0].id && a <= MAX_ID + 1);
    }

    #[test]
    fn hairline_rectangles_keep_their_height() {
        let mut l = Layout::new("Layout", 210.0, 297.0);
        let rule = ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 0.0, radius: 0.0 };
        let id = l.add("Rule", (14.0, 33.0, 182.0, 0.15), rule.clone());
        assert_eq!(l.element(id).map(|e| e.h), Some(0.15));
        // A zero height becomes the thinnest element, not a 1 mm box.
        let zero = l.add("Zero", (14.0, 40.0, 182.0, 0.0), rule);
        assert_eq!(l.element(zero).map(|e| e.h), Some(MIN_ELEMENT_MM));
        // So does a loaded one.
        let text = r#"{"name":"x","width":210,"height":297,"elements":[
            {"id":1,"name":"a","x":14,"y":33,"w":182,"h":0.15,"kind":{"Rectangle":{"fill":[0,0,0,255],"outline":[0,0,0,0],"width":0}}},
            {"id":2,"name":"b","x":14,"y":40,"w":182,"h":0,"kind":{"Rectangle":{"fill":[0,0,0,0],"outline":[0,0,0,255],"width":0.4}}}]}"#;
        let mut l: Layout = serde_json::from_str(text).unwrap();
        l.clamp();
        assert_eq!(l.element(1).map(|e| e.h), Some(0.15));
        assert_eq!(l.element(2).map(|e| e.h), Some(MIN_ELEMENT_MM));
    }

    #[test]
    fn frame_camera_keeps_paper_scale() {
        let map = Map::new("Map");
        let cam = frame_camera(&map, Coord::new(0.0, 0.0), 1_000_000.0, 0.0, 0.0);
        // At the equator one logical point of paper (0.2646 mm) covers 264.6 m at 1:1,000,000.
        assert!((cam.res - 264.583).abs() < 0.5, "{}", cam.res);
        let (c, s) = fit_scale(&map, &rustgis_geom::Extent::new(-10.0, -10.0, 10.0, 10.0), 200.0, 200.0).unwrap_or_default();
        assert!(c.x.abs() < 1e-6 && s > 1e7 && s < 2e7, "{c:?} {s}");
        // A finds drawing at 2:1 on the equator, where a frame's camera is closest (#902).
        let cam = frame_camera(&map, Coord::new(0.0, 0.0), MIN_FRAME_SCALE, 0.0, 0.0);
        assert!((cam.res / (MIN_FRAME_SCALE * PAPER_M_PER_PT) - 1.0).abs() < 1e-9, "{}", cam.res);
        // A 2 cm find fitted into a 100 mm frame would print at about 4.5:1: held at 2:1.
        let find = rustgis_geom::Extent::new(0.0, 0.0, 0.02 / 111_320.0, 0.02 / 111_320.0);
        assert_eq!(fit_scale(&map, &find, 100.0, 100.0).map(|(_, s)| s), Some(MIN_FRAME_SCALE));
    }

    #[test]
    fn automatic_scale_bars_count_in_what_the_plan_shows() {
        assert_eq!(ScaleUnits::Auto.resolve(5_000.0), (1000.0, "km"));
        assert_eq!(ScaleUnits::Auto.resolve(20.0), (1.0, "m"));
        assert_eq!(ScaleUnits::Auto.resolve(1.0), (1.0, "m"));
        assert_eq!(ScaleUnits::Auto.resolve(0.05), (0.01, "cm"));
        assert_eq!(ScaleUnits::Auto.resolve(0.005), (0.001, "mm"));
        assert_eq!(ScaleUnits::Meters.resolve(0.005), (1.0, "m"), "a chosen unit stays");
    }

    #[test]
    fn frame_camera_follows_the_frame_on_the_globe() {
        let mut map = Map::new("Map");
        map.projection = crate::Projection::Globe;
        let near = frame_camera(&map, Coord::new(10.0, 20.0), 60_000_000.0, 0.0, 0.0);
        let far = frame_camera(&map, Coord::new(10.0, 20.0), 300_000_000.0, 0.0, 0.0);
        assert!((far.res / near.res - 5.0).abs() < 1e-6, "{} {}", near.res, far.res);
        let c = map.to_lonlat(near.center).unwrap_or_default();
        assert!((c.x - 10.0).abs() < 1e-6 && (c.y - 20.0).abs() < 1e-6, "{c:?}");
        // The globe's ground scale at the centre is the frame's paper scale.
        let ground = near.res * 20f64.to_radians().cos();
        assert!((ground - 60_000_000.0 * PAPER_M_PER_PT).abs() < 1e-3, "{ground}");
    }

    #[test]
    fn repeated_ids_keep_references_on_the_first_element() {
        let text = r#"{"name":"x","width":200,"height":100,"elements":[
            {"id":1,"name":"frame","x":0,"y":0,"w":100,"h":80,"kind":{"MapFrame":{"map":"M","center":{"x":0,"y":0},"scale":1e7}}},
            {"id":1,"name":"box","x":0,"y":0,"w":10,"h":10,"kind":{"Rectangle":{"fill":[0,0,0,255],"outline":[0,0,0,255],"width":1}}},
            {"id":2,"name":"arrow","x":110,"y":0,"w":10,"h":10,"kind":{"NorthArrow":{"frame":1}}}]}"#;
        let mut l: Layout = serde_json::from_str(text).unwrap();
        l.clamp();
        assert_eq!(l.element(1).map(|e| e.name.as_str()), Some("frame"));
        let ids: std::collections::HashSet<u32> = l.elements.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 3, "the box got a fresh id");
        assert!(matches!(l.element(2).map(|e| &e.kind), Some(ElementKind::NorthArrow { frame: 1, .. })));
    }

    #[test]
    fn loaded_guides_are_limited() {
        let mut l = Layout::new("Layout", 297.0, 210.0);
        l.guides.x = (0..200).map(f64::from).collect();
        l.guides.y = vec![f64::NAN, 1e9, -20.0, f64::INFINITY, 50.0];
        l.clamp();
        assert_eq!(l.guides.x.len(), MAX_GUIDES);
        assert_eq!(l.guides.y, vec![-20.0, 50.0]);
    }

    /// A measured grid's labels: whole values, or the kilometres' last two digits; a graticule
    /// saved before measured grids is written as it was; the UTM zone at a point.
    #[test]
    fn measured_grids_label_eastings_and_northings() {
        let g = Grid { kind: GridKind::Measured, ..Grid::default() };
        assert_eq!(g.measured_label(543_000.0, 1000.0), "543000");
        assert_eq!(g.measured_label(5_262_000.0, 1000.0), "5262000");
        assert_eq!(g.measured_label(2_500.5, 0.5), "2500.5");
        assert_eq!(g.measured_label(-1_000.0, 1000.0), "-1000");
        // Off the step by rounding error from the stepped loop: back on it.
        assert_eq!(g.measured_label(543_000.000_000_1, 1000.0), "543000");
        assert_eq!(g.measured_label(-0.000_000_1, 1000.0), "0");
        let short = Grid { short: true, ..g.clone() };
        assert_eq!(short.measured_label(543_000.0, 1000.0), "43");
        assert_eq!(short.measured_label(5_262_000.0, 1000.0), "62");
        assert_eq!(short.measured_label(5_200_000.0, 1000.0), "0");
        assert_eq!(short.measured_label(543_500.0, 500.0), "43.5");
        assert_eq!(short.measured_label(-1_000.0, 1000.0), "99");
        // The UTM zone (WGS 84) at a point, north and south of the Equator.
        assert_eq!(utm_epsg(8.73, 47.5), 32632);
        assert_eq!(utm_epsg(-180.0, 10.0), 32601);
        assert_eq!(utm_epsg(179.9, -10.0), 32760);
        assert_eq!(utm_epsg(180.0, -10.0), 32701, "the antimeridian is -180°, zone 1's west edge");
        assert_eq!(utm_epsg(151.2, -33.9), 32756);
        assert_eq!(utm_epsg(f64::NAN, 0.0), 32631);
        assert_eq!(utm_epsg(190.0, 10.0), 32602, "a world copy east of the antimeridian");
        assert_eq!(short.measured_label(500_000.0, 100_000.0), "500000", "too wide a step for short labels");
        // A graticule saved as it always was; a measured grid keeps its type, system and labels.
        let saved = serde_json::to_value(Grid::default()).unwrap();
        for k in ["kind", "crs", "short"] {
            assert!(saved.get(k).is_none(), "{k} written for a graticule");
        }
        let m = Grid { kind: GridKind::Measured, crs: Some("EPSG:25832".into()), short: true, step: 1000.0, ..Grid::default() };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!((v["kind"].as_str(), v["crs"].as_str(), v["short"].as_bool()), (Some("measured"), Some("EPSG:25832"), Some(true)));
        assert_eq!(serde_json::from_value::<Grid>(v).unwrap(), m);
        assert_eq!(GridKind::from_name(" Measured "), Some(GridKind::Measured));
        assert_eq!(GridKind::from_name("utm"), None);
    }

    #[test]
    fn grid_labels_follow_the_format() {
        let g = Grid::default();
        // #210: the Equator and prime meridian are 0°, the antimeridian 180°, with no letter.
        assert_eq!(g.label(0.0, false, 10.0), "0°");
        assert_eq!(g.label(-0.0, false, 10.0), "0°");
        assert_eq!(g.label(0.0, true, 10.0), "0°");
        assert_eq!(g.label(180.0, true, 10.0), "180°");
        assert_eq!(g.label(-180.0, true, 10.0), "180°");
        assert_eq!(g.label(10.0, false, 10.0), "10°N");
        assert_eq!(g.label(-10.0, false, 10.0), "10°S");
        assert_eq!(g.label(20.0, true, 10.0), "20°E");
        assert_eq!(g.label(-20.0, true, 10.0), "20°W");
        // As many decimals as the step needs.
        assert_eq!(g.label(0.5, false, 0.25), "0.50°N");
        assert_eq!(g.label(-112.5, true, 0.5), "112.5°W");
        // Accumulated decimal steps miss 0 and 180 by rounding error.
        let mut v = -0.5;
        for _ in 0..5 {
            v += 0.1;
        }
        assert_ne!(v, 0.0);
        assert_eq!(g.label(v, false, 0.1), "0.0°");
        let mut v = 179.5;
        for _ in 0..5 {
            v += 0.1;
        }
        assert_eq!(g.label(v, true, 0.1), "180.0°");
        // Signed rather than lettered.
        let signed = Grid { hemisphere: false, ..Grid::default() };
        assert_eq!(signed.label(-112.5, true, 0.5), "-112.5°");
        assert_eq!(signed.label(45.0, false, 15.0), "45°");
        assert_eq!(signed.label(0.0, false, 15.0), "0°");
        // Degrees, minutes and seconds, leaving out what the step doesn't need.
        let dms = Grid { format: CoordFormat::Dms, ..Grid::default() };
        assert_eq!(dms.label(30.0, false, 5.0), "30°N");
        assert_eq!(dms.label(-112.5, true, 0.5), "112°30′W");
        assert_eq!(dms.label(-112.0, true, 0.5), "112°00′W");
        assert_eq!(dms.label(12.0 + 1.0 / 120.0, false, 1.0 / 120.0), "12°00′30″N");
        // Rounding up a second carries into the minutes and degrees.
        assert_eq!(dms.label(12.999_999_9, false, 0.25), "13°00′N");
    }

    #[test]
    fn surrounds_saved_before_their_options_open_as_they_were() {
        let arrow: ElementKind = serde_json::from_str(r#"{"NorthArrow":{"frame":3}}"#).unwrap();
        assert_eq!(arrow, ElementKind::north_arrow(3));
        assert!(matches!(arrow, ElementKind::NorthArrow { style: ArrowStyle::Classic, .. }));
        let bar: ElementKind = serde_json::from_str(r#"{"ScaleBar":{"frame":3}}"#).unwrap();
        assert_eq!(bar, ElementKind::scale_bar(3));
        assert!(matches!(bar, ElementKind::ScaleBar { units: ScaleUnits::Auto, divisions: 4, style: ScaleBarStyle::Alternating, .. }));
        assert!(matches!(bar, ElementKind::ScaleBar { color, fill, .. } if color == ink() && fill == white()));
        let grid: Grid = serde_json::from_str(r#"{"step":10,"color":[64,72,85,150],"labels":true}"#).unwrap();
        assert_eq!(grid, Grid { step: 10.0, ..Grid::default() });
        assert_eq!(grid.edges, GridEdges { top: false, bottom: true, left: true, right: false });
        assert_eq!(grid.style, GridStyle::Lines);
    }

    #[test]
    fn loaded_grids_and_scale_bars_are_limited() {
        let text = r#"{"name":"x","width":200,"height":100,"elements":[
            {"id":1,"name":"frame","x":0,"y":0,"w":100,"h":80,"kind":{"MapFrame":{"map":"M","center":{"x":0,"y":0},"scale":1e7,
                "grid":{"step":10,"color":[0,0,0,255],"tick_length":1e9,"width":-5}}}},
            {"id":2,"name":"bar","x":110,"y":0,"w":50,"h":10,"kind":{"ScaleBar":{"frame":1,"divisions":100000}}}]}"#;
        let mut l: Layout = serde_json::from_str(text).unwrap();
        l.clamp();
        let Some(ElementKind::MapFrame { grid: Some(g), .. }) = l.element(1).map(|e| &e.kind) else { panic!() };
        assert_eq!((g.tick_length, g.width), (MAX_TICK_MM, 0.0));
        assert!(matches!(l.element(2).map(|e| &e.kind), Some(ElementKind::ScaleBar { divisions: MAX_SCALE_DIVISIONS, .. })));
        // A step in degrees for a graticule, in the system's units for a measured grid: 1 km
        // kept, and limited as `layout.setGrid` limits it.
        let step = |kind: &str, step: f64| {
            let text = text.replace(r#""step":10"#, &format!(r#""step":{step},"kind":"{kind}""#));
            let mut l: Layout = serde_json::from_str(&text).unwrap();
            l.clamp();
            let Some(ElementKind::MapFrame { grid: Some(g), .. }) = l.element(1).map(|e| &e.kind) else { panic!() };
            g.step
        };
        assert_eq!((step("graticule", 1000.0), step("graticule", -1.0)), (MAX_GRATICULE_STEP, 0.0));
        assert_eq!(
            (step("measured", 1000.0), step("measured", 1e12), step("measured", 0.25)),
            (1000.0, MAX_MEASURED_STEP, MIN_MEASURED_STEP)
        );
        assert_eq!(step("measured", 0.0), 0.0, "Auto");
    }
}
