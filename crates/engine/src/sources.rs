//! Data sources the user returns to: favourites, the ones added recently, saved services,
//! databases and STAC APIs, and a library of well-known public ones. Kept for the session; the
//! desktop and web apps keep them with their preferences. Never a secret: addresses are kept
//! without their passwords, tokens or signatures.

use rustgis_data::source::credentials::{redact_query, secret_key};
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// How many recent sources are kept.
pub const MAX_RECENT: usize = 20;
/// How many favourites and saved sources are kept.
pub const MAX_KEPT: usize = 200;
/// The longest address or name kept.
const MAX_TEXT: usize = 2048;

/// The kinds of source, by what adds them.
pub const KINDS: &[&str] = &["file", "url", "tiles", "service", "database", "zarr", "stac", "sample", "s3"];

/// A data source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// One of [`KINDS`].
    pub kind: String,
    /// What it is called.
    pub name: String,
    /// Its path, web address, connection label or name.
    pub address: String,
    /// What else adds it: a service's type or layer, a database's table, a Zarr variable.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub extra: Value,
}

/// The sources kept.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sources {
    #[serde(default)]
    pub favourites: Vec<Source>,
    #[serde(default)]
    pub recent: Vec<Source>,
    #[serde(default)]
    pub saved: Vec<Source>,
    /// Bumped on every change, so the app knows when to keep them.
    #[serde(skip)]
    pub generation: u64,
    /// What of the preference this version couldn't use, written back as it was.
    #[serde(skip)]
    kept: Kept,
}

/// The three lists, as the preference names them.
const LISTS: [&str; 3] = ["favourites", "recent", "saved"];

/// What of the saved sources this version couldn't use. A newer RustGIS may add kinds, fields and
/// lists: they are kept and written back unchanged, so a source noted or kept here never drops
/// what the newer one wrote.
#[derive(Clone, Debug, Default, PartialEq)]
struct Kept {
    /// The whole preference as read, when it couldn't be read at all, and why: it is never
    /// written over, so what changes in this session isn't kept.
    whole: Option<(String, String)>,
    /// Rows of each list that couldn't be read (an unknown kind, an address that isn't one…), or
    /// that are past the list's limit.
    rows: BTreeMap<&'static str, Vec<Value>>,
    /// How many of those rows are past the limit, rather than unreadable.
    over: usize,
    /// Fields of read sources this version doesn't know, by list, kind and address.
    fields: BTreeMap<(&'static str, String, String), Map<String, Value>>,
    /// Keys beside the three lists this version doesn't know.
    top: Map<String, Value>,
    /// Rows and fields that couldn't be read and held a password, key or token: not kept, since
    /// this version can't strip it from what it doesn't understand.
    secrets: usize,
    /// Readable sources whose name, address or details [`clean`] changed (a name too long or with
    /// control characters, an address with a password, key or token, details that aren't short
    /// text, more than eight, or secret-looking): saved that way.
    cut: usize,
    /// Readable rows that repeat the identity of an earlier row of their list: dropped (the first
    /// one wins, with its unknown fields).
    repeated: usize,
}

impl Kept {
    /// The entries of `o` without a password, key or token, counting those left out. Every entry is
    /// classified on the original object first, beside the others and `address` (the one a row
    /// keeps apart from them): a bare `key` is a secret beside a web address, and an address
    /// holding a secret doesn't stop being one; `known` is the row's known details (its `extra`), a
    /// sibling too. Only then are the classified entries removed.
    fn without_secrets(&mut self, o: Map<String, Value>, address: &str, known: &Value) -> Map<String, Value> {
        let beside = web_address(&Value::String(address.into()), 0) || web_address(known, 0);
        let (o, secrets) = without_secrets(o, beside);
        self.secrets += secrets;
        o
    }
}

/// The entries of `o` without a password, key or token, and how many were left out. Every entry
/// is classified on the original object first, beside the others: a bare `key` is a secret beside
/// a web address (another entry, or one outside `o` when `beside`), and an address holding a
/// secret doesn't stop being one. Only then are the classified entries removed.
pub(crate) fn without_secrets(mut o: Map<String, Value>, beside: bool) -> (Map<String, Value>, usize) {
    // Count address-bearing siblings once; unknown preference fields are not capped.
    let addresses =
        o.iter().filter(|(k, v)| !k.eq_ignore_ascii_case("data") && !k.eq_ignore_ascii_case("text") && web_address(v, 0)).count();
    let secret: BTreeSet<String> = o
        .iter()
        .filter(|(k, v)| {
            let mut probe = Map::new();
            probe.insert((*k).clone(), (*v).clone());
            let own_address = !k.eq_ignore_ascii_case("data") && !k.eq_ignore_ascii_case("text") && web_address(v, 0);
            if beside || addresses > usize::from(own_address) {
                probe.insert("\u{1}beside".into(), "https://a".into());
            }
            holds_secret(&Value::Object(probe))
        })
        .map(|(k, _)| k.clone())
        .collect();
    let left_out = secret.len();
    o.retain(|k, _| !secret.contains(k));
    (o, left_out)
}

/// Whether `v` is, or holds, a web address (not under a file's `data` or `text`).
pub(crate) fn web_address(v: &Value, depth: usize) -> bool {
    match v {
        Value::String(t) => t.trim_start().get(..4).is_some_and(|p| p.eq_ignore_ascii_case("http")),
        Value::Object(o) if depth < 4 => {
            o.iter().any(|(k, v)| !k.eq_ignore_ascii_case("data") && !k.eq_ignore_ascii_case("text") && web_address(v, depth + 1))
        }
        Value::Array(a) if depth < 4 => a.iter().any(|v| web_address(v, depth + 1)),
        _ => false,
    }
}

/// Whether [`clean`] changed something of `before` that isn't just tidied (whitespace, an empty
/// name or details).
fn cut(before: &Source, after: &Source) -> bool {
    let name = before.name.trim();
    let extra = match &before.extra {
        Value::Null => false,
        Value::Object(o) => !o.is_empty() && before.extra != after.extra,
        _ => true,
    };
    (!name.is_empty() && after.name != name) || after.address != before.address.trim() || extra
}

/// A saved row: the source, and the fields this version doesn't know.
#[derive(Deserialize)]
struct Row {
    #[serde(flatten)]
    source: Source,
    #[serde(flatten)]
    other: Map<String, Value>,
}

impl Sources {
    /// Sources read back from the app's preferences: each one checked and cleaned again (the
    /// preferences are a file anyone could have edited). What this version can't use is kept, to
    /// be written back by [`Sources::to_json`] as it was: rows it can't read, fields and lists it
    /// doesn't know, or the whole preference when it can't be read at all. [`Sources::unread`]
    /// says what was kept.
    pub fn from_json(text: &str) -> Sources {
        let mut s = Sources::default();
        // An editor's byte-order mark isn't part of the JSON; a preference kept whole keeps it.
        let body = text.trim_start_matches('\u{feff}');
        if body.trim().is_empty() {
            return s;
        }
        let whole = |why: &str| Kept { whole: Some((text.to_string(), why.to_string())), ..Kept::default() };
        let mut o = match serde_json::from_str::<Value>(body) {
            Ok(Value::Object(o)) => o,
            _ => {
                s.kept = whole("they aren't a JSON object");
                return s;
            }
        };
        let mut lists = Vec::with_capacity(LISTS.len());
        for list in LISTS {
            match o.remove(list) {
                None => lists.push(Vec::new()),
                Some(Value::Array(rows)) => lists.push(rows),
                Some(_) => {
                    s.kept = whole("a list of them isn't a JSON array");
                    return s;
                }
            }
        }
        s.kept.top = s.kept.without_secrets(o, "", &Value::Null);
        for (list, rows) in LISTS.into_iter().zip(lists) {
            let cap = if list == "recent" { MAX_RECENT } else { MAX_KEPT };
            let mut into: Vec<Source> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for row in rows {
                // Entry by entry: one that doesn't read is kept as it was, not the whole list.
                let read = serde_json::from_value::<Row>(row.clone()).ok().and_then(|r| {
                    let mut cleaned = clean(r.source.clone()).ok()?;
                    let was_cut = cut(&r.source, &cleaned);
                    // A login binding is the app's own output, never authority from an earlier
                    // process's login: it isn't kept, and clearing it is no loss.
                    crate::s3_connections::clear_bindings(std::iter::once(&mut cleaned.address));
                    Some((was_cut, cleaned, r.other))
                });
                match read {
                    // The first row of an identity wins; a repeat is dropped, and said.
                    Some((_, source, _)) if seen.contains(&identity(&source.kind, &source.address)) => s.kept.repeated += 1,
                    Some((was_cut, source, other)) if into.len() < cap => {
                        seen.insert(identity(&source.kind, &source.address));
                        s.kept.cut += usize::from(was_cut);
                        let other = s.kept.without_secrets(other, &source.address, &source.extra);
                        if !other.is_empty() {
                            s.kept.fields.insert((list, source.kind.clone(), source.address.clone()), other);
                        }
                        into.push(source);
                    }
                    _ if holds_secret(&row) => s.kept.secrets += 1,
                    read => {
                        // A readable row past the cap is still the first of its identity, so a
                        // later repeat of it is said as repeated, not counted past the cap again.
                        if let Some((_, source, _)) = &read {
                            seen.insert(identity(&source.kind, &source.address));
                        }
                        s.kept.over += usize::from(read.is_some());
                        s.kept.rows.entry(list).or_default().push(row);
                    }
                }
            }
            if let Ok(l) = s.list_mut(list) {
                *l = into;
            }
        }
        s
    }

    /// What of the preference read by [`Sources::from_json`] isn't used here, one line per kind
    /// (none when all of it is).
    pub fn unread(&self) -> Vec<String> {
        let k = &self.kept;
        if let Some((_, why)) = &k.whole {
            return vec![format!(
                "The saved data sources couldn't be read ({why}): none is listed, and the favourites, recent and saved sources of this session aren't kept over them; fix or remove \"data_sources\" in the preferences file to keep them"
            )];
        }
        let mut said = Vec::new();
        let rows = k.rows.values().map(Vec::len).sum::<usize>().saturating_sub(k.over);
        if k.repeated > 0 {
            let n = k.repeated;
            said.push(format!(
                "{n} repeated saved data source{} removed (the first of each is kept, with its settings)",
                if n == 1 { " was" } else { "s were" }
            ));
        }
        if k.over > 0 {
            let n = k.over;
            said.push(format!(
                "{n} saved data source{} past the lists' limits ({MAX_KEPT} favourites or saved, {MAX_RECENT} recent): not listed, and kept as {} when the sources are saved",
                if n == 1 { " is" } else { "s are" },
                if n == 1 { "it was" } else { "they were" }
            ));
        }
        if rows > 0 {
            said.push(format!(
                "{rows} saved data source{} couldn't be read (from a newer RustGIS, or damaged): not listed, and kept as {} when the sources are saved",
                if rows == 1 { "" } else { "s" },
                if rows == 1 { "it was" } else { "they were" }
            ));
        }
        if !k.fields.is_empty() {
            let n = k.fields.len();
            said.push(format!(
                "{n} saved data source{} settings this version doesn't know (from a newer RustGIS): ignored here, and kept when the sources are saved",
                if n == 1 { " has" } else { "s have" }
            ));
        }
        if k.cut > 0 {
            let n = k.cut;
            said.push(format!(
                "{n} saved data source{} a name, address or details this version changes (a name too long or with control characters, an address with a password, key or token, details that aren't short text, more than eight, or secret-looking): saved changed",
                if n == 1 { " has" } else { "s have" }
            ));
        }
        if k.secrets > 0 {
            said.push(secrets_notice(k.secrets));
        }
        said
    }

