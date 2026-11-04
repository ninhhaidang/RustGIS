//! Static STAC catalogs: a tree of `catalog.json` and `collection.json` documents linked by
//! `child` links, with items behind `item` links, served as plain files (Planet's disaster data,
//! Vantor's events, the catalogs in the Portolan registry). There is no search: the tree is walked
//! from a node, each document read once, and the items found are filtered here by place, date,
//! cloud cover and phase. Like the rest of [`super`], this is sans I/O: [`Walk`] says what to
//! read next and takes the answers. Documents are hostile input: the walk is bounded in documents
//! read, depth, links followed and items kept.
//! A fetched document's login binding follows only links on its canonical origin.

use std::collections::{HashSet, VecDeque};

use serde_json::{Value, json};

use super::{Item, Search, datetime, is_web, link, resolve, text};

/// The most documents a walk reads.
pub const MAX_READS: usize = 400;
/// How deep a walk goes below its start.
pub const MAX_DEPTH: usize = 8;
/// The most links read from one document.
const MAX_LINKS: usize = 5000;
/// The most documents waiting to be read.
const MAX_QUEUE: usize = 20_000;
/// The most documents read at once.
pub const AT_ONCE: usize = 6;
/// The most children listed for a catalog.
const MAX_CHILDREN: usize = 2000;

/// Whether an address names a static catalog's document (a `.json` file) rather than a STAC API.
pub fn is_static(url: &str) -> bool {
    let url = crate::logins::inner(url);
    let path = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/').to_ascii_lowercase();
    path.ends_with(".json")
}

/// A catalog or collection below another: its title and address.
#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    pub title: String,
    pub url: String,
}

/// A label for a document without a title: its folder's name for a `catalog.json` or
/// `collection.json`, else its file's name without `.json`.
pub fn label_of(url: &str) -> String {
    let url = crate::logins::inner(url);
    let path = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or("");
    let lower = file.to_ascii_lowercase();
    let name = if matches!(lower.as_str(), "catalog.json" | "collection.json") { parts.next().unwrap_or(file) } else { file };
    let name = name.strip_suffix(".json").unwrap_or(name).replace(['-', '_'], " ");
    let name = name.trim();
    if name.is_empty() { url.chars().take(200).collect() } else { name.chars().take(200).collect() }
}

/// The children (`child` links) of a catalog's document fetched from `from`: web addresses only,
/// sorted by title.
pub fn parse_children(bytes: &[u8], from: &str) -> Result<Vec<Child>, String> {
    let v = super::json(bytes)?;
    if !v.is_object() || !matches!(v.get("type").and_then(Value::as_str), Some("Catalog" | "Collection")) {
        return Err("the answer isn't a STAC catalog or collection".into());
    }
    let mut seen = HashSet::new();
    let mut out: Vec<Child> = links(&v, "child", from)
        .into_iter()
        .filter(|(url, _)| is_web(url) && crate::address::check(url, Some(from)).is_ok() && seen.insert(url.clone()))
        .take(MAX_CHILDREN)
        .map(|(url, title)| Child { title: if title.is_empty() { label_of(&url) } else { title }, url })
        .collect();
    out.sort_by_key(|c| c.title.to_lowercase());
    Ok(out)
}

/// The items (`item` links) of a catalog's or collection's document fetched from `from`: web
/// addresses only, in the order the document gives them.
pub fn parse_item_links(bytes: &[u8], from: &str) -> Result<Vec<Child>, String> {
    let v = super::json(bytes)?;
    if !v.is_object() || !matches!(v.get("type").and_then(Value::as_str), Some("Catalog" | "Collection")) {
        return Err("the answer isn't a STAC catalog or collection".into());
    }
    let mut seen = HashSet::new();
    Ok(links(&v, "item", from)
        .into_iter()
        .filter(|(url, _)| is_web(url) && crate::address::check(url, Some(from)).is_ok() && seen.insert(url.clone()))
        .take(MAX_CHILDREN)
        .map(|(url, title)| Child { title: if title.is_empty() { label_of(&url) } else { title }, url })
        .collect())
}

