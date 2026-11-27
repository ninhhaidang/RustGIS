//! Text options: alignment, font, case, spacing and the usual sizes.

use super::*;

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

pub(super) fn is_sans(f: &FontFamily) -> bool {
    *f == FontFamily::Sans
}

/// The most letter spacing a text element takes, either way (points).
pub const MAX_LETTER_SPACING: f64 = 50.0;
pub const MIN_LETTER_SPACING: f64 = -5.0;

pub(super) fn is_left(a: &TextAlign) -> bool {
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
pub(super) fn is_top(v: &TextVAlign) -> bool {
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
pub(super) fn is_no_case(v: &TextCase) -> bool {
    v.kept() == TextCase::None
}

/// The usual spacing between a text's lines, as a multiple of it.
pub const LINE_SPACING: f64 = 1.0;
/// The tightest and widest a text's line spacing may be, as multiples of the usual.
pub const MIN_LINE_SPACING: f64 = 0.5;
pub const MAX_LINE_SPACING: f64 = 3.0;

pub(super) fn line_spacing() -> f64 {
    LINE_SPACING
}

/// Whether a save leaves a text's line spacing out: the usual, or one not finite, which
/// [`line_spacing_in_range`] reads as the usual.
pub(super) fn is_line_spacing(v: &f64) -> bool {
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
pub(super) fn de_line_spacing<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v: Option<serde_json::Value> = Deserialize::deserialize(d)?;
    Ok(match v {
        None | Some(serde_json::Value::Null) => LINE_SPACING,
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(_) => f64::NAN,
    })
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

pub(super) fn legend_text_pt() -> f64 {
    LEGEND_TEXT_PT
}

pub(super) fn is_legend_text_pt(size: &f64) -> bool {
    *size == LEGEND_TEXT_PT
}

pub(super) fn scale_bar_text_pt() -> f64 {
    SCALE_BAR_TEXT_PT
}

pub(super) fn is_scale_bar_text_pt(size: &f64) -> bool {
    *size == SCALE_BAR_TEXT_PT
}

pub(super) fn grid_label_pt() -> f64 {
    GRID_LABEL_PT
}

pub(super) fn is_grid_label_pt(size: &f64) -> bool {
    *size == GRID_LABEL_PT
}

/// The most characters a layout text holds: a longer one is cut when a layout is read.
pub const MAX_TEXT_CHARS: usize = 5000;
