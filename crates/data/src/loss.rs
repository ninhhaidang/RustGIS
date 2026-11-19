//! Shared classification for data-loss notices, independent of their wording.

/// What happened to the data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LossKind {
    Dropped,
    Changed,
    Approximated,
    Assumed,
}

impl LossKind {
    /// A language-independent name for this kind of loss.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Dropped => "dropped",
            Self::Changed => "changed",
            Self::Approximated => "approximated",
            Self::Assumed => "assumed",
        }
    }
}

/// The specific cause of a loss. Add causes here so producers use the same code for the same loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LossCode {
    DroppedLegend,
    DroppedRing,
    DroppedExtraAttributes,
    DroppedTime,
    DroppedValue,
    DroppedCalculationResult,
    ChangedTruncated,
}

/// A code's identifier, effect on the data, and what the user can do about it.
pub struct LossSpec {
    pub id: &'static str,
    pub kind: LossKind,
    pub action: &'static str,
}

impl LossCode {
    /// The registry entry that supplies the untranslated identifier and remediation.
    pub const fn spec(self) -> LossSpec {
        use LossKind::{Changed, Dropped};
        let (id, kind, action) = match self {
            Self::DroppedRing => ("dropped.ring", Dropped, "Correct the unreadable polygon ring in the source."),
            Self::DroppedExtraAttributes => {
                ("dropped.extra-attributes", Dropped, "Add fields for the extra attribute values or remove those values from the input.")
            }
            Self::DroppedLegend => ("dropped.legend", Dropped, "Use a service with short HTTP or HTTPS addresses for its legend pictures."),
            Self::DroppedTime => ("dropped.time", Dropped, "Write to a date-time field to keep the time of day."),
            Self::DroppedValue => ("dropped.value", Dropped, "Correct the value or use a destination field that can hold it."),
            Self::DroppedCalculationResult => {
                ("dropped.calculation-result", Dropped, "Correct the expression or check the input rows named in the notice.")
            }
            Self::ChangedTruncated => ("changed.truncated", Changed, "Use a decimal field to keep the fractional part."),
        };
        LossSpec { id, kind, action }
    }
}

/// A message produced at the point a reader or tool detects a loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossMessage {
    pub code: LossCode,
    pub text: String,
}
