//! Controlled, synchronous durable output. Preparation owns every replacement before the first
//! mutation. The edit list is also the rollback journal; applying and undoing it only swaps
//! disjoint whole fields at fixed locations. No observer, policy planner or callback may see the
//! temporary projection. Formatters are internal, borrow only the projection and never reenter.

use std::collections::{HashSet, hash_map::RandomState};
use std::hash::BuildHasher;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

/// Counted losses at the output boundary. Generated display text is cleaned without a notice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub(crate) addresses: u32,
    pub(crate) videos: u32,
    pub(crate) steps: u32,
    pub(crate) lines: u32,
    pub(crate) withheld: u32,
    pub(crate) unchecked: u32,
    pub(crate) not_remembered: bool,
    pub(crate) lost: u32,
    pub(crate) not_retained: u32,
    pub(crate) executable_changes: u32,
    pub(crate) reconnect: u32,
}

impl Report {
    /// The existing `project.save` result's `stripped` count, not a count of unique URLs or layers.
    /// Changed address fields, including streamed provenance and source paths, count separately.
    /// Explicit saves also add layers whose runtime login has no recorded connection to restore it.
    pub fn layer_addresses(&self) -> u32 {
        self.addresses
    }

    /// Addresses whose keys the receiving session lost during peer replacement.
    pub fn lost(&self) -> u32 {
        self.lost
    }

    /// One notice for each kind of loss, and none for a clean output.
    pub fn notices(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (n, text) in [
            (
                self.addresses,
                "layer address(es) were saved without their keys, tokens, passwords or URL fragments; after RustGIS closes, add those layers again to restore missing values",
            ),
            (self.videos, "video address(es) were written without their keys, tokens, passwords or URL fragments"),
            (self.steps, "executable step(s) were written without credentials or URL fragments; supply missing values again before replay"),
            (self.lines, "command line(s) were written without credentials or URL fragments; supply missing values again before replay"),
            (self.withheld, "credential-bearing slot(s) could not be read and were withheld"),
            (self.unchecked, "step(s) contain opaque parameters that may hold credentials and were kept unchecked"),
            (self.not_retained, "address(es) exceeded the bridge retention limit; their keys will not survive a round trip"),
            (self.lost, "address(es) lost their keys when a peer sent back its project; add them again in the receiving session"),
            (self.reconnect, "database source(s) from a peer require an explicit reconnect before reading"),
            (
                self.executable_changes,
                "credential-bearing executable field(s) changed when a peer sent back its project; supply missing values before replay",
            ),
        ] {
            if n > 0 {
                out.push(format!("{n} {text}"));
            }
        }
        if self.not_remembered {
            out.push("This output's credential restoration exceeds the memory limit; reopening it will not bring its keys back".into());
        }
        out
    }

    /// Other loss notices for a consumer that presents the layer-address count separately.
    /// Suppresses that kind explicitly, independent of the ordering of notices.
    pub fn notices_without_layer_addresses(&self) -> Vec<String> {
        let mut report = self.clone();
        report.addresses = 0;
        report.notices()
    }

    /// Adds the independently prepared losses of a nested typed document.
    pub(crate) fn merge(&mut self, other: &Self) {
        self.addresses = self.addresses.saturating_add(other.addresses);
        self.videos = self.videos.saturating_add(other.videos);
        self.steps = self.steps.saturating_add(other.steps);
        self.lines = self.lines.saturating_add(other.lines);
        self.withheld = self.withheld.saturating_add(other.withheld);
        self.unchecked = self.unchecked.saturating_add(other.unchecked);
        self.not_remembered |= other.not_remembered;
        self.lost = self.lost.saturating_add(other.lost);
        self.not_retained = self.not_retained.saturating_add(other.not_retained);
        self.executable_changes = self.executable_changes.saturating_add(other.executable_changes);
        self.reconnect = self.reconnect.saturating_add(other.reconnect);
    }
}

