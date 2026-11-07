//! Opens a GeoLibre project as a new map, resolving sources through existing commands.
use super::ps;
use crate::{CmdResult, CommandSpec as C, Session};
use rustgis_map::geolibre::{self, Report, array, boolean, number, string};
use serde_json::{Value, json};

/// The importer reads files and URLs named inside the project, so callers require approval.
pub fn commands() -> Vec<C> {
    vec![C::new("project.importGeolibre", "Import GeoLibre Project", "Project › Import", import_project).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files)).params(
        r#"{"path"?: "*.geolibre|*.geolibre.json", "url"?: "https://…", "text"?: "project JSON", "data"?: "base64 JSON", "dir"?: "folder for local data"} → {map, layers, notes}"#,
    )]
}

/// Reads at most 64 MiB, before decoding or parsing hostile project data.
fn project_text(p: &Value) -> Result<String, String> {
    use std::io::Read;
    let bytes = if let Some(data) = ps(p, "data") {
        if data.len() > geolibre::MAX_TEXT / 3 * 4 + 4 {
            return Err("GeoLibre data exceeds 64 MiB".into());
        }
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| format!("data must be base64: {e}"))?
    } else if let Some(text) = ps(p, "text") {
        if text.len() > geolibre::MAX_TEXT {
            return Err("GeoLibre text exceeds 64 MiB".into());
        }
        return Ok(text.into());
    } else if let Some(url) = ps(p, "url") {
        if !rustgis_map::tiles::is_web(url) {
            return Err("url must be http:// or https://".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::render::rustgis_tiles::cache::download_blocking_limited(&geolibre::download_url(url), geolibre::MAX_TEXT)?
        }
        #[cfg(target_arch = "wasm32")]
        {
            return Err("pass downloaded GeoLibre JSON as text or data".into());
        }
    } else if let Some(path) = ps(p, "path") {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(geolibre::MAX_TEXT as u64 + 1).read_to_end(&mut bytes))
            .map_err(|e| format!("{path}: {e}"))?;
        bytes
    } else {
        return Err("path, url, text or data is required".into());
    };
    if bytes.len() > geolibre::MAX_TEXT {
        return Err("GeoLibre project exceeds 64 MiB".into());
    }
    String::from_utf8(bytes).map_err(|_| "GeoLibre project must be UTF-8 JSON".into())
}

