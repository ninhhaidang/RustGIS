//! The catalog of data cubes (opengeos/RustGIS#683): `cube.catalog` lists its entries by theme,
//! area and words, and `cube.open` adds one to the map through the command that reads it
//! (`stac.addCube`, or `layer.addZarr` following the time slider), with the entry's collection,
//! band, range and cloud cap. `cube.trend` and `cube.reduce` take an entry by `"cube"` too
//! ([`with_entry`]). JAXA Earth's datasets join the list once the Data Cubes pane has read its
//! catalog (`rustgis_tiles::jaxa`); `cube.catalog` says where that stands.

use std::borrow::Cow;

use serde_json::{Value, json};

use super::ps;
use crate::render::rustgis_tiles::cubes::{self, Entry, Source};
use crate::render::rustgis_tiles::jaxa;
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("cube.catalog", "Data Cube Catalog", "Home › Add › Web Data", catalog).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"theme"?: "vegetation"|"land cover"|"water"|"climate"|"ocean"|"atmosphere"|"elevation", "bbox"?: [west, south, east, north] | "view" (entries with data there; default: anywhere), "query"?: "words, all of them in an entry's name, provider, id or keywords (a JAXA Earth entry's bands and step too)"} → the data cubes ready to open (cube.open), each with its provider, themes, area, period, step, resolution, source (a STAC collection and band, or a Zarr variable), licence and credit; JAXA Earth's datasets (a cube a band) once the Data Cubes pane has read its catalog, and "jaxa": where that stands (unread, reading, read with the datasets left out and why, or failed)"#,
        ),
        C::new("cube.open", "Open a Data Cube", "Home › Add › Web Data", open).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"id": an entry of cube.catalog (a JAXA Earth dataset's is "<dataset id>/<band>"), "bbox"?: [west, south, east, north] | "view" (a STAC entry's area, default the map's extent; a Zarr entry's variable is read whole, but an area given must meet the entry's), "start"?, "end"?: dates, both or neither (default: the entry's last full calendar year), "maxCloud"?: percent (a STAC entry's cloud cover cap; default the entry's), "range"?: [low, high] | "auto" (a STAC entry's values shown from black to white, in the band's units; default the entry's, or "auto" for one that gives none: the 2nd–98th percentiles of a sample of the files, as stac.addCube takes it), "name"?: text} → the cube on the map, following the time slider, as stac.addCube or layer.addZarr adds it (desktop); an entry with a credit (JAXA Earth's) puts it on the cube's tile layers and gives its collection's licence"#,
        ),
    ]
}

fn catalog(s: &mut Session, p: &Value) -> CmdResult {
    // No area given: anywhere (not the map's extent, as for the commands that read data).
    let area = match p.get("bbox").filter(|v| !v.is_null()) {
        None => None,
        Some(_) => Some(super::catalogs::area(s, p)?),
    };
    // A filter of another kind is refused, not dropped, which would list every entry.
    if let Some(k) = ["theme", "query"].into_iter().find(|k| p.get(*k).is_some_and(|v| !v.is_null() && !v.is_string())) {
        return Err(format!("{k} is text"));
    }
    // The hosted list, once the app has read it; else the built-in one.
    let all = cubes::list();
    let list = cubes::select(&all, ps(p, "theme"), area, ps(p, "query"))?;
    let entries: Vec<Value> = list
        .iter()
        .map(|e| {
            let mut v = e.json();
            if let Some(Ok(l)) = jaxa::licence(&e.url) {
                v["licenseUrl"] = json!(l.url);
            }
            v
        })
        .collect();
    Ok(json!({"count": list.len(), "cubes": entries, "themes": cubes::THEMES, "jaxa": jaxa_status()}))
}

/// Where JAXA Earth's catalog stands: its datasets left out, with why, once read.
fn jaxa_status() -> Value {
    match jaxa::status() {
        jaxa::Status::Unread => json!({"state": "unread", "note": "read when the Data Cubes pane is first shown"}),
        jaxa::Status::Reading => json!({"state": "reading"}),
        jaxa::Status::Read(c) => json!({
            "state": "read", "cubes": c.entries.len(), "datasets": c.datasets,
            "leftOut": c.left_out.iter().map(|l| json!({"id": l.id, "title": l.title, "why": l.why})).collect::<Vec<_>>(),
            "cut": c.cut,
            "hidden": cubes::jaxa_hidden(),
        }),
        jaxa::Status::Failed(e) => json!({"state": "failed", "error": e}),
    }
}