/// A child owner in a typed location, independent of any field's current contents.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Owner {
    Map(usize),
    Area(usize),
    Table(usize),
    Task(usize),
    Model(usize),
    History(usize),
    Notebook(usize),
    Presentation(usize),
    Graph,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Leaf {
    Map(rustgis_map::SlotPos),
    Layer(rustgis_map::LayerRole),
    Table,
    Task(usize),
    TaskCommand(usize),
    ModelStep(usize),
    ModelStepTool(usize),
    ModelNode(usize),
    ModelNodeTool(usize),
    HistoryParams,
    HistoryId,
    HistoryCommand,
    HistoryCommandId,
    Message(usize),
    CellSource(usize),
    CellOutput(usize),
    Video(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct At {
    pub(crate) owners: Vec<Owner>,
    pub(crate) leaf: Leaf,
}

#[derive(Clone, Debug, PartialEq)]
enum Payload {
    Text(String),
    Json(Value),
}

pub(crate) enum FieldMut<'a> {
    Text(&'a mut String),
    Json(&'a mut Value),
}

impl FieldMut<'_> {
    /// Type-checks before swapping; after the swap the caller immediately advances its journal.
    fn swap(self, held: &mut Payload) -> bool {
        match (self, held) {
            (Self::Text(field), Payload::Text(value)) => std::mem::swap(field, value),
            (Self::Json(field), Payload::Json(value)) => std::mem::swap(field, value),
            _ => return false,
        }
        true
    }

    /// Clones only one changed owning field, after the live projection has been rolled back.
    fn payload(&self) -> Payload {
        match self {
            Self::Text(v) => Payload::Text((**v).clone()),
            Self::Json(v) => Payload::Json((**v).clone()),
        }
    }

    /// Compares the actual parsed field to the recorded written value without policy dispatch.
    fn equals(&self, value: &Payload) -> bool {
        match (self, value) {
            (Self::Text(a), Payload::Text(b)) => **a == *b,
            (Self::Json(a), Payload::Json(b)) => **a == *b,
            _ => false,
        }
    }

    /// Counts retained text/JSON bytes before copying into the bounded reopen memory.
    fn bytes(&self) -> usize {
        match self {
            Self::Text(v) => v.len(),
            Self::Json(v) => json_bytes(v),
        }
    }
}

/// Counts payload without allocating a serialized copy, including JSON punctuation/escaping.
fn json_bytes(v: &Value) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        /// Counts the serializer's bytes with saturating arithmetic, retaining no payload.
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(b.len());
            Ok(b.len())
        }
        /// The counting writer has no buffered bytes to flush.
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Counter(0);
    if serde_json::to_writer(&mut c, v).is_err() {
        return usize::MAX;
    }
    c.0
}

pub(crate) trait Durable {
    /// Preparation is read-only, exhaustive over owning fields, and offers each leaf once.
    fn prepare(&self, p: &mut Prepare<'_>);
    /// Resolves recorded indices only, through plain owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Runtime,
    Credential,
    Path,
    Generated,
}
struct Edit {
    at: At,
    held: Payload,
    kind: Kind,
}

pub(crate) struct Prepare<'a> {
    owners: Vec<Owner>,
    edits: Vec<Edit>,
    pub(crate) report: Report,
    relative_to: Option<&'a str>,
    pub(crate) depth: usize,
}

impl<'a> Prepare<'a> {
    /// Makes every replacement and journal allocation before the live borrow is mutated.
    fn new(relative_to: Option<&'a str>, depth: usize) -> Self {
        Self { owners: Vec::new(), edits: Vec::new(), report: Report::default(), relative_to, depth }
    }

    /// Prepares one child under a fixed, typed owning location.
    pub(crate) fn child(&mut self, owner: Owner, value: &impl Durable) {
        self.owners.push(owner);
        value.prepare(self);
        self.owners.pop();
    }

    /// Records a single replacement of a whole owning field, never a subfield of an edit.
    fn edit(&mut self, leaf: Leaf, held: Payload, kind: Kind) {
        self.edits.push(Edit { at: At { owners: self.owners.clone(), leaf }, held, kind });
    }

