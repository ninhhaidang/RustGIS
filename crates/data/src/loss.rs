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
    DroppedZm => (
        "dropped.zm",
        Dropped,
        "Keep the source file where heights or measures matter: RustGIS keeps x and y only."
    ),
    DroppedCrsName => (
        "dropped.crs-name",
        Dropped,
        "Keep the source file's coordinate system definition with the export."
    ),
    ChangedTruncated => ("changed.truncated", Changed, "Use a decimal field to keep the fractional part."),
    ChangedBody => ("changed.body", Changed, "Export in the layer's own coordinate system, which keeps its body."),
    ChangedPosition => (
        "changed.position",
        Changed,
        "Export in a coordinate system that holds the positions, such as WGS 84."
    ),
    ApproximatedDatum => (
        "approximated.datum",
        Approximated,
        "Export in the layer's own coordinate system, or give that system with a known transformation to WGS 84."
    ),
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

/// Something a reader, writer or tool says, and the loss it reports, if any: the code travels
/// with the text wherever the text goes, to the notice that says it.
///
/// It has no `Display` and turns into no `String`, so a code isn't dropped by formatting the note
/// or passing it on as text: [`Note::prefixed`] and [`Note::map_text`] keep it. The words alone
/// ([`Note::text`], [`Note::as_str`], [`Notes::texts`]) are for sinks that show or store text
/// only, such as a manifest or a refusal's message; a note read through them loses its code.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Note {
    pub text: String,
    pub loss: Option<LossCode>,
}

impl Note {
    /// A note that reports a loss.
    pub fn loss(code: LossCode, text: impl Into<String>) -> Self {
        Self { text: text.into(), loss: Some(code) }
    }

    /// A note that reports no loss.
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), loss: None }
    }

    /// The same note, its text after `prefix: ` (the layer or file it is about).
    pub fn prefixed(self, prefix: &str) -> Self {
        Self { text: format!("{prefix}: {}", self.text), loss: self.loss }
    }

    /// The same note, its text changed by `f` (a qualifier added).
    pub fn map_text(self, f: impl FnOnce(String) -> String) -> Self {
        Self { text: f(self.text), loss: self.loss }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn contains(&self, pattern: &str) -> bool {
        self.text.contains(pattern)
    }

    pub fn starts_with(&self, pattern: &str) -> bool {
        self.text.starts_with(pattern)
    }

    pub fn ends_with(&self, pattern: &str) -> bool {
        self.text.ends_with(pattern)
    }
}

impl From<String> for Note {
    fn from(text: String) -> Self {
        Self::plain(text)
    }
}

impl From<&str> for Note {
    fn from(text: &str) -> Self {
        Self::plain(text)
    }
}

impl From<&String> for Note {
    fn from(text: &String) -> Self {
        Self::plain(text.as_str())
    }
}

impl From<LossMessage> for Note {
    fn from(loss: LossMessage) -> Self {
        Self::loss(loss.code, loss.text)
    }
}

/// A plain note equals its words; a note that reports a loss equals no plain text.
impl PartialEq<str> for Note {
    fn eq(&self, other: &str) -> bool {
        self.loss.is_none() && self.text == other
    }
}

impl PartialEq<&str> for Note {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl PartialEq<String> for Note {
    fn eq(&self, other: &String) -> bool {
        self == other.as_str()
    }
}

/// What a reader, a source or a write says, in order: plain text pushed onto it stays plain, and
/// a [`Note::loss`] keeps its code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notes(Vec<Note>);

impl Notes {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn push(&mut self, note: impl Into<Note>) {
        self.0.push(note.into());
    }

    pub fn insert(&mut self, at: usize, note: impl Into<Note>) {
        self.0.insert(at, note.into());
    }

    pub fn retain(&mut self, keep: impl FnMut(&Note) -> bool) {
        self.0.retain(keep);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }

    pub fn drain(&mut self, range: impl std::ops::RangeBounds<usize>) -> std::vec::Drain<'_, Note> {
        self.0.drain(range)
    }