/// The links of a relation, resolved against `from`: (address, title), the first
/// [`MAX_LINKS`] of them (counted among that relation's alone).
fn links(v: &Value, rel: &str, from: &str) -> Vec<(String, String)> {
    of_rel(v, rel).take(MAX_LINKS).filter_map(|l| Some((resolve(from, l.get("href")?.as_str()?)?, text(l.get("title"))))).collect()
}

/// A document's links of a relation.
fn of_rel<'a>(v: &'a Value, rel: &'a str) -> impl Iterator<Item = &'a Value> {
    v.get("links").and_then(Value::as_array).into_iter().flatten().filter(move |l| l.get("rel").and_then(Value::as_str) == Some(rel))
}

/// Whether a document has more links of a relation than a walk follows.
fn too_many(v: &Value, rel: &str) -> bool {
    of_rel(v, rel).nth(MAX_LINKS).is_some()
}

/// What a walk keeps of the items it finds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// West, south, east, north: items whose bbox meets it (items without one are kept).
    pub bbox: Option<[f64; 4]>,
    /// The first and last instants, in seconds since 1970 (either open): items whose date falls
    /// between (undated items, and dates that can't be read, are kept, as items without a bbox
    /// are).
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// The most cloud cover, in percent (items that don't say are kept).
    pub max_cloud: Option<f64>,
    /// `pre` or `post`: an event's imagery from before or after it (`phase`, as Vantor's items
    /// say; items that don't say are kept).
    pub phase: Option<String>,
}

impl Filter {
    /// The filter a search describes (its collections are the catalog's nodes, not a filter).
    pub fn from_search(s: &Search, phase: Option<&str>) -> Result<Filter, String> {
        let (from, to) = match s.datetime.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => {
                let d = datetime(d)?;
                let (a, b) = d.split_once('/').unwrap_or((d.as_str(), d.as_str()));
                let bound = |x: &str| -> Result<Option<i64>, String> {
                    if x == ".." {
                        return Ok(None);
                    }
                    instant(x).map(Some).ok_or_else(|| format!("{x}: not a date and time RustGIS reads"))
                };
                (bound(a)?, bound(b)?)
            }
            None => (None, None),
        };
        if let Some([w, south, e, n]) = s.bbox {
            let ok = |x: f64, lim: f64| x.is_finite() && (-lim..=lim).contains(&x);
            if !(ok(w, 180.0) && ok(e, 180.0) && ok(south, 90.0) && ok(n, 90.0) && south < n) {
                return Err("bbox is [west, south, east, north] in longitude/latitude".into());
            }
        }
        let phase = phase.map(|p| p.trim().to_ascii_lowercase()).filter(|p| !p.is_empty() && p != "any" && p != "all");
        if let Some(p) = &phase
            && p != "pre"
            && p != "post"
        {
            return Err(format!("{p}: phase is pre or post"));
        }
        Ok(Filter { bbox: s.bbox, from, to, max_cloud: s.max_cloud.filter(|c| c.is_finite()), phase })
    }

    /// Whether an item is kept (`phase` is its `phase` property, if it has one).
    fn keeps(&self, item: &Item, phase: Option<&str>) -> bool {
        if let (Some([w, s, e, n]), Some([iw, is, ie, inn])) = (self.bbox, item.bbox) {
            // A box over the antimeridian (west east of east), the filter's or the item's, is two:
            // up to 180° and from -180°.
            let spans = |w: f64, e: f64| if w <= e { vec![(w, e)] } else { vec![(w, 180.0), (-180.0, e)] };
            let meets = spans(w, e).iter().any(|(a, b)| spans(iw, ie).iter().any(|(c, d)| d >= a && c <= b));
            if !meets || is > n || inn < s {
                return false;
            }
        }
        if let (Some(max), Some(c)) = (self.max_cloud, item.cloud)
            && c > max
        {
            return false;
        }
        if let (Some(want), Some(p)) = (&self.phase, phase) {
            let p = p.trim().to_ascii_lowercase();
            if p.trim_end_matches("-event") != want {
                return false;
            }
        }
        // Compared as instants, whatever their offsets; a bare date is its day's start.
        if let Some(d) = instant(&item.datetime)
            && (self.from.is_some_and(|f| d < f) || self.to.is_some_and(|t| d > t))
        {
            return false;
        }
        true
    }
}