    /// Cleans an address and then makes its local path relative, in one journal entry.
    pub(crate) fn string(&mut self, leaf: Leaf, value: &str, address: bool, local_path: bool) {
        let mut replacement = None;
        let mut kind = Kind::Path;
        if address {
            replacement = match public_address(value) {
                Kept::Unchanged => None,
                Kept::Changed(s) => Some(s),
                Kept::Withheld(prefix) if prefix != value => {
                    self.report.withheld = self.report.withheld.saturating_add(1);
                    Some(prefix.into())
                }
                Kept::Withheld(_) => None,
            };
            if replacement.is_some() {
                kind = Kind::Credential;
                let binding_only =
                    crate::project::without_login_binding(value).is_some_and(|public| matches!(public_address(&public), Kept::Unchanged));
                if binding_only {
                    kind = Kind::Runtime;
                }
                if !binding_only || matches!(leaf, Leaf::Video(_)) {
                    if matches!(leaf, Leaf::Video(_)) {
                        self.report.videos = self.report.videos.saturating_add(1);
                    } else {
                        self.report.addresses = self.report.addresses.saturating_add(1);
                    }
                }
            }
        }
        let current = replacement.as_deref().unwrap_or(value);
        if local_path
            && let Some(dir) = self.relative_to
            && rustgis_map::paths::is_absolute_local(current)
            && let Some(relative) = rustgis_map::paths::relative_to(dir, current).filter(|r| rustgis_map::paths::is_local(r))
        {
            replacement = Some(relative);
        }
        if let Some(s) = replacement.filter(|s| s != value) {
            self.edit(leaf, Payload::Text(s), kind);
        }
    }

    /// Prepares both fields of one replayable command, counting the owning step once.
    pub(crate) fn command(&mut self, leaf: Leaf, id_leaf: Leaf, id: &str, value: &Value) {
        self.executable((leaf, id_leaf), id, value, crate::roles::command(id, value, self.depth));
    }

    /// Model and history tool params use the same common roles as `gp.run` params.
    pub(crate) fn tool(&mut self, leaf: Leaf, id_leaf: Leaf, id: &str, value: &Value) {
        self.executable((leaf, id_leaf), id, value, crate::roles::tool(id, value, self.depth));
    }

    /// Journals the identifier and params independently while reporting their shared owner.
    fn executable(&mut self, (leaf, id_leaf): (Leaf, Leaf), id: &str, value: &Value, (clean, report): (Option<Value>, Report)) {
        let clean = match clean {
            Some(clean) => Some(crate::project::public_login_params(&clean).unwrap_or(clean)),
            None => crate::project::public_login_params(value),
        };
        self.report.merge(&report);
        let clean_id = crate::roles::identifier(id, &mut self.report);
        if clean.is_some() || clean_id.is_some() {
            self.report.steps = self.report.steps.saturating_add(1);
        }
        if let Some(clean) = clean {
            self.edit(leaf, Payload::Json(clean), Kind::Credential);
        }
        if let Some(clean) = clean_id {
            self.edit(id_leaf, Payload::Text(clean), Kind::Credential);
        }
    }

    /// Rewrites only changed command lines; comments, data and foreign-language cells stay exact.
    pub(crate) fn script(&mut self, leaf: Leaf, source: &str) {
        let (clean, report) = crate::roles::script(source, self.depth);
        let clean = match clean {
            Some(clean) => Some(crate::project::without_recorded_login_bindings(&clean).unwrap_or(clean)),
            None => crate::project::without_recorded_login_bindings(source),
        };
        self.report.merge(&report);
        if let Some(clean) = clean {
            self.edit(leaf, Payload::Text(clean), Kind::Credential);
        }
    }

    /// Cleans only addresses in text RustGIS generated, without claiming opaque text detection.
    pub(crate) fn generated(&mut self, leaf: Leaf, source: &str) {
        let clean = generated_text(source);
        if clean != source {
            self.edit(leaf, Payload::Text(clean), Kind::Generated);
        }
    }
}

/// Generated display text only: remove capability headers structurally, sanitize addresses
/// while their syntax is intact, then mask active secrets. Never use as execution or approval.
pub(crate) fn generated_text(source: &str) -> String {
    let public = crate::project::without_recorded_login_bindings(source);
    let source = public.as_deref().unwrap_or(source);
    // Address scrubbing keeps later prose, but an unknown secret value may run past whitespace.
    if let Some(strict) = rustgis_data::diagnostics::secret_continuation_cut(source) {
        return crate::render::rustgis_tiles::logins::scrub(&strict);
    }
    let (clean, _) = rustgis_data::address::scrub_addresses(source);
    crate::render::rustgis_tiles::logins::scrub(&clean)
}

struct Transaction<'a, T: Durable> {
    live: &'a mut T,
    edits: Vec<Edit>,
    applied: usize,
    defect: bool,
}

