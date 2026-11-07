//! WGS 84 Universal Transverse Mercator (UTM) and Military Grid Reference System (MGRS)
//! coordinates: the pointer readouts and the grid references typed into the search box.
//!
//! The projection is the Krüger series to sixth order in the third flattening n, as given by
//! Karney, C. F. F. (2011), "Transverse Mercator with an accuracy of a few nanometers",
//! Journal of Geodesy 85(8), 475–485, doi:10.1007/s00190-011-0445-3: the conformal latitude of
//! eqs. (7)–(9), the series of eqs. (11) and (35) forward and (15) and (36) back, and Newton's
//! method of eqs. (19)–(21) for the latitude from the conformal one. Within the UTM zones it is
//! accurate to well under a millimetre.
//!
//! The grid follows NGA's definition: NGA.STND.0037 (2014), "Universal Grids and Grid Reference
//! Systems" (formerly DMA TM 8358.1): 6° zones with the exceptions of southern Norway (32V runs
//! from 3° E to 12° E, 31V only to 3° E) and Svalbard (31X, 33X, 35X and 37X are 9° or 12° wide;
//! 32X, 34X and 36X don't exist), latitude bands C to X of 8° (X runs 12°, from 72° N to 84° N),
//! and the 100 km squares of the AA lettering scheme used on WGS 84: column letters A–H, J–R and
//! S–Z by zone in turn, row letters A–V (no I or O) from the equator, starting at F in even zones.
//! A reference truncates its easting and northing to its precision, it doesn't round them, and
//! writes the zone with two digits (`04QFJ`), so references line up and sort; one digit is read
//! as well.
//!
//! Only the UTM latitudes, 80° S to 84° N, are covered: the polar areas use the Universal Polar
//! Stereographic (UPS) grid, which isn't implemented. There the functions return `None`, and a
//! typed polar reference is refused with [`GridError::Polar`].

use std::fmt;
use std::sync::LazyLock;

use crate::Coord;

/// The southern limit of the UTM zones (degrees).
pub const UTM_MIN_LAT: f64 = -80.0;
/// The northern limit of the UTM zones (degrees); 84° N itself is UPS.
pub const UTM_MAX_LAT: f64 = 84.0;

/// WGS 84 semi-major axis (metres) and flattening.
const WGS84_A: f64 = 6_378_137.0;
const WGS84_F: f64 = 1.0 / 298.257_223_563;
/// UTM's scale on the central meridian, false easting and the southern false northing.
const K0: f64 = 0.9996;
const FALSE_EASTING: f64 = 500_000.0;
const FALSE_NORTHING_SOUTH: f64 = 10_000_000.0;
/// MGRS 100 km square side (metres) and the 2,000 km cycle of its row letters.
const SQUARE: f64 = 100_000.0;
const ROW_CYCLE: f64 = 2_000_000.0;
/// How far (degrees) a typed reference may lie outside its zone's longitudes, beyond the size of
/// the cell it names: references near a zone edge are often written in the neighbouring zone, and
/// the position they give is still the right one. A band's latitudes get no such allowance: the
/// band is the latitude's own letter, so a reference in the wrong band is refused.
const EDGE_SLOP_DEG: f64 = 0.5;
/// How far (degrees) a place, or the nearest part of a cell, may lie outside its band, or past the
/// UTM limits, for rounding: about a metre, the precision a UTM position is written to.
const ROUNDING_DEG: f64 = 1.0 / M_PER_DEG;
/// Metres per degree of latitude, roughly, for turning a cell's size into degrees.
const M_PER_DEG: f64 = 111_000.0;

/// Latitude band letters from 80° S, 8° each (X runs to 84° N).
const BANDS: &[u8; 20] = b"CDEFGHJKLMNPQRSTUVWX";
/// MGRS column letters of the zones 1, 4, 7…; 2, 5, 8…; and 3, 6, 9…
const COLUMNS: [&[u8; 8]; 3] = [b"ABCDEFGH", b"JKLMNPQR", b"STUVWXYZ"];
/// MGRS row letters, repeating every 2,000 km of northing.
const ROWS: &[u8; 20] = b"ABCDEFGHJKLMNPQRSTUV";
/// Even zones start their rows five letters on, at F (AA scheme).
const EVEN_ZONE_ROW_OFFSET: i64 = 5;

/// The ellipsoid's constants for the series (Karney 2011, eqs. 14, 35, 36).
struct Series {
    /// First eccentricity.
    e: f64,
    /// Rectifying radius A: a / (1 + n) (1 + n²/4 + n⁴/64 + n⁶/256).
    rect: f64,
    /// Forward coefficients α₁…α₆.
    alpha: [f64; 6],
    /// Inverse coefficients β₁…β₆.
    beta: [f64; 6],
}

static SERIES: LazyLock<Series> = LazyLock::new(|| {
    let f = WGS84_F;
    let n = f / (2.0 - f);
    let (n2, n3, n4, n5, n6) = (n * n, n.powi(3), n.powi(4), n.powi(5), n.powi(6));
    Series {
        e: (f * (2.0 - f)).sqrt(),
        rect: WGS84_A / (1.0 + n) * (1.0 + n2 / 4.0 + n4 / 64.0 + n6 / 256.0),
        alpha: [
            n / 2.0 - 2.0 / 3.0 * n2 + 5.0 / 16.0 * n3 + 41.0 / 180.0 * n4 - 127.0 / 288.0 * n5 + 7891.0 / 37800.0 * n6,
            13.0 / 48.0 * n2 - 3.0 / 5.0 * n3 + 557.0 / 1440.0 * n4 + 281.0 / 630.0 * n5 - 1_983_433.0 / 1_935_360.0 * n6,
            61.0 / 240.0 * n3 - 103.0 / 140.0 * n4 + 15061.0 / 26880.0 * n5 + 167_603.0 / 181_440.0 * n6,
            49561.0 / 161_280.0 * n4 - 179.0 / 168.0 * n5 + 6_601_661.0 / 7_257_600.0 * n6,
            34729.0 / 80640.0 * n5 - 3_418_889.0 / 1_995_840.0 * n6,
            212_378_941.0 / 319_334_400.0 * n6,
        ],
        beta: [
            n / 2.0 - 2.0 / 3.0 * n2 + 37.0 / 96.0 * n3 - 1.0 / 360.0 * n4 - 81.0 / 512.0 * n5 + 96199.0 / 604_800.0 * n6,
            1.0 / 48.0 * n2 + 1.0 / 15.0 * n3 - 437.0 / 1440.0 * n4 + 46.0 / 105.0 * n5 - 1_118_711.0 / 3_870_720.0 * n6,
            17.0 / 480.0 * n3 - 37.0 / 840.0 * n4 - 209.0 / 4480.0 * n5 + 5569.0 / 90720.0 * n6,
            4397.0 / 161_280.0 * n4 - 11.0 / 504.0 * n5 - 830_251.0 / 7_257_600.0 * n6,
            4583.0 / 161_280.0 * n5 - 108_847.0 / 3_991_680.0 * n6,
            20_648_693.0 / 638_668_800.0 * n6,
        ],
    }
});

/// tan of the conformal latitude from tan of the latitude, τ′(τ) (Karney 2011, eqs. 7–9).
fn conformal_tan(tau: f64) -> f64 {
    let e = SERIES.e;
    let sigma = (e * (e * tau / tau.hypot(1.0)).atanh()).sinh();
    tau * sigma.hypot(1.0) - sigma * tau.hypot(1.0)
}

/// tan of the latitude from tan of the conformal latitude, by Newton's method (Karney 2011,
/// eqs. 19–21).
fn geographic_tan(taup: f64) -> Option<f64> {
    let e2m = 1.0 - SERIES.e * SERIES.e;
    let mut tau = taup / e2m;
    for _ in 0..10 {
        let tp = conformal_tan(tau);
        let dtau = (taup - tp) * (1.0 + e2m * tau * tau) / (e2m * tp.hypot(1.0) * tau.hypot(1.0));
        tau += dtau;
        if dtau.is_nan() || dtau.abs() <= 1e-15 * tau.abs().max(1.0) {
            break;
        }
    }
    tau.is_finite().then_some(tau)
}

/// Transverse Mercator at scale K0: (x, y) in metres from the central meridian and the equator.
fn tm_forward(lat: f64, dlon: f64) -> Option<(f64, f64)> {
    let s = &*SERIES;
    let lam = dlon.to_radians();
    let taup = conformal_tan(lat.to_radians().tan());
    let xip = taup.atan2(lam.cos());
    let etap = (lam.sin() / taup.hypot(lam.cos())).asinh();
    let (mut xi, mut eta) = (xip, etap);
    for (j, a) in s.alpha.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        xi += a * (k * xip).sin() * (k * etap).cosh();
        eta += a * (k * xip).cos() * (k * etap).sinh();
    }
    let (x, y) = (K0 * s.rect * eta, K0 * s.rect * xi);
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// The inverse of [`tm_forward`]: (latitude, longitude from the central meridian) in degrees.
fn tm_inverse(x: f64, y: f64) -> Option<(f64, f64)> {
    let s = &*SERIES;
    let (xi, eta) = (y / (K0 * s.rect), x / (K0 * s.rect));
    let (mut xip, mut etap) = (xi, eta);
    for (j, b) in s.beta.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        xip -= b * (k * xi).sin() * (k * eta).cosh();
        etap -= b * (k * xi).cos() * (k * eta).sinh();
    }
    let taup = xip.sin() / etap.sinh().hypot(xip.cos());
    let lam = etap.sinh().atan2(xip.cos());
    let lat = geographic_tan(taup)?.atan().to_degrees();
    let dlon = lam.to_degrees();
    (lat.is_finite() && dlon.is_finite()).then_some((lat, dlon))
}

