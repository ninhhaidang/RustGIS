//! Field packages (#1902): a map taken to a phone and brought back.
//!
//! A field package (`.rgisfield`) is a zip holding a project of one map, `<area> (offline)`, and
//! the files it reads beside it: the MBTiles and PMTiles archives its tile layers draw from, under
//! `basemaps/`, so the map has its basemap with no network. Packaging the active map makes an
//! offline area of it in this project first (as Download Map does), so the edits made in the
//! field can be brought back to it by feature, with the offline area's conflict handling.
//!
//! Opening a field package unpacks it into the app's own folder (`field/<name>`) and opens the
//! project there, unsaved, as a project package opens.
//!
//! Coming back, the field's map is packaged as it is, with the photos its features name (in a
//! layer's `photos` field, paths separated by `;`) under `photos/`, and the list of what the phone
//! changed in the rows it was given ([`super::field_changes`]). Bring Back Edits
//! (`offline.bringBack`) reads such a package in the project it was made from, puts its photos in
//! a folder beside that project, and applies the list to the area's map, saying which features were
//! changed on both sides. A package without a list (made or opened where none could be kept) is
//! compared with the area as last synced, as Sync does.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;
use crate::project::Project;

/// A field package's extension.
pub const EXTENSION: &str = "rgisfield";
/// The folder of a field package that holds its tile archives.
const BASEMAPS: &str = "basemaps";
/// The folder of a field package that holds its features' photos.
const PHOTOS: &str = "photos";
/// What an offline area's map is called after: `<area> (offline)`.
pub const OFFLINE: &str = " (offline)";
/// The most bytes read from one field package's entries when it is opened.
const MOST_UNPACKED: usize = rustgis_data::zip::MAX_TOTAL;
/// Optional information for the crew. Older readers extract it like any other companion file.
const LEFT_BEHIND: &str = "left-behind.v1.json";
const MAX_MANIFEST: usize = 256 << 10;
const MAX_NOTICES: usize = 16;

/// Whether a file name or path is a field package's, ignoring case.
pub fn is_field_package(name: &str) -> bool {
    name.to_ascii_lowercase().strip_suffix(EXTENSION).is_some_and(|s| s.ends_with('.'))
}

/// Default name for the save picker before the desktop export starts.
#[cfg(not(target_arch = "wasm32"))]
pub fn suggested_name(s: &Session, p: &Value) -> String {
    let name = s.map().name.as_str();
    if s.project.field_origin.is_some() || s.opened_package.as_deref().is_some_and(|p| is_field_package(&p.to_string_lossy())) {
        return name.strip_suffix(OFFLINE).unwrap_or(name).to_string();
    }
    if let Some(area) = name.strip_suffix(OFFLINE)
        && s.project.offline_areas.iter().any(|saved| saved.name.eq_ignore_ascii_case(area))
    {
        return area.to_string();
    }
    super::ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map_or_else(|| format!("{name} offline"), str::to_string)
}

/// A field package of the active map, as (name, bytes).
///
/// The map of a home offline area or of an opened field package goes as it is, so its edits
/// can come back. Any other map is first made an offline area of this project (`extent`
/// and `name` as for `offline.download`, the view by default), whose map is packaged.
///
/// Never silent: the raster layers an offline area leaves behind, the tile layers that stay
/// online (a web address, not an archive), and an archive that couldn't be read are said.
#[cfg(test)]
pub(crate) fn package(s: &mut Session, p: &Value) -> Result<(String, Vec<u8>), String> {
    let made = package_output(s, p);
    // As the command's end keeps the floors of the ids packages hold.
    s.keep_field_floors();
    let (name, bytes, notes) = made?;
    for note in notes {
        s.say(note);
    }
    Ok((name, bytes))
}

/// Construction notes describe the delivered package, so callers emit them after delivery.
pub(crate) fn package_output(s: &mut Session, p: &Value) -> Result<(String, Vec<u8>, rustgis_data::loss::Notes), String> {
    let mut notes = rustgis_data::loss::Notes::new();
    let (name, bytes) = build_package(s, p, &mut notes)?;
    Ok((name, bytes, notes))
}

fn field_edit(s: &mut Session, label: &'static str, change: impl FnOnce(&mut Session) -> crate::CmdResult) -> crate::CmdResult {
    if s.restorable > 0 { change(s) } else { s.guarded(label, true, change) }
}

fn legacy_user(s: &Session) -> Result<(), String> {
    if cfg!(target_arch = "wasm32") || !s.policy_user() {
        Err("legacy field recovery requires explicit native User dispatch".into())
    } else {
        Ok(())
    }
}

/// Builds the field package of the active map: its name and bytes. Going out, from the project the
/// edits will come back to, it is of an offline area (made here when the map isn't one's) and
/// carries the area's identities and return check, assigned here the first time and recorded as
/// one undoable change. Coming back, from a field project, it is that project's own map with the
/// photos its features name, keeping the origin it was made with. What it leaves behind is added to
/// `notes`, said once the package has been delivered. Cancel is checked between streamed
/// chunks, tile archives and the encoding steps. A single archive read or ZIP compression
/// runs through before the next check.
fn build_package(s: &mut Session, p: &Value, notes: &mut rustgis_data::loss::Notes) -> Result<(String, Vec<u8>), String> {
    use rustgis_data::progress;
    progress::check().map_err(|e| e.to_string())?;
    let mut losses = rustgis_data::loss::Notes::new();
    let mut archive_errors = Vec::new();
    // Each is notified as an error as soon as it is met, and a copy of its text is kept for the
    // recipient's manifest.
    let mut archive_error = |s: &mut Session, text: String| {
        s.notify(text.clone(), true);
        archive_errors.push(text);
    };
    let legacy_return = if p.get("legacyReturn").is_some() {
        legacy_user(s)?;
        super::b(p, "legacyReturn").ok_or("legacyReturn must be true or false")?
    } else {
        false
    };
    if legacy_return && (s.project.field_origin.is_some() || !s.project.origin_id.is_empty()) {
        return Err("legacyReturn cannot override a present field or home identity".into());
    }

    let opened_field = legacy_return
        || s.project.field_origin.is_some()
        || s.opened_package.as_deref().is_some_and(|p| is_field_package(&p.to_string_lossy()));
    if opened_field && s.project.maps.len() != 1 {
        return Err("a returning field project must hold only its original field map: remove added maps before sending it back".into());
    }
    let home_area = if opened_field { None } else { super::workspace::map_area_index(&s.project, s.project.active_map) };
    let returning = if opened_field {
        Some(s.map().name.strip_suffix(OFFLINE).unwrap_or(&s.map().name).to_string())
    } else {
        home_area.and_then(|i| s.project.offline_areas.get(i)).map(|area| area.name.clone())
    };
    // Only a map coming back from the field carries its photos. Its persisted provenance
    // survives an ordinary Save As; going out, the photos named are at home.
    let coming_back = returning.is_some() && opened_field;
    // Going out from an area's offline map with edits not synced yet, a feature sketched there and
    // one added to the area's map since can share an id, so the crew's edits would come home to the
    // wrong one: refused, as Bring Back refuses it, with nothing changed.
    if let Some(ai) = home_area.filter(|_| returning.is_some())
        && super::workspace::area_unsynced(&s.project, ai)
    {
        let area = s.project.offline_areas.get(ai).map(|a| a.name.clone()).unwrap_or_default();
        return Err(format!(
            "offline area '{area}' has edits in its offline map that aren't synced yet, and a package made from it could give the field ids the map uses for other features. Nothing was changed. Sync the area first (Data › Offline › Sync), then make the package again"
        ));
    }
    let (area, mut map) = match returning {
        Some(area) => {
            // Its raster layers stay behind, as an offline area's do going out: said.
            let mut m = s.map().clone();
            let rasters: Vec<u32> = m.layers.iter().filter(|l| l.is_raster()).map(|l| l.id).collect();
            if !rasters.is_empty() {
                m.release_tiles(&rasters);
                m.layers.retain(|l| !l.is_raster());
                losses.push(format!("{} raster layer(s) were left out of the field package: it holds feature layers", rasters.len()));
            }
            (area, m)
        }
        None => {
            let rasters = s.map().layers.iter().filter(|l| l.is_raster()).count();
            let features = |m: &rustgis_map::Map| {
                m.layers.iter().filter(|l| !l.is_raster() && !l.is_streamed()).map(|l| l.dataset.features.len()).sum::<usize>()
            };
            let had = features(s.map());
            // The new area's first package is of the state it starts in: made with it, as one undo step.
            let made = field_edit(s, "Create field offline area", |s| {
                let made = super::workspace::offline_download(s, p)?;
                let generation = crate::project::field_id()?;
                let name = made.get("name").and_then(Value::as_str);
                if let Some(area) = name.and_then(|n| s.project.offline_areas.iter_mut().find(|a| a.name == n)) {
                    area.generation = generation;
                }
                Ok(made)
            })?;
            let name = made.get("name").and_then(Value::as_str).ok_or("the offline area wasn't made")?.to_string();
            let data =
                s.project.offline_areas.iter().find(|a| a.name == name).map(|a| a.data.clone()).ok_or("the offline area wasn't made")?;
            if rasters > 0 {
                losses.push(format!("{rasters} raster layer(s) were left out of the field package: an offline area holds feature layers"));
            }
            notes.push(format!(
                    "Made the offline area '{name}' of this map for the field package: save this project to bring the field's edits back to it later"
                ));
            // The area holds the features its extent (the view, unless one is given) touches.
            let out = had.saturating_sub(features(&data));
            if out > 0 {
                losses.push(format!(
                    "{out} feature(s) outside the offline area's extent, or without a shape, stayed out of the field package"
                ));
            }
            (name, data)
        }
    };
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut budget = most_unpacked() / 2;
    // Each local archive goes in beside the project, its layer reading it from there.
    let (mut online, mut unread, mut uploaded) = (Vec::new(), Vec::new(), Vec::new());
    let mut taken = std::collections::HashSet::new();
    progress::begin(map.tiles.len());
    for t in &mut map.tiles {
        progress::step(1).map_err(|e| e.to_string())?;
        let Some(file) = t.archive_file().map(str::to_string) else {
            // An archive added to the web app lives in its memory, not in a file to put in.
            if t.url.starts_with(rustgis_map::tiles::UPLOAD) {
                uploaded.push(t.name.clone());
            } else {
                online.push(t.name.clone());
            }
            continue;
        };
        match crate::project::read_file(Path::new(&file), budget) {
            Ok(bytes) => {
                budget = budget.saturating_sub(bytes.len() as u64);
                // A name every system's folders can hold as one file, the same in the entry and
                // the layer's address: letters, digits, `-`, `_`, `.` and spaces, anything else `_`
                // (`:`, `\`, `*`, `?`… are fine in a Linux name, refused or split elsewhere).
                let stem: String = Path::new(&file).file_name().map_or_else(
                    || "basemap".into(),
                    |n| {
                        n.to_string_lossy()
                            .chars()
                            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ') { c } else { '_' })
                            .collect()
                    },
                );
                let mut name = stem.clone();
                for n in 2.. {
                    if taken.insert(name.to_ascii_lowercase()) {
                        break;
                    }
                    name = format!("{n}-{stem}");
                }
                t.url = format!("{BASEMAPS}/{name}");
                entries.push((t.url.clone(), bytes));
            }
            Err(e) => unread.push(format!("{} ({e})", t.name)),
        }
    }
    if !online.is_empty() {
        losses.push(format!(
                "{} tile layer(s) in the field package draw from the web and show only online: {}. Add an MBTiles or PMTiles archive for a basemap with no network",
                online.len(),
                online.join(", ")
            ));
    }
    if !uploaded.is_empty() {
        archive_error(
            s,
            format!(
                "{} tile archive(s) added to this browser couldn't be put in the field package, as they live only in its memory: {}. Add them to the field from the file",
                uploaded.len(),
                uploaded.join(", ")
            ),
        );
    }
    if !unread.is_empty() {
        archive_error(s, format!("{} tile archive(s) couldn't be put in the field package: {}", unread.len(), unread.join(", ")));
    }
    // The photos the features name go with them: going out, those named here, the features
    // naming them as before (the phone points them at the unpacked files); coming back, those
    // taken in the field (and those that came from home, under their paths there).
    let photos = if coming_back {
        // Room for the package's own entries, as going out.
        let most_entries = usize::from(u16::MAX).saturating_sub(entries.len() + 16);
        take_photos(&mut map, &mut budget, s.project.field_origin.as_ref().map_or("", |o| o.package.as_str()), most_entries)?
    } else {
        // Room for the package's own entries (its project, README, notes, marker and index).
        let most_entries = usize::from(u16::MAX).saturating_sub(entries.len() + 16);
        collect_photos(&map, &mut budget, most_entries)?
    };
    // Coming back, a photo too large for the package was always an error; going out, it stays
    // home with the others left out.
    let mut unread = photos.unread;
    if coming_back {
        unread.extend(photos.too_large.iter().map(|path| format!("{path} (too large for the field package)")));
    }
    if !unread.is_empty() {
        archive_error(s, format!("{} photo(s) couldn't be put in the field package: {}", unread.len(), some_of(&unread)));
    }
    if !photos.not_pictures.is_empty() {
        let named = &photos.not_pictures;
        losses.push(format!(
            "{} file(s) a photos field names weren't put in the field package: only pictures go ({}): {}",
            named.len(),
            PICTURES.join(", "),
            some_of(named)
        ));
    }
    // Coming back, photos home has are named where they are there; any other photo not on this
    // device goes back as its name alone.
    if coming_back && !photos.missing.is_empty() {
        let named = &photos.missing;
        losses.push(format!(
            "{} photo(s) a photos field names aren't on this device, so the field package names them without them: {}",
            named.len(),
            some_of(named)
        ));
    }
    if !coming_back {
        if !photos.missing.is_empty() {
            let named = &photos.missing;
            losses.push(format!(
                "{} photo(s) a photos field names aren't files on this computer by their full path, so they stayed out of the field package: {}",
                named.len(),
                some_of(named)
            ));
        }
        if !photos.too_large.is_empty() {
            let named = &photos.too_large;
            losses.push(format!(
                "{} photo(s) didn't fit in the field package (its size, index or entry limit), so they stayed home: {}",
                named.len(),
                some_of(named)
            ));
        }
    }
    losses.extend(photos.notes.iter().cloned());
    // Photos sent out are optional: dropped below if the map needs their room.
    let carried = if coming_back { 0 } else { photos.entries.iter().filter(|(n, _)| n != INDEX).count() };
    entries.extend(photos.entries);
    if coming_back {
        // What the phone changed in the rows it was given, which Bring Back applies.
        let package = s.project.field_origin.as_ref().map(|o| o.package.clone()).unwrap_or_default();
        match s.project.field_origin.as_ref().filter(|_| package.is_empty()) {
            Some(origin) if origin.generation.is_empty() => losses.push(
                "The field package goes back as a whole map with no return check: it was made before packages had one, so Bring Back can't tell it from a copy already brought back, and bringing it back may undo or repeat what came home since",
            ),
            Some(_) => losses.push(WHOLE_MAP.replace("{why}", "the package was made before packages had an identity of their own")),
            None => {}
        }
        if !package.is_empty() {
            match super::field_changes::list(s.map(), &package) {
                Ok(Some(mut changes)) => {
                    photos_named_at_home(&mut changes)?;
                    if !changes.gone.is_empty() {
                        losses.push(format!(
                            "{} layer(s) the field package gave were removed from its map, so none of their features change at home: {}",
                            changes.gone.len(),
                            changes.gone.join(", ")
                        ));
                    }
                    entries.push((super::field_changes::ENTRY.into(), serde_json::to_vec(&changes).map_err(|e| e.to_string())?));
                }
                Ok(None) => losses.push(WHOLE_MAP.replace("{why}", "this device doesn't keep the rows the package was given")),
                Err(e) if e == rustgis_data::DataError::Cancelled.to_string() => return Err(e),
                Err(e) => losses.push(
                    WHOLE_MAP.replace("{why}", &format!("the rows the package was given couldn't be read or compared with the map ({e})")),
                ),
            }
        }
    }
    // A project of the one map: what else the project holds (tables, notebooks, models, shared
    // items and their paths…) stays at home. The custom colour ramps and styles go with it, as its
    // layers may draw with them.
    let origin = if coming_back {
        if s.project.field_origin.is_none() {
            if !s.project.origin_id.is_empty() {
                return Err("the field package has an incomplete origin identity; legacy recovery cannot override it".into());
            }
            notes.push("Legacy field return: its origin is unverified; bring it back only with an explicitly verified legacyArea in the home project");
        }
        s.project.field_origin.clone()
    } else {
        let ai = home_area
            .or_else(|| s.project.offline_areas.iter().position(|a| a.name.eq_ignore_ascii_case(&area)))
            .ok_or("the offline area was not found")?;
        let new_home = s.project.origin_id.is_empty().then(crate::project::field_id).transpose()?;
        let new_area = s.project.offline_areas.get(ai).is_some_and(|a| a.id.is_empty()).then(crate::project::field_id).transpose()?;
        // The first package of an area gives it the state a return is checked against; later ones
        // are made of the same state until a sync or Bring Back replaces it.
        let new_generation =
            s.project.offline_areas.get(ai).is_some_and(|a| a.generation.is_empty()).then(crate::project::field_id).transpose()?;
        let assigned_identity = new_home.is_some() || new_area.is_some();
        let gave_generation = new_generation.is_some();
        if assigned_identity || gave_generation {
            field_edit(s, "Assign field origin identities", |s| {
                if let Some(id) = new_home {
                    s.project.origin_id = id;
                }
                if let Some(area) = s.project.offline_areas.get_mut(ai) {
                    if let Some(id) = new_area {
                        area.id = id;
                    }
                    if let Some(id) = new_generation {
                        area.generation = id;
                    }
                }
                Ok(Value::Null)
            })?;
        }
        if assigned_identity {
            notes.push("Assigned this legacy offline area an origin identity: save the home project before sending the field package");
        } else if gave_generation {
            notes.push("Gave this offline area a return check: save the home project before sending the field package");
        }
        let home_area = s.project.offline_areas.get(ai).ok_or("the offline area was not found")?;
        Some(crate::project::FieldOrigin {
            project: s.project.origin_id.clone(),
            area: home_area.id.clone(),
            generation: home_area.generation.clone(),
            package: crate::project::field_id()?,
            ids: Default::default(),
        })
    };
    if origin
        .as_ref()
        .is_some_and(|origin| !crate::project::valid_field_id(&origin.project) || !crate::project::valid_field_id(&origin.area))
    {
        return Err("the field origin identity is invalid: make a new field package from the home project".into());
    }
    let mut project = crate::project::Project::new(area.clone());
    project.field_origin = origin;
    // New layers made in the field are held in the home project's system, as at home.
    (project.coordinate_system, project.crs_from_first_layer) = (s.project.coordinate_system, s.project.crs_from_first_layer);
    // The package has no home offline areas and carries its association in field_origin.
    map.offline_area_id = None;
    let mut copied = Vec::new();
    if !coming_back {
        // Older phones also look at the outgoing title to show Send Field Package.
        map.name = format!("{area}{OFFLINE}");
        let extent = s
            .project
            .offline_areas
            .iter()
            .find(|saved| saved.name.eq_ignore_ascii_case(&area))
            .ok_or("the offline area was not found")?
            .extent;
        let available = project_room(companion_bytes(&entries));
        copied = copy_streams(&mut map, extent, stream_budget(available), &mut losses)?;
        // Their photos stay home: a reference copy's edits and photos don't come back. Counted by
        // their separators, without looking at the disk: a count for the notice.
        let mut named = 0usize;
        for (l, fi) in map.layers.iter().filter(|l| copied.contains(&l.id)).flat_map(|l| photos_fields(l).map(move |fi| (l, fi))) {
            for value in l.dataset.features.column(fi) {
                progress::check().map_err(|e| e.to_string())?;
                if let rustgis_data::Value::Text(t) = value {
                    named += t.split(';').filter(|p| !p.trim().is_empty()).count();
                }
            }
        }
        if named > 0 {
            losses.push(format!(
                "{named} photo reference(s) of layer(s) copied for reference stayed home: those copies' photos don't go in the field package"
            ));
        }
    }
    // Going out, the highest id each layer has: features added in the field are given ids above it,
    // and so are those added here, in the area's map or its offline map, while the package is out.
    if !coming_back && let Some(origin) = project.field_origin.as_mut() {
        // At least what earlier packages of the area recorded: the area's map, renewed from the
        // snapshot, holds no floor of its own, and a package still out may hold a higher id.
        let recorded =
            s.project.offline_areas.iter().find(|a| a.name.eq_ignore_ascii_case(&area)).map(|a| a.ids.clone()).unwrap_or_default();
        // New features are given ids above the highest: at `i64::MAX`, the largest a GeoPackage or
        // PostGIS feature can have, the next would be one neither can hold, and the floor isn't one a
        // project can keep.
        if let Some(l) = map.layers.iter().find(|l| !l.is_raster() && l.dataset.features.highest_id() >= i64::MAX.unsigned_abs()) {
            return Err(format!(
                "{}: a feature's id is {} or more, so features added in the field couldn't be given ids a GeoPackage or PostGIS table can hold. Give the layer's features smaller ids (copy them to a new layer), then package it",
                l.name,
                i64::MAX
            ));
        }
        origin.ids = map
            .layers
            .iter()
            .filter(|l| !l.is_raster())
            .map(|l| (l.id, l.dataset.features.highest_id().max(recorded.get(&l.id).copied().unwrap_or(0))))
            .filter(|(_, id)| *id > 0)
            .collect();
        if let Some(home) = s.project.offline_areas.iter_mut().find(|a| a.name.eq_ignore_ascii_case(&area)) {
            for (layer, id) in &origin.ids {
                let held = home.ids.entry(*layer).or_insert(0);
                *held = (*held).max(*id);
            }
        }
    }
    project.maps = vec![map];
    let home = &s.project;
    let stayed: Vec<String> = [
        (home.tables.len(), "table(s)"),
        (home.notebooks.len(), "notebook(s)"),
        (home.models.len(), "model(s)"),
        (home.layouts.len(), "layout(s)"),
        (home.reports.len(), "report(s)"),
        (home.presentations.len(), "presentation(s)"),
        (home.workspaces.len(), "imagery workspace(s)"),
        (home.dl_models.len(), "deep learning model(s)"),
        (home.folders.len(), "folder(s)"),
        (home.shared.len(), "shared item(s)"),
        (home.data_stores.len(), "data store(s)"),
        (home.tasks.len(), "task(s)"),
        (home.jobs.len(), "job(s)"),
        (home.gp_history.len(), "geoprocessing history run(s)"),
        (home.offline_areas.iter().filter(|saved| !saved.name.eq_ignore_ascii_case(&area)).count(), "other offline area(s)"),
        (home.maps.len().saturating_sub(1), "other map(s)"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    if !stayed.is_empty() {
        losses.push(format!("The field package holds only the map '{area}': {} stayed in this project", stayed.join(", ")));
    }
    project.ramps = s.project.ramps.clone();
    project.styles = s.project.styles.clone();
    let readme = format!(
        "{area}: a RustGIS field package.\nOpen it with RustGIS on a phone or a computer (Open Project). Its basemaps are in {BASEMAPS}/.\n"
    );
    entries.push(("README.txt".into(), readme.into_bytes()));
    // With room for the marker a package going out ends with.
    let mut companions = companion_bytes(&entries) + if coming_back { 0 } else { MARKER_ROOM };
    let too_large = || "the field package would be too large: leave a tile archive, photos or some data out".to_string();
    if !copied.is_empty() {
        let map = project.maps.first().ok_or("the field package holds no map")?;
        let names: Vec<_> = copied.iter().filter_map(|id| map.layer(*id)).map(|l| l.name.as_str()).collect();
        losses.push(format!(
            "{} layer(s) read in place at home were copied for reference: {}. Edits to these copies stay in the field project",
            names.len(),
            names.join(", ")
        ));
    }
    let mut manifest = manifest_bytes(&archive_errors, &losses)?;
    progress::check().map_err(|e| e.to_string())?;
    let (mut text, mut report) =
        crate::durable::output(&mut project, None, |p| crate::project::json_within(p, project_room(companions + manifest.len() as u64)))?;
    // The map comes first: photos it needs the room of stay home (the features still name them
    // where they are here), and that is said.
    if text.is_none() && carried > 0 {
        entries.retain(|(n, _)| !n.starts_with(&format!("{PHOTOS}/")));
        companions = companion_bytes(&entries) + MARKER_ROOM;
        // Before the note the fallback below takes back, if it runs.
        // The note of the layers copied for reference is the last one when there are any (pushed
        // just before the manifest); the fallback below takes it back first.
        let at = losses.len() - usize::from(!copied.is_empty());
        losses.insert(at, format!("{carried} photo(s) stayed home: the map needed the field package's room for them"));
        manifest = manifest_bytes(&archive_errors, &losses)?;
        (text, report) = crate::durable::output(&mut project, None, |p| {
            crate::project::json_within(p, project_room(companions + manifest.len() as u64))
        })?;
    }
    if text.is_none() && !copied.is_empty() {
        // Row storage and encoded JSON measure different things. The package's hard limit
        // also includes loaded layers and basemaps, so retry once without the streamed copies.
        losses.pop();
        let map = project.maps.first_mut().ok_or("the field package holds no map")?;
        map.release_tiles(&copied);
        map.layers.retain(|l| {
            if copied.contains(&l.id) {
                losses.push(format!(
                    "{}: {} feature(s) in the offline area were left out because the field package would be too large: package a smaller area, or save the rows you need as a separate layer (for example, export the filtered rows) and package that layer",
                    l.name,
                    l.dataset.features.len()
                ));
                false
            } else {
                true
            }
        });
        manifest = manifest_bytes(&archive_errors, &losses)?;
        (text, report) = crate::durable::output(&mut project, None, |p| {
            crate::project::json_within(p, project_room(companions + manifest.len() as u64))
        })?;
    }
    let text = text.ok_or_else(too_large)?;
    progress::check().map_err(|e| e.to_string())?;
    crate::durable::notify(s, &report);
    entries.insert(0, (format!("{}.{}", super::items::safe(&area), crate::project::EXTENSION), text.into_bytes()));
    if !manifest.is_empty() {
        entries.push((LEFT_BEHIND.into(), manifest));
    }
    // Going out, the package says so, with its identity: only its rows are kept as given.
    if let Some(package) = project.field_origin.as_ref().map(|o| o.package.as_str()).filter(|p| !coming_back && !p.is_empty()) {
        entries.push((super::field_changes::GIVEN.into(), super::field_changes::given_entry(package)?));
    }
    let bytes = rustgis_data::zip::write(&entries)?;
    progress::check().map_err(|e| e.to_string())?;
    notes.extend(losses);
    Ok((area, bytes))
}

/// Said when a returning package goes without its list of changes (`{why}`).
const WHOLE_MAP: &str = "The field package goes back as a whole map, without the list of what changed in it: {why}. Bring Back refuses it if its offline area has been brought back or synced since the package was made";

/// Builds and stages the package against a shared-row project snapshot. Delivery checks
/// the live home state before installing the file and recording the new identities or area.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn export_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    if p.get("legacyReturn").is_some() {
        legacy_user(s)?;
    }
    let path = super::ps(p, "path").ok_or("path is required")?.to_string();
    not_the_project(s, Path::new(&path))?;
    let p = p.clone();
    let construction_notes = s.constructions.output_notes(s.map().layers.iter().map(|l| (s.map().instance_id(), l)))?;
    let project = s.project.clone();
    let (view, opened_package) = (s.view_size, s.opened_package.clone());
    let (opened, state) = (s.opened, s.state());
    Ok(Box::new(move || {
        let mut copy = Session::new(project);
        copy.notices.clear();
        copy.undo_memory.memory = None;
        copy.view_size = view;
        copy.opened_package = opened_package;
        let before = (copy.project.origin_id.clone(), area_ids(&copy.project));
        let staged = package_output(&mut copy, &p).and_then(|(area, bytes, notes)| {
            let answer = packaged(&path, &area, bytes.len());
            let file = rustgis_data::io::stage_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            // A concurrent sync can advance a home area's generation. A return from the
            // field owns its snapshot without any home update.
            let outbound = copy.project.offline_areas.iter().any(|saved| saved.name.eq_ignore_ascii_case(&area));
            Ok((answer, file, notes, outbound))
        });
        // A return spends its package: the rows it gave are let go once the return is written.
        let spent = spent_package(&copy);
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        let (errors, other): (Vec<_>, Vec<_>) = copy.notices.into_iter().partition(|n| n.error);
        let diagnostics = errors.into_iter().map(|n| (n.severity, n.text)).collect();
        let next = copy.project;
        let change = (before != (next.origin_id.clone(), area_ids(&next))).then_some((next.origin_id, next.offline_areas));
        let control = rustgis_analysis::control::current();
        let then: super::exports::Then = Box::new(move |s, answer| {
            let (result, file, notes, outbound) = staged?;
            let spent = spent.filter(|_| !outbound);
            if outbound && (s.opened != opened || s.state() != state) {
                return Err("the home project changed during packaging; make a new package before sending it to the field".into());
            }
            // Only a rename and directory sync run on the window thread. The live authority
            // check applies until the rename, including when the worker has finished staging.
            {
                let _probe = rustgis_analysis::control::install_progress(control);
                not_the_project(s, file.destination())?;
                file.commit().map_err(|e| format!("{path}: {e}"))?;
            }
            if let Some((origin, areas)) = change {
                field_edit(s, "Prepare home project for field package", |s| {
                    s.project.origin_id = origin;
                    s.project.offline_areas = areas;
                    Ok(Value::Null)
                })
                .map_err(|e| {
                    format!("{path} was written, but the home project couldn't record its offline area ({e}): make a new package before sending this one")
                })?;
            }
            let mark = s.notice_mark();
            super::exports::say(s, other.into_iter().map(|n| (n.severity, n.text)).collect());
            for note in notes.into_iter().chain(construction_notes.into_iter().map(rustgis_data::loss::Note::plain)) {
                s.say(note);
            }
            spend(s, spent);
            s.retain_written_notices(mark);
            *answer = result;
            Ok(())
        });
        Ok(super::exports::Wrote {
            answer: Value::Null,
            notes: Default::default(),
            then: Some(then),
            written_path: None,
            diagnostics,
            refused_notes: Vec::new(),
            owner: None,
            crs_used: Vec::new(),
        })
    }))
}

