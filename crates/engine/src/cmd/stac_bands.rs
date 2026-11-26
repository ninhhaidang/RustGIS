//! `stac.addBands`: a band combination of a STAC item as one multi-band raster layer.
//!
//! Sentinel-2 and Landsat publish one file per band, so a colour image is made from three files.
//! This reads the window of each (the view or a box, in place by range, as `layer.addUrl` with a
//! `bbox` does), puts them on one grid (the finest of the three, by nearest cell) and keeps them as
//! one raster of three bands drawn as red, green and blue. Scale and offset from the catalog are
//! applied, so the values are what the bands measure (reflectance), and a cell with no value in
//! any of the three is no data in all of them. The band names and which combinations an item
//! offers are [`bands`](crate::render::rustgis_tiles::stac::bands)'s.
//!
//! It makes its raster on another thread from the app, with Cancel ([`super::workflows`]).

use serde_json::Value;

use super::workflows::Work;
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![C::new("stac.addBands", "Add STAC Bands", "Home › Add › Web Data › STAC Search", add_bands).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
        r#"{"item": a STAC item (from stac.search), "combination"?: "trueColor"|"colorInfrared"|"shortwaveInfrared"|"falseColorUrban"|"agriculture"|"geology"|"bathymetric" (default trueColor), "red"?, "green"?, "blue"?: band names (red, nir, swir16… or an asset's key) used instead of a combination, "bbox"?: [west, south, east, north] in degrees | "view" (default), "cellSize"?: the coarsest overview whose cells are at most this size is read (default: about 2,048 cells across the box), "name"?: text, "ran"?: (internal: a raster the app has worked out)}"#,
    ),
    C::new("stac.addIndex", "Add STAC Index", "Home › Add › Web Data › STAC Search", add_index).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
        r#"{"item": a STAC item (from stac.search), "index": "ndvi"|"ndwi"|"savi"|"nbr"|"ndbi"|"evi" (the Indices section's formulas, on the item's bands: NDVI nir and red, NDWI green and nir, SAVI nir and red, NBR nir and swir22, NDBI swir16 and nir, EVI nir, red and blue; their scale and offset applied), "bbox"?: [west, south, east, north] in degrees | "view" (default), "cellSize"?: the coarsest overview whose cells are at most this size is read (default: about 2,048 cells across the box), "name"?: text, "ran"?: (internal: a raster the app has worked out)} → a raster layer of one band"#,
    )]
}

fn add_bands(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "stac.addBands")
}

fn add_index(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "stac.addIndex")
}

/// The browser has no thread to wait for several files on.
#[cfg(target_arch = "wasm32")]
const IN_THE_BROWSER: &str = "band combinations are made on the desktop and the command line for now";

/// What `stac.addBands` reads of the session (the item given, the view), and the work that reads
/// the three files and makes the raster.
pub(crate) fn bands_work(s: &Session, p: &Value) -> Result<Work, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err(IN_THE_BROWSER.into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    desktop::work(s, p)
}

