//! What the map's graticule draws (#1042): latitudes and longitudes, a measured grid of eastings
//! and northings in one projected system, the UTM zones with each zone's own grid, or the MGRS
//! grid with its grid zone designations and 100 km squares named.

use serde::{Deserialize, Serialize};

/// The lines of the map's graticule (View › Display › Graticule).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GraticuleType {
    /// Latitudes and longitudes.
    #[default]
    Degrees,
    /// Eastings and northings in one projected system ([`crate::Map::graticule_crs`]): none is
    /// the map's own when it is projected, else the UTM zone (WGS 84) at the view's centre.
    Measured,
    /// The UTM zones (WGS 84) and their latitude bands, with the Norway and Svalbard exceptions,
    /// and in each zone its own eastings and northings once few zones are in view.
    Utm,
    /// The MGRS grid (NGA.STND.0037): the UTM zones and bands as grid zone designations, and in
    /// each of a few in view its own grid at a power of ten that suits the scale (100 km to
    /// 100 m), the 100 km lines heavier, named by grid zone designation or 100 km square. Nothing
    /// is drawn in the polar areas (UPS) yet.
    Mgrs,
}

impl GraticuleType {
    pub const ALL: [GraticuleType; 4] = [GraticuleType::Degrees, GraticuleType::Measured, GraticuleType::Utm, GraticuleType::Mgrs];

    /// The name a command takes.
    pub fn code(self) -> &'static str {
        match self {
            GraticuleType::Degrees => "degrees",
            GraticuleType::Measured => "measured",
            GraticuleType::Utm => "utm",
            GraticuleType::Mgrs => "mgrs",
        }
    }

    /// The name the interface shows.
    pub fn label(self) -> &'static str {
        match self {
            GraticuleType::Degrees => "Latitude and Longitude",
            GraticuleType::Measured => "Measured Grid",
            GraticuleType::Utm => "UTM Zones",
            GraticuleType::Mgrs => "MGRS",
        }
    }

    /// The type named `s` ([`code`](GraticuleType::code), in any case).
    pub fn from_name(s: &str) -> Option<GraticuleType> {
        GraticuleType::ALL.into_iter().find(|t| t.code().eq_ignore_ascii_case(s.trim()))
    }

    pub(crate) fn is_degrees(&self) -> bool {
        *self == GraticuleType::Degrees
    }
}

/// A measured graticule's coordinate system as a message or menu shows it: its first line, cut at
/// 60 characters (a WKT can run to thousands), with "…" where it was cut.
pub fn short_crs(crs: &str) -> String {
    let crs = crs.trim();
    let cut: String = crs.lines().next().unwrap_or_default().chars().take(60).collect();
    if cut.len() < crs.len() { format!("{cut}…") } else { cut }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graticule_types_are_named_in_any_case() {
        for t in GraticuleType::ALL {
            assert_eq!(GraticuleType::from_name(&t.code().to_uppercase()), Some(t));
        }
        assert_eq!(GraticuleType::from_name(" utm "), Some(GraticuleType::Utm));
        assert_eq!(GraticuleType::from_name("Mgrs"), Some(GraticuleType::Mgrs));
        assert_eq!(GraticuleType::from_name("ups"), None);
    }

    #[test]
    fn a_long_or_multi_line_system_is_shown_cut() {
        assert_eq!(short_crs(" EPSG:2056 "), "EPSG:2056");
        assert_eq!(short_crs("PROJCS[\"x\",\n  GEOGCS[]]"), "PROJCS[\"x\",…");
        assert_eq!(short_crs(&"x".repeat(100)).chars().count(), 61);
    }
}