/// Parses before mutating the session; an unreadable layer is noted and the others continue.
fn import_project(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    if ps(p, "url").is_some() && ps(p, "text").is_none() && ps(p, "data").is_none() {
        let mut q = p.clone();
        q["geolibreProject"] = json!(true);
        if let Some(url) = ps(p, "url") {
            if !rustgis_map::tiles::is_web(url) {
                return Err("url must be http:// or https://".into());
            }
            q["url"] = json!(geolibre::download_url(url));
        }
        return s.request_import_url(&q);
    }
    let text = project_text(p)?;
    let mut report = Report::default();
    let project = geolibre::read_project(&text, &mut report)?;
    if cfg!(target_arch = "wasm32") || s.leave_downloads {
        ensure_download_capacity(s, &project.layers, cfg!(target_arch = "wasm32"))?;
    }
    let dir = ps(p, "dir").filter(|d| !d.trim().is_empty()).map(str::to_string).or_else(|| {
        ps(p, "path")
            .and_then(|p| std::path::Path::new(p).parent())
            .map(|d| if d.as_os_str().is_empty() { ".".into() } else { d.to_string_lossy().into_owned() })
    });
    s.run_part("map.new", &json!({"name":project.name}))?;
    s.map_mut().basemap = rustgis_map::Basemap::Blank;
    s.map_mut().graticule = false;
    if !project.basemap.is_empty() {
        if let Err(e) = s.run_part(
            "map.addTiles",
            &json!({"url":project.basemap,"kind":"style","name":"GeoLibre background","opacity":project.basemap_opacity}),
        ) {
            report.skipped("", format!("basemap: {e}"));
        }
        if let Some(t) = s.map_mut().tiles.last_mut() {
            t.visible = project.basemap_visible;
        }
    }
    let mut group_index: std::collections::HashMap<&str, &Value> = Default::default();
    for group in &project.groups {
        group_index.entry(string(group, "id")).or_insert(group);
    }
    let mut added = Vec::new();
    let mut changed = 0usize;
    let mut groups: std::collections::HashMap<String, Vec<u32>> = Default::default();
    let mut order = Vec::new();
    for record in project.layers.iter().rev() {
        let name = string(record, "name");
        if !record.is_object() {
            report.skipped("", "malformed layer record");
            continue;
        }
        let before = s.project.clone();
        let active = s.active_layer;
        let requests = s.url_requests.len();
        let folder = group_look(&group_index, string(record, "groupId"));
        let mut layer_report = Report::default();
        let completed = match add_layer(s, record, dir.as_deref(), &mut layer_report, folder) {
            Ok(Added::Feature(id)) => {
                added.push(name.to_string());
                order.push(id);
                groups.entry(string(record, "groupId").into()).or_default().push(id);
                s.map().layer(id).is_some_and(|l| !l.dataset.source.as_deref().unwrap_or("").starts_with("geolibre-pending:"))
            }
            Ok(Added::Tile(index)) => {
                let (visible, opacity) = folder;
                if let Some(tile) = s.map_mut().tiles.get_mut(index) {
                    tile.visible &= visible;
                    tile.opacity *= opacity as f32;
                }
                added.push(name.to_string());
                s.map().tiles.get(index).is_some_and(|t| !t.id.starts_with("geolibre-pending:"))
            }
            Err(e) => {
                s.project = before;
                s.active_layer = active;
                s.url_requests.truncate(requests);
                report.skipped(name, format!("layer: {e}"));
                false
            }
        };
        if completed
            && (!string(record, "beforeId").is_empty() || layer_report.notes.iter().any(|n| n.kind == geolibre::NoteKind::Approximated))
        {
            changed += 1;
        }
        // Keep per-record summary accounting separate from Report's name/text deduplication.
        for note in layer_report.notes {
            for _ in 0..note.count {
                report.note(&note.layer, note.kind, note.text.clone());
            }
        }
    }
    for g in &project.groups {
        let mut group = rustgis_map::LayerGroup::new(string(g, "name"));
        group.children = if string(g, "id").is_empty() { Vec::new() } else { groups.remove(string(g, "id")).unwrap_or_default() };
        group.expanded = !boolean(g, "collapsed", false);
        let (visible, opacity) = group_look(&group_index, string(g, "id"));
        group.visible = visible;
        group.transparency = (100.0 * (1.0 - opacity)) as f32;
        s.map_mut().add_group(group);
        if !string(g, "parentId").is_empty() {
            report.approximated(string(g, "name"), "nested folder flattened; inherited visibility and opacity retained");
        }
    }
    // Its GeoTIFFs read in place, read together rather than one after another.
    crate::project::read_in_place(s.map_mut());
    s.wait_in_place_reads();
    order.reverse();
    s.map_mut().layers.sort_by_key(|l| order.iter().position(|id| *id == l.id).unwrap_or(usize::MAX));
    s.request_only = false;
    s.map_mut().camera = project.camera;
    s.map_mut().bookmarks = project.bookmarks;
    let skipped = project.layers.len().saturating_sub(added.len());
    s.notify(
        format!(
            "Imported {} of {} GeoLibre layers; {} drawn differently, {skipped} left out (see the import's notes)",
            added.len(),
            project.layers.len(),
            changed
        ),
        false,
    );
    Ok(json!({"map":project.name,"layers":added,"notes":report.notes}))
}

/// Inline features take precedence over saved paths for GeoJSON layers.
fn inline_data(r: &Value) -> Option<&Value> {
    r.get("geojson")
        .filter(|v| v.is_object() || v.is_string())
        .or_else(|| r.get("metadata").and_then(|m| m.get("embeddedGeoJSON")).filter(|v| v.is_object() || v.is_string()))
        .or_else(|| r.get("source").and_then(|s| s.get("data")).filter(|d| d.is_object()))
}