/// What `stac.addIndex` reads of the session (the item given, the view), and the work that reads
/// the bands' files and makes the index.
pub(crate) fn index_work(s: &Session, p: &Value) -> Result<Work, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err(IN_THE_BROWSER.replace("band combinations are", "indices are"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    desktop::index(s, p)
}

#[cfg(not(target_arch = "wasm32"))]
mod desktop {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, Ordering};

    use rustgis_data::raster::Raster;
    use rustgis_map::{Ramp, RasterLayer, RasterRenderer, Stretch};
    use serde_json::{Value, json};

    use super::super::layer::{WindowAsked, WindowRaster};
    use super::super::workflows::{Made, Then, Work};
    use crate::Session;
    use crate::render::rustgis_tiles::stac::{self, bands as sb};

    /// A window's cells across, when no cell size is given: its overview is the one nearest this.
    const ACROSS: f64 = 2048.0;

    /// A kilometre of a degree of latitude, in metres.
    const DEGREE_M: f64 = 111_320.0;

    /// The item given and the bands its GeoTIFF files offer; `to` says what they are read for
    /// (`a band combination`, `an index`), for the refusal of an item with none.
    fn item_of(p: &Value, to: &str) -> Result<(stac::Item, Vec<sb::Band>), String> {
        let item = match p.get("item").filter(|v| v.is_object()) {
            Some(v) => stac::item_from_json(v, None).ok_or("item isn't a STAC item")?,
            None => return Err("give item: a STAC item, as stac.search answers".into()),
        };
        let all = sb::bands(&item);
        if all.is_empty() {
            return Err(format!("{}: it has no GeoTIFF asset to make {to} from", item.id));
        }
        Ok((item, all))
    }

    /// What reading the bands `wanted` takes: each band found (or an error naming what the item has),
    /// each file's address (checked as `stac.addAsset` checks it), and the window and cell size.
    fn plan(
        s: &Session,
        p: &Value,
        item: &stac::Item,
        all: &[sb::Band],
        wanted: &[String],
        keys_first: bool,
    ) -> Result<(Vec<sb::Band>, Vec<String>, [f64; 4], (f64, bool)), String> {
        let mut picked = Vec::with_capacity(wanted.len());
        for name in wanted {
            // A band asked for by name by the user (not a preset's) may be an exact asset key: it comes first.
            let found = if keys_first { sb::find_key(all, name).or_else(|| sb::find(all, name)) } else { sb::find(all, name) };
            let band = found.ok_or_else(|| {
                let cut = sb::cut_note(item).map(|n| format!("; {n}")).unwrap_or_default();
                format!("{}: no band '{name}' (it has {}){cut}", item.id, names_of(all))
            })?;
            picked.push(band.clone());
        }
        // A band the catalog gives unusable numbers for is refused before any file is read.
        refuse_unusable(picked.iter(), wanted)?;
        let mut hrefs = Vec::with_capacity(picked.len());
        for band in &picked {
            hrefs.push(item.asset_href(Some(&band.asset))?.1);
        }
        let mut asked = p.clone();
        if asked.get("bbox").is_none_or(Value::is_null) {
            asked["bbox"] = json!("view");
        }
        let bbox = super::super::layer::window_box_of(s, &asked)?;
        // The cell size, and whether it is the automatic one (the user gave none).
        let cell = match p.get("cellSize").filter(|v| !v.is_null()) {
            None => (auto_cell(bbox), true),
            Some(v) => (v.as_f64().filter(|c| c.is_finite() && *c > 0.0).ok_or("cellSize is a size above 0")?, false),
        };
        Ok((picked, hrefs, bbox, cell))
    }

    pub(super) fn work(s: &Session, p: &Value) -> Result<Work, String> {
        let (item, all) = item_of(p, "a band combination")?;
        // A parameter of the other command is refused, not dropped.
        if given(p, "index") {
            return Err("index is for stac.addIndex (stac.addBands takes a combination, or red, green and blue)".into());
        }
        // The three bands: a combination, or the three named.
        let named = ["red", "green", "blue"].map(|k| text(p, k)).into_iter().collect::<Result<Vec<_>, _>>()?;
        let named: [Option<&str>; 3] = named.try_into().map_err(|_| "red, green and blue are names".to_string())?;
        // One or the other: a combination given with the three bands would be dropped without a word.
        let combination = text(p, "combination")?;
        if named.iter().any(Option::is_some) && combination.is_some() {
            return Err("give a combination, or red, green and blue, not both".into());
        }
        let (label, wanted): (String, [String; 3]) = if named.iter().any(Option::is_some) {
            let [Some(r), Some(g), Some(b)] = named else { return Err("give red, green and blue together, or a combination".into()) };
            (format!("{r}-{g}-{b}"), [r.to_string(), g.to_string(), b.to_string()])
        } else {
            let id = combination.unwrap_or("trueColor");
            let c = sb::COMBINATIONS.iter().find(|c| c.id.eq_ignore_ascii_case(id)).ok_or_else(|| {
                let ids: Vec<&str> = sb::COMBINATIONS.iter().map(|c| c.id).collect();
                format!("unknown combination '{id}' (the combinations are {})", ids.join(", "))
            })?;
            (c.name.to_string(), c.channels.map(str::to_string))
        };
        let custom = named.iter().any(Option::is_some);
        let (picked, hrefs, bbox, cell) = plan(s, p, &item, &all, &wanted, custom)?;
        let name = match text(p, "name")? {
            Some(n) => super::super::within("name", "the output's name", n, 200)?,
            None => format!("{} {label}", item.id),
        };
        let channels = [wanted[0].clone(), wanted[1].clone(), wanted[2].clone()];
        Ok(Box::new(move || {
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let (made, notes) = read_and_stack(&name, &channels, &picked, &hrefs, bbox, cell)?;
            made_saying(made, notes, json!({"bands": channels, "bbox": bbox}))
        }))
    }

    /// `stac.addIndex`: the bands an index reads, read as a combination's are (the item's files, by
    /// range, over the window), and the index made from them as one band of numbers, by the
    /// Indices section's formula. The band math checks Cancel and counts its rows as it runs (see
    /// `band_math`); Cancel is also looked at before each file.
    pub(super) fn index(s: &Session, p: &Value) -> Result<Work, String> {
        let (item, all) = item_of(p, "an index")?;
        if let Some(k) = ["combination", "red", "green", "blue"].into_iter().find(|k| given(p, k)) {
            return Err(format!("{k} is for stac.addBands (stac.addIndex takes an index)"));
        }
        let id = text(p, "index")?.ok_or_else(|| {
            let ids: Vec<&str> = sb::INDICES.iter().map(|i| i.id).collect();
            format!("give index: {}", ids.join(", "))
        })?;
        let ix = sb::INDICES.iter().find(|i| i.id.eq_ignore_ascii_case(id)).ok_or_else(|| {
            let ids: Vec<&str> = sb::INDICES.iter().map(|i| i.id).collect();
            format!("unknown index '{id}' (the indices are {})", ids.join(", "))
        })?;
        let wanted: Vec<String> = ix.channels.iter().map(|c| (*c).to_string()).collect();
        let (picked, hrefs, bbox, cell) = plan(s, p, &item, &all, &wanted, false)?;
        // Where each input is in the bands read, for the formula (the ones it doesn't use are never named).
        let at = |names: &[&str]| ix.channels.iter().position(|c| names.contains(c)).map_or(1, |i| i as u64 + 1);
        let expr = super::super::imagery::index_expression(
            ix.id,
            at(&["blue"]),
            at(&["green"]),
            at(&["red"]),
            at(&["nir"]),
            at(&["swir16", "swir22"]),
        )?;
        let name = match text(p, "name")? {
            Some(n) => super::super::within("name", "the output's name", n, 200)?,
            None => format!("{} {}", item.id, ix.id.to_ascii_uppercase()),
        };
        let id = ix.id;
        Ok(Box::new(move || {
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let (stacked, notes) = read_and_stack(&name, &wanted, &picked, &hrefs, bbox, cell)?;
            let band = rustgis_analysis::rasterops::band_math(&stacked, &expr)?;
            // An index with no value anywhere (every cell 0 / 0, say) is no result: an error, not an empty layer.
            if !rustgis_analysis::rasterops::has_values(&band, stacked.width as usize)? {
                return Err(format!(
                    "the {id} index has no value in any cell: its bands are zero or empty here, so it can't be worked out"
                ));
            }
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let raster = rustgis_analysis::rasterops::derive(&stacked, &name, vec![band])?;
            let then = noted(raster.name.clone(), notes);
            // The encode below is one step with no look inside it: Cancel is looked at before it.
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let bytes = rustgis_data::raster::geotiff::write_all(&raster).map_err(|e| e.to_string())?;
            let layer_name = raster.name.clone();
            let mut layer = RasterLayer::from_written(raster, format!("{layer_name}.tif"), bytes);
            // Drawn in colour over the range the index means something in, not grey over this window's own.
            let (ramp, min, max) = index_look(id);
            layer.style.renderer = RasterRenderer::Stretch { band: 0, ramp, invert: false };
            layer.style.stretch = Stretch::Custom { min, max, bands: None };
            Ok(super::super::workflows::made_layer(
                layer_name,
                layer,
                Some(json!({"index": id, "bands": wanted, "bbox": bbox})),
                Some(then),
            ))
        }))
    }

    /// The colours an index is drawn in and the values they run between (low to high): greens for
    /// the vegetation indices, blues for water, red to blue for a burn ratio (burnt to healthy), and
    /// blue to red for the built-up index. The range is the index's own, so two scenes compare.
    pub(super) fn index_look(id: &str) -> (Ramp, f64, f64) {
        match id {
            "ndwi" => (Ramp::Lagoon, -0.5, 0.5),
            "nbr" => (Ramp::Diverging, -0.5, 0.5),
            "ndbi" => (Ramp::Thermal, -0.5, 0.5),
            "evi" => (Ramp::Forest, 0.0, 1.0),
            _ => (Ramp::Forest, -0.1, 0.9),
        }
    }

    /// Whether a parameter is given: there, and neither null nor blank text.
    fn given(p: &Value, key: &str) -> bool {
        p.get(key).is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty()))
    }

    /// A text parameter: its trimmed text, `None` when it is missing, null or blank, and an error
    /// when it is something else (a number would otherwise be taken as missing, and the default used).
    fn text<'a>(p: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(t)) => Ok(Some(t.trim()).filter(|t| !t.is_empty())),
            Some(_) => Err(format!("{key} is text")),
        }
    }

    /// The names a bands list offers, for an error: common names, else keys, each once.
    fn names_of(all: &[sb::Band]) -> String {
        let mut seen = BTreeSet::new();
        let names: Vec<&str> = all
            .iter()
            .map(|b| if b.common.is_empty() { b.asset.as_str() } else { b.common.as_str() })
            .filter(|n| seen.insert(*n))
            .collect();
        // At most 24 are listed, and the list says when there are more.
        let more = if names.len() > 24 { ", …" } else { "" };
        format!("{}{more}", names.iter().take(24).copied().collect::<Vec<_>>().join(", "))
    }

    /// The cell size that reads a box about [`ACROSS`] cells across: the overview at least that
    /// coarse, or the full image of a window small enough for it.
    pub(super) fn auto_cell(bbox: [f64; 4]) -> f64 {
        let [west, south, east, north] = bbox;
        let mid = ((south + north) / 2.0).to_radians().cos().abs().max(0.01);
        let (w, h) = ((east - west).abs() * DEGREE_M * mid, (north - south).abs() * DEGREE_M);
        (w.max(h) / ACROSS).max(0.001)
    }

    /// One file's window, every band, from its signed address (only the file's own address is ever said).
    fn read_one(signed: &str, bbox: [f64; 4], cell: (f64, bool), bands: usize, stop: &AtomicBool) -> Result<WindowRaster, String> {
        let asked = WindowAsked::of(signed, bbox, Some(cell.0), bands);
        super::super::layer::read_window_raster(&if cell.1 { asked.automatic() } else { asked }, stop)
    }

    /// The windows of these files (address, bands to read), in order: the files are independent and
    /// network-bound, so up to the user's thread count are read at once, each on a thread of its own
    /// while this one looks at the run's Cancel and stops the reads once it is set. The same windows
    /// as reading them one by one. A thread the system refuses costs only that: the file is read
    /// here, Cancel looked at before it starts (a read here, once started, runs to its end).
    ///
    /// Memory: up to the thread count of windows are held at once (at most a combination's three, an
    /// index's two to three), each bounded by the window reader's own limit on values. An ordinary
    /// window (one band, about 4 million values) is far below it; files of many bands can take up to
    /// about that many times one window's peak, where reading them one at a time took one.
    pub(super) fn read_windows(files: &[(String, usize)], bbox: [f64; 4], cell: (f64, bool)) -> Result<Vec<WindowRaster>, String> {
        // The run's request predicate is of this thread: each reader thread is given it, as the shared
        // helpers give their workers, so a request whose claim has gone is not sent.
        let still = rustgis_data::source::remote::current_still();
        // The requests the reads make belong to the session that asked: its diagnostics capture goes
        // with each reader thread, as the source readers' does (the network readers have no test).
        let diagnostics = rustgis_data::diagnostics::Capture::current();
        // Signed here, in order, before any thread starts: a Planetary Computer file is signed when it is
        // read (not when the item was found), the first token fetched is cached for the files of its
        // container, and Cancel is looked at before each request.
        let mut signed = Vec::with_capacity(files.len());
        for (href, _) in files {
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let url = stac::sign_blocking(href)?;
            // The item is a document's: its files connect to public addresses only, whatever a public-looking
            // name resolves to. Marked (the address as given, and as signed) before any reader starts.
            crate::render::rustgis_tiles::cache::restrict_to_public(href);
            crate::render::rustgis_tiles::cache::restrict_to_public(&url);
            signed.push(url);
        }
        let stop = AtomicBool::new(false);
        let cause = std::sync::Mutex::new(None);
        let mut out = Vec::with_capacity(files.len());
        let jobs: Vec<(&String, &String, usize)> = files.iter().zip(&signed).map(|((href, bands), s)| (href, s, *bands)).collect();
        for chunk in jobs.chunks(rustgis_data::threads::max().max(1)) {
            let results: Vec<Result<WindowRaster, String>> = std::thread::scope(|scope| {
                let started: Vec<_> = chunk
                    .iter()
                    .map(|(href, signed, bands)| {
                        // Taken as references and values, so the reader thread owns what it uses.
                        let (stop_r, cause_r, diagnostics_r, still_r) = (&stop, &cause, &diagnostics, &still);
                        let (signed_s, bands_n) = (signed.as_str(), *bands);
                        let message = format!("{href}: the read failed unexpectedly");
                        let reader = std::thread::Builder::new().name("STAC band window".into()).spawn_scoped(scope, move || {
                            let _diagnostics = diagnostics_r.enter();
                            rustgis_data::source::remote::with_optional_still(still_r.clone(), || {
                                // A reader that panics stops the run too (the guard, unless it is disarmed on return).
                                let mut guard = PanicStop { stop: stop_r, cause: cause_r, message, armed: true };
                                let read = read_one(signed_s, bbox, cell, bands_n, stop_r);
                                guard.armed = false;
                                // A file that failed stops the reads beside it: they would only finish to be dropped.
                                if let Err(e) = &read {
                                    note_failure(stop_r, cause_r, e);
                                }
                                read
                            })
                        });
                        (href, signed, *bands, reader)
                    })
                    .collect();
                while started.iter().any(|(_, _, _, r)| r.as_ref().is_ok_and(|r| !r.is_finished())) {
                    if rustgis_data::progress::check().is_err() {
                        stop.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                started
                    .into_iter()
                    .map(|(href, signed, bands, reader)| match reader {
                        Ok(r) => r.join().unwrap_or_else(|_| Err(format!("{href}: the read failed unexpectedly"))),
                        Err(_) => {
                            // Looked at before it starts: once started, this read runs to its end.
                            rustgis_data::progress::check().map_err(|e| e.to_string())?;
                            let read = read_one(signed, bbox, cell, bands, &stop);
                            if let Err(e) = &read {
                                note_failure(&stop, &cause, e);
                            }
                            read
                        }
                    })
                    .collect()
            });
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            if let Some(e) = cause.lock().ok().and_then(|c| c.clone()) {
                return Err(e);
            }
            for r in results {
                out.push(r?);
            }
        }
        Ok(out)
    }

    /// Stops the run when a reader thread panics: dropped still armed (the read didn't return), it
    /// notes the failure as an `Err` would be, so the reads beside it end instead of running on.
    pub(super) struct PanicStop<'a> {
        pub(super) stop: &'a AtomicBool,
        pub(super) cause: &'a std::sync::Mutex<Option<String>>,
        pub(super) message: String,
        pub(super) armed: bool,
    }

    impl Drop for PanicStop<'_> {
        fn drop(&mut self) {
            if self.armed {
                note_failure(self.stop, self.cause, &self.message);
            }
        }
    }

    /// A read that failed: if it is the first thing to stop the run (the user's Cancel has not),
    /// it stops the reads beside it and its message is the one kept to say. What those then answer
    /// is not a cause, whatever it says.
    pub(super) fn note_failure(stop: &AtomicBool, cause: &std::sync::Mutex<Option<String>>, message: &str) {
        if stop.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_ok()
            && let Ok(mut said) = cause.lock()
        {
            *said = Some(message.to_string());
        }
    }

    /// What a window note is about, apart from the numbers and edge names that differ from file to
    /// file: the note with its digits and the edges the box reaches past taken out.
    pub(super) fn note_kind(note: &str) -> String {
        if let Some((before, rest)) = note.split_once("the box reaches past the file's ")
            && let Some((_, after)) = rest.split_once(" edge(s)")
        {
            return format!("{before}the box reaches past the file's edge(s){after}");
        }
        note.chars().filter(|c| !c.is_ascii_digit() && *c != '.').collect()
    }

    /// What each file's read said (the bands that take the file, its notes), as one notice for a kind,
    /// naming every band of the files it is for and how many files. The notes of a kind differ by a
    /// number or an edge name from file to file, so they are matched by [`note_kind`], and each file's
    /// own wording is kept; a file counts once for a kind, however many of its notes are of it.
    pub(super) fn merge_notes(files: &[(Vec<String>, Vec<String>)]) -> Vec<String> {
        let mut merged: Vec<(String, Vec<String>, Vec<String>, usize)> = Vec::new();
        for (whos, notes) in files {
            let mut counted: Vec<String> = Vec::new();
            for n in notes {
                let kind = note_kind(n);
                let first = !counted.contains(&kind);
                match merged.iter_mut().find(|(k, ..)| *k == kind) {
                    Some((_, texts, bands, files)) => {
                        if !texts.contains(n) {
                            texts.push(n.clone());
                        }
                        if first {
                            bands.extend(whos.iter().cloned());
                            *files += 1;
                        }
                    }
                    None => merged.push((kind.clone(), vec![n.clone()], whos.clone(), 1)),
                }
                if first {
                    counted.push(kind);
                }
            }
        }
        merged
            .into_iter()
            .map(|(_, texts, bands, files)| {
                format!("{} ({files} file{}): {}", bands.join(", "), if files == 1 { "" } else { "s" }, texts.join("; "))
            })
            .collect()
    }

    /// The three windows, read, and put on one grid: the raster of three bands, and what to say.
    fn read_and_stack(
        name: &str,
        channels: &[String],
        picked: &[sb::Band],
        hrefs: &[String],
        bbox: [f64; 4],
        cell: (f64, bool),
    ) -> Result<(Raster, Vec<String>), String> {
        // One read for a file, whichever of its bands are wanted (the first to want it sets the count), and
        // every band of the file is read, so the count asked is the file's, not the band's place.
        // The cost grows with the file's band count (a 13-band file is about 13 times the bytes
        // and memory of the one band wanted); the values read are bounded by the window's own
        // limit, and a window reader that picks bands would spare it.
        let mut files: Vec<(String, usize)> = Vec::new();
        for (i, band) in picked.iter().enumerate() {
            let href = hrefs.get(i).ok_or("a band has no address")?;
            if !files.iter().any(|(h, _)| h == href) {
                files.push((href.clone(), band.in_file.max(band.index) as usize));
            }
        }
        let read = read_windows(&files, bbox, cell)?;
        // What each file's read said, merged into one notice for a kind (see [`merge_notes`]).
        let mut windows: Vec<(String, WindowRaster)> = Vec::new();
        let mut said: Vec<(Vec<String>, Vec<String>)> = Vec::new();
        for ((href, _), w) in files.into_iter().zip(read) {
            let whos: Vec<String> =
                picked.iter().zip(channels).zip(hrefs).filter(|(_, h)| **h == href).map(|((_, c), _)| c.clone()).collect();
            said.push((whos, w.notes()));
            windows.push((href, w));
        }
        let mut notes = merge_notes(&said);
        let sources: Vec<(&Raster, &sb::Band)> = picked
            .iter()
            .enumerate()
            .map(|(i, band)| {
                let href = hrefs.get(i).ok_or("a band has no address")?;
                let w = windows.iter().find(|(h, _)| h == href).map(|(_, w)| &w.raster).ok_or("a band was not read")?;
                Ok((w, band))
            })
            .collect::<Result<_, String>>()?;
        let (raster, said) = stack(name, channels, &sources)?;
        notes.extend(said);
        Ok((raster, notes))
    }

    /// The bands whose catalog numbers can't be used, refused by name: a scale of 0 (or one that isn't
    /// a number) would turn every value of a band into its offset, and numbers or failed-retrieval
    /// codes the catalog gave but this couldn't read would leave the values unscaled or unmasked: a
    /// catalog error to say, not a constant or wrong band to draw. They need only the bands' metadata,
    /// so they are looked at before any file is read (`plan`), and again where the bands are stacked.
    pub(super) fn refuse_unusable<'a>(bands: impl Iterator<Item = &'a sb::Band>, channels: &[String]) -> Result<(), String> {
        for (band, who) in bands.zip(channels) {
            if band.unread {
                return Err(format!(
                    "{who}: the catalog gives its scale, offset or no-data as something that is not a number (a string, say), so its values can't be scaled"
                ));
            }
            if band.codes_unread {
                return Err(format!(
                    "{who}: the catalog names codes for a failed retrieval but they could not be read whole, so its values can't be told from them"
                ));
            }
            if !band.scale.is_finite() || band.scale == 0.0 {
                return Err(format!("{who}: the catalog gives its values a scale of {}, so they can't be scaled", band.scale));
            }
        }
        Ok(())
    }

    /// The bands as one raster of as many bands on the finest grid among them, by nearest cell, with
    /// the catalog's scale and offset applied. A cell with no value in any is no data in all. The rows
    /// are worked one after another on this thread (about 4 million cell transforms a band at the
    /// default window, which is brief), looking at Cancel and counting progress on each.
    pub(super) fn stack(name: &str, channels: &[String], sources: &[(&Raster, &sb::Band)]) -> Result<(Raster, Vec<String>), String> {
        let (Some((first, _)), true) = (sources.first(), sources.len() >= 2) else { return Err("a stack needs at least two bands".into()) };
        if sources.iter().any(|(r, _)| r.crs != first.crs) {
            return Err("the bands are in different coordinate systems: they can't be put on one grid".into());
        }
        refuse_unusable(sources.iter().map(|(_, b)| *b), channels)?;
        let finest = sources
            .iter()
            .map(|(r, _)| *r)
            .min_by(|x, y| x.transform.pixel_width.abs().total_cmp(&y.transform.pixel_width.abs()))
            .ok_or("a stack needs at least two bands")?;
        let (w, h) = (finest.width as usize, finest.height as usize);
        let mut bands: Vec<Vec<f32>> = Vec::with_capacity(3);
        // Progress counts the rows of every band; each row looks at Cancel.
        rustgis_data::progress::begin(sources.len() * h);
        for (raster, band) in sources {
            let mut out = rustgis_analysis::rasterops::new_band(w, h, rustgis_analysis::rasterops::NODATA)?;
            let data = &raster.bands.get(band.index.saturating_sub(1) as usize).ok_or("a band is not in its file")?.data;
            let catalog_nodata = band.nodata.map(|v| v as f32);
            for row in 0..h {
                rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
                for col in 0..w {
                    let world = finest.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
                    let (sc, sr) = raster.transform.to_pixel(world);
                    // A geographic grid stored in another turn of longitude (-10° and 350°) is the same place.
                    let sc = raster.wrap_col(sc);
                    let (sc, sr) = (sc.floor(), sr.floor());
                    if !(sc >= 0.0 && sr >= 0.0 && sc < f64::from(raster.width) && sr < f64::from(raster.height)) {
                        continue;
                    }
                    let Some(&v) = data.get(sr as usize * raster.width as usize + sc as usize) else { continue };
                    if raster.is_valid(v) && catalog_nodata != Some(v) && !band.invalid.iter().any(|&code| code as f32 == v) {
                        let value = (f64::from(v) * band.scale + band.offset) as f32;
                        if value.is_finite()
                            && let Some(o) = out.get_mut(row * w + col)
                        {
                            *o = value;
                        }
                    }
                }
            }
            bands.push(out);
        }
        // A cell with no value in one band is no data in all: a colour from two bands is no colour.
        let none = rustgis_analysis::rasterops::NODATA;
        let mut left = 0usize;
        for i in 0..w * h {
            if bands.iter().any(|b| b.get(i).is_none_or(|v| *v == none)) {
                left += 1;
                bands.iter_mut().for_each(|b| {
                    if let Some(v) = b.get_mut(i) {
                        *v = none;
                    }
                });
            }
        }
        // Not one cell with a value in every band: an empty image is no result, so it is an error.
        if w > 0 && h > 0 && left == w * h {
            return Err(format!(
                "no cell has a value in every band ({}): the item's data doesn't cover this part of the map, or its files are empty here",
                channels.join(", ")
            ));
        }
        let mut notes = Vec::new();
        // The window reader rounds whole numbers past 2^24 and doubles to 32-bit floats: said once, by band.
        let rounded: Vec<&str> = sources
            .iter()
            .zip(channels)
            .filter(|((r, b), _)| r.rounded.get(b.index.saturating_sub(1) as usize).is_some_and(|n| *n > 0))
            .map(|(_, c)| c.as_str())
            .collect();
        if !rounded.is_empty() {
            notes.push(format!(
                "{} read with values rounded to 32-bit floats (whole numbers past 16,777,216, or more digits than a 32-bit float holds): the stacked values are the rounded ones",
                rounded.join(", ")
            ));
        }
        // A band taken from a colour image (the item has no file of its own for it) is not in the
        // units of the bands that come from band files: the catalog's scale and offset are for those.
        // One notice with the bands, and none when they all come from the one image (the units agree).
        let one_image = sources.iter().all(|(_, b)| Some(&b.href) == sources.first().map(|(_, f)| &f.href));
        let from_colour: Vec<String> = sources
            .iter()
            .zip(channels)
            .filter(|((_, b), _)| b.in_file > 1)
            .map(|((_, b), who)| format!("{who} (band {} of {})", b.index, b.asset))
            .collect();
        if !from_colour.is_empty() && !one_image {
            notes.push(format!(
                "{} {} taken from a file of several bands, as the item has no file of its own: the values may not be in the units of the other bands (a colour image's are 8-bit)",
                from_colour.join(", "),
                if from_colour.len() == 1 { "is" } else { "are" }
            ));
        }
        // Integer files the catalog gives no scale for are used as stored: not reflectance, which the
        // indices' constants and a colour mixed with scaled bands assume. One notice, naming them.
        let unscaled: Vec<String> = sources
            .iter()
            .zip(channels)
            .filter(|((r, b), _)| !b.scaled && (r.data_type.starts_with("UInt") || r.data_type.starts_with("Int")))
            .map(|((r, _), who)| format!("{who} ({})", r.data_type))
            .collect();
        if !unscaled.is_empty() {
            notes.push(format!(
                "{} have no scale in the catalog, so their integer values are used as stored, not as reflectance: an index, or a colour mixing them with scaled bands, will be off",
                unscaled.join(", ")
            ));
        }
        let size = finest.transform.pixel_width.abs();
        let resampled: Vec<&str> = sources
            .iter()
            .zip(channels)
            .filter(|((r, _), _)| (r.transform.pixel_width.abs() - size).abs() > size * 0.01)
            .map(|(_, c)| c.as_str())
            .collect();
        if !resampled.is_empty() {
            notes.push(format!(
                "{} read on a coarser grid than the finest of the bands, and put on it by nearest cell",
                resampled.join(", ")
            ));
        }
        if left > 0 {
            let share = left as f64 * 100.0 / (w * h).max(1) as f64;
            notes.push(format!("{left} cells ({share:.1}%) have no value in at least one band (no data, or outside a band's window) and are no data in every band"));
        }
        let mut raster = rustgis_analysis::rasterops::derive(finest, name, bands)?;
        for (b, c) in raster.bands.iter_mut().zip(channels) {
            b.name = c.clone();
        }
        Ok((raster, notes))
    }

    /// The made raster, kept as a GeoTIFF and drawn as red, green and blue, with what to say of it
    /// once it is added (by its name on the map) and what the command answers besides.
    fn made_saying(raster: Raster, notes: Vec<String>, extra: Value) -> Result<Made, String> {
        // The encode is one step with no look inside it: Cancel is looked at before it.
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        let bytes = rustgis_data::raster::geotiff::write_all(&raster).map_err(|e| e.to_string())?;
        let name = raster.name.clone();
        let mut layer = RasterLayer::from_written(raster, format!("{name}.tif"), bytes);
        layer.style.renderer = RasterRenderer::Rgb { red: 0, green: 1, blue: 2 };
        let then = noted(name.clone(), notes);
        Ok(super::super::workflows::made_layer(name, layer, Some(extra), Some(then)))
    }

    /// What to say of a made layer once it is added: each note, by the layer's name on the map.
    fn noted(name: String, notes: Vec<String>) -> Then {
        Box::new(move |s: &mut Session, v: &mut Value| {
            let id = v.get("layer").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok());
            let added = id.and_then(|id| s.map().layer(id)).map_or(name, |l| l.name.clone());
            for note in notes {
                s.notify(format!("{added}: {note}"), false);
            }
        })
    }
}