/// The package a field project's return spends: its own identity, when it has one.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spent_package(s: &Session) -> Option<String> {
    s.project.field_origin.as_ref().map(|o| o.package.clone()).filter(|p| !p.is_empty())
}

/// Once a return of package `spent` has been written: home brings a package back once, so the rows
/// it gave this device are no longer needed, and are removed. Said either way.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spend(s: &mut Session, spent: Option<String>) {
    let Some(package) = spent else { return };
    match super::field_changes::forget(&package) {
        Ok(true) => s.notify(
            "The field package went back: this device no longer keeps the rows it gave. A package comes back once, so make a new field package for more edits",
            false,
        ),
        Ok(false) => {}
        Err(e) => s.notify(format!("The field package went back, but the rows it gave couldn't be removed from this device: {e}"), true),
    }
}

/// Offline area identity state which packaging can change. Feature rows are shared with home
/// until the worker builds a new area's clipped baseline.
#[cfg(not(target_arch = "wasm32"))]
fn area_ids(project: &Project) -> Vec<(String, String, std::collections::BTreeMap<u32, u64>)> {
    project.offline_areas.iter().map(|a| (a.id.clone(), a.generation.clone(), a.ids.clone())).collect()
}

fn companion_bytes(entries: &[(String, Vec<u8>)]) -> u64 {
    entries.iter().map(|(_, bytes)| bytes.len() as u64).sum()
}

/// The project JSON must fit its own limit and the space left in the unpacked ZIP.
fn project_room(companions: u64) -> u64 {
    crate::project::max_bytes().min(most_unpacked().saturating_sub(companions))
}

/// [`MOST_UNPACKED`], or a smaller limit a test sets ([`tests::with_unpacked`]).
fn most_unpacked() -> u64 {
    #[cfg(test)]
    if let Some(most) = tests::UNPACKED.with(std::cell::Cell::get) {
        return most;
    }
    MOST_UNPACKED as u64
}

/// Retain one map's worth of clipped streamed rows within a shared budget. Leave some of the
/// package room for the JSON and plan for both encoders and the other data already in memory.
fn stream_budget(room: u64) -> usize {
    let memory = rustgis_data::memory::planned(8 << 20, 128 << 20, 1);
    memory.min(usize::try_from(room / 2).unwrap_or(usize::MAX))
}

/// Copies only streams backed by portable reads. Failed or oversized layers are removed
/// whole, so a missing source or a truncated sample cannot look like a complete field layer.
fn copy_streams(
    map: &mut rustgis_map::Map,
    extent: rustgis_data::Extent,
    mut budget: usize,
    losses: &mut rustgis_data::loss::Notes,
) -> Result<Vec<u32>, String> {
    let mut copied = Vec::new();
    let mut omitted = Vec::new();
    for layer in map.layers.iter_mut().filter(|l| l.is_streamed()) {
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        match copy_stream(layer, extent, budget, losses) {
            Ok(Some(used)) => {
                budget -= used;
                copied.push(layer.id);
            }
            Ok(None) => omitted.push(layer.id),
            Err(error) => {
                rustgis_data::progress::check().map_err(|e| e.to_string())?;
                losses.push(format!("1 streamed layer was left out of the field package: {} ({error})", layer.name));
                omitted.push(layer.id);
            }
        }
    }
    if !omitted.is_empty() {
        map.release_tiles(&omitted);
        let omitted: std::collections::HashSet<_> = omitted.into_iter().collect();
        map.layers.retain(|l| !omitted.contains(&l.id));
    }
    Ok(copied)
}