/// A longitude within [-180, 180).
fn wrap(lon: f64) -> f64 {
    let l = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if l >= 180.0 { l - 360.0 } else { l }
}

/// The longitude of a zone's central meridian (degrees).
pub fn central_meridian(zone: u8) -> f64 {
    f64::from(zone) * 6.0 - 183.0
}

/// The UTM zone (1–60) of a place, with the exceptions for southern Norway and Svalbard; `None`
/// outside the UTM latitudes (80° S to 84° N) or for a non-finite place. Longitude 180° is zone 1.
pub fn utm_zone(c: Coord) -> Option<u8> {
    if !c.is_finite() || !(UTM_MIN_LAT..UTM_MAX_LAT).contains(&c.y) {
        return None;
    }
    let (lon, lat) = (wrap(c.x), c.y);
    let mut zone = (((lon + 180.0) / 6.0).floor() as i64 + 1).clamp(1, 60);
    if (56.0..64.0).contains(&lat) && (3.0..12.0).contains(&lon) {
        zone = 32;
    }
    if (72.0..UTM_MAX_LAT).contains(&lat) && (0.0..42.0).contains(&lon) {
        zone = match lon {
            l if l < 9.0 => 31,
            l if l < 21.0 => 33,
            l if l < 33.0 => 35,
            _ => 37,
        };
    }
    u8::try_from(zone).ok()
}

/// The latitude band letter (C–X, without I and O) of a latitude; `None` outside 80° S to 84° N.
pub fn latitude_band(lat: f64) -> Option<char> {
    if !(UTM_MIN_LAT..UTM_MAX_LAT).contains(&lat) {
        return None;
    }
    let i = (((lat - UTM_MIN_LAT) / 8.0).floor() as usize).min(BANDS.len() - 1);
    BANDS.get(i).map(|b| char::from(*b))
}

/// The latitudes (south, north) a band letter covers; `None` for a letter that isn't a band.
pub fn band_latitudes(band: char) -> Option<(f64, f64)> {
    let i = BANDS.iter().position(|b| char::from(*b) == band)?;
    let south = UTM_MIN_LAT + 8.0 * i as f64;
    Some((south, if band == 'X' { UTM_MAX_LAT } else { south + 8.0 }))
}

/// The longitudes (west, east) a zone covers within a band, with the Norway and Svalbard
/// exceptions; `None` for a zone that doesn't exist there (32X, 34X, 36X) or out of range.
pub fn zone_longitudes(zone: u8, band: char) -> Option<(f64, f64)> {
    if !(1..=60).contains(&zone) {
        return None;
    }
    let cm = central_meridian(zone);
    let span = match (band, zone) {
        ('V', 31) => (0.0, 3.0),
        ('V', 32) => (3.0, 12.0),
        ('X', 32 | 34 | 36) => return None,
        ('X', 31) => (0.0, 9.0),
        ('X', 33) => (9.0, 21.0),
        ('X', 35) => (21.0, 33.0),
        ('X', 37) => (33.0, 42.0),
        _ => (cm - 3.0, cm + 3.0),
    };
    Some(span)
}

/// A place in UTM: its zone, latitude band, and easting and northing in metres (northings in the
/// southern hemisphere carry the 10,000 km false northing).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Utm {
    pub zone: u8,
    pub band: char,
    pub easting: f64,
    pub northing: f64,
}

impl Utm {
    /// Whether the place is in the northern hemisphere (bands N to X).
    pub fn north(&self) -> bool {
        self.band >= 'N'
    }

    /// The hemisphere spelt out, as the readout writes it after the zone: `north` or `south`.
    pub fn hemisphere(&self) -> &'static str {
        if self.north() { "north" } else { "south" }
    }
}

impl fmt::Display for Utm {
    /// `33north 412345 5523456`, `21south 371625 6170423`: the zone and the hemisphere spelt out
    /// (GeographicLib's long form, as `GeoConvert -u -l` writes it), then the easting and northing
    /// rounded to the metre. Latitude bands belong to MGRS: in plain UTM the letter after the zone
    /// is commonly read as the hemisphere, so `38S` (band S, 32° N to 40° N) would be taken for
    /// the southern hemisphere, and a single `S` can't be told apart from band S when the text is
    /// read back. The spelt-out hemisphere reads one way only and parses back with [`parse_utm`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{} {:.0} {:.0}", self.zone, self.hemisphere(), self.easting.round(), self.northing.round())
    }
}

/// A place's UTM coordinates in its own zone; `None` outside 80° S to 84° N (the UPS areas) or
/// for a non-finite place.
pub fn to_utm(c: Coord) -> Option<Utm> {
    let zone = utm_zone(c)?;
    let band = latitude_band(c.y)?;
    let (easting, northing) = to_utm_in_zone(c, zone, band >= 'N')?;
    Some(Utm { zone, band, easting, northing })
}

/// A place's easting and northing in a given zone and hemisphere, as a projection to that zone
/// works (EPSG:326zz north, 327zz south). `None` for a zone outside 1–60, a non-finite place, a
/// latitude past ±90° or a longitude more than 45° from the zone's central meridian, where the
/// series no longer holds.
pub fn to_utm_in_zone(c: Coord, zone: u8, north: bool) -> Option<(f64, f64)> {
    if !(1..=60).contains(&zone) || !c.is_finite() || c.y.abs() > 90.0 {
        return None;
    }
    let dlon = wrap(c.x - central_meridian(zone));
    if dlon.abs() > 45.0 {
        return None;
    }
    let (x, y) = tm_forward(c.y, dlon)?;
    Some((FALSE_EASTING + x, if north { y } else { y + FALSE_NORTHING_SOUTH }))
}

/// The place (longitude, latitude) at an easting and northing in a zone and hemisphere. `None`
/// for a zone outside 1–60, non-finite values, or an easting or northing so far out that it
/// names no place (more than 1,000 km from the central meridian or past a pole).
pub fn from_utm(zone: u8, north: bool, easting: f64, northing: f64) -> Option<Coord> {
    if !(1..=60).contains(&zone) || !easting.is_finite() || !northing.is_finite() {
        return None;
    }
    let x = easting - FALSE_EASTING;
    let y = if north { northing } else { northing - FALSE_NORTHING_SOUTH };
    if x.abs() > 1_000_000.0 || y.abs() > K0 * SERIES.rect * std::f64::consts::FRAC_PI_2 {
        return None;
    }
    let (lat, dlon) = tm_inverse(x, y)?;
    Some(Coord::new(wrap(central_meridian(zone) + dlon), lat))
}

/// A place as `33north 412345 5523456` (zone and hemisphere; see [`Utm`]'s `Display`); `None`
/// outside 80° S to 84° N.
pub fn format_utm(c: Coord) -> Option<String> {
    to_utm(c).map(|u| u.to_string())
}

/// An MGRS reference: zone, band, the 100 km square's column and row letters, and the easting
/// and northing within the square truncated to `digits` digits each (0–5: 100 km to 1 m).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mgrs {
    pub zone: u8,
    pub band: char,
    pub column: char,
    pub row: char,
    pub easting: u32,
    pub northing: u32,
    pub digits: u8,
}

impl Mgrs {
    /// The size of the cell the reference names, in metres (1 m for 5 digits, 100 km for 0).
    pub fn precision_m(&self) -> f64 {
        10f64.powi(5 - i32::from(self.digits.min(5)))
    }

    /// Without spaces: `33UVP1234523456`, `04QFJ12345678`.
    pub fn compact(&self) -> String {
        format!("{:02}{}{}{}{}", self.zone, self.band, self.column, self.row, self.digit_groups().concat())
    }

    /// The easting and northing digits, zero-padded to the precision (empty for 100 km).
    fn digit_groups(&self) -> [String; 2] {
        let w = usize::from(self.digits.min(5));
        if w == 0 {
            return [String::new(), String::new()];
        }
        [format!("{:0w$}", self.easting), format!("{:0w$}", self.northing)]
    }

    /// The centre of the cell the reference names, checked against its zone and band.
    pub fn centre(&self) -> Result<Coord, GridError> {
        locate(self)
    }
}

impl fmt::Display for Mgrs {
    /// `33U VP 12345 23456` (the readout's form; `33U VP` at 100 km; `04Q FJ …` in zones 1–9).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}{} {}{}", self.zone, self.band, self.column, self.row)?;
        let [e, n] = self.digit_groups();
        if !e.is_empty() {
            write!(f, " {e} {n}")?;
        }
        Ok(())
    }
}