/// Resolves the saved address fields in the same order for loading and browser admission.
fn source_address(r: &Value) -> &str {
    let kind = string(r, "type");
    let src = &r["source"];
    let path = string(r, "sourcePath");
    if !path.is_empty() {
        return path;
    }
    let tile = array(src, "tiles").first().and_then(Value::as_str).unwrap_or("");
    if matches!(kind, "xyz" | "wms" | "wmts" | "vector-tiles" | "raster") && !tile.is_empty() {
        return tile;
    }
    let url = string(src, "url");
    if !url.is_empty() {
        url
    } else if !tile.is_empty() {
        tile
    } else {
        src.get("data").and_then(Value::as_str).unwrap_or("")
    }
}

/// The loader's whole-file queue choice; native MBTiles is synchronous and browser TIFFs stream.
fn needs_deferred_download(r: &Value, browser: bool) -> bool {
    let kind = string(r, "type");
    if string(r.get("metadata").unwrap_or(&Value::Null), "sourceKind") == "sql-query"
        || (kind == "geojson" && inline_data(r).is_some())
        || !rustgis_map::tiles::is_web(source_address(r))
    {
        return false;
    }
    match kind {
        "geojson" | "flatgeobuf" | "geoparquet" | "zarr" | "lidar" => true,
        "mbtiles" => browser,
        "raster" => {
            array(r.get("source").unwrap_or(&Value::Null), "tiles").is_empty()
                && (!browser || !super::layer::is_tiff_url(source_address(r)))
        }
        _ => false,
    }
}

/// Refuses a deferred import before creating its map when whole-file requests cannot all fit.
/// Streaming tiles/COGs and inline features consume no whole-file request slots.
fn ensure_download_capacity(s: &Session, layers: &[Value], browser: bool) -> Result<(), String> {
    let count = layers.iter().filter(|r| needs_deferred_download(r, browser)).count();
    // Once the UI drains url_requests, each Zarr reader (including a queued reader)
    // owns a session download id. Count all stages so a second import sees those slots.
    let occupied =
        s.queued_url_count().saturating_add(s.downloading.len()).saturating_add(s.downloaded.len()).saturating_add(s.landed_reads.len());
    let available = crate::MAX_URL_REQUESTS.saturating_sub(occupied);
    if count > available {
        return Err(format!(
            "GeoLibre import needs {count} downloads, but only {available} of {} download slots are available; split the project or wait for pending downloads before importing",
            crate::MAX_URL_REQUESTS
        ));
    }
    Ok(())
}

/// The actual storage chosen by a source loader determines whether a folder owns its look.
enum Added {
    Feature(u32),
    Tile(usize),
}