#[expect(clippy::disallowed_methods, reason = "a thread the system refuses fails the test, as it should")]
#[cfg(all(test, not(target_arch = "wasm32")))]
#[expect(clippy::unwrap_used, reason = "a test module gated on more than `test`, which clippy's allow-unwrap-in-tests does not see")]
mod tests {
    use super::desktop::{auto_cell, index_look, note_kind, read_windows, stack};
    use crate::render::rustgis_tiles::stac::bands::Band;
    use rustgis_data::raster::{ColorHint, GeoTransform, Raster, crs::Crs};
    use serde_json::json;

    /// A raster of one band on a grid with these cells (`size` degrees), `w × h` from the corner.
    fn grid(w: u32, h: u32, size: f64, values: Vec<f32>) -> Raster {
        let t = GeoTransform { origin_x: 10.0, origin_y: 45.0, pixel_width: size, pixel_height: -size };
        Raster::new("b", w, h, vec![values], t, Crs::from_epsg(4326).unwrap(), Some(0.0), ColorHint::Gray, "UInt16").unwrap()
    }

    fn band(scale: f64, offset: f64) -> Band {
        Band {
            asset: "x".into(),
            href: "https://x.org/x.tif".into(),
            index: 1,
            in_file: 1,
            common: String::new(),
            scale,
            offset,
            nodata: None,
            scaled: true,
            unread: false,
            invalid: Vec::new(),
            codes_unread: false,
        }
    }

