//! Static collections laid out by date, as JAXA Earth's are: `<date>[/<day>]/<level>/<longitudes>/
//! <latitudes>.json`, an item per tile of a fixed grid, with the collection's `summaries` saying
//! how its dates are written (`je:stac_date_format`) and which level holds the full images
//! (`je:cog_level_max`). A walk ([`super::catalog::Walk`]) goes through every date from the first
//! and spends its documents long before the period asked for, so a data cube reads these
//! differently: only the period's date folders, one date's level and longitude folders to learn
//! the area's items, and then each date's same items directly, about one document per date and
//! tile. A tile the date looked at doesn't have is missed in the other dates too. The documents
//! are hostile input: links are checked and resolved as a walk's are, and counts are bounded.

use serde_json::Value;

use super::catalog::{AT_ONCE, Filter, links, too_many};
use super::{Item, is_web, resolve};

/// How many dates, from the last, are tried for the area's items: the newest can be still coming.
const DATES_TRIED: usize = 3;

/// What a collection says of its layout.
#[derive(Debug, PartialEq)]
struct Layout {
    /// Whether its dates are `<year-month>/<day>` folders, rather than one `<year>` or
    /// `<year-month>` folder.
    day_folders: bool,
    /// The folder of the level that holds the full images.
    level: String,
}

/// A collection's layout: `None` when it doesn't describe one, an error when it does but its dates
/// can't make a data cube.
fn layout(collection: &Value) -> Result<Option<Layout>, String> {
    let first = |k: &str| collection.get("summaries")?.get(k)?.as_array()?.first().cloned();
    let Some(format) = first("je:stac_date_format") else { return Ok(None) };
    let day_folders = match format.as_str().unwrap_or("") {
        "YYYY" | "YYYY-MM" => false,
        "YYYY-MM/DD" => true,
        "MM" | "MM-DD" => return Err("its dates have no year (it is a climatology), so they make no time series".into()),
        "YYYY-MM/DD-hh" => return Err("it is hourly, and a data cube has one file per day".into()),
        "" => return Err(format!("its date format {} isn't text", format.to_string().chars().take(40).collect::<String>())),
        f => return Err(format!("its dates are written {}, which RustGIS doesn't read yet", f.chars().take(40).collect::<String>())),
    };
    let level = first("je:cog_level_max").and_then(|l| l.as_u64()).ok_or("it doesn't say which level holds the full images")?;
    Ok(Some(Layout { day_folders, level: level.to_string() }))
}

/// The folder a `catalog.json` is in, or a document's own name without `.json`.
fn name_of(url: &str) -> &str {
    let path = crate::logins::inner(url).split(['?', '#']).next().unwrap_or("");
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or("");
    match file.strip_suffix(".json") {
        Some("catalog" | "collection") => parts.next().unwrap_or(""),
        Some(name) => name,
        None => file,
    }
}

/// Whether an address is a folder's `catalog.json`.
fn is_folder(url: &str) -> bool {
    crate::logins::inner(url).split(['?', '#']).next().unwrap_or("").ends_with("/catalog.json")
}

/// The degrees a tile's folder or file spans, from its name (`W090.00-E000.00`, `S90.00-N00.00`).
fn degrees(name: &str) -> Option<(f64, f64)> {
    let one = |t: &str| -> Option<f64> {
        let sign = match t.as_bytes().first()? {
            b'E' | b'N' => 1.0,
            b'W' | b'S' => -1.0,
            _ => return None,
        };
        t.get(1..)?.parse::<f64>().ok().filter(|x| x.is_finite()).map(|x| x * sign)
    };
    let (a, b) = name.split_once('-')?;
    Some((one(a)?, one(b)?))
}

/// Whether a tile's folder or file, by its name, can hold a part of `lo`..`hi`, which crosses the
/// antimeridian when `lo` is past `hi`. A name that isn't a span of degrees is read to find out.
/// Touching isn't meeting: a box on a tile's edge would add a tile with nothing of the box in it.
fn may_meet(name: &str, lo: f64, hi: f64) -> bool {
    let Some((a, b)) = degrees(name) else { return true };
    let spans = if lo <= hi { [(lo, hi), (lo, hi)] } else { [(lo, 180.0), (-180.0, hi)] };
    spans.iter().any(|(from, to)| a < *to && b > *from)
}