/// An RFC 3339 date and time (`2024-06-02T10:30:00.5Z`, `2024-06-02T12:30:00+02:00`) or a bare
/// date (its start, in UTC) as seconds since 1970; `None` for anything else.
pub fn instant(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    let num = |a: usize, n: usize| -> Option<i64> {
        let t = s.get(a..a + n)?;
        t.bytes().all(|c| c.is_ascii_digit()).then(|| t.parse().ok()).flatten()
    };
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let (y, m, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_days = match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=12).contains(&m) || !(1..=month_days).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    if s.len() == 10 {
        return Some(days * 86_400);
    }
    if !matches!(b.get(10), Some(b'T' | b't' | b' ')) || b.get(13) != Some(&b':') || b.get(16) != Some(&b':') {
        return None;
    }
    let (hh, mm, ss) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // A fraction of a second, then the offset.
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let offset = match b.get(i) {
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if i + 6 == b.len() && b.get(i + 3) == Some(&b':') => {
            let o = num(i + 1, 2)? * 3600 + num(i + 4, 2)? * 60;
            if *sign == b'+' { o } else { -o }
        }
        // No offset: taken as UTC.
        None => 0,
        _ => return None,
    };
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - offset)
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian), after Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A walk of a static catalog from one node: the documents still to read, and the items found.
#[derive(Debug)]
pub struct Walk {
    /// Items to read (before more catalogs, so the first found come quickly), with their depth.
    items: VecDeque<(String, usize)>,
    /// Catalogs and collections to read, with their depth.
    folders: VecDeque<(String, usize)>,
    /// Addresses handed out to read.
    visited: HashSet<String>,
    /// Addresses handed out and not yet answered: their depth.
    asked: Vec<(String, usize)>,
    filter: Filter,
    limit: usize,
    found: Vec<Item>,
    /// Documents read (or failed).
    pub reads: usize,
    /// Documents that couldn't be read or weren't STAC.
    pub failed: usize,
    /// Whether some links weren't followed (too deep, too many).
    pub cut: bool,
}

impl Walk {
    /// A walk from the catalog, collection or item at `root`, keeping at most `limit` items.
    pub fn new(root: &str, filter: Filter, limit: usize) -> Walk {
        Walk {
            items: VecDeque::new(),
            folders: VecDeque::from([(root.trim().to_string(), 0)]),
            visited: HashSet::new(),
            asked: Vec::new(),
            filter,
            limit: limit.clamp(1, super::MAX_ITEMS),
            found: Vec::new(),
            reads: 0,
            failed: 0,
            cut: false,
        }
    }

    /// The next document to read, if the walk wants one: none once enough items are found, the
    /// budget is spent or nothing is left (some may still be waiting for their answers).
    pub fn next_url(&mut self) -> Option<String> {
        while self.found.len() < self.limit && self.reads + self.asked.len() < MAX_READS {
            let (url, depth) = self.items.pop_front().or_else(|| self.folders.pop_front())?;
            if !self.visited.insert(url.clone()) {
                continue;
            }
            self.asked.push((url.clone(), depth));
            return Some(url);
        }
        None
    }

    /// How many documents are asked for and not yet answered.
    pub fn waiting(&self) -> usize {
        self.asked.len()
    }

    /// Whether the walk is over: nothing waiting, and nothing more it will read.
    pub fn finished(&self) -> bool {
        self.asked.is_empty()
            && (self.found.len() >= self.limit || self.reads >= MAX_READS || (self.items.is_empty() && self.folders.is_empty()))
    }