    pub fn dedup(&mut self) {
        self.0.dedup();
    }

    pub fn pop(&mut self) -> Option<Note> {
        self.0.pop()
    }

    /// Their words alone, without their codes: for a sink that shows or stores text only.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(Note::as_str)
    }

    /// Whether one of them has the text `text`, whatever its code: a match by wording alone,
    /// where `==` with a string matches a plain note only.
    pub fn says(&self, text: &str) -> bool {
        self.0.iter().any(|n| n.text == text)
    }

    /// Each, its text after `prefix: `.
    pub fn prefixed(self, prefix: &str) -> Self {
        Self(self.0.into_iter().map(|n| n.prefixed(prefix)).collect())
    }

    pub fn into_vec(self) -> Vec<Note> {
        self.0
    }
}

impl std::ops::Deref for Notes {
    type Target = [Note];
    fn deref(&self) -> &[Note] {
        &self.0
    }
}

impl<N: Into<Note>> Extend<N> for Notes {
    fn extend<I: IntoIterator<Item = N>>(&mut self, notes: I) {
        self.0.extend(notes.into_iter().map(Into::into));
    }
}

impl<N: Into<Note>> FromIterator<N> for Notes {
    fn from_iter<I: IntoIterator<Item = N>>(notes: I) -> Self {
        Self(notes.into_iter().map(Into::into).collect())
    }
}

impl<N: Into<Note>> From<Vec<N>> for Notes {
    fn from(notes: Vec<N>) -> Self {
        notes.into_iter().collect()
    }
}

impl IntoIterator for Notes {
    type Item = Note;
    type IntoIter = std::vec::IntoIter<Note>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Notes {
    type Item = &'a Note;
    type IntoIter = std::slice::Iter<'a, Note>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<S, const N: usize> PartialEq<[S; N]> for Notes
where
    Note: PartialEq<S>,
{
    fn eq(&self, other: &[S; N]) -> bool {
        self.0.len() == N && self.0.iter().zip(other).all(|(a, b)| a == b)
    }
}

impl<S> PartialEq<&[S]> for Notes
where
    Note: PartialEq<S>,
{
    fn eq(&self, other: &&[S]) -> bool {
        self.0.len() == other.len() && self.0.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl<S> PartialEq<Vec<S>> for Notes
where
    Note: PartialEq<S>,
{
    fn eq(&self, other: &Vec<S>) -> bool {
        self.0.len() == other.len() && self.0.iter().zip(other).all(|(a, b)| a == b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain note equals its words and a coded one no plain words; prefixing, rewording,
    /// collecting and extending keep a code; `says` matches by wording alone.
    #[test]
    fn notes_keep_their_codes() {
        let plain = Note::plain("3 rows left out");
        let coded = Note::loss(LossCode::DroppedRing, "1 ring left out");
        assert_eq!(plain, "3 rows left out");
        assert_ne!(coded, "1 ring left out");
        assert_eq!(coded.clone().prefixed("a.rss"), Note::loss(LossCode::DroppedRing, "a.rss: 1 ring left out"));
        assert_eq!(coded.clone().map_text(|t| format!("{t}, in the view")).loss, Some(LossCode::DroppedRing));
        let mut notes: Notes = ["first"].into_iter().collect();
        notes.push(coded.clone());
        notes.extend([plain.clone()]);
        assert_eq!(notes.iter().map(|n| n.loss).collect::<Vec<_>>(), [None, Some(LossCode::DroppedRing), None]);
        assert!(notes.says("1 ring left out") && !notes.says("1 ring"));
        assert_eq!(notes.clone().prefixed("x")[1], Note::loss(LossCode::DroppedRing, "x: 1 ring left out"));
        assert_ne!(notes, ["first", "1 ring left out", "3 rows left out"]);
        assert_eq!(Notes::from(vec![String::from("a")]), ["a"]);
    }

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