/// Counts every in-area row even when the layer is too large, dropping the retained rows as
/// soon as it cannot fit. Source readers keep one chunk (or one indivisible row) at a time.
fn copy_stream(
    layer: &mut rustgis_map::Layer,
    extent: rustgis_data::Extent,
    budget: usize,
    losses: &mut rustgis_data::loss::Notes,
) -> Result<Option<usize>, String> {
    use rustgis_data::{Feature, progress, source};
    let eligible = layer.source.as_ref().is_some_and(|s| s.linked_file().is_some() || s.embedded.is_some());
    let Some((src, mut dataset)) = (if eligible { layer.chunk_in_extent()? } else { None }) else {
        let count = layer
            .source
            .as_ref()
            .and_then(|s| s.info())
            .and_then(|i| i.count)
            .map_or_else(|| "source feature count unavailable".into(), |count| format!("{count} feature(s) in the source"));
        losses.push(format!("{}: streamed layer left out because its source can't be copied in pieces ({count})", layer.name));
        return Ok(None);
    };
    let mut rows = Vec::new();
    let (mut used, mut inside, mut outside) = (0usize, 0usize, 0usize);
    let mut fits = true;
    let info = src.info();
    // Its rows come in the source's own system, held there in the copy; the area is compared in
    // longitude/latitude.
    let to_lonlat = src.coord_crs().to(rustgis_data::raster::Crs::LonLat).map_err(|e| format!("{}: {e}", layer.name))?;
    let batch = src
        .visit_chunks(Some(extent), 1 << 20, &mut |chunk| {
            progress::check()?;
            for row in chunk {
                // Only a row the copy keeps is reshaped; one past the budget is just counted.
                let Some((row, bytes)) = (if fits {
                    source::chunk_memory::clip_row(row, &extent, dataset.fields.len(), &to_lonlat)?
                } else {
                    source::chunk_memory::clip_row(row, &extent, 0, &to_lonlat)?
                }) else {
                    outside += 1;
                    continue;
                };
                inside += 1;
                if fits {
                    // Count twice the row slots for the vector's spare capacity, as it grows.
                    used = used.saturating_add(bytes.saturating_add(2 * std::mem::size_of::<Feature>()));
                    fits = used <= budget && rows.try_reserve(1).is_ok();
                    if fits {
                        rows.push(row);
                    } else {
                        rows = Vec::new();
                    }
                }
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    let unread = batch.skipped.saturating_add(src.skipped_at_open());
    if unread > 0 {
        losses.push(format!("{}: {unread} feature(s) couldn't be read while preparing the field package", layer.name));
    }
    if !batch.complete {
        return Err("the source did not provide a complete read".into());
    }
    if !fits {
        losses.push(format!(
            "{}: {inside} feature(s) in the offline area were left out because the streamed layer exceeded the field package budget: package a smaller area, or save the rows you need as a separate layer (for example, export the filtered rows) and package that layer",
            layer.name
        ));
        return Ok(None);
    }
    // An indexed read can skip the rest of the file outright, so the total outside the area comes
    // from the source's count. Rows a source refused when it opened are left out of that count
    // (GeoJSON's reprojection failures, for one) and said above; rows skipped while reading may
    // not be, so with any of those the rest is a lower bound, said as one.
    let skipped = batch.skipped as u64;
    let outside =
        info.count.map_or(outside as u64, |total| total.saturating_sub(inside as u64).saturating_sub(skipped).max(outside as u64));
    if outside > 0 {
        let at_least = if skipped > 0 { "at least " } else { "" };
        losses.push(format!(
            "{}: {at_least}{outside} feature(s) outside the offline area's extent, or without a shape, stayed out of the field package",
            layer.name
        ));
    }
    let counted = [
        source::unread_dates_note(&info.fields, &batch.unread_dates),
        source::rounded_dates_note(&info.fields, &batch.rounded_dates),
        source::null_text_note(&info.fields, &batch.null_text),
    ];
    losses.extend(info.notes.iter().chain(&batch.notes).cloned().map(|note| note.prefixed(&layer.name)));
    losses.extend(counted.into_iter().flatten().map(|note| note.prefixed(&layer.name)));
    // A shape with no place in longitude/latitude is kept as the source holds it (#2032 C2f-4b),
    // and said, as a whole read says it.
    dataset.crs_incomplete = src.lost_to_crs();
    dataset.features = rows.into();
    if !src.says_unplaced_at_open() {
        losses.extend(dataset.kept_unplaced_notes()?.into_iter().map(|n| format!("{}: {n}", layer.name)));
    }
    if dataset.geometry_type == rustgis_data::GeometryType::None
        && let Some(g) = dataset.features.geometry(0)
    {
        dataset.geometry_type = g.kind();
    }
    dataset.source = None;
    layer.source = None;
    layer.dataset = dataset;
    layer.synced = None;
    super::workspace::unbind_table_edits(layer);
    layer.selection.clear();
    layer.touch_geometry();
    Ok(Some(used))
}

/// Only portable, redacted summaries go into the archive. Keep the list within the receiver's
/// diagnostic limits even when a project has thousands of missing layers.
fn manifest_bytes(errors: &[String], losses: &rustgis_data::loss::Notes) -> Result<Vec<u8>, String> {
    let total = errors.len() + losses.len();
    if total == 0 {
        return Ok(Vec::new());
    }
    let mut notes: Vec<_> =
        errors.iter().map(String::as_str).chain(losses.texts()).take(MAX_NOTICES).map(|s| Session::redacted(s).into_string()).collect();
    // JSON can grow when text has escapes. Drop the tail and count it if the byte limit is hit.
    loop {
        let mut candidate = notes.clone();
        let omitted = total - notes.len();
        if omitted > 0 {
            candidate.push(format!("{omitted} more field package notice(s) were reported when the package was made"));
        }
        if let Some(text) = crate::project::json_within(&candidate, MAX_MANIFEST as u64)? {
            return Ok(text.into_bytes());
        }
        notes.pop();
    }
}

/// Each of a layer's photos fields, by place: its text fields named `photos` or `photo`, whatever
/// the case (as the touch interface names them); a layer can have both.
fn photos_fields(layer: &rustgis_map::Layer) -> impl Iterator<Item = usize> + '_ {
    layer.dataset.fields.iter().enumerate().filter_map(|(i, f)| {
        (f.ty == rustgis_data::FieldType::Text && (f.name.eq_ignore_ascii_case("photos") || f.name.eq_ignore_ascii_case("photo")))
            .then_some(i)
    })
}

/// Rewrites each photos field of `map` with `rewrite` (one path in, the path to name instead out).
fn rewrite_photos(map: &mut rustgis_map::Map, rewrite: impl FnMut(&str) -> String) {
    let _ = rewrite_photos_until(map, rewrite, || Ok(()));
}

/// [`rewrite_photos`], asking `go_on` before each row and stopping with its error: Cancel, for
/// a pass whose reading of a row can look at the disk.
fn rewrite_photos_until(
    map: &mut rustgis_map::Map,
    mut rewrite: impl FnMut(&str) -> String,
    mut go_on: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    for l in map.layers.iter_mut().filter(|l| !l.is_raster()) {
        let fields: Vec<usize> = photos_fields(l).collect();
        let features = &mut l.dataset.features;
        for fi in fields {
            for row in 0..features.len() {
                go_on()?;
                let rustgis_data::Value::Text(held) = features.value(row, fi) else { continue };
                if let Some(out) = rewrite_paths(held, &mut rewrite) {
                    let _ = features.set_value(row, fi, rustgis_data::Value::Text(out));
                }
            }
        }
    }
    Ok(())
}

/// A photos field's text `held` with each path rewritten by `rewrite`, or `None` when none
/// changes. Only the paths rewritten change: the rest of the text stays as it was written, so a
/// feature isn't seen as edited for its spacing.
fn rewrite_paths(held: &str, rewrite: &mut impl FnMut(&str) -> String) -> Option<String> {
    let (mut out, mut last) = (String::new(), 0);
    for r in photo_paths(held) {
        let (Some(before), Some(path)) = (held.get(last..r.start), held.get(r.clone())) else { continue };
        let new = rewrite(path);
        if new != path {
            out.push_str(before);
            out.push_str(&new);
            last = r.end;
        }
    }
    if last == 0 {
        return None;
    }
    out.push_str(held.get(last..).unwrap_or_default());
    Some(out)
}

/// Names each photo a package brought from home by its path at home again in the old values of
/// `changes`: the field held it at the path it was unpacked to, and Bring Back checks each old
/// value against home's.
fn photos_named_at_home(changes: &mut super::field_changes::Changes) -> Result<(), String> {
    let mut unpacked = Unpacked::new();
    let package = changes.package.clone();
    let mut at_home = |path: &str| home_path(&mut unpacked, &package, path);
    for layer in &mut changes.layers {
        for given in layer.edited.iter_mut().chain(layer.deleted.iter_mut()) {
            // Reading a value's paths can look at the disk: Cancel is looked at for each row.
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            for (field, value) in &mut given.values {
                if !(field.eq_ignore_ascii_case("photos") || field.eq_ignore_ascii_case("photo")) {
                    continue;
                }
                let rustgis_data::Value::Text(held) = value else { continue };
                if let Some(out) = rewrite_paths(held, &mut at_home) {
                    *value = rustgis_data::Value::Text(out);
                }
            }
        }
    }
    Ok(())
}

/// The most pieces of a photos field (split at `;`) tried as one path with `;` in it.
const JOINED: usize = 16;

/// Where the paths a photos field holds are in its text (`;` between them), trimmed, empty ones
/// left out. A path with a `;` of its own is one path when, joined again, it names a file on this
/// device.
fn photo_paths(held: &str) -> Vec<std::ops::Range<usize>> {
    let mut pieces = Vec::new();
    let mut start = 0;
    for (i, c) in held.char_indices() {
        if c == ';' {
            pieces.push(start..i);
            start = i + 1;
        }
    }
    pieces.push(start..held.len());
    let names_file = |r: &std::ops::Range<usize>| held.get(r.clone()).is_some_and(|p| Path::new(p.trim()).is_file());
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(first) = pieces.get(i) {
        // The longest run of pieces from here that names a file, or this one alone: tried only
        // from a piece that is a whole path's start, and over a few pieces, so a long value costs
        // a few looks at the disk for each piece, not one for each pair.
        let absolute = held.get(first.clone()).is_some_and(|p| Path::new(p.trim()).is_absolute());
        let reach = if absolute { pieces.len().min(i + JOINED) } else { i + 1 };
        let last = (i + 1..reach).rev().find(|j| pieces.get(*j).is_some_and(|end| names_file(&(first.start..end.end)))).unwrap_or(i);
        let end = pieces.get(last).map_or(first.end, |p| p.end);
        if let Some(text) = held.get(first.start..end) {
            let lead = text.len() - text.trim_start().len();
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                out.push(first.start + lead..first.start + lead + trimmed.len());
            }
        }
        i = last + 1;
    }
    out
}

/// The photos `map`'s features name that a package going out carries: each picture named by its
/// full path on this computer, under `photos/` (within `budget`), with [`INDEX`] giving the path
/// each was named by, which the phone points the features at the unpacked file by
/// ([`project_in`]). The features aren't changed here, so the rows the map shares with the
/// offline area aren't copied. A name a feature already gives under `photos/` isn't used for
/// another. What stays out is listed by kind. Cancel is checked before each photo is looked at.
fn collect_photos(map: &rustgis_map::Map, budget: &mut u64, most_entries: usize) -> Result<Photos, String> {
    let mut found = Photos::default();
    // Every path named, once, in order; and the names already given under `photos/`, kept.
    // The names are borrowed from the map's rows, not copied.
    let (mut named, mut seen, mut taken): (Vec<&str>, std::collections::HashSet<&str>, std::collections::HashSet<String>) =
        (Vec::new(), std::collections::HashSet::new(), std::collections::HashSet::new());
    // A streamed layer's photos are left to its copy for reference, which counts them as staying
    // home (or leaves the layer out): the rows it holds now aren't what the package will hold.
    for (l, fi) in map.layers.iter().filter(|l| !l.is_raster() && !l.is_streamed()).flat_map(|l| photos_fields(l).map(move |fi| (l, fi))) {
        for value in l.dataset.features.column(fi) {
            // Reading a field's paths can look at the disk (a path with a `;` in it).
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let rustgis_data::Value::Text(held) = value else { continue };
            for r in photo_paths(held) {
                let Some(path) = held.get(r) else { continue };
                if let Some(name) = path.strip_prefix(&format!("{PHOTOS}/")) {
                    taken.insert(name.to_ascii_lowercase());
                }
                if seen.insert(path) {
                    named.push(path);
                }
            }
        }
    }
    let (mut index, mut number) = (String::new(), 0u32);
    // Each photo looked at is a step: its file is read.
    rustgis_data::progress::begin(named.len());
    for path in named {
        rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        let file = Path::new(&path);
        if !file.is_absolute() {
            found.missing.push(path.to_string());
            continue;
        }
        // The index holds one path a line, its fields split by tabs; a photos field splits its
        // paths at `;`, which the phone can't tell from one path once the file isn't there.
        if path.contains(['\t', '\n', '\r', ';']) {
            found.unread.push(format!("{path} (its name holds a tab, a line break or a ;)"));
            continue;
        }
        let size = match std::fs::metadata(file) {
            Ok(m) if m.is_file() => m.len(),
            Ok(_) => {
                found.missing.push(path.to_string());
                continue;
            }
            // The browser has no files to look at by path: not here either.
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::Unsupported) => {
                found.missing.push(path.to_string());
                continue;
            }
            Err(e) => {
                found.unread.push(format!("{path} ({e})"));
                continue;
            }
        };
        if !file.extension().is_some_and(|e| PICTURES.iter().any(|p| e.eq_ignore_ascii_case(p))) {
            found.not_pictures.push(path.to_string());
            continue;
        }
        // A short name of plain letters and digits, numbered: two names a system takes for one
        // (by case or by how an accent is written) or one too long for it can't come of it. The
        // index keeps the path it was named by.
        let ext = file.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let mut name = String::new();
        while name.is_empty() || taken.contains(&name) {
            number += 1;
            name = format!("home-{number:05}.{ext}");
        }
        let entry = format!("{PHOTOS}/{name}");
        let line = format!("{entry}\t{path}\n");
        // Past the budget, past what the phone reads of the index, or past the entries a package
        // holds: it stays home.
        if size > *budget || (index.len() + line.len()) as u64 > MAX_INDEX || found.entries.len() >= most_entries {
            found.too_large.push(path.to_string());
            continue;
        }
        match crate::project::read_file(file, *budget) {
            Ok(bytes) => {
                *budget = budget.saturating_sub(bytes.len() as u64);
                taken.insert(name);
                index.push_str(&line);
                found.entries.push((entry, bytes));
            }
            Err(e) => found.unread.push(format!("{path} ({e})")),
        }
    }
    if !index.is_empty() {
        found.entries.push((INDEX.to_string(), index.into_bytes()));
    }
    Ok(found)
}

/// Coming back, the photo files `map`'s features name that are on this device, as package entries under
/// `photos/` (within `budget`), each feature then naming its photos there, with [`INDEX`] giving
/// the path each was named by. Only picture files ([`PICTURES`]) named by an absolute path go in,
/// each under a plain name ([`plain_name`]): a photos field is free text, and could name any file.
/// A photo that isn't on this device (one named at home) is left as it is named, and listed in
/// [`Photos::missing`]. A photo a package from home brought is named by
/// its path at home again, from the [`INDEX`] unpacked beside it ([`home_path`]), and isn't
/// carried: home has it, so it takes none of the package's room, and a feature whose photos the
/// field left alone names them as home does. Each row is a counted step, Cancel looked at before
/// it.
fn take_photos(map: &mut rustgis_map::Map, budget: &mut u64, package: &str, most_entries: usize) -> Result<Photos, String> {
    let mut found = Photos::default();
    // Each photo's entry and the path it was named by, for the way back.
    let mut index = String::new();
    let mut placed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // The names features already give under `photos/` are kept: no photo carried takes one.
    let mut taken = std::collections::HashSet::new();
    for (l, fi) in map.layers.iter().filter(|l| !l.is_raster()).flat_map(|l| photos_fields(l).map(move |fi| (l, fi))) {
        for value in l.dataset.features.column(fi) {
            let rustgis_data::Value::Text(held) = value else { continue };
            for name in held.split(';').filter_map(|p| p.trim().strip_prefix(&format!("{PHOTOS}/"))) {
                taken.insert(name.to_ascii_lowercase());
            }
        }
    }
    // The next number to try for each name: photos sharing one don't each count up from 2.
    let mut next: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    // The files already said (missing, not pictures, too large, or unread): each said once,
    // however many name it.
    let mut said = std::collections::HashSet::new();
    // The index unpacked with each folder of photos a package brought: entry → path at home.
    let mut unpacked = Unpacked::new();
    // Each row is a counted step, and Cancel is looked at before it: reading a row's paths can
    // look at the disk.
    let rows: usize = map.layers.iter().filter(|l| !l.is_raster()).map(|l| photos_fields(l).count() * l.dataset.features.len()).sum();
    rustgis_data::progress::begin(rows);
    let go_on = || rustgis_data::progress::step(1).map_err(|e| e.to_string());
    rewrite_photos_until(
        map,
        |path| {
            if let Some(entry) = placed.get(path) {
                return entry.clone();
            }
            if said.contains(path) {
                return path.to_string();
            }
            // Brought from home in this package: named where it is at home, and not carried back.
            let at_home = home_path(&mut unpacked, package, path);
            if at_home != path {
                placed.insert(path.to_string(), at_home.clone());
                return at_home;
            }
            let file = Path::new(path);
            // A picture only: a photos field is free text, and could name any file on the device.
            let picture = file.extension().is_some_and(|e| PICTURES.iter().any(|p| e.eq_ignore_ascii_case(p)));
            if !file.is_absolute() {
                said.insert(path.to_string());
                found.missing.push(path.to_string());
                return path.to_string();
            }
            // Not on this device (in a returning package, a photo named at home): left as named. One
            // there that can't be looked at is said, as one that can't be read is.
            let size = match std::fs::metadata(file) {
                Ok(m) if m.is_file() => m.len(),
                // A folder, or nothing there: no photo file on this device.
                Ok(_) => {
                    said.insert(path.to_string());
                    found.missing.push(path.to_string());
                    return path.to_string();
                }
                // The browser has no files to look at by path: not here either.
                Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::Unsupported) => {
                    said.insert(path.to_string());
                    found.missing.push(path.to_string());
                    return path.to_string();
                }
                Err(e) => {
                    said.insert(path.to_string());
                    found.unread.push(format!("{path} ({e})"));
                    return path.to_string();
                }
            };
            if !picture {
                said.insert(path.to_string());
                found.not_pictures.push(path.to_string());
                return path.to_string();
            }
            // The index holds one path a line, its fields split by tabs: one holding either would be
            // read back as another path.
            if path.contains(['\t', '\n', '\r']) {
                said.insert(path.to_string());
                found.unread.push(format!("{path} (its name holds a tab or a line break)"));
                return path.to_string();
            }
            // Past the budget, past what the computer reads of the index (its line as long as it can
            // be, with a numbered name), or past the entries a package holds: it stays on the phone.
            let longest_line = PHOTOS.len() + 8 + file.file_name().map_or(0, |n| n.len()) + path.len() + 2;
            if size > *budget || (index.len() + longest_line) as u64 > MAX_INDEX || found.entries.len() >= most_entries {
                said.insert(path.to_string());
                found.too_large.push(path.to_string());
                return path.to_string();
            }
            match crate::project::read_file(file, *budget) {
                Ok(bytes) => {
                    *budget = budget.saturating_sub(bytes.len() as u64);
                    let stem = file.file_name().map(|n| plain_name(&n.to_string_lossy())).unwrap_or_else(|| "photo.jpg".into());
                    let mut name = stem.clone();
                    let from = next.entry(stem.to_ascii_lowercase()).or_insert(2);
                    for n in *from.. {
                        if taken.insert(name.to_ascii_lowercase()) {
                            *from = n;
                            break;
                        }
                        name = format!("{n}-{stem}");
                    }
                    let entry = format!("{PHOTOS}/{name}");
                    index.push_str(&format!("{entry}\t{path}\n"));
                    found.entries.push((entry.clone(), bytes));
                    placed.insert(path.to_string(), entry.clone());
                    entry
                }
                Err(e) => {
                    said.insert(path.to_string());
                    found.unread.push(format!("{path} ({e})"));
                    path.to_string()
                }
            }
        },
        go_on,
    )?;
    for (folder, read) in &unpacked {
        if let Err(e) = read {
            found.notes.push(format!(
                "The photo index of the field package this map came from couldn't be read ({}: {e}): its photos go back as copies, not as the computer's own files",
                folder.display()
            ));
        }
    }
    if !index.is_empty() {
        found.entries.push((INDEX.to_string(), index.into_bytes()));
    }
    Ok(found)
}

/// The first few of `names`, and how many more: a photos layer can name thousands.
fn some_of(names: &[String]) -> String {
    const SHOWN: usize = 10;
    let first = names.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    match names.len().saturating_sub(SHOWN) {
        0 => first,
        more => format!("{first} and {more} more"),
    }
}

/// The [`INDEX`] of each folder of photos a package unpacked, read once: entry → path at home.
type Unpacked = std::collections::HashMap<PathBuf, Result<std::collections::HashMap<String, String>, String>>;

/// The path a photo at `path` was named by at home, when a package brought it from there (the
/// [`INDEX`] unpacked beside it names it); otherwise `path` itself.
fn home_path(unpacked: &mut Unpacked, package: &str, path: &str) -> String {
    let file = Path::new(path);
    // Only a full path names an unpacked file: a relative one would be looked for wherever the
    // app runs.
    if !file.is_absolute() {
        return path.to_string();
    }
    file.parent()
        .zip(file.file_name())
        .and_then(|(folder, own)| {
            let index = unpacked.entry(folder.to_path_buf()).or_insert_with(|| unpacked_index(folder, package));
            index.as_ref().ok()?.get(&format!("{PHOTOS}/{}", own.to_string_lossy())).cloned()
        })
        .unwrap_or_else(|| path.to_string())
}

/// The [`INDEX`] a package unpacked into `folder` (its `photos` folder), as entry → path named
/// by where it was packaged; empty where there is none. One that is there but can't be read is an
/// error, said by the caller: its photos can't be told from the field's.
fn unpacked_index(folder: &Path, package: &str) -> Result<std::collections::HashMap<String, String>, String> {
    // Only this field project's package from home: one that went out names itself beside its
    // photos (`given.v1.json`). A return opened elsewhere has none, and another package's photos
    // aren't this one's, so their photos go back as they are.
    if folder.file_name().is_none_or(|n| n != PHOTOS) {
        return Ok(std::collections::HashMap::new());
    }
    // A marker that is there but can't be read is a damaged package, said by the caller.
    let marked = match folder.parent() {
        Some(dir) => super::field_changes::marked(dir, package)?,
        None => false,
    };
    if !marked {
        return Ok(std::collections::HashMap::new());
    }
    // A package's own index, read within a bound: one line for each photo it carried. A package
    // that carried no photo has none; one whose photos are there without it is damaged.
    let file = folder.join(INDEX_FILE);
    if matches!(std::fs::symlink_metadata(&file), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
        let photos = std::fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?.next().is_some();
        return if photos { Err(format!("{}: missing beside the photos", file.display())) } else { Ok(std::collections::HashMap::new()) };
    }
    let bytes = crate::project::read_file(&file, MAX_INDEX)?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8 text", file.display()))?;
    let mut index = std::collections::HashMap::new();
    // One the package didn't write that way (a line without its two fields, an entry listed twice)
    // is damaged: none of it is trusted, and the caller says so.
    for line in text.lines() {
        let (entry, named) = line.split_once('\t').ok_or_else(|| format!("{}: a line isn't an entry and a path", file.display()))?;
        if index.insert(entry.to_string(), named.to_string()).is_some() {
            return Err(format!("{}: {entry} is listed twice", file.display()));
        }
    }
    Ok(index)
}

/// The most a package's photo index is read: a line for each of many thousand photos.
const MAX_INDEX: u64 = 16 << 20;

/// The room kept in a package going out for the marker it ends with
/// ([`super::field_changes::GIVEN`]), which is read back within the same bound.
const MARKER_ROOM: u64 = 4096;

/// What [`take_photos`] found: the package's photo entries (with [`INDEX`]), and the photos named
/// that stay out of it: not on this device (or not named by a full path), not pictures, past the
/// package's budget, or unreadable.
#[derive(Default)]
struct Photos {
    entries: Vec<(String, Vec<u8>)>,
    /// What couldn't be told about the photos as a whole (a package's photo index unreadable).
    notes: Vec<String>,
    missing: Vec<String>,
    not_pictures: Vec<String>,
    too_large: Vec<String>,
    unread: Vec<String>,
}

/// The entry listing each photo of a package and the path its feature named it by before
/// (`photos/a.jpg<TAB>/storage/…/a.jpg`), so a photo brought back to where it already is keeps
/// that path.
const INDEX: &str = "photos/index.tsv";

/// [`INDEX`]'s file name, in the folder a package's photos are unpacked into.
const INDEX_FILE: &str = "index.tsv";

/// The extensions of the picture files a returning field package carries.
const PICTURES: &[&str] = &["jpg", "jpeg", "png", "heic", "heif", "webp"];

/// A file name with nothing a package's photo entry may not hold: no folder separator or drive
/// colon, and not hidden (a leading dot).
fn plain_name(name: &str) -> String {
    let name: String = name.chars().map(|c| if c.is_control() || FORBIDDEN.contains(c) { '_' } else { c }).collect();
    let name = name.trim_end_matches(['.', ' ']).to_string();
    match name.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None if device_name(&name) => format!("_{name}"),
        None if name.is_empty() => "photo".into(),
        None => name,
    }
}

/// What a file name can't hold on one of the systems a package goes to (Windows forbids the most),
/// and `;`, which separates a photos field's paths.
const FORBIDDEN: &str = "/\\:*?\"<>|;";

/// Whether a file name is a Windows device's (`CON`, `NUL.jpg`, `COM1`…), which can't be a file.
fn device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.ends_with(|c: char| c.is_ascii_digit()))
}

/// Whether a photo entry's name is a plain file name on every system: nothing that would put it
/// outside its folder or that a system refuses, so no photo is half written before another fails.
fn plain_file(file: &str) -> bool {
    !file.is_empty() && file == plain_name(file)
}