    /// Whether everything below the start was read (so the items are all that match).
    pub fn complete(&self) -> bool {
        self.finished() && self.items.is_empty() && self.folders.is_empty() && !self.cut && self.failed == 0
    }

    /// The items found.
    pub fn items(&self) -> &[Item] {
        &self.found
    }

    /// The items found, taken: by date, then id (documents are read several at a time, so the
    /// order they were found in isn't the catalog's).
    pub fn into_items(mut self) -> Vec<Item> {
        self.found.sort_by(|a, b| a.datetime.cmp(&b.datetime).then_with(|| a.id.cmp(&b.id)));
        self.found
    }

    /// Takes the answer for `url` (handed out by [`Walk::next_url`]).
    pub fn land(&mut self, url: &str, answer: Result<Vec<u8>, String>) {
        let Some(i) = self.asked.iter().position(|(u, _)| u == url) else { return };
        let (_, depth) = self.asked.swap_remove(i);
        self.reads += 1;
        let Some(v) = answer.ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).filter(Value::is_object) else {
            self.failed += 1;
            return;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("Feature") => self.keep(&v, url),
            Some(kind @ ("Catalog" | "Collection")) => {
                // A collection whose data are its own assets (as Portolan's are), not items'.
                if kind == "Collection"
                    && links(&v, "item", url).is_empty()
                    && let Some(c) = collection_item(&v, url)
                {
                    self.keep(&c, url);
                }
                if depth >= MAX_DEPTH {
                    self.cut |= v.get("links").and_then(Value::as_array).is_some_and(|l| !l.is_empty());
                    return;
                }
                for (rel, queue) in [("item", true), ("child", false)] {
                    // Links past the most followed leave the walk incomplete, and say so.
                    self.cut |= too_many(&v, rel);
                    for (u, _) in links(&v, rel, url) {
                        // A document on the web can't send the walk into the local network.
                        if !is_web(&u) || self.visited.contains(&u) || crate::address::check(&u, Some(url)).is_err() {
                            continue;
                        }
                        if self.items.len() + self.folders.len() >= MAX_QUEUE {
                            self.cut = true;
                            break;
                        }
                        if queue { self.items.push_back((u, depth + 1)) } else { self.folders.push_back((u, depth + 1)) }
                    }
                }
            }
            _ => self.failed += 1,
        }
    }

    fn keep(&mut self, v: &Value, from: &str) {
        if self.found.len() >= self.limit {
            return;
        }
        let Some(item) = super::item(v, Some(from)) else {
            self.failed += 1;
            return;
        };
        let phase = v.get("properties").and_then(|p| p.get("phase")).and_then(Value::as_str);
        if self.filter.keeps(&item, phase) && !self.found.iter().any(|f| f.id == item.id && f.collection == item.collection) {
            self.found.push(item);
        }
    }
}

