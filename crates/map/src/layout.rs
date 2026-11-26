//! Page layouts: a page (in millimetres) holding map frames and the surrounds that explain them
//! (north arrows, scale bars, legends, text, pictures, tables, charts), exported as images or PDF.
//!
//! Element positions are in millimetres from the page's top-left corner. A map frame shows a map
//! of the project at its own centre and scale, independent of the map view.

use std::collections::BTreeMap;

use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};

use crate::raster::{Colorizer, Normalizer, RasterLayer, RasterMask, RasterRenderer, RasterStyle};
use crate::report::StatKind;
use crate::symbology::Dash;
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
    /// A measured grid labelled as MGRS: each 100 km square named with its grid zone designation
    /// (`32T MT`), and the lines by their kilometres' last two digits (`05`). Its system is a WGS 84 UTM
    /// zone: the one named, else the map's own when it is one, else the zone at the frame's centre.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mgrs: bool,
    /// The size of its labels, in points, within [`LAYOUT_TEXT_SIZE`] (#2581): their room from
    /// the frame's edge and MGRS squares' names follow in proportion. [`GRID_LABEL_PT`] unless
    /// set, and left out then, so layouts saved before keep their bytes.
    #[serde(default = "grid_label_pt", skip_serializing_if = "is_grid_label_pt")]
    pub label_size: f64,
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

/// The WGS 84 UTM zone a coordinate system names by its EPSG code (`EPSG:32632`, `32632`, any
/// case, also `epsg 32632`): its zone and whether it is the northern hemisphere's. `None` for any
/// other system, and for a UTM zone given as WKT or a PROJ string, which MGRS labels don't read.
pub fn utm_zone_of(crs: &str) -> Option<(u8, bool)> {
    let t = crs.trim();
    let digits = match t.get(..5) {
        Some(p) if p.eq_ignore_ascii_case("EPSG:") || p.eq_ignore_ascii_case("EPSG ") => t.get(5..)?.trim(),
        _ => t,
    };
    if !(digits.len() == 5 && digits.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let code: u32 = digits.parse().ok()?;
    let zone = u8::try_from(code % 100).ok()?;
    match code / 100 {
        326 | 327 if (1..=60).contains(&zone) => Some((zone, code / 100 == 326)),
        _ => None,
    }
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
            mgrs: false,
            label_size: GRID_LABEL_PT,
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
        if (self.short || self.mgrs) && step < 100_000.0 {
            let km = (v / 1000.0).rem_euclid(100.0);
            // (99.9999… after rounding error is 0 again.)
            let km = if (km - 100.0).abs() < 1e-6 { 0.0 } else { km };
            let s = decimal(km, step / 1000.0);
            // MGRS prints its two digits always: `05`, `00`.
            return if self.mgrs && km < 10.0 { format!("0{s}") } else { s };
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

/// Where a text element's block of lines sits down its box.
///
/// `Invalid` is what a file's unknown name or wrong kind of value is read as; it is never kept:
/// [`Layout::clamp`] sends it to the default and counts the element, and a save writes the
/// default's name for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextVAlign {
    /// From the top of the box.
    #[default]
    Top,
    /// Centred in the box.
    Middle,
    /// Down to the bottom of the box.
    Bottom,
    /// A value read from a file that is not a name; repaired to the default on load.
    #[doc(hidden)]
    Invalid,
}

impl TextVAlign {
    pub const ALL: [TextVAlign; 3] = [TextVAlign::Top, TextVAlign::Middle, TextVAlign::Bottom];

    pub fn code(self) -> &'static str {
        match self {
            TextVAlign::Top | TextVAlign::Invalid => "top",
            TextVAlign::Middle => "middle",
            TextVAlign::Bottom => "bottom",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TextVAlign::Top | TextVAlign::Invalid => "Top",
            TextVAlign::Middle => "Middle",
            TextVAlign::Bottom => "Bottom",
        }
    }

    /// `top`, `middle` or `bottom` in any case, trimmed; `center` and `centre` read as middle.
    pub fn from_name(s: &str) -> Option<TextVAlign> {
        let s = s.trim();
        TextVAlign::ALL
            .into_iter()
            .find(|v| v.code().eq_ignore_ascii_case(s))
            .or((s.eq_ignore_ascii_case("centre") || s.eq_ignore_ascii_case("center")).then_some(TextVAlign::Middle))
    }

    /// This value, or the default where it is [`TextVAlign::Invalid`].
    pub fn kept(self) -> TextVAlign {
        if self == TextVAlign::Invalid { TextVAlign::default() } else { self }
    }
}

impl Serialize for TextVAlign {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.kept().code())
    }
}

impl<'de> Deserialize<'de> for TextVAlign {
    /// A name as [`TextVAlign::from_name`] reads it; null as the default; anything else (an
    /// unknown name, a number, a list) as `Invalid`, so the read never fails and `Layout::clamp`
    /// repairs and counts it.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<TextVAlign, D::Error> {
        let v: Option<serde_json::Value> = Deserialize::deserialize(d)?;
        Ok(match v {
            None | Some(serde_json::Value::Null) => TextVAlign::default(),
            Some(serde_json::Value::String(s)) => TextVAlign::from_name(&s).unwrap_or(TextVAlign::Invalid),
            Some(_) => TextVAlign::Invalid,
        })
    }
}

/// Whether a save leaves a text's block alignment out: the top, or a value that is not a name
/// ([`TextVAlign::kept`]), which reads back as the top.
fn is_top(v: &TextVAlign) -> bool {
    v.kept() == TextVAlign::Top
}

/// The case a text element is drawn in.
///
/// `Invalid` is what a file's unknown name or wrong kind of value is read as; it is never kept:
/// [`Layout::clamp`] sends it to the default and counts the element, and a save writes the
/// default's name for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextCase {
    /// As typed.
    #[default]
    None,
    /// All upper case.
    Upper,
    /// All lower case.
    Lower,
    /// A value read from a file that is not a name; repaired to the default on load.
    #[doc(hidden)]
    Invalid,
}

impl TextCase {
    pub const ALL: [TextCase; 3] = [TextCase::None, TextCase::Upper, TextCase::Lower];

    pub fn code(self) -> &'static str {
        match self {
            TextCase::None | TextCase::Invalid => "none",
            TextCase::Upper => "upper",
            TextCase::Lower => "lower",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TextCase::None | TextCase::Invalid => "As typed",
            TextCase::Upper => "Upper case",
            TextCase::Lower => "Lower case",
        }
    }

    /// `none`, `upper` or `lower` in any case, trimmed.
    pub fn from_name(s: &str) -> Option<TextCase> {
        let s = s.trim();
        TextCase::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s))
    }

    /// This value, or the default where it is [`TextCase::Invalid`].
    pub fn kept(self) -> TextCase {
        if self == TextCase::Invalid { TextCase::default() } else { self }
    }

    /// `s` in this case (full Unicode case mapping: `ß` is `SS` in upper case).
    pub fn apply(self, s: String) -> String {
        match self.kept() {
            TextCase::Upper => s.to_uppercase(),
            TextCase::Lower => s.to_lowercase(),
            _ => s,
        }
    }
}

impl Serialize for TextCase {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.kept().code())
    }
}

impl<'de> Deserialize<'de> for TextCase {
    /// A name as [`TextCase::from_name`] reads it; null as the default; anything else as
    /// `Invalid`, so the read never fails and `Layout::clamp` repairs and counts it.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<TextCase, D::Error> {
        let v: Option<serde_json::Value> = Deserialize::deserialize(d)?;
        Ok(match v {
            None | Some(serde_json::Value::Null) => TextCase::default(),
            Some(serde_json::Value::String(s)) => TextCase::from_name(&s).unwrap_or(TextCase::Invalid),
            Some(_) => TextCase::Invalid,
        })
    }
}

/// Whether a save leaves a text's case out: as typed, or a value that is not a name
/// ([`TextCase::kept`]), which reads back as typed.
fn is_no_case(v: &TextCase) -> bool {
    v.kept() == TextCase::None
}

/// The usual spacing between a text's lines, as a multiple of it.
pub const LINE_SPACING: f64 = 1.0;
/// The tightest and widest a text's line spacing may be, as multiples of the usual.
pub const MIN_LINE_SPACING: f64 = 0.5;
pub const MAX_LINE_SPACING: f64 = 3.0;

fn line_spacing() -> f64 {
    LINE_SPACING
}

/// Whether a save leaves a text's line spacing out: the usual, or one not finite, which
/// [`line_spacing_in_range`] reads as the usual.
fn is_line_spacing(v: &f64) -> bool {
    !v.is_finite() || *v == LINE_SPACING
}

/// A text's line spacing as it is drawn and kept: the usual one ([`LINE_SPACING`]) when it is not
/// a finite number, else clamped to [`MIN_LINE_SPACING`]..=[`MAX_LINE_SPACING`]. The one reader
/// for the range, so a layout's repair, its capability check and the page agree.
pub fn line_spacing_in_range(v: f64) -> f64 {
    finite_or(v, LINE_SPACING).clamp(MIN_LINE_SPACING, MAX_LINE_SPACING)
}

/// A text's line spacing as a file holds it: a number as it is (out of range is for
/// `Layout::clamp`), null as the usual, anything else (a string, a flag, a list, an object) as NaN,
/// which `Layout::clamp` brings back to the usual and counts.
fn de_line_spacing<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v: Option<serde_json::Value> = Deserialize::deserialize(d)?;
    Ok(match v {
        None | Some(serde_json::Value::Null) => LINE_SPACING,
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(_) => f64::NAN,
    })
}

/// The most layers a legend leaves out.
pub const MAX_LEGEND_EXCLUDED: usize = 1000;

/// The most layers a legend labels.
pub const MAX_LEGEND_LABELS: usize = 1000;

/// The most characters of a legend's label for a layer.
pub const MAX_LEGEND_LABEL_CHARS: usize = 200;

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

/// The most subdivisions the first division after zero of a scale bar is split into.
pub const MAX_SCALE_SUBDIVISIONS: usize = 10;

/// The most divisions a scale bar draws to the left of its zero.
pub const MAX_SCALE_BEFORE_ZERO: usize = 2;

/// The most decimal places a scale bar's numbers show.
pub const MAX_SCALE_DECIMALS: usize = 3;

/// The most characters a scale bar's unit label has.
pub const MAX_UNIT_LABEL_CHARS: usize = 100;

/// Where a scale bar's numbers sit against the bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScaleLabelPosition {
    /// Over the bar.
    #[default]
    Above,
    /// Under the bar.
    Below,
}

impl ScaleLabelPosition {
    pub const ALL: [ScaleLabelPosition; 2] = [ScaleLabelPosition::Above, ScaleLabelPosition::Below];

    pub fn code(self) -> &'static str {
        match self {
            ScaleLabelPosition::Above => "above",
            ScaleLabelPosition::Below => "below",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScaleLabelPosition::Above => "Above the bar",
            ScaleLabelPosition::Below => "Below the bar",
        }
    }

    pub fn from_name(s: &str) -> Option<ScaleLabelPosition> {
        ScaleLabelPosition::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// Where a scale bar's unit label sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnitLabelPosition {
    /// After the last number, on the same line.
    #[default]
    AfterLabels,
    /// Beside the end of the bar.
    AfterBar,
    /// Over the bar.
    Above,
    /// Under the bar.
    Below,
}

impl UnitLabelPosition {
    pub const ALL: [UnitLabelPosition; 4] =
        [UnitLabelPosition::AfterLabels, UnitLabelPosition::AfterBar, UnitLabelPosition::Above, UnitLabelPosition::Below];

    pub fn code(self) -> &'static str {
        match self {
            UnitLabelPosition::AfterLabels => "afterLabels",
            UnitLabelPosition::AfterBar => "afterBar",
            UnitLabelPosition::Above => "above",
            UnitLabelPosition::Below => "below",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UnitLabelPosition::AfterLabels => "After the labels",
            UnitLabelPosition::AfterBar => "After the bar",
            UnitLabelPosition::Above => "Above the bar",
            UnitLabelPosition::Below => "Below the bar",
        }
    }

    pub fn from_name(s: &str) -> Option<UnitLabelPosition> {
        UnitLabelPosition::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// What a scale bar gives way on when its length does not fit its box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScaleBarFit {
    /// Keep the number of divisions and the bar's width; the value of a division changes.
    #[default]
    AdjustDivisionValue,
    /// Keep the value of a division and the bar's width; the number of divisions changes.
    AdjustDivision,
    /// Keep the value and the number of divisions; the bar's width changes.
    AdjustWidth,
}

impl ScaleBarFit {
    pub const ALL: [ScaleBarFit; 3] = [ScaleBarFit::AdjustDivisionValue, ScaleBarFit::AdjustDivision, ScaleBarFit::AdjustWidth];

    pub fn code(self) -> &'static str {
        match self {
            ScaleBarFit::AdjustDivisionValue => "adjustDivisionValue",
            ScaleBarFit::AdjustDivision => "adjustDivision",
            ScaleBarFit::AdjustWidth => "adjustWidth",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScaleBarFit::AdjustDivisionValue => "Adjust division value",
            ScaleBarFit::AdjustDivision => "Adjust number of divisions",
            ScaleBarFit::AdjustWidth => "Adjust width",
        }
    }

    pub fn from_name(s: &str) -> Option<ScaleBarFit> {
        ScaleBarFit::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// The most columns a legend lays its rows in.
pub const MAX_LEGEND_COLUMNS: usize = 3;

fn one_column() -> usize {
    1
}

fn is_one_column(columns: &usize) -> bool {
    *columns == 1
}

/// A legend's text size until it is set (#2581), in points: its layers' names and classes'
/// labels, as legends have always drawn them.
pub const LEGEND_TEXT_PT: f64 = 9.0;
/// A scale bar's labels' size until it is set (#2581), in points, as scale bars have always
/// drawn them.
pub const SCALE_BAR_TEXT_PT: f64 = 8.0;
/// A grid's labels' size until it is set (#2581), in points, as grids have always drawn them.
pub const GRID_LABEL_PT: f64 = 8.0;
/// The sizes a legend's, a scale bar's or a grid's text takes, in points (#2581).
pub const LAYOUT_TEXT_SIZE: std::ops::RangeInclusive<f64> = 4.0..=100.0;

/// A legend's, scale bar's or grid's text size kept within [`LAYOUT_TEXT_SIZE`], its `default`
/// when it is no number: what a layout opened and the drawing take, so they agree (a command
/// refuses one out of range).
pub fn layout_text_in_range(pt: f64, default: f64) -> f64 {
    finite_or(pt, default).clamp(*LAYOUT_TEXT_SIZE.start(), *LAYOUT_TEXT_SIZE.end())
}

/// How many times its `default` a text of `pt` points is drawn, `pt` brought into range as
/// [`layout_text_in_range`] does: what a legend's rows, a scale bar's or a grid's labels and
/// their room are scaled by (#2581).
pub fn layout_text_scale(pt: f64, default: f64) -> f64 {
    layout_text_in_range(pt, default) / default
}

fn legend_text_pt() -> f64 {
    LEGEND_TEXT_PT
}

fn is_legend_text_pt(size: &f64) -> bool {
    *size == LEGEND_TEXT_PT
}

fn scale_bar_text_pt() -> f64 {
    SCALE_BAR_TEXT_PT
}

fn is_scale_bar_text_pt(size: &f64) -> bool {
    *size == SCALE_BAR_TEXT_PT
}

fn grid_label_pt() -> f64 {
    GRID_LABEL_PT
}

fn is_grid_label_pt(size: &f64) -> bool {
    *size == GRID_LABEL_PT
}

/// The dark grey a scale bar draws in by default, and a legend, colorbar, table or chart without
/// an ink of its own ([`ElementKind::ink`]).
pub const INK: Rgba = Rgba::rgb(0x22, 0x22, 0x22);

fn ink() -> Rgba {
    INK
}

fn white() -> Rgba {
    Rgba::rgb(0xFF, 0xFF, 0xFF)
}

fn yes() -> bool {
    true
}

/// Which way a colorbar runs, or a chart's bars.
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
        /// The outline drawn round the frame, along its shape: its colour and width (points), 0
        /// for none (#2557). Dark grey and 0.8 unless set, and left out then, so layouts saved
        /// before keep their bytes.
        #[serde(default = "frame_border_color", skip_serializing_if = "is_frame_border_color")]
        border_color: Rgba,
        #[serde(default = "frame_border_width", skip_serializing_if = "is_frame_border_width")]
        border_width: f64,
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
        /// The size of its labels, in points, within [`LAYOUT_TEXT_SIZE`] (#2581): the room over
        /// the bar and between labels follows in proportion, the bar keeping its size.
        /// [`SCALE_BAR_TEXT_PT`] unless set, and left out then, so layouts saved before keep their
        /// bytes.
        #[serde(default = "scale_bar_text_pt", skip_serializing_if = "is_scale_bar_text_pt")]
        text_size: f64,
        /// Parts the first division after zero is split into, 0 (none) to [`MAX_SCALE_SUBDIVISIONS`].
        #[serde(default, skip_serializing_if = "is_default")]
        subdivisions: usize,
        /// Divisions drawn left of zero, 0 to [`MAX_SCALE_BEFORE_ZERO`].
        #[serde(default, skip_serializing_if = "is_default")]
        divisions_before_zero: usize,
        /// Where the numbers sit against the bar.
        #[serde(default, skip_serializing_if = "is_default")]
        label_position: ScaleLabelPosition,
        /// Text printed in place of the units' abbreviation. `None` prints the abbreviation and
        /// `Some("")` prints nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit_label: Option<String>,
        /// Where the unit label sits.
        #[serde(default, skip_serializing_if = "is_default")]
        unit_label_position: UnitLabelPosition,
        /// Decimal places in the numbers, 0 to [`MAX_SCALE_DECIMALS`], their thousands set apart;
        /// none (auto), the decimals each label needs, as bars always printed.
        #[serde(default, deserialize_with = "de_scale_decimals", skip_serializing_if = "Option::is_none")]
        decimals: Option<u8>,
        /// What gives way when the bar's length does not fit its box.
        #[serde(default, skip_serializing_if = "is_default")]
        fit: ScaleBarFit,
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
        /// Include the grey fallback for Graduated Colors' missing numeric values. Absent in
        /// older layouts, whose appearance is preserved; this changes only the legend.
        #[serde(default, skip_serializing_if = "is_false")]
        show_no_data: bool,
        /// Layers (by name, any case) the legend leaves out though the map draws them: a mask, a
        /// highlight, a basemap-like layer. Empty, it lists every layer it would otherwise.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude: Vec<String>,
        /// The columns its rows are laid in, 1 to [`MAX_LEGEND_COLUMNS`], under its title: as even
        /// as they can be, a layer's name staying with the row under it.
        #[serde(default = "one_column", skip_serializing_if = "is_one_column")]
        columns: usize,
        /// The label it shows for a layer instead of the layer's name, by the layer's name (any
        /// case, as the map spells it): its row, or the name over its classes (#1654). The layer
        /// keeps its name everywhere else. A layer not in it shows its name.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        labels: BTreeMap<String, String>,
        /// The colour of its text and lines (#2257); none, the dark grey of [`INK`]. Left out
        /// when none, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Rgba>,
        /// The size of its layers' names and its classes' labels, in points, within
        /// [`LAYOUT_TEXT_SIZE`] (#2581): its title, rows, swatches and gaps follow in proportion,
        /// a point symbol keeping the size the map draws it at. [`LEGEND_TEXT_PT`] unless set,
        /// and left out then, so layouts saved before keep their bytes.
        #[serde(default = "legend_text_pt", skip_serializing_if = "is_legend_text_pt")]
        text_size: f64,
    },
    /// The colour scale of a raster layer in a frame's map: a gradient bar for a stretch, steps
    /// for classes, a block per value for unique values and colour tables (#326).
    Colorbar {
        frame: u32,
        /// The raster layer (by name); `None` is the frame map's top shown raster with a colour scale
        /// that isn't drawn by classes ([`colorbar_layer`]).
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
        /// The colour of its text and lines (#2257); none, the dark grey of [`INK`]. Left out
        /// when none, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Rgba>,
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
        /// Turned about the centre of its box, in degrees clockwise as a frame's `bearing` is
        /// (90 reads top to bottom, -90 bottom to top), -180 to 180 ([`rotation()`]); the element's
        /// box stays the unturned one. Left out when 0, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_zero")]
        rotation: f64,
        /// Broken into lines at spaces to fit the width of its (unturned) box when drawn, after
        /// its dynamic text is filled in (#2346); off, its lines break only where the text has a
        /// line break. Left out when off, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_false")]
        wrap: bool,
        /// Where its block of lines sits down the (unturned) box. Left out when top, so layouts
        /// saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_top")]
        valign: TextVAlign,
        /// The space between its lines, as a multiple of the usual ([`MIN_LINE_SPACING`] to
        /// [`MAX_LINE_SPACING`]). Left out when usual, so layouts saved before keep their bytes.
        #[serde(default = "line_spacing", skip_serializing_if = "is_line_spacing", deserialize_with = "de_line_spacing")]
        line_spacing: f64,
        /// Its words in upper or lower case when drawn, after its dynamic text is filled in. Left
        /// out when as typed, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_no_case")]
        case: TextCase,
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
        /// The colour of its text and lines (#2257); none, the dark grey of [`INK`]. Left out
        /// when none, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Rgba>,
    },
    /// A bar chart of a numeric field of a layer in a frame's map: one bar per feature, or one
    /// per value of a `group` field ([`ElementKind::chart_data`], #2487).
    Chart {
        frame: u32,
        layer: String,
        field: String,
        /// Bars are labelled with this field, if set (not read when grouped: a bar is labelled
        /// with its value of `group`).
        #[serde(default)]
        label: Option<String>,
        /// The colour of its labels and title (#2257); none, the dark grey of [`INK`]. Its bars
        /// keep their own colour. Left out when none, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Rgba>,
        /// One bar per value of this field, its height `statistic` of `field` over the features
        /// with that value; none, one bar per feature. The fields below are left out at their
        /// defaults, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
        /// What a grouped bar measures (sum by default); a bar per feature is its own value.
        #[serde(default, skip_serializing_if = "is_default")]
        statistic: StatKind,
        #[serde(default, skip_serializing_if = "is_default")]
        order: ChartOrder,
        /// The most bars drawn, 1 to [`MAX_CHART_BARS`] (read clamped).
        #[serde(default = "chart_bars", skip_serializing_if = "is_chart_bars")]
        bars: usize,
        /// The bars past `bars` gathered in a last "Other" bar (so `bars` - 1 of their own),
        /// rather than left out.
        #[serde(default, skip_serializing_if = "is_false")]
        other: bool,
        /// Bars running across (the default) or columns standing up ([`ChartLook`]). This and the
        /// options below are left out at their defaults, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_default")]
        orientation: Orientation,
        /// Each bar's value written past its end.
        #[serde(default, skip_serializing_if = "is_false")]
        values: bool,
        /// The decimals those values are written with, 0 to [`MAX_CHART_DECIMALS`]; none, up to
        /// two, the zeros they end in dropped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decimals: Option<u8>,
        /// Its own title, "" for none; none, the title it is given ([`ElementKind::chart_title`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Its bars' colour; none, [`CHART_BAR`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bar_color: Option<Rgba>,
        /// Where bars running across have their labels in the labels' column: at its left (the
        /// default), or at its right, against the bars. Columns have theirs centred under them.
        #[serde(default, skip_serializing_if = "is_left")]
        label_align: TextAlign,
        /// Each bar in the colour its features have on the map, by the layer's symbology classes
        /// (#2487 part 3): a bar per feature its feature's, a grouped bar the colour its features
        /// share. A bar whose features have several, and an Other bar, keep `bar_color`.
        #[serde(default, skip_serializing_if = "is_false")]
        classes: bool,
        /// Each grouped bar split by the values of this field, a stretch per value, with a key over
        /// the bars (#2487 part 3b). Read only when the chart groups by another field and sums or
        /// counts, so that its stretches add up to their bar ([`ElementKind::chart_stack`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stack: Option<String>,
        /// Drawn as its mirror image, its text reading as ever (#2487 part 4): bars running across
        /// from the right, their labels at the right; columns hanging from the top, their labels
        /// over them. Beside a chart drawn the usual way, the two face each other.
        #[serde(default, skip_serializing_if = "is_false")]
        mirror: bool,
        /// The value at the far end of its bars' room, so that charts given the same one share a
        /// scale; a bar past it runs to that end. None, or one not above 0 (not read), its bars'
        /// own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// Its labels left out, their room given to the bars.
        #[serde(default, skip_serializing_if = "is_false")]
        hide_labels: bool,
        /// A line round each bar, or each stretch of a stacked one; none by default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bar_outline: Option<ChartOutline>,
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
        /// Dashed: the pattern along the outline, in points, as a line layer's stroke takes it
        /// ([`Dash`]); `None` is solid. Left out when solid, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::symbology::de_dash")]
        dash: Option<Dash>,
    },
    /// An ellipse filling the element's box: a circle in a square one.
    Ellipse {
        fill: Rgba,
        outline: Rgba,
        width: f64,
        /// Dashed: the pattern along the outline, in points, as a line layer's stroke takes it
        /// ([`Dash`]); `None` is solid. Left out when solid, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::symbology::de_dash")]
        dash: Option<Dash>,
    },
    /// A straight line across the element's box, `width` points wide.
    Line {
        color: Rgba,
        width: f64,
        #[serde(default)]
        dir: LineDir,
        /// Arrowheads: none makes it a plain line.
        #[serde(default)]
        head: LineHead,
        /// Dashed: the pattern along the line, in points, as a line layer's stroke takes it
        /// ([`Dash`]); `None` is solid. Only the shaft is dashed: arrowheads stay solid. Left out
        /// when solid, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::symbology::de_dash")]
        dash: Option<Dash>,
    },
}

