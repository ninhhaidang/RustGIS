//! The north arrow's look.

use super::*;

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