    #[test]
    fn a_combination_given_with_red_green_and_blue_is_refused_not_dropped() {
        let asset = |name: &str| {
            json!({"href": format!("https://example.org/{name}.tif"), "type": "image/tiff; application=geotiff; profile=cloud-optimized",
                "eo:bands": [{"common_name": name}]})
        };
        let item = json!({"type": "Feature", "id": "scene", "bbox": [10.0, 45.0, 10.02, 45.02], "properties": {"datetime": "2024-06-01T00:00:00Z"},
            "assets": {"red": asset("red"), "green": asset("green"), "blue": asset("blue"), "nir": asset("nir")}});
        let s = crate::Session::new(crate::Project::default());
        let refused = |p: serde_json::Value| super::bands_work(&s, &p).err();
        let both = refused(
            json!({"item": item, "combination": "colorInfrared", "red": "nir", "green": "red", "blue": "green", "bbox": [10.0, 45.0, 10.02, 45.02]}),
        );
        assert!(both.is_some_and(|e| e.contains("not both")), "the pair is refused");
        // Either alone is fine: the three bands, or a combination.
        assert!(
            refused(json!({"item": item, "red": "nir", "green": "red", "blue": "green", "bbox": [10.0, 45.0, 10.02, 45.02]})).is_none()
        );
        assert!(refused(json!({"item": item, "combination": "colorInfrared", "bbox": [10.0, 45.0, 10.02, 45.02]})).is_none());
        // A blank combination is not one.
        assert!(
            refused(
                json!({"item": item, "combination": " ", "red": "nir", "green": "red", "blue": "green", "bbox": [10.0, 45.0, 10.02, 45.02]})
            )
            .is_none()
        );
    }