/// A document's child catalogs that `wanted` keeps (by address), those that may be read from where
/// it was found, and how many it wanted but refused: an address a document on the web can't point
/// to, or one past the links followed (counted once, as the document isn't read whole).
fn children(doc: &Value, from: &str, wanted: impl Fn(&str) -> bool) -> (Vec<String>, usize) {
    let mut refused = usize::from(too_many(doc, "child"));
    let mut kept: Vec<String> = Vec::new();
    for (u, _) in links(doc, "child", from) {
        // A link listed twice is one folder.
        if !wanted(&u) || kept.contains(&u) {
            continue;
        }
        if is_web(&u) && crate::address::check(&u, Some(from)).is_ok() {
            kept.push(u);
        } else {
            refused += 1;
        }
    }
    (kept, refused)
}

/// A catalog's or collection's document from its bytes.
fn catalog(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(bytes).ok().filter(|v| matches!(v.get("type").and_then(Value::as_str), Some("Catalog" | "Collection")))
}

/// Reads documents a few at a time, each on its own thread: the answers, in their order. Each
/// thread carries the command's request context, so a request withdrawn meanwhile isn't sent.
fn read_all(urls: &[String], fetch: Fetch<'_>) -> Vec<Result<Vec<u8>, String>> {
    let mut out = Vec::with_capacity(urls.len());
    let still = crate::cache::current_still();
    for chunk in urls.chunks(AT_ONCE) {
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|u| {
                    let still = still.clone();
                    s.spawn(move || crate::cache::with_optional_still(still, || fetch(u)))
                })
                .collect();
            out.extend(handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("the read failed".to_string()))));
        });
    }
    out
}

/// How a document is read.
type Fetch<'a> = &'a (dyn Fn(&str) -> Result<Vec<u8>, String> + Sync);

/// The area's items below a date's folder, as addresses relative to it: its level's longitude
/// folders that meet the area, and their items that do. Each date of `dates` reads them all, so
/// more than `max_items` in the end is refused before they are read. Also how many items over the
/// area were refused: an address a document on the web can't point to, or one that isn't below
/// the date's folder as a plain path, which wouldn't stay in the other dates' folders.
fn tiles_of(
    date: &str,
    layout: &Layout,
    filter: &Filter,
    dates: usize,
    max_items: usize,
    fetch: Fetch<'_>,
) -> Result<(Vec<String>, usize), String> {
    let read = |url: &str| -> Result<Value, String> { catalog(&fetch(url)?).ok_or_else(|| format!("{url} isn't a STAC catalog")) };
    let levels = read(date)?;
    // A level refused, or written as a file rather than a folder, is a level missing: the error
    // says so.
    let level = children(&levels, date, |u| name_of(u) == layout.level && is_folder(u))
        .0
        .into_iter()
        .next()
        .ok_or_else(|| format!("{date} has no level {}", layout.level))?;
    let (west, east, south, north) = filter.bbox.map_or((-180.0, 180.0, -90.0, 90.0), |[w, s, e, n]| (w, e, s, n));
    let (columns, mut refused) = children(&read(&level)?, &level, |u| may_meet(name_of(u), west, east));
    let folder = crate::logins::inner(date).rsplit_once('/').map_or("", |(folder, _)| folder).to_string() + "/";
    let mut tiles: Vec<String> = Vec::new();
    for (column, answer) in columns.iter().zip(read_all(&columns, fetch)) {
        let doc = catalog(&answer?).ok_or_else(|| format!("{column} isn't a STAC catalog"))?;
        // A column listing more items than are followed isn't read whole: counted.
        if too_many(&doc, "item") {
            refused += 1;
        }
        for (item, _) in links(&doc, "item", column) {
            if !may_meet(name_of(&item), south, north) {
                continue;
            }
            // Only an item below the date's folder, by a plain path, has a place in the other
            // dates' folders too: `//host/...` there would be another host's.
            let path = crate::logins::inner(&item).strip_prefix(folder.as_str());
            let Some(path) =
                path.filter(|p| !p.starts_with('/') && !p.contains("://") && crate::address::check(&item, Some(column)).is_ok())
            else {
                refused += 1;
                continue;
            };
            // A tile listed twice is one tile.
            if tiles.iter().any(|t| t == path) {
                continue;
            }
            tiles.push(path.to_string());
            if tiles.len().saturating_mul(dates) > max_items {
                return Err(format!("more than {max_items} items: take a shorter period or a smaller area"));
            }
        }
    }
    Ok((tiles, refused))
}