/// A collection's own assets as an item (its extent's first box and start), when it has assets
/// beyond thumbnails and metadata.
fn collection_item(v: &Value, from: &str) -> Option<Value> {
    let assets = v.get("assets")?.as_object()?;
    let data = assets.values().take(super::MAX_ASSETS).any(|a| {
        let roles: Vec<&str> = a.get("roles").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        let t = a.get("type").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
        let href = a.get("href").and_then(Value::as_str).unwrap_or("");
        let path = crate::logins::inner(href).split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        // Data RustGIS can add (by its media type or extension), or an asset that says it is data.
        const TYPES: [&str; 9] = ["geotiff", "image/tiff", "parquet", "pmtiles", "flatgeobuf", "geo+json", "vnd.las", "laszip", "zarr"];
        const EXTENSIONS: [&str; 11] =
            [".tif", ".tiff", ".parquet", ".geoparquet", ".pmtiles", ".fgb", ".geojson", ".laz", ".las", ".zarr", ".gpkg"];
        let addable = TYPES.iter().any(|k| t.contains(k)) || EXTENSIONS.iter().any(|e| path.ends_with(e));
        !roles.iter().any(|r| matches!(*r, "thumbnail" | "overview" | "metadata" | "style")) && (addable || roles.contains(&"data"))
    });
    if !data {
        return None;
    }
    let extent = v.get("extent");
    let bbox = extent.and_then(|e| e.get("spatial")).and_then(|s| s.get("bbox")).and_then(Value::as_array).and_then(|b| b.first()).cloned();
    let start = extent
        .and_then(|e| e.get("temporal"))
        .and_then(|t| t.get("interval"))
        .and_then(Value::as_array)
        .and_then(|i| i.first())
        .and_then(Value::as_array)
        .and_then(|i| i.first())
        .and_then(Value::as_str)
        .unwrap_or("");
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    let mut links = vec![];
    if let Some(s) = link(v, "self", from) {
        links.push(json!({"rel": "self", "href": s}));
    }
    Some(json!({
        "type": "Feature",
        "id": id,
        "collection": id,
        "bbox": bbox,
        "geometry": null,
        "properties": {"datetime": start},
        "assets": assets,
        "links": links,
    }))
}