impl<T: Durable> Transaction<'_, T> {
    /// Guard is already armed. The original becomes journal-owned at each infallible swap.
    fn apply(&mut self) -> Result<(), String> {
        for edit in &mut self.edits {
            let field = self.live.field_mut(&edit.at.owners, &edit.at.leaf).ok_or("internal output location changed")?;
            if !field.swap(&mut edit.held) {
                return Err("internal output field type changed".into());
            }
            self.applied += 1;
        }
        Ok(())
    }

    /// Restores the applied prefix in reverse, with no discovery, parsing or allocation on the
    /// valid path. A resolver defect is an internal error, never a successful partial output.
    fn rollback(&mut self) {
        while self.applied > 0 {
            self.applied -= 1;
            if let Some(edit) = self.edits.get_mut(self.applied) {
                match self.live.field_mut(&edit.at.owners, &edit.at.leaf) {
                    Some(field) => self.defect |= !field.swap(&mut edit.held),
                    None => self.defect = true,
                }
            } else {
                self.defect = true;
            }
        }
    }
}

impl<T: Durable> Drop for Transaction<'_, T> {
    /// Restores the applied journal before releasing the live borrow, including native unwinding.
    fn drop(&mut self) {
        self.rollback();
    }
}

/// Rejects repeated locations before arming, so one edit can never overwrite another's original.
fn prepared<'a>(value: &impl Durable, relative_to: Option<&'a str>, depth: usize) -> Result<Prepare<'a>, String> {
    let mut p = Prepare::new(relative_to, depth);
    value.prepare(&mut p);
    let mut seen = HashSet::new();
    if p.edits.iter().any(|e| !seen.insert(&e.at)) {
        return Err("internal duplicate output location".into());
    }
    drop(seen);
    Ok(p)
}

/// No whole-project clone: every live field is restored on success, error and native unwind.
/// Abort/OOM/process death cannot run Drop; no temporarily edited state is persisted by them.
pub(crate) fn output<T: Durable, R>(
    value: &mut T,
    relative_to: Option<&str>,
    f: impl FnOnce(&T) -> Result<R, String>,
) -> Result<(R, Report), String> {
    let p = prepared(value, relative_to, 0)?;
    let mut t = Transaction { live: value, edits: p.edits, applied: 0, defect: false };
    t.apply()?;
    let result = f(t.live);
    t.rollback();
    if t.defect {
        return Err("internal output rollback failed".into());
    }
    result.map(|r| (r, p.report))
}

/// Consumes a draft the writer already owns, after structural edits; no extra project clone.
pub(crate) fn output_owned<T: Durable, R>(value: T, f: impl FnOnce(&T) -> Result<R, String>) -> Result<(R, Report), String> {
    output_owned_at(value, 0, f)
}

/// Nested documents share the executable nesting budget, instead of starting a fresh one.
pub(crate) fn output_owned_at<T: Durable, R>(
    value: T,
    depth: usize,
    f: impl FnOnce(&T) -> Result<R, String>,
) -> Result<(R, Report), String> {
    output_owned_changed(value, depth, |value, _| f(value))
}

/// Typed documents serialize only when an actual field changed, independently of notices.
pub(crate) fn output_owned_changed<T: Durable, R>(
    mut value: T,
    depth: usize,
    f: impl FnOnce(&T, bool) -> Result<R, String>,
) -> Result<(R, Report), String> {
    let p = prepared(&value, None, depth)?;
    let changed = !p.edits.is_empty();
    for mut edit in p.edits {
        if !value.field_mut(&edit.at.owners, &edit.at.leaf).ok_or("internal output location changed")?.swap(&mut edit.held) {
            return Err("internal output field type changed".into());
        }
    }
    f(&value, changed).map(|r| (r, p.report))
}

struct Remembered {
    at: At,
    written: Payload,
    original: Payload,
}
struct Written {
    len: usize,
    hash: u64,
    bytes: usize,
    edits: Vec<Remembered>,
}
const MAX_ENTRY: usize = 1 << 20;
const MAX_TOTAL: usize = 8 << 20;
const MAX_ENTRIES: usize = 32;

/// Process-local identity, never a public hash or a peer's authority.
fn identity(text: &str) -> (usize, u64) {
    static KEY: OnceLock<RandomState> = OnceLock::new();
    (text.len(), KEY.get_or_init(RandomState::new).hash_one(text.as_bytes()))
}

/// The explicit-save cache is independent of the output rollback journal.
fn kept() -> std::sync::MutexGuard<'static, Vec<Written>> {
    static KEPT: OnceLock<Mutex<Vec<Written>>> = OnceLock::new();
    KEPT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Remembers only credential edits after rollback, with exact written-value equality on reopen.