/// What a collection holds over an area and a period.
#[derive(Debug)]
pub struct Found {
    /// The items, by date.
    pub items: Vec<Item>,
    /// Documents that couldn't be read, or weren't what they should be: dates or tiles missing.
    pub unread: usize,
}

/// A collection's items over `filter`'s area and dates, `period` being its first and last days as
/// `YYYY-MM-DD`: at most `max_items`, or an error. A collection that doesn't describe its layout
/// is refused, as are those whose dates can't make a data cube.
pub fn search(root: &str, filter: &Filter, period: (&str, &str), max_items: usize, fetch: Fetch<'_>) -> Result<Found, String> {
    let (start, end) = period;
    let doc = catalog(&fetch(root)?).ok_or_else(|| format!("{root} isn't a STAC catalog or collection"))?;
    let layout = layout(&doc)
        .map_err(|why| format!("{root} can't be read as a data cube: {why}"))?
        .ok_or("a data cube needs a STAC API's search, or a static collection laid out by date (JAXA Earth's); this one doesn't describe its layout")?;
    // A date's folder, by its name: a year or a year and month, within the period at that length.
    let within = |name: &str| {
        let n = name.len();
        matches!(n, 4 | 7)
            && name.starts_with(|c: char| c.is_ascii_digit())
            && start.get(..n).is_some_and(|s| s <= name)
            && end.get(..n).is_some_and(|e| name <= e)
    };
    let (folders, mut unread) = children(&doc, root, |u| within(name_of(u)));
    // A date is a folder of its own: a date written as a file (`2024-01.json`) would give every
    // date the same items, so it is counted, not read.
    let (folders, files): (Vec<String>, Vec<String>) = folders.into_iter().partition(|u| is_folder(u));
    unread += files.len();
    let mut dates: Vec<(String, String)> = Vec::new();
    if layout.day_folders {
        for (month, answer) in folders.iter().zip(read_all(&folders, fetch)) {
            let Some(days) = answer.ok().as_deref().and_then(catalog) else {
                unread += 1;
                continue;
            };
            let in_period = |day: &str| {
                let date = format!("{}-{}", name_of(month), name_of(day));
                date.as_str() >= start && date.as_str() <= end
            };
            let (days, refused) = children(&days, month, in_period);
            let (days, files): (Vec<String>, Vec<String>) = days.into_iter().partition(|u| is_folder(u));
            unread += refused + files.len();
            for day in days {
                dates.push((format!("{}-{}", name_of(month), name_of(&day)), day));
            }
        }
    } else {
        dates.extend(folders.into_iter().map(|f| (name_of(&f).to_string(), f)));
    }
    dates.sort();
    // The area's items, from the last date that has them.
    let mut tiles = Vec::new();
    let mut last_error = None;
    for (_, date) in dates.iter().rev().take(DATES_TRIED) {
        match tiles_of(date, &layout, filter, dates.len(), max_items, fetch) {
            Ok((t, refused)) => {
                unread += refused;
                if !t.is_empty() {
                    tiles = t;
                    last_error = None;
                    break;
                }
            }
            // Too many is the answer however many dates are tried.
            Err(e) if e.starts_with("more than") => return Err(e),
            // A date whose folders can't be read: an earlier one may give the area's items, but
            // tiles only this one has are missing then, so it is counted.
            Err(e) => {
                unread += 1;
                last_error = Some(e);
            }
        }
    }
    if let Some(e) = last_error {
        return Err(e);
    }
    // Each date's own address for an item, checked as a document's link is: one refused is counted.
    let mut urls: Vec<String> = Vec::new();
    for (_, date) in &dates {
        for t in &tiles {
            match resolve(date, t).filter(|u| crate::address::check(u, Some(date)).is_ok()) {
                Some(u) => urls.push(u),
                None => unread += 1,
            }
        }
    }
    let mut items: Vec<Item> = Vec::with_capacity(urls.len());
    let mut last_error = None;
    // The dates were chosen by their folders, which hold a month or a year: an item dated its
    // month's first day is in a period that starts later in that month, so only the area is
    // checked here.
    let area = Filter { from: None, to: None, ..filter.clone() };
    for (url, answer) in urls.iter().zip(read_all(&urls, fetch)) {
        match answer.and_then(|bytes| super::parse_item(&bytes, url)) {
            Ok(mut item) if area.keeps(&item, None) => {
                // An item's files are judged from the item, as a document's links are: one on the
                // web can't point into the local network. An item losing a file is counted.
                let files = item.assets.len();
                item.assets.retain(|a| crate::address::check(&a.href, Some(url)).is_ok());
                if item.assets.len() < files {
                    unread += 1;
                }
                items.push(item);
            }
            Ok(_) => {}
            Err(e) => {
                unread += 1;
                last_error = Some(e);
            }
        }
    }
    // Nothing read, where something should have been: why, rather than an empty search.
    if let (true, Some(e)) = (items.is_empty(), last_error) {
        return Err(e);
    }
    Ok(Found { items, unread })
}