    /// How many values holding a password, key or token have been left out of what is kept so far.
    /// A caller that changes a source compares it before and after, to say what that change left
    /// out ([`secrets_notice`]); [`Sources::unread`] says what reading did.
    pub fn secrets_left_out(&self) -> usize {
        self.kept.secrets
    }

    /// Takes in the sources kept with the preferences, keeping what this session already has:
    /// in-memory entries stay (and first), stored ones are added after them unless the same source
    /// ([`Source::is`]) is there already.
    pub fn merge(&mut self, mut stored: Sources) {
        // Stored sources that no longer fit (a change before the first read filled the list) are
        // kept as rows, so they are said and written back.
        let mut over = Vec::new();
        for list in LISTS {
            let cap = if list == "recent" { MAX_RECENT } else { MAX_KEPT };
            let Ok(incoming) = stored.list_mut(list).map(std::mem::take) else { continue };
            let Ok(now) = self.list_mut(list) else { continue };
            for source in incoming {
                if now.iter().any(|n| same(n, &source)) {
                    continue;
                }
                if now.len() < cap {
                    now.push(source);
                } else {
                    over.push((list, stored.row(list, &source)));
                    stored.kept.fields.remove(&(list, source.kind.clone(), source.address.clone()));
                }
            }
        }
        self.kept = stored.kept;
        self.kept.over += over.len();
        for (list, row) in over {
            self.kept.rows.entry(list).or_default().push(row);
        }
        // A readable row kept past a limit for a source that is listed gives it its fields.
        for list in LISTS {
            if self.kept.rows.contains_key(list) {
                let ids: Vec<_> = match list {
                    "favourites" => &self.favourites,
                    "saved" => &self.saved,
                    _ => &self.recent,
                }
                .iter()
                .map(|s| identity(&s.kind, &s.address))
                .collect();
                for id in ids {
                    self.adopt(list, &id);
                }
            }
        }
        self.recheck_fields();
        self.generation += 1;
    }

    /// Classifies the fields kept for each listed source again, against the source as it is now
    /// (a changed `extra` or address can make a field that passed at load a secret), and drops
    /// what is one, counted with the rest. Every change to a list that keeps fields ends here, so
    /// [`Sources::to_json`] writes only what passed.
    fn recheck_fields(&mut self) {
        let keys: Vec<_> = self.kept.fields.keys().cloned().collect();
        for key in keys {
            let (list, kind, address) = &key;
            let listed = match *list {
                "favourites" => &self.favourites,
                "saved" => &self.saved,
                _ => &self.recent,
            };
            let id = (kind.clone(), address.clone());
            let Some(source) = listed.iter().find(|s| identity(&s.kind, &s.address) == id).cloned() else { continue };
            if let Some(fields) = self.kept.fields.remove(&key) {
                let fields = self.kept.without_secrets(fields, &source.address, &source.extra);
                if !fields.is_empty() {
                    self.kept.fields.insert(key, fields);
                }
            }
        }
    }

    /// Listing a source takes along the fields of a readable row of it kept past the list's limit
    /// (the first fields of an identity win), and removes that row: it would list it twice.
    fn adopt(&mut self, list: &'static str, id: &(String, String)) {
        let Some(rows) = self.kept.rows.get_mut(list) else { return };
        let mut taken = Vec::new();
        rows.retain(|r| {
            let ours = row_identity(r).as_ref() == Some(id);
            if ours && let Ok(row) = serde_json::from_value::<Row>(r.clone()) {
                taken.push(row.other);
            }
            !ours
        });
        self.kept.over = self.kept.over.saturating_sub(taken.len());
        for other in taken {
            let fields = self.kept.fields.entry((list, id.0.clone(), id.1.clone())).or_default();
            for (k, v) in other {
                fields.entry(k).or_insert(v);
            }
        }
        self.kept.fields.retain(|_, f| !f.is_empty());
    }

    /// The `data_sources` preference: these sources, with what [`Sources::from_json`] kept
    /// written back as it was (a preference it couldn't read at all, unchanged).
    pub fn to_json(&self) -> String {
        if let Some((text, _)) = &self.kept.whole {
            return text.clone();
        }
        let mut o = self.kept.top.clone();
        for (list, sources) in LISTS.into_iter().zip([&self.favourites, &self.recent, &self.saved]) {
            let mut rows: Vec<Value> = sources.iter().map(|source| self.row(list, source)).collect();
            rows.extend(self.kept.rows.get(list).into_iter().flatten().cloned());
            o.insert(list.into(), Value::Array(rows));
        }
        Value::Object(o).to_string()
    }