pub(crate) fn output_remembered<T: Durable>(
    value: &mut T,
    dir: Option<&str>,
    f: impl FnOnce(&T) -> Result<String, String>,
) -> Result<(String, Report), String> {
    let p = prepared(value, dir, 0)?;
    let mut t = Transaction { live: value, edits: p.edits, applied: 0, defect: false };
    t.apply()?;
    let result = f(t.live);
    t.rollback();
    if t.defect {
        return Err("internal output rollback failed".into());
    }
    let text = result?;
    let mut report = p.report;
    let (len, hash) = identity(&text);
    let mut bytes = 0usize;
    for e in t.edits.iter().filter(|e| e.kind == Kind::Credential) {
        let original = t.live.field_mut(&e.at.owners, &e.at.leaf).ok_or("internal output location changed")?;
        let written = match &e.held {
            Payload::Text(v) => v.len(),
            Payload::Json(v) => json_bytes(v),
        };
        bytes = bytes
            .saturating_add(original.bytes())
            .saturating_add(written)
            .saturating_add(std::mem::size_of::<At>())
            .saturating_add(e.at.owners.len().saturating_mul(std::mem::size_of::<Owner>()));
    }
    let mut cache = kept();
    cache.retain(|e| (e.len, e.hash) != (len, hash));
    if bytes > MAX_ENTRY {
        report.not_remembered = true;
    } else if bytes > 0 {
        let mut edits = Vec::new();
        for e in std::mem::take(&mut t.edits).into_iter().filter(|e| e.kind == Kind::Credential) {
            let mut original = t.live.field_mut(&e.at.owners, &e.at.leaf).ok_or("internal output location changed")?.payload();
            match &mut original {
                Payload::Text(text) => crate::project::clear_login_bindings([text]),
                Payload::Json(value) => {
                    if let Some(public) = crate::project::public_login_params(value) {
                        *value = public;
                    }
                }
            }
            edits.push(Remembered { at: e.at, written: e.held, original });
        }
        insert(&mut cache, Written { len, hash, bytes, edits });
    }
    Ok((text, report))
}

/// Oldest explicit saves go first, bounded by both count and total retained payload.
fn insert(cache: &mut Vec<Written>, entry: Written) {
    while cache.len() >= MAX_ENTRIES || cache.iter().fold(entry.bytes, |n, e| n.saturating_add(e.bytes)) > MAX_TOTAL {
        cache.remove(0);
    }
    cache.push(entry);
}

/// Runs before normalize/path resolution. Imports and peer ingress never call this function.
pub(crate) fn restore(value: &mut impl Durable, text: &str) {
    let id = identity(text);
    let cache = kept();
    if let Some(entry) = cache.iter().find(|e| (e.len, e.hash) == id) {
        for e in &entry.edits {
            if let Some(field) = value.field_mut(&e.at.owners, &e.at.leaf)
                && field.equals(&e.written)
            {
                let mut original = e.original.clone();
                let _ = field.swap(&mut original);
            }
        }
    }
}