/// Adds data or tiles, retaining the original layer's presentation. Tile layers have their own
/// order in RustGIS, so mixing them with features is explicitly reported.
fn add_layer(s: &mut Session, r: &Value, dir: Option<&str>, report: &mut Report, folder: (bool, f64)) -> Result<Added, String> {
    #[cfg(not(target_arch = "wasm32"))]
    let _ = folder;
    let name = string(r, "name");
    let kind = string(r, "type");
    let src = &r["source"];
    if matches!(kind, "deckgl-viz" | "video" | "gaussian-splat" | "3d-tiles" | "arcgis" | "duckdb-query" | "sql-query" | "image")
        || string(&r["metadata"], "sourceKind") == "sql-query"
    {
        return Err(format!("unsupported {kind} layer"));
    }
    let inline = inline_data(r);
    let before: std::collections::HashSet<_> = s.map().layers.iter().map(|l| l.id).collect();
    let result = if kind == "geojson" && inline.is_some() {
        let data = inline.ok_or("missing GeoJSON")?;
        let text = if let Some(t) = data.as_str() { t.to_string() } else { data.to_string() };
        s.run_part("layer.addDataText", &json!({"fileName":"geolibre.geojson","text":text,"name":name}))?
    } else {
        let address = source_address(r);
        if address.is_empty() {
            return Err("no inline data, path or URL".into());
        }
        let address = resolve(address, dir)?;
        if kind == "mbtiles" && rustgis_map::tiles::is_web(&address) {
            let index;
            #[cfg(not(target_arch = "wasm32"))]
            {
                let bytes = crate::render::rustgis_tiles::cache::download_blocking(&address)?;
                let out = super::raster::add_named_bytes(s, &json!({"name":name}), format!("{}.mbtiles", pending_token()), bytes, None)?;
                index =
                    out.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or("archive added no tile layer")?;
                if let Some(t) = s.map_mut().tiles.get_mut(index) {
                    t.visible = boolean(r, "visible", true);
                    t.opacity = number(r, "opacity", 1.0).clamp(0.0, 1.0) as f32;
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                let token = pending_token();
                // An unfilled upload address cannot start an independent archive fetch.
                let mut tile =
                    rustgis_map::tiles::TileLayer::new(name, rustgis_map::tiles::TileKind::MbTiles, &format!("upload:{token}.mbtiles"));
                tile.id = token.clone();
                tile.visible = boolean(r, "visible", true);
                tile.opacity = number(r, "opacity", 1.0).clamp(0.0, 1.0) as f32;
                s.map_mut().tiles.insert(0, tile);
                index = 0;
                let mut saved = r.clone();
                saved["visible"] = json!(boolean(r, "visible", true) && folder.0);
                saved["opacity"] = json!(number(r, "opacity", 1.0).clamp(0.0, 1.0) * folder.1);
                s.request_import_url(&json!({"url":address,"whole":true,"name":name,"geolibreLayer":{"token":token,"record":saved}}))?;
                report.approximated(name, "MBTiles archive is downloading; wait before saving");
            }
            report.approximated(name, "tile overlays draw above feature layers; tile vector styling uses RustGIS defaults");
            return Ok(Added::Tile(index));
        }
        if matches!(kind, "xyz" | "wms" | "wmts" | "vector-tiles" | "pmtiles" | "mbtiles")
            || (kind == "raster" && !array(src, "tiles").is_empty())
        {
            let tk = match kind {
                "vector-tiles" => "vector",
                "pmtiles" => "pmtiles",
                "mbtiles" => "mbtiles",
                _ => "raster",
            };
            let mut q = json!({"url":address,"kind":tk,"name":name,"overlay":true,"opacity":number(r,"opacity",1.0).clamp(0.0,1.0)});
            for (source, target) in [
                ("minzoom", "minZoom"),
                ("maxzoom", "maxZoom"),
                ("tileSize", "tileSize"),
                ("bounds", "bounds"),
                ("attribution", "attribution"),
            ] {
                if let Some(value) = src.get(source) {
                    q[target] = value.clone();
                }
            }
            if string(src, "scheme") == "tms" {
                q["tms"] = json!(true);
            }
            let out = s.run_part("map.addTiles", &q)?;
            let index =
                out.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or("source added no tile layer")?;
            if let Some(t) = s.map_mut().tiles.get_mut(index) {
                t.visible = boolean(r, "visible", true);
            }
            report.approximated(name, "tile overlays draw above feature layers; feature/tile interleaving is not retained");
            if matches!(kind, "vector-tiles" | "pmtiles" | "mbtiles") {
                report.approximated(name, "tile vector styling uses RustGIS defaults");
            }
            return Ok(Added::Tile(index));
        }
        if !matches!(kind, "geojson" | "cog" | "raster" | "flatgeobuf" | "geoparquet" | "zarr" | "lidar") {
            return Err(format!("unsupported {kind} layer"));
        }
        let remote = rustgis_map::tiles::is_web(&address);
        // The project's saved camera is restored after the import, so each added layer keeps the view.
        let mut q = json!({"name":name,"zoom":false});
        q[if remote { "url" } else { "path" }] = json!(address);
        let defer = cfg!(target_arch = "wasm32") || s.leave_downloads;
        if defer && needs_deferred_download(r, cfg!(target_arch = "wasm32")) {
            // Reserve the final position and group membership before downloads can finish in
            // arbitrary order. The token follows the layer across map switches and reordering.
            let token = pending_token();
            let mut ds = rustgis_data::Dataset::new(name, rustgis_data::GeometryType::Point, Vec::new());
            ds.source = Some(token.clone());
            let id = s.map_mut().add_dataset(ds);
            if let Some(layer) = s.map_mut().layer_mut(id) {
                layer.visible = boolean(r, "visible", true);
                layer.transparency = (100.0 * (1.0 - number(r, "opacity", 1.0).clamp(0.0, 1.0))) as f32;
            }
            q["whole"] = json!(true);
            if kind == "zarr" {
                q["geolibreZarr"] = json!(true);
                if let Some(variable) = src.get("variable") {
                    q["variable"] = variable.clone();
                }
                if let Some(index) = src.get("index") {
                    q["index"] = index.clone();
                }
            }
            q["geolibreLayer"] = json!({"token":token,"record":r});
            s.request_import_url(&q)?;
            report.approximated(name, "URL data is downloading; its saved style is applied when the download completes");
            return Ok(Added::Feature(id));
        }
        if kind == "cog" && remote {
            super::layer::add_cog_with(s, &q, &address, false, false)?
        } else if kind == "zarr" {
            if let Some(v) = src.get("variable") {
                q["variable"] = v.clone();
            }
            if let Some(index) = src.get("index") {
                q["index"] = index.clone();
            }
            s.run_part("layer.addZarr", &q)?
        } else {
            s.run_part(if remote { "layer.addUrl" } else { "layer.addData" }, &q)?
        }
    };
    if result.get("pending").and_then(Value::as_bool) == Some(true) {
        return Err("source is pending and has not been imported".into());
    }
    if let Some(index) = result.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()) {
        if let Some(t) = s.map_mut().tiles.get_mut(index) {
            t.visible = boolean(r, "visible", true);
            t.opacity = number(r, "opacity", 1.0).clamp(0.0, 1.0) as f32;
        }
        return Ok(Added::Tile(index));
    }
    let ids: Vec<_> = s.map().layers.iter().filter(|l| !before.contains(&l.id)).map(|l| l.id).collect();
    if ids.len() != 1 {
        return Err("source must contain exactly one map layer (select or export one dataset first)".into());
    }
    let id = ids.first().copied().ok_or("source added no map layer")?;
    let layer = s.map_mut().layer_mut(id).ok_or("source added no map layer")?;
    geolibre::style::apply(layer, r, report);
    Ok(Added::Feature(id))
}