/// A place's MGRS reference to `digits` digits per axis (clamped to 0–5: 100 km to 1 m),
/// truncated as MGRS requires; `None` outside 80° S to 84° N (the UPS areas).
pub fn to_mgrs(c: Coord, digits: u8) -> Option<Mgrs> {
    let digits = digits.min(5);
    let u = to_utm(c)?;
    // Whole metres first, truncated, so the square letters and the digits agree at an edge. Just
    // south of the equator the northing can round up to the full 10,000 km false northing, which
    // is the equator itself and row A of band N: it is kept a metre short, in row V of band M.
    let (e, mut n) = (u.easting.floor(), u.northing.floor());
    if !u.north() {
        n = n.min(FALSE_NORTHING_SOUTH - 1.0);
    }
    if !(SQUARE..9.0 * SQUARE).contains(&e) || !(0.0..=FALSE_NORTHING_SOUTH).contains(&n) {
        return None;
    }
    let (e, n) = (e as i64, n as i64);
    let set = COLUMNS.get(usize::from((u.zone - 1) % 3))?;
    let column = char::from(*set.get(usize::try_from(e / 100_000 - 1).ok()?)?);
    let offset = if u.zone.is_multiple_of(2) { EVEN_ZONE_ROW_OFFSET } else { 0 };
    let row = char::from(*ROWS.get(usize::try_from((n / 100_000 + offset).rem_euclid(20)).ok()?)?);
    let scale = 10i64.pow(5 - u32::from(digits));
    Some(Mgrs {
        zone: u.zone,
        band: u.band,
        column,
        row,
        easting: u32::try_from(e % 100_000 / scale).ok()?,
        northing: u32::try_from(n % 100_000 / scale).ok()?,
        digits,
    })
}

/// A place as `33U VP 12345 23456` (1 m); `None` outside 80° S to 84° N.
pub fn format_mgrs(c: Coord) -> Option<String> {
    to_mgrs(c, 5).map(|m| m.to_string())
}

/// Why a typed UTM or MGRS reference was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridError {
    /// Not written as a grid reference at all.
    Syntax,
    /// A zone outside 1–60.
    Zone,
    /// A letter that isn't a latitude band.
    Band,
    /// A polar (UPS) reference: bands A, B, Y and Z.
    Polar,
    /// 100 km square letters that the zone doesn't use.
    Square,
    /// An odd number of digits, more than ten, or two groups of different lengths.
    Digits,
    /// The northing doesn't fall in the latitude band given.
    BandMismatch,
    /// The place lies outside the zone (or the zone doesn't exist in that band).
    OutsideZone,
    /// `S` after a UTM zone with a northing outside band S: it may mean the southern hemisphere.
    Ambiguous,
}

impl fmt::Display for GridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GridError::Syntax => "not a UTM or MGRS grid reference",
            GridError::Zone => "the UTM zone must be 1 to 60",
            GridError::Band => "the latitude band must be a letter from C to X (not I or O)",
            GridError::Polar => "polar (UPS) grid references, north of 84° N or south of 80° S, aren't supported",
            GridError::Square => "the 100 km square letters aren't used in this zone",
            GridError::Digits => "the grid digits must be an even count of 0 to 10, half for the easting and half for the northing",
            GridError::BandMismatch => "the northing doesn't fall in the latitude band given",
            GridError::OutsideZone => "the place lies outside the zone given",
            GridError::Ambiguous => {
                "S after the zone is latitude band S; for the southern hemisphere write the zone, then south (33 south 412345 3768000)"
            }
        })
    }
}

impl std::error::Error for GridError {}

/// Whether a place found from a zone and band lies in them. `lats` are the lowest and highest
/// latitudes of the cell the reference names (the place's own for a UTM position): the cell must
/// reach into the band, give or take [`ROUNDING_DEG`]. The place may lie outside the zone's
/// longitudes by `cell_m` (the cell's size) and [`EDGE_SLOP_DEG`].
fn check_place(p: Coord, lats: (f64, f64), zone: u8, band: char, cell_m: f64) -> Result<(), GridError> {
    let (south, north) = band_latitudes(band).ok_or(GridError::Band)?;
    if lats.1 < south - ROUNDING_DEG || lats.0 > north + ROUNDING_DEG {
        return Err(GridError::BandMismatch);
    }
    let (west, east) = zone_longitudes(zone, band).ok_or(GridError::OutsideZone)?;
    let cm = central_meridian(zone);
    let dlon = wrap(p.x - cm);
    let lon_slop = cell_m / (M_PER_DEG * p.y.to_radians().cos().max(0.01)) + EDGE_SLOP_DEG;
    if dlon < west - cm - lon_slop || dlon > east - cm + lon_slop {
        return Err(GridError::OutsideZone);
    }
    Ok(())
}

/// The lowest and highest latitudes of a grid cell (south-west corner `e0`, `n0`, side `cell`).
/// Along a line of constant easting the latitude grows with the northing, and along a line of
/// constant northing it lies farthest from the equator on the central meridian. A cell never
/// straddles the central meridian (its 500 km easting is a multiple of every cell size), so the
/// extremes are at its corners. `None` if a corner names no place (past a pole).
fn cell_latitudes(zone: u8, north: bool, e0: f64, n0: f64, cell: f64) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for e in [e0, e0 + cell] {
        for n in [n0, n0 + cell] {
            let p = from_utm(zone, north, e, n)?;
            lo = lo.min(p.y);
            hi = hi.max(p.y);
        }
    }
    Some((lo, hi))
}

/// The centre of the cell an MGRS reference names: the 2,000 km row cycle is settled by the band,
/// which the cell must reach into.
fn locate(m: &Mgrs) -> Result<Coord, GridError> {
    if !(1..=60).contains(&m.zone) {
        return Err(GridError::Zone);
    }
    if matches!(m.band, 'A' | 'B' | 'Y' | 'Z') {
        return Err(GridError::Polar);
    }
    if m.digits > 5 {
        return Err(GridError::Digits);
    }
    let (south, _) = band_latitudes(m.band).ok_or(GridError::Band)?;
    let set = COLUMNS.get(usize::from((m.zone - 1) % 3)).ok_or(GridError::Zone)?;
    let col = set.iter().position(|c| char::from(*c) == m.column).ok_or(GridError::Square)?;
    let row = ROWS.iter().position(|c| char::from(*c) == m.row).ok_or(GridError::Square)? as i64;
    let offset = if m.zone.is_multiple_of(2) { EVEN_ZONE_ROW_OFFSET } else { 0 };
    let row = (row - offset).rem_euclid(20) as f64;
    let cell = m.precision_m();
    if f64::from(m.easting) * cell >= SQUARE || f64::from(m.northing) * cell >= SQUARE {
        return Err(GridError::Digits);
    }
    let e0 = (col as f64 + 1.0) * SQUARE + f64::from(m.easting) * cell;
    let n0 = row * SQUARE + f64::from(m.northing) * cell;
    let north = south >= 0.0;
    // Of the five northings the letters can stand for (0 to 10,000 km), the one in the band.
    let mut found = None;
    for k in 0..5 {
        let n0 = n0 + f64::from(k) * ROW_CYCLE;
        let Some(p) = from_utm(m.zone, north, e0 + cell / 2.0, n0 + cell / 2.0) else { continue };
        let Some(lats) = cell_latitudes(m.zone, north, e0, n0, cell) else { continue };
        match check_place(p, lats, m.zone, m.band, cell) {
            Ok(()) => {
                found = Some(p);
                break;
            }
            Err(GridError::OutsideZone) => return Err(GridError::OutsideZone),
            Err(_) => {}
        }
    }
    found.ok_or(GridError::BandMismatch)
}

/// Most characters a typed grid reference may have.
const MAX_REF_CHARS: usize = 64;