pub(crate) enum Kept {
    Unchanged,
    Changed(String),
    Withheld(&'static str),
}

/// Prefix recognition precedes parsing: malformed recognized wrappers never become opaque.
pub(crate) fn public_address(address: &str) -> Kept {
    public_address_at(address, 0)
}

/// Wrapper recursion is bounded independently of executable document nesting.
fn public_address_at(address: &str, depth: usize) -> Kept {
    use rustgis_data::{
        address::{Public, public_web_address},
        s3_address,
    };
    if let Some((mut public, _)) = rustgis_data::source::myconn::split_password(address) {
        rustgis_data::address::redact_query(&mut public);
        return if public == address { Kept::Unchanged } else { Kept::Changed(public) };
    }
    if let Some((public, _)) = rustgis_data::source::msconn::split_password(address) {
        return if public == address { Kept::Unchanged } else { Kept::Changed(public) };
    }
    let text = address.trim_start();
    let lower = text.to_ascii_lowercase();
    let recognized = [
        "rustgis-login:",
        "rustgis-anonymous:",
        "rustgis-s3:",
        "pmtiles:",
        "cog:",
        "postgresql:",
        "postgres:",
        "pg:",
        "https:",
        "http:",
        "s3:",
    ]
    .into_iter()
    .find(|p| lower.starts_with(p));
    let Some(prefix) = recognized else { return Kept::Unchanged };
    if depth >= 8 {
        return Kept::Withheld(prefix);
    }
    if prefix == "rustgis-login:" {
        let Ok(Some(bound)) = rustgis_data::login_address::parse(text) else { return Kept::Withheld(prefix) };
        return match public_address_at(bound.url, depth + 1) {
            Kept::Unchanged => Kept::Changed(bound.url.to_string()),
            other => other,
        };
    }
    if prefix == "rustgis-anonymous:" {
        let Some(inner) = text.get(prefix.len()..) else { return Kept::Withheld(prefix) };
        return match public_address_at(inner, depth + 1) {
            Kept::Unchanged => Kept::Unchanged,
            Kept::Changed(s) => Kept::Changed(format!("{prefix}{s}")),
            Kept::Withheld(_) => Kept::Withheld(prefix),
        };
    }
    if matches!(prefix, "pmtiles:" | "cog:") {
        let Some((head, archive)) = crate::sources::template_archive(text) else { return Kept::Withheld(prefix) };
        let (file, look) = split_look(archive);
        return match public_address_at(file, depth + 1) {
            Kept::Unchanged => Kept::Unchanged,
            Kept::Changed(s) => Kept::Changed(format!("{head}{s}{look}")),
            Kept::Withheld(_) => Kept::Withheld(prefix),
        };
    }
    if prefix == "rustgis-s3:" {
        let (file, look) = split_look(text);
        let Ok(Some(b)) = s3_address::parse(file) else { return Kept::Withheld(prefix) };
        let endpoint = public_address_at(b.endpoint, depth + 1);
        let url = public_address_at(b.url, depth + 1);
        if matches!(endpoint, Kept::Withheld(_)) || matches!(url, Kept::Withheld(_)) {
            return Kept::Withheld(prefix);
        }
        if matches!((&endpoint, &url), (Kept::Unchanged, Kept::Unchanged)) {
            return Kept::Unchanged;
        }
        let endpoint = match &endpoint {
            Kept::Changed(s) => s.as_str(),
            _ => b.endpoint,
        };
        let url = match &url {
            Kept::Changed(s) => s.as_str(),
            _ => b.url,
        };
        return match s3_address::bind(b.binding, endpoint, url) {
            Ok(s) => Kept::Changed(format!("{s}{look}")),
            Err(_) => Kept::Withheld(prefix),
        };
    }
    if matches!(prefix, "postgresql:" | "postgres:" | "pg:") {
        return match rustgis_data::source::pgconn::public_label(text) {
            Some(s) if s == address => Kept::Unchanged,
            Some(s) => Kept::Changed(s),
            None => Kept::Withheld(prefix),
        };
    }
    // A COG's look is kept, as on the wrappers above; any other fragment goes, as OAuth and
    // SAS-style links carry tokens there.
    let (file, look) = split_look(address);
    match public_web_address(file) {
        Public::Unchanged => Kept::Unchanged,
        Public::Changed { address, .. } => Kept::Changed(format!("{address}{look}")),
        Public::Withheld => Kept::Withheld(prefix),
    }
}

/// A COG address without its look (`…#bands=…`), and the look as written. A reader takes any word
/// for `dequantize` and `cube`, but a saved look holds only numbers and these keys' own words: any
/// other, or a key not named here, stays in the address and goes as any other fragment does.
fn split_look(address: &str) -> (&str, &str) {
    let file = crate::render::rustgis_tiles::cog::split(address).0;
    let look = address.get(file.len()..).unwrap_or("");
    let plain = look.split(['#', '&']).filter(|kv| !kv.is_empty()).all(|kv| {
        // `Look::parse` has checked that `bands` and `range` are numbers.
        matches!(kv.split_once('='), Some(("bands" | "range", _) | ("dequantize", "aef" | "none") | ("cube", "0" | "1")))
    });
    if plain { (file, look) } else { (address, "") }
}

/// Emits preparation losses after the durable projection's journal has restored live fields.
/// This does not confirm delivery or commit the enclosing command's notices.
pub(crate) fn notify(s: &mut crate::Session, report: &Report) {
    for text in report.notices() {
        s.notify(format!("Prepared output: {text}"), false);
    }
}

/// Compares credential-bearing executable leaves through the same typed output inventory.
/// Peer input never restores these credentials; a changed or removed field is reported once.
pub(crate) fn changed_executables(old: &mut crate::Project, incoming: &mut crate::Project) -> Result<u32, String> {
    let prepared = prepared(old, None, 0)?;
    let mut count = 0u32;
    for edit in prepared.edits {
        if edit.kind != Kind::Credential
            || !matches!(
                edit.at.leaf,
                Leaf::Task(_)
                    | Leaf::TaskCommand(_)
                    | Leaf::ModelStep(_)
                    | Leaf::ModelStepTool(_)
                    | Leaf::ModelNode(_)
                    | Leaf::ModelNodeTool(_)
                    | Leaf::HistoryParams
                    | Leaf::HistoryId
                    | Leaf::HistoryCommand
                    | Leaf::HistoryCommandId
                    | Leaf::CellSource(_)
            )
        {
            continue;
        }
        let same = match (old.field_mut(&edit.at.owners, &edit.at.leaf), incoming.field_mut(&edit.at.owners, &edit.at.leaf)) {
            (Some(FieldMut::Text(a)), Some(FieldMut::Text(b))) => a == b,
            (Some(FieldMut::Json(a)), Some(FieldMut::Json(b))) => a == b,
            _ => false,
        };
        count = count.saturating_add(u32::from(!same));
    }
    Ok(count)
}

impl<T: Durable> Durable for Vec<T> {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        for (i, v) in self.iter().enumerate() {
            p.child(Owner::History(i), v);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        let (Owner::History(i), rest) = owners.split_first()? else { return None };
        self.get_mut(*i)?.field_mut(rest, leaf)
    }
}

impl Durable for rustgis_map::Map {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        for s in self.slots() {
            p.string(Leaf::Map(s.at), s.value, s.address, s.local_path);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        if !owners.is_empty() {
            return None;
        }
        let Leaf::Map(at) = leaf else { return None };
        self.slot_mut(at).map(FieldMut::Text)
    }
}

impl Durable for rustgis_map::Layer {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        for (role, value, address, path) in self.output_slots() {
            p.string(Leaf::Layer(role), value, address, path);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        if !owners.is_empty() {
            return None;
        }
        let Leaf::Layer(at) = leaf else { return None };
        self.output_slot_mut(*at).map(FieldMut::Text)
    }
}

impl Durable for rustgis_map::presentation::Presentation {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        use rustgis_map::presentation::{Page, PageKind};
        let Self { name: _, width: _, height: _, pages } = self;
        for (i, Page { title: _, kind, notes: _, seconds: _ }) in pages.iter().enumerate() {
            match kind {
                PageKind::Video { source, poster: _, caption: _ } => p.string(Leaf::Video(i), source, true, false),
                PageKind::Map { map: _, center: _, width_m: _ }
                | PageKind::Blank { text: _, background: _ }
                | PageKind::Image { image: _, caption: _ } => {}
            }
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        if !owners.is_empty() {
            return None;
        }
        let Leaf::Video(i) = leaf else { return None };
        match &mut self.pages.get_mut(*i)?.kind {
            rustgis_map::presentation::PageKind::Video { source, .. } => Some(FieldMut::Text(source)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_memory_has_entry_total_and_count_bounds() {
        let marker = "RGSYNTH_1";
        let mut p = crate::Project::default();
        p.tasks.push(crate::project::Task {
            name: "large".into(),
            description: String::new(),
            steps: vec![crate::project::TaskStep {
                title: String::new(),
                instructions: String::new(),
                command: "layer.addUrl".into(),
                params: serde_json::json!({"token": marker.repeat(MAX_ENTRY / marker.len() + 1)}),
            }],
        });
        let (text, report) = p.saved_text(None).unwrap();
        assert!(report.not_remembered && report.notices().iter().any(|n| n.contains("memory limit")));
        assert!(crate::Project::from_json(&text).unwrap().tasks[0].steps[0].params["token"].as_str().unwrap().is_empty());
        let mut cache = Vec::new();
        for hash in 0..40 {
            insert(&mut cache, Written { len: 0, hash, bytes: MAX_ENTRY, edits: Vec::new() });
        }
        assert_eq!(cache.len(), MAX_TOTAL / MAX_ENTRY);
        assert_eq!(cache.first().unwrap().hash, 32);
        let mut cache = Vec::new();
        for hash in 0..40 {
            insert(&mut cache, Written { len: 0, hash, bytes: 1, edits: Vec::new() });
        }
        assert_eq!(cache.len(), MAX_ENTRIES);
        assert_eq!(cache.first().unwrap().hash, 8);
    }
}
