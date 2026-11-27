//! A map frame's grid: its kinds, steps, styles and coordinate labels.

use super::*;

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

    pub(super) fn is_graticule(&self) -> bool {
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

pub(super) fn tick_length() -> f64 {
    2.0
}

pub(super) fn grid_width() -> f64 {
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
pub(super) fn decimal(a: f64, step: f64) -> String {
    let places = (0..4usize).find(|&n| {
        let k = step * 10f64.powi(n as i32);
        (k - k.round()).abs() < 1e-6
    });
    format!("{a:.prec$}", prec = places.unwrap_or(4))
}

/// `a` degrees as `12°30′15″`, leaving out the minutes and seconds `step` doesn't need.
pub(super) fn dms(a: f64, step: f64) -> String {
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