/// Reads a zone number (one or two digits) and the letter after it, skipping spaces. The zone
/// isn't checked to be 1–60 here (the caller does, once the rest reads as a reference): until
/// then, `75 Main St` is text.
fn zone_and_letter(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<(u8, char), GridError> {
    while chars.next_if(|c| c.is_whitespace()).is_some() {}
    let mut zone = String::new();
    while let Some(d) = chars.next_if(char::is_ascii_digit) {
        zone.push(d);
    }
    if zone.is_empty() || zone.len() > 2 {
        return Err(GridError::Syntax);
    }
    let zone: u8 = zone.parse().map_err(|_| GridError::Syntax)?;
    while chars.next_if(|c| c.is_whitespace()).is_some() {}
    let letter = chars.next().filter(char::is_ascii_alphabetic).ok_or(GridError::Syntax)?.to_ascii_uppercase();
    Ok((zone, letter))
}

/// Checks a band letter, telling the polar letters apart.
fn band_letter(letter: char) -> Result<char, GridError> {
    if matches!(letter, 'A' | 'B' | 'Y' | 'Z') {
        return Err(GridError::Polar);
    }
    band_latitudes(letter).map(|_| letter).ok_or(GridError::Band)
}

/// Reads an MGRS reference, case-insensitive, with or without spaces: `33UVP1234523456`,
/// `33U VP 12345 23456`, `33UVP 1234 2345`, `4QFJ` (100 km). The digits are an even count of 0
/// to 10, as one run or as two groups of the same length. The square letters are checked against
/// the zone, and the northing against the band, so a reference that doesn't exist is refused
/// rather than placed somewhere else. Polar (UPS) references are refused with
/// [`GridError::Polar`].
pub fn parse_mgrs(text: &str) -> Result<Mgrs, GridError> {
    let text = text.trim();
    if text.chars().count() > MAX_REF_CHARS {
        return Err(GridError::Syntax);
    }
    let mut chars = text.chars().peekable();
    let (zone, band) = zone_and_letter(&mut chars)?;
    let mut letters = [' '; 2];
    for l in letters.iter_mut() {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        *l = chars.next().filter(char::is_ascii_alphabetic).ok_or(GridError::Syntax)?.to_ascii_uppercase();
    }
    let rest: String = chars.collect();
    if rest.chars().any(|c| !c.is_ascii_digit() && !c.is_whitespace()) {
        return Err(GridError::Syntax);
    }
    let groups: Vec<&str> = rest.split_whitespace().collect();
    let (e, n) = match groups.as_slice() {
        [] => ("", ""),
        [one] => {
            if one.len() % 2 != 0 || one.len() > 10 {
                return Err(GridError::Digits);
            }
            one.split_at(one.len() / 2)
        }
        [e, n] => {
            if e.len() != n.len() || e.len() > 5 {
                return Err(GridError::Digits);
            }
            (*e, *n)
        }
        _ => return Err(GridError::Digits),
    };
    let band = band_letter(band)?;
    let [column, row] = letters;
    let digit = |s: &str| if s.is_empty() { Ok(0) } else { s.parse::<u32>().map_err(|_| GridError::Digits) };
    let m = Mgrs {
        zone,
        band,
        column,
        row,
        easting: digit(e)?,
        northing: digit(n)?,
        digits: u8::try_from(e.len()).map_err(|_| GridError::Digits)?,
    };
    locate(&m)?;
    Ok(m)
}

/// Reads a UTM position: a zone, then the hemisphere spelt out or a latitude band letter, then
/// the easting and northing in metres (decimals allowed; spaces or commas between). Accepted:
///
/// - `33north 412345 5523456`, `33south 412345 4476544` (also `33 north`, any case): zone and
///   hemisphere, as the readout writes it.
/// - `33U 412345 5523456` or `33 U 412345 5523456`: zone and latitude band. The northing must
///   fall in the band.
/// - `33N 412345 5523456`: band N lies wholly in the northern hemisphere, so a northing outside
///   band N is read as the northern hemisphere (both readings give the same place).
///
/// `S` after the zone is band S (32° N to 40° N). A northing outside band S may mean the
/// southern hemisphere, so it is refused as [`GridError::Ambiguous`] rather than guessed.
/// Polar (UPS) references are refused.
pub fn parse_utm(text: &str) -> Result<Coord, GridError> {
    let text = text.trim();
    if text.chars().count() > MAX_REF_CHARS {
        return Err(GridError::Syntax);
    }
    let mut chars = text.chars().peekable();
    let (zone, letter) = zone_and_letter(&mut chars)?;
    let mut word = String::from(letter);
    while let Some(c) = chars.next_if(char::is_ascii_alphabetic) {
        word.push(c.to_ascii_uppercase());
    }
    if !matches!(word.as_str(), "NORTH" | "SOUTH") && word.chars().count() != 1 {
        return Err(GridError::Syntax);
    }
    let rest: String = chars.collect();
    let nums: Vec<&str> = rest.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).collect();
    let [es, ns] = nums.as_slice() else { return Err(GridError::Syntax) };
    let number = |s: &str| {
        // Plain decimals only: no signs, exponents or "inf".
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Err(GridError::Syntax);
        }
        s.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or(GridError::Syntax)
    };
    let (easting, northing) = (number(es)?, number(ns)?);
    if !(1..=60).contains(&zone) {
        return Err(GridError::Zone);
    }
    if !(0.0..1_000_000.0).contains(&easting) || !(0.0..=FALSE_NORTHING_SOUTH).contains(&northing) {
        return Err(GridError::OutsideZone);
    }
    // A place from a hemisphere, checked against the band of its own latitude, or of a latitude
    // within the metre it was rounded to: a readout just south of 72° N in zone 32, 34 or 36 can
    // read back a hair north of it, where Svalbard's zones take over, and so can one by the edges
    // of southern Norway's 32V. The band form allows the same.
    let by_hemisphere = |north: bool| -> Result<Coord, GridError> {
        let p = from_utm(zone, north, easting, northing).ok_or(GridError::OutsideZone)?;
        if p.y >= UTM_MAX_LAT + ROUNDING_DEG || p.y < UTM_MIN_LAT - ROUNDING_DEG {
            return Err(GridError::Polar);
        }
        let band_at = |lat: f64| latitude_band(lat.clamp(UTM_MIN_LAT, UTM_MAX_LAT - 1e-9)).ok_or(GridError::Polar);
        let own = band_at(p.y)?;
        let first = check_place(p, (p.y, p.y), zone, own, 1.0);
        if first.is_ok() {
            return Ok(p);
        }
        for lat in [p.y - ROUNDING_DEG, p.y + ROUNDING_DEG] {
            let band = band_at(lat)?;
            if band != own && check_place(p, (p.y, p.y), zone, band, 1.0).is_ok() {
                return Ok(p);
            }
        }
        first.map(|()| p)
    };
    match word.as_str() {
        "NORTH" => by_hemisphere(true),
        "SOUTH" => by_hemisphere(false),
        w if w.chars().count() == 1 => {
            let band = band_letter(letter)?;
            let north = band >= 'N';
            let p = from_utm(zone, north, easting, northing).ok_or(GridError::OutsideZone)?;
            // Past 84° N or 80° S is polar, as in the hemisphere form, whatever the band says.
            if p.y >= UTM_MAX_LAT + ROUNDING_DEG || p.y < UTM_MIN_LAT - ROUNDING_DEG {
                return Err(GridError::Polar);
            }
            match check_place(p, (p.y, p.y), zone, band, 1.0) {
                Ok(()) => Ok(p),
                Err(GridError::BandMismatch) if band == 'N' => by_hemisphere(true),
                Err(GridError::BandMismatch) if band == 'S' => Err(GridError::Ambiguous),
                Err(e) => Err(e),
            }
        }
        _ => Err(GridError::Syntax),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(lon: f64, lat: f64) -> Coord {
        Coord::new(lon, lat)
    }

    /// A small deterministic generator for sweeps (no dependency).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// GeographicLib's GeoConvert documentation converts 33.3° N 44.4° E (Baghdad) to
    /// `38n 444140.54 3684706.36` and MGRS `38SMB4414084706`
    /// (https://geographiclib.sourceforge.io/C++/doc/GeoConvert.1.html). Also checked against
    /// proj4rs in `crates/data/tests/utm_vs_proj.rs`.
    #[test]
    fn published_reference_point() {
        let u = to_utm(c(44.4, 33.3)).unwrap();
        assert_eq!((u.zone, u.band), (38, 'S'));
        assert!((u.easting - 444_140.54).abs() < 0.005, "{}", u.easting);
        assert!((u.northing - 3_684_706.36).abs() < 0.005, "{}", u.northing);
        assert_eq!(to_mgrs(c(44.4, 33.3), 5).unwrap().compact(), "38SMB4414084706");
        assert_eq!(format_utm(c(44.4, 33.3)).unwrap(), "38north 444141 3684706");
        assert_eq!(format_mgrs(c(44.4, 33.3)).unwrap(), "38S MB 44140 84706");
    }

    /// The equator at the prime meridian: GeographicLib's `GeoConvert -m` gives `31NAA6602100000`,
    /// and just south of it the row letter wraps to V with the 10,000 km false northing.
    #[test]
    fn equator_and_prime_meridian() {
        assert_eq!(to_mgrs(c(0.0, 0.0), 5).unwrap().compact(), "31NAA6602100000");
        let s = to_utm(c(0.0, -1e-7)).unwrap();
        assert_eq!(s.band, 'M');
        assert!(!s.north());
        assert!(s.northing > 9_999_980.0 && s.northing < FALSE_NORTHING_SOUTH, "{}", s.northing);
        assert_eq!(to_mgrs(c(0.0, -1e-7), 5).unwrap().compact(), "31MAV6602199999");
        // On the central meridian the easting is exactly the false easting, the northing zero.
        let u = to_utm(c(3.0, 0.0)).unwrap();
        assert!((u.easting - 500_000.0).abs() < 1e-6 && u.northing.abs() < 1e-6, "{u:?}");
    }

    /// The meridian distance at k0 = 0.9996: the quarter meridian of WGS 84 is 10,001,965.729 m
    /// (Karney 2011, table 1's A·π/2), so 9,997,964.943 m at the pole on the central meridian.
    #[test]
    fn meridian_arc_matches_the_quarter_meridian() {
        let (_, y) = tm_forward(90.0, 0.0).unwrap();
        assert!((y - 0.9996 * 10_001_965.729_313).abs() < 1e-3, "{y}");
        let (_, y) = tm_forward(45.0, 0.0).unwrap();
        // The WGS 84 meridian arc to 45° is 4,984,944.378 m (Karney 2011 / GeographicLib).
        assert!((y - 0.9996 * 4_984_944.378).abs() < 1e-3, "{y}");
    }

    #[test]
    fn round_trips_within_a_nanodegree() {
        let mut r = Lcg(7);
        for _ in 0..20_000 {
            let lat = UTM_MIN_LAT + r.next() * (UTM_MAX_LAT - UTM_MIN_LAT);
            let lon = -180.0 + r.next() * 360.0;
            let u = to_utm(c(lon, lat)).unwrap();
            let back = from_utm(u.zone, u.north(), u.easting, u.northing).unwrap();
            assert!((back.y - lat).abs() < 1e-9, "{lon} {lat} {back:?}");
            assert!(wrap(back.x - lon).abs() < 1e-9, "{lon} {lat} {back:?}");
            assert!((100_000.0..900_000.0).contains(&u.easting), "{lon} {lat} {u:?}");
        }
        // Far from the central meridian, as Svalbard's wide zones and a forced zone reach (up to
        // the 1,000 km `from_utm` takes).
        for (dlon, lat) in [(9.0, 72.0), (8.0, 10.0), (15.0, 60.0), (-8.0, -45.0)] {
            let (e, n) = to_utm_in_zone(c(dlon + 3.0, lat), 31, lat >= 0.0).unwrap();
            let back = from_utm(31, lat >= 0.0, e, n).unwrap();
            assert!((back.y - lat).abs() < 1e-9 && (back.x - dlon - 3.0).abs() < 1e-9, "{dlon} {lat} {back:?}");
        }
    }

    #[test]
    fn zones_at_their_edges() {
        assert_eq!(utm_zone(c(-180.0, 0.0)), Some(1));
        assert_eq!(utm_zone(c(180.0, 0.0)), Some(1));
        assert_eq!(utm_zone(c(179.999_999, 0.0)), Some(60));
        assert_eq!(utm_zone(c(540.0, 0.0)), Some(1));
        assert_eq!(utm_zone(c(-174.000_001, 0.0)), Some(1));
        assert_eq!(utm_zone(c(-174.0, 0.0)), Some(2));
        assert_eq!(utm_zone(c(0.0, 10.0)), Some(31));
        assert_eq!(utm_zone(c(-1e-9, 10.0)), Some(30));
        assert_eq!(utm_zone(c(6.0, 10.0)), Some(32));
        assert_eq!(utm_zone(c(5.999_999, 10.0)), Some(31));
        // Southern Norway: 32V from 3° E to 12° E between 56° N and 64° N.
        assert_eq!(utm_zone(c(2.999_999, 60.0)), Some(31));
        assert_eq!(utm_zone(c(3.0, 60.0)), Some(32));
        assert_eq!(utm_zone(c(5.0, 60.0)), Some(32));
        assert_eq!(utm_zone(c(11.999, 60.0)), Some(32));
        assert_eq!(utm_zone(c(12.0, 60.0)), Some(33));
        assert_eq!(utm_zone(c(5.0, 55.999_999)), Some(31));
        assert_eq!(utm_zone(c(5.0, 56.0)), Some(32));
        assert_eq!(utm_zone(c(5.0, 63.999_999)), Some(32));
        assert_eq!(utm_zone(c(5.0, 64.0)), Some(31));
        // Svalbard: 31X 0–9° E, 33X 9–21° E, 35X 21–33° E, 37X 33–42° E from 72° N.
        for (lon, zone) in [
            (-0.000_001, 30),
            (0.0, 31),
            (8.999, 31),
            (9.0, 33),
            (20.999, 33),
            (21.0, 35),
            (32.999, 35),
            (33.0, 37),
            (41.999, 37),
            (42.0, 38),
        ] {
            assert_eq!(utm_zone(c(lon, 78.0)), Some(zone), "{lon}");
        }
        assert_eq!(utm_zone(c(10.0, 71.999_999)), Some(32));
        assert_eq!(utm_zone(c(10.0, 72.0)), Some(33));
        // The UTM latitudes: 80° S included, 84° N not; the rest is UPS.
        assert_eq!(utm_zone(c(0.0, -80.0)), Some(31));
        assert_eq!(utm_zone(c(0.0, -80.000_001)), None);
        assert_eq!(utm_zone(c(0.0, 83.999_999)), Some(31));
        assert_eq!(utm_zone(c(0.0, 84.0)), None);
        assert_eq!(utm_zone(c(f64::NAN, 0.0)), None);
        assert_eq!(utm_zone(c(0.0, f64::INFINITY)), None);
    }

    #[test]
    fn bands_at_their_edges() {
        assert_eq!(latitude_band(-80.0), Some('C'));
        assert_eq!(latitude_band(-72.000_001), Some('C'));
        assert_eq!(latitude_band(-72.0), Some('D'));
        assert_eq!(latitude_band(-0.000_001), Some('M'));
        assert_eq!(latitude_band(0.0), Some('N'));
        assert_eq!(latitude_band(7.999_999), Some('N'));
        assert_eq!(latitude_band(8.0), Some('P'));
        assert_eq!(latitude_band(47.999), Some('T'));
        assert_eq!(latitude_band(48.0), Some('U'));
        assert_eq!(latitude_band(63.999), Some('V'));
        assert_eq!(latitude_band(64.0), Some('W'));
        assert_eq!(latitude_band(71.999), Some('W'));
        assert_eq!(latitude_band(72.0), Some('X'));
        assert_eq!(latitude_band(80.0), Some('X'));
        assert_eq!(latitude_band(83.999_999), Some('X'));
        assert_eq!(latitude_band(84.0), None);
        assert_eq!(latitude_band(-80.000_001), None);
        assert_eq!(latitude_band(f64::NAN), None);
        // Every band round-trips through its latitudes, I and O skipped.
        for (i, b) in "CDEFGHJKLMNPQRSTUVWX".chars().enumerate() {
            let (s, n) = band_latitudes(b).unwrap();
            assert_eq!(s, -80.0 + 8.0 * i as f64);
            assert_eq!(latitude_band(s), Some(b));
            assert_eq!(latitude_band(n - 1e-9), Some(b));
        }
        assert_eq!(band_latitudes('X'), Some((72.0, 84.0)));
        assert_eq!(band_latitudes('I'), None);
        assert_eq!(band_latitudes('O'), None);
        assert_eq!(band_latitudes('Y'), None);
    }

    /// The 100 km square letters along the equator at each zone's central meridian (easting
    /// 500 km: the fifth column letter) cycle through the three column sets and, in even zones,
    /// start their rows at F.
    #[test]
    fn square_letters_by_zone_set_and_parity() {
        let expect = ["EA", "NF", "WA", "EF", "NA", "WF"];
        for zone in 1u8..=60 {
            let m = to_mgrs(c(central_meridian(zone), 0.0), 5).unwrap();
            assert_eq!(m.zone, zone);
            let letters: String = [m.column, m.row].iter().collect();
            assert_eq!(letters, expect[usize::from((zone - 1) % 6)], "zone {zone}");
        }
        // Across a zone edge on the equator: 31's first column (A), 30's last (Z), 32's first (J).
        assert_eq!(to_mgrs(c(0.000_001, 0.5), 5).unwrap().column, 'A');
        assert_eq!(to_mgrs(c(-0.000_001, 0.5), 5).unwrap().column, 'Z');
        assert_eq!(to_mgrs(c(6.000_001, 0.5), 5).unwrap().column, 'J');
        assert_eq!(to_mgrs(c(5.999_999, 0.5), 5).unwrap().column, 'H');
        // Rows go up a letter every 100 km and wrap after V at 2,000 km.
        let odd = |n: f64| to_mgrs(from_utm(31, true, 500_000.0, n).unwrap(), 5).unwrap().row;
        assert_eq!(odd(50_000.0), 'A');
        assert_eq!(odd(150_000.0), 'B');
        assert_eq!(odd(1_950_000.0), 'V');
        assert_eq!(odd(2_050_000.0), 'A');
        let even = |n: f64| to_mgrs(from_utm(32, true, 500_000.0, n).unwrap(), 5).unwrap().row;
        assert_eq!(even(50_000.0), 'F');
        assert_eq!(even(1_450_000.0), 'V');
        assert_eq!(even(1_550_000.0), 'A');
        // Norway's 32V: west of 6° E, in zone 32 with its J–R columns, not 31's A–H.
        let m = to_mgrs(c(5.0, 60.0), 5).unwrap();
        assert_eq!((m.zone, m.band), (32, 'V'));
        assert!("JKLMNPQR".contains(m.column), "{m}");
        // Svalbard's 33X at 10° E.
        let m = to_mgrs(c(10.0, 78.0), 5).unwrap();
        assert_eq!((m.zone, m.band), (33, 'X'));
        assert!("STUVWXYZ".contains(m.column), "{m}");
    }

    #[test]
    fn mgrs_truncates_to_its_precision() {
        // A place at easting 444,140.54 and northing 3,684,706.36 (38S MB).
        let p = c(44.4, 33.3);
        let want = ["38S MB", "38S MB 4 8", "38S MB 44 84", "38S MB 441 847", "38S MB 4414 8470", "38S MB 44140 84706"];
        for (d, w) in want.iter().enumerate() {
            let m = to_mgrs(p, d as u8).unwrap();
            assert_eq!(m.to_string(), *w);
            assert_eq!(m.precision_m(), 10f64.powi(5 - d as i32));
        }
        // A place 0.9 m short of a metre is truncated to the metre below, not rounded up.
        let near = from_utm(38, true, 444_140.9, 3_684_706.9).unwrap();
        assert_eq!(to_mgrs(near, 5).unwrap().compact(), "38SMB4414084706");
        let m = to_mgrs(near, 4).unwrap();
        assert_eq!((m.easting, m.northing), (4414, 8470));
        // More than five digits is clamped to five.
        assert_eq!(to_mgrs(p, 9).unwrap().digits, 5);
        // The UTM readout rounds instead.
        assert_eq!(from_utm(38, true, 444_140.6, 3_684_706.4).map(format_utm), Some(Some("38north 444141 3684706".into())));
        assert!(to_mgrs(c(0.0, 84.0), 5).is_none());
        assert!(format_mgrs(c(0.0, -85.0)).is_none());
    }

    #[test]
    fn mgrs_parses_every_precision_both_ways() {
        let p = c(44.4, 33.3);
        for d in 0u8..=5 {
            let m = to_mgrs(p, d).unwrap();
            let cell = m.precision_m();
            for text in [m.to_string(), m.compact(), m.compact().to_lowercase(), format!("  {}  ", m.to_string().to_lowercase())] {
                let parsed = parse_mgrs(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
                assert_eq!(parsed, m, "{text}");
                let centre = parsed.centre().unwrap();
                // The centre of the cell lies within half a cell of the place, along each axis.
                let u = to_utm(centre).unwrap();
                let pu = to_utm(p).unwrap();
                assert!((u.easting - pu.easting).abs() <= cell / 2.0 + 1.0, "{text} {u:?}");
                assert!((u.northing - pu.northing).abs() <= cell / 2.0 + 1.0, "{text} {u:?}");
            }
        }
        // A 1 m reference names its metre square; its centre is half a metre in.
        let at = parse_mgrs("38SMB4414084706").unwrap().centre().unwrap();
        let u = to_utm(at).unwrap();
        assert!((u.easting - 444_140.5).abs() < 1e-6 && (u.northing - 3_684_706.5).abs() < 1e-6, "{u:?}");
        // Two groups, one group, spaces anywhere in the letters.
        for t in ["38SMB 44140 84706", "38S MB4414084706", "38 S M B 44140 84706", "38smb4414084706"] {
            assert_eq!(parse_mgrs(t).unwrap().compact(), "38SMB4414084706", "{t}");
        }
        // Leading zero in the zone; a one-digit zone.
        assert_eq!(parse_mgrs("04QFJ12345678").map(|m| m.zone), Ok(4));
        assert_eq!(parse_mgrs("4QFJ12345678").map(|m| (m.zone, m.digits)), Ok((4, 4)));
    }

    #[test]
    fn mgrs_round_trips_everywhere() {
        let mut r = Lcg(11);
        for _ in 0..5_000 {
            let lat = UTM_MIN_LAT + r.next() * (UTM_MAX_LAT - UTM_MIN_LAT - 1e-6);
            let lon = -180.0 + r.next() * 360.0;
            let m = to_mgrs(c(lon, lat), 5).unwrap();
            let parsed = parse_mgrs(&m.compact()).unwrap_or_else(|e| panic!("{m} at {lon} {lat}: {e}"));
            assert_eq!(parsed, m);
            let back = to_utm(parsed.centre().unwrap()).unwrap();
            let u = to_utm(c(lon, lat)).unwrap();
            assert!((back.easting - u.easting).abs() <= 1.0 && (back.northing - u.northing).abs() <= 1.0, "{m} {u:?} {back:?}");
        }
    }

    #[test]
    fn invalid_mgrs_is_refused() {
        for (t, e) in [
            ("", GridError::Syntax),
            ("SMB4414084706", GridError::Syntax),
            ("38", GridError::Syntax),
            ("38S", GridError::Syntax),
            ("38SM", GridError::Syntax),
            ("38SMB441408470", GridError::Digits),
            ("38SMB4414084706 1", GridError::Digits),
            ("38SMB 4414 84706", GridError::Digits),
            ("38SMB 441408 470600", GridError::Digits),
            ("38SMB441408470612", GridError::Digits),
            ("38SMB 1 2 3", GridError::Digits),
            ("38SMB44140-84706", GridError::Syntax),
            ("38SMB4414O84706", GridError::Syntax),
            ("38SMB4414084706X", GridError::Syntax),
            ("38S-MB", GridError::Syntax),
            ("123SMB", GridError::Syntax),
            ("0SMB", GridError::Zone),
            ("61SMB", GridError::Zone),
            // The zone is judged once the text is written as a reference: a number past 60
            // before a word is ordinary text.
            ("75 Main St", GridError::Syntax),
            ("99 Problems", GridError::Syntax),
            ("38IMB", GridError::Band),
            ("38OMB", GridError::Band),
            ("38ZMB", GridError::Polar),
            ("38AMB", GridError::Polar),
            // Zone 38's columns are J–R (38 is in the second set): A–H and S–Z are other zones'.
            ("38SAB4414084706", GridError::Square),
            ("38SSB4414084706", GridError::Square),
            // I and O are no row letters, W–Z are past V.
            ("38SMI4414084706", GridError::Square),
            ("38SMW4414084706", GridError::Square),
            // 38S MB's letters repeat every 2,000 km of northing: at 15° N (band P), 33° N (S),
            // 51° N (U) and 69° N (W). In band T (40°–48° N) or N (0°–8° N) they name no place.
            ("38TMB4414084706", GridError::BandMismatch),
            ("38NMB4414084706", GridError::BandMismatch),
            ("38QMB4414084706", GridError::BandMismatch),
            // Zone 33's westmost column S lies 350 km from the central meridian, beyond the
            // zone's edge at 60° N (167 km).
            ("33VSJ0000000000", GridError::OutsideZone),
            // 31V runs only to 3° E (Norway); 32X doesn't exist (Svalbard).
            ("31VGJ0000000000", GridError::OutsideZone),
            ("32XNM0000000000", GridError::OutsideZone),
        ] {
            assert_eq!(parse_mgrs(t), Err(e), "{t}");
        }
        // Garbage of every sort never panics.
        for t in ["99999999999999999999", "1\u{0}", "١٢SMB", "38S MB ８４７０６", "38ŞMB", &"9".repeat(200), "60X", "60XZZ", "1C"] {
            let _ = parse_mgrs(t);
            let _ = parse_utm(t);
        }
    }

    #[test]
    fn utm_parses_band_and_hemisphere_forms() {
        let p = c(15.3, 49.85);
        let u = to_utm(p).unwrap();
        assert_eq!((u.zone, u.band), (33, 'U'));
        let near = |q: Coord| (q.x - p.x).abs() < 1e-5 && (q.y - p.y).abs() < 1e-5;
        let text = u.to_string();
        assert!(near(parse_utm(&text).unwrap()), "{text}");
        let (e, n) = (u.easting.round(), u.northing.round());
        for t in [
            format!("33u {e} {n}"),
            format!("33 U {e}, {n}"),
            format!("33 north {e} {n}"),
            format!("33North {e} {n}"),
            format!("33N {e} {n}"),
            format!("33 n {e} {n}"),
            format!("33U {:.2} {:.2}", u.easting, u.northing),
        ] {
            assert!(near(parse_utm(&t).unwrap_or_else(|e| panic!("{t}: {e}"))), "{t}");
        }
        // The southern hemisphere, as the readout writes it, by band or spelt out with a space.
        let q = c(-58.4, -34.6);
        let s = to_utm(q).unwrap();
        assert_eq!((s.zone, s.band), (21, 'H'));
        assert!(s.to_string().starts_with("21south "), "{s}");
        for t in [
            s.to_string(),
            format!("21H {:.0} {:.0}", s.easting, s.northing),
            format!("21 south {:.0} {:.0}", s.easting, s.northing),
            format!("21SOUTH {:.0} {:.0}", s.easting, s.northing),
        ] {
            let got = parse_utm(&t).unwrap();
            assert!((got.x - q.x).abs() < 1e-5 && (got.y - q.y).abs() < 1e-5, "{t} {got:?}");
        }
        // Los Angeles is in band S (32°–40° N): the readout writes the hemisphere, and band S
        // typed by hand still reads as the band.
        let la = c(-118.25, 34.05);
        let l = to_utm(la).unwrap();
        assert_eq!((l.zone, l.band), (11, 'S'));
        assert!(l.to_string().starts_with("11north "), "{l}");
        let (e, n) = (l.easting.round(), l.northing.round());
        for t in [l.to_string(), format!("11S {e} {n}"), format!("11s {e} {n}")] {
            let got = parse_utm(&t).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert!((got.x - la.x).abs() < 1e-5 && (got.y - la.y).abs() < 1e-5, "{t}");
        }
    }

    /// The readout states the hemisphere, never a band letter, and reads back through
    /// [`parse_utm`] to within the half metre it is rounded to, in its own zone and hemisphere.
    fn assert_readout_round_trips(p: Coord) {
        let u = to_utm(p).unwrap_or_else(|| panic!("{p:?} is in UTM"));
        let text = format_utm(p).unwrap();
        let hemi = if p.y >= 0.0 { "north" } else { "south" };
        assert!(text.starts_with(&format!("{}{hemi} ", u.zone)), "{text} at {p:?}");
        assert_eq!(text, u.to_string());
        let back = parse_utm(&text).unwrap_or_else(|e| panic!("{text} at {p:?}: {e}"));
        let (e, n) = to_utm_in_zone(back, u.zone, p.y >= 0.0).unwrap();
        assert!((e - u.easting).abs() <= 0.5 + 1e-6 && (n - u.northing).abs() <= 0.5 + 1e-6, "{text} at {p:?}: {e} {n}");
        // As typed in another case, too.
        assert_eq!(parse_utm(&text.to_uppercase()), Ok(back), "{text}");
    }

    #[test]
    fn the_readout_writes_the_hemisphere_and_reads_back() {
        // Baghdad in band S, Buenos Aires in band H, Sydney, Cape Town, Ushuaia, Reykjavik.
        for p in [c(44.4, 33.3), c(-58.4, -34.6), c(151.2, -33.87), c(18.42, -33.92), c(-68.3, -54.8), c(-21.9, 64.15)] {
            assert_readout_round_trips(p);
        }
        assert_eq!(format_utm(c(151.2, -33.87)).map(|t| t[..8].to_string()), Some("56south ".into()));
        // Either side of the equator, including a place south of it whose northing rounds to the
        // full 10,000 km false northing.
        for lat in [-0.3, -1e-4, -1e-7, -1e-12, 0.0, 1e-12, 1e-7, 1e-4, 0.3] {
            for lon in [-177.0, 0.0, 3.0, 5.99, 44.4, 179.9] {
                assert_readout_round_trips(c(lon, lat));
            }
        }
        let rounds_up = (1..400)
            .map(|i| c(15.0, -1e-17 * f64::from(i) * 5.0))
            .find(|p| to_utm(*p).is_some_and(|u| u.band == 'M' && u.northing.round() == FALSE_NORTHING_SOUTH))
            .unwrap();
        assert!(format_utm(rounds_up).unwrap().starts_with("33south 500000 10000000"), "{:?}", format_utm(rounds_up));
        assert_readout_round_trips(rounds_up);
        // Band X up to 84° N and band C down to 80° S, and the band edges between.
        let metres = |m: f64| m / M_PER_DEG;
        for lat in [UTM_MAX_LAT - metres(0.6), 83.9, 72.0, 72.0 - 1e-7, UTM_MIN_LAT, UTM_MIN_LAT + metres(0.6), -72.0, -72.0 - 1e-7] {
            for lon in [-179.0, 4.0, 10.0, 25.0, 40.0, 100.0] {
                assert_readout_round_trips(c(lon, lat));
            }
        }
        // Zones 1–9 keep one digit, in both hemispheres.
        for zone in 1u8..=9 {
            for lat in [-45.0, -1.0, 1.0, 45.0] {
                let p = c(central_meridian(zone) + 2.9, lat);
                assert!(format_utm(p).unwrap().starts_with(&format!("{zone}{}", if lat > 0.0 { "north " } else { "south " })));
                assert_readout_round_trips(p);
            }
        }
        // Southern Norway's 32V and Svalbard's wide zones, also within a rounded metre of the
        // parallels where the zones change (56°, 64° and 72° N), where the place read back can lie
        // in the neighbouring band.
        for p in [c(3.5, 60.0), c(11.9, 63.9), c(2.9, 60.0), c(8.9, 78.0), c(9.1, 78.0), c(41.9, 80.0)] {
            assert_readout_round_trips(p);
        }
        for edge in [56.0, 64.0, 72.0] {
            for off in [-1e-5, -1e-6, -1e-7, 0.0, 1e-7, 1e-6, 1e-5] {
                for i in 0..=168 {
                    assert_readout_round_trips(c(f64::from(i) * 0.25, edge + off));
                }
            }
        }
        let mut r = Lcg(31);
        for _ in 0..20_000 {
            let lat = UTM_MIN_LAT + r.next() * (UTM_MAX_LAT - UTM_MIN_LAT - 1e-6);
            assert_readout_round_trips(c(-180.0 + r.next() * 360.0, lat));
        }
        // No band letter anywhere: a letter after the zone would read as a band or a hemisphere.
        for p in [c(44.4, 33.3), c(-58.4, -34.6)] {
            let text = format_utm(p).unwrap();
            let zone_end = text.find(|ch: char| !ch.is_ascii_digit()).unwrap();
            assert!(text[zone_end..].starts_with("north ") || text[zone_end..].starts_with("south "), "{text}");
        }
        // Where UTM has no zone the readout still writes nothing, and the hemisphere form of a
        // polar position is refused.
        for p in [c(0.0, 84.0), c(0.0, -80.000_1), c(0.0, 90.0)] {
            assert!(format_utm(p).is_none(), "{p:?}");
        }
        let (e, n) = to_utm_in_zone(c(15.0, 84.05), 33, true).unwrap();
        assert_eq!(parse_utm(&format!("33north {} {}", e.round(), n.round())), Err(GridError::Polar));
        let (e, n) = to_utm_in_zone(c(15.0, -80.05), 33, false).unwrap();
        assert_eq!(parse_utm(&format!("33south {} {}", e.round(), n.round())), Err(GridError::Polar));
    }

    /// The single letter `S` after a zone is still band S when typed: the readout's spelt-out
    /// hemisphere doesn't loosen the refusal of a northing outside band S.
    #[test]
    fn a_typed_s_stays_band_s() {
        let s = to_utm(c(-58.4, -34.6)).unwrap();
        let (e, n) = (s.easting.round(), s.northing.round());
        assert_eq!(parse_utm(&format!("21S {e} {n}")), Err(GridError::Ambiguous));
        assert_eq!(parse_utm(&format!("21s {e} {n}")), Err(GridError::Ambiguous));
        assert!(parse_utm(&format!("21south {e} {n}")).is_ok_and(|q| (q.y + 34.6).abs() < 1e-5));
        // An abbreviation of a hemisphere other than the single letter isn't read.
        assert_eq!(parse_utm(&format!("21sou {e} {n}")), Err(GridError::Syntax));
    }

    #[test]
    fn invalid_utm_is_refused() {
        for (t, e) in [
            ("", GridError::Syntax),
            ("33U", GridError::Syntax),
            ("33U 412345", GridError::Syntax),
            ("33U 412345 5523456 1", GridError::Syntax),
            ("33U -412345 5523456", GridError::Syntax),
            ("33U 4e5 5523456", GridError::Syntax),
            ("33U inf 5523456", GridError::Syntax),
            ("33U 412345m 5523456", GridError::Syntax),
            ("33 east 412345 5523456", GridError::Syntax),
            ("0U 412345 5523456", GridError::Zone),
            ("61U 412345 5523456", GridError::Zone),
            ("75 Main St", GridError::Syntax),
            ("64 Bit 32 16", GridError::Syntax),
            ("33I 412345 5523456", GridError::Band),
            ("33Z 412345 5523456", GridError::Polar),
            // 5,523 km north is 49.8° N: not band T (40°–48° N).
            ("33T 412345 5523456", GridError::BandMismatch),
            // Band S with a northing outside it could be the southern hemisphere.
            ("33S 412345 5523456", GridError::Ambiguous),
            ("33U 1000000 5523456", GridError::OutsideZone),
            ("33U 412345 10000001", GridError::OutsideZone),
            // An easting of 20 km is 480 km west of the central meridian, 6.7° at 49.8° N:
            // beyond zone 33's western edge at 12° E.
            ("33U 20000 5523456", GridError::OutsideZone),
            // North of 84° N.
            ("33 north 500000 9500000", GridError::Polar),
        ] {
            assert_eq!(parse_utm(t), Err(e), "{t}");
        }
    }

    /// The band is the latitude's own letter: a place 0.2° (22 km) into band U written in band T
    /// is refused, by UTM and by MGRS alike, while the right band reads it back. Past 84° N a
    /// hemisphere-form position is polar, by however little.
    #[test]
    fn a_band_is_checked_to_the_cell_not_to_the_zone_edge() {
        let p = c(15.0, 48.2);
        let u = to_utm(p).unwrap();
        assert_eq!(u.band, 'U');
        let (e, n) = (u.easting.round(), u.northing.round());
        assert!(parse_utm(&format!("33U {e} {n}")).is_ok());
        assert_eq!(parse_utm(&format!("33T {e} {n}")), Err(GridError::BandMismatch));
        let m = to_mgrs(p, 5).unwrap().compact();
        assert!(parse_mgrs(&m).is_ok(), "{m}");
        let wrong = m.replacen("33U", "33T", 1);
        assert_eq!(parse_mgrs(&wrong), Err(GridError::BandMismatch), "{wrong}");
        // A 100 km square straddling the band edge is still read in either band it touches.
        let edge = to_mgrs(c(15.0, 48.0005), 0).unwrap().compact();
        assert!(parse_mgrs(&edge).is_ok(), "{edge}");
        // Just past 84° N, in the hemisphere form: polar.
        let (e, n) = to_utm_in_zone(c(15.0, 84.05), 33, true).unwrap();
        assert_eq!(parse_utm(&format!("33 north {} {}", e.round(), n.round())), Err(GridError::Polar));
        let (e, n) = to_utm_in_zone(c(15.0, 83.95), 33, true).unwrap();
        assert!(parse_utm(&format!("33 north {} {}", e.round(), n.round())).is_ok());
    }

    /// Zones 1–9 are written with two digits, in the readout and without spaces, and read back.
    #[test]
    fn one_digit_zones_are_written_with_two() {
        let m = to_mgrs(c(central_meridian(1), 0.5), 5).unwrap();
        assert_eq!(m.zone, 1);
        assert!(m.to_string().starts_with("01N EA "), "{m}");
        assert!(m.compact().starts_with("01NEA"), "{}", m.compact());
        for text in [m.to_string(), m.compact(), m.compact().replacen("01", "1", 1)] {
            assert_eq!(parse_mgrs(&text), Ok(m), "{text}");
        }
        assert_eq!(to_mgrs(c(central_meridian(9), 10.0), 0).unwrap().to_string(), "09P WM");
        assert_eq!(to_mgrs(c(central_meridian(10), 10.0), 0).unwrap().to_string(), "10P ES");
        // UTM keeps the zone as it is numbered (EPSG:32604), like GeographicLib's `4n` and
        // `4north`, and reads back.
        let text = format_utm(c(central_meridian(4), 20.0)).unwrap();
        assert!(text.starts_with("4north "), "{text}");
        assert!(parse_utm(&text).is_ok_and(|q| (q.x - central_meridian(4)).abs() < 1e-5 && (q.y - 20.0).abs() < 1e-5), "{text}");
        let text = format_utm(c(central_meridian(9), -20.0)).unwrap();
        assert!(text.starts_with("9south "), "{text}");
    }

    /// Just south of the equator the northing rounds to the full false northing (10,000 km, which
    /// is the equator, row A of band N): the reference stays in row V of band M and reads back.
    #[test]
    fn a_northing_just_south_of_the_equator_stays_in_band_m() {
        // A latitude a few 1e-15 degrees south: in band M, yet its northing rounds to 10,000 km.
        let p = (1..400)
            .map(|i| c(15.0, -1e-17 * f64::from(i) * 5.0))
            .find(|p| to_utm(*p).is_some_and(|u| u.band == 'M' && u.northing == FALSE_NORTHING_SOUTH))
            .expect("a place in band M whose northing rounds to the false northing");
        assert_eq!(to_utm(p).map(|u| u.zone), Some(33));
        for d in 0u8..=5 {
            let m = to_mgrs(p, d).unwrap();
            assert_eq!((m.band, m.row), ('M', 'V'), "{m}");
            assert_eq!(parse_mgrs(&m.compact()), Ok(m), "{m}");
        }
        assert_eq!(to_mgrs(p, 5).unwrap().northing, 99_999);
        // The equator itself and the north are untouched.
        assert_eq!(to_mgrs(c(15.0, 0.0), 5).unwrap().compact(), "33NWA0000000000");
    }

    /// A cell is read in a band only if it reaches into it. 38T MJ (4,300–4,400 km north, in zone
    /// 38's rows) ends at 39.7° N, short of band T's 40° N, while 38T MK crosses 40° N and is read
    /// in either band; 38S MJ lies in band S.
    #[test]
    fn a_square_must_reach_into_its_band() {
        assert_eq!(parse_mgrs("38TMJ"), Err(GridError::BandMismatch));
        assert_eq!(parse_mgrs("38TMJ 5 9"), Err(GridError::BandMismatch));
        for ok in ["38TMK", "38SMK", "38SMJ", "38TMK 5 2", "38SMK 5 2"] {
            assert!(parse_mgrs(ok).is_ok(), "{ok}");
        }
        // 40° N is about 4,428 km north on the central meridian, in 38S MK's third 10 km row:
        // the row above it isn't in band S, the row below it isn't in T.
        assert_eq!(parse_mgrs("38SMK 5 4"), Err(GridError::BandMismatch));
        assert_eq!(parse_mgrs("38TMK 5 1"), Err(GridError::BandMismatch));
        // The 100 km square holding 48° N in zone 33, wide of the central meridian where the
        // parallel bends south across the square: read in both bands.
        for p in [c(12.1, 48.0001), c(12.1, 47.9999), c(17.9, 48.0001), c(17.9, 47.9999)] {
            let m = to_mgrs(p, 0).unwrap();
            let other = if m.band == 'U' { 'T' } else { 'U' };
            let swapped = Mgrs { band: other, ..m };
            assert!(parse_mgrs(&m.compact()).is_ok(), "{m}");
            assert!(parse_mgrs(&swapped.compact()).is_ok(), "{swapped}");
        }
    }

    /// Every reference the readout writes reads back to itself, at every precision, at random and
    /// at the band edges (where a cell only partly in its band must still be read).
    #[test]
    fn every_precision_round_trips_at_band_edges() {
        let mut r = Lcg(23);
        let mut edges: Vec<f64> = (0..20).map(|i| UTM_MIN_LAT + 8.0 * f64::from(i)).collect();
        edges.push(UTM_MAX_LAT);
        for edge in edges {
            for off in [-0.3, -1e-4, -1e-7, 0.0, 1e-7, 1e-4, 0.3] {
                let lat = edge + off;
                if !(UTM_MIN_LAT..UTM_MAX_LAT).contains(&lat) {
                    continue;
                }
                for _ in 0..60 {
                    let lon = -180.0 + r.next() * 360.0;
                    for d in 0u8..=5 {
                        let m = to_mgrs(c(lon, lat), d).unwrap();
                        assert_eq!(parse_mgrs(&m.compact()), Ok(m), "{m} at {lon} {lat}");
                    }
                }
            }
        }
        for _ in 0..3_000 {
            let p = c(-180.0 + r.next() * 360.0, UTM_MIN_LAT + r.next() * (UTM_MAX_LAT - UTM_MIN_LAT - 1e-6));
            let d = (r.next() * 6.0) as u8;
            let m = to_mgrs(p, d).unwrap();
            assert_eq!(parse_mgrs(&m.to_string()), Ok(m), "{m} at {p:?}");
        }
    }

    /// Past 84° N or 80° S by more than the metre a position is rounded to, a UTM position is
    /// polar whether it is written with band X or C or with the hemisphere.
    #[test]
    fn utm_past_the_limits_is_polar_in_either_form() {
        let metres = |m: f64| m / M_PER_DEG;
        for (lat, band, north) in [(UTM_MAX_LAT + metres(1.5), 'X', true), (UTM_MIN_LAT - metres(1.5), 'C', false)] {
            let (e, n) = to_utm_in_zone(c(15.0, lat), 33, north).unwrap();
            let (e, n) = (e.round(), n.round());
            let hemi = if north { "north" } else { "south" };
            assert_eq!(parse_utm(&format!("33{band} {e} {n}")), Err(GridError::Polar), "{lat}");
            assert_eq!(parse_utm(&format!("33 {hemi} {e} {n}")), Err(GridError::Polar), "{lat}");
        }
        // Within the limits, by a metre, both forms read.
        for (lat, band, north) in [(UTM_MAX_LAT - metres(1.0), 'X', true), (UTM_MIN_LAT + metres(1.0), 'C', false)] {
            let (e, n) = to_utm_in_zone(c(15.0, lat), 33, north).unwrap();
            let (e, n) = (e.round(), n.round());
            let hemi = if north { "north" } else { "south" };
            assert!(parse_utm(&format!("33{band} {e} {n}")).is_ok(), "{lat}");
            assert!(parse_utm(&format!("33 {hemi} {e} {n}")).is_ok(), "{lat}");
        }
    }

    #[test]
    fn outside_utm_is_said_not_guessed() {
        for p in [c(0.0, 84.0), c(10.0, 89.9), c(0.0, 90.0), c(-60.0, -80.5), c(0.0, -90.0), c(f64::NAN, 1.0)] {
            assert!(to_utm(p).is_none(), "{p:?}");
            assert!(format_utm(p).is_none(), "{p:?}");
            assert!(format_mgrs(p).is_none(), "{p:?}");
        }
        assert!(from_utm(0, true, 500_000.0, 0.0).is_none());
        assert!(from_utm(61, true, 500_000.0, 0.0).is_none());
        assert!(from_utm(31, true, f64::NAN, 0.0).is_none());
        assert!(from_utm(31, true, 5e6, 0.0).is_none());
        assert!(from_utm(31, true, 500_000.0, 1e7).is_none());
        assert!(to_utm_in_zone(c(100.0, 0.0), 31, true).is_none());
        assert!(to_utm_in_zone(c(3.0, 91.0), 31, true).is_none());
    }
}
