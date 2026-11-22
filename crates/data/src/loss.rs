//! Shared classification for data-loss notices, independent of their wording.
//!
//! A code's identifier is public: scripts match on it (`rustgis-cli run --strict --allow`), so it
//! is never translated, renamed or reused. The command line guide lists every code, checked by
//! `the_guide_lists_every_code`.

/// What happened to the data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LossKind {
    Dropped,
    Changed,
    Approximated,
    Assumed,
}

impl LossKind {
    /// Every kind, in the order the guide lists them.
    pub const ALL: [LossKind; 4] = [Self::Dropped, Self::Changed, Self::Approximated, Self::Assumed];

    /// A language-independent name for this kind of loss.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Dropped => "dropped",
            Self::Changed => "changed",
            Self::Approximated => "approximated",
            Self::Assumed => "assumed",
        }
    }

    /// The kind this identifier names.
    pub fn from_id(id: &str) -> Option<LossKind> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }
}

/// Declares the codes once, so the list of every code can't miss one.
macro_rules! loss_codes {
    ($($code:ident => ($id:literal, $kind:ident, $action:literal),)*) => {
        /// The specific cause of a loss. Add causes here so producers use the same code for the
        /// same loss.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum LossCode {
            $($code,)*
        }

        impl LossCode {
            /// Every code, in the order they are declared.
            pub const ALL: &'static [LossCode] = &[$(Self::$code,)*];

            /// The registry entry that supplies the untranslated identifier and remediation.
            pub const fn spec(self) -> LossSpec {
                match self {
                    $(Self::$code => LossSpec { id: $id, kind: LossKind::$kind, action: $action },)*
                }
            }
        }
    };
}

loss_codes! {
    DroppedLegend => ("dropped.legend", Dropped, "Use a service with short HTTP or HTTPS addresses for its legend pictures."),
    DroppedRing => ("dropped.ring", Dropped, "Correct the unreadable polygon ring in the source."),
    DroppedExtraAttributes => (
        "dropped.extra-attributes",
        Dropped,
        "Add fields for the extra attribute values or remove those values from the input."
    ),
    DroppedTime => ("dropped.time", Dropped, "Write to a date-time field to keep the time of day."),
    DroppedValue => ("dropped.value", Dropped, "Correct the value or use a destination field that can hold it."),
    DroppedCalculationResult => (
        "dropped.calculation-result",
        Dropped,
        "Correct the expression or check the input rows named in the notice."
    ),
    ChangedTruncated => ("changed.truncated", Changed, "Use a decimal field to keep the fractional part."),
}

impl LossCode {
    /// The code this identifier names.
    pub fn from_id(id: &str) -> Option<LossCode> {
        Self::ALL.iter().copied().find(|code| code.spec().id == id)
    }
}

/// A code's identifier, effect on the data, and what the user can do about it.
pub struct LossSpec {
    pub id: &'static str,
    pub kind: LossKind,
    pub action: &'static str,
}

/// A message produced at the point a reader or tool detects a loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossMessage {
    pub code: LossCode,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_named_after_their_kind() {
        for (i, code) in LossCode::ALL.iter().enumerate() {
            let spec = code.spec();
            assert!(spec.id.starts_with(&format!("{}.", spec.kind.id())), "{} is not under {}", spec.id, spec.kind.id());
            assert!(LossCode::ALL[..i].iter().all(|other| other.spec().id != spec.id), "{} is declared twice", spec.id);
            assert_eq!(LossCode::from_id(spec.id), Some(*code));
        }
        assert_eq!(LossCode::from_id("dropped"), None);
        assert_eq!(LossKind::from_id("assumed"), Some(LossKind::Assumed));
        assert_eq!(LossKind::from_id("dropped.ring"), None);
    }

    #[test]
    fn the_guide_lists_every_code() {
        let guide = include_str!("../../../docs/cli.md").replace("\r\n", "\n");
        let section = guide.split("\n### Loss codes\n").nth(1).unwrap_or_default();
        let section = section.split("\n#").next().unwrap_or_default();
        let rows: Vec<&str> = section.lines().filter(|l| l.starts_with("| `")).collect();
        for code in LossCode::ALL {
            let spec = code.spec();
            let row = format!("| `{}` | {} |", spec.id, spec.action);
            assert!(rows.contains(&row.as_str()), "docs/cli.md: the loss codes should list\n{row}");
        }
        for row in rows {
            let id = row.trim_start_matches("| `").split('`').next().unwrap_or_default();
            assert!(LossCode::from_id(id).is_some(), "docs/cli.md lists `{id}`, which is not a loss code");
        }
    }
}