/// Absolute saved paths are tried first, then their basename in the supplied project folder.
/// Local paths require a locally chosen project or explicit folder, even when absolute.
fn resolve(address: &str, dir: Option<&str>) -> Result<String, String> {
    if rustgis_map::tiles::is_web(address) {
        return Ok(address.into());
    }
    if crate::map::qgis::is_drive_relative(address) {
        return Err("drive-relative path is unsupported".into());
    }
    let dir = dir.ok_or("local data requires a local project path or explicit dir")?;
    let path = std::path::Path::new(address);
    if !crate::map::qgis::is_relative(address) && path.exists() {
        return Ok(address.into());
    }
    {
        let name = address.rsplit(['/', '\\']).next().unwrap_or(address);
        let path = if crate::map::qgis::is_relative(address) {
            rustgis_map::paths::resolve(dir, address)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::Path::new(dir).join(address))
        } else {
            std::path::Path::new(dir).join(name)
        };
        if path.exists() {
            return Ok(path.to_string_lossy().into_owned());
        }
    }
    Err(format!("data file {address} is unavailable (pass dir for moved files)"))
}

/// Finishes an imported URL layer in its reserved position, even after the user switches maps.
/// A deleted or undone placeholder cancels the result rather than adding to a different map.
pub(crate) fn finish_layer(s: &mut Session, p: &Value, name: String, bytes: Vec<u8>) -> CmdResult {
    let pending = p.get("geolibreLayer").ok_or("missing GeoLibre download state")?;
    let token = string(pending, "token");
    if token.is_empty() {
        return Err("missing GeoLibre download token".into());
    }
    if string(&pending["record"], "type") == "mbtiles" {
        return finish_archive(s, p, bytes);
    }
    let target = s
        .project
        .maps
        .iter()
        .enumerate()
        .find_map(|(i, m)| m.layers.iter().find(|l| l.dataset.source.as_deref() == Some(token)).map(|l| (i, l.id)))
        .ok_or("GeoLibre layer was removed while downloading")?;
    let (active, layer) = (s.project.active_map, s.active_layer);
    s.project.active_map = target.0;
    let camera = s.map().camera;
    let result = (|| {
        let before: std::collections::HashSet<_> = s.map().layers.iter().map(|l| l.id).collect();
        super::raster::add_named_bytes(s, p, name, bytes, None)?;
        let ids: Vec<_> = s.map().layers.iter().filter(|l| !before.contains(&l.id)).map(|l| l.id).collect();
        if ids.len() != 1 {
            return Err("GeoLibre URL must contain one map layer".into());
        }
        let id = ids.first().copied().ok_or("download added no layer")?;
        let pos = s.map().layers.iter().position(|l| l.id == id).ok_or("download added no layer")?;
        let mut loaded = s.map_mut().layers.remove(pos);
        loaded.id = target.1;
        if let Some(old) = s.map().layer(target.1) {
            loaded.name = old.name.clone();
            loaded.dataset.name = old.name.clone();
        }
        let mut report = Report::default();
        geolibre::style::apply(&mut loaded, &pending["record"], &mut report);
        let dest = s.map_mut().layer_mut(target.1).ok_or("GeoLibre layer was removed")?;
        loaded.visible = dest.visible;
        loaded.transparency = dest.transparency;
        *dest = loaded;
        for note in report.notes {
            s.notify(format!("{}: {}", note.layer, note.text), false);
        }
        Ok(json!({"layer":target.1}))
    })();
    s.map_mut().camera = camera;
    s.project.active_map = active;
    s.active_layer = layer;
    result
}