/// `cube.open`. It runs on the UI thread, as `stac.addCube` (which it calls) does: their STAC search
/// and first reads of the files aren't on a background path yet, and this one moves with them. What
/// it adds there is bounded: a JAXA collection's licence, at most 1 MB within 60 s, kept for the
/// run once read (one that failed after 5 s or more is asked for again only 10 minutes later); and
/// a range's sample, the newest file of up to four tiles, read at an overview: each file's header
/// and values are two reads of up to two minutes apiece, so a server that stalls on every read
/// holds the window up to 16 minutes in all. The sample comes after the refusals that read no file
/// (room for the tile layers, web addresses, the slider's steps); a cube refused once its files
/// are opened (one that isn't a cloud-optimised GeoTIFF read in place) has spent it.
fn open(s: &mut Session, p: &Value) -> CmdResult {
    let id = ps(p, "id").map(str::trim).filter(|i| !i.is_empty()).ok_or("id is required (an entry of cube.catalog)")?;
    open_entry(s, &find(id)?, p)
}

/// The catalog's entry `id`. One not found, while JAXA Earth's catalog isn't read (its datasets
/// join the list then), says where that stands.
fn find(id: &str) -> Result<Entry, String> {
    cubes::find(id).map_err(|e| {
        let why = match jaxa::status() {
            jaxa::Status::Read(_) => return e,
            jaxa::Status::Unread => "it hasn't been read in this run (the Data Cubes pane reads it when first shown)".to_string(),
            jaxa::Status::Reading => "it is being read".to_string(),
            jaxa::Status::Failed(f) => format!("it couldn't be read ({f})"),
        };
        format!("{e}; JAXA Earth's datasets join it once their catalog is read, and {why}")
    })
}

/// Adds `e` to the map, with the area, period and options of `p` (as `cube.open` takes them).
pub(crate) fn open_entry(s: &mut Session, e: &Entry, p: &Value) -> CmdResult {
    let given = |k: &str| p.get(k).filter(|v| !v.is_null());
    let zarr = matches!(e.source, Source::Zarr { .. });
    // A band's range and a scene collection's cloud cap don't apply to a Zarr variable.
    if let Some(k) = ["range", "maxCloud"].into_iter().find(|k| zarr && given(k).is_some()) {
        return Err(format!("{k} is a STAC entry's; {} is a Zarr variable", e.id));
    }
    let (start, end) = period(e, p)?;
    // A STAC entry's area, the map's extent by default. A Zarr entry's variable is read whole,
    // but an area given is checked all the same.
    let area = if zarr && given("bbox").is_none() { None } else { Some(super::catalogs::area(s, p)?) };
    if area.is_some_and(|a| !e.overlaps(a)) {
        let [w, so, ea, n] = e.bbox;
        return Err(format!("{} has data in [{w}, {so}, {ea}, {n}] only, which the area doesn't meet", e.name));
    }
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty());
    let mut r = match &e.source {
        Source::Stac { collection, asset } => {
            let mut q = json!({"url": e.url, "collection": collection, "asset": asset, "bbox": area, "start": start, "end": end});
            // An entry that gives no range (JAXA Earth's) takes one from a sample of its files.
            q["range"] = given("range").cloned().unwrap_or_else(|| e.range.map_or_else(|| json!("auto"), |r| json!(r)));
            if let Some(c) = given("maxCloud").cloned().or_else(|| e.max_cloud.map(|c| json!(c))) {
                q["maxCloud"] = c;
            }
            if let Some(n) = name {
                q["name"] = json!(n);
            }
            if !e.credit.is_empty() {
                q["attribution"] = json!(e.credit);
            }
            let mut r = s.run("stac.addCube", &q)?;
            if !e.credit.is_empty() {
                credit(s, e, &mut r);
            }
            r
        }
        Source::Zarr { variable } => {
            let mut q = json!({"url": e.url, "variable": variable, "time": true, "start": start, "end": through(&end)});
            q["name"] = json!(name.map_or_else(|| e.name.clone(), str::to_string));
            s.run("layer.addZarr", &q)?
        }
    };
    if let Some(o) = r.as_object_mut() {
        o.insert("cube".into(), json!(e.id));
        o.insert("start".into(), start);
        o.insert("end".into(), end);
        if zarr && area.is_some() {
            o.insert("note".into(), json!("a Zarr entry's variable is read whole; bbox was left aside"));
        }
    }
    Ok(r)
}