/// Brings a field package's edits back (`offline.bringBack`): the package (`path`, or `data` in
/// base64) holds an offline area's map, which must be an area of this project. Its photos go in a
/// folder beside this project (`<area> photos`), or the app's own `field photos/<area>` for a
/// project not saved yet, and its features name them there; one whose name is taken there by
/// other bytes gets a name of its own (`2-<name>`), and an entry that isn't a plain file name is
/// left out and counted. Then its list of changes is applied to the area's map
/// ([`super::field_changes::apply`]); a package without one is compared with the area as last
/// synced, as Sync does (`sync_area`), and only while the area is as it was when the package was
/// made. Each says the conflicts. A package comes back once: its identity is kept with the area,
/// and a package of that identity is refused after.
///
/// It runs where it is called, on the window's thread, as it did before packages carried a list of
/// changes and as Sync does: it changes the area's map, the area and its offline map, and writes
/// the photos, as one undoable step, which none of the background paths (tools, exports, reads,
/// rasters) carries yet; moving it to one, with progress and Cancel, belongs with #1562. Its work:
/// the package is one read, each photo one write, the changes one pass over each changed layer of
/// the map and of the package's map, and, when the area's offline map is open with rows no longer
/// shared with its snapshot, one comparison of those rows with the snapshot's
/// ([`super::workspace::area_unsynced`]); a whole map, one pass over the area's features. No
/// Cancel between them.
pub(crate) fn bring_back(s: &mut Session, p: &Value) -> Result<Value, String> {
    let legacy_area = if p.get("legacyArea").is_some() {
        legacy_user(s)?;
        Some(super::ps(p, "legacyArea").filter(|name| !name.trim().is_empty()).ok_or("legacyArea must name a verified home offline area")?)
    } else {
        None
    };
    let bytes = match super::ps(p, "path") {
        Some(path) => crate::project::read_file(Path::new(path), MOST_UNPACKED as u64)?,
        None => {
            use base64::Engine;
            let data = super::ps(p, "data").ok_or("path or data is required")?;
            base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|_| "data must be base64".to_string())?
        }
    };
    let entries = rustgis_data::zip::read(&bytes)?;
    let mut text = None;
    let mut photos = Vec::new();
    // Photo entries that aren't a plain file name, left out.
    let mut strange = 0usize;
    let mut index = String::new();
    let mut changes = None;
    let mut outbound = false;
    for (name, data) in entries {
        if name == INDEX {
            index = String::from_utf8_lossy(&data).into_owned();
        } else if name == super::field_changes::ENTRY {
            changes = Some(data);
        } else if name == super::field_changes::GIVEN {
            outbound = true;
        } else if !name.contains('/') && crate::project::is_project_file(&name) {
            text = Some(String::from_utf8(data).map_err(|_| "the field package's project is damaged".to_string())?);
        } else if let Some(file) = name.strip_prefix(&format!("{PHOTOS}/")) {
            // A photo's own name only: nothing that would put it outside its folder.
            if plain_file(file) {
                photos.push((file.to_string(), data));
            } else {
                strange += 1;
            }
        }
    }
    let text = text.ok_or("the file holds no field package's project")?;
    // A package made here to go out, not one coming back: bringing it back would record it as
    // returned, and refuse the phone's real return after.
    if outbound {
        return Err("this field package was made here to go to the field, not sent back from it: Bring Back reads the package the phone sends with Send Field Package. Nothing was changed".into());
    }
    // Held within where the package came from ([`Session::file_reach`]) before its read reads
    // anything it names.
    let (project, named) = Project::from_import_json_within(&text, None, s.file_reach(p))?;
    named.say(s, "the field package");
    let ai = if let Some(area) = legacy_area {
        let raw: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        // Only entirely absent identities qualify. Null/empty/malformed metadata is not a
        // reason to discard a present association; typed parsing already rejects bad IDs.
        if raw.get("requires").and_then(Value::as_array).is_some_and(|caps| caps.iter().any(|cap| cap.as_str() == Some("field-origin")))
            || raw.get("field_origin").is_some()
            || raw.get("origin_id").is_some()
            || project.field_origin.is_some()
            || !project.origin_id.is_empty()
        {
            return Err("legacyArea cannot override a present or incomplete field identity".into());
        }
        s.project
            .offline_areas
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(area))
            .ok_or("legacyArea does not name an offline area in this home project")?
    } else {
        let origin = project.field_origin.as_ref().ok_or("this legacy field package has no origin identity: explicitly verify its home area and use legacyArea, or make a new field package")?;
        if !crate::project::valid_field_id(&origin.project)
            || !crate::project::valid_field_id(&origin.area)
            || s.project.origin_id.is_empty()
            || origin.project != s.project.origin_id
        {
            return Err(
                "the field package belongs to another home project, or has no valid origin identity: open the project it was made from"
                    .into(),
            );
        }
        s.project
            .offline_areas
            .iter()
            .position(|a| a.id == origin.area)
            .ok_or("this project has no offline area with the field package's identity: open the project as saved after packaging")?
    };
    let area = s.project.offline_areas.get(ai).ok_or("no such area")?.name.clone();
    let origin = project.field_origin.as_ref().filter(|_| legacy_area.is_none());
    let package = origin.map(|o| o.package.clone()).unwrap_or_default();
    // A package comes back once: its changes applied again would be repeated.
    if !package.is_empty() && s.project.offline_areas.get(ai).is_some_and(|a| a.returned.contains(&package)) {
        return Err(format!(
            "this field package was brought back to offline area '{area}' already, so bringing it back again would repeat its changes. Nothing was changed. Give each phone a package of its own, and a new one for more edits after a Bring Back: make it from the area's offline map"
        ));
    }
    let changes = match changes {
        Some(bytes) if !package.is_empty() => Some(super::field_changes::Changes::read(&bytes, &package)?),
        Some(_) => return Err("the field package lists its changes but has no identity of its own: it is damaged".into()),
        None => None,
    };
    // Without a list of its changes, the package's map is compared with the area as last synced, so
    // it must be of that state: a copy of a package already brought back (a second phone's, or the
    // same phone's second return), or one made before a sync, would undo or repeat what came home
    // since. Nothing has been changed yet.
    if let Some(origin) = origin.filter(|_| changes.is_none()) {
        let held = s.project.offline_areas.get(ai).map(|a| a.generation.as_str()).unwrap_or_default();
        if origin.generation.is_empty() {
            s.notify(
                format!(
                    "This field package was made before packages carried a return check, so it can't be told from a copy of one already brought back to '{area}': if another phone or an earlier return used it, its edits are undone or repeated. Make a new package for each phone and each return"
                ),
                false,
            );
        } else if origin.generation != held {
            return Err(format!(
                "this field package is out of date: offline area '{area}' has been brought back or synced since it was made, so bringing it back too would undo or repeat what came home. Nothing was changed. Make a new package from the area's offline map for this phone (one package per phone, a new one after each Bring Back), then redo this phone's edits"
            ));
        }
    }
    // An offline map of the area open here with edits not synced yet would be replaced as the area
    // is renewed from its map: refused first, with nothing changed.
    if super::workspace::area_unsynced(&s.project, ai) {
        return Err(format!(
            "offline area '{area}' has edits in its offline map that aren't synced yet, and bringing the field package back would replace them. Nothing was changed. Sync the area first (Data › Offline › Sync), then Bring Back again"
        ));
    }
    if project.maps.len() != 1 {
        return Err("a field package must hold exactly its original field map".into());
    }
    let mut edited = project.maps.into_iter().next().ok_or("the field package holds no map")?;
    // An area of that name in another project isn't this package's: its layers, matched by id,
    // must be the area's own, of the same kind (a name may have changed in the field), or features
    // of other layers would be changed.
    let baseline = s.project.offline_areas.get(ai).map(|a| &a.data).ok_or("no such area")?;
    let foreign =
        edited.layers.iter().filter(|l| !l.is_raster()).any(|l| {
            baseline.layers.iter().find(|b| b.id == l.id).is_some_and(|b| !b.is_streamed() && b.geometry_type() != l.geometry_type())
        }) || !edited.layers.iter().any(|l| baseline.layers.iter().any(|b| b.id == l.id));
    if foreign {
        return Err(format!(
            "the field package's map isn't one made from this project's offline area '{area}': its layers aren't the area's. Open the project it was made from"
        ));
    }
    let mut reference = Vec::new();
    // A layer the area has no copy of (one added in the field) isn't the area's: matched by id
    // alone, it could reach a home layer of the same id. Left out, and said.
    let mut unowned = Vec::new();
    edited.layers.retain(|l| {
        if !l.is_raster() && !baseline.layers.iter().any(|b| b.id == l.id) {
            unowned.push(l.name.clone());
            return false;
        }
        let keep = !baseline.layers.iter().any(|b| b.id == l.id && b.is_streamed());
        if !keep {
            reference.push(l.name.clone());
        }
        keep
    });
    // The changes to the layers left out (copies of sources read in place, layers the area hasn't)
    // stay in the field project with them.
    let changes = changes.map(|mut c| {
        c.layers.retain(|lc| edited.layers.iter().any(|l| l.id == lc.layer));
        c
    });
    let skipped_photos = if (!reference.is_empty() || !unowned.is_empty()) && !photos.is_empty() {
        let before = photos.len();
        // An entry may be shared by a kept feature and a dropped one. Only discard entries
        // left without a reference among the layers passed to Sync.
        let mut unused: std::collections::HashSet<_> = photos.iter().map(|(name, _)| name.clone()).collect();
        let prefix = format!("{PHOTOS}/");
        for (layer, field) in edited.layers.iter().filter(|l| !l.is_raster()).flat_map(|l| photos_fields(l).map(move |fi| (l, fi))) {
            for value in layer.dataset.features.column(field) {
                let rustgis_data::Value::Text(text) = value else { continue };
                for span in photo_paths(text) {
                    if let Some(name) = text.get(span).and_then(|path| path.strip_prefix(&prefix)) {
                        unused.remove(name);
                    }
                }
            }
        }
        photos.retain(|(name, _)| !unused.contains(name));
        before - photos.len()
    } else {
        0
    };
    // With a list of changes, only the photos of the cells it changes and the features it adds
    // come home: a cell the field didn't change stays as home has it, so its photo would be kept
    // with nothing naming it.
    let unchanged_photos = match &changes {
        Some(changes) if !photos.is_empty() => {
            let home = s.project.offline_areas.get(ai).and_then(|a| s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&a.map)));
            let named = super::field_changes::photos_named(
                &edited,
                home,
                changes,
                |l| photos_fields(l).collect(),
                |text| {
                    photo_paths(text)
                        .into_iter()
                        .filter_map(|r| text.get(r).and_then(|p| p.strip_prefix(&format!("{PHOTOS}/"))).map(str::to_string))
                        .collect()
                },
            );
            let before = photos.len();
            photos.retain(|(name, _)| named.contains(name));
            before - photos.len()
        }
        _ => 0,
    };
    // The photos, beside the project, and the features naming them there.
    let mut put = 0usize;
    let mut renamed = 0usize;
    let mut folder = None;
    if !photos.is_empty() {
        let dir = match s.path.as_deref().and_then(Path::parent) {
            Some(home) => home.join(format!("{} photos", super::items::safe(&area))),
            None => rustgis_data::dirs::app_data_dir()
                .ok_or("this computer gives RustGIS no folder for the field package's photos: save the project first")?
                .join("field photos")
                .join(super::items::safe(&area)),
        };
        match std::fs::metadata(&dir) {
            Ok(metadata) if !metadata.is_dir() => return Err(format!("{}: not a folder; no photo was brought back", dir.display())),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("{}: {error}; no photo was brought back", dir.display()));
            }
            _ => {}
        }
        // The path each photo was named by where it was packaged.
        let before: std::collections::HashMap<&str, &str> = index.lines().filter_map(|l| l.split_once('\t')).collect();
        // Where each photo goes, decided before anything is written: the edits are brought back
        // first, so one refused leaves no photo behind.
        let mut placed = std::collections::HashMap::new();
        for (name, data) in &photos {
            let entry = format!("{PHOTOS}/{name}");
            // Packaged from here (the same file, the same bytes): it keeps its own path.
            if let Some(own) = before.get(entry.as_str()).filter(|p| holds(Path::new(p), data)) {
                placed.insert(entry, (PathBuf::from(own), false));
                continue;
            }
            // A file of that name already there (an earlier package's) is kept: the same bytes
            // are that photo, other bytes, or anything that can't be looked at, get a name of their
            // own.
            let mut path = dir.join(name);
            let mut write = true;
            for n in 2.. {
                match std::fs::metadata(&path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                    // A destination looked at before writing is not an input of the command.
                    Ok(_) if rustgis_data::provenance::without(|| holds(&path, data)) => {
                        write = false;
                        break;
                    }
                    Err(e) => return Err(format!("{}: {e}; no photo was brought back", path.display())),
                    Ok(_) => path = dir.join(format!("{n}-{name}")),
                }
            }
            if write && path.file_name().is_some_and(|f| f.to_string_lossy() != name.as_str()) {
                renamed += 1;
            }
            placed.insert(entry, (path, write));
        }
        rewrite_photos(&mut edited, |path| placed.get(path).map_or_else(|| path.to_string(), |(to, _)| to.display().to_string()));
        folder = Some((dir, photos, placed));
    }
    // Fields the field added (a layer's photos field, made with its first photo) come too.
    // A list of changes names the fields the field added; a whole map is compared with the area.
    let added = match &changes {
        Some(changes) => super::field_changes::add_fields(s, ai, changes),
        None => take_new_fields(s, ai, &edited),
    };
    if !added.is_empty() {
        s.notify(format!("Added the field(s) the field package's layers added: {}", added.join(", ")), false);
    }
    let mut done = match &changes {
        Some(changes) => super::field_changes::apply(s, ai, &edited, changes)?,
        None => super::workspace::sync_area(s, ai, &edited, &package)?,
    };
    if !package.is_empty()
        && let Some(a) = s.project.offline_areas.get_mut(ai)
    {
        a.returned.push(package.clone());
    }
    if let Some(n) = done.get("renumbered").and_then(Value::as_u64).filter(|n| *n > 0) {
        s.notify(format!("{n} feature(s) added in the field took a new id here, theirs being used in this map already"), false);
    }
    // The edits are back: now the photos are written. One that can't be takes those written before
    // it away again, and the command fails (the project is put back as it was).
    let folder = match folder {
        Some((dir, photos, placed)) => {
            let mut written: Vec<PathBuf> = Vec::new();
            for (name, data) in &photos {
                let Some((path, true)) = placed.get(&format!("{PHOTOS}/{name}")) else { continue };
                if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(path, data)) {
                    for done in &written {
                        let _ = std::fs::remove_file(done);
                    }
                    return Err(format!("{}: {e}; no photo was brought back", path.display()));
                }
                written.push(path.clone());
            }
            put = written.len();
            Some(dir.display().to_string())
        }
        None => None,
    };
    let said = |k: &str| done.get(k).and_then(Value::as_u64).unwrap_or(0);
    let mut summary = format!(
        "Brought the field's edits back to offline area '{area}': {} feature(s) changed, {} added, {} deleted",
        said("updated"),
        said("added"),
        said("deleted")
    );
    if let Some(dir) = &folder {
        summary.push_str(&format!("; {put} photo(s) put in {dir}"));
        if renamed > 0 {
            summary.push_str(&format!(" ({renamed} under a new name: one of its name was there already)"));
        }
    }
    if !reference.is_empty() {
        s.notify(
            format!(
                "Bring Back skipped {} layer(s) copied from sources read in place at home. Any edits to them stay in the field project: {}",
                reference.len(),
                reference.join(", ")
            ),
            false,
        );
    }
    if !unowned.is_empty() {
        s.notify(
            format!(
                "Bring Back skipped {} layer(s) that aren't in offline area '{area}' (added in the field). Their features stay in the field project: {}",
                unowned.len(),
                unowned.join(", ")
            ),
            false,
        );
    }
    if unchanged_photos > 0 {
        s.notify(
            format!(
                "Bring Back left out {unchanged_photos} photo(s) that no change it brings back names (photos the field didn't change, or of features deleted here): home's are kept"
            ),
            false,
        );
    }
    if skipped_photos > 0 {
        s.notify(format!("Bring Back skipped {skipped_photos} photo(s) without a feature in the layers being brought back"), false);
    }
    if strange > 0 {
        s.notify(format!("{strange} photo(s) in the field package weren't brought back: their names aren't plain file names"), true);
    }
    if legacy_area.is_some() {
        s.notify("Legacy field return: applied to the explicitly selected home offline area with an unverified origin; create a fresh field package for further work", false);
    }
    s.notify(summary, false);
    if let Some(o) = done.as_object_mut() {
        o.insert("area".into(), json!(area));
        o.insert("photos".into(), json!(put));
        o.insert("photoFolder".into(), json!(folder));
    }
    Ok(done)
}

/// Adds to offline area `ai`'s source layers, and to the area's own copy of them (so features
/// aren't all taken as edited for lacking it), the fields `edited`'s layers have that neither has:
/// a field the field added (a layer's photos field, made with its first photo). A field the area's
/// copy has and the source hasn't was removed at home, and isn't put back. Returns them as
/// `layer: field`.
fn take_new_fields(s: &mut Session, ai: usize, edited: &rustgis_map::Map) -> Vec<String> {
    let Some(area) = s.project.offline_areas.get(ai) else { return Vec::new() };
    let source = area.map.clone();
    let Some(si) = s.project.maps.iter().position(|m| m.name.eq_ignore_ascii_case(&source)) else { return Vec::new() };
    let mut added = Vec::new();
    for ol in edited.layers.iter().filter(|l| !l.is_raster()) {
        let Some(sl) = s.project.maps.get(si).and_then(|m| m.layers.iter().find(|l| l.id == ol.id && !l.is_raster())) else { continue };
        let base = s.project.offline_areas.get(ai).and_then(|a| a.data.layers.iter().find(|l| l.id == ol.id));
        let extra: Vec<rustgis_data::Field> = ol
            .dataset
            .fields
            .iter()
            .filter(|f| sl.dataset.field_index(&f.name).is_none() && base.is_none_or(|b| b.dataset.field_index(&f.name).is_none()))
            .cloned()
            .collect();
        if extra.is_empty() {
            continue;
        }
        let name = sl.name.clone();
        if let Some(l) = s.project.maps.get_mut(si).and_then(|m| m.layers.iter_mut().find(|l| l.id == ol.id)) {
            for f in &extra {
                l.dataset.ensure_field(&f.name, f.ty);
            }
        }
        if let Some(b) = s.project.offline_areas.get_mut(ai).and_then(|a| a.data.layers.iter_mut().find(|l| l.id == ol.id)) {
            for f in &extra {
                b.dataset.ensure_field(&f.name, f.ty);
            }
        }
        added.extend(extra.iter().map(|f| format!("{name}: {}", f.name)));
    }
    added
}

/// The folder a field package named `stem` is unpacked into: the app's own `field/<stem>`.
fn unpack_dir(stem: &str) -> Result<PathBuf, String> {
    let base =
        rustgis_data::dirs::app_data_dir().ok_or("this computer gives RustGIS no folder of its own to unpack the field package into")?;
    Ok(fresh_dir(&base.join("field"), &super::items::safe(stem)))
}

/// A folder in `base` named `name`, or `name 2`, `name 3`… when one is there: a copy unpacked
/// before (another package of the same name, perhaps still open) is never written over.
fn fresh_dir(base: &Path, name: &str) -> PathBuf {
    let mut dir = base.join(name);
    for n in 2.. {
        if !dir.exists() {
            break;
        }
        dir = base.join(format!("{name} {n}"));
    }
    dir
}

/// Unpacks a field package's `bytes` into a folder of its own in the app's folder (`field/<name>`,
/// numbered when that is taken) and returns the folder, project text and construction notices. An entry that
/// would land outside the folder is refused, and the package with it.
///
/// The project's own paths are trusted as a project file's are: a package from someone else can
/// name files outside the folder, as a `.rgis` can.
pub(crate) fn unpack(bytes: &[u8], stem: &str) -> Result<(PathBuf, String, Vec<String>), String> {
    let dir = unpack_dir(stem)?;
    let (text, notes) = unpack_into(bytes, &dir)?;
    Ok((dir, text, notes))
}