fn rows() -> usize {
    12
}

/// The bars a chart draws unless told otherwise, and the most it takes.
pub const CHART_BARS: usize = 20;
pub const MAX_CHART_BARS: usize = 100;

fn chart_bars() -> usize {
    CHART_BARS
}

fn is_chart_bars(n: &usize) -> bool {
    *n == CHART_BARS
}

/// What a file's scale bar decimals that are no count at all (negative, fractional, not a number)
/// read as: read from a file only, never kept. `Layout::clamp` sends it back to automatic and
/// counts it.
const SCALE_DECIMALS_INVALID: u8 = u8::MAX;

/// A scale bar's decimals as a file holds them, of whatever JSON kind: null or the word "auto"
/// (any case, trimmed) as automatic, a whole number from 0 to 253 as it is, a whole number past
/// that as 254 (more than any count, so `Layout::clamp` cuts it to the most), and anything else
/// (negative, fractional, another word, a flag, a list, an object) as
/// [`SCALE_DECIMALS_INVALID`]. So `Layout::clamp` brings every invalid value into range and says
/// so, rather than the whole layout failing to load or a bad value passing as a good one.
fn de_scale_decimals<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u8>, D::Error> {
    let v: Option<serde_json::Value> = serde::Deserialize::deserialize(d)?;
    Ok(match v {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) if s.trim().eq_ignore_ascii_case("auto") => None,
        Some(serde_json::Value::Number(n)) => {
            if let Some(whole) = n.as_u64() {
                // 254 is the cap.
                Some(whole.min(254) as u8)
            } else {
                match n.as_f64() {
                    // Whole and not negative (a float such as 2.0 or 1e30): 254 is the cap.
                    Some(f) if f.fract() == 0.0 && f >= 0.0 => Some(f.min(254.0) as u8),
                    _ => Some(SCALE_DECIMALS_INVALID),
                }
            }
        }
        Some(_) => Some(SCALE_DECIMALS_INVALID),
    })
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// How a chart orders its bars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChartOrder {
    /// The largest value first.
    #[default]
    Descending,
    /// The smallest value first.
    Ascending,
    /// By label, A to Z (case aside).
    Category,
    /// As the features come in the layer (grouped, as each value first comes).
    Layer,
}

impl ChartOrder {
    pub const ALL: [ChartOrder; 4] = [ChartOrder::Descending, ChartOrder::Ascending, ChartOrder::Category, ChartOrder::Layer];
    pub fn code(self) -> &'static str {
        match self {
            ChartOrder::Descending => "descending",
            ChartOrder::Ascending => "ascending",
            ChartOrder::Category => "category",
            ChartOrder::Layer => "layer",
        }
    }
    pub fn from_code(s: &str) -> Option<ChartOrder> {
        ChartOrder::ALL.into_iter().find(|o| o.code().eq_ignore_ascii_case(s))
    }
    pub fn label(self) -> &'static str {
        match self {
            ChartOrder::Descending => "Largest first",
            ChartOrder::Ascending => "Smallest first",
            ChartOrder::Category => "By label",
            ChartOrder::Layer => "Layer order",
        }
    }
}

/// The most decimals a chart writes its values with.
pub const MAX_CHART_DECIMALS: u8 = 6;

/// A chart's bars' colour unless given one.
pub const CHART_BAR: Rgba = Rgba::rgb(0x3B, 0x82, 0xC4);

/// How a chart draws its bars ([`ElementKind::chart_look`]), its data aside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartLook {
    pub orientation: Orientation,
    pub values: bool,
    pub decimals: Option<u8>,
    pub bar_color: Rgba,
    pub label_align: TextAlign,
    /// Drawn as its mirror image ([`ElementKind::Chart`]'s `mirror`).
    pub mirror: bool,
    /// The value at the far end of its bars' room, above 0; none, its bars' own.
    pub max: Option<f64>,
    pub hide_labels: bool,
    /// The line round each bar, as it is read ([`chart_outline`]); none, no line.
    pub bar_outline: Option<ChartOutline>,
}

/// A chart's bar outline (#2487 part 4b): its colour and width in points. A key left out of a
/// hand-edited file takes the one it is first given ([`CHART_OUTLINE`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartOutline {
    #[serde(default = "chart_outline_color")]
    pub color: Rgba,
    #[serde(default = "chart_outline_width")]
    pub width: f64,
}

fn chart_outline_color() -> Rgba {
    CHART_OUTLINE.color
}

fn chart_outline_width() -> f64 {
    CHART_OUTLINE.width
}

/// A bar outline's colour and width when first given, and the widest it is drawn.
pub const CHART_OUTLINE: ChartOutline = ChartOutline { color: Rgba::rgb(0x22, 0x22, 0x22), width: 0.5 };
pub const MAX_CHART_OUTLINE_WIDTH: f64 = 5.0;

/// A chart's bar outline as it is read: a width above 0 and finite, drawn at most
/// [`MAX_CHART_OUTLINE_WIDTH`]; else none.
pub fn chart_outline(o: Option<ChartOutline>) -> Option<ChartOutline> {
    o.filter(|o| o.width.is_finite() && o.width > 0.0).map(|o| ChartOutline { width: o.width.min(MAX_CHART_OUTLINE_WIDTH), ..o })
}

impl Default for ChartLook {
    fn default() -> Self {
        ChartLook {
            orientation: Orientation::Horizontal,
            values: false,
            decimals: None,
            bar_color: CHART_BAR,
            label_align: TextAlign::Left,
            mirror: false,
            max: None,
            hide_labels: false,
            bar_outline: None,
        }
    }
}

/// A chart's fixed maximum as it is read: above 0 and finite, else none (its bars' own).
pub fn chart_max(max: Option<f64>) -> Option<f64> {
    max.filter(|m| m.is_finite() && *m > 0.0)
}

/// A chart's value as written on its bar: its thousands apart with commas, with `decimals`
/// decimals, or none given, up to two, the zeros they end in dropped (`2,095`, `13.3`, `-0.25`).
/// The point is a decimal point; a layout with a decimal comma swaps the two when it prints it.
pub fn chart_value_text(v: f64, decimals: Option<u8>) -> String {
    let d = decimals.unwrap_or(2).min(MAX_CHART_DECIMALS) as usize;
    let mut s = format!("{:.*}", d, v.abs());
    if decimals.is_none() && s.contains('.') {
        s.truncate(s.trim_end_matches('0').trim_end_matches('.').len());
    }
    let (whole, fraction) = s.split_once('.').map_or((s.as_str(), None), |(w, f)| (w, Some(f)));
    let mut out = String::new();
    // Rounded to nothing, a negative value is written as 0, not -0.
    if v < 0.0 && s.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
        out.push('-');
    }
    out.push_str(&crate::graphics::commas(whole));
    if let Some(f) = fraction {
        out.push('.');
        out.push_str(f);
    }
    out
}

/// What a bar with no value of a chart's `group` field is labelled, as a report's chart does.
pub const CHART_BLANK: &str = "(blank)";
/// The label of the bar gathering a chart's bars past its limit.
pub const CHART_OTHER: &str = "Other";

/// The bars a chart draws from its layer, and what it leaves out of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartData {
    /// Each bar's value and label, in the order drawn.
    pub bars: Vec<(f64, String)>,
    /// Whether its bars have labels: when grouped, or labelled by a field.
    pub labelled: bool,
    /// Bars past its limit, left out (none when gathered in an "Other" bar).
    pub left_out: usize,
    /// Bars whose statistic is past a number's range (a sum or mean overflowing), left out; an
    /// Other bar's counts the bars it would gather.
    pub out_of_range: usize,
    /// Features with no number in its field (not a number, or not finite), left out; a count
    /// counts them.
    pub no_number: usize,
    /// Its title: the field, or the statistic of the field by the group.
    pub title: String,
    /// Coloured by classes, each bar's colour as its features have it on the map, in the order of
    /// `bars`: `None` keeps the chart's bar colour (an Other bar, or a bar whose features have
    /// several or none). Empty when not coloured by classes, and when stacked: its stretches
    /// take the colours of its `key`.
    pub colors: Vec<Option<Rgba>>,
    /// Bars drawn whose features have several colours on the map, which keep the chart's.
    pub mixed: usize,
    /// Bars drawn whose features show no colour on the map (a hidden class, points drawn as an
    /// image), which keep the chart's.
    pub unshown: usize,
    /// Stacked, the key: each value of the stack field the bars drawn show, with its colour, in the
    /// order its stretch takes along every bar, a last [`CHART_OTHER`] gathering the values past
    /// [`CHART_STACKS`]. Empty when not stacked.
    pub key: Vec<(String, Rgba)>,
    /// Stacked, each bar's stretches in the order of `key` (0 where the bar has none of that
    /// value), which add up to the bar's value. Empty when not stacked.
    pub stretches: Vec<Vec<f64>>,
    /// Stacked and coloured by classes, the values of the key that keep their palette colour:
    /// their features have several colours on the map, or show none.
    pub key_kept: usize,
    /// Stacked, the values the bars drawn show past [`CHART_STACKS`], gathered in the key's Other.
    pub key_gathered: usize,
    /// Stacked, the bars drawn whole (their `stretches` empty): a stretch of theirs, or their
    /// stretches stacked, add up past a number's range, though their total doesn't.
    pub whole: usize,
    /// Its stack field isn't in the layer (deleted since, say): its bars are drawn whole.
    pub stack_gone: bool,
}

/// The most values of a stack field a chart's key lists, one per colour of
/// [`crate::diagram::CATEGORY_COLORS`]: past it, all but the last, and an Other for the rest.
pub const CHART_STACKS: usize = crate::diagram::CATEGORY_COLORS.len();

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
            rotation: 0.0,
            wrap: false,
            valign: TextVAlign::Top,
            line_spacing: LINE_SPACING,
            case: TextCase::None,
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
            text_size: SCALE_BAR_TEXT_PT,
            subdivisions: 0,
            divisions_before_zero: 0,
            label_position: ScaleLabelPosition::default(),
            unit_label: None,
            unit_label_position: UnitLabelPosition::default(),
            decimals: None,
            fit: ScaleBarFit::default(),
        }
    }

    /// Whether this is a text whose block alignment, line spacing or case is not the default, once
    /// brought into range as [`Layout::clamp`] does: what a layout file needs a newer reader for.
    pub fn text_uses_block(&self) -> bool {
        let ElementKind::Text { valign, line_spacing, case, .. } = self else {
            return false;
        };
        valign.kept() != TextVAlign::Top || case.kept() != TextCase::None || line_spacing_in_range(*line_spacing) != LINE_SPACING
    }

    /// Whether this is a scale bar with any of the details set: subdivisions, divisions before
    /// zero, a label or unit label placement, a unit label, decimals or a fit. Its text size
    /// isn't one: [`ElementKind::has_custom_text_size`] tells that.
    pub fn scale_bar_uses_details(&self) -> bool {
        let ElementKind::ScaleBar {
            frame,
            subdivisions,
            divisions_before_zero,
            label_position,
            unit_label,
            unit_label_position,
            decimals,
            fit,
            ..
        } = self
        else {
            return false;
        };
        let ElementKind::ScaleBar {
            subdivisions: s0,
            divisions_before_zero: z0,
            label_position: lp0,
            unit_label: u0,
            unit_label_position: up0,
            decimals: d0,
            fit: f0,
            ..
        } = ElementKind::scale_bar(*frame)
        else {
            return false;
        };
        // One subdivision is none: it clamps to 0, and an imported text is checked before that.
        // Before clamp a file's invalid decimals read as 255 (a detail): over-requires for an invalid file only.
        let subdivisions = if *subdivisions == 1 { 0 } else { *subdivisions };
        (subdivisions, *divisions_before_zero, *label_position, unit_label, *unit_label_position, *decimals, *fit)
            != (s0, z0, lp0, &u0, up0, d0, f0)
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
    /// Whether an element of this kind takes a background (#2257): all but a map frame, which
    /// draws its map and the paper behind it itself.
    pub fn takes_background(&self) -> bool {
        !matches!(self, ElementKind::MapFrame { .. })
    }

    /// Whether an element of this kind takes an ink (#2257): a legend, colorbar, table or chart,
    /// whose text and lines are otherwise dark grey. The other kinds' colours are their own options.
    pub fn takes_ink(&self) -> bool {
        matches!(self, ElementKind::Legend { .. } | ElementKind::Colorbar { .. } | ElementKind::Table { .. } | ElementKind::Chart { .. })
    }

    /// Whether `key` given as null changes an element of this kind, giving it back its default:
    /// an ink's `color`, a chart's `title` (the title it is given), `barColor`, `max` (its bars'
    /// own scale) and `barOutline` (none), a legend's or scale bar's `textSize` (#2581), and a
    /// scale bar's `unitLabel` (its unit's abbreviation).
    pub fn null_resets(&self, key: &str) -> bool {
        (self.takes_ink() && key == "color")
            || (matches!(self, ElementKind::Chart { .. }) && matches!(key, "title" | "barColor" | "max" | "barOutline"))
            || (matches!(self, ElementKind::Legend { .. } | ElementKind::ScaleBar { .. }) && key == "textSize")
            || (matches!(self, ElementKind::ScaleBar { .. }) && key == "unitLabel")
    }

    /// Its ink, when it [takes one](ElementKind::takes_ink) and has one; `None` draws in [`INK`].
    pub fn ink(&self) -> Option<Rgba> {
        match self {
            ElementKind::Legend { color, .. }
            | ElementKind::Colorbar { color, .. }
            | ElementKind::Table { color, .. }
            | ElementKind::Chart { color, .. } => *color,
            _ => None,
        }
    }

    /// Its ink to change, when it [takes one](ElementKind::takes_ink).
    pub fn ink_mut(&mut self) -> Option<&mut Option<Rgba>> {
        match self {
            ElementKind::Legend { color, .. }
            | ElementKind::Colorbar { color, .. }
            | ElementKind::Table { color, .. }
            | ElementKind::Chart { color, .. } => Some(color),
            _ => None,
        }
    }

    /// Whether it is a legend whose text, a scale bar whose labels or a map frame whose grid's
    /// labels aren't their default size (#2581).
    pub fn has_custom_text_size(&self) -> bool {
        match self {
            ElementKind::Legend { text_size, .. } => !is_legend_text_pt(text_size),
            ElementKind::ScaleBar { text_size, .. } => !is_scale_bar_text_pt(text_size),
            ElementKind::MapFrame { grid: Some(g), .. } => !is_grid_label_pt(&g.label_size),
            _ => false,
        }
    }

    /// Whether it is a map frame whose border isn't the default one (#2557).
    pub fn has_custom_border(&self) -> bool {
        matches!(self, ElementKind::MapFrame { border_color, border_width, .. }
            if !is_frame_border_color(border_color) || !is_frame_border_width(border_width))
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

    /// Whether a chart uses what a chart had none of before (grouping, its statistic, its order,
    /// its limit, an "Other" bar): an older reader would draw it as one bar per feature, the 20
    /// largest, and save it again without them; a statistic kept while ungrouped is lost so too.
    pub fn chart_uses_data_options(&self) -> bool {
        matches!(self, ElementKind::Chart { group, statistic, order, bars, other, .. }
            if group.is_some() || *statistic != StatKind::default() || *order != ChartOrder::default() || *bars != CHART_BARS || *other)
    }

    /// The bars a chart draws from `ds`, its layer's features (#2487); `None` for another kind, or
    /// when its field or group field isn't in `ds`. Ungrouped, a bar per feature with a number in
    /// its field; grouped, a bar per value of the group field ([`CHART_BLANK`] for none), its
    /// statistic of the field over that value's features. Ordered, then cut to its limit, the
    /// bars past it left out or gathered in a last [`CHART_OTHER`] bar, the statistic over their
    /// features (their sum, ungrouped), whatever the order.
    ///
    /// It holds a value and an index per feature with a number (16 bytes) ungrouped. Grouped, it
    /// holds a 48-byte tally and an index per value, with the value's name once: at most the
    /// group field's own text again when every feature has a value of its own; a number or a
    /// yes/no value also has its key and index. Labels are read only for the bars drawn, except
    /// sorted by label: then, while it sorts, a label and its lowercase copy are held for every
    /// bar not yet cut to its limit (every feature with a number, ungrouped), about the label
    /// field's own text twice over.
    ///
    /// It reads no colours ([`ChartData::colors`] stays empty): [`ElementKind::chart_data_of`] reads
    /// them from the layer for a chart coloured by classes.
    pub fn chart_data(&self, ds: &rustgis_data::Dataset) -> Option<ChartData> {
        self.chart_data_with(ds, None)
    }

    /// [`ElementKind::chart_data`] of `layer`'s features, a chart coloured by classes reading each
    /// feature's colour as the map frame draws it: in `env`, the frame's scale and the series
    /// page ([`crate::Layer::styler_with`]), by the feature's own geometry, the layer's where it
    /// has none ([`crate::symbology::Symbol::shown_colour`]). A feature its symbol draws nothing
    /// of, in a hidden class, has none. The layer's transparency isn't read: a bar is drawn whole.
    pub fn chart_data_of(&self, layer: &crate::Layer, env: &rustgis_data::query::Env) -> Option<ChartData> {
        if !self.chart_uses_classes() {
            return self.chart_data(&layer.dataset);
        }
        let styler = layer.styler_with(env.clone());
        let ds = &layer.dataset;
        let colour = |row: usize| {
            let g = ds.features.geometry(row).map_or(ds.geometry_type, rustgis_data::Geometry::kind);
            styler.symbol(row).shown_colour(g)
        };
        self.chart_data_with(ds, Some(&colour))
    }

    /// [`ElementKind::chart_data`], each bar coloured by `colour` (a feature row's colour, none for
    /// a feature drawn with nothing) when given: a bar per feature its row's, a grouped bar the
    /// colour all the features its statistic reads share (a count's every feature, another's those
    /// with a number), those drawn with nothing aside. A bar without one keeps the chart's.
    /// Grouped, it holds a colour (5 bytes) per value besides, and reads no more colours of a value
    /// once its features have two; each colour read is the feature's symbol, as drawing the map
    /// reads it.
    ///
    /// Stacked ([`ElementKind::chart_stack`]), the colours go to the values of the stack field
    /// instead: each one takes the colour all its features on the bars shown share, else its
    /// palette colour, so the key holds one colour per value. Stacked, it also holds a sum, a
    /// colour and two indexes (32 bytes, and the B-tree's own room) for each pair of a group value
    /// and a stack value that some feature has, at most one per feature, and the stack field's
    /// names once, as the group field's.
    fn chart_data_with(&self, ds: &rustgis_data::Dataset, colour: Option<&dyn Fn(usize) -> Option<Rgba>>) -> Option<ChartData> {
        let ElementKind::Chart { field, label, group, statistic, order, bars, other, .. } = self else { return None };
        let fi = ds.field_index(field)?;
        let gi = match group {
            Some(g) => Some(ds.field_index(g)?),
            None => None,
        };
        // Stacked only grouped by another field, summing or counting; a stack field the layer
        // hasn't (deleted since, say) leaves the bars whole, which `stack_gone` says.
        let si = self.chart_stack().and_then(|st| ds.field_index(st));
        let stack_gone = self.chart_stack().is_some() && si.is_none();
        let number = |row: usize| ds.value(row, fi).as_f64().filter(|v| v.is_finite());
        let (mut no_number, mut out_of_range) = (0, 0);
        // Grouped, a tally per value of the group field, in the order the values first come, and
        // each one's name (none: blank, apart from a value that reads "(blank)").
        let mut groups: Vec<Tally> = Vec::new();
        let mut names: Vec<Option<String>> = Vec::new();
        // Each bar's value and its group, or ungrouped its feature's row.
        let mut drawn: Vec<(f64, usize)> = Vec::new();
        // Coloured, grouped: the colour each value's features share, by the tallies' order.
        let mut shades: Vec<Shade> = Vec::new();
        // Stacked: the total of each pair of a group value and a stack value, and coloured the
        // colour the pair's features share, so a value's colour is read only over the bars shown.
        // Only a sum or a count stacks, so a total is all a stretch needs.
        let mut stacks = ValueIndex::default();
        let mut cells = std::collections::BTreeMap::<(usize, usize), (f64, Shade)>::new();
        // The pair the last features were in, their total and colour: features in a row of the
        // same pair (as sorted or nearby rows often are) are added up before the map is looked up.
        let mut run: Option<((usize, usize), (f64, Shade))> = None;
        let stat = match gi {
            Some(gi) => {
                let mut index = ValueIndex::default();
                for row in 0..ds.features.len() {
                    let at = index.slot(ds.value(row, gi));
                    if at == groups.len() {
                        groups.push(Tally::default());
                    }
                    let Some(t) = groups.get_mut(at) else { continue };
                    t.records += 1;
                    // A count reads no number.
                    let n = if *statistic == StatKind::Count { None } else { number(row) };
                    let counted = *statistic == StatKind::Count
                        || match n {
                            Some(n) => {
                                t.add(n, *statistic == StatKind::Mean);
                                true
                            }
                            None => {
                                no_number += 1;
                                false
                            }
                        };
                    if let Some(si) = si.filter(|_| counted) {
                        let pair = (at, stacks.slot(ds.value(row, si)));
                        // Another pair ends the run: its total and colour go to its pair's cell.
                        let same = run.as_ref().is_some_and(|(p, _)| *p == pair);
                        if !same && let Some((p, cell)) = run.replace((pair, (0.0, Shade::Unread))) {
                            add_cell(&mut cells, p, cell);
                        }
                        if let Some((_, (total, shade))) = &mut run {
                            // A sum adds the feature's number; a count, the feature.
                            *total += n.unwrap_or(1.0);
                            // Once mixed, a pair's colour is settled: no more symbols are read.
                            if let Some(colour) = colour.filter(|_| !matches!(shade, Shade::Mixed)) {
                                shade.add(colour(row));
                            }
                        }
                    } else if let Some(colour) = colour.filter(|_| counted) {
                        shades.resize(groups.len(), Shade::Unread);
                        if let Some(shade) = shades.get_mut(at).filter(|s| !matches!(s, Shade::Mixed)) {
                            shade.add(colour(row));
                        }
                    }
                }
                if let Some((pair, cell)) = run.take() {
                    add_cell(&mut cells, pair, cell);
                }
                // A value whose features have no number has no bar: they are counted in `no_number`;
                // one whose statistic is past a number's range, in `out_of_range`.
                names = index.names();
                for (i, t) in groups.iter().enumerate() {
                    match t.value(*statistic) {
                        Some(v) => drawn.push((v, i)),
                        None if t.numbers > 0 => out_of_range += 1,
                        None => {}
                    }
                }
                *statistic
            }
            None => {
                for row in 0..ds.features.len() {
                    match number(row) {
                        Some(n) => drawn.push((n, row)),
                        None => no_number += 1,
                    }
                }
                StatKind::Sum
            }
        };
        let li = label.as_deref().and_then(|l| ds.field_index(l));
        let name = |i: usize| match gi {
            Some(_) => names.get(i).and_then(|n| n.as_deref()).unwrap_or(CHART_BLANK).to_string(),
            None => li.map(|l| ds.value(i, l).to_string()).unwrap_or_default(),
        };
        // Stable sorts: equal values keep the layer's order.
        match order {
            ChartOrder::Descending => drawn.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)),
            ChartOrder::Ascending => drawn.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)),
            ChartOrder::Category => drawn.sort_by_cached_key(|(_, i)| name(*i).to_lowercase()),
            ChartOrder::Layer => {}
        }
        let limit = (*bars).clamp(1, MAX_CHART_BARS);
        let mut left_out = 0;
        let mut gathered = None;
        // Stacked, the groups an Other bar gathers, whose stretches it adds up.
        let mut rest_groups: Vec<usize> = Vec::new();
        if drawn.len() > limit {
            if *other {
                let rest = drawn.split_off(limit - 1);
                let mut t = Tally::default();
                for (v, i) in &rest {
                    match gi {
                        Some(_) => t.merge(groups.get(*i)),
                        None => {
                            t.records += 1;
                            t.add(*v, false);
                        }
                    }
                }
                gathered = t.value(stat);
                // A gathered statistic past a number's range has no bar: its bars are said.
                if gathered.is_none() {
                    out_of_range += rest.len();
                } else if si.is_some() {
                    rest_groups = rest.iter().map(|(_, i)| *i).collect();
                }
            } else {
                left_out = drawn.len() - limit;
                drawn.truncate(limit);
            }
        }
        // Stacked, the key and each bar's stretches, an Other bar's from the groups it gathers.
        let (key, stretches, key_kept, key_gathered, whole) = match si {
            None => (Vec::new(), Vec::new(), 0, 0, 0),
            Some(_) => {
                let mut bar_of = vec![None; groups.len()];
                for (bar, (_, g)) in drawn.iter().enumerate() {
                    if let Some(b) = bar_of.get_mut(*g) {
                        *b = Some(bar);
                    }
                }
                for g in &rest_groups {
                    if let Some(b) = bar_of.get_mut(*g) {
                        *b = Some(drawn.len());
                    }
                }
                let bars = drawn.len() + usize::from(gathered.is_some());
                stack_bars(&cells, &bar_of, bars, stacks.names(), colour.is_some(), *order == ChartOrder::Category)
            }
        };
        // Coloured, each bar's colour, those without one counted; an Other bar keeps the chart's.
        let (mut mixed, mut unshown) = (0, 0);
        let colors: Vec<Option<Rgba>> = match colour.filter(|_| si.is_none()) {
            None => Vec::new(),
            Some(colour) => drawn
                .iter()
                .map(|(_, i)| {
                    let shown = match gi {
                        None => colour(*i),
                        Some(_) => match shades.get(*i) {
                            Some(Shade::One(c)) => Some(*c),
                            Some(Shade::Mixed) => {
                                mixed += 1;
                                return None;
                            }
                            _ => None,
                        },
                    };
                    if shown.is_none() {
                        unshown += 1;
                    }
                    shown
                })
                .chain(gathered.map(|_| None))
                .collect(),
        };
        let mut out: Vec<(f64, String)> = drawn.into_iter().map(|(v, i)| (v, name(i))).collect();
        out.extend(gathered.map(|v| (v, CHART_OTHER.to_string())));
        let title = self.chart_title().unwrap_or_default();
        // An Other bar is named, so it isn't taken for one feature's.
        let labelled = gi.is_some() || li.is_some() || gathered.is_some();
        Some(ChartData {
            bars: out,
            labelled,
            left_out,
            out_of_range,
            no_number,
            title,
            colors,
            mixed,
            unshown,
            key,
            stretches,
            key_kept,
            key_gathered,
            whole,
            stack_gone,
        })
    }

    /// A chart's title: its own, "" for none, or the title it is given: its field, or the
    /// statistic of its field by the group; `None` for another kind.
    pub fn chart_title(&self) -> Option<String> {
        match self {
            ElementKind::Chart { title: Some(t), .. } => Some(t.clone()),
            _ => self.chart_given_title(),
        }
    }

    /// The title a chart is given when it has none of its own: its field, or the statistic of
    /// its field by the group; `None` for another kind.
    pub fn chart_given_title(&self) -> Option<String> {
        let ElementKind::Chart { field, group, statistic, .. } = self else { return None };
        Some(match (group, statistic) {
            (Some(g), StatKind::Count) => format!("Count by {g}"),
            (Some(g), s) => format!("{} of {field} by {g}", s.label()),
            (None, _) => field.clone(),
        })
    }

    /// Whether a chart uses what a chart had none of before #2487's look options (columns,
    /// values, their decimals, its own title or none, a bar colour, labels against the bars): an
    /// older reader would draw it as bars running across in the default blue, with the title it
    /// is given, and save it again without them.
    pub fn chart_uses_look_options(&self) -> bool {
        // Only part 2's options: facing another chart is its own capability ([`Self::chart_uses_facing`]).
        let part_two = |l: ChartLook| ChartLook { mirror: false, max: None, hide_labels: false, bar_outline: None, ..l };
        matches!(self, ElementKind::Chart { title: Some(_), .. }) || self.chart_look().is_some_and(|l| part_two(l) != ChartLook::default())
    }

    /// Whether a chart colours its bars by its layer's classes, which a chart had no way to before
    /// #2487 part 3: an older reader would draw them all in its bar colour, and save it again
    /// without.
    pub fn chart_uses_classes(&self) -> bool {
        matches!(self, ElementKind::Chart { classes: true, .. })
    }

    /// The field a chart stacks its bars by: only when it groups by another field and sums or
    /// counts, so that a bar's stretches add up to it. Stored otherwise, it is drawn unstacked
    /// (commands refuse it or let it go, saying so).
    pub fn chart_stack(&self) -> Option<&str> {
        match self {
            ElementKind::Chart { stack: Some(st), group, statistic, .. }
                if chart_stack_refusal(group.as_deref(), *statistic, st).is_none() =>
            {
                Some(st)
            }
            _ => None,
        }
    }

    /// Whether a chart has a stack field (a blank one stacks nothing), which a chart had no way to
    /// before #2487 part 3b: an older reader would draw its bars whole, and save it again without.
    pub fn chart_uses_stack(&self) -> bool {
        matches!(self, ElementKind::Chart { stack: Some(s), .. } if !s.trim().is_empty())
    }

    /// How a chart draws its bars; `None` for another kind.
    pub fn chart_look(&self) -> Option<ChartLook> {
        let ElementKind::Chart { orientation, values, decimals, bar_color, label_align, mirror, max, hide_labels, bar_outline, .. } = self
        else {
            return None;
        };
        Some(ChartLook {
            orientation: *orientation,
            values: *values,
            decimals: *decimals,
            bar_color: bar_color.unwrap_or(CHART_BAR),
            label_align: *label_align,
            mirror: *mirror,
            max: chart_max(*max),
            hide_labels: *hide_labels,
            bar_outline: chart_outline(*bar_outline),
        })
    }

    /// Whether a chart faces another: mirrored, given a maximum or with its labels hidden, which a
    /// chart had no way to before #2487 part 4: an older reader would draw it the usual way round,
    /// on its bars' own scale, labelled, and save it again without. A maximum not above 0 is let go
    /// when the layout is read ([`Layout::clamp`]), so a stored one is always drawn to.
    pub fn chart_uses_facing(&self) -> bool {
        matches!(self, ElementKind::Chart { mirror, max, hide_labels, .. } if *mirror || *hide_labels || max.is_some())
    }

    /// Whether a chart draws a line round its bars, which a chart had no way to before #2487 part
    /// 4b: an older reader would draw them without, and save it again without.
    pub fn chart_uses_outline(&self) -> bool {
        matches!(self, ElementKind::Chart { bar_outline, .. } if chart_outline(*bar_outline).is_some())
    }
}