/// Gives the credit a STAC cube's tile layers took (`stac.addCube`'s `attribution`), as its terms
/// ask, and its collection's licence (a JAXA Earth dataset's: JAXA's terms for most, NASA's or
/// Copernicus's for some), or says why it wasn't read.
fn credit(s: &mut Session, e: &Entry, r: &mut Value) {
    r["credit"] = json!(e.credit);
    // The browser reads the licence in the pane instead.
    #[cfg(target_arch = "wasm32")]
    let _ = s;
    #[cfg(not(target_arch = "wasm32"))]
    match jaxa::licence_blocking(&e.url) {
        Ok(l) => r["license"] = json!({"url": l.url, "title": l.title}),
        Err(why) => s.warn(format!("{}: its collection's licence wasn't read ({why}); JAXA's terms are at {}", e.name, jaxa::TERMS)),
    }
}

/// The period asked for (both ends or neither), else the entry's last full calendar year; one
/// wholly outside the entry's dates is refused, naming them.
fn period(e: &Entry, p: &Value) -> Result<(Value, Value), String> {
    let given = |k: &str| p.get(k).filter(|v| !v.is_null()).cloned();
    let (start, end) = match (given("start"), given("end")) {
        (Some(a), Some(b)) => (a, b),
        (None, None) => {
            let now = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map_err(|_| "the system clock is before 1970; give start and end explicitly")?
                .as_millis() as f64;
            let this_year = rustgis_data::time::format(now, "YYYY")
                .parse::<i32>()
                .map_err(|_| "the system clock's year could not be read; give start and end explicitly")?;
            let y = e.last_year(this_year);
            (json!(format!("{y}-01-01")), json!(format!("{y}-12-31")))
        }
        _ => return Err("give start and end together, or neither for the entry's last full year".into()),
    };
    let ms = |v: &Value| rustgis_data::time::parse_input(v);
    let (first, last) = (rustgis_data::time::parse_text(&e.start), e.end.as_deref().and_then(rustgis_data::time::parse_text));
    // An end given as a year or a month reaches its last day.
    let until = super::stac::period_after(&end).map(|a| a - 1.0).or_else(|| ms(&end));
    if until.zip(first).is_some_and(|(u, f)| u < f) || ms(&start).zip(last).is_some_and(|(s, l)| s > l) {
        let to = e.end.as_deref().map_or_else(|| "to date".to_string(), |d| format!("to {d}"));
        return Err(format!("{} runs from {} {to}; the period is outside it", e.name, e.start));
    }
    Ok((start, end))
}

/// A period's end as `layer.addZarr` takes it, an instant: a day, a month or a year given reaches
/// its last moment, as `stac.addCube` takes them, so an hourly variable's last day is read whole.
fn through(end: &Value) -> Value {
    let day = end.as_str().map(str::trim).filter(|d| d.len() == 10).and_then(rustgis_data::time::parse_text).map(|d| d + 86_400_000.0);
    super::stac::period_after(end).or(day).map_or_else(|| end.clone(), |after| json!(after - 1.0))
}

/// `p` with the entry named by `"cube"` filled in, for `cube.trend` and `cube.reduce`: a STAC
/// entry's API, collection, band, cell size and cloud cap (the last two unless given), or a Zarr
/// entry's store and variable. Keys naming other data are refused alongside it, since the entry
/// would override them.
pub fn with_entry(p: &Value) -> Result<Cow<'_, Value>, String> {
    let Some(id) = p.get("cube").filter(|v| !v.is_null()) else { return Ok(Cow::Borrowed(p)) };
    let e = find(id.as_str().ok_or("cube is an entry's id, from cube.catalog")?)?;
    if let Some(k) =
        ["layer", "layers", "path", "url", "collection", "asset", "variable"].into_iter().find(|k| p.get(*k).is_some_and(|v| !v.is_null()))
    {
        return Err(format!("cube names the data to read; leave out {k}"));
    }
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.remove("cube");
        o.insert("url".into(), json!(e.url));
        match &e.source {
            Source::Stac { collection, asset } => {
                o.insert("collection".into(), json!(collection));
                o.insert("asset".into(), json!(asset));
                for (k, v) in [("cellSize", e.cell_size), ("maxCloud", e.max_cloud)] {
                    if let Some(v) = v.filter(|_| o.get(k).is_none_or(Value::is_null)) {
                        o.insert(k.into(), json!(v));
                    }
                }
            }
            Source::Zarr { variable } => {
                o.insert("variable".into(), json!(variable));
            }
        }
    }
    Ok(Cow::Owned(q))
}

/// Whether `cube.open` with `p` opens a Zarr variable, which a plugin may do only as it may run
/// `layer.addZarr` (with the files and network permissions).
pub fn opens_zarr(p: &Value) -> bool {
    ps(p, "id").and_then(|id| cubes::find(id).ok()).is_some_and(|e| matches!(e.source, Source::Zarr { .. }))
}