    #[test]
    fn a_band_the_catalog_gave_unreadable_numbers_is_refused_and_an_unscaled_integer_file_is_said() {
        let a = grid(2, 2, 0.0001, vec![1000.0; 4]);
        let names: Vec<String> = vec!["nir".into(), "red".into()];
        // A scale given as text was not read: its band is refused, not drawn unscaled.
        let mut text = band(1.0, 0.0);
        (text.scaled, text.unread) = (false, true);
        let err = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &text)]).unwrap_err();
        assert!(err.starts_with("red:") && err.contains("not a number"), "{err}");
        // No scale on an integer file ("UInt16" in the grid helper): said once, naming the band and its type.
        let mut plain = band(1.0, 0.0);
        plain.scaled = false;
        let (_, notes) = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &plain)]).unwrap();
        assert!(notes.iter().any(|n| n.starts_with("red (UInt16) have no scale in the catalog")), "{notes:?}");
        // A stated scale says nothing, and neither does a float file without one.
        let (_, none) = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &band(1.0, 0.0))]).unwrap();
        assert!(!none.iter().any(|n| n.contains("no scale")), "{none:?}");
        let mut float = grid(2, 2, 0.0001, vec![0.5; 4]);
        float.data_type = "Float32".into();
        let (_, none) = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&float, &plain)]).unwrap();
        assert!(!none.iter().any(|n| n.contains("no scale")), "reflectance as stored needs no note: {none:?}");
    }

    #[test]
    fn an_index_is_the_indices_formula_over_the_stacked_bands() {
        use crate::render::rustgis_tiles::stac::bands::INDICES;
        // Near infrared 0.5 and red 0.1 (stored 5000 and 1000, scaled 0.0001): NDVI = 0.4 / 0.6.
        let nir = grid(2, 2, 0.0001, vec![5000.0; 4]);
        let red = grid(2, 2, 0.0001, vec![1000.0; 4]);
        let ix = INDICES.iter().find(|i| i.id == "ndvi").unwrap();
        let names: Vec<String> = ix.channels.iter().map(|c| (*c).to_string()).collect();
        assert_eq!(names, ["nir", "red"]);
        let (stacked, _) = stack("s", &names, &[(&nir, &band(0.0001, 0.0)), (&red, &band(0.0001, 0.0))]).unwrap();
        assert_eq!(stacked.bands.len(), 2, "as many bands as the index reads");
        let expr = crate::cmd::imagery::index_expression("ndvi", 1, 1, 2, 1, 1).unwrap();
        let v = rustgis_analysis::rasterops::band_math(&stacked, &expr).unwrap();
        assert!(v.iter().all(|x| (x - 0.6667).abs() < 1e-3), "{v:?}");
        // EVI reads three bands (nir, red, blue): 2.5 · (0.5 − 0.1) / (0.5 + 6 · 0.1 − 7.5 · 0.05 + 1).
        let blue = grid(2, 2, 0.0001, vec![500.0; 4]);
        let evi = INDICES.iter().find(|i| i.id == "evi").unwrap();
        let names: Vec<String> = evi.channels.iter().map(|c| (*c).to_string()).collect();
        assert_eq!(names, ["nir", "red", "blue"]);
        let (stacked, _) =
            stack("s", &names, &[(&nir, &band(0.0001, 0.0)), (&red, &band(0.0001, 0.0)), (&blue, &band(0.0001, 0.0))]).unwrap();
        let expr = crate::cmd::imagery::index_expression("evi", 3, 1, 2, 1, 1).unwrap();
        assert_eq!(expr, "2.5 * (b1 - b2) / (b1 + 6 * b2 - 7.5 * b3 + 1)");
        let v = rustgis_analysis::rasterops::band_math(&stacked, &expr).unwrap();
        assert!(v.iter().all(|x| (x - 0.5797).abs() < 1e-3), "EVI of 0.5, 0.1, 0.05: {v:?}");
        // One band is not a stack; an unknown index is refused.
        assert!(stack("s", &names[..1], &[(&nir, &band(1.0, 0.0))]).is_err());
        assert!(crate::cmd::imagery::index_expression("nope", 1, 1, 1, 1, 1).is_err());
    }

    fn names() -> [String; 3] {
        ["red".into(), "green".into(), "blue".into()]
    }

    #[test]
    fn three_bands_become_one_raster_on_the_finest_grid_with_their_scale() {
        // Red and green at 10 m cells (4 × 4); blue at twice the size (2 × 2).
        let red = grid(4, 4, 0.0001, vec![1000.0; 16]);
        let green = grid(4, 4, 0.0001, vec![2000.0; 16]);
        let blue = grid(2, 2, 0.0002, vec![3000.0; 4]);
        let (r, notes) =
            stack("s", &names(), &[(&red, &band(0.0001, 0.0)), (&green, &band(0.0001, 0.0)), (&blue, &band(0.0001, -0.1))]).unwrap();
        assert_eq!((r.width, r.height, r.bands.len()), (4, 4, 3));
        let values: Vec<f32> = r.bands.iter().map(|b| b.data[0]).collect();
        assert_eq!(values, [0.1, 0.2, 0.2], "reflectance: v · scale + offset");
        assert_eq!(r.bands.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["red", "green", "blue"]);
        assert!(notes.iter().any(|n| n.contains("blue") && n.contains("coarser grid")), "{notes:?}");
        assert!(!notes.iter().any(|n| n.contains("no data")), "every cell has all three: {notes:?}");
    }

    #[test]
    fn a_cell_missing_from_one_band_is_no_data_in_all_and_said() {
        let mut gone = vec![1000.0; 16];
        gone[5] = 0.0; // the file's no data
        let ok = grid(4, 4, 0.0001, vec![1000.0; 16]);
        let holed = grid(4, 4, 0.0001, gone);
        let (r, notes) = stack("s", &names(), &[(&ok, &band(1.0, 0.0)), (&holed, &band(1.0, 0.0)), (&ok, &band(1.0, 0.0))]).unwrap();
        let none = rustgis_analysis::rasterops::NODATA;
        assert!(r.bands.iter().all(|b| b.data[5] == none), "no data in all three");
        assert!(r.bands.iter().all(|b| b.data[4] == 1000.0), "the cells beside it are kept");
        assert!(notes.iter().any(|n| n.contains("1 cells") && n.contains("no value in at least one band")), "{notes:?}");
    }

    #[test]
    fn a_window_that_does_not_reach_a_cell_leaves_it_no_data() {
        let wide = grid(4, 4, 0.0001, vec![1000.0; 16]);
        // Covers only the top-left 2 × 2 of the finest grid.
        let small = grid(2, 2, 0.0001, vec![1000.0; 4]);
        let (r, notes) = stack("s", &names(), &[(&wide, &band(1.0, 0.0)), (&wide, &band(1.0, 0.0)), (&small, &band(1.0, 0.0))]).unwrap();
        let none = rustgis_analysis::rasterops::NODATA;
        assert_eq!(r.bands[0].data.iter().filter(|v| **v == none).count(), 12);
        assert!(notes.iter().any(|n| n.contains("12 cells")), "{notes:?}");
    }

    #[test]
    fn every_index_is_drawn_in_colour_over_a_range_of_its_own() {
        use crate::render::rustgis_tiles::stac::bands::INDICES;
        use rustgis_map::Ramp;
        for ix in &INDICES {
            let (ramp, min, max) = index_look(ix.id);
            assert!(min < max && min.is_finite() && max.is_finite(), "{}: {min}..{max}", ix.id);
            assert_ne!(ramp, Ramp::Greys, "{} is not drawn in grey", ix.id);
        }
        // Vegetation in greens, water in blues.
        assert_eq!((index_look("ndvi").0, index_look("ndwi").0), (Ramp::Forest, Ramp::Lagoon));
    }

    #[test]
    fn bands_with_no_cell_in_common_are_an_error_not_an_empty_image() {
        // The first band has a value in the top-left cell only, the second in the bottom-right
        // cell only (0 is the files' no data): no cell has a value in both.
        let wide = grid(4, 4, 0.0001, vec![1000.0; 16]);
        let mut values = vec![0.0; 16];
        values[15] = 1000.0;
        let corner = grid(4, 4, 0.0001, values);
        let mut top = vec![0.0; 16];
        top[0] = 1000.0;
        let first = grid(4, 4, 0.0001, top);
        let err = stack("s", &["a".into(), "b".into()], &[(&first, &band(1.0, 0.0)), (&corner, &band(1.0, 0.0))]).unwrap_err();
        assert!(err.contains("no cell has a value in every band") && err.contains("a, b"), "{err}");
        // One cell in common is a result, said as before.
        let (r, notes) = stack("s", &["a".into(), "b".into()], &[(&wide, &band(1.0, 0.0)), (&corner, &band(1.0, 0.0))]).unwrap();
        assert_eq!(r.bands[0].data.iter().filter(|v| **v != rustgis_analysis::rasterops::NODATA).count(), 1);
        assert!(notes.iter().any(|n| n.contains("15 cells")), "{notes:?}");
    }

    #[test]
    fn a_band_taken_from_a_colour_image_is_said_and_a_band_file_is_not() {
        let a = grid(2, 2, 0.0001, vec![1000.0; 4]);
        let names: Vec<String> = vec!["nir".into(), "red".into()];
        let colour = |index: u32| {
            let mut b = band(1.0, 0.0);
            (b.asset, b.href, b.in_file, b.index) = ("visual".into(), "https://x.org/visual.tif".into(), 3, index);
            b
        };
        let (_, notes) = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &colour(1))]).unwrap();
        assert!(notes.iter().any(|n| n.starts_with("red (band 1 of visual) is taken from a file of several bands")), "{notes:?}");
        let (_, none) = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &band(1.0, 0.0))]).unwrap();
        assert!(!none.iter().any(|n| n.contains("several bands")), "files of their own say nothing: {none:?}");
        // Two bands from colour images are one notice naming both; all from one image (the units agree) say nothing.
        let three: Vec<String> = vec!["nir".into(), "red".into(), "green".into()];
        let (_, notes) = stack("s", &three, &[(&a, &band(1.0, 0.0)), (&a, &colour(1)), (&a, &colour(1))]).unwrap();
        let said: Vec<&String> = notes.iter().filter(|n| n.contains("several bands")).collect();
        assert!(said.len() == 1 && said[0].contains("red (band 1 of visual), green (band 1 of visual) are taken"), "{notes:?}");
        let (_, same) = stack("s", &names, &[(&a, &colour(1)), (&a, &colour(1))]).unwrap();
        assert!(!same.iter().any(|n| n.contains("several bands")), "one image: {same:?}");
    }

    #[test]
    fn a_parameter_of_the_wrong_type_is_refused_not_taken_as_missing() {
        let asset = |name: &str| {
            json!({"href": format!("https://example.org/{name}.tif"), "type": "image/tiff; application=geotiff; profile=cloud-optimized",
                "eo:bands": [{"common_name": name}]})
        };
        let item = json!({"type": "Feature", "id": "scene", "bbox": [10.0, 45.0, 10.02, 45.02], "properties": {"datetime": "2024-06-01T00:00:00Z"},
            "assets": {"red": asset("red"), "green": asset("green"), "blue": asset("blue"), "nir": asset("nir")}});
        let s = crate::Session::new(crate::Project::default());
        let bbox = json!([10.0, 45.0, 10.02, 45.02]);
        let err = |extra: serde_json::Value| {
            let mut p = json!({"item": item, "bbox": bbox});
            for (k, v) in extra.as_object().unwrap() {
                p[k] = v.clone();
            }
            super::bands_work(&s, &p).err()
        };
        assert!(err(json!({"combination": 5})).is_some_and(|e| e.contains("combination is text")));
        assert!(err(json!({"red": 4})).is_some_and(|e| e.contains("red is text")));
        assert!(err(json!({"name": 5})).is_some_and(|e| e.contains("name is text")));
        assert!(err(json!({"combination": null, "name": ""})).is_none(), "null and blank are missing");
    }

    #[test]
    fn a_band_with_a_scale_of_zero_is_refused_not_drawn_as_one_value() {
        let a = grid(2, 2, 0.0001, vec![1000.0; 4]);
        let names: Vec<String> = vec!["nir".into(), "red".into()];
        let err = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &band(0.0, 0.5))]).unwrap_err();
        assert!(err.starts_with("red:") && err.contains("scale of 0"), "{err}");
        let err = stack("s", &names, &[(&a, &band(f64::NAN, 0.0)), (&a, &band(1.0, 0.0))]).unwrap_err();
        assert!(err.starts_with("nir:") && err.contains("scale"), "{err}");
        // A negative scale is odd but is a scale.
        assert!(stack("s", &names, &[(&a, &band(-1.0, 0.0)), (&a, &band(1.0, 0.0))]).is_ok());
    }

    #[test]
    fn bands_in_different_coordinate_systems_are_refused() {
        let a = grid(2, 2, 0.0001, vec![1.0; 4]);
        let mut b = grid(2, 2, 0.0001, vec![1.0; 4]);
        b.crs = Crs::from_epsg(3857).unwrap();
        let err = stack("s", &names(), &[(&a, &band(1.0, 0.0)), (&b, &band(1.0, 0.0)), (&a, &band(1.0, 0.0))]).unwrap_err();
        assert!(err.contains("different coordinate systems"), "{err}");
    }

    #[test]
    fn the_reader_threads_keep_the_runs_request_predicate() {
        // A claim that has gone: the reads stop before any request is sent, on the reader threads too.
        let gone: rustgis_data::source::remote::Still = std::sync::Arc::new(|| false);
        let files = vec![("https://127.0.0.1:9/a.tif".to_string(), 1), ("https://127.0.0.1:9/b.tif".to_string(), 1)];
        let err = rustgis_data::source::remote::with_still(gone, || read_windows(&files, [10.0, 45.0, 10.1, 45.1], (10.0, false))).err();
        assert!(err.is_some_and(|e| e.contains("cancelled before it was sent")), "the predicate reached the reads");
    }

    #[test]
    fn the_automatic_cell_size_never_says_the_full_image_is_finer_but_the_users_does() {
        use crate::cmd::layer::finer_than_file;
        // A 10 m file read for a box small enough that the automatic cell is about 5 m: no level fits.
        let auto = auto_cell([10.0, 45.0, 10.1, 45.1]);
        assert!((4.0..10.0).contains(&auto), "finer than the file: {auto}");
        assert_eq!(finer_than_file(Some(auto), false, true), None, "the automatic size says nothing");
        assert_eq!(finer_than_file(Some(auto), false, false), Some(auto), "the same size, the user's, is said");
        assert_eq!(finer_than_file(Some(5.0), false, false), Some(5.0), "a size the user gave, finer than the file, is said");
        assert_eq!(finer_than_file(Some(50.0), true, false), None, "a level that fits says nothing");
    }

    #[test]
    fn failed_retrieval_codes_are_not_values_and_codes_unread_are_refused() {
        let a = grid(2, 1, 0.0001, vec![1000.0, 65535.0]);
        let names: Vec<String> = vec!["nir".into(), "red".into()];
        let mut coded = band(1.0, 0.0);
        coded.invalid = vec![65535.0];
        let (r, _) = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &coded)]).unwrap();
        assert_eq!(r.bands[1].data.iter().filter(|v| **v != rustgis_analysis::rasterops::NODATA).count(), 1, "the code is no data");
        let mut unread = band(1.0, 0.0);
        unread.codes_unread = true;
        let err = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &unread)]).unwrap_err();
        assert!(err.starts_with("red:") && err.contains("failed retrieval"), "{err}");
    }

    #[test]
    fn a_parameter_of_the_other_command_is_refused_not_dropped() {
        let asset = |name: &str| {
            json!({"href": format!("https://example.org/{name}.tif"), "type": "image/tiff; application=geotiff; profile=cloud-optimized",
                "eo:bands": [{"common_name": name}]})
        };
        let item = json!({"type": "Feature", "id": "scene", "bbox": [10.0, 45.0, 10.02, 45.02], "properties": {"datetime": "2024-06-01T00:00:00Z"},
            "assets": {"red": asset("red"), "green": asset("green"), "blue": asset("blue"), "nir": asset("nir")}});
        let s = crate::Session::new(crate::Project::default());
        let bbox = json!([10.0, 45.0, 10.02, 45.02]);
        let with = |base: serde_json::Value, k: &str, v: serde_json::Value| {
            let mut p = base;
            p["item"] = item.clone();
            p["bbox"] = bbox.clone();
            p[k] = v;
            p
        };
        assert!(
            super::bands_work(&s, &with(json!({}), "index", json!("ndvi"))).err().is_some_and(|e| e.contains("index is for stac.addIndex"))
        );
        for k in ["combination", "red", "green", "blue"] {
            let err = super::index_work(&s, &with(json!({"index": "ndvi"}), k, json!("red"))).err();
            assert!(err.is_some_and(|e| e.contains(&format!("{k} is for stac.addBands"))), "{k}");
        }
        // Blank and null are not given; each command alone is fine.
        assert!(super::bands_work(&s, &with(json!({}), "index", json!(" "))).is_ok());
        assert!(super::index_work(&s, &with(json!({"index": "ndvi"}), "combination", json!(null))).is_ok());
    }

    #[test]
    fn a_failed_retrieval_code_that_f32_rounds_is_still_masked() {
        // 4294967295 is not exactly an f32: the sample reads 4294967296.
        let a = grid(2, 1, 0.0001, vec![1000.0, 4_294_967_295.0_f32]);
        let names: Vec<String> = vec!["nir".into(), "red".into()];
        let mut coded = band(1.0, 0.0);
        coded.invalid = vec![4_294_967_295.0];
        let (r, _) = stack("s", &names, &[(&a, &band(1.0, 0.0)), (&a, &coded)]).unwrap();
        assert_eq!(r.bands[1].data.iter().filter(|v| **v != rustgis_analysis::rasterops::NODATA).count(), 1, "the rounded code is no data");
    }

    #[test]
    fn a_band_the_catalog_gives_unusable_numbers_is_refused_before_any_file_is_read() {
        let asset = |name: &str, extra: serde_json::Value| {
            let mut band = json!({"common_name": name});
            band.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            json!({"href": format!("https://example.org/{name}.tif"), "type": "image/tiff; application=geotiff; profile=cloud-optimized",
                "eo:bands": [band]})
        };
        let item = |red: serde_json::Value| {
            json!({"type": "Feature", "id": "scene", "bbox": [10.0, 45.0, 10.02, 45.02], "properties": {"datetime": "2024-06-01T00:00:00Z"},
                "assets": {"red": asset("red", red), "green": asset("green", json!({})), "blue": asset("blue", json!({})), "nir": asset("nir", json!({}))}})
        };
        let s = crate::Session::new(crate::Project::default());
        let bbox = json!([10.0, 45.0, 10.02, 45.02]);
        // At the plan, not once the files are read: the work isn't made, the error is.
        for (red, said) in [(json!({"raster:scale": 0}), "scale of 0"), (json!({"raster:scale": "0.0001"}), "not a number")] {
            let err = super::bands_work(&s, &json!({"item": item(red.clone()), "bbox": bbox})).err();
            assert!(err.is_some_and(|e| e.starts_with("red:") && e.contains(said)), "bands: {red}");
            let err = super::index_work(&s, &json!({"item": item(red.clone()), "index": "ndvi", "bbox": bbox})).err();
            assert!(err.is_some_and(|e| e.starts_with("red:") && e.contains(said)), "index: {red}");
        }
        assert!(super::bands_work(&s, &json!({"item": item(json!({"raster:scale": 0.0001})), "bbox": bbox})).is_ok());
    }

    #[test]
    fn a_users_band_may_be_an_exact_asset_key_and_a_presets_goes_by_name() {
        let asset = |common: &str| {
            json!({"href": format!("https://example.org/{common}.tif"), "type": "image/tiff; application=geotiff; profile=cloud-optimized",
                "eo:bands": [{"common_name": common}]})
        };
        // B08 and B8A are both near infrared.
        let item = json!({"type": "Feature", "id": "scene", "bbox": [10.0, 45.0, 10.02, 45.02], "properties": {"datetime": "2024-06-01T00:00:00Z"},
            "assets": {"B08": asset("nir"), "B8A": asset("nir"), "red": asset("red"), "green": asset("green")}});
        let s = crate::Session::new(crate::Project::default());
        let bbox = json!([10.0, 45.0, 10.02, 45.02]);
        let run = |extra: serde_json::Value| {
            let mut p = json!({"item": item, "bbox": bbox});
            for (k, v) in extra.as_object().unwrap() {
                p[k] = v.clone();
            }
            super::bands_work(&s, &p)
        };
        assert!(run(json!({"red": "B8A", "green": "red", "blue": "green"})).is_ok(), "the key picks its file");
        assert!(run(json!({"red": "B08", "green": "red", "blue": "green"})).is_ok());
        assert!(run(json!({"red": "nir", "green": "red", "blue": "green"})).is_ok(), "a name still goes by name");
        assert!(run(json!({"red": "nope", "green": "red", "blue": "green"})).is_err());
    }

    #[test]
    fn grids_stored_in_other_turns_of_longitude_are_the_same_place() {
        // Two windows of the same place: one stored at -10°..-9°, the other at 350°..351°.
        let at = |origin_x: f64, values: Vec<f32>| {
            let t = GeoTransform { origin_x, origin_y: 45.0, pixel_width: 0.5, pixel_height: -0.5 };
            Raster::new("b", 2, 2, vec![values], t, Crs::from_epsg(4326).unwrap(), Some(0.0), ColorHint::Gray, "UInt16").unwrap()
        };
        let west = at(-10.0, vec![1000.0; 4]);
        let east = at(350.0, vec![2000.0; 4]);
        let names: Vec<String> = vec!["nir".into(), "red".into()];
        let (r, notes) = stack("s", &names, &[(&west, &band(1.0, 0.0)), (&east, &band(1.0, 0.0))]).unwrap();
        let none = rustgis_analysis::rasterops::NODATA;
        assert!(r.bands.iter().all(|b| b.data.iter().all(|v| *v != none)), "every cell has both bands: {notes:?}");
        assert_eq!((r.bands[0].data[0], r.bands[1].data[0]), (1000.0, 2000.0));
    }

    #[test]
    fn every_file_read_is_held_to_public_addresses() {
        use crate::render::rustgis_tiles::{address::Reach, cache};
        // A claim that has gone, so no request is sent; the files are marked before the readers start.
        let gone: rustgis_data::source::remote::Still = std::sync::Arc::new(|| false);
        let url = "https://rustgis-public-only.example/b.tif";
        assert_eq!(cache::ceiling_for(url), Reach::Local, "unmarked, a URL's ceiling is local");
        let _ = rustgis_data::source::remote::with_still(gone, || {
            read_windows(&[(url.to_string(), 1)], [10.0, 45.0, 10.1, 45.1], (10.0, false))
        });
        assert_eq!(cache::ceiling_for(url), Reach::Public, "the read of it connects to public addresses only");
    }

    #[test]
    fn a_malformed_band_list_refuses_every_band_and_a_file_above_the_cap_keeps_its_count() {
        use crate::render::rustgis_tiles::stac::{self, bands};
        let asset = |extra: serde_json::Value| {
            let mut a = json!({"href": "https://example.org/s.tif", "type": "image/tiff; application=geotiff",
                "eo:bands": [{"common_name": "blue"}, {"common_name": "green"}, {"common_name": "red"}, {"common_name": "nir"}]});
            a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            json!({"type": "Feature", "id": "s", "bbox": [0.0, 0.0, 1.0, 1.0], "properties": {"datetime": "2024-06-01T00:00:00Z"}, "assets": {"s": a}})
        };
        let get = |extra: serde_json::Value| {
            let it = stac::item_from_json(&asset(extra), None).unwrap();
            let again = stac::item_from_json(&stac::item_json(&it), None).unwrap();
            (bands::bands(&it), bands::bands(&again))
        };
        // The later bands (nir, red) are refused too, through the command's copy of the item.
        let (a, b) = get(json!({"raster:bands": {"scale": 0.0001}}));
        assert!(a.iter().chain(&b).all(|x| x.unread), "every band, not the first only");
        let (a, _) = get(json!({}));
        assert!(a.iter().all(|x| !x.unread));
        // A file of 65 bands: the count read is the file's, though 64 are kept.
        let many: Vec<serde_json::Value> = (0..65).map(|_| json!({})).collect();
        let (a, b) = get(json!({"eo:bands": many}));
        assert_eq!((a.len(), a[0].in_file, b[0].in_file), (64, 65, 65));
    }

    #[test]
    fn a_reader_that_panics_stops_the_run_like_one_that_fails() {
        use super::desktop::PanicStop;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicBool, Ordering};
        let (stop, cause) = (AtomicBool::new(false), Mutex::new(None));
        // A reader that panics (the guard dropped while unwinding, still armed) stops the others.
        let panicked = std::thread::scope(|s| {
            s.spawn(|| {
                let _guard = PanicStop { stop: &stop, cause: &cause, message: "a.tif: the read failed unexpectedly".into(), armed: true };
                panic!("a reader panics");
            })
            .join()
        });
        assert!(panicked.is_err());
        assert!(stop.load(Ordering::Relaxed));
        assert_eq!(cause.lock().unwrap().as_deref(), Some("a.tif: the read failed unexpectedly"));
        // One that returned (disarmed) leaves the run alone.
        let (stop, cause) = (AtomicBool::new(false), Mutex::new(None));
        drop(PanicStop { stop: &stop, cause: &cause, message: "x".into(), armed: false });
        assert!(!stop.load(Ordering::Relaxed) && cause.lock().unwrap().is_none());
    }

    #[test]
    fn the_failure_said_is_the_one_that_stopped_the_others_not_their_cancellations() {
        use super::desktop::note_failure;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicBool, Ordering};
        // The first failure stops the others and is the one kept; a later one (a sibling's, whatever
        // its wording: even one that says "cancel") is not.
        let (stop, cause) = (AtomicBool::new(false), Mutex::new(None));
        note_failure(&stop, &cause, "b.tif: the server cancelled the request: 404");
        note_failure(&stop, &cause, "a.tif: cancelled");
        assert!(stop.load(Ordering::Relaxed));
        assert_eq!(cause.lock().unwrap().as_deref(), Some("b.tif: the server cancelled the request: 404"));
        // Already stopped by the user's Cancel: what the reads then answer is not a cause.
        let (stop, cause) = (AtomicBool::new(true), Mutex::new(None));
        note_failure(&stop, &cause, "a.tif: cancelled");
        assert_eq!(*cause.lock().unwrap(), None);
    }

    #[test]
    fn a_band_read_with_rounded_values_is_said_and_a_clean_one_is_not() {
        let names: Vec<String> = vec!["nir".into(), "red".into()];
        let clean = grid(2, 2, 0.0001, vec![1000.0; 4]);
        let mut lossy = grid(2, 2, 0.0001, vec![1000.0; 4]);
        lossy.rounded = vec![3];
        let (_, notes) = stack("s", &names, &[(&clean, &band(1.0, 0.0)), (&lossy, &band(1.0, 0.0))]).unwrap();
        let said: Vec<&String> = notes.iter().filter(|n| n.contains("rounded")).collect();
        assert!(said.len() == 1 && said[0].starts_with("red read with values rounded"), "{notes:?}");
        let (_, none) = stack("s", &names, &[(&clean, &band(1.0, 0.0)), (&clean, &band(1.0, 0.0))]).unwrap();
        assert!(!none.iter().any(|n| n.contains("rounded")), "{none:?}");
    }

    #[test]
    fn the_notes_of_the_files_are_one_notice_for_a_kind_counting_each_file_once() {
        use super::desktop::merge_notes;
        let edges = |e: &str| format!("the box reaches past the file's {e} edge(s): the window is cut to the file");
        let pieces =
            "3 piece(s) of the box's edges not followed to a quarter of a pixel: the window may fall short of a bent edge".to_string();
        let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Two files whose edge notes name different edges: one notice, two files, both wordings kept.
        let two = merge_notes(&[(w(&["red"]), vec![edges("north")]), (w(&["green", "blue"]), vec![edges("south")])]);
        assert_eq!(two.len(), 1, "{two:?}");
        assert!(two[0].starts_with("red, green, blue (2 files): ") && two[0].contains("north") && two[0].contains("south"), "{two:?}");
        // A file with two notes of one kind counts once, its bands listed once.
        let once = merge_notes(&[(w(&["red"]), vec![edges("north"), edges("east")])]);
        assert_eq!(once.len(), 1);
        assert!(once[0].starts_with("red (1 file): "), "{once:?}");
        // Different kinds stay apart; a clean run says nothing.
        assert_eq!(merge_notes(&[(w(&["red"]), vec![edges("north"), pieces])]).len(), 2);
        assert!(merge_notes(&[(w(&["red"]), Vec::new()), (w(&["green"]), Vec::new())]).is_empty());
    }

    #[test]
    fn window_notes_of_one_kind_match_whatever_their_numbers_and_edges() {
        let edges = |e: &str| format!("the box reaches past the file's {e} edge(s): the window is cut to the file");
        assert_eq!(note_kind(&edges("north, east")), note_kind(&edges("south")));
        let pieces = |n: u32| {
            format!("{n} piece(s) of the box's edges not followed to a quarter of a pixel: the window may fall short of a bent edge")
        };
        assert_eq!(note_kind(&pieces(3)), note_kind(&pieces(12)));
        let cell = |c: f64| format!("cellSize 5 is finer than the file's finest cells ({c:.1}m): the full image is read");
        assert_eq!(note_kind(&cell(10.0)), note_kind(&cell(20.5)));
        // Different kinds stay apart.
        assert_ne!(note_kind(&edges("north")), note_kind(&pieces(3)));
    }

    #[test]
    fn the_cell_size_read_follows_the_box() {
        // A 10 km box is read at about 5 m cells (the full image of a 10 m scene); a 2° box coarser.
        let small = auto_cell([10.0, 45.0, 10.09, 45.09]);
        let large = auto_cell([10.0, 45.0, 12.0, 47.0]);
        assert!((4.0..6.0).contains(&small), "{small}");
        assert!(large > 100.0, "{large}");
        // A degenerate box doesn't give a zero cell.
        assert!(auto_cell([10.0, 45.0, 10.0, 45.0]) > 0.0);
    }
}