/// Walks a static catalog from `root` waiting for each document (the command line; the app reads
/// them off the frame): the walk, finished.
#[cfg(not(target_arch = "wasm32"))]
pub fn walk_blocking(root: &str, filter: Filter, limit: usize) -> Walk {
    let mut w = Walk::new(root, filter, limit);
    loop {
        // A few documents at a time, each on its own thread.
        let urls: Vec<String> = std::iter::from_fn(|| w.next_url()).take(AT_ONCE).collect();
        if urls.is_empty() {
            break;
        }
        let answers: Vec<(String, Result<Vec<u8>, String>)> = std::thread::scope(|s| {
            let handles: Vec<_> = urls
                .into_iter()
                .map(|u| {
                    s.spawn(move || {
                        let r = crate::cache::download_blocking(&u);
                        (u, r)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (u, r) in answers {
            w.land(&u, r);
        }
        // A thread that died leaves its document asked for: it counts as failed.
        for u in w.asked.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>() {
            w.land(&u, Err("the read failed".into()));
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers a walk from a map of documents (missing ones fail).
    fn run(w: &mut Walk, docs: &[(&str, Value)]) {
        while let Some(u) = w.next_url() {
            let r = docs.iter().find(|(k, _)| *k == u).map(|(_, v)| v.to_string().into_bytes()).ok_or_else(|| "404".to_string());
            w.land(&u, r);
        }
        assert!(w.finished());
    }

    fn feature(id: &str, bbox: [f64; 4], date: &str, phase: &str) -> Value {
        json!({"type": "Feature", "id": id, "bbox": bbox, "geometry": null,
            "properties": {"datetime": date, "phase": phase, "eo:cloud_cover": 10},
            "assets": {"visual": {"href": format!("./{id}.tif"), "type": "image/tiff; application=geotiff"}}})
    }

    #[test]
    fn static_addresses_and_children() {
        assert!(is_static("https://x.org/catalog.json") && is_static("https://x.org/a/collection.JSON?x=1"));
        assert!(!is_static("https://x.org/stac/v1") && !is_static("https://x.org/stac/v1/search"));
        let root = json!({"type": "Catalog", "id": "r", "links": [
            {"rel": "self", "href": "https://x.org/catalog.json"},
            {"rel": "child", "href": "./b/collection.json", "title": "Bravo"},
            {"rel": "child", "href": "https://y.org/Alpha-Event/collection.json"},
            {"rel": "child", "href": "./b/collection.json", "title": "Again"},
            {"rel": "child", "href": "file:///etc/passwd"},
            {"rel": "child"},
            {"rel": "item", "href": "./i.json"}]});
        let c = parse_children(root.to_string().as_bytes(), "https://x.org/catalog.json").unwrap();
        assert_eq!(
            c,
            vec![
                Child { title: "Alpha Event".into(), url: "https://y.org/Alpha-Event/collection.json".into() },
                Child { title: "Bravo".into(), url: "https://x.org/b/collection.json".into() },
            ]
        );
        for bad in [&b"[]"[..], b"<html>", br#"{"type": "Feature"}"#, br#"{"id": "untyped"}"#] {
            assert!(parse_children(bad, "https://x.org/c.json").is_err(), "{}", String::from_utf8_lossy(bad));
        }
        // A catalog whose links aren't a list has no children.
        assert_eq!(parse_children(br#"{"type": "Catalog", "links": 7}"#, "https://x.org/c.json"), Ok(vec![]));
        assert_eq!(label_of("https://x.org/events/catalog.json"), "events");
        assert_eq!(label_of("https://x.org/events/a.json"), "a");
        // A collection lists its items by `item` links, in the order it gives them.
        let coll = json!({"type": "Collection", "id": "c", "links": [
            {"rel": "item", "href": "./i2/i2.json", "title": "Second"},
            {"rel": "item", "href": "./i1/i1.json"},
            {"rel": "item", "href": "./i2/i2.json"},
            {"rel": "item", "href": "file:///etc/passwd"},
            {"rel": "child", "href": "./sub/catalog.json"}]});
        let i = parse_item_links(coll.to_string().as_bytes(), "https://x.org/c/collection.json").unwrap();
        assert_eq!(
            i,
            vec![
                Child { title: "Second".into(), url: "https://x.org/c/i2/i2.json".into() },
                Child { title: "i1".into(), url: "https://x.org/c/i1/i1.json".into() },
            ]
        );
        assert!(parse_item_links(br#"{"type": "Feature"}"#, "https://x.org/c.json").is_err());
    }

    #[test]
    fn walks_filter_and_resolve() {
        let docs = [
            (
                "https://x.org/catalog.json",
                json!({"type": "Catalog", "id": "root", "links": [
                {"rel": "child", "href": "./ev/collection.json"},
                {"rel": "child", "href": "./data/collection.json"},
                {"rel": "child", "href": "./missing/catalog.json"},
                {"rel": "child", "href": "./catalog.json"}]}),
            ),
            (
                "https://x.org/ev/collection.json",
                json!({"type": "Collection", "id": "ev", "links": [
                {"rel": "item", "href": "a.json"}, {"rel": "item", "href": "b.json"}, {"rel": "item", "href": "c.json"}]}),
            ),
            ("https://x.org/ev/a.json", feature("a", [7.0, 46.0, 8.0, 47.0], "2024-06-02T10:00:00Z", "pre-event")),
            ("https://x.org/ev/b.json", feature("b", [7.0, 46.0, 8.0, 47.0], "2024-07-02T10:00:00Z", "post")),
            ("https://x.org/ev/c.json", feature("c", [100.0, 0.0, 101.0, 1.0], "2024-06-03T10:00:00Z", "post")),
            // A collection whose data is its own (Portolan's): one item of its assets.
            (
                "https://x.org/data/collection.json",
                json!({"type": "Collection", "id": "roads",
                "extent": {"spatial": {"bbox": [[7.5, 46.5, 7.6, 46.6]]}, "temporal": {"interval": [[null, null]]}},
                "assets": {"thumbnail": {"href": "t.png", "type": "image/png", "roles": ["thumbnail"]},
                           "visual": {"href": "roads.pmtiles", "type": "application/vnd.pmtiles"}},
                "links": []}),
            ),
        ];
        let mut w = Walk::new("https://x.org/catalog.json", Filter::default(), 50);
        run(&mut w, &docs);
        let ids: Vec<&str> = w.items().iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "roads"]);
        assert_eq!(w.failed, 1, "the missing catalog");
        // A collection whose only assets are documents (a license, metadata) isn't an item.
        let docs_only = json!({"type": "Collection", "id": "docs", "links": [],
            "assets": {"license": {"href": "LICENSE.pdf", "type": "application/pdf"}, "meta": {"href": "meta.json", "type": "application/json"}}});
        assert!(collection_item(&docs_only, "https://x.org/c.json").is_none());
        assert!(!w.complete());
        let roads = &w.items()[3];
        assert_eq!(roads.bbox, Some([7.5, 46.5, 7.6, 46.6]));
        assert_eq!(roads.pick(None).unwrap().href, "https://x.org/data/roads.pmtiles");
        assert_eq!(w.items()[0].pick(None).unwrap().href, "https://x.org/ev/a.tif");

        // Place, dates and phase.
        let s = Search { bbox: Some([7.2, 46.2, 7.55, 46.55]), datetime: Some("2024-06-01/2024-06-30".into()), ..Default::default() };
        let mut w = Walk::new("https://x.org/catalog.json", Filter::from_search(&s, None).unwrap(), 50);
        run(&mut w, &docs);
        let ids: Vec<&str> = w.items().iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["a", "roads"]);
        let mut w = Walk::new("https://x.org/ev/collection.json", Filter::from_search(&Search::default(), Some("post")).unwrap(), 50);
        run(&mut w, &docs);
        assert_eq!(w.items().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert!(w.complete());
        let mut w = Walk::new("https://x.org/ev/collection.json", Filter::from_search(&Search::default(), Some("pre")).unwrap(), 50);
        run(&mut w, &docs);
        assert_eq!(w.items().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert!(Filter::from_search(&Search::default(), Some("during")).is_err());
        // Open-ended ranges, and instants with offsets.
        let to = Filter::from_search(&Search { datetime: Some("../2024-06-30".into()), ..Default::default() }, None).unwrap();
        assert_eq!((to.from, to.to), (None, instant("2024-06-30T23:59:59Z")));
        let mut w = Walk::new("https://x.org/ev/collection.json", to, 50);
        run(&mut w, &docs);
        assert_eq!(w.items().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(instant("1970-01-01"), Some(0));
        assert_eq!(instant("2024-06-02T12:30:00+02:00"), instant("2024-06-02T10:30:00Z"));
        assert_eq!(instant("2024-06-02T10:30:00.123456Z"), instant("2024-06-02T10:30:00Z"));
        assert_eq!(instant("2024-03-01T00:00:00Z").zip(instant("2024-02-28")).map(|(a, b)| a - b), Some(2 * 86_400));
        assert!(instant("2024-02-29").is_some() && instant("2023-02-29").is_none());
        // A box over the antimeridian.
        let fiji = Filter { bbox: Some([170.0, -20.0, -170.0, -10.0]), ..Default::default() };
        let at = |w: f64, e: f64| {
            let v = feature("f", [w, -18.0, e, -17.0], "", "");
            fiji.keeps(&crate::stac::item_from_json(&v, Some("https://x.org/f.json")).unwrap(), None)
        };
        assert!(at(178.0, 179.0) && at(-179.0, -178.0) && !at(0.0, 1.0));
        // An item over the antimeridian, with a filter on either side of it or over it too.
        assert!(at(179.0, -179.0));
        let east = Filter { bbox: Some([-179.5, -20.0, -170.0, -10.0]), ..Default::default() };
        let west = Filter { bbox: Some([0.0, -20.0, 10.0, -10.0]), ..Default::default() };
        let over = crate::stac::item_from_json(&feature("o", [179.0, -18.0, -179.0, -17.0], "", ""), Some("https://x.org/o.json")).unwrap();
        assert!(east.keeps(&over, None) && !west.keeps(&over, None));
        // Links past the most followed are counted among their own relation, and cut the walk.
        let many: Vec<Value> = (0..MAX_LINKS + 3).map(|i| json!({"rel": "license", "href": format!("l{i}.txt")})).collect();
        let mut doc = json!({"type": "Catalog", "links": many});
        if let Some(l) = doc["links"].as_array_mut() {
            l.push(json!({"rel": "item", "href": "a.json"}));
        }
        assert_eq!(links(&doc, "item", "https://x.org/c.json").len(), 1, "other relations don't hide items");
        assert!(too_many(&doc, "license") && !too_many(&doc, "item"));
        for bad in ["", "2024", "2024-13-01", "2024-04-31", "2024-06-02T25:00:00Z", "2024-06-02T10:30:00+0200", "yesterday", "2024-06-02X"]
        {
            assert_eq!(instant(bad), None, "{bad}");
        }
        assert!(Filter::from_search(&Search { datetime: Some("soon".into()), ..Default::default() }, None).is_err());

        // A limit stops the walk.
        let mut w = Walk::new("https://x.org/catalog.json", Filter::default(), 2);
        run(&mut w, &docs);
        assert_eq!(w.items().len(), 2);
        assert!(!w.complete());
    }

    #[test]
    fn hostile_catalogs_are_bounded() {
        // A catalog linking to ever deeper catalogs: cut at the depth limit.
        let mut w = Walk::new("https://x.org/0.json", Filter::default(), 50);
        let mut reads = 0;
        while let Some(u) = w.next_url() {
            let n: usize = u.trim_start_matches("https://x.org/").trim_end_matches(".json").parse().unwrap_or(0);
            let doc = json!({"type": "Catalog", "id": "c", "links": [{"rel": "child", "href": format!("{}.json", n + 1)}]});
            w.land(&u, Ok(doc.to_string().into_bytes()));
            reads += 1;
        }
        assert!(w.finished() && w.cut && !w.complete());
        assert_eq!(reads, MAX_DEPTH + 1);

        // A catalog of very many children: the budget of reads ends it.
        let children: Vec<Value> = (0..MAX_QUEUE + 10).map(|i| json!({"rel": "child", "href": format!("c{i}.json")})).collect();
        let big = json!({"type": "Catalog", "id": "big", "links": children}).to_string().into_bytes();
        let mut w = Walk::new("https://x.org/big.json", Filter::default(), 50);
        let mut reads = 0;
        while let Some(u) = w.next_url() {
            let answer = if u.ends_with("big.json") { Ok(big.clone()) } else { Ok(b"{\"type\": \"Catalog\"}".to_vec()) };
            w.land(&u, answer);
            reads += 1;
        }
        assert_eq!(reads, MAX_READS);
        assert!(w.finished() && !w.complete());

        // Junk answers, cycles and answers nobody asked for.
        let mut w = Walk::new("https://x.org/a.json", Filter::default(), 50);
        w.land("https://x.org/nobody.json", Ok(b"{}".to_vec()));
        assert_eq!(w.reads, 0);
        let cycle = json!({"type": "Catalog", "links": [{"rel": "child", "href": "a.json"}, {"rel": "item", "href": "x.json"},
            {"rel": "item", "href": "s3://b/k.json"}, {"rel": "child", "href": 7}]});
        while let Some(u) = w.next_url() {
            let r = match u.as_str() {
                "https://x.org/a.json" => Ok(cycle.to_string().into_bytes()),
                _ => Ok(b"\xff\xfe not json".to_vec()),
            };
            w.land(&u, r);
        }
        assert_eq!((w.reads, w.failed, w.items().len()), (2, 1, 0));
        // Several asked at once, answered out of order.
        let mut w = Walk::new("https://x.org/ev/collection.json", Filter::default(), 50);
        let first = w.next_url().unwrap();
        w.land(
            &first,
            Ok(json!({"type": "Collection", "links": [{"rel": "item", "href": "a.json"}, {"rel": "item", "href": "b.json"}]})
                .to_string()
                .into_bytes()),
        );
        let (a, b) = (w.next_url().unwrap(), w.next_url().unwrap());
        assert_eq!(w.waiting(), 2);
        w.land(&b, Ok(feature("b", [0.0, 0.0, 1.0, 1.0], "", "").to_string().into_bytes()));
        assert!(!w.finished());
        w.land(&a, Err("timeout".into()));
        assert!(w.finished() && w.items().len() == 1);
    }
}