/// The colour a grouped bar's features share on the map ([`ElementKind::chart_data_of`]): none
/// read yet (or only features drawn with nothing), one, or several.
#[derive(Clone, Copy, Debug)]
enum Shade {
    Unread,
    One(Rgba),
    Mixed,
}

impl Shade {
    /// The colour two sets of features share: the one they both show, or either's when the other
    /// shows none.
    fn merge(&mut self, other: Shade) {
        match other {
            Shade::Unread => {}
            Shade::One(c) => self.add(Some(c)),
            Shade::Mixed => *self = Shade::Mixed,
        }
    }

    /// A feature drawn with nothing (a hidden class) shows no colour, so it leaves the bar the
    /// colour of the features that do show.
    fn add(&mut self, c: Option<Rgba>) {
        let Some(c) = c else { return };
        *self = match *self {
            Shade::Unread => Shade::One(c),
            Shade::One(o) if o == c => Shade::One(o),
            _ => Shade::Mixed,
        };
    }
}

/// The values of a field a chart groups or stacks its bars by, each numbered as it first comes
/// ([`ElementKind::chart_data`]). Each name is held once, in `index`; a text is looked up as it
/// is, and only a new value's name is copied. A number or a yes/no is looked up by its value in
/// `by_value` first, so it is written out once per value, not per feature: a value first seen goes
/// through `index` by its name, so values written alike (3 and 3.0) stay one. No value, or only
/// spaces, is the one blank value (apart from a value that reads "(blank)").
#[derive(Default)]
struct ValueIndex {
    index: std::collections::HashMap<String, usize>,
    by_value: std::collections::HashMap<GroupKey, usize>,
    blank: Option<usize>,
    len: usize,
}

impl ValueIndex {
    /// The number of `value`, a new one for a value not seen before.
    fn slot(&mut self, value: &rustgis_data::Value) -> usize {
        match (value, GroupKey::of(value)) {
            (rustgis_data::Value::Null, _) => self.named(None),
            (rustgis_data::Value::Text(t), _) => self.named(Some(t)),
            (v, Some(key)) => match self.by_value.get(&key) {
                Some(at) => *at,
                None => {
                    let at = self.named(Some(&v.to_string()));
                    self.by_value.insert(key, at);
                    at
                }
            },
            (v, None) => self.named(Some(&v.to_string())),
        }
    }

    fn named(&mut self, text: Option<&str>) -> usize {
        let at = match text.filter(|t| !t.trim().is_empty()) {
            None => match self.blank {
                Some(at) => return at,
                None => {
                    self.blank = Some(self.len);
                    self.len
                }
            },
            Some(t) => match self.index.get(t) {
                Some(at) => return *at,
                None => {
                    self.index.insert(t.to_string(), self.len);
                    self.len
                }
            },
        };
        self.len += 1;
        at
    }

    /// Each value's name by its number, none for the blank one.
    fn names(self) -> Vec<Option<String>> {
        let mut names = vec![None; self.len];
        for (name, i) in self.index {
            if let Some(slot) = names.get_mut(i) {
                *slot = Some(name);
            }
        }
        names
    }
}

/// Why a chart grouped by `group` and taking `statistic` can't stack its bars by `stack`; none
/// when it can (#2487 part 3b). It stacks grouped by another field, summing or counting, so that
/// its stretches add up to their bar. Commands refuse a stack given otherwise and let go one a
/// change leaves unable to stack, with this reason; the Format pane offers only fields it takes.
pub fn chart_stack_refusal(group: Option<&str>, statistic: StatKind, stack: &str) -> Option<&'static str> {
    match group {
        _ if stack.trim().is_empty() => Some("it names no field"),
        None => Some("it doesn't group its bars"),
        Some(g) if g.trim().eq_ignore_ascii_case(stack.trim()) => Some("it groups by that field"),
        Some(_) if !matches!(statistic, StatKind::Sum | StatKind::Count) => {
            Some("a mean, least or greatest of its stretches wouldn't add up to its bar")
        }
        Some(_) => None,
    }
}

/// Adds a run of features' total and colour to their pair's in `cells`.
fn add_cell(cells: &mut std::collections::BTreeMap<(usize, usize), (f64, Shade)>, pair: (usize, usize), (total, shade): (f64, Shade)) {
    let (t, s) = cells.entry(pair).or_insert((0.0, Shade::Unread));
    *t += total;
    s.merge(shade);
}

/// A stacked chart's key and stretches ([`ElementKind::chart_data`]) from the total of each pair of
/// a group value and a stack value (`cells`), the bar each group is drawn in (`bar_of`, an Other
/// bar's groups all in the last of `bars`) and the stack values' names; `coloured`, each pair's
/// colour gives a value the one its features share over the bars shown. The key holds the values
/// the bars show, by their total over them, largest first; past [`CHART_STACKS`], the largest all
/// but the last and an Other for the rest; `by_label`, those kept A to Z. Returns the key, each
/// bar's stretches by the key's order, the values of the key keeping their palette colour though
/// coloured, the values gathered, and the bars drawn whole (their stretches empty) as a stretch of
/// theirs is past a number's range.
fn stack_bars(
    cells: &std::collections::BTreeMap<(usize, usize), (f64, Shade)>,
    bar_of: &[Option<usize>],
    bars: usize,
    mut names: Vec<Option<String>>,
    coloured: bool,
    by_label: bool,
) -> (Vec<(String, Rgba)>, Vec<Vec<f64>>, usize, usize, usize) {
    // The map is read in its own order, so an Other stretch is always the same sum.
    let shown = || cells.iter().filter_map(|((g, k), (t, shade))| Some((bar_of.get(*g).copied().flatten()?, *k, *t, *shade)));
    let mut totals: Vec<Option<f64>> = vec![None; names.len()];
    let mut shades = vec![Shade::Unread; names.len()];
    for (_, k, t, shade) in shown() {
        if let (Some(s), Some(c)) = (totals.get_mut(k), shades.get_mut(k)) {
            *s = Some(s.unwrap_or(0.0) + t);
            c.merge(shade);
        }
    }
    // The key keeps the values with the largest totals, ordered by label once kept.
    // A value whose total is past a number's range, a pair's or once added up, orders as 0.
    let mut by_total: Vec<(f64, usize)> =
        totals.iter().enumerate().filter_map(|(k, t)| t.map(|t| (if t.is_finite() { t } else { 0.0 }, k))).collect();
    by_total.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let own = if by_total.len() > CHART_STACKS { CHART_STACKS - 1 } else { by_total.len() };
    if let Some(kept) = by_label.then(|| by_total.get_mut(..own)).flatten() {
        kept.sort_by_cached_key(|(_, k)| names.get(*k).and_then(|n| n.as_deref()).unwrap_or(CHART_BLANK).to_lowercase());
    }
    let mut key_of = vec![own; names.len()];
    let (mut key, mut kept) = (Vec::with_capacity(own + 1), 0);
    for (at, (_, k)) in by_total.iter().take(own).enumerate() {
        if let Some(slot) = key_of.get_mut(*k) {
            *slot = at;
        }
        let palette = crate::diagram::CATEGORY_COLORS.get(at).copied().unwrap_or(crate::symbology::OTHER_FILL);
        let colour = match (coloured, shades.get(*k)) {
            (false, _) => palette,
            (true, Some(Shade::One(c))) => *c,
            (true, _) => {
                kept += 1;
                palette
            }
        };
        let name = names.get_mut(*k).and_then(Option::take).unwrap_or_else(|| CHART_BLANK.to_string());
        key.push((name, colour));
    }
    let gathered = by_total.len() - own;
    if gathered > 0 {
        key.push((CHART_OTHER.to_string(), crate::symbology::OTHER_FILL));
    }
    let mut stretches = vec![vec![0.0; key.len()]; bars];
    for (bar, k, t, _) in shown() {
        if let Some(s) = key_of.get(k).and_then(|at| stretches.get_mut(bar)?.get_mut(*at)) {
            *s += t;
        }
    }
    // A bar a stretch of which adds up past a number's range, or whose stretches above zero or
    // below it do once stacked (features of opposite signs near the largest number, its total
    // still within it), is drawn whole, with no stretches.
    let past = |s: &[f64]| {
        let (lo, hi) = s.iter().fold((0.0_f64, 0.0_f64), |(lo, hi), v| if *v < 0.0 { (lo + v, hi) } else { (lo, hi + v) });
        !(lo.is_finite() && hi.is_finite())
    };
    let mut whole = 0;
    for s in stretches.iter_mut().filter(|s| past(s)) {
        s.clear();
        whole += 1;
    }
    (key, stretches, kept, gathered, whole)
}

/// A chart's group value looked up by itself rather than by its name ([`ElementKind::chart_data`]):
/// a whole number, a number's bits, or a yes/no. The same key is always written alike, so a bar
/// found by it is the bar of that name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum GroupKey {
    Whole(i64),
    Bits(u64),
    Bool(bool),
}

impl GroupKey {
    /// `v`'s key; none for a text, a date or no value, looked up by name.
    fn of(v: &rustgis_data::Value) -> Option<GroupKey> {
        match v {
            rustgis_data::Value::Int(i) => Some(GroupKey::Whole(*i)),
            rustgis_data::Value::Float(f) => Some(GroupKey::Bits(f.to_bits())),
            rustgis_data::Value::Bool(b) => Some(GroupKey::Bool(*b)),
            _ => None,
        }
    }
}

/// The most characters a layout text holds: a longer one is cut when a layout is read.
pub const MAX_TEXT_CHARS: usize = 5000;

/// A layer's figure in a layout text (#2853): `{count:<layer>}`, or `{sum:<layer>:<field>}` and
/// the same with `mean`, `min` and `max`. The layer is everything between the first colon and the
/// field's (the last), so a layer named with a colon still reads; a count's, everything after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatToken<'a> {
    /// The token as written, braces included.
    pub token: &'a str,
    pub stat: StatKind,
    pub layer: &'a str,
    /// None for a count.
    pub field: Option<&'a str>,
}

/// The [`StatToken`]s in `text`, in the order they come; other braces (`{scale}`, `{foo}`,
/// `{sum:}`) are not among them.
pub fn stat_tokens(text: &str) -> Vec<StatToken<'_>> {
    scan_stat_tokens(text, false)
}

/// [`stat_tokens`] and also the `{sum:Layer}`, `{mean:Layer}`, `{min:Layer}` and `{max:Layer}` written
/// without their field, which are no tokens to draw but a figure the writer meant: for a notice.
pub fn stat_tokens_and_gaps(text: &str) -> Vec<StatToken<'_>> {
    scan_stat_tokens(text, true)
}

fn scan_stat_tokens(text: &str, gaps: bool) -> Vec<StatToken<'_>> {
    let mut out = Vec::new();
    for (open, _) in text.match_indices('{') {
        let Some(after) = text.get(open + 1..) else { continue };
        // The first brace after it ends the token; another opening one means this isn't one.
        let Some(len) = after.find(['{', '}']).filter(|&i| after.get(i..).is_some_and(|r| r.starts_with('}'))) else { continue };
        let (Some(inner), Some(token)) = (after.get(..len), text.get(open..open + len + 2)) else { continue };
        let Some((code, rest)) = inner.split_once(':') else { continue };
        let stat = match code {
            "count" => StatKind::Count,
            "sum" => StatKind::Sum,
            "mean" => StatKind::Mean,
            "min" => StatKind::Min,
            "max" => StatKind::Max,
            _ => continue,
        };
        let (layer, field) = match stat {
            StatKind::Count => (rest, None),
            _ => match rest.rsplit_once(':') {
                Some((layer, field)) if !field.is_empty() => (layer, Some(field)),
                // No field, or an empty one: only a notice reads it.
                Some((layer, _)) if gaps => (layer, None),
                None if gaps => (rest, None),
                _ => continue,
            },
        };
        if !layer.is_empty() {
            out.push(StatToken { token, stat, layer, field });
        }
    }
    out
}

/// `text` with the layer of each statistic token put in another name by `rename` (given the layer's
/// name, and the name to write in its place, or none to leave it): none when no token changes. The
/// layer a token names is read as [`stat_tokens`] reads it.
pub fn rename_stat_layers(text: &str, mut rename: impl FnMut(&str) -> Option<String>) -> Option<String> {
    let mut out = text.to_string();
    let mut changed = false;
    let mut done: Vec<&str> = Vec::new();
    for t in stat_tokens(text) {
        if done.contains(&t.token) {
            continue;
        }
        done.push(t.token);
        let Some(layer) = rename(t.layer) else { continue };
        let code = t.stat.code();
        let new = match t.field {
            Some(field) => format!("{{{code}:{layer}:{field}}}"),
            None => format!("{{{code}:{layer}}}"),
        };
        if new != t.token {
            out = out.replace(t.token, &new);
            changed = true;
        }
    }
    changed.then_some(out)
}

/// Why a [`StatToken`] has no figure: drawn as written instead, and said when the text is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatMissing {
    /// The layer has no such field (or a sum, mean, least or greatest names none).
    Field,
    /// The field holds no numbers to add up.
    NotNumeric,
    /// A raster layer, which has no features to count.
    Raster,
    /// None of the features the layer's definition query keeps has a number in the field, or the
    /// statistic is past a number's range.
    NoValue,
}

/// `stat` of `layer`'s `field` (none for a count) over the features its definition query keeps
/// ([`crate::Layer::visible_rows`]), its numbers read as a chart's are (finite, nulls skipped).
pub fn layer_statistic(layer: &crate::Layer, stat: StatKind, field: Option<&str>) -> Result<f64, StatMissing> {
    layer_statistics(layer, &[(stat, field)]).into_iter().next().unwrap_or(Err(StatMissing::NoValue))
}