/// Unpacks a field package's `bytes` into `dir`, a folder not there yet (see [`unpack`]), and
/// returns the project text and its optional notices. A write that fails removes what was written.
pub(crate) fn unpack_into(bytes: &[u8], dir: &Path) -> Result<(String, Vec<String>), String> {
    let entries = rustgis_data::zip::read(bytes)?;
    let mut project = None;
    let mut manifest = None;
    let mut files = Vec::new();
    for (name, data) in entries {
        let parts: Vec<&str> = name.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() || parts.iter().any(|p| *p == ".." || *p == "." || p.contains(':')) || name.starts_with(['/', '\\']) {
            return Err(format!("the field package holds an entry outside its folder: {name}"));
        }
        if parts.len() == 1 && crate::project::is_project_file(&name) {
            if project.is_some() {
                return Err("the field package holds more than one project".into());
            }
            project = Some(String::from_utf8(data).map_err(|_| "the field package's project is damaged".to_string())?);
        } else {
            if parts.as_slice() == [LEFT_BEHIND] {
                if manifest.is_some() {
                    return Err("the field package holds more than one list of what stayed behind".into());
                }
                manifest = Some(read_manifest(&data));
            }
            files.push((parts.iter().collect::<PathBuf>(), data));
        }
    }
    let text = project.ok_or("the field package holds no project")?;
    // Checked before anything is written: a project that can't be read leaves no folder behind.
    // Nothing it names is read here; that waits for its hold ([`project_in_within`]).
    Project::check_import_json(&text, Some(dir))?;
    if dir.exists() {
        return Err(format!("{}: the folder is already there", dir.display()));
    }
    let written = (|| {
        for (rel, data) in files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&path, data).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok::<(), String>(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(dir);
        return Err(e);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok((text, manifest.unwrap_or_default()))
}

/// A missing manifest is normal for an old package; a damaged manifest leaves the map usable
/// and gets a notice of its own.
fn read_manifest(bytes: &[u8]) -> Vec<String> {
    if bytes.len() <= MAX_MANIFEST
        && let Ok(notes) = serde_json::from_slice::<Vec<String>>(bytes)
        && notes.len() <= MAX_NOTICES + 1
    {
        return notes;
    }
    vec!["The field package's list of what stayed behind could not be read".into()]
}

/// Reads a field package with paths to its unpacked basemaps and photos. Absolute paths to
/// photos stay usable after opening a recovery copy, which has no unpack directory of its own.
/// References that cannot store an absolute path are left relative and reported on open. A package
/// that went out from home has the rows it gives taken (`Project::given`), for
/// `offline.keepRows` to keep on this device. What the project names is held within `reach` before
/// anything it names is read ([`Project::from_import_json_within`]).
pub(crate) fn project_in_within(
    text: &str,
    dir: &Path,
    reach: crate::render::rustgis_tiles::address::Reach,
) -> Result<(Project, crate::received::Named), String> {
    let (mut project, named) = Project::from_import_json_within(text, Some(dir), reach)?;
    let photos = std::path::absolute(dir.join(PHOTOS)).map_err(|e| e.to_string())?;
    let mut relative = 0_usize;
    // Photos from home that keep their names there, the unpack folder's name not being text.
    let mut unshown = std::collections::HashSet::new();
    // Photos from home the index lists that the package doesn't hold.
    let mut absent = std::collections::HashSet::new();
    if photos.is_dir() {
        // A package from home names its photos where they are at home; its index says which entry
        // each is.
        let index = match unpacked_index(&photos, project.field_origin.as_ref().map_or("", |o| o.package.as_str())) {
            Ok(index) => index,
            Err(e) => {
                project.read_notices.push((
                    crate::project::ReadPart::Maps,
                    crate::notice_text::NoticeText::format(
                        "The field package's photo index couldn't be read ({reason}): its photos keep the names they have at home, which may not be on this device.",
                        [("reason", e.into())],
                    ),
                ));
                Default::default()
            }
        };
        // An entry the index lists belongs to the home path it names, and to nothing else: a
        // feature naming the entry itself isn't pointed at that photo. A streamed layer's copy for
        // reference can: its rows are read after the photo names are given (`collect_photos` skips
        // streamed layers), so a home row naming `photos/home-00001.jpg` isn't kept clear of them.
        let listed: std::collections::HashSet<String> = index.keys().cloned().collect();
        let at_home: std::collections::HashMap<String, String> = index.into_iter().map(|(entry, named)| (named, entry)).collect();
        for map in &mut project.maps {
            rewrite_photos(map, |path| {
                if listed.contains(path) {
                    return path.to_string();
                }
                // A return's photo is pointed at its unpacked file by its full path, or by its entry,
                // relative to the package, where the unpack folder's name can't be held as text. One
                // named where it is at home is pointed there by its full path only: otherwise it
                // keeps its name at home, so the way back can't take its name here for an edit.
                let (named, entry) = (path, at_home.get(path).map_or(path, String::as_str));
                if let Some(name) = entry.strip_prefix("photos/").filter(|name| plain_file(name)) {
                    let file = photos.join(name);
                    // A photo the index lists that isn't there: the feature keeps its name at home.
                    if named != entry && !file.is_file() {
                        absent.insert(named.to_string());
                    }
                    if file.is_file() {
                        if let Some(path) = file.to_str() {
                            return path.to_string();
                        }
                        if named != entry {
                            unshown.insert(named.to_string());
                            return named.to_string();
                        }
                        relative += 1;
                    }
                }
                named.to_string()
            });
        }
    }
    // The rows as given, taken (sharing them) for `offline.keepRows` to keep, when this package
    // went out from home: a return opened here keeps none.
    project.given = super::field_changes::GivenRows::in_package(dir, &project).map(std::sync::Arc::new);
    if !absent.is_empty() {
        project.read_notices.push((
            crate::project::ReadPart::Maps,
            crate::notice_text::NoticeText::format(
                "{count} photo(s) the field package lists aren't in it: their features keep the names they have at home, which may not be on this device.",
                [("count", absent.len().to_string().into())],
            ),
        ));
    }
    if !unshown.is_empty() {
        project.read_notices.push((
            crate::project::ReadPart::Maps,
            crate::notice_text::NoticeText::format(
                "{count} photo(s) the field package brought from home keep the names they have there, because the unpack folder is not UTF-8: they don't show on this device.",
                [("count", unshown.len().to_string().into())],
            ),
        ));
    }
    if relative > 0 {
        project.read_notices.push((
            crate::project::ReadPart::Maps,
            crate::notice_text::NoticeText::format(
                "{count} unpacked field photo reference(s) kept their relative paths because the unpack folder is not UTF-8. They may stop resolving after Save As or recovery.",
                [("count", relative.to_string().into())],
            ),
        ));
    }
    Ok((project, named))
}

/// [`project_in_within`] for a package of the user's own.
#[cfg(test)]
pub(crate) fn project_in(text: &str, dir: &Path) -> Result<Project, String> {
    project_in_within(text, dir, crate::render::rustgis_tiles::address::Reach::Local).map(|(p, _)| p)
}

/// Removes the offline areas past the first `had` of this project: those a field package just made,
/// when it wasn't saved after all (the save was cancelled or failed). Says so.
pub fn forget_new_areas(s: &mut Session, had: usize) {
    let made: Vec<String> = s.project.offline_areas.iter().skip(had).map(|a| a.name.clone()).collect();
    for name in made {
        s.project.offline_areas.retain(|a| a.name != name);
        s.notify(format!("The field package wasn't saved, so its offline area '{name}' was removed too"), false);
    }
}

/// Refuses `path` when it is the open project's own file (or the field package it was opened
/// from): written there, the package would replace the project, as an export there is refused.
pub(crate) fn not_the_project(s: &Session, path: &Path) -> Result<(), String> {
    let same = |a: &Path, b: &Path| a == b || a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b);
    if s.path.iter().chain(s.opened_package.iter()).any(|project| same(project, path)) {
        return Err("the field package can't go in the open project's own file: choose another".into());
    }
    Ok(())
}

/// Whether `path` is a file holding `data`: its size is looked at first, and no more than that is
/// read, so a device or a pipe named in a package's index isn't read without end.
fn holds(path: &Path, data: &[u8]) -> bool {
    use std::io::Read;
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() || meta.len() != data.len() as u64 {
        return false;
    }
    let mut there = Vec::with_capacity(data.len());
    rustgis_data::provenance::open(path).and_then(|f| f.take(data.len() as u64 + 1).read_to_end(&mut there)).is_ok() && there == data
}

/// What `offline.package` returns.
pub(crate) fn packaged(path: &str, area: &str, bytes: usize) -> Value {
    json!({"path": path, "area": area, "bytes": bytes})
}

#[cfg(test)]
#[path = "field_audit_tests.rs"]
mod audit;

#[expect(clippy::disallowed_methods, reason = "a thread the system refuses fails the test, as it should")]
#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_map::tiles::{TileKind, TileLayer};

    /// A folder of the test's own, removed when it ends.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let dir = std::env::temp_dir().join(format!("rustgis-field-{name}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample() -> Session {
        Session::new(crate::sample::world_project())
    }

    thread_local! {
        pub(super) static UNPACKED: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    }

    /// Runs `f` with the unpacked limit of a package at `most` on this thread; put back after.
    fn with_unpacked<T>(most: u64, f: impl FnOnce() -> T) -> T {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                UNPACKED.with(|m| m.set(None));
            }
        }
        UNPACKED.with(|m| m.set(Some(most)));
        let _restore = Restore;
        f()
    }

    /// Photos the map needs the room of stay home, and that is said: a package that fit without
    /// them still goes out.
    #[test]
    fn photos_the_map_needs_the_room_of_stay_home() {
        let temp = Temp::new("photo-room");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let area = json!({"name": "Room", "extent": [-180.0, -85.0, 180.0, 85.0]});
        let map_bytes = |bytes: &[u8]| {
            rustgis_data::zip::read(bytes).unwrap().into_iter().find(|(n, _)| n.ends_with(".rgis")).map(|(_, b)| b.len() as u64).unwrap()
        };
        let json = map_bytes(&package(&mut home, &area).unwrap().1);
        // A photo that fits the budget for archives and photos (half the limit), but not beside the map.
        let photo = temp.path().join("big.jpg");
        std::fs::write(&photo, vec![0u8; (json * 6 / 10) as usize]).unwrap();
        home.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
        let mark = home.notice_mark();
        let out = with_unpacked(json * 14 / 10, || package(&mut home, &json!({"name": "Room"}))).unwrap().1;
        let entries = rustgis_data::zip::read(&out).unwrap();
        assert!(entries.iter().all(|(n, _)| !n.starts_with("photos/")));
        let said: Vec<String> = home.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.starts_with("1 photo(s) stayed home: the map needed")), "{said:?}");
        // With room for both, it goes.
        let out = package(&mut home, &json!({"name": "Room"})).unwrap().1;
        assert!(rustgis_data::zip::read(&out).unwrap().iter().any(|(n, _)| n == "photos/home-00001.jpg"));
    }

    #[test]
    fn companions_do_not_reduce_the_project_json_limit() {
        crate::project::with_max_bytes(1024, || {
            assert_eq!(project_room(128), 1024);
            assert_eq!(project_room(MOST_UNPACKED as u64 - 64), 64);
            assert_eq!(project_room(MOST_UNPACKED as u64 + 1), 0);
        });
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_photo_is_a_sender_error_and_a_receiver_notice() {
        let temp = Temp::new("unreadable-photo");
        let path = temp.path().join("unreadable.jpg");
        std::os::unix::fs::symlink("unreadable.jpg", &path).unwrap();
        let mut home = sample();
        let (_, bytes) = package(&mut home, &json!({"name":"Site", "extent":[-180, -85, 180, 85]})).unwrap();
        let dir = temp.path().join("out");
        let (text, _) = unpack_into(&bytes, &dir).unwrap();
        let mut crew = Session::new(project_in(&text, &dir).unwrap());
        crew.run("edit.addField", &json!({"layer":"Cities", "name":"photos", "type":"text"})).unwrap();
        crew.run("edit.setValue", &json!({"layer":"Cities", "row":0, "field":"photos", "value":path})).unwrap();

        let mark = crew.notice_mark();
        let (_, bytes) = package(&mut crew, &Value::Null).unwrap();
        let expected = "1 photo(s) couldn't be put in the field package";
        assert_eq!(crew.notices_since(mark).iter().filter(|n| n.error && n.text.contains(expected)).count(), 1);
        let (_, losses) = unpack_into(&bytes, &temp.path().join("back")).unwrap();
        assert!(losses.iter().any(|note| note.contains(expected)));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn stream_budget_is_in_bytes_on_a_constrained_host() {
        const TEST: &str = "cmd::field::tests::stream_budget_is_in_bytes_on_a_constrained_host";
        const CHILD: &str = "RUSTGIS_TEST_FIELD_BUDGET";
        if std::env::var_os(CHILD).is_some() {
            // A 256 MiB host gives the job 64 MiB, and the package has room for it.
            assert_eq!(stream_budget(256 << 20), 64 << 20);
            assert_eq!(stream_budget(32 << 20), 16 << 20);
            return;
        }
        // The memory override is read once per process. Keep it out of concurrent tests.
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST])
            .env(CHILD, "1")
            .env(rustgis_data::memory::OVERRIDE, (256 << 20).to_string())
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stdout));
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn refused_field_export_leaves_the_destination_as_it_was() {
        use crate::cmd::exports::{export_land, export_start};
        let temp = Temp::new("refused");
        let mut home = sample();
        for existing in [false, true] {
            let path = temp.path().join(format!("{existing}.rgisfield"));
            if existing {
                std::fs::write(&path, b"previous package").unwrap();
            }
            let params = json!({"path": path, "name": "Site", "extent": [-20, -40, 60, 40]});
            let run = export_start(&mut home, "offline.package", &params).unwrap();
            let started = run.started();
            let wrote = run.run(&rustgis_analysis::control::Control::new());
            assert!(wrote.is_ok());
            home.run("map.rename", &json!({"name": format!("Changed {existing}")})).unwrap();
            let err = export_land(&mut home, started, wrote).unwrap_err();
            assert!(err.contains("the home project changed"), "{err}");
            assert!(home.project.offline_areas.is_empty());
            if existing {
                assert_eq!(std::fs::read(&path).unwrap(), b"previous package");
            } else {
                assert!(!path.exists());
            }
            assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), usize::from(existing));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn archive_losses_are_sender_errors_and_receiver_notices() {
        use crate::cmd::exports::{export_land, export_start};
        let temp = Temp::new("archive-loss");
        for (background, write_succeeds) in [(false, true), (false, false), (true, true), (true, false)] {
            let mut home = sample();
            let missing = temp.path().join("missing.mbtiles");
            home.map_mut().tiles.push(TileLayer::new("Unread", TileKind::MbTiles, missing.display().to_string()));
            home.map_mut().tiles.push(TileLayer::new("Uploaded", TileKind::MbTiles, format!("{}site.mbtiles", rustgis_map::tiles::UPLOAD)));
            let path = if write_succeeds { temp.path().join("site.rgisfield") } else { temp.path().join("missing/site.rgisfield") };
            let mark = home.notice_mark();
            let params = json!({"path": path, "name": "Site", "extent": [-20, -40, 60, 40]});
            let result = if background {
                let run = export_start(&mut home, "offline.package", &params).unwrap();
                let started = run.started();
                export_land(&mut home, started, run.run(&rustgis_analysis::control::Control::new()))
            } else {
                home.run("offline.package", &params)
            };
            assert_eq!(result.is_ok(), write_succeeds, "{result:?}");
            for name in ["Unread", "Uploaded"] {
                assert!(home.notices_since(mark).iter().any(|n| n.error && n.text.contains(name)));
            }
            if write_succeeds {
                let (_, losses) = unpack_into(&std::fs::read(&path).unwrap(), &temp.path().join(format!("field-{background}"))).unwrap();
                for name in ["Unread", "Uploaded"] {
                    assert!(losses.iter().any(|n| n.contains(name)));
                }
            }
        }
    }

    #[test]
    fn a_map_packaged_for_the_field_opens_with_its_basemap_and_comes_from_an_offline_area() {
        let temp = Temp::new("package");
        let archive = temp.path().join("site.mbtiles");
        std::fs::write(&archive, b"stand-in archive bytes").unwrap();
        let mut s = sample();
        s.map_mut().tiles.push(TileLayer::new("Site", TileKind::MbTiles, archive.display().to_string()));
        s.map_mut().tiles.push(TileLayer::new("Streets", TileKind::Raster, "https://tiles.example.com/{z}/{x}/{y}.png"));
        let mark = s.notice_mark();
        let (area, bytes) = package(&mut s, &json!({"name": "Survey", "extent": [-20.0, -40.0, 60.0, 40.0]})).unwrap();
        assert_eq!(area, "Survey");
        // The offline area is in this project, to bring the edits back to.
        assert!(s.project.offline_areas.iter().any(|a| a.name == "Survey"));
        // The online tile layer is said; so is the area made.
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.contains("Streets") && t.contains("online")), "{said:?}");
        assert!(said.iter().any(|t| t.contains("offline area 'Survey'")), "{said:?}");
        // Clipped to the extent: the features left outside it are said.
        assert!(said.iter().any(|t| t.contains("outside the offline area's extent")), "{said:?}");
        // The package: the project, the archive under basemaps/, a README.
        let entries = rustgis_data::zip::read(&bytes).unwrap();
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Survey.rgis", "basemaps/site.mbtiles", "README.txt", LEFT_BEHIND, crate::cmd::field_changes::GIVEN]);
        assert_eq!(entries[1].1, b"stand-in archive bytes");
        // Unpacked and read: one map, the area's, its features in it, its basemap read from beside it.
        let into = temp.path().join("unpacked");
        let text = unpack_into(&bytes, &into).unwrap().0;
        let project = project_in(&text, &into).unwrap();
        assert_eq!(project.maps.len(), 1);
        let map = &project.maps[0];
        assert_eq!(map.name, "Survey (offline)");
        assert!(map.layers.iter().any(|l| !l.dataset.features.is_empty()));
        let base = map.tiles.iter().find(|t| t.name == "Site").unwrap();
        assert_eq!(Path::new(&base.url), into.join("basemaps/site.mbtiles"));
        assert!(into.join("basemaps/site.mbtiles").is_file());
        // Unpacked again: into a folder of its own, the first left as it is.
        assert!(unpack_into(&bytes, &into).unwrap_err().contains("already there"));
        let again = fresh_dir(temp.path(), "unpacked");
        assert_eq!(again, temp.path().join("unpacked 2"));
        unpack_into(&bytes, &again).unwrap();
        assert!(into.join("basemaps/site.mbtiles").is_file() && again.join("basemaps/site.mbtiles").is_file());
    }

    #[test]
    fn field_package_carries_streamed_features_after_the_source_is_gone() {
        let temp = Temp::new("stream");
        let source = temp.path().join("places.gpkg");
        let mut seed = Session::new(Project::default());
        let mut places = rustgis_data::Dataset::new("Places", rustgis_data::GeometryType::Point, vec![]);
        places.crs = rustgis_data::raster::Crs::from_epsg(4326);
        for x in [1.0, 10.0] {
            places.push(Some(rustgis_data::Geometry::Point(rustgis_data::Coord::new(x, 1.0))), vec![]);
        }
        let id = seed.map_mut().add_dataset(places.clone());
        seed.run("share.exportLayer", &json!({"layer": id, "path": source})).unwrap();

        let mut home = Session::new(Project::default());
        home.run("layer.addData", &json!({"name": "Streamed", "path": source, "layer": "Places", "stream": true})).unwrap();
        assert!(home.map().layer_by_name("Streamed").unwrap().is_streamed());
        home.map_mut().add_dataset(places);
        let mark = home.notice_mark();
        let extent = rustgis_data::Extent::new(0.0, 0.0, 2.0, 2.0);
        let mut oversized = home.map().clone();
        let mut omissions = rustgis_data::loss::Notes::new();
        assert!(copy_streams(&mut oversized, extent, 0, &mut omissions).unwrap().is_empty());
        assert!(oversized.layer_by_name("Streamed").is_none());
        assert!(omissions.iter().any(|n| n.contains("1 feature(s) in the offline area") && n.contains("budget")), "{omissions:?}");

        let (_, bytes) = package(&mut home, &json!({"name": "Site", "extent": [0, 0, 2, 2]})).unwrap();
        assert!(home.notices_since(mark).iter().any(|n| n.text.contains("copied for reference")));
        let stem = temp.path().file_name().unwrap().to_str().unwrap();
        let path = temp.path().join(format!("{stem}.rgisfield"));
        std::fs::write(&path, bytes).unwrap();
        let _unpacked = Temp(unpack_dir(stem).unwrap());
        let mut crew = Session::new(Project::default());
        let held = source.with_extension("held");
        // Hide the original source before opening on the receiving session. SQLite may
        // keep the file open on Windows, so drop the live session and use its saved state.
        let home_text = serde_json::to_string(&home.project).unwrap();
        drop(oversized);
        drop(home);
        std::fs::rename(&source, &held).unwrap();
        let mark = crew.notice_mark();
        crew.run("project.open", &json!({"path": path})).unwrap();
        let copy = crew.map().layer_by_name("Streamed").unwrap();
        assert_eq!(copy.dataset.features.len(), 1, "the crew should have the in-area feature");
        assert!(!copy.is_streamed());
        assert!(crew.notices_since(mark).iter().any(|n| n.text.contains("copied for reference")));
        assert!(crew.notices_since(mark).iter().all(|n| !n.text.contains("Made the offline area")));

        std::fs::rename(&held, &source).unwrap();
        let mut home = Session::new(Project::from_json(&home_text).unwrap());
        crew.run("edit.addFeature", &json!({"layer":"Streamed", "geometry":{"type":"Point", "coordinates":[1.5, 1.5]}})).unwrap();
        let photo = temp.path().join("reference.jpg");
        let shared_photo = temp.path().join("shared.jpg");
        std::fs::write(&photo, b"reference photo").unwrap();
        std::fs::write(&shared_photo, b"shared photo").unwrap();
        crew.run("edit.addField", &json!({"layer": "Streamed", "name": "photos", "type": "text"})).unwrap();
        let named = format!("{}; {}", photo.display(), shared_photo.display());
        crew.run("edit.setValue", &json!({"layer": "Streamed", "row": 0, "field": "photos", "value": named})).unwrap();
        crew.run("edit.addField", &json!({"layer": "Places", "name": "photos", "type": "text"})).unwrap();
        crew.run("edit.setValue", &json!({"layer": "Places", "row": 0, "field": "photos", "value": shared_photo})).unwrap();
        home.path = Some(temp.path().join("home.rgis"));
        let (_, bytes) = package(&mut crew, &Value::Null).unwrap();
        assert!(rustgis_data::zip::read(&bytes).unwrap().iter().any(|(name, _)| name == "photos/reference.jpg"));
        std::fs::remove_file(&photo).unwrap();
        std::fs::remove_file(&shared_photo).unwrap();
        let returned = temp.path().join("returned.rgisfield");
        std::fs::write(&returned, bytes).unwrap();
        let mark = home.notice_mark();
        let done = home.run("offline.bringBack", &json!({"path": returned})).unwrap();
        assert_eq!(done["added"], 0);
        assert_eq!(done["photos"], 1);
        let photos = temp.path().join("Site photos");
        assert!(!photos.join("reference.jpg").exists());
        assert_eq!(std::fs::read(photos.join("shared.jpg")).unwrap(), b"shared photo");
        assert!(home.notices_since(mark).iter().any(|n| n.text.contains("Bring Back skipped 1 photo")));
        assert!(home.notices_since(mark).iter().any(|n| n.text.contains("Bring Back skipped 1 layer")));
        assert!(home.map().layer_by_name("Streamed").unwrap().is_streamed());
    }

    /// A same-length rewrite can move a point into the area without invalidating its byte offset.
    #[test]
    fn field_package_reports_a_streamed_geojson_changed_since_opening() {
        let temp = Temp::new("changed-geojson");
        let source = temp.path().join("places.geojson");
        let write_point = |x| {
            let data = json!({
                "type": "FeatureCollection",
                "features": [{"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [x, 1]}}],
            });
            std::fs::write(&source, data.to_string()).unwrap();
        };
        write_point(9);
        let mut home = Session::new(Project::default());
        home.run("layer.addData", &json!({"name": "Places", "path": source, "stream": true})).unwrap();
        assert!(home.map().layer_by_name("Places").unwrap().is_streamed());

        let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
        write_point(1);
        // Give the rewrite a distinct timestamp even on filesystems with coarse timestamps.
        std::fs::File::options().write(true).open(&source).unwrap().set_modified(modified + std::time::Duration::from_secs(4)).unwrap();

        let mark = home.notice_mark();
        let (_, bytes) = package(&mut home, &json!({"name": "Site", "extent": [0, 0, 2, 2]})).unwrap();
        let into = temp.path().join("unpacked");
        let (text, notices) = unpack_into(&bytes, &into).unwrap();
        let project = project_in(&text, &into).unwrap();
        assert!(project.maps[0].layer_by_name("Places").is_none(), "the stale index included an empty reference layer");
        let notice = |text: &str| {
            text.contains("Places")
                && text.contains("1 streamed layer")
                && text.contains("changed on disk since it was opened. Reopen the layer")
        };
        assert!(notices.iter().any(|n| notice(n)), "{notices:?}");
        assert!(home.notices_since(mark).iter().any(|n| notice(&n.text)));
    }

    #[test]
    fn a_field_package_holds_only_its_map_and_a_basemap_of_any_name_is_read() {
        let temp = Temp::new("only");
        // A name a file can have anywhere but Windows, which a package's folders couldn't hold as
        // it is. Windows refuses `:`, `\`, `*` and `?` in a file name, so there the name has
        // characters it allows that a package still renames, the same number of them.
        let name = if cfg!(windows) { "site#a&b%é.mbtiles" } else { "site:a\\b*?.mbtiles" };
        let archive = temp.path().join(name);
        std::fs::write(&archive, b"stand-in archive bytes").unwrap();
        let mut s = sample();
        s.map_mut().tiles.push(TileLayer::new("Site", TileKind::MbTiles, archive.display().to_string()));
        // What else the project holds stays at home.
        s.project
            .tables
            .push(crate::project::Table::new(7, rustgis_data::Dataset::new("Inventory", rustgis_data::GeometryType::None, Vec::new())));
        s.project.folders.push("/home/someone/data".into());
        let mark = s.notice_mark();
        let (_, bytes) = package(&mut s, &json!({"name": "Survey", "extent": [-20.0, -40.0, 60.0, 40.0]})).unwrap();
        // What stayed at home is said.
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(
            said.iter().any(|t| t.contains("stayed in this project") && t.contains("1 table(s)") && t.contains("1 folder(s)")),
            "{said:?}"
        );
        let names: Vec<String> = rustgis_data::zip::read(&bytes).unwrap().into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"basemaps/site_a_b__.mbtiles".to_string()), "{names:?}");
        let into = temp.path().join("unpacked");
        let project = project_in(&unpack_into(&bytes, &into).unwrap().0, &into).unwrap();
        assert!(project.tables.is_empty() && project.folders.is_empty() && project.offline_areas.is_empty());
        assert_eq!(project.maps.len(), 1);
        let base = project.maps[0].tiles.iter().find(|t| t.name == "Site").unwrap();
        assert_eq!(Path::new(&base.url), into.join("basemaps/site_a_b__.mbtiles"));
        assert!(into.join("basemaps/site_a_b__.mbtiles").is_file());
    }

    #[test]
    fn a_field_package_not_saved_leaves_no_offline_area() {
        let mut s = sample();
        let had = s.project.offline_areas.len();
        // A place that can't be written: the command is refused, and the area made for it goes.
        let e = s.run("offline.package", &json!({"path": "/no/such/folder/site.rgisfield", "name": "Lost"})).unwrap_err();
        assert!(e.contains("site.rgisfield"), "{e}");
        assert_eq!(s.project.offline_areas.len(), had);
        // As the front end does when its Save is cancelled: the area goes, and that is said.
        package(&mut s, &json!({"name": "Cancelled"})).unwrap();
        assert_eq!(s.project.offline_areas.len(), had + 1);
        let mark = s.notice_mark();
        forget_new_areas(&mut s, had);
        assert_eq!(s.project.offline_areas.len(), had);
        assert!(s.notices_since(mark).iter().any(|n| n.text.contains("'Cancelled' was removed")));
        // Not over the open project's own file.
        let temp = Temp::new("own");
        let own = temp.path().join("home.rgis");
        std::fs::write(&own, b"{}").unwrap();
        s.path = Some(own.clone());
        let e = s.run("offline.package", &json!({"path": own.display().to_string(), "name": "Own"})).unwrap_err();
        assert!(e.contains("open project's own file"), "{e}");
        assert_eq!(std::fs::read(&own).unwrap(), b"{}");
        // A project of the one map and nothing else: nothing is said to have stayed.
        let home = &mut s.project;
        home.maps.truncate(1);
        home.active_map = 0;
        home.tables.clear();
        home.notebooks.clear();
        home.models.clear();
        home.layouts.clear();
        home.reports.clear();
        home.presentations.clear();
        home.workspaces.clear();
        home.dl_models.clear();
        let mark = s.notice_mark();
        package(&mut s, &json!({"name": "Alone"})).unwrap();
        assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("stayed in this project")));
    }

    #[test]
    fn what_a_package_leaves_behind_is_said_and_a_clean_one_says_nothing() {
        let temp = Temp::new("notices");
        let archive = temp.path().join("base.pmtiles");
        std::fs::write(&archive, b"tiles").unwrap();
        // Clean: one local archive, no raster, no web layer: no notice of anything left behind.
        let mut s = sample();
        s.map_mut().layers.retain(|l| !l.is_raster());
        s.map_mut().tiles.push(TileLayer::new("Base", TileKind::PmTiles, archive.display().to_string()));
        let mark = s.notice_mark();
        let (_, clean) = package(&mut s, &json!({"name": "Clean", "extent": [-180, -90, 180, 90]})).unwrap();
        let (_, clean_notes) = unpack_into(&clean, &temp.path().join("clean")).unwrap();
        assert!(clean_notes.is_empty(), "{clean_notes:?}");
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().all(|t| !t.contains("left out") && !t.contains("from the web") && !t.contains("couldn't be put")), "{said:?}");
        // An archive that can't be read, and a raster layer: each said.
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
        let gt = GeoTransform { origin_x: 10.0, origin_y: 10.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r =
            Raster::new("relief", 2, 2, vec![vec![1.0; 4]], gt, Crs::from_epsg(4326).unwrap(), None, ColorHint::Gray, "Float32").unwrap();
        let tif = temp.path().join("relief.tif");
        std::fs::write(&tif, geotiff::write(&r, 0).unwrap()).unwrap();
        s.run("layer.addData", &json!({"path": tif.display().to_string()})).unwrap();
        assert!(s.map().layers.iter().any(|l| l.is_raster()));
        s.map_mut().tiles.push(TileLayer::new("Gone", TileKind::MbTiles, temp.path().join("gone.mbtiles").display().to_string()));
        let mark = s.notice_mark();
        let (_, lossy) = package(&mut s, &json!({"name": "Lossy"})).unwrap();
        let (_, losses) = unpack_into(&lossy, &temp.path().join("lossy")).unwrap();
        assert!(losses.iter().any(|t| t.contains("couldn't be put") && t.contains("Gone")));
        assert!(losses.iter().any(|t| t.contains("raster layer(s) were left out")));
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.contains("couldn't be put") && t.contains("Gone")), "{said:?}");
        assert!(said.iter().any(|t| t.contains("raster layer(s) were left out")), "{said:?}");
    }

    #[test]
    fn a_returning_maps_rasters_and_a_browsers_archives_are_said() {
        let temp = Temp::new("returning");
        let mut s = sample();
        s.run("offline.download", &json!({"name": "Back", "extent": [-20.0, -40.0, 60.0, 40.0]})).unwrap();
        s.run("offline.open", &json!({"name": "Back"})).unwrap();
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
        let gt = GeoTransform { origin_x: 10.0, origin_y: 10.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r =
            Raster::new("relief", 2, 2, vec![vec![1.0; 4]], gt, Crs::from_epsg(4326).unwrap(), None, ColorHint::Gray, "Float32").unwrap();
        let tif = temp.path().join("relief.tif");
        std::fs::write(&tif, geotiff::write(&r, 0).unwrap()).unwrap();
        s.run("layer.addData", &json!({"path": tif.display().to_string()})).unwrap();
        s.map_mut().tiles.push(TileLayer::new("Uploaded", TileKind::MbTiles, format!("{}base.mbtiles", rustgis_map::tiles::UPLOAD)));
        let mark = s.notice_mark();
        let (_, bytes) = package(&mut s, &Value::Null).unwrap();
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.contains("raster layer(s) were left out")), "{said:?}");
        assert!(said.iter().any(|t| t.contains("Uploaded") && t.contains("memory")), "{said:?}");
        assert!(!said.iter().any(|t| t.contains("from the web")), "{said:?}");
        // The package holds no raster layer.
        let text = rustgis_data::zip::read(&bytes).unwrap().into_iter().find(|(n, _)| n.ends_with(".rgis")).unwrap().1;
        let project = Project::from_import_json(&String::from_utf8(text).unwrap(), None).unwrap();
        assert!(project.maps[0].layers.iter().all(|l| !l.is_raster()));
    }

    #[test]
    fn a_map_only_named_offline_is_packaged_as_any_other() {
        let mut s = sample();
        s.run("map.rename", &json!({"name": "Plan (offline)"})).unwrap();
        let had = s.project.offline_areas.len();
        // A new area, named past the map that already holds "Plan (offline)".
        let (area, _) = package(&mut s, &json!({"name": "Plan"})).unwrap();
        assert_eq!(area, "Plan 2");
        assert_eq!(s.project.offline_areas.len(), had + 1);
        // The whole world: every feature is in it, and nothing is said to have stayed out.
        let mark = s.notice_mark();
        package(&mut s, &json!({"name": "World", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().all(|t| !t.contains("outside the offline area's extent")), "{said:?}");
    }

    #[test]
    fn an_offline_map_is_packaged_as_it_is_to_go_back() {
        let mut s = sample();
        s.run("offline.download", &json!({"name": "Back", "extent": [-20.0, -40.0, 60.0, 40.0]})).unwrap();
        s.run("offline.open", &json!({"name": "Back"})).unwrap();
        assert_eq!(s.map().name, "Back (offline)");
        let areas = s.project.offline_areas.len();
        let (area, bytes) = package(&mut s, &Value::Null).unwrap();
        assert_eq!(area, "Back");
        // No new area: the map already is one.
        assert_eq!(s.project.offline_areas.len(), areas);
        let entries = rustgis_data::zip::read(&bytes).unwrap();
        assert_eq!(entries[0].0, "Back.rgis");
    }

    /// What a new package of `home`'s only area does for a phone's copy: an identity not brought
    /// back yet, and the area's return check as it is now. This device keeps no rows for that
    /// identity, so it goes back as a whole map, compared with the area as last synced.
    fn given_a_new_package(home: &Session, field: &mut Session) {
        let held = home.project.offline_areas[0].generation.clone();
        let origin = field.project.field_origin.as_mut().unwrap();
        origin.generation = held;
        origin.package = crate::project::field_id().unwrap();
    }

    /// A session of a field project just read from its package, its given rows kept as the app
    /// keeps them once it opens.
    fn opened(project: Project) -> Session {
        let mut s = Session::new(project);
        s.run("offline.keepRows", &Value::Null).unwrap();
        s
    }

    /// Layer `name`'s id in the active map.
    fn layer(s: &Session, name: &str) -> u32 {
        s.map().layers.iter().find(|l| l.name == name).map(|l| l.id).unwrap()
    }

    /// The text of field `field` in row `row` of layer `name`.
    fn value(s: &Session, name: &str, row: usize, field: &str) -> String {
        let l = s.map().layers.iter().find(|l| l.name == name).unwrap();
        l.dataset.value(row, l.dataset.field_index(field).unwrap()).to_string()
    }

    /// A package names the unpacked photo by absolute path so it can be saved or recovered elsewhere.
    #[test]
    fn field_photo_fixture_resolves_to_the_unpacked_file() {
        let temp = Temp::new("field-photo-fixture");
        let relative = "photos/site.jpg";
        let archive = rustgis_data::zip::write(&[
            ("Survey.rgis".into(), include_bytes!("../../../../schema/fixtures/field_photos.rgis").to_vec()),
            (relative.into(), b"photo bytes".to_vec()),
        ])
        .unwrap();
        let photo_notices = |project: &mut Project| {
            project
                .take_read_notices(&[crate::project::ReadPart::Maps])
                .iter()
                .map(crate::notice_text::NoticeText::english)
                .filter(|notice| notice.contains("unpacked field photo reference"))
                .collect::<Vec<_>>()
        };
        let unpacked = temp.path().join("unpacked");
        let (text, _) = unpack_into(&archive, &unpacked).unwrap();
        let mut project = project_in(&text, &unpacked).unwrap();
        assert!(photo_notices(&mut project).is_empty());
        let photo = unpacked.join(relative);
        assert_eq!(Path::new(&value(&Session::new(project), "Sites", 0, "photos")), photo.as_path());
        assert_eq!(std::fs::read(photo).unwrap(), b"photo bytes");

        // A folder whose name isn't UTF-8. Apple's APFS refuses such a name ("Illegal byte
        // sequence"), so this part runs on other Unix systems.
        #[cfg(all(unix, not(target_vendor = "apple")))]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let unpacked = temp.path().join(std::ffi::OsStr::from_bytes(b"non-utf8-\xff"));
            // Where the file system allows such a name: macOS's refuses one that isn't UTF-8
            // (EILSEQ, 92 there). Any other failure to make or remove the folder fails the test.
            let allowed = match std::fs::create_dir(&unpacked) {
                Ok(()) => {
                    std::fs::remove_dir(&unpacked).unwrap();
                    true
                }
                Err(e) if cfg!(target_os = "macos") && e.raw_os_error() == Some(92) => false,
                Err(e) => panic!("{}: {e}", unpacked.display()),
            };
            if allowed {
                let (text, _) = unpack_into(&archive, &unpacked).unwrap();
                let mut project = project_in(&text, &unpacked).unwrap();
                let notices = photo_notices(&mut project);
                assert_eq!(notices.len(), 1);
                assert!(notices[0].starts_with("1 unpacked field photo reference"));
                assert_eq!(value(&Session::new(project), "Sites", 0, "photos"), relative);
            }
        }
    }

    #[test]
    fn the_fields_edits_and_photos_come_back_to_the_map_they_were_packaged_from() {
        let temp = Temp::new("round-trip");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let (_, out) = package(&mut home, &json!({"name": "Trip", "extent": [-20.0, -40.0, 60.0, 40.0]})).unwrap();
        // In the field: the package opened, a city renamed, one added, one deleted, a photo taken.
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        // As `project.open` marks a project opened from a field package.
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        assert_eq!(field.map().name, "Trip (offline)");
        let cities = layer(&field, "Cities");
        let name =
            field.map().layer(cities).unwrap().dataset.fields.iter().find(|f| f.ty == rustgis_data::FieldType::Text).unwrap().name.clone();
        let renamed = field.map().layer(cities).unwrap().dataset.features[0].id;
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "Field Camp"})).unwrap();
        field.run("edit.addFeature", &json!({"layer": cities, "geometry": {"type": "Point", "coordinates": [10.0, 5.0]}, "attributes": {name.clone(): "New Site"}})).unwrap();
        let gone = field.map().layer(cities).unwrap().dataset.features[1].id;
        field.run("select.rows", &json!({"layer": cities, "rows": [1]})).unwrap();
        field.run("edit.delete", &Value::Null).unwrap();
        let photo = temp.path().join("photo-1.jpg");
        std::fs::write(&photo, b"jpeg bytes").unwrap();
        field.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
        // Sent back: the photo goes with it, its feature naming it in the package, and with only
        // pictures named nothing is said of other files.
        let mark = field.notice_mark();
        let (area, back) = package(&mut field, &Value::Null).unwrap();
        assert!(field.notices_since(mark).iter().all(|n| !n.text.contains("only pictures go")));
        assert_eq!(area, "Trip");
        let entries = rustgis_data::zip::read(&back).unwrap();
        assert!(entries.iter().any(|(n, b)| n == "photos/photo-1.jpg" && b == b"jpeg bytes"));
        // On the phone it was taken on, not here: brought back, it is copied in.
        std::fs::remove_file(&photo).unwrap();
        // Meanwhile at home, the renamed city was changed too: a conflict, said.
        let row = home.map().layer(layer(&home, "Cities")).unwrap().dataset.features.iter().position(|f| f.id == renamed).unwrap();
        let home_cities = layer(&home, "Cities");
        home.run("edit.setValue", &json!({"layer": home_cities, "row": row, "field": name, "value": "Home Edit"})).unwrap();
        let before = home.map().layer(home_cities).unwrap().dataset.features.len();
        let sent = temp.path().join("back.rgisfield");
        std::fs::write(&sent, &back).unwrap();
        let mark = home.notice_mark();
        let done = home.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap();
        assert_eq!((done["updated"].as_u64(), done["added"].as_u64(), done["deleted"].as_u64()), (Some(1), Some(1), Some(1)), "{done}");
        let home_layer = home.map().layer(home_cities).unwrap();
        assert_eq!(home_layer.dataset.features.len(), before);
        assert!(!home_layer.dataset.features.iter().any(|f| f.id == gone));
        let row = home_layer.dataset.features.iter().position(|f| f.id == renamed).unwrap();
        assert_eq!(value(&home, "Cities", row, &name), "Field Camp");
        // The photo is beside the project, and the feature names it there.
        let kept = temp.path().join("Trip photos").join("photo-1.jpg");
        assert_eq!(std::fs::read(&kept).unwrap(), b"jpeg bytes");
        assert_eq!(value(&home, "Cities", row, "photos"), kept.display().to_string());
        let said: Vec<String> = home.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.contains("changed in the map") && t.contains("Cities")), "{said:?}");
        assert!(said.iter().any(|t| t.contains("Brought the field's edits back") && t.contains("1 photo(s)")), "{said:?}");
        // The same package brought back again is refused: no feature twice.
        let again = home.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap_err();
        assert!(again.contains("brought back to offline area 'Trip' already"), "{again}");
        assert_eq!(home.map().layer(home_cities).unwrap().dataset.features.len(), before);
        // A package for another home is refused before looking up an area by name.
        let mut other = sample();
        let e = other.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap_err();
        assert!(e.contains("another home project"), "{e}");
    }

    #[test]
    fn photos_go_back_only_as_pictures_and_never_replace_one_already_home() {
        let temp = Temp::new("photos");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let secret = temp.path().join("notes.txt");
        let picture = temp.path().join(".pending.jpg");
        std::fs::write(&secret, b"not a picture").unwrap();
        std::fs::write(&picture, b"first").unwrap();
        let named = format!("{};{}", secret.display(), picture.display());
        home.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": named})).unwrap();
        // Going out: the picture, under a plain name; the text file is said and stays home.
        let mark = home.notice_mark();
        let (_, out) = package(&mut home, &json!({"name": "Pics", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let photos: Vec<String> =
            rustgis_data::zip::read(&out).unwrap().into_iter().map(|(n, _)| n).filter(|n| n.starts_with("photos/") && n != INDEX).collect();
        assert_eq!(photos, ["photos/home-00001.jpg"]);
        let said: Vec<String> = home.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.starts_with("1 file(s) a photos field names") && t.contains("notes.txt")), "{said:?}");
        // Coming back: the picture only, under a plain name; the text file stays named as it was.
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        // As `project.open` marks a project opened from a field package.
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        // The field names the photos again, in another order: a change it brings back.
        let reordered = format!("{};{}", picture.display(), secret.display());
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": reordered})).unwrap();
        let back = |field: &mut Session| package(field, &Value::Null).unwrap().1;
        let entries = rustgis_data::zip::read(&back(&mut field)).unwrap();
        let photos: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).filter(|n| n.starts_with("photos/") && *n != INDEX).collect();
        assert_eq!(photos, ["photos/_pending.jpg"]);
        let first = back(&mut field);
        // Brought back where the photo isn't (as from a phone): copied in beside the project.
        std::fs::remove_file(&picture).unwrap();
        let path = temp.path().join("back.rgisfield");
        std::fs::write(&path, &first).unwrap();
        home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap();
        // A second package's photo of that name, other bytes: kept beside the first, renamed.
        std::fs::write(&picture, b"second").unwrap();
        given_a_new_package(&home, &mut field);
        let second = back(&mut field);
        std::fs::remove_file(&picture).unwrap();
        std::fs::write(&path, &second).unwrap();
        let mark = home.notice_mark();
        home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap();
        let dir = temp.path().join("Pics photos");
        assert_eq!(std::fs::read(dir.join("_pending.jpg")).unwrap(), b"first");
        assert_eq!(std::fs::read(dir.join("2-_pending.jpg")).unwrap(), b"second");
        assert!(home.notices_since(mark).iter().any(|n| n.text.contains("under a new name")));
    }

    #[test]
    fn a_photo_brought_back_to_where_it_already_is_keeps_its_path_and_no_edit_is_counted() {
        let temp = Temp::new("same-place");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let photo = temp.path().join("a.jpg");
        std::fs::write(&photo, b"a picture").unwrap();
        let gif = temp.path().join("b.gif");
        std::fs::write(&gif, b"gif").unwrap();
        home.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
        home.run("edit.setValue", &json!({"layer": cities, "row": 1, "field": "photos", "value": gif.display().to_string()})).unwrap();
        let (_, out) = package(&mut home, &json!({"name": "Here", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        // Sent back with no edit: the picture, brought from home, isn't carried back; the gif is
        // said.
        let mark = field.notice_mark();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let said: Vec<String> = field.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.contains("only pictures go") && t.contains("b.gif")), "{said:?}");
        let entries = rustgis_data::zip::read(&back).unwrap();
        assert!(entries.iter().all(|(n, _)| !n.starts_with("photos/")));
        // Brought back here: the photo is where it was, so nothing changed and nothing is copied.
        let sent = temp.path().join("back.rgisfield");
        std::fs::write(&sent, &back).unwrap();
        let done = home.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap();
        assert_eq!((done["updated"].as_u64(), done["conflicts"].as_u64()), (Some(0), Some(0)), "{done}");
        assert_eq!(value(&home, "Cities", 0, "photos"), photo.display().to_string());
        assert!(!temp.path().join("Here photos").join("a.jpg").exists());
    }

    /// A photo named at home goes out with its feature, and comes back to the path it has at home:
    /// an edit to its feature's photos, or the feature's deletion, is no conflict and copies
    /// nothing. Photos the package can't carry are counted, going out.
    #[test]
    fn photos_named_at_home_go_out_and_come_back_to_their_own_paths() {
        let temp = Temp::new("home-photos");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let (kept, dropped) = (temp.path().join("kept.jpg"), temp.path().join("dropped.jpg"));
        std::fs::write(&kept, b"kept at home").unwrap();
        std::fs::write(&dropped, b"of a feature deleted in the field").unwrap();
        let missing = temp.path().join("not-here.jpg");
        let set = |s: &mut Session, row: usize, text: String| {
            s.run("edit.setValue", &json!({"layer": cities, "row": row, "field": "photos", "value": text})).unwrap();
        };
        set(&mut home, 0, kept.display().to_string());
        set(&mut home, 1, dropped.display().to_string());
        set(&mut home, 2, format!("{};relative.jpg", missing.display()));
        let (ids, before) = {
            let f = &home.map().layer(cities).unwrap().dataset.features;
            ([f[0].id, f[1].id], f.len())
        };
        let mark = home.notice_mark();
        let (_, out) = package(&mut home, &json!({"name": "Away", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let entries = rustgis_data::zip::read(&out).unwrap();
        assert!(entries.iter().any(|(n, b)| n == "photos/home-00001.jpg" && b == b"kept at home"));
        assert!(entries.iter().any(|(n, b)| n == "photos/home-00002.jpg" && b == b"of a feature deleted in the field"));
        // The two named that aren't files here (one by no full path) are counted, for the sender
        // and the crew.
        let expected = "2 photo(s) a photos field names aren't files on this computer by their full path";
        let said: Vec<String> = home.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert_eq!(said.iter().filter(|t| t.starts_with(expected) && t.contains("relative.jpg")).count(), 1, "{said:?}");
        let unpacked = temp.path().join("field");
        let (text, losses) = unpack_into(&out, &unpacked).unwrap();
        assert!(losses.iter().any(|t| t.starts_with(expected)), "{losses:?}");
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let there = unpacked.join("photos").join("home-00001.jpg");
        assert_eq!(Path::new(&value(&field, "Cities", 0, "photos")), there.as_path());
        // In the field: a photo added to the first feature, the second feature deleted.
        let taken = temp.path().join("photo-9.jpg");
        std::fs::write(&taken, b"taken in the field").unwrap();
        set(&mut field, 0, format!("{};{}", there.display(), taken.display()));
        field.run("select.rows", &json!({"layer": cities, "rows": [1]})).unwrap();
        field.run("edit.delete", &Value::Null).unwrap();
        let mark = field.notice_mark();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        // The two photos that never went out aren't here either, and that is said.
        let said: Vec<String> = field.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.starts_with("2 photo(s) a photos field names aren't on this device")), "{said:?}");
        // Only the field's photo goes back: home has its own.
        let carried: Vec<String> = rustgis_data::zip::read(&back)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .filter(|n| n.starts_with("photos/") && n != INDEX)
            .collect();
        assert_eq!(carried, ["photos/photo-9.jpg"]);
        std::fs::remove_file(&taken).unwrap();
        let sent = temp.path().join("back.rgisfield");
        std::fs::write(&sent, &back).unwrap();
        let done = home.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap();
        assert_eq!(
            (done["updated"].as_u64(), done["deleted"].as_u64(), done["conflicts"].as_u64(), done["kept"].as_u64()),
            (Some(1), Some(1), Some(0), Some(0)),
            "{done}"
        );
        let f = &home.map().layer(cities).unwrap().dataset.features;
        assert_eq!(f.len(), before - 1);
        assert!(f.iter().all(|r| r.id != ids[1]));
        // The home photo keeps its own path; only the field's is copied in.
        let row = f.iter().position(|r| r.id == ids[0]).unwrap();
        let copied = temp.path().join("Away photos").join("photo-9.jpg");
        assert_eq!(value(&home, "Cities", row, "photos"), format!("{};{}", kept.display(), copied.display()));
        assert_eq!(std::fs::read(&copied).unwrap(), b"taken in the field");
        assert!(!temp.path().join("Away photos").join("kept.jpg").exists());
        assert!(home.project.offline_areas[0].conflicts.is_empty());
    }

    /// A streamed layer's photos aren't packed with the home photos: its copy for reference (or
    /// its absence) decides what goes, and its photos are counted as staying home.
    #[test]
    fn a_streamed_layers_photos_stay_home_and_are_counted() {
        let temp = Temp::new("stream-photos");
        let photo = temp.path().join("site.jpg");
        std::fs::write(&photo, b"a streamed layer's photo").unwrap();
        let source = temp.path().join("sites.gpkg");
        let mut seed = Session::new(Project::default());
        let fields = vec![rustgis_data::Field::new("photos", rustgis_data::FieldType::Text)];
        let mut sites = rustgis_data::Dataset::new("Sites", rustgis_data::GeometryType::Point, fields);
        sites.crs = rustgis_data::raster::Crs::from_epsg(4326);
        let point = rustgis_data::Geometry::Point(rustgis_data::Coord::new(1.0, 1.0));
        sites.push(Some(point), vec![rustgis_data::Value::Text(photo.display().to_string())]);
        let id = seed.map_mut().add_dataset(sites);
        seed.run("share.exportLayer", &json!({"layer": id, "path": source})).unwrap();
        let mut home = Session::new(Project::default());
        home.path = Some(temp.path().join("home.rgis"));
        home.run("layer.addData", &json!({"name": "Streamed", "path": source, "layer": "Sites", "stream": true})).unwrap();
        assert!(home.map().layer_by_name("Streamed").unwrap().is_streamed());
        // The rows a drawn view has read from the source, as the area's map may hold them.
        let streamed = home.map_mut().layers.iter_mut().find(|l| l.name == "Streamed").unwrap();
        let point = rustgis_data::Geometry::Point(rustgis_data::Coord::new(1.0, 1.0));
        let fi = streamed.dataset.field_index("photos").unwrap();
        let mut values = vec![rustgis_data::Value::Null; streamed.dataset.fields.len()];
        values[fi] = rustgis_data::Value::Text(photo.display().to_string());
        streamed.dataset.push(Some(point), values);
        let mut budget = u64::MAX;
        assert!(collect_photos(home.map(), &mut budget, usize::MAX).unwrap().entries.is_empty());
        let mark = home.notice_mark();
        let (_, out) = package(&mut home, &json!({"name": "Streams", "extent": [0, 0, 2, 2]})).unwrap();
        assert!(rustgis_data::zip::read(&out).unwrap().iter().all(|(n, _)| !n.starts_with("photos/")));
        let said: Vec<String> = home.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.starts_with("1 photo reference(s) of layer(s) copied for reference stayed home")), "{said:?}");
    }

    /// Coming back, a photo whose path holds a line break isn't carried (the index couldn't hold
    /// it), and that is said; one beside it goes.
    #[cfg(unix)]
    #[test]
    fn a_photo_named_with_a_line_break_stays_on_the_phone_and_is_said() {
        let temp = Temp::new("newline-photo");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let (_, out) = package(&mut home, &json!({"name": "Break", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let cities = layer(&field, "Cities");
        let (odd, plain) = (temp.path().join("odd\nname.jpg"), temp.path().join("plain.jpg"));
        std::fs::write(&odd, b"odd").unwrap();
        std::fs::write(&plain, b"plain").unwrap();
        field.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let named = format!("{};{}", odd.display(), plain.display());
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": named})).unwrap();
        let mark = field.notice_mark();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let entries: Vec<String> = rustgis_data::zip::read(&back).unwrap().into_iter().map(|(n, _)| n).collect();
        assert!(entries.iter().any(|n| n == "photos/plain.jpg") && !entries.iter().any(|n| n.contains("odd")), "{entries:?}");
        let said: Vec<String> = field.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.starts_with("1 photo(s) couldn't be put") && t.contains("tab or a line break")), "{said:?}");
    }

    /// A package from home whose photo index is there but can't be read is said, on opening and
    /// on sending back: its photos can't be told from the field's.
    #[test]
    fn an_unreadable_photo_index_is_said_on_opening_and_on_sending_back() {
        let temp = Temp::new("bad-index");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let photo = temp.path().join("site.jpg");
        std::fs::write(&photo, b"a picture").unwrap();
        home.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
        let (_, out) = package(&mut home, &json!({"name": "Bad", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let index = unpacked.join("photos").join("index.tsv");
        let notes = |project: &mut Project| -> Vec<String> {
            project.take_read_notices(&[crate::project::ReadPart::Maps]).iter().map(crate::notice_text::NoticeText::english).collect()
        };
        // A photo the index lists that the package doesn't hold: counted.
        let listed = std::fs::read_to_string(&index).unwrap();
        let held = unpacked.join("photos").join("home-00001.jpg");
        let bytes = std::fs::read(&held).unwrap();
        std::fs::remove_file(&held).unwrap();
        let said = notes(&mut project_in(&text, &unpacked).unwrap());
        assert!(said.iter().any(|t| t.starts_with("1 photo(s) the field package lists aren't in it")), "{said:?}");
        std::fs::write(&held, bytes).unwrap();
        // An index that isn't UTF-8 text: damaged.
        std::fs::write(&index, b"photos/home-00001.jpg\t\xff\n").unwrap();
        let said = notes(&mut project_in(&text, &unpacked).unwrap());
        assert!(said.iter().any(|t| t.contains("not UTF-8 text")), "{said:?}");
        // An entry listed twice: the index is damaged.
        std::fs::write(&index, format!("{listed}{listed}")).unwrap();
        let said = notes(&mut project_in(&text, &unpacked).unwrap());
        assert!(said.iter().any(|t| t.contains("is listed twice")), "{said:?}");
        // The package's marker damaged: its photos can't be told as home's, and that is said.
        let marker = unpacked.join(crate::cmd::field_changes::GIVEN);
        let held_marker = std::fs::read(&marker).unwrap();
        std::fs::write(&marker, b"not json").unwrap();
        let said = notes(&mut project_in(&text, &unpacked).unwrap());
        assert!(said.iter().any(|t| t.starts_with("The field package's photo index couldn't be read")), "{said:?}");
        std::fs::write(&marker, held_marker).unwrap();
        // The index gone from beside the photos: damaged too.
        std::fs::remove_file(&index).unwrap();
        let mut missing = project_in(&text, &unpacked).unwrap();
        let said: Vec<String> =
            missing.take_read_notices(&[crate::project::ReadPart::Maps]).iter().map(crate::notice_text::NoticeText::english).collect();
        assert!(said.iter().any(|t| t.contains("missing beside the photos")), "{said:?}");
        // The index damaged: a folder where the file was.
        std::fs::create_dir(&index).unwrap();
        let mut project = project_in(&text, &unpacked).unwrap();
        let opened_notes: Vec<String> =
            project.take_read_notices(&[crate::project::ReadPart::Maps]).iter().map(crate::notice_text::NoticeText::english).collect();
        assert!(opened_notes.iter().any(|t| t.starts_with("The field package's photo index couldn't be read")), "{opened_notes:?}");
        let mut field = opened(project);
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        // The feature names the unpacked photo: sending back can't tell it was home's.
        let there = unpacked.join("photos").join("home-00001.jpg");
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": there.display().to_string()})).unwrap();
        let mark = field.notice_mark();
        package(&mut field, &Value::Null).unwrap();
        let said: Vec<String> = field.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.starts_with("The photo index of the field package this map came from couldn't be read")), "{said:?}");
    }

    /// Unpacked into a folder whose name isn't UTF-8, a photo from home keeps its name at home
    /// (no path the project holds can name the unpacked file), and that is said: the way back then
    /// sees no edit in it. Apple's file system refuses such a name, so this runs on other Unix
    /// systems.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    #[test]
    fn a_home_photo_unpacked_where_no_text_names_it_keeps_its_home_name() {
        use std::os::unix::ffi::OsStrExt as _;
        let temp = Temp::new("home-photo-non-utf8");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let photo = temp.path().join("site.jpg");
        std::fs::write(&photo, b"a picture").unwrap();
        home.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
        let (_, out) = package(&mut home, &json!({"name": "Odd", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join(std::ffi::OsStr::from_bytes(b"non-utf8-\xff"));
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut project = project_in(&text, &unpacked).unwrap();
        let said: Vec<String> =
            project.take_read_notices(&[crate::project::ReadPart::Maps]).iter().map(crate::notice_text::NoticeText::english).collect();
        assert!(said.iter().any(|t| t.starts_with("1 photo(s) the field package brought from home keep the names")), "{said:?}");
        assert_eq!(value(&Session::new(project), "Cities", 0, "photos"), photo.display().to_string());
    }

    /// Every photos field of a layer is read going out, and coming back a name a feature already
    /// gives under `photos/` isn't given to a photo carried.
    #[test]
    fn both_photos_fields_go_out_and_a_returns_names_keep_clear_of_those_given() {
        let temp = Temp::new("two-photo-fields");
        let (one, two) = (temp.path().join("one.jpg"), temp.path().join("x.jpg"));
        std::fs::write(&one, b"one").unwrap();
        std::fs::write(&two, b"two").unwrap();
        let mut s = sample();
        let cities = layer(&s, "Cities");
        for field in ["photos", "photo"] {
            s.run("edit.addField", &json!({"layer": cities, "name": field, "type": "text"})).unwrap();
        }
        s.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": one.display().to_string()})).unwrap();
        s.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photo", "value": two.display().to_string()})).unwrap();
        let mut budget = u64::MAX;
        let out = collect_photos(s.map(), &mut budget, usize::MAX).unwrap();
        assert_eq!(out.entries.iter().filter(|(n, _)| n != INDEX).count(), 2);
        // Coming back: a feature names `photos/x.jpg` (not on this device); the field's x.jpg takes
        // another name.
        s.run("edit.setValue", &json!({"layer": cities, "row": 1, "field": "photos", "value": "photos/x.jpg"})).unwrap();
        let mut map = s.map().clone();
        let back = take_photos(&mut map, &mut budget, "", usize::MAX).unwrap();
        let names: Vec<&str> = back.entries.iter().map(|(n, _)| n.as_str()).filter(|n| *n != INDEX).collect();
        assert!(names.contains(&"photos/2-x.jpg") && !names.contains(&"photos/x.jpg"), "{names:?}");
    }

    /// A photo taken into a layer's second photos field comes home with its edit.
    #[test]
    fn a_photo_in_a_second_photos_field_comes_home() {
        let temp = Temp::new("second-field-home");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        for field in ["photos", "photo"] {
            home.run("edit.addField", &json!({"layer": cities, "name": field, "type": "text"})).unwrap();
        }
        let id = home.map().layer(cities).unwrap().dataset.features[0].id;
        let (_, out) = package(&mut home, &json!({"name": "Second", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let taken = temp.path().join("photo-7.jpg");
        std::fs::write(&taken, b"in the second field").unwrap();
        let row = field.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == id).unwrap();
        field.run("edit.setValue", &json!({"layer": cities, "row": row, "field": "photo", "value": taken.display().to_string()})).unwrap();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        std::fs::remove_file(&taken).unwrap();
        let sent = temp.path().join("back.rgisfield");
        std::fs::write(&sent, &back).unwrap();
        home.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap();
        let copied = temp.path().join("Second photos").join("photo-7.jpg");
        assert_eq!(std::fs::read(&copied).unwrap(), b"in the second field");
        let row = home.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == id).unwrap();
        assert_eq!(value(&home, "Cities", row, "photo"), copied.display().to_string());
    }

    /// A return opened on another device and sent again carries the field's photos from there:
    /// only a package from home maps its photos back to home's paths, so a return's own index
    /// (which names the first phone's paths) never stands in for the photo.
    #[test]
    fn a_return_sent_again_from_another_device_carries_the_fields_photos() {
        let temp = Temp::new("resent-photo");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let (_, out) = package(&mut home, &json!({"name": "Again", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let first = temp.path().join("first");
        let text = unpack_into(&out, &first).unwrap().0;
        let mut phone = opened(project_in(&text, &first).unwrap());
        phone.opened_package = Some(temp.path().join("sent.rgisfield"));
        let taken = temp.path().join("photo-3.jpg");
        std::fs::write(&taken, b"taken on the first phone").unwrap();
        phone.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": taken.display().to_string()})).unwrap();
        let (_, returned) = package(&mut phone, &Value::Null).unwrap();
        // Gone from the first phone; the return is opened on another device and sent from there.
        std::fs::remove_file(&taken).unwrap();
        let second = temp.path().join("second");
        let text = unpack_into(&returned, &second).unwrap().0;
        let mut other = Session::new(project_in(&text, &second).unwrap());
        other.opened_package = Some(temp.path().join("returned.rgisfield"));
        let (_, again) = package(&mut other, &Value::Null).unwrap();
        let entries = rustgis_data::zip::read(&again).unwrap();
        assert!(entries.iter().any(|(n, b)| n == "photos/photo-3.jpg" && b == b"taken on the first phone"));
    }

    /// A package that goes back as a whole map (its rows weren't kept on the device) names the
    /// photos it brought from home where they are at home: a feature the field left alone is no
    /// edit, even when home's photo changed on disk meanwhile, so home's own edit to it stays.
    #[test]
    fn a_whole_map_return_leaves_a_feature_with_a_home_photo_as_home_has_it() {
        let temp = Temp::new("whole-map-photo");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let photo = temp.path().join("site.jpg");
        std::fs::write(&photo, b"as it went out").unwrap();
        home.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
        let id = home.map().layer(cities).unwrap().dataset.features[0].id;
        let (_, out) = package(&mut home, &json!({"name": "Whole", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        // Opened where its rows aren't kept: it goes back as a whole map.
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let mark = field.notice_mark();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        assert!(field.notices_since(mark).iter().any(|n| n.text.contains("as a whole map")));
        assert!(rustgis_data::zip::read(&back).unwrap().iter().all(|(n, _)| n != "photos/site.jpg"));
        // Meanwhile at home: the photo replaced on disk, and the feature's name changed.
        std::fs::write(&photo, b"retaken at home").unwrap();
        let name =
            home.map().layer(cities).unwrap().dataset.fields.iter().find(|f| f.ty == rustgis_data::FieldType::Text).unwrap().name.clone();
        home.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "Home Edit"})).unwrap();
        let sent = temp.path().join("back.rgisfield");
        std::fs::write(&sent, &back).unwrap();
        let done = home.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap();
        assert_eq!(done["updated"].as_u64(), Some(0), "{done}");
        let row = home.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == id).unwrap();
        assert_eq!(value(&home, "Cities", row, &name), "Home Edit");
        assert_eq!(value(&home, "Cities", row, "photos"), photo.display().to_string());
        assert!(!temp.path().join("Whole photos").exists());
    }

    /// Going out, a photo past what the package has room for stays home, and is counted. The
    /// features aren't changed, so the map's rows stay shared with the project's; and a name a
    /// feature already gives under `photos/` isn't given to another photo.
    #[test]
    fn a_photo_past_the_packages_budget_stays_home_and_is_counted() {
        let temp = Temp::new("photo-budget");
        let (small, large) = (temp.path().join("small.jpg"), temp.path().join("large.jpg"));
        std::fs::write(&small, [0u8; 8]).unwrap();
        std::fs::write(&large, [0u8; 64]).unwrap();
        let mut s = sample();
        let cities = layer(&s, "Cities");
        s.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let named = format!("{};{}", small.display(), large.display());
        s.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": named})).unwrap();
        s.run("edit.setValue", &json!({"layer": cities, "row": 1, "field": "photos", "value": "photos/home-00001.jpg"})).unwrap();
        let map = s.map().clone();
        let mut budget = 32;
        let photos = collect_photos(&map, &mut budget, usize::MAX).unwrap();
        assert_eq!(photos.too_large, [large.display().to_string()]);
        assert_eq!(photos.missing, ["photos/home-00001.jpg"]);
        assert_eq!(budget, 24);
        let entries: Vec<&str> = photos.entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(entries, ["photos/home-00002.jpg", INDEX]);
        let index = String::from_utf8(photos.entries[1].1.clone()).unwrap();
        assert_eq!(index, format!("photos/home-00002.jpg\t{}\n", small.display()));
        let (held, home) = (&map.layer(cities).unwrap().dataset.features, &s.map().layer(cities).unwrap().dataset.features);
        assert!(held.shares(home));
    }

    #[test]
    fn a_photo_in_a_folder_with_a_semicolon_goes_and_other_text_is_kept_as_written() {
        let temp = Temp::new("semicolon");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        let (_, out) = package(&mut home, &json!({"name": "Semi", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let cities = layer(&field, "Cities");
        // A picture in a folder with a `;` in its name, and a gif named by two features.
        let trips = temp.path().join("Trips;2024");
        std::fs::create_dir_all(&trips).unwrap();
        let photo = trips.join("IMG_1.jpg");
        std::fs::write(&photo, b"jpeg").unwrap();
        let gif = temp.path().join("b.gif");
        std::fs::write(&gif, b"gif").unwrap();
        let first = format!("{}; {}", photo.display(), gif.display());
        let spaced = "notes.jpg ;  other.jpg";
        for (row, value) in [(0, first.as_str()), (1, &*gif.display().to_string()), (2, spaced)] {
            field.run("edit.setValue", &json!({"layer": cities, "row": row, "field": "photos", "value": value})).unwrap();
        }
        let mark = field.notice_mark();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let entries = rustgis_data::zip::read(&back).unwrap();
        assert!(entries.iter().any(|(n, b)| n == "photos/IMG_1.jpg" && b == b"jpeg"));
        // The gif is said once, however many name it.
        let said: Vec<String> = field.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.starts_with("1 file(s)") && t.contains("b.gif")), "{said:?}");
        // Text naming nothing here is kept as it was written, spaces and all.
        let text = entries.iter().find(|(n, _)| n.ends_with(".rgis")).map(|(_, b)| String::from_utf8_lossy(b).into_owned()).unwrap();
        assert!(text.contains(spaced), "{text}");
    }

    #[test]
    fn photo_names_a_system_refuses_are_made_plain_and_left_out_coming_back() {
        for (name, plain) in [
            ("a:b*?.jpg", "a_b__.jpg"),
            ("CON.jpg", "_CON.jpg"),
            ("com1", "_com1"),
            (".x.jpg", "_x.jpg"),
            ("x.jpg.", "x.jpg"),
            ("IMG;1.jpg", "IMG_1.jpg"),
        ] {
            assert_eq!(plain_name(name), plain);
            assert!(!plain_file(name) && plain_file(plain), "{name}");
        }
        assert!(plain_file("IMG_1.jpg") && plain_file("COM10.jpg") && plain_file("console.jpg"));
        // A package with photo entries of such names: they are left out and counted.
        let temp = Temp::new("strange");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let (_, out) = package(&mut home, &json!({"name": "Odd", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let path = temp.path().join("back.rgisfield");
        // Clean: nothing said of photos left out.
        std::fs::write(&path, &back).unwrap();
        let mark = home.notice_mark();
        home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap();
        assert!(home.notices_since(mark).iter().all(|n| !n.text.contains("plain file names")));
        given_a_new_package(&home, &mut field);
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let mut entries = rustgis_data::zip::read(&back).unwrap();
        for odd in ["photos/.hidden.jpg", "photos/NUL.jpg", "photos/a*b.jpg"] {
            entries.push((odd.into(), b"x".to_vec()));
        }
        std::fs::write(&path, rustgis_data::zip::write(&entries).unwrap()).unwrap();
        let mark = home.notice_mark();
        home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap();
        let said: Vec<String> = home.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.starts_with("3 photo(s)") && t.contains("plain file names")), "{said:?}");
        assert!(!temp.path().join("Odd photos").exists());
    }

    /// An unusable destination is refused before edits land, rather than trying numbered photo
    /// names forever. Run it with a completion deadline so a regression fails instead of hanging.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn bringing_back_photos_refuses_a_file_where_their_folder_should_be() {
        let temp = Temp::new("photo-folder-is-file");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let (_, bytes) = package(&mut home, &json!({"name": "Site", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&bytes, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        // A feature naming the photo the package carries.
        let cities = layer(&field, "Cities");
        field.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": "photos/new.jpg"})).unwrap();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let mut entries = rustgis_data::zip::read(&back).unwrap();
        entries.push(("photos/new.jpg".into(), b"photo".to_vec()));
        let path = temp.path().join("back.rgisfield");
        std::fs::write(&path, rustgis_data::zip::write(&entries).unwrap()).unwrap();
        let blocked = temp.path().join("Site photos");
        std::fs::write(&blocked, b"keep this file").unwrap();
        let before = serde_json::to_string(&home.project).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = home.run("offline.bringBack", &json!({"path": path.display().to_string()}));
            let _ = send.send((result, serde_json::to_string(&home.project).unwrap()));
        });
        let (result, after) = receive.recv_timeout(std::time::Duration::from_secs(5)).unwrap(); // test-wait: a broken candidate loop must fail instead of hanging the suite
        let error = result.unwrap_err();
        assert!(error.contains("no photo was brought back") && error.contains("Site photos"), "{error}");
        assert_eq!(after, before);
        assert_eq!(std::fs::read(&blocked).unwrap(), b"keep this file");
    }

    #[test]
    fn a_feature_added_in_the_field_keeps_its_free_id_and_a_second_return_adds_nothing() {
        let temp = Temp::new("free-id");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        // The whole world: the area holds every feature, so the field's next id is free at home.
        let (_, out) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let cities = layer(&field, "Cities");
        field.run("edit.addFeature", &json!({"layer": cities, "geometry": {"type": "Point", "coordinates": [10.0, 5.0]}})).unwrap();
        let id = field.map().layer(cities).unwrap().dataset.features.last().unwrap().id;
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let path = temp.path().join("back.rgisfield");
        std::fs::write(&path, &back).unwrap();
        let mark = home.notice_mark();
        let first = home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap();
        assert_eq!(first["added"].as_u64(), Some(1), "{first}");
        assert!(home.notices_since(mark).iter().all(|n| !n.text.contains("took a new id")));
        let home_cities = layer(&home, "Cities");
        assert!(home.map().layer(home_cities).unwrap().dataset.features.iter().any(|f| f.id == id));
        // Again: refused, so the feature isn't there twice.
        let again = home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap_err();
        assert!(again.contains("already"), "{again}");
        assert_eq!(home.map().layer(home_cities).unwrap().dataset.features.iter().filter(|f| f.id == id).count(), 1);
    }

    #[test]
    fn field_origin_survives_plain_save_and_names_never_choose_the_home_or_area() {
        let temp = Temp::new("origin");
        let mut home = sample();
        let params = json!({"name": "Site", "extent": [-180.0, -85.0, 180.0, 85.0]});
        let (_, out) = package(&mut home, &params).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let field_project = project_in(&text, &unpacked).unwrap();
        let origin = field_project.field_origin.clone().unwrap();
        assert_eq!(origin.project, home.project.origin_id);
        assert_eq!(origin.area, home.project.offline_areas[0].id);
        // Ordinary Save As and reopen have no opened_package hint. The renamed map still
        // returns with its original area identity, rather than making a fresh offline area.
        let plain = serde_json::to_string(&field_project).unwrap();
        assert!(plain.contains("field-origin"));
        let mut field = Session::new(Project::from_json(&plain).unwrap());
        field.map_mut().name = "Renamed on phone".into();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let path = temp.path().join("back.rgisfield");
        std::fs::write(&path, &back).unwrap();
        let mut other = sample();
        package(&mut other, &params).unwrap();
        // These projects have identical layer IDs, types and area labels: only their
        // persisted origin differs. Refusal occurs before edits or photo installation.
        let before = serde_json::to_value(&other.project).unwrap();
        let err = other.run("offline.bringBack", &json!({"path": path})).unwrap_err();
        assert!(err.contains("another home project"), "{err}");
        assert_eq!(serde_json::to_value(&other.project).unwrap(), before);
        home.project.offline_areas[0].name = "Renamed at home".into();
        home.run("offline.bringBack", &json!({"path": path})).unwrap();
        // Recreating a same-named area in the correct home gets a fresh ID too.
        home.project.offline_areas.clear();
        package(&mut home, &params).unwrap();
        let err = home.run("offline.bringBack", &json!({"path": path})).unwrap_err();
        assert!(err.contains("no offline area with"), "{err}");
        // A legacy file remains readable, but cannot guess a home by name or layer ID.
        let mut legacy = field_project;
        legacy.field_origin = None;
        let bytes = rustgis_data::zip::write(&[("Site.rgis".into(), serde_json::to_vec(&legacy).unwrap())]).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let err = home.run("offline.bringBack", &json!({"path": path})).unwrap_err();
        assert!(err.contains("legacy") && err.contains("make a new field package"), "{err}");
        let mark = home.notice_mark();
        home.run("offline.bringBack", &json!({"path": path, "legacyArea":"Site"})).unwrap();
        assert!(home.notices_since(mark).iter().any(|notice| notice.text.contains("unverified origin")));
        let mut incomplete = serde_json::to_value(&legacy).unwrap();
        incomplete["requires"] = json!(["field-origin"]);
        let incomplete = rustgis_data::zip::write(&[("Site.rgis".into(), serde_json::to_vec(&incomplete).unwrap())]).unwrap();
        std::fs::write(&path, incomplete).unwrap();
        let err = home.run("offline.bringBack", &json!({"path":path,"legacyArea":"Site"})).unwrap_err();
        assert!(err.contains("cannot override"), "{err}");
        std::fs::write(&path, &back).unwrap();
        let before = serde_json::to_value(&home.project).unwrap();
        let err = home.run("offline.bringBack", &json!({"path":path,"legacyArea":"Site"})).unwrap_err();
        assert!(err.contains("cannot override"), "{err}");
        assert_eq!(serde_json::to_value(&home.project).unwrap(), before);
        // An old field project saved as a plain file can explicitly preserve its edits.
        let mut old = Session::new(legacy);
        old.map_mut().name = "Renamed old field copy".into();
        let (_, returned) = package(&mut old, &json!({"legacyReturn":true})).unwrap();
        let text = super::super::items::package_text(&returned).unwrap();
        assert!(Project::from_json(&text).unwrap().field_origin.is_none());
        assert!(old.project.offline_areas.is_empty());
    }

    #[test]
    fn an_opened_legacy_field_copy_refuses_an_added_map_before_packaging() {
        let mut field = Session::new(Project::new("Old field"));
        field.opened_package = Some(PathBuf::from("old.rgisfield"));
        field.project.maps.push(rustgis_map::Map::new("Added"));
        field.project.active_map = 1;
        field.clear_history();
        let before = serde_json::to_value(&field.project).unwrap();
        let state = field.state();
        let error = package_output(&mut field, &Value::Null).unwrap_err();
        assert!(error.contains("remove added maps"), "{error}");
        assert_eq!(serde_json::to_value(&field.project).unwrap(), before);
        assert_eq!(field.state(), state);
        assert_eq!(field.undo_depth(), 0);
        assert!(!field.dirty());
    }

    #[test]
    fn packaging_records_new_home_and_area_identities_as_an_undoable_change() {
        let mut home = sample();
        home.clear_history();
        assert!(!home.dirty());
        package_output(&mut home, &json!({"name":"Recorded","extent":[-180,-85,180,85]})).unwrap();
        assert!(home.dirty());
        assert_eq!(home.undo_depth(), 1);
        assert!(!home.project.origin_id.is_empty());
        home.run("edit.undo", &Value::Null).unwrap();
        assert!(home.project.origin_id.is_empty() && home.project.offline_areas.is_empty());
        assert!(!home.dirty());
    }

    #[test]
    fn field_package_construction_notes_wait_for_successful_delivery() {
        let temp = Temp::new("deferred-notes");
        let mut home = sample();
        let mark = home.notice_mark();
        let (_, _, notes) = super::super::items::item_output(&mut home, "fieldPackage", &json!({"name":"Deferred"})).unwrap();
        assert!(notes.iter().any(|note| note.contains("Made the offline area")));
        assert!(home.notices_since(mark).iter().all(|notice| !notice.text.contains("Made the offline area")));
        let mark = home.notice_mark();
        let error = home.run("offline.package", &json!({"name":"Failed","path":temp.path().join("missing/file.rgisfield")})).unwrap_err();
        assert!(error.contains("file.rgisfield"));
        assert!(home.notices_since(mark).iter().all(|notice| !notice.text.contains("Made the offline area 'Failed'")));
    }

    #[test]
    fn a_package_from_another_projects_area_of_the_same_name_is_refused() {
        let temp = Temp::new("foreign");
        // Elsewhere, an area named "Site" of another project's map: its own layers, its own ids.
        let mut other = Session::new(crate::project::Project::new("Other"));
        for (template, name) in [("polygons", "Parcels"), ("lines", "Roads"), ("points", "Wells")] {
            other.run("layer.template", &json!({"template": template, "name": name})).unwrap();
        }
        let roads = layer(&other, "Roads");
        other.run("edit.enable", &json!({"on": true})).unwrap();
        other
            .run("edit.addFeature", &json!({"layer": roads, "geometry": {"type": "LineString", "coordinates": [[0.0, 0.0], [1.0, 1.0]]}}))
            .unwrap();
        let (_, out) = package(&mut other, &json!({"name": "Site", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        // Here, an area of that name made from this project's map.
        let mut home = sample();
        package(&mut home, &json!({"name": "Site", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let ids: Vec<(u32, String)> = home.map().layers.iter().map(|l| (l.id, l.name.clone())).collect();
        let theirs: Vec<(u32, String)> = field.map().layers.iter().map(|l| (l.id, l.name.clone())).collect();
        let path = temp.path().join("back.rgisfield");
        std::fs::write(&path, &back).unwrap();
        let e = home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap_err();
        assert!(e.contains("another home project"), "{e} {ids:?} {theirs:?}");
    }

    #[test]
    fn a_bring_back_refused_leaves_no_photo_and_a_file_is_compared_by_its_size_first() {
        let temp = Temp::new("refused-back");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let (_, out) = package(&mut home, &json!({"name": "Back", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap().0;
        let mut field = opened(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let cities = layer(&field, "Cities");
        let photo = temp.path().join("site.jpg");
        std::fs::write(&photo, b"jpeg").unwrap();
        field.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
        let gone = field.map().layer(cities).unwrap().dataset.fields[0].name.clone();
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": gone, "value": "Changed in the field"})).unwrap();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        std::fs::remove_file(&photo).unwrap();
        // At home meanwhile, a field the field changed is removed: Bring Back is refused.
        let home_cities = layer(&home, "Cities");
        home.run("edit.deleteField", &json!({"layer": home_cities, "field": gone})).unwrap();
        let path = temp.path().join("back.rgisfield");
        std::fs::write(&path, &back).unwrap();
        let refused = home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap_err();
        assert!(refused.contains("hasn't the field(s)") && refused.contains(&gone), "{refused}");
        assert!(!temp.path().join("Back photos").exists());
        // Nor the photos field it would have added.
        assert!(home.map().layer(home_cities).unwrap().dataset.field_index("photos").is_none());
        // A file holds bytes only as a file of their size: a folder, or another size, doesn't.
        let file = temp.path().join("same.jpg");
        std::fs::write(&file, b"abc").unwrap();
        assert!(holds(&file, b"abc"));
        assert!(!holds(&file, b"abcd") && !holds(&file, b"abd") && !holds(temp.path(), b"abc"));
        assert!(!holds(&temp.path().join("missing.jpg"), b"abc"));
    }

    #[test]
    fn a_package_entry_outside_its_folder_is_refused() {
        let temp = Temp::new("refused");
        for bad in ["../escape.txt", "/abs.txt", "a/../../b.txt", "C:/x.txt"] {
            let bytes = rustgis_data::zip::write(&[("p.rgis".into(), b"{}".to_vec()), (bad.into(), b"x".to_vec())]).unwrap();
            let e = unpack_into(&bytes, &temp.path().join("in")).unwrap_err();
            assert!(e.contains("outside"), "{bad}: {e}");
        }
        // Two projects: refused. One that can't be read: refused, with nothing written.
        let two = rustgis_data::zip::write(&[("a.rgis".into(), b"{}".to_vec()), ("b.rgis".into(), b"{}".to_vec())]).unwrap();
        assert!(unpack_into(&two, &temp.path().join("two")).unwrap_err().contains("more than one project"));
        let damaged =
            rustgis_data::zip::write(&[("a.rgis".into(), b"not json".to_vec()), ("basemaps/b.mbtiles".into(), b"x".to_vec())]).unwrap();
        assert!(unpack_into(&damaged, &temp.path().join("damaged")).is_err());
        assert!(!temp.path().join("damaged").exists());
        let none = rustgis_data::zip::write(&[("a.txt".into(), b"x".to_vec())]).unwrap();
        assert!(unpack_into(&none, &temp.path().join("in")).unwrap_err().contains("no project"));
        assert!(is_field_package("Site.RGISFIELD") && !is_field_package("site.rgis"));
        assert!(crate::project::opens_as_project("site.rgisfield"));
    }
}
