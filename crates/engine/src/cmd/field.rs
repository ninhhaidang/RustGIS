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
//! layer's `photos` field, paths separated by `;`) under `photos/`. Bring Back Edits
//! (`offline.bringBack`) reads such a package in the project it was made from, puts its photos in
//! a folder beside that project, and copies the edits back to the area's map by feature, as Sync
//! does, saying which features were changed on both sides.

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
pub(crate) const OFFLINE: &str = " (offline)";
/// The most bytes read from one field package's entries when it is opened.
const MOST_UNPACKED: usize = rustgis_data::zip::MAX_TOTAL;

/// Whether a file name or path is a field package's, ignoring case.
pub fn is_field_package(name: &str) -> bool {
    name.to_ascii_lowercase().strip_suffix(EXTENSION).is_some_and(|s| s.ends_with('.'))
}

/// A field package of the active map, as (name, bytes).
///
/// An offline area's map (`<area> (offline)` of one of this project's areas, or the map of a
/// project opened from a field package) is packaged as it is, to bring its edits back. Any other map is first made an offline area of this project (`extent`
/// and `name` as for `offline.download`, the view by default), whose map is packaged.
///
/// Never silent: the raster layers an offline area leaves behind, the tile layers that stay
/// online (a web address, not an archive), and an archive that couldn't be read are said.
///
/// It runs where it is called, on the window's thread from Save (`ui.saveItem`), as Package
/// Project does: each archive is one read and the zip one write, with no Cancel between them.
pub(crate) fn package(s: &mut Session, p: &Value) -> Result<(String, Vec<u8>), String> {
    // An offline area's map: named for one of this project's areas, or opened from a field
    // package (the phone's project holds no areas). A map only named so is packaged as any other.
    let opened_field = s.opened_package.as_deref().is_some_and(|p| is_field_package(&p.to_string_lossy()));
    let returning = s
        .map()
        .name
        .strip_suffix(OFFLINE)
        .map(str::to_string)
        .filter(|area| opened_field || s.project.offline_areas.iter().any(|a| a.name.eq_ignore_ascii_case(area)));
    // Only a map coming back from the field (its project opened from a field package) carries its
    // photos: going out, an area's map included, the photos named are at home.
    let coming_back = returning.is_some() && opened_field;
    let (area, mut map) = match returning {
        Some(area) => {
            // Its raster layers stay behind, as an offline area's do going out: said.
            let mut m = s.map().clone();
            let rasters: Vec<u32> = m.layers.iter().filter(|l| l.is_raster()).map(|l| l.id).collect();
            if !rasters.is_empty() {
                m.release_tiles(&rasters);
                m.layers.retain(|l| !l.is_raster());
                s.notify(format!("{} raster layer(s) were left out of the field package: it holds feature layers", rasters.len()), false);
            }
            (area, m)
        }
        None => {
            let rasters = s.map().layers.iter().filter(|l| l.is_raster()).count();
            let features =
                |m: &rustgis_map::Map| m.layers.iter().filter(|l| !l.is_raster()).map(|l| l.dataset.features.len()).sum::<usize>();
            let had = features(s.map());
            let made = super::workspace::offline_download(s, p)?;
            let name = made.get("name").and_then(Value::as_str).ok_or("the offline area wasn't made")?.to_string();
            let data =
                s.project.offline_areas.iter().find(|a| a.name == name).map(|a| a.data.clone()).ok_or("the offline area wasn't made")?;
            if rasters > 0 {
                s.notify(
                    format!("{rasters} raster layer(s) were left out of the field package: an offline area holds feature layers"),
                    false,
                );
            }
            s.notify(
                format!("Made the offline area '{name}' of this map for the field package: save this project to bring the field's edits back to it later"),
                false,
            );
            // The area holds the features its extent (the view, unless one is given) touches.
            let out = had.saturating_sub(features(&data));
            if out > 0 {
                s.notify(
                    format!("{out} feature(s) outside the offline area's extent, or without a shape, stayed out of the field package"),
                    false,
                );
            }
            let mut m = data;
            m.name = format!("{name}{OFFLINE}");
            (name, m)
        }
    };
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut budget = MOST_UNPACKED as u64 / 2;
    // Each local archive goes in beside the project, its layer reading it from there.
    let (mut online, mut unread, mut uploaded) = (Vec::new(), Vec::new(), Vec::new());
    let mut taken = std::collections::HashSet::new();
    for t in &mut map.tiles {
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
        s.notify(
            format!(
                "{} tile layer(s) in the field package draw from the web and show only online: {}. Add an MBTiles or PMTiles archive for a basemap with no network",
                online.len(),
                online.join(", ")
            ),
            false,
        );
    }
    if !uploaded.is_empty() {
        s.notify(
            format!(
                "{} tile archive(s) added to this browser couldn't be put in the field package, as they live only in its memory: {}. Add them to the field from the file",
                uploaded.len(),
                uploaded.join(", ")
            ),
            true,
        );
    }
    if !unread.is_empty() {
        s.notify(format!("{} tile archive(s) couldn't be put in the field package: {}", unread.len(), unread.join(", ")), true);
    }
    // Coming back, the photos taken in the field go with their features.
    if coming_back {
        let photos = take_photos(&mut map, &mut budget);
        if !photos.unread.is_empty() {
            let unread = &photos.unread;
            s.notify(format!("{} photo(s) couldn't be put in the field package: {}", unread.len(), unread.join(", ")), true);
        }
        if !photos.not_pictures.is_empty() {
            let named = &photos.not_pictures;
            s.notify(
                format!(
                    "{} file(s) a photos field names weren't put in the field package: only pictures go ({}): {}",
                    named.len(),
                    PICTURES.join(", "),
                    named.join(", ")
                ),
                false,
            );
        }
        entries.extend(photos.entries);
    }
    // A project of the one map: what else the project holds (tables, notebooks, models, shared
    // items and their paths…) stays at home. The custom colour ramps and styles go with it, as its
    // layers may draw with them.
    let mut project = crate::project::Project::new(area.clone());
    // Rasters were taken out above, so the map goes as it is.
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
        (home.maps.len().saturating_sub(1), "other map(s)"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    if !stayed.is_empty() {
        s.notify(format!("The field package holds only the map '{area}': {} stayed in this project", stayed.join(", ")), false);
    }
    project.ramps = s.project.ramps.clone();
    project.styles = s.project.styles.clone();
    let readme = format!(
        "{area}: a RustGIS field package.\nOpen it with RustGIS on a phone or a computer (Open Project). Its basemaps are in {BASEMAPS}/.\n"
    );
    let room = crate::project::max_bytes().min(MOST_UNPACKED as u64).saturating_sub(entries.iter().map(|(_, b)| b.len() as u64).sum());
    let too_large = || "the field package would be too large: leave a tile archive or some data out".to_string();
    let (text, report) = crate::durable::output_owned(project, |p| crate::project::json_within(p, room)?.ok_or_else(too_large))?;
    crate::durable::notify(s, &report);
    entries.insert(0, (format!("{}.{}", super::items::safe(&area), crate::project::EXTENSION), text.into_bytes()));
    entries.push(("README.txt".into(), readme.into_bytes()));
    Ok((area, rustgis_data::zip::write(&entries)?))
}

/// Whether `map` is an offline area's map (`<area> (offline)`), packaged to come back.
fn returning_map(map: &rustgis_map::Map) -> bool {
    map.name.ends_with(OFFLINE)
}

/// A layer's photos field, by its place: its text field named `photos` or `photo`, whatever the
/// case (as the touch interface names it).
fn photos_field(layer: &rustgis_map::Layer) -> Option<usize> {
    layer.dataset.fields.iter().position(|f| {
        f.ty == rustgis_data::FieldType::Text && (f.name.eq_ignore_ascii_case("photos") || f.name.eq_ignore_ascii_case("photo"))
    })
}

/// Rewrites each photos field of `map` with `rewrite` (one path in, the path to name instead out).
fn rewrite_photos(map: &mut rustgis_map::Map, mut rewrite: impl FnMut(&str) -> String) {
    for l in map.layers.iter_mut().filter(|l| !l.is_raster()) {
        let Some(fi) = photos_field(l) else { continue };
        for f in &mut l.dataset.features {
            let Some(rustgis_data::Value::Text(held)) = f.attrs.get(fi) else { continue };
            // Only the paths rewritten change: the rest of the text stays as it was written, so
            // a feature isn't seen as edited for its spacing.
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
                continue;
            }
            out.push_str(held.get(last..).unwrap_or_default());
            if let Some(slot) = f.attrs.get_mut(fi) {
                *slot = rustgis_data::Value::Text(out);
            }
        }
    }
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

/// The photo files `map`'s features name that are on this device, as package entries under
/// `photos/` (within `budget`), each feature then naming its photos there; and the photos named
/// that couldn't be read. Only picture files ([`PICTURES`]) named by an absolute path go in, each
/// under a plain name ([`plain_name`]): a photos field is free text, and could name any file. A
/// photo named that isn't on this device (one named at home) is left as it is named.
fn take_photos(map: &mut rustgis_map::Map, budget: &mut u64) -> Photos {
    let (mut entries, mut unread, mut not_pictures) = (Vec::new(), Vec::new(), Vec::new());
    // Each photo's entry and the path it was named by, for the way back.
    let mut index = String::new();
    let mut placed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut taken = std::collections::HashSet::new();
    // The files already said (not pictures, or unread): each said once, however many name it.
    let mut said = std::collections::HashSet::new();
    rewrite_photos(map, |path| {
        if let Some(entry) = placed.get(path) {
            return entry.clone();
        }
        if said.contains(path) {
            return path.to_string();
        }
        let file = Path::new(path);
        // A picture only: a photos field is free text, and could name any file on the device.
        let picture = file.extension().is_some_and(|e| PICTURES.iter().any(|p| e.eq_ignore_ascii_case(p)));
        if !file.is_absolute() {
            return path.to_string();
        }
        // Not on this device (a photo named at home): left as named. One there that can't be
        // looked at is said, as one that can't be read is.
        match std::fs::metadata(file) {
            Ok(m) if m.is_file() => {}
            Ok(_) => return path.to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return path.to_string(),
            Err(e) => {
                said.insert(path.to_string());
                unread.push(format!("{path} ({e})"));
                return path.to_string();
            }
        }
        if !picture {
            said.insert(path.to_string());
            not_pictures.push(path.to_string());
            return path.to_string();
        }
        match crate::project::read_file(file, *budget) {
            Ok(bytes) => {
                *budget = budget.saturating_sub(bytes.len() as u64);
                let stem = file.file_name().map(|n| plain_name(&n.to_string_lossy())).unwrap_or_else(|| "photo.jpg".into());
                let mut name = stem.clone();
                for n in 2.. {
                    if taken.insert(name.to_ascii_lowercase()) {
                        break;
                    }
                    name = format!("{n}-{stem}");
                }
                let entry = format!("{PHOTOS}/{name}");
                index.push_str(&format!("{entry}\t{path}\n"));
                entries.push((entry.clone(), bytes));
                placed.insert(path.to_string(), entry.clone());
                entry
            }
            Err(e) => {
                said.insert(path.to_string());
                unread.push(format!("{path} ({e})"));
                path.to_string()
            }
        }
    });
    if !index.is_empty() {
        entries.push((INDEX.to_string(), index.into_bytes()));
    }
    Photos { entries, unread, not_pictures }
}

/// What [`take_photos`] found: the package's photo entries (with [`INDEX`]), the photos that
/// couldn't be read, and the files named that aren't pictures.
struct Photos {
    entries: Vec<(String, Vec<u8>)>,
    unread: Vec<String>,
    not_pictures: Vec<String>,
}

/// The entry listing each photo of a package and the path its feature named it by before
/// (`photos/a.jpg<TAB>/storage/…/a.jpg`), so a photo brought back to where it already is keeps
/// that path.
const INDEX: &str = "photos/index.tsv";

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
/// left out and counted. Then its edits are copied back to the area's map by feature, as Sync does
/// (`sync_area`), which says the conflicts.
///
/// It runs where it is called, on the window's thread, as Sync does: the package is one read, each
/// photo one write, and the sync one pass over the area's features, with no Cancel between them.
pub(crate) fn bring_back(s: &mut Session, p: &Value) -> Result<Value, String> {
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
    for (name, data) in entries {
        if name == INDEX {
            index = String::from_utf8_lossy(&data).into_owned();
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
    let project = Project::from_import_json(&text, None)?;
    let mut edited = project
        .maps
        .into_iter()
        .find(returning_map)
        .ok_or("the field package holds no offline area's map: package the map in the field (Send Field Package) to bring it back")?;
    let area = edited.name.strip_suffix(OFFLINE).unwrap_or_default().to_string();
    let ai = s.project.offline_areas.iter().position(|a| a.name.eq_ignore_ascii_case(&area)).ok_or_else(|| {
        format!(
            "this project has no offline area '{area}': open the project the field package was made from, as it was saved after packaging"
        )
    })?;
    // An area of that name in another project isn't this package's: its layers, matched by id,
    // must be the area's own, of the same kind (a name may have changed in the field), or features
    // of other layers would be changed.
    let baseline = s.project.offline_areas.get(ai).map(|a| &a.data).ok_or("no such area")?;
    let foreign = edited
        .layers
        .iter()
        .filter(|l| !l.is_raster())
        .any(|l| baseline.layers.iter().find(|b| b.id == l.id).is_some_and(|b| b.geometry_type() != l.geometry_type()))
        || !edited.layers.iter().any(|l| baseline.layers.iter().any(|b| b.id == l.id));
    if foreign {
        return Err(format!(
            "the field package's map isn't one made from this project's offline area '{area}': its layers aren't the area's. Open the project it was made from"
        ));
    }
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
                    Ok(_) if holds(&path, data) => {
                        write = false;
                        break;
                    }
                    _ => path = dir.join(format!("{n}-{name}")),
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
    let added = take_new_fields(s, ai, &edited);
    if !added.is_empty() {
        s.notify(format!("Added the field(s) the field package's layers added: {}", added.join(", ")), false);
    }
    let mut done = super::workspace::sync_area(s, ai, &edited)?;
    // The phone keeps its own ids: a feature it added whose id was taken here comes back next time
    // under that id, and is replaced then.
    if let Some(n) = done.get("renumbered").and_then(Value::as_u64).filter(|n| *n > 0) {
        s.notify(
            format!(
                "{n} feature(s) added in the field took a new id here, theirs being used in this map already. A later field package from the same phone replaces them under another new id: send a new field package for more edits"
            ),
            false,
        );
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
    if strange > 0 {
        s.notify(format!("{strange} photo(s) in the field package weren't brought back: their names aren't plain file names"), true);
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
/// aren't all taken as edited for lacking it), the fields `edited`'s layers have after theirs: a
/// field the field added. Only fields added after all of the layer's own, of names it hasn't;
/// fields that differ otherwise are left for the sync to refuse. Returns them as `layer: field`.
fn take_new_fields(s: &mut Session, ai: usize, edited: &rustgis_map::Map) -> Vec<String> {
    let Some(area) = s.project.offline_areas.get(ai) else { return Vec::new() };
    let source = area.map.clone();
    let same = |a: &[rustgis_data::Field], b: &[rustgis_data::Field]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.name.eq_ignore_ascii_case(&y.name) && x.ty == y.ty)
    };
    let mut added = Vec::new();
    for ol in edited.layers.iter().filter(|l| !l.is_raster()) {
        let fields = &ol.dataset.fields;
        let Some(si) = s.project.maps.iter().position(|m| m.name.eq_ignore_ascii_case(&source)) else { break };
        let Some(sl) = s.project.maps.get(si).and_then(|m| m.layers.iter().find(|l| l.id == ol.id && !l.is_raster())) else { continue };
        let had = sl.dataset.fields.len();
        let base_same = s
            .project
            .offline_areas
            .get(ai)
            .and_then(|a| a.data.layers.iter().find(|l| l.id == ol.id))
            .is_none_or(|b| same(&b.dataset.fields, &sl.dataset.fields));
        let Some(extra) = fields.get(had..).filter(|e| !e.is_empty()) else { continue };
        if !base_same || !fields.get(..had).is_some_and(|own| same(own, &sl.dataset.fields)) {
            continue;
        }
        if extra.iter().any(|f| sl.dataset.field_index(&f.name).is_some()) {
            continue;
        }
        let name = sl.name.clone();
        if let Some(l) = s.project.maps.get_mut(si).and_then(|m| m.layers.iter_mut().find(|l| l.id == ol.id)) {
            for f in extra {
                l.dataset.ensure_field(&f.name, f.ty);
            }
        }
        if let Some(b) = s.project.offline_areas.get_mut(ai).and_then(|a| a.data.layers.iter_mut().find(|l| l.id == ol.id)) {
            for f in extra {
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
/// numbered when that is taken) and returns the folder and the project's text. An entry that
/// would land outside the folder is refused, and the package with it.
///
/// The project's own paths are trusted as a project file's are: a package from someone else can
/// name files outside the folder, as a `.rgis` can.
pub(crate) fn unpack(bytes: &[u8], stem: &str) -> Result<(PathBuf, String), String> {
    let dir = unpack_dir(stem)?;
    let text = unpack_into(bytes, &dir)?;
    Ok((dir, text))
}

/// Unpacks a field package's `bytes` into `dir`, a folder not there yet (see [`unpack`]), and
/// returns the project's text. A write that fails removes what was written.
fn unpack_into(bytes: &[u8], dir: &Path) -> Result<String, String> {
    let entries = rustgis_data::zip::read(bytes)?;
    let mut project = None;
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
            files.push((parts.iter().collect::<PathBuf>(), data));
        }
    }
    let text = project.ok_or("the field package holds no project")?;
    // Read before anything is written: a project that can't be read leaves no folder behind.
    Project::from_import_json(&text, Some(dir))?;
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
    Ok(text)
}

/// Reads a field package's project, its relative paths resolved against `dir`.
pub(crate) fn project_in(text: &str, dir: &Path) -> Result<Project, String> {
    Project::from_import_json(text, Some(dir))
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
    std::fs::File::open(path).and_then(|f| f.take(data.len() as u64 + 1).read_to_end(&mut there)).is_ok() && there == data
}

/// What `offline.package` returns.
pub(crate) fn packaged(path: &str, area: &str, bytes: usize) -> Value {
    json!({"path": path, "area": area, "bytes": bytes})
}

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
        assert_eq!(names, ["Survey.rgis", "basemaps/site.mbtiles", "README.txt"]);
        assert_eq!(entries[1].1, b"stand-in archive bytes");
        // Unpacked and read: one map, the area's, its features in it, its basemap read from beside it.
        let into = temp.path().join("unpacked");
        let text = unpack_into(&bytes, &into).unwrap();
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
        assert!(said.iter().any(|t| t.contains("stayed in this project") && t.contains("table(s)")), "{said:?}");
        let names: Vec<String> = rustgis_data::zip::read(&bytes).unwrap().into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"basemaps/site_a_b__.mbtiles".to_string()), "{names:?}");
        let into = temp.path().join("unpacked");
        let project = project_in(&unpack_into(&bytes, &into).unwrap(), &into).unwrap();
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
        package(&mut s, &json!({"name": "Clean"})).unwrap();
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
        package(&mut s, &json!({"name": "Lossy"})).unwrap();
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

    /// Layer `name`'s id in the active map.
    fn layer(s: &Session, name: &str) -> u32 {
        s.map().layers.iter().find(|l| l.name == name).map(|l| l.id).unwrap()
    }

    /// The text of field `field` in row `row` of layer `name`.
    fn value(s: &Session, name: &str, row: usize, field: &str) -> String {
        let l = s.map().layers.iter().find(|l| l.name == name).unwrap();
        l.dataset.value(row, l.dataset.field_index(field).unwrap()).to_string()
    }

    #[test]
    fn the_fields_edits_and_photos_come_back_to_the_map_they_were_packaged_from() {
        let temp = Temp::new("round-trip");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let (_, out) = package(&mut home, &json!({"name": "Trip", "extent": [-20.0, -40.0, 60.0, 40.0]})).unwrap();
        // In the field: the package opened, a city renamed, one added, one deleted, a photo taken.
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap();
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
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
        // The same package brought back again: no feature twice.
        let again = home.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap();
        assert_eq!(home.map().layer(home_cities).unwrap().dataset.features.len(), before, "{again}");
        // The feature added in the field kept its id when it was free, and is found again as itself;
        // one that took a new id was said the first time, and is replaced.
        let renumbered = said.iter().any(|t| t.contains("took a new id"));
        assert_eq!(again["added"].as_u64(), Some(u64::from(renumbered)), "{again}");
        // A package for an area this project doesn't have is refused, by name.
        let mut other = sample();
        let e = other.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap_err();
        assert!(e.contains("no offline area 'Trip'"), "{e}");
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
        // Going out: no photos, whatever the features name.
        let (_, out) = package(&mut home, &json!({"name": "Pics", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        assert!(rustgis_data::zip::read(&out).unwrap().iter().all(|(n, _)| !n.starts_with("photos/")));
        // Coming back: the picture only, under a plain name; the text file stays named as it was.
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap();
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
        // As `project.open` marks a project opened from a field package.
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let back = |field: &mut Session| package(field, &Value::Null).unwrap().1;
        let entries = rustgis_data::zip::read(&back(&mut field)).unwrap();
        let photos: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).filter(|n| n.starts_with("photos/") && *n != INDEX).collect();
        assert_eq!(photos, ["photos/_pending.jpg"]);
        let first = back(&mut field);
        // A second package's photo of that name, other bytes: kept beside the first, renamed.
        std::fs::write(&picture, b"second").unwrap();
        let second = back(&mut field);
        // Brought back where the photo isn't (as from a phone): copied in beside the project.
        std::fs::remove_file(&picture).unwrap();
        let path = temp.path().join("back.rgisfield");
        std::fs::write(&path, &first).unwrap();
        home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap();
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
        let text = unpack_into(&out, &unpacked).unwrap();
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        // Sent back with no edit: the picture goes in, listed with its path; the gif is said.
        let mark = field.notice_mark();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let said: Vec<String> = field.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.contains("only pictures go") && t.contains("b.gif")), "{said:?}");
        let entries = rustgis_data::zip::read(&back).unwrap();
        assert!(entries.iter().any(|(n, b)| n == INDEX && String::from_utf8_lossy(b).contains(&*photo.display().to_string())));
        // Brought back here: the photo is where it was, so nothing changed and nothing is copied.
        let sent = temp.path().join("back.rgisfield");
        std::fs::write(&sent, &back).unwrap();
        let done = home.run("offline.bringBack", &json!({"path": sent.display().to_string()})).unwrap();
        assert_eq!((done["updated"].as_u64(), done["conflicts"].as_u64()), (Some(0), Some(0)), "{done}");
        assert_eq!(value(&home, "Cities", 0, "photos"), photo.display().to_string());
        assert!(!temp.path().join("Here photos").join("a.jpg").exists());
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
        let text = unpack_into(&out, &unpacked).unwrap();
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
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
        let text = unpack_into(&out, &unpacked).unwrap();
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        let path = temp.path().join("back.rgisfield");
        // Clean: nothing said of photos left out.
        std::fs::write(&path, &back).unwrap();
        let mark = home.notice_mark();
        home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap();
        assert!(home.notices_since(mark).iter().all(|n| !n.text.contains("plain file names")));
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

    #[test]
    fn a_feature_added_in_the_field_keeps_its_free_id_and_is_found_again() {
        let temp = Temp::new("free-id");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        // The whole world: the area holds every feature, so the field's next id is free at home.
        let (_, out) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap();
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
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
        // Again: found as the same feature, nothing added or deleted.
        let again = home.run("offline.bringBack", &json!({"path": path.display().to_string()})).unwrap();
        assert_eq!((again["added"].as_u64(), again["deleted"].as_u64()), (Some(0), Some(0)), "{again}");
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
        let text = unpack_into(&out, &unpacked).unwrap();
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
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
        assert!(e.contains("layers aren't the area's"), "{e} {ids:?} {theirs:?}");
    }

    #[test]
    fn a_bring_back_refused_leaves_no_photo_and_a_file_is_compared_by_its_size_first() {
        let temp = Temp::new("refused-back");
        let mut home = sample();
        home.path = Some(temp.path().join("home.rgis"));
        let (_, out) = package(&mut home, &json!({"name": "Back", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
        let unpacked = temp.path().join("field");
        let text = unpack_into(&out, &unpacked).unwrap();
        let mut field = Session::new(project_in(&text, &unpacked).unwrap());
        field.opened_package = Some(temp.path().join("sent.rgisfield"));
        let cities = layer(&field, "Cities");
        let photo = temp.path().join("site.jpg");
        std::fs::write(&photo, b"jpeg").unwrap();
        field.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
        field.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
        let (_, back) = package(&mut field, &Value::Null).unwrap();
        std::fs::remove_file(&photo).unwrap();
        // At home meanwhile, a field the field doesn't have: the sync is refused.
        let home_cities = layer(&home, "Cities");
        home.run("edit.addField", &json!({"layer": home_cities, "name": "extra", "type": "text"})).unwrap();
        let path = temp.path().join("back.rgisfield");
        std::fs::write(&path, &back).unwrap();
        assert!(home.run("offline.bringBack", &json!({"path": path.display().to_string()})).is_err());
        assert!(!temp.path().join("Back photos").exists());
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