/// [`layer_statistic`] for each of `wanted` (a statistic and its field), in one pass over the layer's
/// rows: the rows its definition query keeps are worked out once, and every field wanted is tallied
/// in that pass, so a text with many figures of one layer costs one pass, not one for each.
pub fn layer_statistics(layer: &crate::Layer, wanted: &[(StatKind, Option<&str>)]) -> Vec<Result<f64, StatMissing>> {
    if layer.raster.is_some() {
        return wanted.iter().map(|_| Err(StatMissing::Raster)).collect();
    }
    let ds = &layer.dataset;
    // Each statistic's field as a column, or why it has none; the columns, once each.
    let mut columns: Vec<usize> = Vec::new();
    let slots: Vec<Result<Option<usize>, StatMissing>> = wanted
        .iter()
        .map(|(stat, field)| {
            let fi = match (stat, field) {
                (StatKind::Count, _) => return Ok(None),
                (_, Some(f)) => ds.field_index(f).ok_or(StatMissing::Field)?,
                (_, None) => return Err(StatMissing::Field),
            };
            if !ds.fields.get(fi).is_some_and(|f| f.ty.is_numeric()) {
                return Err(StatMissing::NotNumeric);
            }
            let slot = columns.iter().position(|c| *c == fi).unwrap_or_else(|| {
                columns.push(fi);
                columns.len() - 1
            });
            Ok(Some(slot))
        })
        .collect();
    let mut records = 0usize;
    let mut tallies = vec![Tally::default(); columns.len()];
    if slots.iter().any(Result::is_ok) {
        for (row, kept) in layer.visible_rows().into_iter().enumerate() {
            if !kept {
                continue;
            }
            records += 1;
            for (tally, fi) in tallies.iter_mut().zip(&columns) {
                if let Some(n) = ds.value(row, *fi).as_f64().filter(|v| v.is_finite()) {
                    tally.add(n, true);
                }
            }
        }
    }
    wanted
        .iter()
        .zip(slots)
        .map(|((stat, _), slot)| match slot? {
            None => Tally { records, ..Tally::default() }.value(*stat).ok_or(StatMissing::NoValue),
            Some(slot) => Tally { records, ..tallies[slot] }.value(*stat).ok_or(StatMissing::NoValue),
        })
        .collect()
}

/// A chart bar's features as its statistic reads them: how many, and their numbers' sum, mean,
/// least and greatest.
#[derive(Clone, Copy, Debug)]
struct Tally {
    records: usize,
    numbers: usize,
    sum: f64,
    /// Kept as the numbers come, each one's share of it added in turn, so it stays in a number's
    /// range where their sum doesn't (values near `f64::MAX`); read only then.
    mean: f64,
    min: f64,
    max: f64,
}

impl Default for Tally {
    fn default() -> Self {
        Tally { records: 0, numbers: 0, sum: 0.0, mean: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY }
    }
}

impl Tally {
    /// Adds `n`, and to the kept mean when `mean` (only a mean reads it).
    fn add(&mut self, n: f64, mean: bool) {
        self.numbers += 1;
        self.sum += n;
        if mean {
            // Divided before they are taken apart: `n - mean` itself may be past the range.
            let k = self.numbers as f64;
            self.mean += n / k - self.mean / k;
        }
        self.min = self.min.min(n);
        self.max = self.max.max(n);
    }

    fn merge(&mut self, other: Option<&Tally>) {
        let Some(o) = other else { return };
        self.records += o.records;
        if o.numbers > 0 {
            // Moved towards the other's mean by the other's share of the numbers. Of the same sign,
            // their weighted difference stays in range and equal means stay as they are; of opposite
            // signs, their weighted sum does.
            let w = o.numbers as f64 / (self.numbers + o.numbers) as f64;
            if (self.mean < 0.0) == (o.mean < 0.0) {
                self.mean += o.mean * w - self.mean * w;
            } else {
                self.mean = self.mean * (1.0 - w) + o.mean * w;
            }
        }
        self.numbers += o.numbers;
        self.sum += o.sum;
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }

    /// Its statistic, read from the tally as [`StatKind::apply`] reads it from the numbers; none
    /// without a number (a count but one), or past a number's range.
    fn value(&self, stat: StatKind) -> Option<f64> {
        let v = match stat {
            StatKind::Count => self.records as f64,
            _ if self.numbers == 0 => return None,
            StatKind::Sum => self.sum,
            // Their sum over their number while it is a number, so a mean reads as it always has;
            // past the range, the mean kept as they came.
            StatKind::Mean if self.sum.is_finite() => self.sum / self.numbers as f64,
            StatKind::Mean => self.mean,
            StatKind::Min => self.min,
            StatKind::Max => self.max,
        };
        Some(v).filter(|v| v.is_finite())
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
    /// The box drawn behind its content (#2257); none on a map frame, and none by default, so a
    /// layout saved before it draws as it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,
    pub kind: ElementKind,
}

/// The box drawn behind an element's content (#2257): its fill, an outline when it has a width,
/// rounded corners, and how far it reaches past the element's box on every side. The content is
/// drawn as without it, in the element's box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Background {
    /// White unless given, as `layout.update`'s `{"background": {}}` makes it.
    #[serde(default = "white")]
    pub fill: Rgba,
    #[serde(default = "ink")]
    pub outline: Rgba,
    /// The outline's width in points; 0 (the default) draws none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub width: f64,
    /// The corners' radius, in millimetres.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub radius: f64,
    /// How far it reaches past the element's box on every side, in millimetres.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub padding: f64,
}

/// The widest a background's outline may be (points), as a rectangle's.
pub const MAX_BACKGROUND_WIDTH: f64 = 50.0;
/// The largest corner radius a background takes (mm), as a rectangle's.
pub const MAX_BACKGROUND_RADIUS: f64 = 50.0;
/// The farthest a background reaches past its element's box on each side (mm).
pub const MAX_BACKGROUND_PADDING: f64 = 50.0;

impl Background {
    /// Filled with `fill`: no outline, square corners, no padding.
    pub fn filled(fill: Rgba) -> Background {
        Background { fill, outline: ink(), width: 0.0, radius: 0.0, padding: 0.0 }
    }

    /// Its numbers brought into range: finite, 0 or more, at most their limits.
    pub fn clamped(self) -> Background {
        let keep = |v: f64, max: f64| finite_or(v, 0.0).clamp(0.0, max);
        Background {
            width: keep(self.width, MAX_BACKGROUND_WIDTH),
            radius: keep(self.radius, MAX_BACKGROUND_RADIUS),
            padding: keep(self.padding, MAX_BACKGROUND_PADDING),
            ..self
        }
    }
}

impl Element {
    /// Whether the point (mm) is on the element, its box turned as it is drawn; with a background,
    /// on the background, padding included.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (x, y) = self.unturned(x, y);
        let (bx, by, bw, bh) = self.background_box();
        x >= bx && x <= bx + bw && y >= by && y <= by + bh
    }

    /// The box its background fills (x, y, w, h in mm, unturned): its own box grown by the
    /// background's padding, or its own box when it has none.
    pub fn background_box(&self) -> (f64, f64, f64, f64) {
        let pad = self.background.map_or(0.0, |b| b.padding);
        (self.x - pad, self.y - pad, self.w + 2.0 * pad, self.h + 2.0 * pad)
    }

    /// How far it is turned about its centre, in degrees clockwise: a text's `rotation`, else 0.
    pub fn rotation(&self) -> f64 {
        match self.kind {
            ElementKind::Text { rotation, .. } => rotation,
            _ => 0.0,
        }
    }

    /// The point (mm) seen in the element's unturned box: turned back about its centre.
    pub fn unturned(&self, x: f64, y: f64) -> (f64, f64) {
        if self.rotation() == 0.0 {
            return (x, y);
        }
        let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
        let (sin, cos) = (-self.rotation()).to_radians().sin_cos();
        let (dx, dy) = (x - cx, y - cy);
        (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
    }

    /// The rectangle (x, y, w, h in mm) its turned box takes up on the page: its box when it isn't
    /// turned.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let (sin, cos) = self.rotation().to_radians().sin_cos();
        let (w, h) = (self.w * cos.abs() + self.h * sin.abs(), self.w * sin.abs() + self.h * cos.abs());
        (self.x + (self.w - w) / 2.0, self.y + (self.h - h) / 2.0, w, h)
    }
}

/// An angle in degrees as a text's `rotation` keeps it: within -180..=180 (450 is 90, 270 is -90),
/// 0 when it isn't a number.
pub fn rotation(degrees: f64) -> f64 {
    if !degrees.is_finite() {
        return 0.0;
    }
    let a = degrees.rem_euclid(360.0);
    // `rem_euclid` of a tiny negative angle can round to 360 itself.
    if a > 180.0 { a - 360.0 } else { a }
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

/// A series row's name, or its one-based row number when the field is missing, null or blank.
pub fn series_page_name(layer: &Layer, fi: Option<usize>, row: usize) -> String {
    fi.map(|i| layer.dataset.value(row, i))
        .filter(|v| !v.is_null())
        .map(ToString::to_string)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| (row + 1).to_string())
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
    /// The page's colour, painted under every element (#2855); none, white. Always opaque: a
    /// page has nothing under it to show through, so a translucent one in a hand-edited file is
    /// read as it looks on white paper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper: Option<Rgba>,
    #[serde(default)]
    next_id: u32,
    /// Process-local identity, kept by undo snapshots but never stored in a project.
    #[serde(skip)]
    instance: Instance,
}

/// Identifies a live layout, whatever its name or place among the project's layouts (#2802). Not
/// part of the layout's value: layouts that differ only by it are equal.
#[derive(Clone, Copy, Debug)]
struct Instance(u64);

impl Default for Instance {
    fn default() -> Self {
        Instance(crate::next_layer_instance())
    }
}

impl PartialEq for Instance {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Layout {
    /// Identifies this live layout, whatever its name or place among the project's layouts.
    pub fn instance_id(&self) -> u64 {
        self.instance.0
    }

    /// Starts a distinct live layout after copying one. Undo snapshots keep the original identity.
    pub fn renew_instance_id(&mut self) {
        self.instance = Instance::default();
    }

    /// Resolves a series' index layer from its frame and the optional page-name column.
    pub fn series_index<'a>(&self, maps: &'a [Map]) -> Option<(&'a Layer, Option<usize>)> {
        let series = self.series.as_ref()?;
        let ElementKind::MapFrame { map, .. } = &self.element(series.frame)?.kind else { return None };
        let layer = maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?.layer_by_name(&series.layer)?;
        Some((layer, layer.dataset.field_index(&series.name_field)))
    }

    pub fn new(name: impl Into<String>, width: f64, height: f64) -> Self {
        Layout {
            name: name.into(),
            width,
            height,
            elements: Vec::new(),
            series: None,
            guides: Guides::default(),
            decimal_comma: false,
            paper: None,
            next_id: 1,
            instance: Instance::default(),
        }
    }

    /// Adds an element and returns its id.
    pub fn add(&mut self, name: impl Into<String>, (x, y, w, h): (f64, f64, f64, f64), kind: ElementKind) -> u32 {
        let id = self.next_id.max(self.elements.iter().map(|e| e.id).max().unwrap_or(0).saturating_add(1));
        self.next_id = id.saturating_add(1);
        // Templates on tiny pages can compute sizes near zero.
        let (w, h) = (finite_or(w, 1.0).clamp(MIN_ELEMENT_MM, 5000.0), finite_or(h, 1.0).clamp(MIN_ELEMENT_MM, 5000.0));
        self.elements.push(Element { id, name: name.into(), x, y, w, h, visible: true, locked: false, background: None, kind });
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
    /// sizes in range, unique element ids, and no references to missing frames. Returns what it
    /// changed, so a project reader can say so.
    pub fn clamp(&mut self) -> LayoutRepairs {
        let page = (self.width, self.height, self.guides.clone(), self.series.clone());
        let count = self.elements.len();
        self.width = finite_or(self.width, 210.0).clamp(20.0, 5000.0);
        self.height = finite_or(self.height, 297.0).clamp(20.0, 5000.0);
        // A page colour is opaque (a page has nothing under it to show through) and white is the
        // page's own (#2855): a hand-edited translucent one is read as it looks on white paper,
        // and said.
        let white = Rgba::rgb(255, 255, 255);
        let translucent = self.paper.is_some_and(|c| c.a() != 255);
        self.paper = self.paper.map(|c| white.lerp(c.with_alpha(255), f64::from(c.a()) / 255.0)).filter(|c| *c != white);
        // The same limits `layout.guides` sets: the view walks these lists every frame.
        for list in [&mut self.guides.x, &mut self.guides.y] {
            list.retain(|v| v.is_finite() && v.abs() <= 5000.0);
            list.truncate(MAX_GUIDES);
        }
        self.elements.truncate(500);
        let mut seen = std::collections::HashSet::new();
        let mut next = self.elements.iter().map(|e| e.id).filter(|id| *id <= MAX_ID).max().unwrap_or(0).saturating_add(1);
        let mut changed = Vec::with_capacity(self.elements.len());
        let mut mgrs_off = 0usize;
        for e in &mut self.elements {
            // A picture's image (often large) is never changed here, so it isn't copied.
            let mut before = Element {
                name: e.name.clone(),
                kind: match &e.kind {
                    ElementKind::Picture { .. } => ElementKind::Picture { image: String::new() },
                    kind => kind.clone(),
                },
                ..*e
            };
            // A bearing past half a turn names the same direction once normalized, and a pitch
            // too small to tilt is drawn flat: neither is a change.
            if let ElementKind::MapFrame { bearing, pitch, .. } = &mut before.kind {
                if bearing.is_finite() {
                    *bearing = crate::camera::normalize_bearing(*bearing);
                }
                if (0.0..1e-6).contains(pitch) {
                    *pitch = 0.0;
                }
            }
            // One subdivision is the division whole, as none: it draws the same.
            if let ElementKind::ScaleBar { subdivisions, .. } = &mut before.kind
                && *subdivisions == 1
            {
                *subdivisions = 0;
            }
            // A bar outline of width 0 draws none, as the commands take it: letting it go is no
            // change.
            if let ElementKind::Chart { bar_outline, .. } = &mut before.kind
                && bar_outline.is_some_and(|o| o.width == 0.0)
            {
                *bar_outline = None;
            }
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
            let mut cut = e.name.chars().nth(200).is_some();
            e.name = e.name.chars().take(200).collect();
            match &mut e.kind {
                ElementKind::MapFrame { center, scale, shape, grid, bearing, pitch, extent_width, border_width, .. } => {
                    *scale = finite_or(*scale, 50_000_000.0).clamp(MIN_FRAME_SCALE, MAX_FRAME_SCALE);
                    *extent_width = finite_or(*extent_width, INDICATOR_WIDTH).clamp(MIN_INDICATOR_WIDTH, MAX_INDICATOR_WIDTH);
                    *border_width = finite_or(*border_width, FRAME_BORDER_WIDTH).clamp(0.0, MAX_FRAME_BORDER_WIDTH);
                    *bearing = crate::camera::normalize_bearing(*bearing);
                    *pitch = crate::camera::normalize_pitch(*pitch);
                    if !center.is_finite() {
                        *center = Coord::new(0.0, 0.0);
                    }
                    center.x = center.x.clamp(-540.0, 540.0);
                    // A pole is a centre like any other on a polar map: the map's projection
                    // decides where the frame is drawn ([`frame_camera`], #2684).
                    center.y = center.y.clamp(-90.0, 90.0);
                    if let FrameShape::Polygon(p) = shape {
                        let points = p.len();
                        p.retain(|(u, v)| u.is_finite() && v.is_finite());
                        p.truncate(10_000);
                        cut |= p.len() < points;
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
                        // MGRS labels name a WGS 84 UTM zone's squares, on a measured grid: on a
                        // graticule, or with another system named (only a hand-edited file has
                        // either), they are left off, and counted on their own.
                        let named_other = g.crs.as_deref().is_some_and(|c| !c.trim().is_empty() && utm_zone_of(c).is_none());
                        if g.mgrs && (g.kind != GridKind::Measured || named_other) {
                            g.mgrs = false;
                            mgrs_off = mgrs_off.saturating_add(1);
                            // (Counted by `mgrs_off` and said in its own notice, so `before` matches:
                            // not an adjusted element too.)
                            if let ElementKind::MapFrame { grid: Some(b), .. } = &mut before.kind {
                                b.mgrs = false;
                            }
                        }
                        g.width = finite_or(g.width, grid_width()).clamp(0.0, MAX_GRID_WIDTH);
                        g.label_size = layout_text_in_range(g.label_size, GRID_LABEL_PT);
                    }
                }
                ElementKind::Legend { exclude, columns, labels, text_size, .. } => {
                    *text_size = layout_text_in_range(*text_size, LEGEND_TEXT_PT);
                    // Names stay whole: a layer's own name isn't cut, and a cut one would no
                    // longer match it.
                    cut |= exclude.len() > MAX_LEGEND_EXCLUDED;
                    exclude.truncate(MAX_LEGEND_EXCLUDED);
                    *columns = (*columns).clamp(1, MAX_LEGEND_COLUMNS);
                    // The same for the names labelled; a label is cut to its length, and a blank
                    // one shows the name, as it would without.
                    labels.retain(|_, label| !label.trim().is_empty());
                    let labelled = labels.len();
                    // One label per layer, as a command keeps: of two spellings of a name (any
                    // case), the first in order stays and the other is cut.
                    let mut seen = std::collections::HashSet::new();
                    labels.retain(|name, _| seen.insert(name.to_ascii_lowercase()));
                    while labels.len() > MAX_LEGEND_LABELS {
                        labels.pop_last();
                    }
                    cut |= labels.len() < labelled;
                    for label in labels.values_mut() {
                        let end = label.char_indices().nth(MAX_LEGEND_LABEL_CHARS).map_or(label.len(), |(i, _)| i);
                        cut |= end < label.len();
                        label.truncate(end);
                    }
                }
                ElementKind::Text { text, size, letter_spacing, rotation: turn, valign, line_spacing, case, .. } => {
                    *turn = rotation(*turn);
                    // A file's unknown name or wrong kind of value is read as a marker: back to the
                    // default.
                    *valign = valign.kept();
                    *case = case.kept();
                    *line_spacing = line_spacing_in_range(*line_spacing);
                    *size = finite_or(*size, 12.0).clamp(*crate::graphics::FONT_SIZE.start(), *crate::graphics::FONT_SIZE.end());
                    *letter_spacing = finite_or(*letter_spacing, 0.0).clamp(MIN_LETTER_SPACING, MAX_LETTER_SPACING);
                    let end = text.char_indices().nth(MAX_TEXT_CHARS).map_or(text.len(), |(i, _)| i);
                    cut |= end < text.len();
                    text.truncate(end);
                }
                ElementKind::Table { rows, .. } => *rows = (*rows).clamp(1, 500),
                // A maximum the chart can't draw to (only a hand-edited file has one) gives it its
                // bars' own scale back, counted with the values out of range (#2487 part 4); a bar
                // outline's width past 5 points is held to them, and one below 0 let go (part 4b).
                ElementKind::Chart { max, bar_outline, .. } => {
                    *max = chart_max(*max);
                    *bar_outline = chart_outline(*bar_outline);
                }
                ElementKind::ScaleBar { divisions, text_size, subdivisions, divisions_before_zero, decimals, unit_label, .. } => {
                    *divisions = (*divisions).clamp(1, MAX_SCALE_DIVISIONS);
                    *text_size = layout_text_in_range(*text_size, SCALE_BAR_TEXT_PT);
                    // One part is the whole division: saved as none, it asks for no details.
                    *subdivisions = if *subdivisions == 1 { 0 } else { (*subdivisions).min(MAX_SCALE_SUBDIVISIONS) };
                    *divisions_before_zero = (*divisions_before_zero).min(MAX_SCALE_BEFORE_ZERO);
                    // The two together are drawn as at most ten: the ones before zero give way.
                    *divisions_before_zero = (*divisions_before_zero).min(MAX_SCALE_DIVISIONS.saturating_sub(*divisions));
                    // A file's invalid count (negative, fractional) is read as a marker: back to automatic.
                    *decimals = match *decimals {
                        Some(SCALE_DECIMALS_INVALID) => None,
                        d => d.map(|d| d.min(MAX_SCALE_DECIMALS as u8)),
                    };
                    // The label is one line: a break in a hand-edited file is a space.
                    if let Some(label) = unit_label {
                        *label = label.replace("\r\n", " ").replace(['\n', '\r'], " ");
                        let end = label.char_indices().nth(MAX_UNIT_LABEL_CHARS).map_or(label.len(), |(i, _)| i);
                        cut |= end < label.len();
                        label.truncate(end);
                    }
                }
                ElementKind::Rectangle { width, .. } | ElementKind::Ellipse { width, .. } => {
                    *width = finite_or(*width, 1.0).clamp(0.0, 50.0)
                }
                ElementKind::Line { width, .. } => *width = finite_or(*width, 1.0).clamp(0.1, 50.0),
                _ => {}
            }
            // A map frame draws its map and its paper itself: a background on one (a hand-edited
            // file) is dropped.
            e.background = e.background.filter(|_| e.kind.takes_background()).map(Background::clamped);
            let kind_changed = !matches!(e.kind, ElementKind::Picture { .. }) && e.kind != before.kind;
            changed.push((
                kind_changed
                    || e.background != before.background
                    || (e.id, &e.name, e.x, e.y, e.w, e.h, e.visible, e.locked)
                        != (before.id, &before.name, before.x, before.y, before.w, before.h, before.visible, before.locked),
                cut,
            ));
        }
        self.next_id = next;
        // References must point at map frames that exist.
        let frames: std::collections::HashSet<u32> =
            self.elements.iter().filter(|e| matches!(e.kind, ElementKind::MapFrame { .. })).map(|e| e.id).collect();
        let (mut adjusted, mut cut_count) = (0usize, 0usize);
        let elements = std::mem::take(&mut self.elements);
        for (mut e, (mut changed, cut)) in elements.into_iter().zip(changed) {
            if e.kind.frame().is_some_and(|f| !frames.contains(&f)) {
                continue;
            }
            if let ElementKind::MapFrame { extent_of, .. } = &mut e.kind
                && extent_of.is_some_and(|o| !frames.contains(&o) || o == e.id)
            {
                *extent_of = None;
                changed = true;
            }
            adjusted = adjusted.saturating_add(usize::from(changed));
            cut_count = cut_count.saturating_add(usize::from(cut));
            self.elements.push(e);
        }
        let mut series_removed = false;
        if let Some(s) = &mut self.series {
            if !frames.contains(&s.frame) {
                self.series = None;
                series_removed = true;
            } else {
                s.margin = finite_or(s.margin, 0.1).clamp(0.0, 5.0);
            }
        }
        let series_adjusted = !series_removed && self.series != page.3;
        let page_adjusted = series_adjusted || translucent || (self.width, self.height, &self.guides) != (page.0, page.1, &page.2);
        LayoutRepairs {
            elements_adjusted: adjusted,
            elements_cut: cut_count,
            elements_removed: count.saturating_sub(self.elements.len()),
            page_adjusted: usize::from(page_adjusted),
            series_removed: usize::from(series_removed),
            mgrs_off,
        }
    }
}

/// What [`Layout::clamp`] changed in a layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutRepairs {
    /// Elements kept with a value brought into range, a new id, a reference cleared, or a unit
    /// label's line breaks replaced.
    pub elements_adjusted: usize,
    /// Elements with a name, text, polygon points, legend exclusions, scale bar unit labels or
    /// legend labels cut to their limits (the same element may count as adjusted too).
    pub elements_cut: usize,
    /// Elements removed: beyond the 500 a layout holds, or tied to a map frame it doesn't have.
    pub elements_removed: usize,
    /// Whether the page size, guides, page colour or map series margin changed (0 or 1).
    pub page_adjusted: usize,
    /// Whether the map series was removed, its map frame missing (0 or 1).
    pub series_removed: usize,
    /// Map frames whose grid had MGRS labels on a graticule or over a system other than a WGS 84
    /// UTM zone, turned off.
    pub mgrs_off: usize,
}