    /// A source as a preference row: with the fields kept for it that this version doesn't know.
    fn row(&self, list: &'static str, source: &Source) -> Value {
        let mut row = serde_json::to_value(source).unwrap_or(Value::Null);
        let (kind, address) = identity(&source.kind, &source.address);
        if let (Value::Object(row), Some(other)) = (&mut row, self.kept.fields.get(&(list, kind, address))) {
            for (k, v) in other {
                row.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        row
    }

    /// Notes a source just added (the most recent first, each once).
    pub fn note(&mut self, s: Source) {
        let Ok(s) = clean(s) else { return };
        self.recent.retain(|r| !same(r, &s));
        self.recent.insert(0, s);
        self.recent.truncate(MAX_RECENT);
        let id = identity(&self.recent[0].kind, &self.recent[0].address);
        self.adopt("recent", &id);
        // A source no longer recent takes the fields kept for it along.
        let recent = &self.recent;
        self.kept.fields.retain(|(list, kind, address), _| {
            *list != "recent" || recent.iter().any(|r| identity(&r.kind, &r.address) == identity(kind, address))
        });
        self.recheck_fields();
        self.generation += 1;
    }

    /// Keeps a source in `list` (`favourites` or `saved`), replacing the one with the same
    /// identity ([`Source::is`]: kind and address), whose name and `extra` the new one's take over.
    pub fn keep(&mut self, list: &str, s: Source) -> Result<Source, String> {
        let s = clean(s)?;
        let list = LISTS.into_iter().find(|l| *l == list).ok_or_else(|| format!("unknown list '{list}' (favourites, saved or recent)"))?;
        let l = self.list_mut(list)?;
        // The same source keeps its place.
        if let Some(slot) = l.iter_mut().find(|r| same(r, &s)) {
            *slot = s.clone();
        } else {
            if l.len() >= MAX_KEPT {
                return Err(format!("up to {MAX_KEPT} sources in {list}"));
            }
            l.push(s.clone());
        }
        self.adopt(list, &identity(&s.kind, &s.address));
        self.recheck_fields();
        self.generation += 1;
        // What was stored (cleaned): the caller reports it, wherever it sits in the list.
        Ok(s)
    }

    /// Forgets the source of `kind` at `address` from `list`; whether there was one.
    pub fn forget(&mut self, list: &str, kind: &str, address: &str) -> Result<bool, String> {
        let id = identity(kind, address);
        let l = self.list_mut(list)?;
        let before = l.len();
        l.retain(|r| identity(&r.kind, &r.address) != id);
        let gone = l.len() != before;
        let mut kept_gone = false;
        if let Some(list) = LISTS.into_iter().find(|l| *l == list) {
            self.kept.fields.retain(|(l, k, a), _| *l != list || identity(k, a) != id);
            // A readable row kept past the list's limit with the same identity would be listed
            // again by the next start; a row this version can't read is not this source's.
            if let Some(rows) = self.kept.rows.get_mut(list) {
                let before = rows.len();
                rows.retain(|r| row_identity(r).as_ref() != Some(&id));
                self.kept.over = self.kept.over.saturating_sub(before - rows.len());
                kept_gone = rows.len() != before;
            }
        }
        if gone || kept_gone {
            self.generation += 1;
        }
        Ok(gone || kept_gone)
    }

    fn list_mut(&mut self, list: &str) -> Result<&mut Vec<Source>, String> {
        match list {
            "favourites" => Ok(&mut self.favourites),
            "saved" => Ok(&mut self.saved),
            "recent" => Ok(&mut self.recent),
            other => Err(format!("unknown list '{other}' (favourites, saved or recent)")),
        }
    }
}

/// A source's identity, the one every list is matched by: its kind and its address with a login
/// binding cleared, as [`Sources::from_json`] reads it.
fn identity(kind: &str, address: &str) -> (String, String) {
    let mut address = address.to_string();
    crate::s3_connections::clear_bindings(std::iter::once(&mut address));
    (kind.to_string(), address)
}

/// The identity of a saved row [`Sources::from_json`] reads (none for one it can't).
fn row_identity(row: &Value) -> Option<(String, String)> {
    let source = clean(serde_json::from_value::<Row>(row.clone()).ok()?.source).ok()?;
    Some(identity(&source.kind, &source.address))
}

fn same(a: &Source, b: &Source) -> bool {
    identity(&a.kind, &a.address) == identity(&b.kind, &b.address)
}

/// What a `url` source's add may have had that changes how its address is read, kept in `extra`
/// for adding it again with `layer.addUrl`: a service's features are read page by page, a WCS
/// coverage in the system it was asked in (`coverageCrs`, kept with it).
pub const URL_FLAGS: [&str; 2] = ["service", "coverage"];

/// A flag as `extra` keeps it: its values are text.
const FLAG_ON: &str = "true";

impl Source {
    /// Whether the add this source records had `flag` (one of [`URL_FLAGS`]).
    pub fn flag(&self, flag: &str) -> bool {
        self.extra.get(flag).and_then(Value::as_str) == Some(FLAG_ON)
    }

    /// A source's identity: its kind and address. Everywhere a list is matched (kept, forgotten,
    /// noted as recent, shown as a favourite) it is by this; `extra` (a layer, a service type)
    /// is what the last one kept said.
    pub fn is(&self, kind: &str, address: &str) -> bool {
        self.kind == kind && self.address == address
    }
}

/// The notice for `n` values left out of the saved data sources because they hold a secret.
pub fn secrets_notice(n: usize) -> String {
    format!(
        "{n} value{} in the saved data sources {} a password, key or token: not kept, and left out the next time the sources are saved",
        if n == 1 { "" } else { "s" },
        if n == 1 { "holds" } else { "hold" }
    )
}

/// A source made safe to keep: a known kind, bounded text, and an address without secrets.
pub fn clean(mut s: Source) -> Result<Source, String> {
    if !KINDS.contains(&s.kind.as_str()) {
        return Err(format!("unknown kind '{}' ({})", s.kind, KINDS.join(", ")));
    }
    let address = s.address.trim();
    // A display-only prefix is safe to show, but cannot be used as a saved source.
    s.address = match crate::durable::public_address(address) {
        crate::durable::Kept::Withheld(_) => return Err("a source needs an address (a path, web address or connection)".into()),
        crate::durable::Kept::Changed(public) => public,
        crate::durable::Kept::Unchanged => redact(address),
    };
    // Provenance has its own structural bound; it must not consume the destination's limit.
    let (head, archive) = template_archive(&s.address).unwrap_or(("", s.address.as_str()));
    let bounded = match rustgis_data::s3_address::parse(archive) {
        Ok(Some(bound)) => head.len().saturating_add(bound.url.len()) <= MAX_TEXT,
        Ok(None) => s.address.len() <= MAX_TEXT,
        Err(_) => false,
    };
    if s.address.is_empty() || !bounded || s.address.chars().any(char::is_control) {
        return Err("a source needs an address (a path, web address or connection)".into());
    }
    s.name = s.name.trim().chars().filter(|c| !c.is_control()).take(200).collect();
    if s.name.is_empty() {
        s.name = default_name(&s.address);
    }
    // What else adds it: a small object of short strings, never anything that looks secret.
    s.extra = match s.extra {
        Value::Object(o) => Value::Object(
            o.into_iter().filter(|(k, v)| k.len() <= 32 && !secret_key(k) && v.as_str().is_some_and(|t| t.len() <= 512)).take(8).collect(),
        ),
        _ => Value::Null,
    };
    if s.extra.as_object().is_some_and(|o| o.is_empty()) {
        s.extra = Value::Null;
    }
    Ok(s)
}

/// A name from an address: its last part, without the extension.
pub fn default_name(address: &str) -> String {
    let base = address.split(['?', '#']).next().unwrap_or(address).trim_end_matches(['/', '\\']);
    let last = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let stem = last.rsplit_once('.').map(|(s, _)| s).filter(|s| !s.is_empty()).unwrap_or(last);
    let n: String = stem.chars().take(80).collect();
    if n.is_empty() { address.chars().take(80).collect() } else { n }
}

/// Whether command parameters hold something that gives access: a non-empty value under a key
/// that names a password, key, secret or token ([`secret_key`]), or an address or PostgreSQL
/// connection that [`redact`] would change. Recording a task leaves such a command out, since a
/// task is saved with the project and shared as a file.
pub fn holds_secret(params: &serde_json::Value) -> bool {
    // Keys holding a file's contents.
    fn contents_key(k: &str) -> bool {
        k.eq_ignore_ascii_case("data") || k.eq_ignore_ascii_case("text")
    }
    // A PEM private key (`-----BEGIN … PRIVATE KEY-----`), wherever it was pasted.
    fn private_key(t: &str) -> bool {
        t.contains("PRIVATE KEY-----")
    }
    fn walk(v: &serde_json::Value, depth: usize) -> bool {
        use serde_json::Value;
        match v {
            Value::Object(o) if depth < 16 => o.iter().any(|(k, v)| {
                // A switch (`"token": false`) holds no secret; a number might (a PIN).
                let filled = match v {
                    Value::String(t) => !t.trim().is_empty(),
                    Value::Null | Value::Bool(_) => false,
                    Value::Object(o) => !o.is_empty(),
                    Value::Array(a) => !a.is_empty(),
                    Value::Number(_) => true,
                };
                // A bare `key` beside a web address is that service's key; elsewhere it's as likely
                // a field or a lookup key (a `?key=…` in an address `redact` reads in the string).
                // (The address may sit inside a sibling: `{"source": {"url": …}, "key": …}`; a file's
                // contents, under `data` or `text`, isn't one.)
                fn address(v: &Value, depth: usize) -> bool {
                    match v {
                        Value::String(t) => t.trim_start().get(..4).is_some_and(|p| p.eq_ignore_ascii_case("http")),
                        Value::Object(o) if depth < 4 => o.iter().any(|(k, v)| !contents_key(k) && address(v, depth + 1)),
                        Value::Array(a) if depth < 4 => a.iter().any(|v| address(v, depth + 1)),
                        _ => false,
                    }
                }
                let beside_address = || o.iter().any(|(sibling, v)| sibling != k && !contents_key(sibling) && address(v, 0));
                // A file's contents (Add Data's `data`, a GeoJSON `text`) isn't an address to read
                // through, only checked for a private key pasted in.
                let contents = contents_key(k);
                let inside = match v {
                    // (Short ones, which can't be much of a file, are read as any string.)
                    Value::String(t) if contents && t.len() > 1024 => private_key(t),
                    _ => walk(v, depth + 1),
                };
                let plain_key = k.eq_ignore_ascii_case("key");
                let key_like = || v.as_str().is_some_and(looks_like_a_key);
                (secret_key(k) && (!plain_key || beside_address() || key_like()) && filled) || inside
            }),
            Value::Array(a) if depth < 16 => a.iter().any(|v| walk(v, depth + 1)),
            Value::Object(_) | Value::Array(_) => true,
            Value::String(t) => private_key(t) || redact(t) != *t || crate::roles::sql_secrets_removed(t) != *t,
            _ => false,
        }
    }
    walk(params, 0)
}

/// Whether `t` reads as an access key rather than a name: long, with no spaces, and changing
/// between lower case, upper case and digits at a good share of its letters and digits, as a
/// random token does (`sk-4f9aB27c…`, `pk.eyJ1Ijoi…`, a hex hash or id), unlike a field or a
/// lookup key, whose words and numbers come in runs (`parcel_id`, `municipalityCode2020`,
/// `area_2010_2020_km2_total`). A hex id counts as a key on purpose: it may be one, and a step
/// left out (with a notice) costs less than a key shared.
pub(crate) fn looks_like_a_key(t: &str) -> bool {
    #[derive(PartialEq)]
    enum Class {
        Lower,
        Upper,
        Digit,
    }
    let t = t.trim();
    let class = |c: char| {
        if c.is_ascii_digit() {
            Some(Class::Digit)
        } else if c.is_ascii_lowercase() {
            Some(Class::Lower)
        } else if c.is_ascii_uppercase() {
            Some(Class::Upper)
        } else {
            None
        }
    };
    let classes: Vec<Option<Class>> = t.chars().map(class).collect();
    let alnum = classes.iter().filter(|c| c.is_some()).count();
    // Changes between two letters or digits side by side (a separator breaks the pair). An upper
    // case letter followed by lower case is a capitalised word going on (`Land` in camelCase), so
    // each word boundary counts once, at its capital.
    let changes =
        classes.windows(2).filter(|w| matches!(w, [Some(a), Some(b)] if a != b && !(*a == Class::Upper && *b == Class::Lower))).count();
    t.chars().count() >= 20 && !t.contains(char::is_whitespace) && changes * 10 >= alnum * 3
}

/// Whether command parameters set a plain `key` (at any depth), which [`holds_secret`] lets
/// through: as often a field or a lookup key as an access key, so a recording keeps it and says so.
pub fn holds_plain_key(params: &serde_json::Value) -> bool {
    fn walk(v: &serde_json::Value, depth: usize) -> bool {
        use serde_json::Value;
        match v {
            Value::Object(o) if depth < 16 => o
                .iter()
                .any(|(k, v)| (k.eq_ignore_ascii_case("key") && v.as_str().is_some_and(|t| !t.trim().is_empty())) || walk(v, depth + 1)),
            Value::Array(a) if depth < 16 => a.iter().any(|v| walk(v, depth + 1)),
            _ => false,
        }
    }
    walk(params, 0)
}

/// A known source address safe to compose into a repeatedly redacted diagnostic, without
/// publishing a web-login runtime binding or the credentials inside a source wrapper.
pub fn address_in_message(address: &str) -> String {
    let mut shown = rustgis_data::diagnostics::address_in_message(&redact(address));
    // Source redaction can take out a secret query whole; the diagnostic label still says one went.
    let raw = rustgis_data::diagnostics::address_in_message(address);
    if let Some(marker) = ["[query omitted]", "[fragment omitted]"].into_iter().find(|m| raw.ends_with(m))
        && !shown.ends_with(marker)
        && !shown.ends_with("omitted]")
    {
        shown.push_str(marker);
    }
    shown
}

/// An error naming a long known address shortened to `limit` characters, then `reason` (fixed
/// prose, not from the network). A trailing omission marker (`[query omitted]`) is kept whole
/// and the address before it is cut instead, marked `…`; a cut never leaves half a marker. The
/// shortened address is already sanitized, so it isn't parsed again as an address.
pub fn shortened_message_with_address(address: &str, limit: usize, reason: &str) -> String {
    let full = address_in_message(address);
    if full.chars().count() <= limit {
        return message_with_address(address, reason);
    }
    let (head, marker) = match full.rfind('[') {
        Some(at) if full.ends_with(']') => full.split_at(at),
        _ => (full.as_str(), ""),
    };
    let mut shown: String = head.chars().take(limit.saturating_sub(marker.chars().count() + 1)).collect();
    if let Some(at) = shown.rfind('[').filter(|&at| !shown.get(at..).unwrap_or("").contains(']')) {
        shown.truncate(at);
    }
    shown.push('…');
    shown.push_str(marker);
    format!("{shown}: {reason}")
}

/// Names a known source while keeping its failure reason. Exact address occurrences are
/// sanitized before the conservative free-text redactor sees any unescaped secret whitespace.
pub fn message_with_address(address: &str, reason: &str) -> String {
    let message = rustgis_data::diagnostics::message_with_address(address, reason);
    let raw_shown = rustgis_data::diagnostics::address_in_message(address);
    let shown = address_in_message(address);
    let message = if raw_shown.is_empty() || raw_shown == shown { message } else { message.replace(&raw_shown, &shown) };
    // Reusing this helper must not prepend the same safe source label on every hop.
    // Only a verified companion belongs to the same download; unrelated targets keep their origin.
    let reason = message.strip_prefix(&shown).and_then(|tail| tail.strip_prefix(": "));
    let message = match reason.filter(|reason| {
        reason.strip_prefix(&shown).is_some_and(|tail| tail.is_empty() || tail.starts_with(": "))
            || reason.split_once(": ").is_some_and(|(label, _)| {
                label.contains("://")
                    && rustgis_data::io::shapefile_part_urls(address).is_some_and(|parts| {
                        // The part's label may come from either layer's composition.
                        parts.iter().any(|(_, part)| {
                            address_in_message(part) == label || rustgis_data::diagnostics::address_in_message(part) == label
                        })
                    })
            })
    }) {
        Some(reason) => reason,
        None => &message,
    };
    crate::render::rustgis_tiles::logins::scrub(message)
}

/// A PMTiles or COG tile template (`pmtiles:{z}/{x}/{y}@archive`, `cog:…@archive`): what comes
/// before its archive (up to and with the first `@`), and the archive, whose address is the one
/// that holds credentials and names a saved S3 connection.
pub(crate) fn template_archive(address: &str) -> Option<(&str, &str)> {
    use crate::render::rustgis_tiles::{cog, pmtiles};
    let rest = [pmtiles::SCHEME, cog::SCHEME].iter().find_map(|s| address.strip_prefix(s))?;
    let (_, archive) = rest.split_once('@')?;
    let head = address.get(..address.len() - archive.len())?;
    Some((head, archive))
}

/// An address without what would give someone else the user's access: the user and password of
/// a web address, query parameters that are keys, tokens or signatures (a signed blob's SAS, an
/// S3 presigned URL), and a PostgreSQL connection string's password. A MySQL, MariaDB or SQL Server
/// address keeps its user (the one to connect as), never its password. An address read through a
/// saved S3 connection is redacted inside (its inner address), and its length-prefixed form
/// rebuilt; a tile template, in its archive. Web-login runtime bindings are never published.
pub fn redact(address: &str) -> String {
    match crate::durable::public_address(address) {
        crate::durable::Kept::Changed(s) => return s,
        crate::durable::Kept::Withheld(prefix) => return prefix.into(),
        crate::durable::Kept::Unchanged => {}
    }
    // A clean recognized wrapper or database label already passed the shared owner. Keep its
    // layout, including a COG look; the generic scheme fallback is for older connection forms.
    if template_archive(address).is_some()
        || rustgis_data::s3_address::is_wrapper(address)
        || ["rustgis-anonymous:", "postgres:", "postgresql:", "pg:"]
            .iter()
            .any(|p| address.trim_start().get(..p.len()).is_some_and(|s| s.eq_ignore_ascii_case(p)))
    {
        return address.to_string();
    }
    if let Some((mut public, _)) = rustgis_data::source::myconn::split_password(address) {
        rustgis_data::address::redact_query(&mut public);
        return public;
    }
    if let Some((public, _)) = rustgis_data::source::msconn::split_password(address) {
        return public;
    }
    let Some((scheme, rest)) = address.split_once("://") else { return address.to_string() };
    // The user and password go first, before anything splits the address: a password may hold
    // an unencoded `/`, `?`, `#` or `@`. They end at the last `@` before the first `/`, or (a `/`
    // in the password) at the last `@` with a `:` before it.
    let cut = rustgis_data::source::pgconn::userinfo_end(rest);
    let rest = match cut {
        Some(i) => rest.get(i + 1..).unwrap_or(""),
        None => rest,
    };
    // The fragment never: OAuth and SAS-style links carry tokens there, and nothing that adds a
    // source needs it.
    let rest = rest.split('#').next().unwrap_or(rest);
    let mut out = format!("{scheme}://{rest}");
    redact_query(&mut out);
    out
}

/// Strips runtime capability wrappers (`rustgis-login:` and `rustgis-s3:`) from an address or
/// archive template while keeping its inner public/raw URL and any plain COG look.
pub(crate) fn strip_capabilities(address: &str) -> String {
    strip_capabilities_at(address, 0)
}

fn strip_capabilities_at(address: &str, depth: usize) -> String {
    if depth >= 8 {
        return "[address omitted]".into();
    }
    let text = address.trim_start();
    if let Some((head, archive)) = template_archive(text) {
        return format!("{head}{}", strip_capabilities_at(archive, depth + 1));
    }
    if text.starts_with(rustgis_data::login_address::PREFIX) {
        return match rustgis_data::login_address::parse(text) {
            Ok(Some(bound)) => strip_capabilities_at(bound.url, depth + 1),
            _ => "[address omitted]".into(),
        };
    }
    if let Some(inner) = text.strip_prefix(crate::render::rustgis_tiles::s3::ANONYMOUS) {
        return format!("{}{}", crate::render::rustgis_tiles::s3::ANONYMOUS, strip_capabilities_at(inner, depth + 1));
    }
    if rustgis_data::s3_address::is_wrapper(text) {
        let (file, look) = crate::durable::split_look(text);
        return match rustgis_data::s3_address::parse(file) {
            Ok(Some(bound)) => format!("{}{look}", strip_capabilities_at(bound.url, depth + 1)),
            _ => "[address omitted]".into(),
        };
    }
    address.to_string()
}

/// Redacts credentials AND strips all runtime capability wrappers (`rustgis-login:` and
/// `rustgis-s3:`) for command results and untrusted callers.
pub fn redact_output(address: &str) -> String {
    redact(&strip_capabilities(address))
}

/// Replaces every `rustgis-s3:` capability wrapper in free text or serialized JSON strings with
/// its inner address (or `[redacted]` when malformed), matching `rustgis_tiles::logins::scrub`.
pub fn scrub_s3_wrappers(text: &str) -> String {
    if !text.contains(rustgis_data::s3_address::PREFIX) {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(wrapper) = rest.strip_prefix(rustgis_data::s3_address::PREFIX) {
            let unwrapped = wrapper
                .split_once(':')
                .filter(|(b, _)| rustgis_data::s3_address::valid_binding(b))
                .and_then(|(_, r)| r.split_once(':'))
                .and_then(|(len, r)| {
                    len.parse::<usize>().ok().filter(|&n| n <= rustgis_data::s3_address::MAX_ENDPOINT).and_then(|n| r.get(n..))
                })
                .and_then(|r| r.strip_prefix(':'))
                .filter(|u| {
                    !rustgis_data::s3_address::is_wrapper(u)
                        && ["https://", "http://", "s3://"].iter().any(|p| u.get(..p.len()).is_some_and(|s| s.eq_ignore_ascii_case(p)))
                });
            if let Some(url) = unwrapped {
                rest = url;
                continue;
            }
            result.push_str("[redacted]");
            rest = wrapper.split_once(char::is_whitespace).map_or("", |(_, r)| r);
            continue;
        }
        let Some(ch) = rest.chars().next() else { break };
        result.push(ch);
        rest = rest.get(ch.len_utf8()..).unwrap_or("");
    }
    result
}

/// Recursively redacts known URL fields in a JSON value for non-User callers.
/// `tilesets`, `describe` and `capabilities` name protocol documents' addresses, not their
/// contents: respectively `ogc::tileset`, `describe_coverage_url` and Identify's `next_fetch`.
/// Supplied protocol bodies are parsed separately; Identify's input `capabilities` parameter
/// contains XML, while its result field of the same name is a GetCapabilities address.
pub(crate) fn redact_url_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if matches!(k.as_str(), "url" | "next" | "source_url" | "tilesets" | "describe" | "capabilities")
                    && let Some(s) = v.as_str()
                {
                    *v = Value::String(redact_output(s));
                } else if k == "request" && v.is_string() {
                    if let Some(s) = v.as_str() {
                        *v = Value::String(redact_output(s));
                    }
                } else {
                    redact_url_fields(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_url_fields),
        _ => {}
    }
}

fn text<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// The source a successful command added, if it adds one.
pub fn added_by(id: &str, p: &Value) -> Option<Source> {
    let src = |kind: &str, address: &str, name: Option<&str>, extra: Value| Source {
        kind: kind.into(),
        name: name.unwrap_or_default().to_string(),
        address: address.to_string(),
        extra,
    };
    let layer = |k: &str| text(p, k).map(|l| json!({k: l})).unwrap_or(Value::Null);
    match id {
        "layer.addData" => Some(src("file", text(p, "path")?, text(p, "name"), layer("layer"))),
        "layer.addUrl" | "layer.addDownload" => {
            let mut extra = serde_json::Map::new();
            if let Some(l) = text(p, "layer") {
                extra.insert("layer".into(), json!(l));
            }
            for flag in URL_FLAGS {
                if p.get(flag).and_then(crate::cmd::bool_value) == Some(true) {
                    extra.insert(flag.into(), json!(FLAG_ON));
                }
            }
            if extra.contains_key("coverage")
                && let Some(crs) = text(p, "coverageCrs")
            {
                extra.insert("coverageCrs".into(), json!(crs));
            }
            let extra = if extra.is_empty() { Value::Null } else { Value::Object(extra) };
            Some(src("url", text(p, "url")?, text(p, "name"), extra))
        }
        "map.addTiles" => {
            let url = text(p, "url")?;
            let mut extra = serde_json::Map::new();
            if let Some(k) = text(p, "kind") {
                extra.insert("kind".into(), json!(k));
            }
            Some(src("tiles", url, text(p, "name"), Value::Object(extra)))
        }
        "service.add" | "service.import" => {
            let mut extra = serde_json::Map::new();
            for k in ["layer", "type"] {
                if let Some(v) = text(p, k) {
                    extra.insert(k.into(), json!(v));
                }
            }
            Some(src("service", text(p, "url")?, None, Value::Object(extra)))
        }
        "layer.addDatabase" => Some(src("database", text(p, "database")?, None, layer("layer"))),
        "layer.addZarr" => {
            let address = text(p, "url").or_else(|| text(p, "path"))?;
            Some(src("zarr", address, None, text(p, "variable").map(|v| json!({"variable": v})).unwrap_or(Value::Null)))
        }
        "stac.addAsset" => Some(src("stac", text(p, "url")?, None, layer("asset"))),
        "stac.addCube" => {
            let mut extra = serde_json::Map::new();
            for k in ["collection", "asset"] {
                if let Some(v) = text(p, k) {
                    extra.insert(k.into(), json!(v));
                }
            }
            // No API given: Earth Search, as the command takes.
            Some(src("stac", text(p, "url").unwrap_or(crate::render::rustgis_tiles::stac::EARTH_SEARCH), None, Value::Object(extra)))
        }
        "layer.addSample" => Some(src("sample", text(p, "name")?, None, Value::Null)),
        _ => None,
    }
}

/// Well-known public sources, ready to add: basemap tiles, map and feature services, STAC APIs
/// and the sample data.
pub fn library() -> &'static [(&'static str, Source)] {
    static LIBRARY: std::sync::OnceLock<Vec<(&'static str, Source)>> = std::sync::OnceLock::new();
    LIBRARY.get_or_init(build_library)
}

fn build_library() -> Vec<(&'static str, Source)> {
    let s = |kind: &str, name: &str, address: &str, extra: Value| Source {
        kind: kind.into(),
        name: name.into(),
        address: address.into(),
        extra,
    };
    vec![
        (
            "Basemaps",
            s(
                "tiles",
                "OpenStreetMap",
                "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
                json!({"attribution": "© OpenStreetMap contributors", "maxZoom": "19"}),
            ),
        ),
        (
            "Imagery",
            s("service", "EOX Sentinel-2 cloudless", "https://tiles.maps.eox.at/wmts/1.0.0/WMTSCapabilities.xml", json!({"type": "wmts"})),
        ),
        (
            "Imagery",
            s(
                "service",
                "NASA GIBS (Web Mercator)",
                "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/1.0.0/WMTSCapabilities.xml",
                json!({"type": "wmts"}),
            ),
        ),
        (
            "Imagery",
            s(
                "service",
                "USGS Imagery",
                "https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer",
                json!({"type": "arcgis"}),
            ),
        ),
        (
            "Maps",
            s("service", "USGS Topo", "https://basemap.nationalmap.gov/arcgis/rest/services/USGSTopo/MapServer", json!({"type": "arcgis"})),
        ),
        (
            "Maps",
            s("service", "USGS Hydrography", "https://hydro.nationalmap.gov/arcgis/rest/services/nhd/MapServer", json!({"type": "arcgis"})),
        ),
        (
            "Maps",
            s("service", "GEBCO bathymetry", "https://wms.gebco.net/mapserv?request=getcapabilities&service=wms", json!({"type": "wms"})),
        ),
        (
            "Maps",
            s(
                "service",
                "swisstopo WMTS (geo.admin.ch)",
                "https://wmts.geo.admin.ch/EPSG/3857/1.0.0/WMTSCapabilities.xml",
                json!({"type": "wmts"}),
            ),
        ),
        // The same maps as images drawn on request: unlike the tiles, most layers answer Identify.
        (
            "Maps",
            s(
                "service",
                "swisstopo WMS (geo.admin.ch)",
                "https://wms.geo.admin.ch/?SERVICE=WMS&REQUEST=GetCapabilities&VERSION=1.3.0",
                json!({"type": "wms"}),
            ),
        ),
        ("Catalogs", s("stac", "Earth Search (AWS)", "https://earth-search.aws.element84.com/v1", json!({"api": "true"}))),
        (
            "Catalogs",
            s("stac", "Microsoft Planetary Computer", "https://planetarycomputer.microsoft.com/api/stac/v1", json!({"api": "true"})),
        ),
        ("Sample data", s("sample", "World Countries", "countries", Value::Null)),
        ("Sample data", s("sample", "Populated Places", "cities", Value::Null)),
        ("Sample data", s("sample", "Major Rivers", "rivers", Value::Null)),
        ("Sample data", s("sample", "Major Lakes", "lakes", Value::Null)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deeply_nested_capability_addresses_say_when_the_inner_address_is_omitted() {
        let public = "https://example.invalid/data.tif";
        assert_eq!(strip_capabilities(public), public);
        let prefix = crate::render::rustgis_tiles::s3::ANONYMOUS;
        for depth in [8, 32] {
            let nested = format!("{}{public}", prefix.repeat(depth));
            let shown = strip_capabilities(&nested);
            assert!(shown.ends_with("[address omitted]"), "{shown}");
            assert!(!shown.contains(public), "the unvisited inner address is not returned");
        }
    }

    #[test]
    fn restored_sources_never_reuse_a_previous_process_login_binding() {
        let address = rustgis_data::s3_address::bind("0123456789abcdef", "https://s3.example.org", "s3://data/a.fgb").unwrap();
        let source = Source { kind: "url".into(), name: "Saved".into(), address, extra: Value::Null };
        let text = serde_json::json!({"favourites": [source.clone()], "saved": [source.clone()], "recent": [source]}).to_string();
        let restored = Sources::from_json(&text);
        for source in restored.favourites.iter().chain(&restored.saved).chain(&restored.recent) {
            let bound = rustgis_data::s3_address::parse(&source.address).unwrap().unwrap();
            assert_eq!(bound.binding, "");
            assert_eq!(bound.endpoint, "https://s3.example.org");
            assert_eq!(bound.url, "s3://data/a.fgb");
        }
    }

    #[test]
    fn a_saved_archive_still_bounds_its_template_head() {
        let address = rustgis_data::s3_address::bind("", "", "s3://data/a.pmtiles").unwrap();
        let source = Source {
            kind: "url".into(),
            name: "Huge template".into(),
            address: format!("pmtiles:{}@{address}", "x".repeat(MAX_TEXT)),
            extra: Value::Null,
        };
        assert!(clean(source).is_err());
    }

    #[test]
    fn secrets_in_command_parameters_are_noticed() {
        use serde_json::json;
        for p in [
            json!({"database": "postgres:host=db dbname=gis user=ana", "password": "s3cret"}),
            json!({"database": "postgres:host=db dbname=gis user=ana password=s3cret"}),
            json!({"url": "https://ana:s3cret@example.com/data.geojson"}),
            json!({"url": "https://example.com/tiles/{z}/{x}/{y}.png?access_token=pk.abc"}),
            json!({"url": "https://maps.example.com/tiles/{z}/{x}/{y}.png?key=abc"}),
            json!({"url": "https://api.example.com/tiles", "key": "abc"}),
            json!({"source": {"url": "https://api.example.com/tiles"}, "key": "abc"}),
            // A plain key with no address near it, but reading as an access key.
            json!({"provider": "x", "key": "sk-4f9aB27c81dE0e5f3a6b9C"}),
            json!({"key": "pk.eyJ1IjoiYW5hIiwiYSI6ImNsMnQ4"}),
            // Mostly letters, mixed case, as random tokens are.
            json!({"key": "kXqPzLmNvBtRwYsHaJdFgQ"}),
            // A hex hash or id counts as a key on purpose (see `looks_like_a_key`).
            json!({"key": "3fa85f64-5717-4562-b3fc-2c963f66afa6"}),
            json!({"source": {"headers": {"X-API-Key": "abc"}}}),
            json!({"url": "https://example.com/data.geojson", "headers": {"Authorization": "Bearer abc"}}),
            json!({"headers": {"Cookie": "session=abc"}}),
            json!({"text": "https://ana:s3cret@example.com/x"}),
            json!({"url": format!("https://example.com/{}?sig=abc&sv=2024", "a".repeat(10_000))}),
            json!({"certificate": "-----BEGIN RSA PRIVATE KEY-----
MIIE
-----END RSA PRIVATE KEY-----"}),
        ] {
            assert!(holds_secret(&p), "{p}");
        }
        for p in [
            json!({"layer": "Lakes", "color": "#336699"}),
            json!({"tool": "buffer", "params": {"input": "rivers", "distance": 30, "output": "APP"}}),
            json!({"url": "https://example.com/data.geojson"}),
            json!({"database": "postgres:host=db dbname=gis user=ana", "password": ""}),
            json!({"url": "https://example.com/data.geojson", "token": false}),
            json!({"token": {}, "password": []}),
            json!({"layer": "Parcels", "key": "parcel_id"}),
            json!({"fileName": "big.geojson", "data": "A".repeat(100_000)}),
            // A file's contents, past what an address could be, isn't read as one.
            json!({"fileName": "a.geojson", "text": format!("{{\"type\": \"FeatureCollection\", \"note\": \"https://x.example/?token=t\", \"pad\": \"{}\"}}", "x".repeat(2000))}),
        ] {
            assert!(!holds_secret(&p), "{p}");
        }
        // A plain key is let through, and noticed so the user is told.
        assert!(holds_plain_key(&json!({"layer": "Parcels", "key": "parcel_id"})));
        // A file's contents beside a plain key is no address for it.
        assert!(!holds_secret(&json!({"key": "parcel_id", "source": {"data": "https://example.com/list"}})));
        assert!(!holds_secret(&json!({"key": "parcel_id", "text": "https://example.com/list"})));
        // Names stay plain keys, long or not.
        for name in [
            "parcel_id",
            "municipality_code_2020",
            "municipalityCode2020Total",
            "parcelLandUseAreaCode2020",
            "ParcelLandUseAreaCode",
            "CD_MUN",
            "nome do município com espaços longos",
            "area_2010_2020_km2_total",
        ] {
            assert!(!holds_secret(&json!({"layer": "Parcels", "key": name})), "{name}");
        }
        assert!(holds_plain_key(&json!({"source": [{"key": "abc"}]})));
        assert!(!holds_plain_key(&json!({"layer": "Parcels", "key": ""})));
        assert!(!holds_plain_key(&json!({"api_key": "abc"})));
    }

    #[test]
    fn postgres_connections_lose_every_secret() {
        // Quoted values with spaces and escapes, `key = value`, other secret keys, every form.
        for (given, kept) in [
            ("postgres:host=h password='my secret' user=u", "postgres:host=h user=u"),
            ("postgres:host=h password = 'a \\'b c' dbname=g", "postgres:host=h dbname=g"),
            ("postgresql:host=h sslpassword='k k' passfile=/p user=u", "postgres:host=h user=u"),
            ("PG:host=h dbname=g password=pw", "postgres:host=h dbname=g"),
            ("postgres:host=h password='unterminated user=u", "postgres:host=h"),
            ("postgresql://u:p w@h:5432/g?sslmode=require&password=x", "postgresql://u@h:5432/g?sslmode=require"),
        ] {
            let r = redact(given);
            assert_eq!(r, kept, "{given}");
            for secret in ["secret", "pw", "'b", "k k", "p w", "=x", "unterminated"] {
                assert!(!r.contains(secret), "{secret} in {r}");
            }
        }
    }

    #[test]
    fn stored_sources_join_what_the_session_has() {
        // A favourite made (and a source added) before the stored lists are read survives them.
        let mut s = Sources::default();
        s.keep("favourites", Source { kind: "file".into(), address: "/new.gpkg".into(), ..Source::default() }).unwrap();
        s.note(Source { kind: "file".into(), address: "/added.csv".into(), ..Source::default() });
        let stored = Sources::from_json(
            r#"{"favourites":[{"kind":"file","name":"","address":"/old.gpkg"},{"kind":"file","name":"","address":"/new.gpkg"}],"recent":[{"kind":"file","name":"","address":"/older.csv"}],"saved":[{"kind":"stac","name":"","address":"https://s/v1"}]}"#,
        );
        let before = s.generation;
        s.merge(stored);
        let fav: Vec<&str> = s.favourites.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(fav, ["/new.gpkg", "/old.gpkg"]);
        let recent: Vec<&str> = s.recent.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(recent, ["/added.csv", "/older.csv"]);
        assert_eq!(s.saved.len(), 1);
        assert!(s.generation > before);
    }

    /// Rows, fields and lists a newer RustGIS wrote are kept through recording a recent source
    /// and keeping a favourite here; a preference that can't be read at all is kept byte for
    /// byte. Clean input says nothing.
    #[test]
    fn sources_keep_what_this_version_cannot_read() {
        let future = json!({"kind": "gcs", "name": "Bucket", "address": "gs://b/a.tif"});
        let text = json!({
            "favourites": [{"kind": "file", "name": "A", "address": "/a.gpkg", "pinned": true}, future.clone()],
            "recent": [{"kind": "file", "name": "R", "address": "/r.csv", "opened": 3}],
            "saved": [],
            "folders": ["/data"]
        })
        .to_string();
        // With an editor's byte-order mark it reads the same.
        let bom = Sources::from_json(&format!("\u{feff}{text}"));
        assert_eq!((bom.favourites.len(), bom.unread().len()), (1, 2), "{:?}", bom.unread());
        let mut s = Sources::default();
        s.merge(Sources::from_json(&text));
        assert_eq!(s.unread().len(), 2, "{:?}", s.unread());
        assert!(s.unread()[0].starts_with("1 saved data source couldn't be read"), "{:?}", s.unread());
        s.note(Source { kind: "file".into(), address: "/new.csv".into(), ..Source::default() });
        s.keep("favourites", Source { kind: "file".into(), address: "/b.gpkg".into(), ..Source::default() }).unwrap();
        let saved: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(saved["folders"], json!(["/data"]));
        assert_eq!(saved["favourites"][0]["pinned"], true);
        assert_eq!(saved["favourites"][1]["address"], "/b.gpkg");
        assert_eq!(saved["favourites"][2], future);
        assert_eq!(saved["recent"][1]["opened"], 3);
        // A source that is no longer recent, or that is forgotten, takes its fields along.
        for i in 0..MAX_RECENT {
            s.note(Source { kind: "file".into(), address: format!("/{i}.csv"), ..Source::default() });
        }
        s.forget("favourites", "file", "/a.gpkg").unwrap();
        assert!(!s.to_json().contains("opened") && !s.to_json().contains("pinned"));
        assert!(s.to_json().contains("gs://b/a.tif"));

        for whole in ["[1]", r#"{"favourites": {"a": 1}}"#] {
            let mut s = Sources::default();
            s.merge(Sources::from_json(whole));
            assert!(matches!(s.unread().as_slice(), [one] if one.contains("couldn't be read")), "{whole}");
            s.note(Source { kind: "file".into(), address: "/new.csv".into(), ..Source::default() });
            assert_eq!(s.recent.len(), 1, "this session's sources are still listed");
            assert_eq!(s.to_json(), whole);
        }
        // Rows past a list's limit are kept, and said as that.
        let recent: Vec<Value> = (0..=MAX_RECENT).map(|i| json!({"kind": "file", "name": "", "address": format!("/{i}.csv")})).collect();
        let s = Sources::from_json(&json!({ "recent": recent }).to_string());
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert!(matches!(s.unread().as_slice(), [one] if one.starts_with("1 saved data source is past")), "{:?}", s.unread());
        assert!(s.to_json().contains(&format!("/{MAX_RECENT}.csv")));
        // What it can't read and that holds a secret isn't kept: this version can't strip it.
        let secret = json!({"token": "hidden", "saved": [
            {"kind": "url", "name": "A", "address": "https://a/x.fgb", "apiKey": "hidden"},
            {"kind": "gcs", "name": "B", "address": "https://g.example/b?sig=hidden"}
        ]});
        let s = Sources::from_json(&secret.to_string());
        assert!(
            s.unread().iter().any(|t| t.starts_with("3 values in the saved data sources hold a password, key or token")),
            "{:?}",
            s.unread()
        );
        assert!(!s.to_json().contains("hidden"));
        // A key beside a web address is one, in a readable source and among the preference's own keys.
        let beside = json!({"endpoint": "https://api.example", "key": "abc", "saved": [
            {"kind": "url", "name": "A", "address": "https://a/x.fgb", "mirror": "https://m.example", "key": "abc"}
        ]});
        let s = Sources::from_json(&beside.to_string());
        assert!(
            !s.to_json().contains("abc") && s.to_json().contains("api.example") && s.to_json().contains("m.example"),
            "{}",
            s.to_json()
        );
        assert!(s.unread().iter().any(|t| t.starts_with("2 values in the saved data sources hold")), "{:?}", s.unread());
        // A key beside an address among the preference's own keys.
        let top = json!({"address": "https://api.example", "key": "abc"});
        assert!(!Sources::from_json(&top.to_string()).to_json().contains("abc"));
        // Even when that address holds a secret itself.
        let top = json!({"address": "https://api.example?token=hidden", "key": "abc"});
        let saved = Sources::from_json(&top.to_string()).to_json();
        assert!(!saved.contains("abc") && !saved.contains("hidden"), "{saved}");
        // A change before the first read that fills a list doesn't drop what was stored.
        let stored: Vec<Value> =
            (0..MAX_KEPT).map(|i| json!({"kind": "file", "name": "", "address": format!("/{i}.csv"), "pinned": i})).collect();
        let mut s = Sources::default();
        s.keep("favourites", Source { kind: "file".into(), address: "/new.csv".into(), ..Source::default() }).unwrap();
        s.merge(Sources::from_json(&json!({ "favourites": stored }).to_string()));
        assert_eq!(s.favourites.len(), MAX_KEPT);
        assert!(s.unread()[0].starts_with("1 saved data source is past"), "{:?}", s.unread());
        assert!(s.to_json().contains(&format!("/{}.csv", MAX_KEPT - 1)) && s.to_json().contains(&format!("\"pinned\":{}", MAX_KEPT - 1)));
        // Rows of one identity are one source: the first row wins, with its fields, a later row is
        // dropped and said (whatever it holds), and forgetting it leaves nothing behind.
        for twins in [
            json!({"saved": [
                {"kind": "file", "name": "A", "address": "/a.csv", "future": 1},
                {"kind": "file", "name": "A", "address": "/a.csv", "future": 2, "more": 3, "token": "hidden", "endpoint": "https://api.example"}
            ]}),
            json!({"saved": [
                {"kind": "file", "name": "A", "address": "/a.csv", "key": "abc"},
                {"kind": "file", "name": "A", "address": "/a.csv", "endpoint": "https://api.example"}
            ]}),
            json!({"saved": [
                {"kind": "file", "name": "A", "address": "/a.csv", "extra": {"endpoint": "https://api.example"}},
                {"kind": "file", "name": "A", "address": "/a.csv", "key": "abc"}
            ]}),
            json!({"saved": [
                {"kind": "file", "name": "A", "address": "/a.csv", "key": "abc"},
                {"kind": "file", "name": "A", "address": "/a.csv", "extra": {"endpoint": "https://api.example"}}
            ]}),
        ] {
            let mut s = Sources::from_json(&twins.to_string());
            assert_eq!(s.saved.len(), 1);
            let saved: Value = serde_json::from_str(&s.to_json()).unwrap();
            assert_eq!(saved["saved"].as_array().map(Vec::len), Some(1));
            assert!(saved["saved"][0].get("more").is_none() && !s.to_json().contains("hidden"));
            // Nothing of a dropped row combines with the first: no key beside an endpoint.
            assert!(!(s.to_json().contains("abc") && s.to_json().contains("api.example")), "{}", s.to_json());
            assert_eq!(
                s.unread().first().map(String::as_str),
                Some("1 repeated saved data source was removed (the first of each is kept, with its settings)")
            );
            s.forget("saved", "file", "/a.csv").unwrap();
            assert!(!s.to_json().contains("/a.csv"));
        }
        // A source's known details can be changed after its fields were kept, as a key beside an
        // endpoint it now has: checked again when it is written, by keep and by merge.
        let a = || Source {
            kind: "file".into(),
            address: "/a.csv".into(),
            extra: json!({"endpoint": "https://api.example"}),
            ..Source::default()
        };
        let kept = json!({"favourites": [{"kind": "file", "name": "", "address": "/a.csv", "key": "abc", "future": 1}]}).to_string();
        let mut s = Sources::from_json(&kept);
        assert!(s.to_json().contains("abc"));
        s.keep("favourites", a()).unwrap();
        assert!(
            !s.to_json().contains("abc") && s.to_json().contains("\"future\":1") && s.to_json().contains("api.example"),
            "{}",
            s.to_json()
        );
        assert!(s.unread().iter().any(|t| t.starts_with("1 value in the saved data sources holds")), "{:?}", s.unread());
        let mut s = Sources::default();
        s.keep("favourites", a()).unwrap();
        s.merge(Sources::from_json(&kept));
        assert!(!s.to_json().contains("abc") && s.to_json().contains("api.example"), "{}", s.to_json());
        // A readable row kept past the limit gives its fields to the source when it is listed.
        let recent: Vec<Value> =
            (0..=MAX_RECENT).map(|i| json!({"kind": "file", "name": "", "address": format!("/{i}.csv"), "future": i})).collect();
        let mut s = Sources::from_json(&json!({ "recent": recent }).to_string());
        s.note(Source { kind: "file".into(), address: format!("/{MAX_RECENT}.csv"), ..Source::default() });
        let mut s = Sources::from_json(&s.to_json());
        s.note(Source { kind: "file".into(), address: format!("/{MAX_RECENT}.csv"), ..Source::default() });
        let saved: Value = serde_json::from_str(&Sources::from_json(&s.to_json()).to_json()).unwrap();
        assert_eq!(saved["recent"][0]["future"], json!(MAX_RECENT), "{saved}");
        assert_eq!(saved["recent"].as_array().map(Vec::len), Some(MAX_RECENT));
        // The same by merge: the session lists a source the stored preference kept past the limit.
        let mut s = Sources::default();
        s.note(Source { kind: "file".into(), address: format!("/{MAX_RECENT}.csv"), ..Source::default() });
        let recent: Vec<Value> =
            (0..=MAX_RECENT).map(|i| json!({"kind": "file", "name": "", "address": format!("/{i}.csv"), "future": i})).collect();
        s.merge(Sources::from_json(&json!({ "recent": recent }).to_string()));
        assert!(s.to_json().contains(&format!("\"future\":{MAX_RECENT}")), "{}", s.to_json());
        assert_eq!(s.to_json().matches(&format!("/{MAX_RECENT}.csv")).count(), 1, "{}", s.to_json());
        // The row's known details count beside an unknown key, as its address does.
        let extra = json!({"saved": [{"kind": "file", "name": "A", "address": "/a.csv", "extra": {"endpoint": "https://api.example"}, "key": "abc"}]});
        let saved = Sources::from_json(&extra.to_string()).to_json();
        assert!(!saved.contains("abc") && saved.contains("api.example"), "{saved}");
        // Sources merged past the limit are said by the merged sources.
        let mut s = Sources::default();
        s.keep("favourites", Source { kind: "file".into(), address: "/new.csv".into(), ..Source::default() }).unwrap();
        let stored: Vec<Value> = (0..MAX_KEPT).map(|i| json!({"kind": "file", "name": "", "address": format!("/{i}.csv")})).collect();
        s.merge(Sources::from_json(&json!({ "favourites": stored }).to_string()));
        assert!(s.unread().iter().any(|t| t.contains("past the lists' limits")), "{:?}", s.unread());
        // A row past the limit counts once: its repeat is said as repeated, not past the limit.
        let mut over: Vec<Value> = (0..MAX_KEPT).map(|i| json!({"kind": "file", "name": "", "address": format!("/{i}.csv")})).collect();
        over.extend([json!({"kind": "file", "name": "", "address": "/x.csv"}), json!({"kind": "file", "name": "", "address": "/x.csv"})]);
        let s = Sources::from_json(&json!({ "saved": over }).to_string());
        assert_eq!((s.kept.over, s.kept.repeated), (1, 1));
        assert!(s.unread().iter().any(|t| t.starts_with("1 saved data source is past the lists' limits")), "{:?}", s.unread());
        // A repeat past the limit is dropped, not kept as an overflow row. A bound address is the
        // bare one's identity: forgetting by either form removes a kept over-limit row of it, never
        // an unreadable row that merely shares the address.
        let bound = rustgis_data::s3_address::bind("0123456789abcdef", "https://s3.example.org", "s3://data/a.fgb").unwrap();
        let mut full: Vec<Value> = (0..MAX_KEPT).map(|i| json!({"kind": "file", "name": "", "address": format!("/{i}.csv")})).collect();
        full.push(json!({"kind": "file", "name": "", "address": "/0.csv", "token": "hidden"}));
        let s = Sources::from_json(&json!({"saved": full.clone()}).to_string());
        assert!(!s.to_json().contains("hidden") && s.kept.over == 0 && s.kept.repeated == 1);
        full.truncate(MAX_KEPT);
        full.push(json!({"kind": "url", "name": "", "address": bound}));
        let twin = rustgis_data::s3_address::bind("fedcba9876543210", "https://s3.example.org", "s3://data/a.fgb").unwrap();
        full.push(json!({"kind": "url", "name": "", "address": twin, "gcs": 1}));
        full.push(json!({"kind": "gcs", "name": "", "address": "gs://b/a.tif"}));
        full.push(json!({"kind": "future", "name": "", "address": twin}));
        full.push(json!({"kind": " url ", "name": 5, "address": twin}));
        let mut s = Sources::from_json(&json!({"saved": full}).to_string());
        // The twin repeats the bound row's identity, past the limit as it is: said as repeated.
        assert_eq!((s.kept.over, s.kept.repeated), (1, 1), "{:?}", s.unread());
        let generation = s.generation;
        // Only kept rows go: that is still a removal, and the pane must still save.
        assert!(s.forget("saved", "url", &twin).unwrap() && s.generation != generation);
        assert!(
            s.to_json().contains("gs://b/a.tif")
                && s.to_json().contains("\"future\"")
                && !s.to_json().contains("\"gcs\":1")
                && s.to_json().contains("\"name\":5")
        );
        assert_eq!(s.kept.over, 0);
        // A login binding is the app's own output: cleared on read without a notice.
        let s = Sources::from_json(&json!({"saved": [{"kind": "url", "name": "B", "address": bound}]}).to_string());
        assert!(
            s.unread().is_empty() && rustgis_data::s3_address::parse(&s.saved[0].address).unwrap().unwrap().binding.is_empty(),
            "{:?}",
            s.unread()
        );
        // A file's contents beside a field isn't an address for it.
        let file = json!({"data": "https://example.invalid/a.csv", "key": "feature_id"});
        assert!(Sources::from_json(&file.to_string()).to_json().contains("feature_id"));
        // An address with a signed query is changed, and said.
        let signed = json!({"saved": [{"kind": "url", "name": "U", "address": "https://a/x.fgb?sig=hidden"}]});
        let s = Sources::from_json(&signed.to_string());
        assert!(matches!(s.unread().as_slice(), [one] if one.starts_with("1 saved data source has")), "{:?}", s.unread());
        // Details and a name this version cuts down when it reads them are said.
        let long = "n".repeat(300);
        let cut = json!({"saved": [
            {"kind": "service", "name": "S", "address": "https://h/wms", "extra": {"type": "wms", "future": {"mode": 1}}},
            {"kind": "file", "name": long, "address": "/a.csv"}
        ]});
        let s = Sources::from_json(&cut.to_string());
        assert!(matches!(s.unread().as_slice(), [one] if one.starts_with("2 saved data sources have")), "{:?}", s.unread());
        for clean in [
            "",
            r#"{"favourites": [], "recent": [], "saved": []}"#,
            r#"{"recent": [{"kind": "file", "name": " R ", "address": "/r.csv", "extra": {}}]}"#,
            r#"{"saved": [{"kind": "service", "name": "", "address": "https://h/wms", "extra": {"type": "wms"}}]}"#,
        ] {
            assert!(Sources::from_json(clean).unread().is_empty(), "{clean}");
        }
    }

    #[test]
    fn identity_is_kind_and_address() {
        let mut s = Sources::default();
        let svc = |extra: Value| Source { kind: "service".into(), name: "W".into(), address: "https://h/wms".into(), extra };
        s.keep("saved", svc(json!({"type": "wms"}))).unwrap();
        s.keep("saved", svc(json!({"type": "wms", "layer": "roads"}))).unwrap();
        // The same kind and address: one entry, with the last one's extra.
        assert_eq!(s.saved.len(), 1);
        assert_eq!(s.saved[0].extra, json!({"type": "wms", "layer": "roads"}));
        // Saved again, a source keeps its place in the list.
        s.keep("saved", Source { kind: "file".into(), address: "/z.gpkg".into(), ..Source::default() }).unwrap();
        // What `keep` gives back is the entry stored, even when it sits earlier in the list.
        let kept = s.keep("saved", svc(json!({"type": "wmts"}))).unwrap();
        assert_eq!((kept.address.as_str(), &kept.extra), ("https://h/wms", &json!({"type": "wmts"})));
        assert_eq!(s.saved.last().map(|l| l.address.as_str()), Some("/z.gpkg"));
        assert_eq!(s.saved[0].address, "https://h/wms");
        assert_eq!(s.saved[0].extra, json!({"type": "wmts"}));
        assert!(s.forget("saved", "file", "/z.gpkg").unwrap());
        // Another kind at the same address is another source.
        s.keep("saved", Source { kind: "url".into(), address: "https://h/wms".into(), ..Source::default() }).unwrap();
        assert_eq!(s.saved.len(), 2);
        assert!(s.forget("saved", "service", "https://h/wms").unwrap());
        assert_eq!(s.saved.len(), 1);
        s.note(svc(Value::Null));
        s.note(svc(json!({"layer": "x"})));
        assert_eq!(s.recent.len(), 1);
        assert!(s.recent[0].is("service", "https://h/wms"));
    }

    #[test]
    fn address_messages_stay_private_and_named_once_across_hops() {
        for address in [
            "https://alice:hidden@host.example/data?token=hidden hidden-tail",
            "https://host.example/data?page=2&token=hidden",
            "https://host.example/data?custom=hidden hidden-tail",
        ] {
            let mut message = message_with_address(address, "HTTP 503 Service Unavailable");
            for _ in 0..3 {
                message = message_with_address(address, &message);
                let clean = crate::Session::redacted(&message);
                assert!(clean.as_str().contains("HTTP 503 Service Unavailable"), "{clean:?}");
                assert_eq!(clean.as_str().matches("host.example/data").count(), 1, "{clean:?}");
                assert!(!clean.as_str().contains("hidden") && !clean.as_str().contains("alice"), "{clean:?}");
            }
        }
    }

    #[test]
    fn unrelated_target_failures_retain_the_requested_source() {
        let requested = "https://source.example/roads.shp?token=hidden-source";
        let target = "http://127.0.0.1/elsewhere.dbf?token=hidden-target";
        let cause = crate::render::rustgis_tiles::address::check(target, Some(requested)).unwrap_err();
        let mut message = message_with_address(requested, &cause);
        for _ in 0..3 {
            assert!(message.contains("source.example/roads.shp") && message.contains("127.0.0.1/elsewhere.dbf"), "{message}");
            assert!(message.contains(&cause) && !message.contains("hidden"), "{message}");
            assert_eq!(message.matches("source.example/roads.shp").count(), 1, "{message}");
            assert_eq!(crate::Session::redacted(&message).as_str(), message);
            message = message_with_address(requested, &message);
        }
    }

    #[test]
    fn shapefile_parts_with_secret_queries_are_named_once() {
        let shp = "https://h.example/roads.shp?token=hidden";
        let part = "https://h.example/roads.dbf?token=hidden";
        // A part failure as the data layer writes it, passed back through the source's helper.
        let label = rustgis_data::diagnostics::message_with_address(part, "http status: 404");
        let message = message_with_address(shp, &label);
        assert!(message.contains("roads.dbf") && message.contains("404") && !message.contains("hidden"), "{message}");
        assert_eq!(message.matches("://").count(), 1, "{message}");
    }

    #[test]
    fn secrets_are_never_kept() {
        assert_eq!(redact("https://me:pw@host.example/a?token=abc&layer=x&X-Amz-Signature=1#k"), "https://host.example/a?layer=x");
        // A port and an `@` later in the address: an ordinary address, left whole.
        assert_eq!(redact("https://host.example:8080/@user/x"), "https://host.example:8080/@user/x");
        assert_eq!(redact("https://h:80/x?email=a@b"), "https://h:80/x?email=a@b");
        assert_eq!(redact("https://[::1]:8080/@a"), "https://[::1]:8080/@a");
        // An empty port is a port too.
        assert_eq!(redact("https://host:/x?email=a@b"), "https://host:/x?email=a@b");
        // A PostgreSQL URI's password, whatever it holds; its fragment never; libpq's hosts kept.
        for (given, kept) in [
            ("postgresql://u:p/w@db.example/gis", "postgresql://u@db.example/gis"),
            ("postgresql://u:p?w@db.example/gis", "postgresql://u@db.example/gis"),
            ("postgresql://u:p#w@db.example/gis", "postgresql://u@db.example/gis"),
            ("postgresql://u:pw@db.example:5432/gis#x", "postgresql://u@db.example:5432/gis"),
            ("postgresql://db.example:5432/gis", "postgresql://db.example:5432/gis"),
            ("postgresql://h1:5432,h2:5432/db?options=a@b", "postgresql://h1:5432,h2:5432/db?options=a@b"),
            // A password of digits with an unencoded `/` reads like a port: a database's name
            // holds no `@`, so the password still goes.
            ("postgresql://u:123/w@db.example/gis", "postgresql://u@db.example/gis"),
            // No credentials: the fragment goes all the same.
            ("postgresql://db.example/gis#x", "postgresql://db.example/gis"),
        ] {
            assert_eq!(redact(given), kept, "{given}");
        }
        // The user and password go, whatever the password holds (an unencoded `?`, `/`, `#`, `@`).
        for (given, kept) in [
            ("https://u:p?w@host.example/x", "https://host.example/x"),
            ("https://u:p/w@host.example/x", "https://host.example/x"),
            ("https://u:p#w@host.example/x", "https://host.example/x"),
            ("https://u:p@w@host.example/x", "https://host.example/x"),
            ("https://u@host.example/x", "https://host.example/x"),
            ("https://host.example/@user/x", "https://host.example/@user/x"),
        ] {
            assert_eq!(redact(given), kept, "{given}");
        }
        // Short keys are secrets only where they are: an ArcGIS `sr` and a WMS `code` stay; an Azure
        // SAS's fields and an OAuth redirect's code go.
        assert_eq!(
            redact("https://h/arcgis/rest/services/x/MapServer/export?bbox=1&sr=3857&f=image"),
            "https://h/arcgis/rest/services/x/MapServer/export?bbox=1&sr=3857&f=image"
        );
        assert_eq!(redact("https://h/wms?service=WMS&code=EPSG:4326"), "https://h/wms?service=WMS&code=EPSG:4326");
        assert_eq!(
            redact("https://a.blob.core.windows.net/c/x.tif?sv=2024&sr=b&sp=r&se=2026&sig=abc&layer=1"),
            "https://a.blob.core.windows.net/c/x.tif?layer=1"
        );
        assert_eq!(redact("https://h/cb?code=xyz&state=s&tile=1"), "https://h/cb?state=s&tile=1");
        for k in ["Refresh_Token", "x-api-key", "API-KEY", "client_secret", "accessKey", "X-Amz-Credential", "x-amz-security-token", "pwd"]
        {
            assert_eq!(redact(&format!("https://h/x?{k}=v&a=1")), "https://h/x?a=1", "{k}");
        }
        // A fragment is never kept (OAuth and SAS-style links put tokens there).
        for f in ["#Access_Token=t", "#sig=s", "#password=p", "#id_token=i", "#map=4/1/2"] {
            assert_eq!(redact(&format!("https://h.example/x.fgb{f}")), "https://h.example/x.fgb", "{f}");
        }
        // Unreadable connections, web addresses and wrappers cannot become saved sources.
        for (kind, bare) in [
            ("database", "postgres:dbname"),
            ("database", "PG:"),
            ("database", "postgres:password='x"),
            ("url", "https:///path?token=RGSYNTH_unusable_source_20261104"),
            ("tiles", "pmtiles:unreadable"),
        ] {
            assert!(clean(Source { kind: kind.into(), address: bare.into(), ..Source::default() }).is_err(), "{bare}");
        }
        assert_eq!(
            redact("https://acct.blob.core.windows.net/c/a.tif?se=2026&sp=r&sv=1&sig=zzz"),
            "https://acct.blob.core.windows.net/c/a.tif"
        );
        assert_eq!(
            redact("https://m.example/cgi-bin/mapserv?map=/m.map&SERVICE=WMS"),
            "https://m.example/cgi-bin/mapserv?map=/m.map&SERVICE=WMS"
        );
        assert_eq!(redact("postgres:host=h dbname=g password=pw user=u"), "postgres:host=h dbname=g user=u");
        assert_eq!(redact("postgresql://u:pw@h:5432/g"), "postgresql://u@h:5432/g");
        assert_eq!(redact("/data/places.gpkg"), "/data/places.gpkg");
        let s = clean(Source {
            kind: "service".into(),
            name: String::new(),
            address: "https://x.example/wms?apikey=1".into(),
            extra: json!({"type": "wms", "accessToken": "t", "n": 3}),
        })
        .unwrap();
        assert_eq!((s.address.as_str(), s.name.as_str()), ("https://x.example/wms", "wms"));
        assert_eq!(s.extra, json!({"type": "wms"}));
        assert!(clean(Source { kind: "nope".into(), address: "x".into(), ..Source::default() }).is_err());
        assert!(clean(Source { kind: "file".into(), address: " ".into(), ..Source::default() }).is_err());
    }

    #[test]
    fn a_scheme_alone_in_prose_is_no_address() {
        // Notices and errors go through the one diagnostic redactor: a supported scheme alone in
        // prose names no address and is kept.
        for text in [
            "give an http://, https:// or s3:// address",
            "needs an http:// or https:// address",
            "uses unencrypted http://. Tick the confirmation",
            "only http:// and https:// addresses are fetched",
            "(http://) or \"https://\" or 's3://'",
            "HTTP:// and Https://",
        ] {
            assert_eq!(crate::Session::redacted(text).as_str(), text);
            // An error naming the address it refused keeps those words, and its key stays out.
            let error = message_with_address("https://h.example/a?token=hidden", text);
            assert_eq!(error, format!("https://h.example/a[query omitted]: {text}"));
            assert_eq!(crate::Session::redacted(&error).as_str(), error);
        }
        // Anything after the scheme is an address: what can hold a credential goes. A hint's
        // `https://…` is read as its scheme; other schemes are taken for addresses.
        for (text, kept) in [
            ("for example https://…", "for example https:"),
            ("file:// and ftp://", "[address omitted] and [address omitted]"),
            ("see https://user:pw@host/x, or http://", "see https://host/x, or http://"),
            ("https://user:pw@", "https:"),
            ("https://user:pw@…", "https:"),
            ("https://:pw@ failed", "https: failed"),
            ("https://?token=x", "[address omitted][query and the rest omitted]"),
            ("https://#x", "https:"),
            ("https://…?token=x", "[address omitted][query and the rest omitted]"),
            ("http:///", "http:"),
            ("s3://bucket/a.tif?X-Amz-Signature=s", "s3://bucket/a.tif[query and the rest omitted]"),
        ] {
            assert_eq!(crate::Session::redacted(text).as_str(), kept, "{text}");
        }
    }

    #[test]
    fn recent_keeps_each_once_newest_first() {
        let mut s = Sources::default();
        for a in ["/a.geojson", "/b.geojson", "/a.geojson"] {
            s.note(Source { kind: "file".into(), address: a.into(), ..Source::default() });
        }
        let names: Vec<&str> = s.recent.iter().map(|r| r.address.as_str()).collect();
        assert_eq!(names, ["/a.geojson", "/b.geojson"]);
        for i in 0..40 {
            s.note(Source { kind: "file".into(), address: format!("/{i}.csv"), ..Source::default() });
        }
        assert_eq!(s.recent.len(), MAX_RECENT);
        // Read back from preferences someone edited: bad entries are dropped, secrets removed.
        let text = r#"{"favourites":[{"kind":"url","name":"x","address":"https://h/x.fgb?token=s"},{"kind":"bad","name":"","address":"y"}],"recent":[1,2]}"#;
        let back = Sources::from_json(text);
        assert_eq!(back.favourites.len(), 1);
        assert_eq!(back.favourites[0].address, "https://h/x.fgb");
        assert!(back.recent.is_empty());
        // The unreadable rows, and the address cut of its token.
        assert_eq!(back.unread().len(), 2, "{:?}", back.unread());
        let unreadable = Sources::from_json("not json");
        assert!(unreadable.favourites.is_empty() && unreadable.recent.is_empty() && unreadable.saved.is_empty());
    }

    #[test]
    fn commands_name_what_they_added() {
        let s =
            added_by("layer.addDatabase", &json!({"database": "postgres:host=h dbname=g password=pw", "password": "pw", "layer": "roads"}))
                .unwrap();
        let s = clean(s).unwrap();
        assert_eq!(s.address, "postgres:host=h dbname=g");
        assert_eq!(s.extra, json!({"layer": "roads"}));
        assert!(!serde_json::to_string(&s).unwrap().contains("pw"));
        // A MySQL address keeps its user, never its password (in the user part or the query).
        let s = added_by("layer.addDatabase", &json!({"database": "mysql://ana:p%40w#x@db:3306/gis?pass=pw2", "password": "pw"})).unwrap();
        let s = clean(s).unwrap();
        assert_eq!(s.address, "mysql://ana@db:3306/gis");
        assert!(!serde_json::to_string(&s).unwrap().contains("pw"));
        assert!(holds_secret(&json!({"database": "mariadb://ana:pw@db/gis"})));
        assert!(!holds_secret(&json!({"database": "mariadb://ana@db/gis"})));
        let s = added_by(
            "layer.addDatabase",
            &json!({"database": "mssql://ana:p%40w@db:1433/gis?password=pw2&encrypt=strict", "password": "pw"}),
        )
        .unwrap();
        let s = clean(s).unwrap();
        assert_eq!(s.address, "mssql://ana@db:1433/gis?encrypt=strict");
        assert!(!serde_json::to_string(&s).unwrap().contains("pw"));
        assert!(holds_secret(&json!({"database": "mssql://ana:pw@db/gis"})));
        assert!(!holds_secret(&json!({"database": "mssql://ana@db/gis"})));
        for key in ["pwd", "passwd", "token", "access_token", "client_secret"] {
            let params = json!({"database": format!("mssql://ana@db/gis?{key}=query-secret&encrypt=strict")});
            assert!(holds_secret(&params), "{key}");
            let saved = clean(added_by("layer.addDatabase", &params).unwrap()).unwrap();
            assert_eq!(saved.address, "mssql://ana@db/gis?encrypt=strict", "{key}");
            assert!(!serde_json::to_string(&saved).unwrap().contains("query-secret"), "{key}");
        }
        assert_eq!(
            added_by("service.add", &json!({"url": "https://h/wms", "layer": "a", "type": "wms"})).unwrap().extra,
            json!({"layer": "a", "type": "wms"})
        );
        let service = json!({"url": "https://h/collections/a/items", "name": "A", "service": true});
        assert_eq!(added_by("layer.addUrl", &service).unwrap().extra, json!({"service": "true"}));
        assert_eq!(clean(added_by("layer.addDownload", &service).unwrap()).unwrap().extra, json!({"service": "true"}));
        assert_eq!(added_by("layer.addUrl", &json!({"url": "https://h/a.geojson", "service": false})).unwrap().extra, Value::Null);
        let coverage = json!({"url": "https://h/wcs?REQUEST=GetCoverage", "coverage": true, "coverageCrs": "EPSG:2056"});
        let kept = clean(added_by("layer.addDownload", &coverage).unwrap()).unwrap();
        assert_eq!(kept.extra, json!({"coverage": "true", "coverageCrs": "EPSG:2056"}));
        assert!(kept.flag("coverage") && !kept.flag("service"));
        // A system without the coverage flag isn't one the address is read in.
        assert_eq!(added_by("layer.addUrl", &json!({"url": "https://h/a.tif", "coverageCrs": "EPSG:2056"})).unwrap().extra, Value::Null);
        assert!(added_by("layer.remove", &json!({})).is_none());
        assert!(added_by("layer.addData", &json!({})).is_none());
        assert!(library().iter().all(|(_, s)| clean(s.clone()).is_ok()));
    }
}