/// [`search`], waiting for each document (the command line, and the app's `stac.addCube`).
pub fn search_blocking(root: &str, filter: &Filter, period: (&str, &str), max_items: usize) -> Result<Found, String> {
    search(root, filter, period, max_items, &|u| crate::cache::download_blocking(u))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    const ROOT: &str = "https://x.org/c/collection.json";

    fn catalog_of(rel: &str, hrefs: &[String]) -> Vec<u8> {
        let links: Vec<Value> = hrefs.iter().map(|h| json!({"rel": rel, "href": h})).collect();
        json!({"type": "Catalog", "links": links}).to_string().into_bytes()
    }

    /// A collection of `dates` (as they are written below it), each with level 1 of two longitude
    /// folders of two items each, and a level 0 nobody should read.
    fn tree(format: &str, dates: &[&str]) -> HashMap<String, Vec<u8>> {
        let mut docs = HashMap::new();
        let top: Vec<&str> = dates.iter().map(|d| d.split('/').next().unwrap()).collect();
        let hrefs: Vec<String> = top.iter().map(|d| format!("./{d}/catalog.json")).collect();
        let mut unique = hrefs.clone();
        unique.dedup();
        let links: Vec<Value> = unique.iter().map(|h| json!({"rel": "child", "href": h})).collect();
        docs.insert(
            ROOT.to_string(),
            json!({"type": "Collection", "id": "collection", "links": links,
                "summaries": {"je:stac_date_format": [format], "je:cog_level_max": [1]}})
            .to_string()
            .into_bytes(),
        );
        for d in dates {
            let base = format!("https://x.org/c/{d}");
            if let Some((month, day)) = d.split_once('/') {
                let key = format!("https://x.org/c/{month}/catalog.json");
                let mut days: Vec<String> = Vec::new();
                if let Some(old) = docs.get(&key) {
                    let v: Value = serde_json::from_slice(old).unwrap();
                    days = v["links"].as_array().unwrap().iter().map(|l| l["href"].as_str().unwrap().to_string()).collect();
                }
                days.push(format!("./{day}/catalog.json"));
                docs.insert(key, catalog_of("child", &days));
            }
            docs.insert(
                format!("{base}/catalog.json"),
                catalog_of("child", &["./0/catalog.json".to_string(), "./1/catalog.json".to_string()]),
            );
            let columns = ["W090.00-E000.00", "E000.00-E090.00"];
            docs.insert(format!("{base}/1/catalog.json"), catalog_of("child", &columns.map(|c| format!("./{c}/catalog.json"))));
            for (c, (w, e)) in columns.iter().zip([(-90.0, 0.0), (0.0, 90.0)]) {
                let rows = [("S90.00-N00.00", -90.0, 0.0), ("N00.00-N90.00", 0.0, 90.0)];
                docs.insert(format!("{base}/1/{c}/catalog.json"), catalog_of("item", &rows.map(|r| format!("./{}.json", r.0))));
                for (r, so, n) in rows {
                    let day = d.replace('/', "-");
                    let day = if day.len() == 4 {
                        format!("{day}-01-01")
                    } else if day.len() == 7 {
                        format!("{day}-01")
                    } else {
                        day
                    };
                    docs.insert(
                        format!("{base}/1/{c}/{r}.json"),
                        json!({"type": "Feature", "id": r, "bbox": [w, so, e, n], "geometry": null, "collection": "cube",
                            "properties": {"datetime": null, "start_datetime": format!("{day}T00:00:00Z")},
                            "assets": {"V": {"href": format!("./{c}-{r}-V.tiff"), "type": "image/tiff",
                                "je:rasters": {"dn2value": {"slope": 0.5, "offset": 1.0}, "dn": {"nodata": 9.0, "error": [7.0, 8.0]}}}}})
                        .to_string()
                        .into_bytes(),
                    );
                }
            }
        }
        docs
    }

    /// What a search answers from `docs`, and the documents it read.
    fn run(
        docs: &HashMap<String, Vec<u8>>,
        bbox: [f64; 4],
        period: (&str, &str),
        max_items: usize,
    ) -> (Result<Found, String>, Vec<String>) {
        let reads = Mutex::new(Vec::new());
        let filter = Filter { bbox: Some(bbox), ..Default::default() };
        let r = search(ROOT, &filter, period, max_items, &|u| {
            reads.lock().unwrap().push(u.to_string());
            docs.get(u).cloned().ok_or_else(|| format!("{u}: 404"))
        });
        (r, reads.into_inner().unwrap())
    }

    #[test]
    fn only_the_period_and_the_area_are_read() {
        let docs = tree("YYYY-MM", &["2023-12", "2024-01", "2024-02", "2024-03"]);
        let (r, reads) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-02-29"), 100);
        let found = r.unwrap();
        let hrefs: Vec<&str> = found.items.iter().map(|i| i.assets[0].href.as_str()).collect();
        assert_eq!(
            hrefs,
            [
                "https://x.org/c/2024-01/1/W090.00-E000.00/W090.00-E000.00-S90.00-N00.00-V.tiff",
                "https://x.org/c/2024-02/1/W090.00-E000.00/W090.00-E000.00-S90.00-N00.00-V.tiff"
            ],
            "items share an id across dates, and each date keeps its own"
        );
        assert_eq!(found.unread, 0);
        // The collection, the last date's three folders, and one item a date.
        assert_eq!(reads.len(), 6, "{reads:?}");
        assert!(reads.iter().all(|u| !u.contains("2023-12") && !u.contains("2024-03") && !u.contains("/0/")), "{reads:?}");
        // The scale, offset, no-data and error codes of JAXA Earth's items are read.
        let a = &found.items[0].assets[0];
        assert_eq!((a.scale, a.offset, a.nodata, a.invalid.as_slice()), (Some(0.5), Some(1.0), Some(9.0), &[7.0, 8.0][..]));
    }

    #[test]
    fn dates_in_day_folders_and_a_box_over_tiles() {
        let docs = tree("YYYY-MM/DD", &["2024-01/30", "2024-01/31", "2024-02/01", "2024-02/02"]);
        let (r, _) = run(&docs, [-1.0, -1.0, 1.0, 1.0], ("2024-01-31", "2024-02-01"), 100);
        let found = r.unwrap();
        assert_eq!(found.items.len(), 8, "two dates of the four tiles the box is in");
        assert!(found.items.iter().all(|i| i.datetime.starts_with("2024-01-31") || i.datetime.starts_with("2024-02-01")));
        // On a tile's edge, the tile beside it isn't read.
        let (r, _) = run(&docs, [-10.0, -10.0, 0.0, 0.0], ("2024-01-31", "2024-01-31"), 100);
        assert_eq!(r.unwrap().items.len(), 1);
        // Across the antimeridian, the longitudes on either side of it.
        assert!(may_meet("E170.00-E180.00", 175.0, -175.0) && may_meet("W180.00-W170.00", 175.0, -175.0));
        assert!(!may_meet("W090.00-E000.00", 175.0, -175.0));
        // A name that isn't a span of degrees is read to find out.
        assert!(may_meet("whatever", 0.0, 1.0));
    }

    #[test]
    fn missing_documents_are_counted_and_a_lost_search_says_why() {
        let mut docs = tree("YYYY-MM", &["2024-01", "2024-02", "2024-03"]);
        let area = [-50.0, -20.0, -40.0, -10.0];
        let period = ("2024-01-01", "2024-03-31");
        docs.remove("https://x.org/c/2024-02/1/W090.00-E000.00/S90.00-N00.00.json");
        let (r, _) = run(&docs, area, period, 100);
        let found = r.unwrap();
        assert_eq!((found.items.len(), found.unread), (2, 1));
        // The last date missing its level: the date before it gives the area's items, and the
        // last date's own item is still read directly.
        docs.remove("https://x.org/c/2024-03/1/catalog.json");
        let (r, _) = run(&docs, area, period, 100);
        let found = r.unwrap();
        assert_eq!((found.items.len(), found.unread), (2, 2), "2024-03's level and 2024-02's item are missing");
        // No date in the period: nothing, for the caller to say.
        let (r, reads) = run(&docs, area, ("1990-01-01", "1990-12-31"), 100);
        assert!(r.unwrap().items.is_empty());
        assert_eq!(reads, [ROOT]);
        // Every item unreadable: why.
        docs.retain(|k, _| !k.ends_with("S90.00-N00.00.json"));
        let (r, _) = run(&docs, area, period, 100);
        assert!(r.unwrap_err().contains("404"));
    }

    #[test]
    fn more_than_the_cap_is_refused_before_the_items_are_read() {
        let docs = tree("YYYY-MM", &["2024-01", "2024-02"]);
        let (r, reads) = run(&docs, [-1.0, -1.0, 1.0, 1.0], ("2024-01-01", "2024-02-29"), 3);
        assert!(r.unwrap_err().contains("more than 3 items"));
        assert!(reads.iter().all(|u| u.ends_with("catalog.json") || u == ROOT), "{reads:?}");
    }

    #[test]
    fn collections_that_cant_make_a_cube_say_why() {
        for (format, why) in
            [("MM", "no year"), ("MM-DD", "no year"), ("YYYY-MM/DD-hh", "hourly"), ("weekly", "weekly"), ("YYYY-MM", "which level")]
        {
            let mut docs = HashMap::new();
            let mut doc = json!({"type": "Collection", "links": [], "summaries": {"je:stac_date_format": [format]}});
            if format != "YYYY-MM" {
                doc["summaries"]["je:cog_level_max"] = json!([1]);
            }
            docs.insert(ROOT.to_string(), doc.to_string().into_bytes());
            let (r, _) = run(&docs, [0.0, 0.0, 1.0, 1.0], ("2024-01-01", "2024-12-31"), 100);
            let e = r.unwrap_err();
            assert!(e.contains(why) && e.contains(ROOT), "{format}: {e}");
        }
        // Another kind of static catalog, and an answer that isn't one.
        let mut docs = HashMap::new();
        docs.insert(ROOT.to_string(), json!({"type": "Collection", "links": []}).to_string().into_bytes());
        let (r, _) = run(&docs, [0.0, 0.0, 1.0, 1.0], ("2024-01-01", "2024-12-31"), 100);
        assert!(r.unwrap_err().contains("doesn't describe its layout"));
        docs.insert(ROOT.to_string(), b"<html>".to_vec());
        let (r, _) = run(&docs, [0.0, 0.0, 1.0, 1.0], ("2024-01-01", "2024-12-31"), 100);
        assert!(r.unwrap_err().contains("isn't a STAC"));
    }

    #[test]
    fn an_item_path_cant_leave_its_date_folder_in_the_other_dates() {
        // An item of the date that gives the area's items, on its own host but with a path that
        // reads as another host's from the dates' folders: never read, and counted.
        let mut docs = tree("YYYY-MM", &["2024-01", "2024-02"]);
        docs.insert(
            "https://x.org/c/2024-02/1/W090.00-E000.00/catalog.json".to_string(),
            catalog_of("item", &["https://x.org/c/2024-02///127.0.0.1/S90.00-N00.00.json".to_string()]),
        );
        let (r, reads) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-02-29"), 100);
        assert!(reads.iter().all(|u| u.starts_with("https://x.org/")), "{reads:?}");
        let found = r.unwrap();
        assert_eq!(found.unread, 1, "the refused item is told");
        // The date before gives the area's items instead, read at both dates.
        assert_eq!(found.items.len(), 2);
    }

    #[test]
    fn a_month_that_starts_before_the_period_is_kept() {
        // Its folder is in the period, so its item is, though its date is the month's first day.
        let docs = tree("YYYY-MM", &["2024-01", "2024-02"]);
        let filter = Filter {
            bbox: Some([-50.0, -20.0, -40.0, -10.0]),
            from: super::super::catalog::instant("2024-01-15"),
            to: super::super::catalog::instant("2024-02-29T23:59:59Z"),
            ..Default::default()
        };
        let found =
            search(ROOT, &filter, ("2024-01-15", "2024-02-29"), 100, &|u| docs.get(u).cloned().ok_or_else(|| format!("{u}: 404"))).unwrap();
        assert_eq!((found.items.len(), found.unread), (2, 0));
    }

    #[test]
    fn a_tile_listed_twice_is_read_once() {
        let mut docs = tree("YYYY-MM", &["2024-01", "2024-02"]);
        docs.insert(
            "https://x.org/c/2024-02/1/W090.00-E000.00/catalog.json".to_string(),
            catalog_of("item", &["./S90.00-N00.00.json".to_string(), "./S90.00-N00.00.json".to_string()]),
        );
        let (r, _) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-02-29"), 2);
        assert_eq!(r.unwrap().items.len(), 2, "one tile at two dates is within a cap of 2");
    }

    #[test]
    fn an_item_on_the_web_cant_point_its_files_into_the_local_network() {
        // A collection on this computer whose dates are on the web: an item there with a file on
        // the local network loses that file, and it is counted.
        let mut docs = tree("YYYY-MM", &["2024-01"]);
        let local = "http://localhost/c/collection.json";
        let mut root: Value = serde_json::from_slice(&docs[ROOT]).unwrap();
        root["links"] = json!([{"rel": "child", "href": "https://x.org/c/2024-01/catalog.json"}]);
        docs.insert(local.to_string(), root.to_string().into_bytes());
        let key = "https://x.org/c/2024-01/1/W090.00-E000.00/S90.00-N00.00.json";
        let mut item: Value = serde_json::from_slice(&docs[key]).unwrap();
        item["assets"]["V"]["href"] = json!("http://192.168.1.1/a.tiff");
        docs.insert(key.to_string(), item.to_string().into_bytes());
        let filter = Filter { bbox: Some([-50.0, -20.0, -40.0, -10.0]), ..Default::default() };
        let found = search(local, &filter, ("2024-01-01", "2024-01-31"), 100, &|u| docs.get(u).cloned().ok_or_else(|| format!("{u}: 404")))
            .unwrap();
        assert_eq!(found.unread, 1);
        assert!(found.items.iter().all(|i| i.assets.is_empty()), "{:?}", found.items);
    }

    #[test]
    fn every_read_carries_the_commands_request_context() {
        let docs = tree("YYYY-MM", &["2024-01", "2024-02"]);
        let without = Mutex::new(Vec::new());
        let filter = Filter { bbox: Some([-50.0, -20.0, -40.0, -10.0]), ..Default::default() };
        let still: crate::cache::Still = std::sync::Arc::new(|| true);
        let found = crate::cache::with_still(still, || {
            search(ROOT, &filter, ("2024-01-01", "2024-02-29"), 100, &|u| {
                if crate::cache::current_still().is_none() {
                    without.lock().unwrap().push(u.to_string());
                }
                docs.get(u).cloned().ok_or_else(|| format!("{u}: 404"))
            })
        })
        .unwrap();
        assert_eq!(found.items.len(), 2);
        assert!(without.into_inner().unwrap().is_empty(), "reads on a thread without the context");
    }

    #[test]
    fn a_refused_month_or_column_is_counted() {
        // A month of the period on the local network, and a column there: neither read, both told.
        let mut docs = tree("YYYY-MM", &["2024-01", "2024-02"]);
        let mut root: Value = serde_json::from_slice(&docs[ROOT]).unwrap();
        root["links"].as_array_mut().unwrap().push(json!({"rel": "child", "href": "http://127.0.0.1/c/2024-02/catalog.json"}));
        root["links"].as_array_mut().unwrap().push(json!({"rel": "child", "href": "http://127.0.0.1/c/1990-01/catalog.json"}));
        docs.insert(ROOT.to_string(), root.to_string().into_bytes());
        let level = "https://x.org/c/2024-02/1/catalog.json";
        let mut columns: Value = serde_json::from_slice(&docs[level]).unwrap();
        columns["links"].as_array_mut().unwrap().push(json!({"rel": "child", "href": "http://127.0.0.1/W090.00-E000.00/catalog.json"}));
        docs.insert(level.to_string(), columns.to_string().into_bytes());
        let (r, reads) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-02-29"), 100);
        assert!(reads.iter().all(|u| u.starts_with("https://x.org/")), "{reads:?}");
        let found = r.unwrap();
        assert_eq!((found.items.len(), found.unread), (2, 2), "the month and the column in the period and the area; 1990 isn't wanted");
    }

    #[test]
    fn a_column_past_the_links_followed_is_counted() {
        let column = "https://x.org/c/2024-01/1/W090.00-E000.00/catalog.json";
        let at = |n: usize| {
            let mut docs = tree("YYYY-MM", &["2024-01"]);
            docs.insert(column.to_string(), catalog_of("item", &vec!["./S90.00-N00.00.json".to_string(); n]));
            run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-01-31"), 100).0.unwrap()
        };
        let limit = super::super::catalog::MAX_LINKS;
        assert_eq!((at(limit).items.len(), at(limit).unread), (1, 0), "all the links followed: nothing missing");
        assert_eq!((at(limit + 1).items.len(), at(limit + 1).unread), (1, 1), "one past: the column isn't read whole");
    }

    #[test]
    fn a_date_written_as_a_file_is_counted_not_read() {
        let mut docs = tree("YYYY-MM", &["2024-01"]);
        let mut root: Value = serde_json::from_slice(&docs[ROOT]).unwrap();
        root["links"].as_array_mut().unwrap().push(json!({"rel": "child", "href": "./2024-02.json"}));
        docs.insert(ROOT.to_string(), root.to_string().into_bytes());
        docs.insert("https://x.org/c/2024-02.json".to_string(), catalog_of("child", &["./1/catalog.json".to_string()]));
        let (r, reads) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-02-29"), 100);
        assert!(!reads.iter().any(|u| u.ends_with("2024-02.json")), "{reads:?}");
        let found = r.unwrap();
        assert_eq!((found.items.len(), found.unread), (1, 1));
    }

    #[test]
    fn a_date_listed_twice_is_one_date() {
        let mut docs = tree("YYYY-MM", &["2024-01"]);
        let mut root: Value = serde_json::from_slice(&docs[ROOT]).unwrap();
        root["links"].as_array_mut().unwrap().push(json!({"rel": "child", "href": "./2024-01/catalog.json"}));
        docs.insert(ROOT.to_string(), root.to_string().into_bytes());
        let (r, _) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-01-31"), 1);
        assert_eq!(r.unwrap().items.len(), 1, "one tile at one date is within a cap of 1");
    }

    #[test]
    fn a_date_whose_folders_fail_is_counted_though_its_items_are_read() {
        // The last date's level can't be read: the date before gives the area's items, every item
        // is read, and the failed discovery is still told.
        let mut docs = tree("YYYY-MM", &["2024-01", "2024-02", "2024-03"]);
        docs.remove("https://x.org/c/2024-03/1/catalog.json");
        let (r, _) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-03-31"), 100);
        let found = r.unwrap();
        assert_eq!((found.items.len(), found.unread), (3, 1));
        // A level written as a file isn't a level: the same.
        let mut docs = tree("YYYY-MM", &["2024-01", "2024-02"]);
        docs.insert("https://x.org/c/2024-02/catalog.json".to_string(), catalog_of("child", &["./1.json".to_string()]));
        let (r, _) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-02-29"), 100);
        assert_eq!(r.unwrap().unread, 1);
    }

    #[test]
    fn hostile_links_are_not_followed() {
        let mut docs = tree("YYYY-MM", &["2024-01"]);
        // Into the local network, and out of the date's folder: neither is an item of the cube.
        docs.insert(
            "https://x.org/c/2024-01/1/W090.00-E000.00/catalog.json".to_string(),
            catalog_of("item", &["http://127.0.0.1/x/S90.00-N00.00.json".to_string(), "https://y.org/S90.00-N00.00.json".to_string()]),
        );
        let (r, reads) = run(&docs, [-50.0, -20.0, -40.0, -10.0], ("2024-01-01", "2024-01-31"), 100);
        let found = r.unwrap();
        assert!(found.items.is_empty());
        assert_eq!(found.unread, 2, "both refused items are told");
        assert!(reads.iter().all(|u| u.starts_with("https://x.org/")), "{reads:?}");
        // Names that aren't dates, and spans that aren't degrees.
        assert_eq!(degrees("W090.00-E000.00"), Some((-90.0, 0.0)));
        assert_eq!(degrees("S90.00-N00.00"), Some((-90.0, 0.0)));
        for bad in ["", "W090", "X1-E2", "W-E", "Wnan-E1", "Winf-E1"] {
            assert_eq!(degrees(bad), None, "{bad}");
        }
        assert_eq!(
            (name_of("https://x.org/a/2024-01/catalog.json"), name_of("https://x.org/a/S90.00-N00.00.json?x=1")),
            ("2024-01", "S90.00-N00.00")
        );
    }
}
