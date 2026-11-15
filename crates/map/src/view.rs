//! View settings: post-processing effects (illumination, weather, visual and camera effects,
//! colour adjustments), colour vision simulation, scene drawing mode, view clipping, display
//! quality, and on-screen navigation aids.

use serde::{Deserialize, Serialize};

/// Weather drawn over the map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Weather {
    #[default]
    None,
    Haze,
    Fog,
    Rain,
    Snow,
}

/// Simulated colour vision deficiency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorVision {
    #[default]
    Normal,
    Protanopia,
    Deuteranopia,
    Tritanopia,
    Achromatopsia,
}

impl ColorVision {
    pub const ALL: [ColorVision; 5] =
        [ColorVision::Normal, ColorVision::Protanopia, ColorVision::Deuteranopia, ColorVision::Tritanopia, ColorVision::Achromatopsia];
    pub fn code(self) -> &'static str {
        match self {
            ColorVision::Normal => "normal",
            ColorVision::Protanopia => "protanopia",
            ColorVision::Deuteranopia => "deuteranopia",
            ColorVision::Tritanopia => "tritanopia",
            ColorVision::Achromatopsia => "achromatopsia",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ColorVision::Normal => "Normal vision",
            ColorVision::Protanopia => "Protanopia (no red)",
            ColorVision::Deuteranopia => "Deuteranopia (no green)",
            ColorVision::Tritanopia => "Tritanopia (no blue)",
            ColorVision::Achromatopsia => "Achromatopsia (no colour)",
        }
    }
    pub fn from_code(s: &str) -> Option<ColorVision> {
        ColorVision::ALL.into_iter().find(|c| c.code().eq_ignore_ascii_case(s.trim()))
    }
    /// The colour matrix (linear RGB) simulating it.
    pub fn matrix(self) -> Option<[[f32; 3]; 3]> {
        // Machado, Oliveira and Fernandes (2009), severity 1.0.
        match self {
            ColorVision::Normal => None,
            ColorVision::Protanopia => {
                Some([[0.152_286, 1.052_583, -0.204_868], [0.114_503, 0.786_281, 0.099_216], [-0.003_882, -0.048_116, 1.051_998]])
            }
            ColorVision::Deuteranopia => {
                Some([[0.367_322, 0.860_646, -0.227_968], [0.280_085, 0.672_501, 0.047_413], [-0.011_820, 0.042_940, 0.968_881]])
            }
            ColorVision::Tritanopia => {
                Some([[1.255_528, -0.076_749, -0.178_779], [-0.078_411, 0.930_809, 0.147_602], [0.004_733, 0.691_367, 0.303_900]])
            }
            ColorVision::Achromatopsia => Some([[0.2126, 0.7152, 0.0722], [0.2126, 0.7152, 0.0722], [0.2126, 0.7152, 0.0722]]),
        }
    }
}

/// Effects applied to the drawn map.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewEffects {
    /// Stars in the space around the globe.
    #[serde(default = "yes")]
    pub stars: bool,
    /// Daylight and night shading for this time (seconds since 1970, UTC), when set.
    #[serde(default)]
    pub sun_time: Option<f64>,
    #[serde(default)]
    pub weather: Weather,
    /// Darkened corners.
    #[serde(default)]
    pub vignette: bool,
    /// Outlines of shapes drawn over the map (an edge filter).
    #[serde(default)]
    pub edges: bool,
    /// Blur toward the top and bottom of the view, as a miniature (tilt-shift) camera.
    #[serde(default)]
    pub tilt_shift: bool,
    /// Film grain.
    #[serde(default)]
    pub grain: bool,
    /// −100 to 100.
    #[serde(default)]
    pub brightness: f64,
    #[serde(default)]
    pub contrast: f64,
    #[serde(default)]
    pub saturation: f64,
    /// 0 to 100.
    #[serde(default)]
    pub sepia: f64,
}

fn yes() -> bool {
    true
}

impl Default for ViewEffects {
    fn default() -> Self {
        ViewEffects {
            stars: true,
            sun_time: None,
            weather: Weather::None,
            vignette: false,
            edges: false,
            tilt_shift: false,
            grain: false,
            brightness: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            sepia: 0.0,
        }
    }
}

