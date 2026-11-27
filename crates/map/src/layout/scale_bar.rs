//! The scale bar's options: units, style, divisions and labels.

use super::*;

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

pub(super) fn divisions() -> usize {
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

/// What a file's scale bar decimals that are no count at all (negative, fractional, not a number)
/// read as: read from a file only, never kept. `Layout::clamp` sends it back to automatic and
/// counts it.
pub(super) const SCALE_DECIMALS_INVALID: u8 = u8::MAX;

/// A scale bar's decimals as a file holds them, of whatever JSON kind: null or the word "auto"
/// (any case, trimmed) as automatic, a whole number from 0 to 253 as it is, a whole number past
/// that as 254 (more than any count, so `Layout::clamp` cuts it to the most), and anything else
/// (negative, fractional, another word, a flag, a list, an object) as
/// [`SCALE_DECIMALS_INVALID`]. So `Layout::clamp` brings every invalid value into range and says
/// so, rather than the whole layout failing to load or a bad value passing as a good one.
pub(super) fn de_scale_decimals<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u8>, D::Error> {
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
