//! Lines: direction, ends and angle.

use super::*;

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