impl ViewEffects {
    /// No effect to apply after drawing the map (the stars are part of drawing it).
    pub fn is_none(&self) -> bool {
        ViewEffects { stars: true, ..self.clone() } == ViewEffects::default()
    }
}

/// Where a map's terrain comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TerrainSource {
    /// A raster layer of the map holding heights in metres (a DEM), by name.
    Layer(String),
    /// Elevation tiles: an XYZ URL template whose PNG or WebP tiles encode heights in their
    /// colours.
    Tiles { url: String, encoding: TerrainEncoding },
}

/// How elevation tiles encode heights in a pixel's red, green and blue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerrainEncoding {
    /// Mapzen/AWS Terrarium: `R × 256 + G + B / 256 − 32768`.
    #[default]
    Terrarium,
    /// Mapbox Terrain-RGB (also MapTiler's): `−10000 + (R × 65536 + G × 256 + B) × 0.1`.
    Mapbox,
}

impl TerrainEncoding {
    pub fn code(self) -> &'static str {
        match self {
            TerrainEncoding::Terrarium => "terrarium",
            TerrainEncoding::Mapbox => "mapbox",
        }
    }

    pub fn from_code(s: &str) -> Option<TerrainEncoding> {
        match s.trim().to_ascii_lowercase().as_str() {
            "terrarium" => Some(TerrainEncoding::Terrarium),
            "mapbox" | "terrain-rgb" | "terrainrgb" | "maptiler" => Some(TerrainEncoding::Mapbox),
            _ => None,
        }
    }

    /// The height (metres) a pixel's red, green and blue encode.
    pub fn height(self, r: u8, g: u8, b: u8) -> f64 {
        let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
        match self {
            TerrainEncoding::Terrarium => r * 256.0 + g + b / 256.0 - 32768.0,
            TerrainEncoding::Mapbox => -10000.0 + (r * 65536.0 + g * 256.0 + b) * 0.1,
        }
    }
}

/// A map's terrain: where its heights come from and how much they are exaggerated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Terrain {
    pub source: TerrainSource,
    #[serde(default = "one")]
    pub exaggeration: f64,
    /// Water bodies drawn flat at their level (#1915). None: the ground's heights everywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub water: Option<TerrainWater>,
}

fn one() -> f64 {
    1.0
}

/// A terrain's water (#1915): inside each polygon of a layer the ground is flat, at the lowest
/// height along the polygon's shore or at the value of a field, instead of the DEM's heights
/// (a surface model is often noisy or stepped over water).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainWater {
    /// The polygon layer of water bodies, by name (as the terrain's DEM).
    pub layer: String,
    /// A numeric field holding each water body's level (metres); a row without one takes its
    /// shore's lowest height. None: every water body's shore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

/// The most a terrain's heights are exaggerated.
pub const MAX_EXAGGERATION: f64 = 20.0;

/// How the map draws.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewSettings {
    #[serde(default)]
    pub effects: ViewEffects,
    /// Session-only (like the next few): how the screen shows the map, not part of the project.
    #[serde(skip)]
    pub color_vision: ColorVision,
    /// Polygons as outlines only (the scene's wireframe drawing mode).
    #[serde(default)]
    pub wireframe: bool,
    /// Features farther than this many metres from the view's centre aren't drawn, when set.
    #[serde(default)]
    pub clip_distance: Option<f64>,
    /// The ground's relief in a tilted view, when set: heights from a DEM, which the map is
    /// draped over (as MapLibre's terrain).
    #[serde(default)]
    pub terrain: Option<Terrain>,
    /// Speed over quality: no antialiasing.
    #[serde(skip)]
    pub fast: bool,
    /// The on-screen navigator (compass, zoom slider and pan pad).
    #[serde(skip)]
    pub navigator: bool,
    /// Keyboard (WASD) walking navigation instead of mouse panning. Session-only: a reopened
    /// project starts with ordinary navigation, so the keys aren't captured unexpectedly.
    #[serde(skip)]
    pub first_person: bool,
}

/// An animation keyframe: a view, and the seconds taken to reach it from the previous one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub center: rustgis_geom::Coord,
    pub scale: f64,
    #[serde(default = "two")]
    pub seconds: f64,
}

fn two() -> f64 {
    2.0
}