impl LayoutRepairs {
    /// Whether nothing was changed.
    pub fn is_empty(&self) -> bool {
        *self == LayoutRepairs::default()
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

/// A map frame's border until it is set (#2557): dark grey, 0.8 points.
pub const FRAME_BORDER_COLOR: Rgba = Rgba::rgb(0x30, 0x30, 0x30);
pub const FRAME_BORDER_WIDTH: f64 = 0.8;
/// The widest a map frame's border may be (points); 0 draws none.
pub const MAX_FRAME_BORDER_WIDTH: f64 = MAX_INDICATOR_WIDTH;

fn frame_border_color() -> Rgba {
    FRAME_BORDER_COLOR
}

fn is_frame_border_color(c: &Rgba) -> bool {
    *c == FRAME_BORDER_COLOR
}

fn frame_border_width() -> f64 {
    FRAME_BORDER_WIDTH
}

fn is_frame_border_width(w: &f64) -> bool {
    *w == FRAME_BORDER_WIDTH
}

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
    // The centre where the map's projection places it: a pole itself on a polar map. Where it
    // can't (a pole on Web Mercator, at infinity), 85° of latitude, as frames always were (#2684).
    // A longitude past a view's reach (±270° on Web Mercator) is the same meridian a turn of the
    // globe nearer: the frame shows the same place, not one moved to the view's edge (#2711).
    let center = if center.x.abs() > 270.0 { Coord::new((center.x + 180.0).rem_euclid(360.0) - 180.0, center.y) } else { center };
    let at = if frame_center_placed(map, center) { center } else { Coord::new(center.x, center.y.clamp(-85.0, 85.0)) };
    if let Some(c) = map.to_map(at) {
        cam.center = c;
    }
    let per = map.ground_per_unit(cam.center).max(1e-9);
    cam.res = scale * PAPER_M_PER_PT / per;
    cam.clamp();
    cam
}

/// Whether a frame centred on `center` (lon/lat) is drawn centred there on `map`: its projection
/// places the point, and reads it back at the same latitude. A pole on Web Mercator isn't: its
/// projection takes a pole to its edge at 85.05°, which reads back as that ([`frame_camera`]
/// draws the frame at 85° instead); on a polar map it is (#2684). The longitude isn't compared:
/// it may run past ±180°, and at a pole every longitude is the same point.
pub fn frame_center_placed(map: &Map, center: Coord) -> bool {
    map.to_map(center).and_then(|m| map.to_lonlat(m)).is_some_and(|back| (back.y - center.y).abs() < 1e-4)
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

/// The raster layer a colorbar shows: `layer` by name, or the topmost raster the map shows (its
/// groups visible too) with a colour scale ([`color_scale`]) that isn't drawn by classes: a legend lists those (#1679), and a colorbar
/// of their codes would be the only key a land cover had. An RGB composite or a land cover over a
/// DEM leaves it to the DEM. A raster on a ramp (Stretch, a tinted hillshade) whose pixels aren't
/// read yet keeps its place: it is returned with no colour scale to show, rather than the one of
/// a raster under it.
pub fn colorbar_layer<'a>(map: &'a Map, layer: Option<&str>) -> Option<&'a Layer> {
    match layer {
        Some(n) => map.layer_by_name(n).filter(|l| l.is_raster()),
        None => map.layers.iter().filter(|l| map.shows(l)).find(|l| {
            l.raster.as_ref().is_some_and(|r| match r.data {
                None => matches!(r.style.renderer, RasterRenderer::Stretch { .. } | RasterRenderer::ShadedRelief { ramp: Some(_), .. }),
                Some(_) => !r.style.renderer.draws_classes() && color_scale(r).is_some(),
            })
        }),
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
    // A relief's bar is the colour of its flat ground: under the balanced light (#1977), flat
    // ground's own hillshade, which keeps the ramp's colour; under the plain light, full light.
    let shade = match style.renderer {
        RasterRenderer::ShadedRelief { altitude, balanced: true, .. } => {
            Some(crate::raster::hillshade(&[0.0; 9], 1.0, 1.0, 0.0, altitude, 1.0))
        }
        RasterRenderer::ShadedRelief { .. } => Some(255),
        _ => None,
    };
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
    use crate::symbology::MAX_DASH_STEP;

    /// Several figures of one layer, worked out in one pass over its rows, are the figures worked out
    /// one by one, errors included (#2853).
    #[test]
    fn a_layers_figures_in_one_pass_are_the_figures_one_by_one() {
        use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
        let fields = vec![Field::new("n", FieldType::Double), Field::new("m", FieldType::Integer), Field::new("name", FieldType::Text)];
        let mut ds = Dataset::new("sites", GeometryType::None, fields);
        for (n, m) in
            [(Value::Float(2.5), 3), (Value::Float(-1.0), 4), (Value::Null, 5), (Value::Float(f64::NAN), 6), (Value::Float(100.0), 7)]
        {
            ds.push(None, vec![n, Value::Int(m), Value::Text("x".into())]);
        }
        let mut layer = crate::Layer::new(1, ds);
        let wanted = [
            (StatKind::Count, None),
            (StatKind::Sum, Some("n")),
            (StatKind::Mean, Some("n")),
            (StatKind::Min, Some("m")),
            (StatKind::Max, Some("m")),
            (StatKind::Sum, Some("name")),
            (StatKind::Sum, Some("nosuch")),
            (StatKind::Sum, None),
            (StatKind::Mean, Some("n")),
        ];
        for query in [None, Some("m > 3")] {
            layer.definition_query = query.map(String::from);
            let one_by_one: Vec<_> = wanted.iter().map(|(stat, field)| layer_statistic(&layer, *stat, *field)).collect();
            assert_eq!(layer_statistics(&layer, &wanted), one_by_one, "{query:?}");
        }
    }

    /// A layer renamed is renamed in the tokens that name it, whatever their statistic or field, and
    /// a text none of them names is left as it is (#2853).
    #[test]
    fn statistic_tokens_follow_a_renamed_layer() {
        let to_z = |l: &str| (l == "A").then(|| "Z".to_string());
        assert_eq!(
            rename_stat_layers("{count:A} {sum:A:f} {count:A} {max:B:A} {scale}", to_z).as_deref(),
            Some("{count:Z} {sum:Z:f} {count:Z} {max:B:A} {scale}")
        );
        assert_eq!(rename_stat_layers("{count:B} {sum:B:A} A", to_z), None);
    }

    /// The tokens a notice reads also include the figure kinds written without their field, which
    /// are no tokens to draw or to require a capability for (#2853).
    #[test]
    fn statistic_tokens_and_gaps_include_a_figure_without_its_field() {
        fn read(t: &str) -> Vec<(&str, StatKind, &str, Option<&str>)> {
            stat_tokens_and_gaps(t).into_iter().map(|s| (s.token, s.stat, s.layer, s.field)).collect()
        }
        assert_eq!(
            read("{sum:A} {sum:A:b} {count:C} {mean:Zone: north} {max:} {sum::x} {min:A:} {foo:A}"),
            vec![
                ("{sum:A}", StatKind::Sum, "A", None),
                ("{sum:A:b}", StatKind::Sum, "A", Some("b")),
                ("{count:C}", StatKind::Count, "C", None),
                ("{mean:Zone: north}", StatKind::Mean, "Zone", Some(" north")),
                ("{min:A:}", StatKind::Min, "A", None),
            ]
        );
        assert_eq!(stat_tokens("{sum:A} {sum:A:b}").len(), 1, "a figure without its field is no token");
    }

    /// A text's statistic tokens (#2853): each kind, a layer named with a colon, and braces that
    /// aren't one left alone.
    #[test]
    fn statistic_tokens_are_read_from_a_text() {
        fn read(t: &str) -> Vec<(&str, StatKind, &str, Option<&str>)> {
            stat_tokens(t).into_iter().map(|s| (s.token, s.stat, s.layer, s.field)).collect()
        }
        assert_eq!(
            read("{count:Houses} houses, {sum:Plants:mw} MW at {scale}"),
            vec![("{count:Houses}", StatKind::Count, "Houses", None), ("{sum:Plants:mw}", StatKind::Sum, "Plants", Some("mw")),]
        );
        assert_eq!(
            read("{mean:Zone: north:pop}{min:A:b}{max:A:b}{count:Zone: north}"),
            vec![
                ("{mean:Zone: north:pop}", StatKind::Mean, "Zone: north", Some("pop")),
                ("{min:A:b}", StatKind::Min, "A", Some("b")),
                ("{max:A:b}", StatKind::Max, "A", Some("b")),
                ("{count:Zone: north}", StatKind::Count, "Zone: north", None),
            ]
        );
        // Not tokens: other names, an empty layer or field, a sum without its field, an alias the
        // chart's codes take, a brace left open, a brace inside.
        for t in ["{foo}", "{sum:}", "{count:}", "{sum:A}", "{sum::b}", "{sum:A:}", "{average:A:b}", "{Count:A}", "{count:A", "{count:{A}}"]
        {
            assert_eq!(read(t), vec![], "{t}");
        }
    }

    /// A layer's statistic (#2853) over the features its definition query keeps, its numbers read
    /// as a chart's: nulls and non-finite values skipped; a field it lacks, one with no numbers, or
    /// no number among the kept features, none.
    #[test]
    fn layer_statistics_read_the_features_the_definition_query_keeps() {
        use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
        let fields = vec![Field::new("n", FieldType::Double), Field::new("name", FieldType::Text), Field::new("keep", FieldType::Integer)];
        let mut ds = Dataset::new("sites", GeometryType::None, fields);
        for (n, keep) in
            [(Value::Float(2.5), 1), (Value::Float(-1.0), 1), (Value::Null, 1), (Value::Float(f64::NAN), 1), (Value::Float(100.0), 0)]
        {
            ds.push(None, vec![n, Value::Text("x".into()), Value::Int(keep)]);
        }
        let mut layer = crate::Layer::new(1, ds);
        let stat = layer_statistic;
        // Every feature: five, and the 100 counted.
        assert_eq!(stat(&layer, StatKind::Count, None), Ok(5.0));
        assert_eq!(stat(&layer, StatKind::Max, Some("n")), Ok(100.0));
        layer.definition_query = Some("keep = 1".into());
        assert_eq!(stat(&layer, StatKind::Count, None), Ok(4.0));
        assert_eq!(stat(&layer, StatKind::Sum, Some("n")), Ok(1.5));
        assert_eq!(stat(&layer, StatKind::Mean, Some("n")), Ok(0.75));
        assert_eq!(stat(&layer, StatKind::Min, Some("n")), Ok(-1.0));
        assert_eq!(stat(&layer, StatKind::Max, Some("n")), Ok(2.5));
        assert_eq!(stat(&layer, StatKind::Sum, Some("nosuch")), Err(StatMissing::Field));
        assert_eq!(stat(&layer, StatKind::Sum, None), Err(StatMissing::Field));
        assert_eq!(stat(&layer, StatKind::Sum, Some("name")), Err(StatMissing::NotNumeric));
        // A figure is over the whole layer, so a query that reads the series page's values (`@page_number`)
        // is read with no page: the values are null and it keeps nothing.
        layer.definition_query = Some("@page_number > 0".into());
        assert_eq!(stat(&layer, StatKind::Count, None), Ok(0.0));
        // A query keeping none: a count of 0, no sum.
        layer.definition_query = Some("keep = 7".into());
        assert_eq!(stat(&layer, StatKind::Count, None), Ok(0.0));
        assert_eq!(stat(&layer, StatKind::Sum, Some("n")), Err(StatMissing::NoValue));
        // A query that doesn't read keeps every feature, as the map draws them all.
        layer.definition_query = Some("keep = = 1".into());
        assert_eq!(stat(&layer, StatKind::Count, None), Ok(5.0));
    }

    /// A chart of layer "t" with `extra` keys (its field among them).
    fn chart(extra: serde_json::Value) -> ElementKind {
        let mut c = serde_json::json!({"frame": 1, "layer": "t"});
        if let (Some(c), Some(e)) = (c.as_object_mut(), extra.as_object()) {
            c.extend(e.clone());
        }
        serde_json::from_value(serde_json::json!({ "Chart": c })).unwrap()
    }

    /// Places: name, region, people; a missing number for region "S", a blank region.
    fn places() -> rustgis_data::Dataset {
        use rustgis_data::{Dataset, Field, FieldType, Value};
        let mut ds = Dataset::new(
            "t",
            rustgis_data::GeometryType::Point,
            vec![Field::new("name", FieldType::Text), Field::new("region", FieldType::Text), Field::new("people", FieldType::Double)],
        );
        let text = |t: &str| Value::Text(t.into());
        for (name, region, people) in [
            ("a", text("N"), Value::Float(10.0)),
            ("b", text("S"), Value::Float(4.0)),
            ("c", text("N"), Value::Float(2.0)),
            ("d", text("E"), Value::Float(7.0)),
            ("e", text("S"), Value::Null),
            ("f", Value::Null, Value::Float(1.0)),
            ("g", text("W"), Value::Float(-3.0)),
        ] {
            ds.push(None, vec![text(name), region, people]);
        }
        ds
    }

    fn bars(d: &ChartData) -> Vec<(f64, &str)> {
        d.bars.iter().map(|(v, n)| (*v, n.as_str())).collect()
    }

    /// A layer's colours as its map frame draws them (#2487 part 3): each feature by its own
    /// geometry (a line in a point layer by its stroke), one without by the layer's, and a colour
    /// that depends on `@map_scale` at the frame's scale.
    #[test]
    fn chart_bars_read_their_layers_colours_as_the_frame_draws_them() {
        use rustgis_data::query::Env;
        use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
        let mut ds = Dataset::new("mixed", GeometryType::Point, vec![Field::new("n", FieldType::Double)]);
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Float(3.0)]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])), vec![Value::Float(2.0)]);
        ds.push(None, vec![Value::Float(1.0)]);
        let mut layer = crate::Layer::new(1, ds);
        let (fill, stroke) = (Rgba::rgb(0, 0, 200), Rgba::rgb(200, 0, 0));
        let mut symbol = layer.renderer.base_symbol();
        (symbol.fill, symbol.stroke, symbol.stroke_width) = (fill, stroke, 1.0);
        layer.renderer = crate::symbology::Renderer::Single(symbol);
        let kind = chart(serde_json::json!({"field": "n", "classes": true}));
        // Largest first: the point, the line, the row without a geometry (drawn as a point).
        let d = kind.chart_data_of(&layer, &Env::default()).unwrap();
        assert_eq!(d.colors, vec![Some(fill), Some(stroke), Some(fill)]);
        // A fill that depends on the scale: the frame's, or none without one.
        layer.symbol_by.fill = Some("CASE WHEN @map_scale < 100000 THEN '#00ff00' ELSE '#ff00ff' END".into());
        let near = kind.chart_data_of(&layer, &Env::default().at_scale(Some(25_000.0))).unwrap();
        assert_eq!(near.colors.first(), Some(&Some(Rgba::rgb(0, 255, 0))));
        let far = kind.chart_data_of(&layer, &Env::default().at_scale(Some(1e6))).unwrap();
        assert_eq!(far.colors.first(), Some(&Some(Rgba::rgb(255, 0, 255))));
    }

    /// Coloured by classes (#2487 part 3): a bar per feature takes its feature's colour, a grouped
    /// bar the colour its counted features share, none (the chart's) when they have several, which
    /// are counted, and an Other bar none. Without `classes`, or without colours, none are held.
    #[test]
    fn chart_bars_take_their_features_colours() {
        let ds = places();
        // A colour per name, so every feature has its own; and per region, shared within one.
        let by_name = |row: usize| Some(Rgba::rgb(row as u8, 0, 0));
        let region = ds.field_index("region").unwrap();
        let by_region = |row: usize| match ds.value(row, region) {
            rustgis_data::Value::Text(t) if t == "N" => Some(Rgba::rgb(1, 0, 0)),
            _ => Some(Rgba::rgb(2, 0, 0)),
        };
        let one = chart(serde_json::json!({"field": "people", "classes": true}));
        let d = one.chart_data_with(&ds, Some(&by_name)).unwrap();
        // Largest first: a (10), d (7), b (4), c (2), f (1), g (-3).
        let rows = [0u8, 3, 1, 2, 5, 6];
        assert_eq!(d.colors, rows.iter().map(|r| Some(Rgba::rgb(*r, 0, 0))).collect::<Vec<_>>());
        assert_eq!(d.mixed, 0);
        // Grouped by region: N is a and c, which have a colour each by name.
        let grouped = chart(serde_json::json!({"field": "people", "group": "region", "classes": true}));
        let d = grouped.chart_data_with(&ds, Some(&by_name)).unwrap();
        assert_eq!(bars(&d), vec![(12.0, "N"), (7.0, "E"), (4.0, "S"), (1.0, CHART_BLANK), (-3.0, "W")]);
        // S is b alone: e has no number, so its sum doesn't read it.
        assert_eq!(
            d.colors,
            vec![None, Some(Rgba::rgb(3, 0, 0)), Some(Rgba::rgb(1, 0, 0)), Some(Rgba::rgb(5, 0, 0)), Some(Rgba::rgb(6, 0, 0))]
        );
        assert_eq!(d.mixed, 1);
        // A count reads every feature: S is b and e then.
        let counted = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "count", "classes": true}));
        let d = counted.chart_data_with(&ds, Some(&by_name)).unwrap();
        assert_eq!(d.mixed, 2);
        // By region, every bar has one colour; the Other bar keeps the chart's.
        let cut = chart(serde_json::json!({"field": "people", "group": "region", "bars": 3, "other": true, "classes": true}));
        let d = cut.chart_data_with(&ds, Some(&by_region)).unwrap();
        assert_eq!(bars(&d).last().map(|b| b.1), Some(CHART_OTHER));
        assert_eq!(d.colors, vec![Some(Rgba::rgb(1, 0, 0)), Some(Rgba::rgb(2, 0, 0)), None]);
        assert_eq!((d.mixed, d.unshown), (0, 0), "the Other bar is neither");
        // Features drawn with nothing (a hidden class) give their bar none, not a clear one: N's
        // features are hidden, so N keeps the chart's colour without being mixed, and is counted.
        let hidden_n = |row: usize| by_region(row).filter(|c| *c != Rgba::rgb(1, 0, 0));
        let d = grouped.chart_data_with(&ds, Some(&hidden_n)).unwrap();
        assert_eq!(d.colors.first(), Some(&None));
        assert_eq!((d.mixed, d.unshown), (0, 1));
        let d = one.chart_data_with(&ds, Some(&hidden_n)).unwrap();
        assert_eq!(d.colors.first(), Some(&None), "a, in N");
        assert_eq!(d.unshown, 2, "a and c, in N");
        // Only some hidden, the bar takes the colour of those that show: N is a, hidden, and c.
        let hidden_a = |row: usize| by_name(row).filter(|_| row != 0);
        let d = grouped.chart_data_with(&ds, Some(&hidden_a)).unwrap();
        assert_eq!(d.colors.first(), Some(&Some(Rgba::rgb(2, 0, 0))));
        assert_eq!((d.mixed, d.unshown), (0, 0));
        // The colour a symbol shows: a line's stroke, an area's fill or its outline where the
        // fill is clear, none when it draws nothing.
        use crate::symbology::Symbol;
        use rustgis_data::GeometryType as G;
        let red = Rgba::rgb(200, 0, 0);
        let area = Symbol { fill: red, ..Symbol::default_for(G::Polygon, 0) };
        assert_eq!(area.shown_colour(G::Polygon), Some(red));
        assert_eq!(area.shown_colour(G::Polyline), Some(area.stroke));
        let outline = Symbol { fill: Rgba::new(0, 0, 0, 0), ..area };
        assert_eq!(outline.shown_colour(G::Polygon), Some(area.stroke));
        let hidden = Symbol { stroke: Rgba::new(0, 0, 0, 0), ..outline };
        assert_eq!(hidden.shown_colour(G::Polygon), None);
        // A point drawn as an image shows no one colour; drawn as a shape, its fill.
        let dot = Symbol { fill: red, ..Symbol::default_for(G::Point, 0) };
        assert_eq!(dot.shown_colour(G::Point), Some(red));
        let image = Symbol { marker: crate::symbology::Marker::Picture(crate::symbology::Picture::new("iVBORw0KGgo=")), ..dot };
        assert_eq!(image.shown_colour(G::Point), None);
        // Not coloured: no colours are read.
        assert!(one.chart_data(&ds).unwrap().colors.is_empty());
        let plain = chart(serde_json::json!({"field": "people"}));
        assert!(plain.chart_data(&ds).unwrap().colors.is_empty());
        assert!(!plain.chart_uses_classes() && one.chart_uses_classes());
    }

    /// Places by region and kind (#2487 part 3b): N has an x and a y, S an x and a y with no
    /// number, E a y and one without a kind, W a negative x.
    fn kinds() -> rustgis_data::Dataset {
        use rustgis_data::{Dataset, Field, FieldType, Value};
        let mut ds = Dataset::new(
            "t",
            rustgis_data::GeometryType::Point,
            vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Text), Field::new("people", FieldType::Double)],
        );
        let text = |t: &str| Value::Text(t.into());
        for (region, kind, people) in [
            ("N", text("x"), Value::Float(10.0)),
            ("N", text("y"), Value::Float(2.0)),
            ("S", text("x"), Value::Float(4.0)),
            ("S", text("y"), Value::Null),
            ("E", text("y"), Value::Float(7.0)),
            ("E", Value::Null, Value::Float(1.0)),
            ("W", text("x"), Value::Float(-3.0)),
        ] {
            ds.push(None, vec![text(region), kind, people]);
        }
        ds
    }

    fn key(d: &ChartData) -> Vec<&str> {
        d.key.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// A grouped chart that sums or counts splits each bar by the values of its stack field, in
    /// the order of the key: their totals, largest first, or by label A to Z; the stretches add up
    /// to the bar, an Other bar's to its groups'. Ungrouped, by its own group field, or taking a
    /// mean, it isn't stacked.
    #[test]
    fn stacked_bars_split_by_a_second_field() {
        let ds = kinds();
        let palette = crate::diagram::CATEGORY_COLORS;
        let summed = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind"}));
        let d = summed.chart_data(&ds).unwrap();
        assert_eq!(bars(&d), vec![(12.0, "N"), (8.0, "E"), (4.0, "S"), (-3.0, "W")]);
        assert_eq!(key(&d), vec!["x", "y", CHART_BLANK]);
        assert_eq!(d.key.iter().map(|k| k.1).collect::<Vec<_>>(), palette[..3].to_vec());
        assert_eq!(d.stretches, vec![vec![10.0, 2.0, 0.0], vec![0.0, 7.0, 1.0], vec![4.0, 0.0, 0.0], vec![-3.0, 0.0, 0.0]]);
        assert_eq!((d.no_number, d.key_kept), (1, 0));
        assert!(d.colors.is_empty());

        // Counted, every feature is in a stretch, the one with no number too.
        let counted = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "count", "stack": "kind"}));
        let d = counted.chart_data(&ds).unwrap();
        assert_eq!(bars(&d), vec![(2.0, "N"), (2.0, "S"), (2.0, "E"), (1.0, "W")]);
        assert_eq!(d.stretches, vec![vec![1.0, 1.0, 0.0], vec![1.0, 1.0, 0.0], vec![0.0, 1.0, 1.0], vec![1.0, 0.0, 0.0]]);

        // An Other bar adds up the stretches of the groups it gathers.
        let cut = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "bars": 2, "other": true}));
        let d = cut.chart_data(&ds).unwrap();
        assert_eq!(bars(&d), vec![(12.0, "N"), (9.0, CHART_OTHER)]);
        assert_eq!(d.stretches, vec![vec![10.0, 2.0, 0.0], vec![1.0, 7.0, 1.0]]);
        // The bars left out leave the key too: N shows no blank kind.
        let left = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "bars": 1}));
        let d = left.chart_data(&ds).unwrap();
        assert_eq!((key(&d), d.stretches.clone(), d.left_out), (vec!["x", "y"], vec![vec![10.0, 2.0]], 3));

        // By label, the key goes A to Z as the bars do.
        let by_label = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "order": "category"}));
        let d = by_label.chart_data(&ds).unwrap();
        assert_eq!(key(&d), vec![CHART_BLANK, "x", "y"]);
        assert_eq!(d.stretches.first(), Some(&vec![1.0, 0.0, 7.0]));

        for flat in [
            serde_json::json!({"field": "people", "stack": "kind"}),
            serde_json::json!({"field": "people", "group": "region", "stack": "Region"}),
            serde_json::json!({"field": "people", "group": "region", "statistic": "mean", "stack": "kind"}),
            serde_json::json!({"field": "people", "group": "region", "stack": " "}),
            serde_json::json!({"field": "people", "group": "region", "stack": "region "}),
        ] {
            let kind = chart(flat.clone());
            // A blank one doesn't ask a reader for stacking.
            assert!(kind.chart_uses_stack() != (flat["stack"] == " ") && kind.chart_stack().is_none());
            let d = kind.chart_data(&ds).unwrap();
            assert!(d.key.is_empty() && d.stretches.is_empty());
        }
        assert!(summed.chart_uses_stack() && !chart(serde_json::json!({"field": "people"})).chart_uses_stack());
        // A stack field the layer hasn't (deleted since, say) leaves the bars whole, and says so.
        let gone = chart(serde_json::json!({"field": "people", "group": "region", "stack": "gone"})).chart_data(&ds).unwrap();
        let whole = chart(serde_json::json!({"field": "people", "group": "region"})).chart_data(&ds).unwrap();
        assert!(gone.stack_gone && gone.key.is_empty() && gone.stretches.is_empty() && gone.bars == whole.bars && !whole.stack_gone);
    }

    /// A bar a stretch of which adds up past a number's range, or whose stretches do once stacked,
    /// its total within it, is drawn whole and counted, its stretches left empty rather than that
    /// one taken as 0; the others stack.
    #[test]
    fn a_stretch_past_a_numbers_range_leaves_its_bar_whole() {
        let names = || vec![Some("x".to_string()), Some("y".to_string())];
        // Bar 0: x = 1e308 twice, past the range, y = -1e308. Bar 1, Other of groups 1 and 2,
        // each with an x of 1e308 and group 1 a y of -1e308: x adds up past it once merged.
        let cells =
            |c: &[((usize, usize), f64)]| c.iter().map(|(p, v)| (*p, (*v, Shade::Unread))).collect::<std::collections::BTreeMap<_, _>>();
        let pairs =
            [((0, 0), f64::INFINITY), ((0, 1), -1e308), ((1, 0), 1e308), ((1, 1), -1e308), ((2, 0), 1e308), ((3, 0), 2.0), ((3, 1), 1.0)];
        let bar_of = [Some(0), Some(1), Some(1), Some(2)];
        let (key, stretches, _, _, whole) = stack_bars(&cells(&pairs), &bar_of, 3, names(), false, true);
        assert_eq!(key.len(), 2);
        assert_eq!(stretches, vec![Vec::new(), Vec::new(), vec![2.0, 1.0]]);
        assert_eq!(whole, 2);
        // Each stretch within the range, x and y above zero adding up past it once stacked, the
        // bar's total (z below zero) still within it.
        let names = vec![Some("x".to_string()), Some("y".to_string()), Some("z".to_string())];
        let pairs = [((0, 0), 1e308), ((0, 1), 1e308), ((0, 2), -1e308)];
        let (_, stretches, _, _, whole) = stack_bars(&cells(&pairs), &[Some(0)], 1, names, false, true);
        assert_eq!((stretches, whole), (vec![Vec::new()], 1));
        // Its values' totals past the range too, x and y order as 0, after z's 1.
        let names = vec![Some("x".to_string()), Some("y".to_string()), Some("z".to_string())];
        let pairs = [((0, 0), 1e308), ((0, 1), 1e308), ((1, 0), 1e308), ((1, 1), 1e308), ((1, 2), 1.0)];
        let (key, ..) = stack_bars(&cells(&pairs), &[Some(0), Some(1)], 2, names, false, false);
        assert_eq!(key.first().map(|k| k.0.as_str()), Some("z"));
    }

    /// Past the palette's colours, the key keeps all but the last of the values with the largest
    /// totals and gathers the rest in a grey Other.
    #[test]
    fn a_stack_key_past_its_colours_gathers_an_other() {
        use rustgis_data::{Dataset, Field, FieldType, Value};
        let mut ds = Dataset::new(
            "t",
            rustgis_data::GeometryType::Point,
            vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Integer), Field::new("people", FieldType::Double)],
        );
        for k in 0..10 {
            ds.push(None, vec![Value::Text("A".into()), Value::Int(k), Value::Float(10.0 - k as f64)]);
        }
        let d = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind"})).chart_data(&ds).unwrap();
        assert_eq!(key(&d), vec!["0", "1", "2", "3", "4", "5", "6", CHART_OTHER]);
        assert_eq!(d.key.len(), CHART_STACKS);
        assert_eq!(d.key.last().map(|k| k.1), Some(crate::symbology::OTHER_FILL));
        assert_eq!(d.stretches, vec![vec![10.0, 9.0, 8.0, 7.0, 6.0, 5.0, 4.0, 6.0]]);
        assert_eq!(d.key_gathered, 3);
        assert_eq!(
            chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "bars": 1}))
                .chart_data(&kinds())
                .unwrap()
                .key_gathered,
            0
        );
        // By label, the key still keeps the largest values, ordered A to Z once kept.
        let mut ds = Dataset::new(
            "t",
            rustgis_data::GeometryType::Point,
            vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Integer), Field::new("people", FieldType::Double)],
        );
        for k in 0..10 {
            ds.push(None, vec![Value::Text("A".into()), Value::Int(k), Value::Float(k as f64 + 1.0)]);
        }
        let d =
            chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "order": "category"})).chart_data(&ds).unwrap();
        assert_eq!(key(&d), vec!["3", "4", "5", "6", "7", "8", "9", CHART_OTHER]);
        assert_eq!(d.stretches, vec![vec![4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 6.0]]);
    }

    /// Stacked and coloured by classes, each value of the key takes the colour all its features on
    /// the bars shown share on the map; one whose features have several, or show none, keeps its
    /// palette colour and is counted. The bars hold no colours of their own.
    #[test]
    fn stacked_bars_colour_their_key_by_classes() {
        let ds = kinds();
        let palette = crate::diagram::CATEGORY_COLORS;
        let kind = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "classes": true}));
        let ki = ds.field_index("kind").unwrap();
        let (red, blue, green) = (Rgba::rgb(200, 0, 0), Rgba::rgb(0, 0, 200), Rgba::rgb(0, 200, 0));
        let by_kind = |row: usize| match ds.value(row, ki) {
            rustgis_data::Value::Text(t) if t == "x" => Some(red),
            rustgis_data::Value::Text(_) => Some(blue),
            _ => Some(green),
        };
        let d = kind.chart_data_with(&ds, Some(&by_kind)).unwrap();
        assert_eq!(d.key.iter().map(|k| k.1).collect::<Vec<_>>(), vec![red, blue, green]);
        assert_eq!((d.key_kept, d.mixed, d.unshown), (0, 0, 0));
        assert!(d.colors.is_empty());
        // A colour per feature: x and y have several, the blank one feature's own.
        let by_row = |row: usize| Some(Rgba::rgb(row as u8, 0, 0));
        let d = kind.chart_data_with(&ds, Some(&by_row)).unwrap();
        assert_eq!(d.key.iter().map(|k| k.1).collect::<Vec<_>>(), vec![palette[0], palette[1], Rgba::rgb(5, 0, 0)]);
        assert_eq!(d.key_kept, 2);
        // Drawn with nothing, every value keeps its palette colour.
        let d = kind.chart_data_with(&ds, Some(&|_| None)).unwrap();
        assert_eq!(d.key.iter().map(|k| k.1).collect::<Vec<_>>(), palette[..3].to_vec());
        assert_eq!(d.key_kept, 3);
        // A value's colour is read over the bars shown: x red on A's bar, blue on B's left out,
        // and mixed once B is gathered in an Other bar.
        use rustgis_data::{Dataset, Field, FieldType, Value};
        let mut ds = Dataset::new(
            "t",
            rustgis_data::GeometryType::Point,
            vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Text), Field::new("people", FieldType::Double)],
        );
        ds.push(None, vec![Value::Text("A".into()), Value::Text("x".into()), Value::Float(10.0)]);
        ds.push(None, vec![Value::Text("B".into()), Value::Text("x".into()), Value::Float(1.0)]);
        let by_region = |row: usize| Some(if row == 0 { red } else { blue });
        let one = |other: bool| {
            chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "classes": true, "bars": 1, "other": other}))
                .chart_data_with(&ds, Some(&by_region))
                .unwrap()
        };
        let d = one(false);
        assert_eq!((d.key.iter().map(|k| k.1).collect::<Vec<_>>(), d.key_kept), (vec![red], 0));
        let d = one(true);
        assert_eq!((d.key.iter().map(|k| k.1).collect::<Vec<_>>(), d.key_kept), (vec![palette[0]], 1));
    }

    /// Ungrouped, a bar per feature with a number, largest first, labelled by its field; the
    /// rest are counted, not drawn.
    #[test]
    fn an_ungrouped_chart_draws_a_bar_per_feature() {
        let d = chart(serde_json::json!({"field": "people", "label": "name"})).chart_data(&places()).unwrap();
        assert_eq!(bars(&d), [(10.0, "a"), (7.0, "d"), (4.0, "b"), (2.0, "c"), (1.0, "f"), (-3.0, "g")]);
        assert_eq!((d.labelled, d.left_out, d.no_number, d.title.as_str()), (true, 0, 1, "people"));
        // A label field the layer hasn't leaves the bars unlabelled; a field it hasn't, no chart.
        assert!(!chart(serde_json::json!({"field": "people", "label": "gone"})).chart_data(&places()).unwrap().labelled);
        assert!(chart(serde_json::json!({"field": "gone"})).chart_data(&places()).is_none());
        assert!(chart(serde_json::json!({"field": "people", "group": "gone"})).chart_data(&places()).is_none());
    }

    /// Grouped, a bar per value (blank ones together), the statistic over its features; a count
    /// counts features with no number, the others leave them out and say how many.
    #[test]
    fn a_grouped_chart_draws_the_statistic_per_value() {
        let ds = places();
        let grouped =
            |stat: &str| chart(serde_json::json!({"field": "people", "group": "region", "statistic": stat})).chart_data(&ds).unwrap();
        let sum = grouped("sum");
        assert_eq!(bars(&sum), [(12.0, "N"), (7.0, "E"), (4.0, "S"), (1.0, CHART_BLANK), (-3.0, "W")]);
        assert_eq!((sum.labelled, sum.no_number, sum.title.as_str()), (true, 1, "Sum of people by region"));
        let count = grouped("count");
        assert_eq!(bars(&count), [(2.0, "N"), (2.0, "S"), (1.0, "E"), (1.0, CHART_BLANK), (1.0, "W")]);
        assert_eq!((count.no_number, count.title.as_str()), (0, "Count by region"));
        assert_eq!(bars(&grouped("mean")), [(7.0, "E"), (6.0, "N"), (4.0, "S"), (1.0, CHART_BLANK), (-3.0, "W")]);
        assert_eq!(bars(&grouped("min"))[0], (7.0, "E"));
        assert_eq!(bars(&grouped("max"))[0], (10.0, "N"));
        // A value whose features have no number has no bar.
        let mut only_null = places();
        only_null.features.truncate(1);
        let i = only_null.field_index("people").unwrap();
        only_null.features[0].attrs[i] = rustgis_data::Value::Null;
        let d = chart(serde_json::json!({"field": "people", "group": "region"})).chart_data(&only_null).unwrap();
        assert_eq!((d.bars.len(), d.no_number), (0, 1));
        // A value that reads "(blank)" is a bar of its own; a blank one joins the features with none.
        let mut blanks = places();
        let i = blanks.field_index("region").unwrap();
        blanks.features[0].attrs[i] = rustgis_data::Value::Text(CHART_BLANK.into());
        blanks.features[1].attrs[i] = rustgis_data::Value::Text("  ".into());
        let d = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "count", "order": "layer"}))
            .chart_data(&blanks)
            .unwrap();
        assert_eq!(bars(&d), [(1.0, CHART_BLANK), (2.0, CHART_BLANK), (1.0, "N"), (1.0, "E"), (1.0, "S"), (1.0, "W")]);
    }

    /// The four orders; ties keep the layer's order.
    #[test]
    fn a_chart_orders_its_bars() {
        let ds = places();
        let ordered = |order: &str| {
            let d = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "count", "order": order}))
                .chart_data(&ds)
                .unwrap();
            d.bars.into_iter().map(|(_, n)| n).collect::<Vec<_>>()
        };
        assert_eq!(ordered("descending"), ["N", "S", "E", CHART_BLANK, "W"]);
        assert_eq!(ordered("ascending"), ["E", CHART_BLANK, "W", "N", "S"]);
        assert_eq!(ordered("category"), [CHART_BLANK, "E", "N", "S", "W"]);
        assert_eq!(ordered("layer"), ["N", "S", "E", CHART_BLANK, "W"]);
    }

    /// Past its limit, bars are left out and counted, or gathered in a last Other bar measured
    /// over their features, whatever the order; a limit of 1 gathers them all.
    #[test]
    fn a_chart_past_its_limit_leaves_bars_out_or_gathers_them() {
        let ds = places();
        let cut = |extra: serde_json::Value| {
            let mut keys = serde_json::json!({"field": "people", "group": "region"});
            if let (Some(k), Some(e)) = (keys.as_object_mut(), extra.as_object()) {
                k.extend(e.clone());
            }
            chart(keys).chart_data(&ds).unwrap()
        };
        let left = cut(serde_json::json!({"bars": 2}));
        assert_eq!((bars(&left), left.left_out), (vec![(12.0, "N"), (7.0, "E")], 3));
        let other = cut(serde_json::json!({"bars": 3, "other": true}));
        assert_eq!((bars(&other), other.left_out), (vec![(12.0, "N"), (7.0, "E"), (2.0, CHART_OTHER)], 0));
        // Ascending, Other still comes last; its mean is over the features of the bars it gathers.
        let mean = cut(serde_json::json!({"bars": 2, "other": true, "order": "ascending", "statistic": "mean"}));
        assert_eq!(bars(&mean), [(-3.0, "W"), (4.8, CHART_OTHER)]);
        let all = cut(serde_json::json!({"bars": 1, "other": true, "statistic": "count"}));
        assert_eq!(bars(&all), [(7.0, CHART_OTHER)]);
        // Fewer bars than the limit: no Other bar.
        assert_eq!(cut(serde_json::json!({"bars": 9, "other": true})).bars.len(), 5);
        // Ungrouped, Other sums the features past the limit; a limit out of range is read clamped.
        // Unlabelled, the bars are labelled then, so the Other bar is named.
        let ungrouped = chart(serde_json::json!({"field": "people", "bars": 3, "other": true})).chart_data(&ds).unwrap();
        assert_eq!((bars(&ungrouped), ungrouped.labelled), (vec![(10.0, ""), (7.0, ""), (4.0, CHART_OTHER)], true));
        assert!(!chart(serde_json::json!({"field": "people", "bars": 3})).chart_data(&ds).unwrap().labelled);
        assert_eq!(cut(serde_json::json!({"bars": 0})).bars.len(), 1);
        let mut many = rustgis_data::Dataset::new(
            "t",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("people", rustgis_data::FieldType::Double)],
        );
        for i in 0..150 {
            many.push(None, vec![rustgis_data::Value::Float(i as f64)]);
        }
        let d = chart(serde_json::json!({"field": "people", "bars": 5000})).chart_data(&many).unwrap();
        assert_eq!((d.bars.len(), d.left_out), (MAX_CHART_BARS, 50));
        // An Other bar past a number's range isn't drawn: the bars it would gather are left out.
        let mut huge = places();
        let i = huge.field_index("people").unwrap();
        for f in &mut huge.features {
            f.attrs[i] = rustgis_data::Value::Float(f64::MAX);
        }
        let d = chart(serde_json::json!({"field": "people", "bars": 2, "other": true})).chart_data(&huge).unwrap();
        assert_eq!((d.bars.len(), d.left_out, d.out_of_range), (1, 0, 6));
        // So is a grouped bar's: N sums two of them; a count of the same features is drawn.
        let grouped =
            |stat: &str| chart(serde_json::json!({"field": "people", "group": "region", "statistic": stat})).chart_data(&huge).unwrap();
        let sum = grouped("sum");
        assert_eq!((sum.bars.len(), sum.out_of_range), (3, 2));
        assert!(sum.bars.iter().all(|(v, n)| v.is_finite() && n != "N" && n != "S"));
        assert_eq!((grouped("count").bars.len(), grouped("count").out_of_range), (5, 0));
    }

    /// The options are left out at their defaults, so a chart saved before keeps its bytes, and
    /// read back as written otherwise.
    #[test]
    fn a_charts_options_are_saved_only_when_set() {
        let plain = chart(serde_json::json!({"field": "people"}));
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            serde_json::json!({"Chart": {"frame": 1, "layer": "t", "field": "people", "label": null}})
        );
        assert!(!plain.chart_uses_data_options());
        let set = chart(
            serde_json::json!({"field": "people", "group": "region", "statistic": "mean", "order": "category", "bars": 5, "other": true}),
        );
        let back: ElementKind = serde_json::from_value(serde_json::to_value(&set).unwrap()).unwrap();
        assert_eq!(back, set);
        assert!(set.chart_uses_data_options());
        for one in [
            serde_json::json!({"group": "region"}),
            // Kept while ungrouped, which an older reader would save without.
            serde_json::json!({"statistic": "count"}),
            serde_json::json!({"order": "layer"}),
            serde_json::json!({"bars": 21}),
            serde_json::json!({"other": true}),
        ] {
            let mut keys = one.clone();
            keys["field"] = serde_json::json!("people");
            assert!(chart(keys).chart_uses_data_options(), "{one}");
        }
    }

    /// Its look options are left out at their defaults too, read back as written otherwise, and
    /// each one alone tells an older reader it can't draw the chart.
    #[test]
    fn a_charts_look_is_saved_only_when_set() {
        let plain = chart(serde_json::json!({"field": "people"}));
        assert!(!plain.chart_uses_look_options());
        assert_eq!(plain.chart_look(), Some(ChartLook::default()));
        let set = chart(serde_json::json!({
            "field": "people", "orientation": "Vertical", "values": true, "decimals": 0, "title": "",
            "bar_color": [194, 24, 91, 255], "label_align": "Right"
        }));
        let back: ElementKind = serde_json::from_value(serde_json::to_value(&set).unwrap()).unwrap();
        assert_eq!(back, set);
        let look = set.chart_look().unwrap();
        assert_eq!((look.orientation, look.values, look.decimals), (Orientation::Vertical, true, Some(0)));
        assert_eq!((look.bar_color, look.label_align), (Rgba::rgb(194, 24, 91), TextAlign::Right));
        for one in [
            serde_json::json!({"orientation": "Vertical"}),
            serde_json::json!({"values": true}),
            serde_json::json!({"decimals": 2}),
            serde_json::json!({"title": ""}),
            serde_json::json!({"bar_color": [0, 0, 0, 255]}),
            serde_json::json!({"label_align": "Right"}),
        ] {
            let mut keys = one.clone();
            keys["field"] = serde_json::json!("people");
            let c = chart(keys);
            assert!(c.chart_uses_look_options() && !c.chart_uses_data_options(), "{one}");
        }
    }

    /// Its own title, none, or the title it is given; the data's title follows it.
    #[test]
    fn a_chart_takes_its_own_title_or_none() {
        let ds = places();
        let titled = |extra: serde_json::Value| {
            let mut keys = serde_json::json!({"field": "people", "group": "region", "statistic": "mean"});
            if let (Some(k), Some(e)) = (keys.as_object_mut(), extra.as_object()) {
                k.extend(e.clone());
            }
            chart(keys).chart_data(&ds).unwrap().title
        };
        assert_eq!(titled(serde_json::json!({})), "Mean of people by region");
        assert_eq!(titled(serde_json::json!({"title": "People"})), "People");
        assert_eq!(titled(serde_json::json!({"title": ""})), "");
        assert_eq!(ElementKind::text("x", 9.0, INK, false).chart_title(), None);
    }

    /// A value as written on its bar: thousands apart, up to two decimals with the zeros they
    /// end in dropped, or a fixed number of them.
    #[test]
    fn a_charts_values_are_written_with_thousands_and_decimals() {
        for (v, decimals, written) in [
            (2095.0, None, "2,095"),
            (13.3, None, "13.3"),
            (1_349_524.0, None, "1,349,524"),
            (0.126, None, "0.13"),
            (0.5, None, "0.5"),
            (-1234.5, None, "-1,234.5"),
            (100.0, None, "100"),
            (-0.001, None, "0"),
            (6.6, Some(0), "7"),
            (6.0, Some(2), "6.00"),
            (-12345.678, Some(1), "-12,345.7"),
            (1.0, Some(9), "1.000000"),
        ] {
            assert_eq!(chart_value_text(v, decimals), written, "{v} {decimals:?}");
        }
    }

    /// Numbers group by their value, looked up without writing each one out, and values written
    /// alike stay one bar: a whole number and its decimal spelling, a number and its text.
    #[test]
    fn a_chart_groups_numbers_by_what_they_read() {
        use rustgis_data::{Dataset, Field, FieldType, Value};
        let mut ds = Dataset::new(
            "t",
            rustgis_data::GeometryType::Point,
            vec![Field::new("code", FieldType::Double), Field::new("people", FieldType::Double)],
        );
        for code in [Value::Int(3), Value::Float(3.0), Value::Float(2.5), Value::Text("3".into()), Value::Float(2.5), Value::Bool(true)] {
            ds.push(None, vec![code, Value::Float(1.0)]);
        }
        let d =
            chart(serde_json::json!({"field": "people", "group": "code", "statistic": "count", "order": "layer"})).chart_data(&ds).unwrap();
        assert_eq!(bars(&d), [(3.0, "3"), (2.0, "2.5"), (1.0, "True")]);
    }

    /// A mean past what a sum can hold is still drawn: kept as the numbers come, not summed.
    #[test]
    fn a_charts_mean_stays_in_range_where_its_sum_doesnt() {
        let mut huge = places();
        let i = huge.field_index("people").unwrap();
        for f in &mut huge.features {
            f.attrs[i] = rustgis_data::Value::Float(f64::MAX);
        }
        let mean = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "mean"})).chart_data(&huge).unwrap();
        assert_eq!((mean.bars.len(), mean.out_of_range), (5, 0));
        assert!(mean.bars.iter().all(|(v, _)| *v == f64::MAX), "{:?}", mean.bars);
        // So is an Other bar's, gathered from such groups.
        let other = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "mean", "bars": 2, "other": true}))
            .chart_data(&huge)
            .unwrap();
        assert_eq!((bars(&other).last().copied(), other.out_of_range), (Some((f64::MAX, CHART_OTHER)), 0));
        // Gathered from groups of opposite signs, too: the largest number and twice its opposite.
        let mut mixed = places();
        mixed.features.clear();
        for (region, n) in [("A", f64::MAX), ("B", -f64::MAX), ("B", -f64::MAX)] {
            mixed.push(
                None,
                vec![rustgis_data::Value::Text("x".into()), rustgis_data::Value::Text(region.into()), rustgis_data::Value::Float(n)],
            );
        }
        let all = chart(
            serde_json::json!({"field": "people", "group": "region", "statistic": "mean", "order": "layer", "bars": 1, "other": true}),
        )
        .chart_data(&mixed)
        .unwrap();
        let (v, name) = bars(&all)[0];
        assert!(name == CHART_OTHER && (v / (-f64::MAX / 3.0) - 1.0).abs() < 1e-9, "{:?}", bars(&all));
    }

    /// Under the balanced light (#1977) flat ground is drawn in the ramp's own colour, so a relief's
    /// colour bar is that colour, whatever the sun's height; under the plain light, the ramp.
    #[test]
    fn a_balanced_reliefs_colour_bar_is_its_flat_ground() {
        use crate::Ramp;
        use crate::raster::{Colorizer, RasterLayer, hillshade};
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let gt = GeoTransform { origin_x: 0.0, origin_y: 10.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r =
            Raster::new("dem", 10, 10, vec![(0..100).map(|v| v as f32).collect()], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32")
                .unwrap();
        for (altitude, balanced) in [(45.0, true), (20.0, true), (45.0, false)] {
            let mut rl = RasterLayer::from_path(r.clone(), "dem.tif");
            rl.style.renderer = RasterRenderer::ShadedRelief {
                band: 0,
                ramp: Some(Ramp::Terrain),
                azimuth: 315.0,
                altitude,
                z_factor: 1.0,
                shadow: 0.0,
                sky: 0.0,
                fill: 0.0,
                rock: None,
                ambient: None,
                balanced,
            };
            let Some(ColorScale::Ramp { lo, hi, colors }) = color_scale(&rl) else { panic!("a ramp") };
            let colorizer = Colorizer::new(&rl.style, &r, None);
            let flat = if balanced { hillshade(&[0.0; 9], 1.0, 1.0, 315.0, altitude, 1.0) } else { 255 };
            for (i, c) in colors.iter().enumerate() {
                let v = lo + (hi - lo) * i as f64 / (colors.len() - 1) as f64;
                assert_eq!(Some(c.0), colorizer.color(&[v as f32], Some(flat)), "{altitude}° balanced {balanced}: sample {i}");
            }
        }
    }

    #[test]
    fn elements_are_arranged_in_the_drawing_order() {
        let mut l = Layout::new("L", 100.0, 100.0);
        let rect = || ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 0.0, radius: 0.0, dash: None };
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
                border_color: FRAME_BORDER_COLOR,
                border_width: FRAME_BORDER_WIDTH,
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

    /// A legend's text size (#2581): 9 points unless set, and left out then, so a layout saved
    /// before keeps its bytes; one past its range is brought into it, and counted.
    #[test]
    fn a_legends_text_size_defaults_and_is_kept_in_range() {
        let page = |legend: &str| {
            format!(
                r#"{{"name":"x","width":297,"height":210,"elements":[
                {{"id":1,"name":"f","x":0,"y":0,"w":100,"h":100,"kind":{{"MapFrame":{{"map":"M","center":{{"x":0,"y":0}},"scale":1e6}}}}}},
                {{"id":2,"name":"l","x":0,"y":0,"w":50,"h":50,"kind":{{"Legend":{legend}}}}}]}}"#
            )
        };
        let size = |l: &Layout| match &l.elements[1].kind {
            ElementKind::Legend { text_size, .. } => *text_size,
            _ => panic!("legend expected"),
        };
        let old: Layout = serde_json::from_str(&page(r#"{"frame":1,"title":"L"}"#)).unwrap();
        assert_eq!(size(&old), LEGEND_TEXT_PT);
        assert!(!old.elements[1].kind.has_custom_text_size());
        assert!(!serde_json::to_string(&old).unwrap().contains("text_size"), "the default isn't written");
        let mut big: Layout = serde_json::from_str(&page(r#"{"frame":1,"title":"L","text_size":1e9}"#)).unwrap();
        assert_eq!(big.clamp().elements_adjusted, 1);
        assert_eq!(size(&big), *LAYOUT_TEXT_SIZE.end());
        assert!(big.elements[1].kind.has_custom_text_size());
        let saved = serde_json::to_string(&big).unwrap();
        assert!(saved.contains(r#""text_size":100.0"#), "{saved}");
        assert_eq!(serde_json::from_str::<Layout>(&saved).unwrap(), big);
    }

    #[test]
    fn scale_bar_and_grid_label_sizes_default_and_are_kept_in_range() {
        let page = |grid: &str, bar: &str| {
            format!(
                r#"{{"name":"x","width":297,"height":210,"elements":[
                {{"id":1,"name":"f","x":0,"y":0,"w":100,"h":100,"kind":{{"MapFrame":{{"map":"M","center":{{"x":0,"y":0}},"scale":1e6,"grid":{grid}}}}}}},
                {{"id":2,"name":"s","x":0,"y":0,"w":50,"h":10,"kind":{{"ScaleBar":{bar}}}}}]}}"#
            )
        };
        let sizes = |l: &Layout| match (&l.elements[0].kind, &l.elements[1].kind) {
            (ElementKind::MapFrame { grid: Some(g), .. }, ElementKind::ScaleBar { text_size, .. }) => (g.label_size, *text_size),
            _ => panic!("a frame with a grid and a scale bar expected"),
        };
        let old: Layout = serde_json::from_str(&page(r#"{"step":10,"color":[0,0,0,255]}"#, r#"{"frame":1}"#)).unwrap();
        assert_eq!(sizes(&old), (GRID_LABEL_PT, SCALE_BAR_TEXT_PT));
        assert!(old.elements.iter().all(|e| !e.kind.has_custom_text_size()));
        let text = serde_json::to_string(&old).unwrap();
        assert!(!text.contains("text_size") && !text.contains("label_size"), "the defaults aren't written: {text}");
        assert_eq!(ElementKind::scale_bar(1), old.elements[1].kind, "a new bar is the default one");
        // Out of range, or not a number a reader can draw at, each is brought in and counted.
        for (bad, (grid, bar)) in [("1e9", (100.0, 100.0)), ("-3", (4.0, 4.0)), ("0.5", (4.0, 4.0))] {
            let mut l: Layout = serde_json::from_str(&page(
                &format!(r#"{{"step":10,"color":[0,0,0,255],"label_size":{bad}}}"#),
                &format!(r#"{{"frame":1,"text_size":{bad}}}"#),
            ))
            .unwrap();
            assert_eq!(l.clamp().elements_adjusted, 2, "{bad}");
            assert_eq!(sizes(&l), (grid, bar), "{bad}");
            assert!(l.elements.iter().all(|e| e.kind.has_custom_text_size()), "{bad}");
            assert!(l.clamp().is_empty(), "{bad}: a repaired layout needs nothing more");
        }
        let mut sized: Layout =
            serde_json::from_str(&page(r#"{"step":10,"color":[0,0,0,255],"label_size":14}"#, r#"{"frame":1,"text_size":16}"#)).unwrap();
        assert!(sized.clamp().is_empty(), "sizes in range are left as they are");
        assert_eq!(sizes(&sized), (14.0, 16.0));
        let saved = serde_json::to_string(&sized).unwrap();
        assert!(saved.contains(r#""label_size":14.0"#) && saved.contains(r#""text_size":16.0"#), "{saved}");
        assert_eq!(serde_json::from_str::<Layout>(&saved).unwrap(), sized);
        // A non-finite size, as a program may hand it, falls back to the default.
        assert_eq!(layout_text_in_range(f64::NAN, SCALE_BAR_TEXT_PT), SCALE_BAR_TEXT_PT);
        assert_eq!(layout_text_in_range(f64::INFINITY, GRID_LABEL_PT), GRID_LABEL_PT);
    }

    #[test]
    fn hostile_layouts_are_repaired() {
        let text = r#"{"name":"x","width":1e300,"height":-5,"elements":[
            {"id":3,"name":"a","x":0,"y":0,"w":1e9,"h":10,"kind":{"MapFrame":{"map":"M","center":{"x":1e300,"y":200},"scale":-1,"shape":{"Polygon":[[0,0],[1,1]]},"extent_of":3,"extent_width":1e9,"border_width":1e9}}},
            {"id":3,"name":"b","x":0,"y":0,"w":10,"h":10,"kind":{"Text":{"text":"t","size":1e9,"color":[0,0,0,255],"letter_spacing":-1e9}}},
            {"id":9,"name":"c","x":0,"y":0,"w":10,"h":10,"kind":{"Legend":{"frame":42,"title":"L"}}}],
            "series":{"frame":77,"layer":"x"}}"#;
        let mut l: Layout = serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}"));
        let repairs = l.clamp();
        assert_eq!(
            repairs,
            LayoutRepairs { elements_adjusted: 2, elements_cut: 0, elements_removed: 1, page_adjusted: 1, series_removed: 1, mgrs_off: 0 },
            "what changed is counted"
        );
        assert!(l.clamp().is_empty(), "a repaired layout needs nothing more");
        if let ElementKind::MapFrame { bearing, .. } = &mut l.elements[0].kind {
            *bearing = 270.0;
        }
        assert!(l.clamp().is_empty(), "a bearing of 270 is -90 by another name, not a repair");
        if let ElementKind::MapFrame { pitch, .. } = &mut l.elements[0].kind {
            *pitch = 1e-7;
        }
        assert!(l.clamp().is_empty(), "a pitch too small to tilt is flat, not a repair");
        let ids: Vec<u32> = l.elements.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 2, "the legend of a missing frame goes");
        assert_ne!(ids[0], ids[1], "duplicate ids are renumbered");
        assert!(l.width <= 5000.0 && l.height >= 20.0 && l.series.is_none());
        match &l.elements[0].kind {
            ElementKind::MapFrame { scale, shape, extent_of, center, extent_width, border_width, .. } => {
                assert!(*scale == MIN_FRAME_SCALE && *shape == FrameShape::Rectangle && extent_of.is_none() && center.is_finite());
                assert_eq!(*extent_width, MAX_INDICATOR_WIDTH);
                assert_eq!(*border_width, MAX_FRAME_BORDER_WIDTH, "a border past the widest is brought into range");
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
            ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 1.0, radius: 0.0, dash: None },
        );
        assert!(a != big.elements[0].id && a <= MAX_ID + 1);
    }

    #[test]
    fn hairline_rectangles_keep_their_height() {
        let mut l = Layout::new("Layout", 210.0, 297.0);
        let rule = ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 0.0, radius: 0.0, dash: None };
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

    /// A frame centred on a pole keeps that centre, and is drawn centred there on a polar map;
    /// Web Mercator, whose poles are at infinity, draws it at 85° as frames always were, and
    /// [`frame_center_placed`] tells it can't place it (#2684).
    #[test]
    fn a_frame_centred_on_a_pole_is_drawn_there_on_a_polar_map() {
        let mut l = Layout::new("Polo", 150.0, 150.0);
        let frame: ElementKind =
            serde_json::from_value(serde_json::json!({"MapFrame":{"map":"Mapa", "center":{"x":0,"y":-90}, "scale":60000000.0}})).unwrap();
        let id = l.add("Mapa", (5.0, 5.0, 140.0, 140.0), frame);
        l.clamp();
        let center = |l: &Layout| match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::MapFrame { center, .. }) => *center,
            _ => panic!("no frame"),
        };
        assert_eq!(center(&l).y, -90.0, "the pole kept");
        // On a south polar stereographic map, the pole is the frame's centre.
        let mut polar = Map::new("Mapa");
        polar.set_projection(crate::Projection::from_code("EPSG:3031").unwrap());
        let south = Coord::new(0.0, -90.0);
        assert!(frame_center_placed(&polar, south));
        let cam = frame_camera(&polar, south, 60_000_000.0, 0.0, 0.0);
        let pole = polar.to_map(south).unwrap();
        assert!((cam.center.x - pole.x).abs() < 1e-3 && (cam.center.y - pole.y).abs() < 1e-3, "{:?} {pole:?}", cam.center);
        // On Web Mercator it can't be: drawn at 85° S.
        let merc = Map::new("Mapa");
        assert!(!frame_center_placed(&merc, south) && frame_center_placed(&merc, Coord::new(0.0, -85.0)));
        // A longitude past ±180° (the sanitizer keeps up to ±540°) is placed: only the latitude counts.
        assert!(frame_center_placed(&merc, Coord::new(300.0, 40.0)) && frame_center_placed(&merc, Coord::new(-500.0, -60.0)));
        // And drawn there: the same meridian a turn of the globe nearer, not the view's edge at 270°.
        for (lon, lat) in [(300.0, 40.0), (-500.0, -60.0)] {
            let at = merc.to_lonlat(frame_camera(&merc, Coord::new(lon, lat), 60_000_000.0, 0.0, 0.0).center).unwrap();
            let turns = (at.x - lon).rem_euclid(360.0);
            assert!((turns < 1e-6 || turns > 360.0 - 1e-6) && (at.y - lat).abs() < 1e-6, "({lon}, {lat}) drawn at {at:?}");
        }
        let cam = frame_camera(&merc, south, 60_000_000.0, 0.0, 0.0);
        let at = merc.to_lonlat(cam.center).unwrap();
        assert!((at.y + 85.0).abs() < 1e-6, "{at:?}");
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

    /// MGRS labels: the kilometres' last two digits on the lines, saved only when on; a WGS 84 UTM
    /// zone is read from its EPSG code alone; a file with them on a graticule or naming another
    /// system has them left off, counted on their own.
    #[test]
    fn mgrs_labels_need_a_wgs84_utm_zone() {
        let g = Grid { kind: GridKind::Measured, mgrs: true, ..Grid::default() };
        assert_eq!(g.measured_label(543_000.0, 1000.0), "43");
        assert_eq!(g.measured_label(5_262_500.0, 500.0), "62.5");
        assert_eq!(g.measured_label(500_000.0, 1000.0), "00", "two digits, as MGRS prints them");
        assert_eq!(g.measured_label(505_500.0, 500.0), "05.5");
        assert_eq!(Grid { mgrs: false, short: true, ..g.clone() }.measured_label(505_000.0, 1000.0), "5");
        assert_eq!(g.measured_label(500_000.0, 100_000.0), "500000");
        assert!(serde_json::to_value(Grid::default()).unwrap().get("mgrs").is_none());
        let v = serde_json::to_value(&g).unwrap();
        assert_eq!(v["mgrs"].as_bool(), Some(true));
        assert_eq!(serde_json::from_value::<Grid>(v).unwrap(), g);
        for (crs, zone) in [
            ("EPSG:32632", Some((32, true))),
            (" epsg:32760 ", Some((60, false))),
            ("32601", Some((1, true))),
            ("EPSG: 32733", Some((33, false))),
            ("epsg 32632", Some((32, true))),
            (" EPSG 32733 ", Some((33, false))),
            ("EPSG:32600", None),
            ("EPSG:32661", None),
            ("EPSG:32761", None),
            ("EPSG:25832", None),
            ("EPSG:2056", None),
            ("EPSG:326320", None),
            ("+proj=utm +zone=32 +datum=WGS84", None),
            ("", None),
        ] {
            assert_eq!(utm_zone_of(crs), zone, "{crs}");
        }
        let frame = |crs: Option<&str>, kind: GridKind| {
            let mut l = Layout::new("Layout", 297.0, 210.0);
            l.add(
                "Map Frame",
                (10.0, 10.0, 200.0, 150.0),
                ElementKind::MapFrame {
                    map: "Map".into(),
                    center: Coord::new(8.7, 47.5),
                    scale: 25_000.0,
                    shape: FrameShape::Rectangle,
                    grid: Some(Grid { crs: crs.map(Into::into), kind, ..g.clone() }),
                    extent_of: None,
                    extent_color: INDICATOR_COLOR,
                    extent_width: INDICATOR_WIDTH,
                    border_color: FRAME_BORDER_COLOR,
                    border_width: FRAME_BORDER_WIDTH,
                    bearing: 0.0,
                    pitch: 0.0,
                },
            );
            let r = l.clamp();
            let ElementKind::MapFrame { grid: Some(g), .. } = &l.elements[0].kind else { panic!() };
            (g.mgrs, r.mgrs_off, r.elements_adjusted)
        };
        assert_eq!(frame(None, GridKind::Measured), (true, 0, 0));
        assert_eq!(frame(Some("EPSG:32632"), GridKind::Measured), (true, 0, 0));
        assert_eq!(frame(Some(" "), GridKind::Measured), (true, 0, 0));
        assert_eq!(frame(Some("EPSG:2056"), GridKind::Measured), (false, 1, 0), "said on its own, not as a value out of range");
        assert_eq!(frame(None, GridKind::Graticule), (false, 1, 0));
    }

    /// A chart's maximum not above 0, which only a hand-edited file holds and the chart can't
    /// draw to, is let go on read (the chart on its bars' own scale) and counted as a value out of
    /// range, so the project says so (#2487 part 4); one above 0 is kept.
    #[test]
    fn a_charts_maximum_not_above_zero_is_repaired_and_counted() {
        for (max, kept, adjusted) in [(-5.0, None, 1), (0.0, None, 1), (40.0, Some(40.0), 0)] {
            let mut l: Layout = serde_json::from_value(serde_json::json!({
                "name": "L", "width": 120, "height": 90, "elements": [
                    {"id": 1, "name": "Frame", "x": 10, "y": 10, "w": 60, "h": 70,
                     "kind": {"MapFrame": {"map": "Map", "center": {"x": 0, "y": 0}, "scale": 1000000}}},
                    {"id": 2, "name": "Chart", "x": 75, "y": 10, "w": 35, "h": 40,
                     "kind": {"Chart": {"frame": 1, "layer": "Places", "field": "people", "max": max}}}
                ]
            }))
            .unwrap();
            let r = l.clamp();
            let ElementKind::Chart { max: m, .. } = &l.elements[1].kind else { panic!("a chart") };
            assert_eq!((*m, r.elements_adjusted), (kept, adjusted), "{max}");
        }
    }

    /// A bar outline's keys left out of a hand-edited file take those it is first given, as a
    /// background's do, rather than leaving the layout unreadable (#2487 part 4b).
    #[test]
    fn a_charts_bar_outline_takes_its_defaults_for_keys_left_out() {
        let read = |outline: serde_json::Value| {
            let kind: ElementKind = serde_json::from_value(serde_json::json!(
                {"Chart": {"frame": 1, "layer": "Places", "field": "people", "bar_outline": outline}}
            ))
            .unwrap();
            let ElementKind::Chart { bar_outline, .. } = kind else { panic!("a chart") };
            bar_outline
        };
        let red = Rgba::rgb(0xD0, 0x10, 0x10);
        assert_eq!(read(serde_json::json!({"width": 2.0})), Some(ChartOutline { width: 2.0, ..CHART_OUTLINE }));
        assert_eq!(read(serde_json::json!({"color": [208, 16, 16, 255]})), Some(ChartOutline { color: red, ..CHART_OUTLINE }));
        assert_eq!(read(serde_json::json!({})), Some(CHART_OUTLINE));
    }

    /// A bar outline's width out of range, which only a hand-edited file holds, is repaired on
    /// read and counted as a value out of range, so the project says so (#2487 part 4b): past 5
    /// points it is held to 5, below 0 the outline is let go. A width of 0 draws none, as the
    /// commands take it, so it is let go without a word; one in range is kept.
    #[test]
    fn a_charts_bar_outline_out_of_range_is_repaired_and_counted() {
        let ink = Rgba::rgb(0x22, 0x22, 0x22);
        for (width, kept, adjusted) in [(8.0, Some(5.0), 1), (-1.0, None, 1), (0.0, None, 0), (2.0, Some(2.0), 0)] {
            let mut l: Layout = serde_json::from_value(serde_json::json!({
                "name": "L", "width": 120, "height": 90, "elements": [
                    {"id": 1, "name": "Frame", "x": 10, "y": 10, "w": 60, "h": 70,
                     "kind": {"MapFrame": {"map": "Map", "center": {"x": 0, "y": 0}, "scale": 1000000}}},
                    {"id": 2, "name": "Chart", "x": 75, "y": 10, "w": 35, "h": 40,
                     "kind": {"Chart": {"frame": 1, "layer": "Places", "field": "people",
                                        "bar_outline": {"color": [34, 34, 34, 255], "width": width}}}}
                ]
            }))
            .unwrap();
            let r = l.clamp();
            let ElementKind::Chart { bar_outline, .. } = &l.elements[1].kind else { panic!("a chart") };
            let expected = kept.map(|w| ChartOutline { color: ink, width: w });
            assert_eq!((*bar_outline, r.elements_adjusted), (expected, adjusted), "{width}");
        }
    }

    /// A page colour with alpha, which only a hand-edited file holds, is read as it looks on white
    /// paper (a page has nothing else under it), so a clear one is the white page, and every one
    /// is counted as a page repaired (#2855). An opaque one is kept, and opaque white is the page's
    /// own, so it is let go without a word.
    #[test]
    fn a_translucent_page_colour_is_made_opaque_and_counted() {
        for (paper, kept, adjusted) in [
            (serde_json::json!([11, 27, 43, 128]), Some(Rgba::rgb(133, 141, 149)), 1),
            (serde_json::json!([11, 27, 43, 0]), None, 1),
            (serde_json::json!([11, 27, 43, 255]), Some(Rgba::rgb(11, 27, 43)), 0),
            (serde_json::json!([255, 255, 255, 255]), None, 0),
            (serde_json::json!([255, 255, 255, 128]), None, 1),
        ] {
            let mut l: Layout =
                serde_json::from_value(serde_json::json!({"name": "L", "width": 120, "height": 90, "elements": [], "paper": paper}))
                    .unwrap();
            let r = l.clamp();
            assert_eq!((l.paper, r.page_adjusted), (kept, adjusted), "{paper}");
        }
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

    /// Lines and the outlines of rectangles and ellipses keep a dash pattern through a save
    /// (#1763); solid, nothing is written, so a layout saved before is written back byte for
    /// byte; a pattern read from a file is brought into range as a line layer's is.
    #[test]
    fn dashes_round_trip_and_old_layouts_keep_their_bytes() {
        let black = Rgba::rgb(0, 0, 0);
        let mut l = Layout::new("L", 100.0, 100.0);
        let line = l.add(
            "Line",
            (10.0, 10.0, 60.0, 6.0),
            ElementKind::Line { color: black, width: 1.0, dir: LineDir::Horizontal, head: LineHead::End, dash: None },
        );
        let rect = l.add(
            "Box",
            (10.0, 30.0, 60.0, 30.0),
            ElementKind::Rectangle { fill: black, outline: black, width: 1.0, radius: 0.0, dash: None },
        );
        let oval = l.add("Oval", (10.0, 70.0, 30.0, 20.0), ElementKind::Ellipse { fill: black, outline: black, width: 1.0, dash: None });
        let old = serde_json::to_string(&l).unwrap();
        assert!(!old.contains("dash"), "solid writes no pattern");
        assert_eq!(serde_json::to_string(&serde_json::from_str::<Layout>(&old).unwrap()).unwrap(), old, "written back byte for byte");
        let dash_of = |l: &Layout, id: u32| match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::Line { dash, .. } | ElementKind::Rectangle { dash, .. } | ElementKind::Ellipse { dash, .. }) => {
                dash.map(|d| d.steps().to_vec())
            }
            _ => panic!("a line or a shape"),
        };
        let pattern = Dash::new(&[4.0, 2.0, 1.0, 2.0]);
        for id in [line, rect, oval] {
            if let Some(ElementKind::Line { dash, .. } | ElementKind::Rectangle { dash, .. } | ElementKind::Ellipse { dash, .. }) =
                l.element_mut(id).map(|e| &mut e.kind)
            {
                *dash = pattern;
            }
        }
        let saved = serde_json::to_string(&l).unwrap();
        assert!(saved.contains(r#""dash":[4.0,2.0,1.0,2.0]"#), "{saved}");
        let back: Layout = serde_json::from_str(&saved).unwrap();
        for id in [line, rect, oval] {
            assert_eq!(dash_of(&back, id), Some(vec![4.0, 2.0, 1.0, 2.0]));
        }
        // From a file, as a line layer's pattern: steps clamped, all-zero read as solid.
        let file = saved.replacen("[4.0,2.0,1.0,2.0]", "[4,1e9,-2]", 1).replacen("[4.0,2.0,1.0,2.0]", "[0,0]", 1);
        let read: Layout = serde_json::from_str(&file).unwrap();
        assert_eq!(dash_of(&read, line), Some(vec![4.0, MAX_DASH_STEP, 0.0]));
        assert_eq!(dash_of(&read, rect), None);
        assert_eq!(dash_of(&read, oval), Some(vec![4.0, 2.0, 1.0, 2.0]));
    }

    /// A text turned 90° or -90° (#1763) keeps its turn through a save; one saved before reads
    /// upright and is written back byte for byte; a turn read from a file is kept within -180 to
    /// 180.
    #[test]
    fn text_rotation_round_trips_and_old_layouts_keep_their_bytes() {
        let turn_of = |l: &Layout, id: u32| l.element(id).map(Element::rotation);
        let mut l = Layout::new("L", 100.0, 100.0);
        let upright = l.add("Title", (10.0, 10.0, 60.0, 10.0), ElementKind::text("Brasil", 12.0, Rgba::rgb(0, 0, 0), false));
        let old = serde_json::to_string(&l).unwrap();
        assert!(!old.contains("rotation"), "an upright text writes no turn");
        let back: Layout = serde_json::from_str(&old).unwrap();
        assert_eq!(turn_of(&back, upright), Some(0.0));
        assert_eq!(serde_json::to_string(&back).unwrap(), old, "written back byte for byte");
        for turn in [90.0, -90.0] {
            let mut l = l.clone();
            if let Some(ElementKind::Text { rotation, .. }) = l.element_mut(upright).map(|e| &mut e.kind) {
                *rotation = turn;
            }
            let back: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
            assert_eq!(turn_of(&back, upright), Some(turn));
        }
        // Read from a file, a turn is kept within -180..=180.
        let file = old.replace(r#""bold":false"#, r#""bold":false,"rotation":450"#);
        let mut read: Layout = serde_json::from_str(&file).unwrap();
        read.clamp();
        assert_eq!(turn_of(&read, upright), Some(90.0));
        assert_eq!(
            [rotation(270.0), rotation(-270.0), rotation(180.0), rotation(-180.0), rotation(f64::NAN)],
            [-90.0, 90.0, 180.0, 180.0, 0.0]
        );
    }

    /// A turned text is hit, and takes up the page, where it is drawn: its box turned about its
    /// centre (#1763).
    #[test]
    fn a_turned_text_is_hit_on_its_turned_box() {
        let mut l = Layout::new("L", 100.0, 100.0);
        let id = l.add("Source", (20.0, 45.0, 60.0, 10.0), ElementKind::text("Fonte: IBGE", 8.0, Rgba::rgb(0, 0, 0), false));
        // Upright: 60 wide, 10 high, centred on (50, 50).
        assert_eq!((l.hit(25.0, 50.0), l.hit(50.0, 25.0)), (Some(id), None));
        if let Some(ElementKind::Text { rotation, .. }) = l.element_mut(id).map(|e| &mut e.kind) {
            *rotation = 90.0;
        }
        // Turned: 10 wide, 60 high, about the same centre.
        assert_eq!((l.hit(25.0, 50.0), l.hit(50.0, 25.0)), (None, Some(id)));
        let (x, y, w, h) = l.element(id).unwrap().bounds();
        assert!((x - 45.0).abs() < 1e-9 && (y - 20.0).abs() < 1e-9 && (w - 10.0).abs() < 1e-9 && (h - 60.0).abs() < 1e-9);
        // The element keeps its unturned box.
        let e = l.element(id).unwrap();
        assert_eq!((e.x, e.y, e.w, e.h), (20.0, 45.0, 60.0, 10.0));
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

    /// A background (#2257) is left out of a saved element that has none, so older layouts read and
    /// write as before; read back, it keeps its fill, outline, corners and padding, and a hand-edited
    /// one is brought into range, a map frame's dropped.
    #[test]
    fn element_backgrounds_are_saved_read_back_and_repaired() {
        let mut l = Layout::new("L", 100.0, 100.0);
        let t = l.add("T", (40.0, 40.0, 20.0, 10.0), ElementKind::text("t", 12.0, Rgba::rgb(0, 0, 0), false));
        let saved = serde_json::to_value(&l).unwrap();
        assert!(saved["elements"][0].get("background").is_none(), "none, not written: {saved}");
        let b = Background { fill: Rgba::new(255, 255, 255, 204), outline: Rgba::rgb(0, 0, 0), width: 0.5, radius: 1.0, padding: 1.5 };
        l.element_mut(t).unwrap().background = Some(b);
        let back: Layout = serde_json::from_value(serde_json::to_value(&l).unwrap()).unwrap();
        assert_eq!(back.element(t).unwrap().background, Some(b));
        // Only a fill given: no outline, square corners, no padding.
        let fill: Background = serde_json::from_str(r#"{"fill":[1,2,3,255]}"#).unwrap();
        assert_eq!(fill, Background::filled(Rgba::rgb(1, 2, 3)));
        // An empty one is white, as `layout.update` makes `{"background": {}}`, not a file that fails to open.
        let empty: Background = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Background::filled(Rgba::rgb(255, 255, 255)));

        let text = r#"{"name":"x","width":100,"height":100,"elements":[
            {"id":1,"name":"f","x":0,"y":0,"w":50,"h":50,"background":{"fill":[255,0,0,255]},"kind":{"MapFrame":{"map":"M","center":{"x":0,"y":0},"scale":1000000}}},
            {"id":2,"name":"t","x":0,"y":0,"w":10,"h":10,"background":{"fill":[255,0,0,255],"width":1e9,"radius":-3,"padding":1e300},"kind":{"Text":{"text":"t","size":12,"color":[0,0,0,255]}}},
            {"id":3,"name":"u","x":0,"y":0,"w":10,"h":10,"background":{"fill":[255,0,0,255],"padding":2},"kind":{"Text":{"text":"u","size":12,"color":[0,0,0,255]}}}]}"#;
        let mut l: Layout = serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}"));
        let repairs = l.clamp();
        assert_eq!(repairs.elements_adjusted, 2, "the frame's background and the text's numbers: {repairs:?}");
        assert_eq!(l.element(1).unwrap().background, None, "a map frame has none");
        let b = l.element(2).unwrap().background.unwrap();
        assert_eq!((b.width, b.radius, b.padding), (MAX_BACKGROUND_WIDTH, 0.0, MAX_BACKGROUND_PADDING));
        assert_eq!(l.element(3).unwrap().background.unwrap().padding, 2.0, "in range, kept");
        assert!(l.clamp().is_empty(), "a repaired layout needs nothing more");
    }

    /// A click on an element's background picks it, padding included; without one, its box only.
    #[test]
    fn a_background_is_part_of_what_a_click_picks() {
        let mut l = Layout::new("L", 100.0, 100.0);
        let t = l.add("T", (40.0, 40.0, 20.0, 10.0), ElementKind::text("t", 12.0, Rgba::rgb(0, 0, 0), false));
        assert_eq!(l.hit(38.0, 45.0), None);
        l.element_mut(t).unwrap().background = Some(Background { padding: 3.0, ..Background::filled(Rgba::rgb(255, 255, 255)) });
        assert_eq!(l.hit(38.0, 45.0), Some(t));
        assert_eq!(l.hit(36.0, 45.0), None, "past the padding");
        assert_eq!(l.element(t).unwrap().background_box(), (37.0, 37.0, 26.0, 16.0));
    }

    /// A legend's, colorbar's, table's or chart's ink (#2257): none in a layout saved before it,
    /// not written when none, and read back when set. Other kinds have none to set.
    #[test]
    fn inks_are_saved_and_read_back() {
        let text = r#"{"name":"x","width":100,"height":100,"elements":[
            {"id":1,"name":"f","x":0,"y":0,"w":50,"h":50,"kind":{"MapFrame":{"map":"M","center":{"x":0,"y":0},"scale":1000000}}},
            {"id":2,"name":"l","x":0,"y":0,"w":10,"h":10,"kind":{"Legend":{"frame":1,"title":"L"}}},
            {"id":3,"name":"c","x":0,"y":0,"w":10,"h":10,"kind":{"Colorbar":{"frame":1}}},
            {"id":4,"name":"t","x":0,"y":0,"w":10,"h":10,"kind":{"Table":{"frame":1,"layer":"A"}}},
            {"id":5,"name":"g","x":0,"y":0,"w":10,"h":10,"kind":{"Chart":{"frame":1,"layer":"A","field":"n"}}}]}"#;
        let mut l: Layout = serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}"));
        assert!(l.elements[1..].iter().all(|e| e.kind.takes_ink() && e.kind.ink().is_none()), "the default ink in an older layout");
        assert!(!l.elements[0].kind.takes_ink(), "a map frame has no ink");
        let saved = serde_json::to_string(&l).unwrap();
        assert!(!saved.contains("\"color\""), "none is not written: {saved}");
        let white = Rgba::new(255, 255, 255, 230);
        for e in &mut l.elements[1..] {
            *e.kind.ink_mut().unwrap() = Some(white);
        }
        let back: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert!(back.elements[1..].iter().all(|e| e.kind.ink() == Some(white)), "{back:?}");
        assert_eq!(back, l);
        let mut text = ElementKind::text("t", 12.0, Rgba::rgb(0, 0, 0), false);
        assert!(!text.takes_ink() && text.ink_mut().is_none(), "a text's colour is its own option");
    }

    /// A text's block alignment, line spacing and case: left out of the file at their defaults, so a
    /// text saved before saves byte for byte; written and read back when set; and a hand-edited value
    /// out of range, of the wrong kind or not a name loads, brought into range and counted.
    #[test]
    fn text_block_settings_save_only_when_set_and_load_into_range() {
        use serde_json::json;
        for plain in [
            r#"{"Text":{"text":"a","size":10.0,"color":[0,0,0,255],"bold":false}}"#,
            r#"{"Text":{"text":"a\nb","size":12.0,"color":[31,41,55,255],"bold":true,"italic":true,"align":"Center","letter_spacing":2.0,"font":"Serif","font_name":"Georgia","rotation":90.0,"wrap":true}}"#,
        ] {
            let k: ElementKind = serde_json::from_str(plain).unwrap();
            assert_eq!(serde_json::to_string(&k).unwrap(), plain);
            assert!(!k.text_uses_block());
        }
        let mut set = ElementKind::text("a", 10.0, Rgba::rgb(0, 0, 0), false);
        if let ElementKind::Text { valign, line_spacing, case, .. } = &mut set {
            (*valign, *line_spacing, *case) = (TextVAlign::Bottom, 1.5, TextCase::Upper);
        }
        let s = serde_json::to_string(&set).unwrap();
        assert!(s.ends_with(r#""bold":false,"valign":"bottom","line_spacing":1.5,"case":"upper"}}"#), "{s}");
        assert_eq!(serde_json::from_str::<ElementKind>(&s).unwrap(), set);
        assert!(set.text_uses_block());
        // Unclamped, a value that is not a name or not a finite number saves as the default does:
        // nothing, never "top" or a null spacing.
        let plain = ElementKind::text("a", 10.0, Rgba::rgb(0, 0, 0), false);
        let plain_json = serde_json::to_string(&plain).unwrap();
        for (v, ls, c) in [
            (TextVAlign::Invalid, f64::NAN, TextCase::Invalid),
            (TextVAlign::Invalid, LINE_SPACING, TextCase::None),
            (TextVAlign::Top, f64::NAN, TextCase::None),
            (TextVAlign::Top, f64::INFINITY, TextCase::None),
            (TextVAlign::Top, f64::NEG_INFINITY, TextCase::Invalid),
        ] {
            let mut odd = plain.clone();
            if let ElementKind::Text { valign, line_spacing, case, .. } = &mut odd {
                (*valign, *line_spacing, *case) = (v, ls, c);
            }
            assert_eq!(serde_json::to_string(&odd).unwrap(), plain_json, "{v:?} {ls} {c:?}");
            assert!(!odd.text_uses_block(), "{v:?} {ls} {c:?}");
        }
        // One reader for the range: not finite is the usual spacing, else the nearer limit.
        for (ls, want) in [(f64::NAN, 1.0), (f64::INFINITY, 1.0), (f64::NEG_INFINITY, 1.0), (9.0, 3.0), (0.1, 0.5), (1.5, 1.5), (3.0, 3.0)]
        {
            assert_eq!(line_spacing_in_range(ls), want, "{ls}");
        }
        for (name, v) in [
            (" MIDDLE ", TextVAlign::Middle),
            ("centre", TextVAlign::Middle),
            ("Center", TextVAlign::Middle),
            ("Top", TextVAlign::Top),
            ("bottom", TextVAlign::Bottom),
        ] {
            assert_eq!(TextVAlign::from_name(name), Some(v), "{name}");
        }
        assert_eq!((TextVAlign::from_name("baseline"), TextCase::from_name("title")), (None, None));
        assert_eq!(TextCase::from_name(" Upper "), Some(TextCase::Upper));
        assert_eq!(TextCase::Upper.apply("Straße {9}".into()), "STRASSE {9}");
        assert_eq!(TextCase::Lower.apply("ÉTÉ".into()), "été");
        assert_eq!(TextCase::None.apply("As Typed".into()), "As Typed");
        // A hand-edited layout.
        let extras = [
            json!({"line_spacing": 9}),                                      // 2: to 3, said
            json!({"line_spacing": 0.1}),                                    // 3: to 0.5, said
            json!({"line_spacing": "1.5"}),                                  // 4: no number: 1, said
            json!({"line_spacing": [2]}),                                    // 5: 1, said
            json!({"valign": "sideways"}),                                   // 6: top, said
            json!({"valign": 7}),                                            // 7: top, said
            json!({"case": "title"}),                                        // 8: none, said
            json!({"case": {"x": 1}}),                                       // 9: none, said
            json!({"valign": "Middle", "case": "UPPER", "line_spacing": 2}), // 10: kept, not said
            json!({"valign": null, "case": null, "line_spacing": null}),     // 11: defaults, not said
            json!({"valign": "centre", "line_spacing": 3.0}),                // 12: kept, not said
        ];
        let elements: Vec<_> = extras
            .iter()
            .enumerate()
            .map(|(i, extra)| {
                let mut t = json!({"text": "a", "size": 10.0, "color": [0, 0, 0, 255], "bold": false});
                t.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
                json!({"id": i + 2, "name": "t", "x": 0, "y": 0, "w": 10, "h": 10, "kind": {"Text": t}})
            })
            .collect();
        let mut l: Layout = serde_json::from_value(json!({"name": "x", "width": 100, "height": 100, "elements": elements})).unwrap();
        let repairs = l.clamp();
        let block = |id| match &l.element(id).unwrap().kind {
            ElementKind::Text { valign, line_spacing, case, .. } => (*valign, *line_spacing, *case),
            k => panic!("{k:?}"),
        };
        use {TextCase as C, TextVAlign as V};
        let want = [
            (2, (V::Top, 3.0, C::None)),
            (3, (V::Top, 0.5, C::None)),
            (4, (V::Top, 1.0, C::None)),
            (5, (V::Top, 1.0, C::None)),
            (6, (V::Top, 1.0, C::None)),
            (7, (V::Top, 1.0, C::None)),
            (8, (V::Top, 1.0, C::None)),
            (9, (V::Top, 1.0, C::None)),
            (10, (V::Middle, 2.0, C::Upper)),
            (11, (V::Top, 1.0, C::None)),
            (12, (V::Middle, 3.0, C::None)),
        ];
        for (id, b) in want {
            assert_eq!(block(id), b, "{id}");
        }
        assert_eq!((repairs.elements_adjusted, repairs.elements_cut), (8, 0));
        // Repaired to the defaults, they save nothing; no marker is ever written.
        let saved = serde_json::to_string(&l).unwrap();
        assert!(!saved.contains("sideways") && !saved.contains("title") && saved.matches("\"valign\"").count() == 2, "{saved}");
    }

    #[test]
    fn scale_bars_without_details_save_as_before() {
        // As main writes a plain bar: colour and fill always, nothing new.
        let plain = r#"{"ScaleBar":{"frame":2,"units":"Auto","divisions":4,"style":"Alternating","color":[34,34,34,255],"fill":[255,255,255,255]}}"#;
        let bar: ElementKind = serde_json::from_str(plain).unwrap();
        assert_eq!(serde_json::to_string(&bar).unwrap(), plain);
        assert_eq!(bar, ElementKind::scale_bar(2));
        assert!(!bar.scale_bar_uses_details());
        let ElementKind::ScaleBar {
            subdivisions,
            divisions_before_zero,
            label_position,
            unit_label,
            unit_label_position,
            decimals,
            text_size,
            fit,
            ..
        } = &bar
        else {
            panic!("a scale bar")
        };
        assert_eq!((*subdivisions, *divisions_before_zero, *decimals, *text_size), (0, 0, None, SCALE_BAR_TEXT_PT));
        assert_eq!(
            (*label_position, unit_label.clone(), *unit_label_position, *fit),
            (ScaleLabelPosition::Above, None, UnitLabelPosition::AfterLabels, ScaleBarFit::AdjustDivisionValue)
        );
        // Every setting, saved under its own key and read back equal.
        let mut all = bar.clone();
        if let ElementKind::ScaleBar {
            subdivisions,
            divisions_before_zero,
            label_position,
            unit_label,
            unit_label_position,
            decimals,
            text_size,
            fit,
            ..
        } = &mut all
        {
            (*subdivisions, *divisions_before_zero, *label_position) = (4, 1, ScaleLabelPosition::Below);
            (*unit_label, *unit_label_position) = (Some("Kilometres".into()), UnitLabelPosition::AfterBar);
            (*decimals, *text_size, *fit) = (Some(2), 10.5, ScaleBarFit::AdjustWidth);
        }
        assert!(all.scale_bar_uses_details());
        // Its text size alone is no detail: the text size capability covers it.
        let mut sized = bar.clone();
        if let ElementKind::ScaleBar { text_size, .. } = &mut sized {
            *text_size = 16.0;
        }
        assert!(!sized.scale_bar_uses_details() && sized.has_custom_text_size());
        let s = serde_json::to_string(&all).unwrap();
        // The middle fit saves as the camelCase of its variant.
        let mut middle = all.clone();
        if let ElementKind::ScaleBar { fit, .. } = &mut middle {
            *fit = ScaleBarFit::AdjustDivision;
        }
        assert!(serde_json::to_string(&middle).unwrap().contains(r#""fit":"adjustDivision""#));
        assert!(s.starts_with(r#"{"ScaleBar":{"frame":2,"units":"Auto","divisions":4,"style":"Alternating","color":[34,34,34,255],"fill":[255,255,255,255],"#), "{s}");
        for part in [
            r#""subdivisions":4"#,
            r#""divisions_before_zero":1"#,
            r#""label_position":"below""#,
            r#""unit_label":"Kilometres""#,
            r#""unit_label_position":"afterBar""#,
            r#""decimals":2"#,
            r#""text_size":10.5"#,
            r#""fit":"adjustWidth""#,
        ] {
            assert!(s.contains(part), "{part} in {s}");
        }
        assert_eq!(serde_json::from_str::<ElementKind>(&s).unwrap(), all);
        // An empty unit label is a setting too: it prints none.
        let mut blank = bar.clone();
        if let ElementKind::ScaleBar { unit_label, .. } = &mut blank {
            *unit_label = Some(String::new());
        }
        assert!(blank.scale_bar_uses_details());
        assert!(serde_json::to_string(&blank).unwrap().contains(r#""unit_label":"""#));
        // Fixed at no decimals is a setting too, apart from auto: its thousands are set apart.
        let mut whole = bar.clone();
        if let ElementKind::ScaleBar { decimals, .. } = &mut whole {
            *decimals = Some(0);
        }
        assert!(whole.scale_bar_uses_details());
        assert!(serde_json::to_string(&whole).unwrap().contains(r#""decimals":0"#));
        // A loaded layout's out-of-range values are brought into range, and said.
        let text = format!(
            r#"{{"name":"x","width":200,"height":100,"elements":[
            {{"id":1,"name":"a","x":0,"y":0,"w":10,"h":10,"kind":{{"MapFrame":{{"map":"m","center":{{"x":0,"y":0}},"scale":1000}}}}}},
            {{"id":2,"name":"b","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"subdivisions":99,"divisions_before_zero":9,"decimals":9,"text_size":1e9,"unit_label":"{}"}}}}}},
            {{"id":3,"name":"c","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"text_size":-4,"unit_label":"a\r\nb\rc\nd"}}}}}},
            {{"id":4,"name":"d","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"subdivisions":1}}}}}},
            {{"id":5,"name":"e","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":300}}}}}},
            {{"id":6,"name":"f","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":-1}}}}}},
            {{"id":7,"name":"g","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":2.5}}}}}},
            {{"id":8,"name":"h","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":" Auto "}}}}}},
            {{"id":9,"name":"i","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":"two"}}}}}},
            {{"id":10,"name":"j","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":[2]}}}}}},
            {{"id":11,"name":"k","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"divisions":10,"divisions_before_zero":2}}}}}}]}}"#,
            "x".repeat(150)
        );
        let mut l: Layout = serde_json::from_str(&text).unwrap();
        let repairs = l.clamp();
        let got = |id| match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::ScaleBar { subdivisions, divisions_before_zero, decimals, text_size, unit_label, .. }) => {
                (*subdivisions, *divisions_before_zero, *decimals, *text_size, unit_label.clone().unwrap_or_default())
            }
            _ => (0, 0, None, 0.0, String::new()),
        };
        assert_eq!(
            got(2),
            (
                MAX_SCALE_SUBDIVISIONS,
                MAX_SCALE_BEFORE_ZERO,
                Some(MAX_SCALE_DECIMALS as u8),
                *LAYOUT_TEXT_SIZE.end(),
                "x".repeat(MAX_UNIT_LABEL_CHARS)
            )
        );
        assert_eq!(got(3), (0, 0, None, *LAYOUT_TEXT_SIZE.start(), "a b c d".to_string()));
        // One subdivision is the division whole: it loads as none, quietly, and asks for no details.
        assert_eq!(got(4), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()));
        assert!(l.element(4).is_some_and(|e| !e.kind.scale_bar_uses_details()));
        // A count past 255 loads too (it would not fit a byte) and ends at the most, said.
        assert_eq!(got(5), (0, 0, Some(MAX_SCALE_DECIMALS as u8), SCALE_BAR_TEXT_PT, String::new()));
        // A negative or fractional count is no count: it goes back to automatic, and is said too.
        assert_eq!(got(6), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()));
        assert_eq!(got(7), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()));
        // Any JSON kind loads: the word "auto" is automatic and no repair; other words and lists
        // are no count, so they go back to automatic and are said too.
        for id in [8, 9, 10] {
            assert_eq!(got(id), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()), "{id}");
        }
        // Ten divisions with two before zero are more than the ten a bar draws: the ones before zero
        // give way, and it is said.
        assert_eq!(got(11), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()));
        assert!(matches!(l.element(11).map(|e| &e.kind), Some(ElementKind::ScaleBar { divisions: 10, .. })));
        assert_eq!((repairs.elements_adjusted, repairs.elements_cut), (8, 1));
        for v in ScaleLabelPosition::ALL {
            assert_eq!(ScaleLabelPosition::from_name(&format!(" {} ", v.code().to_uppercase())), Some(v));
        }
        for v in UnitLabelPosition::ALL {
            assert_eq!(UnitLabelPosition::from_name(v.code()), Some(v));
        }
        for v in ScaleBarFit::ALL {
            assert_eq!(ScaleBarFit::from_name(v.code()), Some(v));
        }
        assert_eq!(ScaleBarFit::from_name("manual"), None);
    }
}