/// A runtime token cannot collide with a placeholder saved by an earlier application run.
fn pending_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let time = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("geolibre-pending:{time}:{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Replaces an MBTiles placeholder in the tile stack, preserving its position on late completion.
fn finish_archive(s: &mut Session, p: &Value, bytes: Vec<u8>) -> CmdResult {
    let pending = &p["geolibreLayer"];
    let token = string(pending, "token");
    let target = s
        .project
        .maps
        .iter()
        .position(|m| m.tiles.iter().any(|t| t.id == token))
        .ok_or("GeoLibre archive was removed while downloading")?;
    let (active, layer) = (s.project.active_map, s.active_layer);
    s.project.active_map = target;
    let camera = s.map().camera;
    let result = (|| {
        // A unique upload key keeps two archives with equal filenames independent.
        let filename = format!("{token}.mbtiles");
        let out = super::raster::add_named_bytes(s, p, filename, bytes, None)?;
        let index =
            out.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or("download added no tile archive")?;
        if index >= s.map().tiles.len() {
            return Err("download added no tile archive".into());
        }
        let mut loaded = s.map_mut().tiles.remove(index);
        let dest = s.map().tiles.iter().position(|t| t.id == token).ok_or("GeoLibre archive was removed")?;
        if let Some(t) = s.map_mut().tiles.get_mut(dest) {
            // The reservation already carries the inherited import look and later user edits.
            loaded.visible = t.visible;
            loaded.opacity = t.opacity;
            loaded.blend = t.blend;
            loaded.name = t.name.clone();
            *t = loaded;
        }
        Ok(json!({"index":dest}))
    })();
    s.map_mut().camera = camera;
    s.project.active_map = active;
    s.active_layer = layer;
    result
}

/// Discards a failed download's empty reservation. Tokens, rather than active-map state, ensure
/// a failure cannot remove another layer after switching maps or undoing the import.
pub fn discard_pending(s: &mut Session, p: &Value) {
    let token = p.get("geolibreLayer").map(|v| string(v, "token")).unwrap_or("");
    if token.is_empty() {
        return;
    }
    s.retire_import_history(token);
    for (index, map) in s.project.maps.iter_mut().enumerate() {
        let removed: std::collections::HashSet<_> =
            map.layers.iter().filter(|l| l.dataset.source.as_deref() == Some(token)).map(|l| l.id).collect();
        map.layers.retain(|l| !removed.contains(&l.id));
        map.tiles.retain(|t| t.id != token);
        for group in &mut map.groups {
            group.children.retain(|id| !removed.contains(id));
        }
        if s.project.active_map == index && s.active_layer.is_some_and(|id| removed.contains(&id)) {
            s.active_layer = None;
        }
    }
}

/// Effective visibility and opacity of one folder, including bounded, possibly cyclic ancestry.
fn group_look(groups: &std::collections::HashMap<&str, &Value>, id: &str) -> (bool, f64) {
    let mut seen = std::collections::HashSet::new();
    let mut id = id;
    let (mut visible, mut opacity) = (true, 1.0);
    while !id.is_empty() && seen.insert(id) {
        let Some(group) = groups.get(id).copied() else { break };
        visible &= boolean(group, "visible", true);
        opacity *= number(group, "opacity", 1.0).clamp(0.0, 1.0);
        id = string(group, "parentId");
    }
    (visible, opacity)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod admission_tests {
    use super::*;

    /// Taken opaque requests still occupy capacity; native deferred import must refuse atomically.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn geolibre_native_deferred_import_checks_taken_request_capacity() {
        let mut s = Session::new(crate::project::Project::default());
        s.request_import_url(&json!({"url":"https://capacity.invalid/held.geojson"})).unwrap();
        let held = s.take_url_requests().pop().unwrap();
        let before = serde_json::to_value(&s.project).unwrap();
        let layers: Vec<_> = (0..crate::MAX_URL_REQUESTS)
            .map(|i| json!({"id":i.to_string(),"name":format!("Layer {i}"),"type":"geojson","source":{"url":format!("https://capacity.invalid/{i}.geojson")}}))
            .collect();
        let text = json!({"version":"0.2.0","name":"Capacity","mapView":{},"basemapStyleUrl":"","layers":layers}).to_string();
        let result = s.run_leaving_downloads("project.importGeolibre", &json!({"text":text}));
        assert!(result.is_err(), "an oversized native deferred import returned a partial map");
        assert!(result.unwrap_err().contains("15"), "the held opaque request must occupy its slot");
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
        assert!(s.take_url_requests().is_empty());
        let claim = s.download_started_for(&held).unwrap();
        s.download_failed(claim);
        s.request_import_url(&json!({"url":"https://capacity.invalid/abandoned.geojson"})).unwrap();
        let abandoned = s.take_url_requests().pop().unwrap();
        assert!(ensure_download_capacity(&s, &layers, false).is_err());
        drop(abandoned);
        assert!(ensure_download_capacity(&s, &layers, false).is_ok(), "a dropped opaque handle must release its slot");
    }

    /// A generic raster TIFF deferred by the native loader keeps its import reservation and style.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn geolibre_native_deferred_raster_tiff_keeps_its_reservation() {
        let mut s = Session::new(crate::project::Project::default());
        let record = json!({"id":"r","name":"Raster","type":"raster","source":{"url":"https://capacity.invalid/image.tif"}});
        let text = json!({"version":"0.2.0","name":"Raster","mapView":{},"basemapStyleUrl":"","layers":[record]}).to_string();
        let out = s.run_leaving_downloads("project.importGeolibre", &json!({"text":text})).unwrap();
        assert_eq!(out["layers"], json!(["Raster"]));
        let layer = s.map().layer_by_name("Raster").unwrap();
        assert!(layer.dataset.source.as_deref().is_some_and(|s| s.starts_with("geolibre-pending:")));
        let requests = s.take_url_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].params().get("geolibreLayer").is_some());
    }

    #[test]
    fn geolibre_browser_import_refuses_an_oversized_download_batch_before_mutation() {
        let mut s = Session::new(crate::project::Project::default());
        let layers: Vec<_> =
            (0..17).map(|i| json!({"type":"geojson","source":{"url":format!("https://example.org/{i}.geojson")}})).collect();
        let before = s.project.clone();
        assert!(ensure_download_capacity(&s, &layers, true).unwrap_err().contains("needs 17 downloads"));
        assert_eq!(s.project.maps.len(), before.maps.len());
        assert!(s.url_requests.is_empty());
        assert!(ensure_download_capacity(&s, &layers[..16], true).is_ok());
        s.request_url(&json!({"url":"https://example.org/other.geojson"})).unwrap();
        assert!(ensure_download_capacity(&s, &layers[..16], true).is_err());
        assert!(ensure_download_capacity(&s, &layers[..15], true).is_ok());
    }

    #[test]
    fn geolibre_browser_streaming_and_inline_sources_need_no_download_slots() {
        let mut s = Session::new(crate::project::Project::default());
        for i in 0..crate::MAX_URL_REQUESTS {
            s.request_url(&json!({"url":format!("https://example.org/{i}.geojson")})).unwrap();
        }
        let layers = vec![
            json!({"type":"geojson","sourcePath":"https://example.org/ignored.geojson","geojson":{"type":"FeatureCollection","features":[]}}),
            json!({"type":"cog","source":{"url":"https://example.org/image.tif"}}),
            json!({"type":"raster","source":{"url":"https://example.org/image.tiff?sig=1"}}),
            json!({"type":"raster","source":{"tiles":["https://example.org/{z}/{x}/{y}.png"]}}),
            json!({"type":"pmtiles","source":{"url":"https://example.org/archive.pmtiles"}}),
            json!({"type":"geojson","sourcePath":"local.geojson"}),
            json!({"type":"geojson","metadata":{"sourceKind":"sql-query"},"source":{"url":"https://example.org/query"}}),
        ];
        assert!(ensure_download_capacity(&s, &layers, true).is_ok());
        for kind in ["flatgeobuf", "geoparquet", "zarr", "lidar", "mbtiles", "raster"] {
            assert!(
                ensure_download_capacity(&s, &[json!({"type":kind,"source":{"url":"https://example.org/data"}})], true).is_err(),
                "{kind}"
            );
        }
        assert!(
            ensure_download_capacity(&s, &[json!({"type":"mbtiles","source":{"url":"https://example.org/archive.mbtiles"}})], false)
                .is_ok()
        );
        assert!(ensure_download_capacity(&s, &layers[2..3], false).is_err(), "native deferred TIFFs occupy a whole-file slot");
    }

    /// A long ancestry and a cycle exercise indexed lookup without recursion or repeated scans.
    #[test]
    fn geolibre_group_ancestry_is_indexed_and_cycles_apply_each_folder_once() {
        let groups: Vec<_> = (0..1000).map(|i| json!({"id":i.to_string(),"parentId":if i == 0 {String::new()} else {(i-1).to_string()},"opacity":if i == 0 {0.5} else {1.0}})).collect();
        let index: std::collections::HashMap<_, _> = groups.iter().map(|g| (string(g, "id"), g)).collect();
        for i in 0..1000 {
            assert_eq!(group_look(&index, &i.to_string()), (true, 0.5));
        }
        let cycle = [json!({"id":"a","parentId":"b","opacity":0.5}), json!({"id":"b","parentId":"a","opacity":0.25,"visible":false})];
        let index = cycle.iter().map(|g| (string(g, "id"), g)).collect();
        assert_eq!(group_look(&index, "a"), (false, 0.125));
    }

    /// Moving an earlier import from engine requests into UI-owned downloads must not free slots.
    #[test]
    fn geolibre_browser_admission_counts_zarr_reads_after_the_ui_takes_requests() {
        let mut s = Session::new(crate::project::Project::default());
        let before = s.project.maps.len();
        let layers: Vec<_> = (0..10).map(|i| json!({"type":"zarr","source":{"url":format!("https://example.org/{i}.zarr")}})).collect();
        for i in 0..10 {
            s.request_import_url(&json!({"url":format!("https://example.org/{i}.zarr"),"geolibreZarr":true})).unwrap();
        }
        let requests = s.take_url_requests();
        let ids: Vec<_> = requests.iter().map(|request| s.download_started_for(request).unwrap()).collect();
        assert!(ensure_download_capacity(&s, &layers, true).is_err());
        assert!(ensure_download_capacity(&s, &layers[..6], true).is_ok());
        // Landed bytes still await their completion callback and retain the slot.
        assert!(s.download_landed(ids[0], Vec::new()));
        assert!(ensure_download_capacity(&s, &layers, true).is_err());
        assert!(ensure_download_capacity(&s, &layers[..6], true).is_ok());
        assert_eq!(s.project.maps.len(), before);
        assert!(s.url_requests.is_empty());
    }
}
