//! GeoJSON publishing: prove that the source survives a read/write before replacing it.

use super::{Published, refuse_link, refused, source_lock};
use crate::sync::{FileSync, Synced};
use crate::{Dataset, Result};
use serde_json::{Value as Json, json};
use std::path::Path;

/// Publishes a whole-read GeoJSON layer. Checks source bytes and the original read's digest,
/// then proves that both the original and the edits survive a round trip. Foreign members,
/// duplicate keys and noncanonical number text are refused. A legacy `crs` member is kept, and
/// the layer is written back in the system it declares, which the layer must still be held in. A file
/// with feature ids permits value/geometry edits, but not row changes: the reader doesn't keep
/// those external ids as layer ids. Parsing, JSON equality and serialization are single steps
/// that finish before Cancel is seen;
/// rendering, source hashing and number-text validation check Cancel between features or blocks.
/// Rendering one feature, hashing prepared bytes or writing/syncing a stage runs to completion
/// before Cancel is seen.
/// The source lock covers every check through the final atomic rename.
pub fn geojson(ds: &Dataset, path: &Path, synced: &Synced) -> Result<Published> {
    let _lock = source_lock(path)?;
    let unchanged = || -> Result<()> {
        refuse_link(path, path)?;
        let changed = synced.files.changed(path)?;
        if changed.is_empty() { Ok(()) } else { Err(refused(path, format!("{} changed since the layer was read", changed.join(", ")))) }
    };
    unchanged()?;
    let bytes = super::read_part(path, crate::io::file_size_error)?.ok_or_else(|| refused(path, "the source file is missing"))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| refused(path, format!("its text isn't UTF-8: {e}")))?;
    // Parse with serde's depth limit before the raw duplicate-key traversal below.
    let original: Json = serde_json::from_str(text).map_err(|e| refused(path, e))?;
    let valid = unique_keys(text).and_then(|()| canonical_numbers(text));
    crate::progress::check()?;
    valid.map_err(|e| refused(path, e))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mut as_read, report) = crate::io::read_geojson(text, &name)?;
    // What the read skipped or said can't be rewritten: refused with what it said, rather than
    // taken for rows the layer added, removed or reordered.
    if report.skipped > 0 || !report.messages.is_empty() {
        let mut said = report.messages.clone();
        if said.is_empty() {
            said.push(format!("{} features couldn't be read", report.skipped));
        }
        return Err(refused(path, format!("reading it isn't lossless ({}), so a rewrite wouldn't keep the file", said.join("; "))));
    }
    // Written back in the system the file declares (its legacy `crs` member, else
    // longitude/latitude), so what the kept member says stays true.
    let declared = as_read.coord_crs();
    if ds.coord_crs() != declared {
        // A layer of a project saved before RustGIS kept a file's own coordinates holds them in
        // longitude/latitude.
        return Err(refused(
            path,
            format!(
                "the file's coordinates are in {}, and the layer holds them in {}; add the file again to publish to it, or export a copy",
                declared.label(),
                ds.coord_crs().label()
            ),
        ));
    }
    if ds.crs.unwrap_or(crate::raster::Crs::LonLat) != declared {
        return Err(refused(
            path,
            format!(
                "the file's coordinates are in {}, and the layer's coordinate system was defined as {} since; export a copy instead",
                declared.label(),
                ds.crs.unwrap_or(ds.coord_crs()).label()
            ),
        ));
    }
    as_read.crs = ds.crs;
    if synced.digest_of(&as_read)? != synced.digest() {
        return Err(refused(path, "it reads otherwise now than when the layer was read; add the file again to compare it"));
    }
    {
        let rebuilt = document(&as_read, &original).map_err(|e| refused(path, e))?;
        if let Some(at) = difference(&original, &rebuilt) {
            return Err(refused(
                path,
                format!(
                    "reading and rewriting it would change {at}; its geometry, properties, numbers or foreign members aren't fully retained"
                ),
            ));
        }
    }
    let edited = document(ds, &original).map_err(|e| refused(path, e))?;
    let written = serde_json::to_vec(&edited).map_err(|e| refused(path, e))?;
    let output = std::str::from_utf8(&written).map_err(|e| refused(path, e))?;
    let (mut read, _) = crate::io::read_geojson(output, &name)?;
    read.crs = ds.crs;
    let names: std::collections::HashSet<&str> = read.fields.iter().map(|f| f.name.as_str()).collect();
    if ds.fields.len() != names.len() || ds.fields.iter().any(|f| !names.contains(f.name.as_str())) {
        return Err(refused(path, "the edited file wouldn't retain the layer's fields (an empty collection has no schema)"));
    }
    if document(&read, &edited).map_err(|e| refused(path, e))? != edited {
        return Err(refused(path, "the file couldn't hold the edited layer's geometry and values"));
    }
    let removed: Vec<&str> = as_read.fields.iter().filter(|f| !names.contains(f.name.as_str())).map(|f| f.name.as_str()).collect();
    let notes = if removed.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "{}: {} properties left out after fields were removed or renamed: {}",
            path.display(),
            removed.len(),
            removed.iter().take(5).map(|n| n.chars().take(80).collect::<String>()).collect::<Vec<_>>().join(", ")
        )]
    };
    let (rows, native) = crate::sync::digests(&read)?;
    let targets = vec![(path.to_path_buf(), written)];
    let files = FileSync::of_written(path, &targets)?;
    let staged = crate::io::stage_shapefile(&targets)?;
    let placed = (|| -> Result<()> {
        let (temporary, _) = staged.first().ok_or_else(|| refused(path, "no file was staged"))?;
        if std::fs::read(temporary)? != targets.first().map(|(_, b)| b.as_slice()).unwrap_or_default() {
            return Err(refused(path, "the staged file didn't read back as written"));
        }
        unchanged()?;
        crate::progress::check()?;
        std::fs::rename(temporary, path)?;
        crate::io::sync_folder(path);
        Ok(())
    })();
    crate::io::unstage(&staged);
    placed?;
    Ok(Published { files, read, rows, native, features: ds.features.len(), notes })
}

/// Builds only the members RustGIS understands. Comparing the original with its rebuild catches
/// anything the reader lost. Keeps a collection's name and a feature's external id; ids require
/// the original one-based row order, since the reader assigns layer ids independently.
fn document(ds: &Dataset, original: &Json) -> std::result::Result<Json, String> {
    let kind = original.get("type").and_then(Json::as_str).ok_or("its type is missing")?;
    let originals: Vec<&Json> = match kind {
        "FeatureCollection" => original.get("features").and_then(Json::as_array).ok_or("its features aren't an array")?.iter().collect(),
        "Feature" => vec![original],
        _ => Vec::new(),
    };
    let has_ids = originals.iter().any(|f| f.get("id").is_some());
    let rows_changed = ds.features.len() != originals.len() || ds.features.iter().enumerate().any(|(i, f)| f.id != i as u64 + 1);
    if has_ids && rows_changed {
        return Err(
            "it has feature ids; publishing added, removed or reordered rows couldn't preserve their identities; export a copy instead"
                .into(),
        );
    }
    // Without fields, each row keeps its original's absent, null or object `properties`, found by
    // position; a changed row set would move those onto other rows.
    let shapes: std::collections::HashSet<Option<bool>> = originals.iter().map(|f| f.get("properties").map(Json::is_null)).collect();
    if ds.fields.is_empty() && shapes.len() > 1 && rows_changed {
        return Err(
            "its features mix absent, null and empty properties; publishing added, removed or reordered rows couldn't keep each one's; export a copy instead"
                .into(),
        );
    }
    // A bbox describes the coordinates, so it is recomputed from what is written; a source box
    // that doesn't match its own coordinates fails the round trip and is refused.
    let feature_boxes = originals.iter().any(|f| f.get("bbox").is_some());
    let mut total = None;
    crate::progress::begin(ds.features.len());
    let mut features = Vec::with_capacity(ds.features.len());
    for (i, f) in ds.features.iter().enumerate() {
        crate::progress::step(1).map_err(|e| e.to_string())?;
        let mut properties = serde_json::Map::new();
        if f.attrs.len() != ds.fields.len() {
            return Err("a row doesn't have one value per field".into());
        }
        for (field, value) in ds.fields.iter().zip(&f.attrs) {
            if value.is_non_finite() {
                return Err(format!("field '{}' has a number JSON can't hold", field.name));
            }
            if properties.insert(field.name.clone(), value.to_json()).is_some() {
                return Err(format!("field name '{}' occurs more than once", field.name));
            }
        }
        let mut feature =
            json!({"type": "Feature", "properties": properties, "geometry": f.geometry.as_ref().map(crate::io::geometry_to_geojson)});
        if let Some(id) = originals.get(i).and_then(|f| f.get("id")) {
            feature["id"] = id.clone();
        }
        // Null and absent properties are distinct from an empty object in the source document.
        if ds.fields.is_empty()
            && let Some(template) = originals.get(i)
            && let Some(object) = feature.as_object_mut()
        {
            match template.get("properties") {
                Some(Json::Null) => {
                    object.insert("properties".into(), Json::Null);
                }
                None => {
                    object.remove("properties");
                }
                _ => {}
            }
        }
        let mut own = None;
        if let Some(geometry) = feature.get("geometry") {
            extent(geometry, &mut own);
        }
        if let Some(own) = own {
            grow(&mut total, own);
            // A row in its place keeps its own choice; after rows change, any box means boxes for all.
            let boxed = match originals.get(i) {
                Some(template) if !rows_changed => template.get("bbox").is_some(),
                _ => feature_boxes,
            };
            if boxed {
                feature["bbox"] = json!(own);
            }
        }
        features.push(feature);
    }
    let mut out = match kind {
        "FeatureCollection" => {
            let mut out = json!({"type": "FeatureCollection", "features": features});
            if let Some(name) = original.get("name") {
                out["name"] = name.clone();
            }
            if original.get("bbox").is_some() {
                let total = total.ok_or("its bbox can't be kept: no geometry is left to bound; export a copy instead")?;
                out["bbox"] = json!(total);
            }
            out
        }
        "Feature" if features.len() == 1 => features.pop().ok_or("the feature is missing")?,
        "Feature" => return Err("a single Feature can't hold added or removed rows; export a collection instead".into()),
        _ if ds.fields.is_empty() && features.len() == 1 => {
            features.first().and_then(|f| f.get("geometry")).cloned().ok_or("the geometry is missing")?
        }
        _ => return Err("a bare geometry can't hold attributes or a different number of rows; export a collection instead".into()),
    };
    // A legacy `crs` member says the system the coordinates are in, which they're written in.
    if let (Some(crs), Some(object)) = (original.get("crs"), out.as_object_mut())
        && matches!(kind, "FeatureCollection" | "Feature")
    {
        object.insert("crs".into(), crs.clone());
    }
    keep_numbers(&mut out, original)?;
    Ok(out)
}

/// Widens `acc` (`[west, south, east, north]`) by the positions of a written GeoJSON geometry:
/// its `coordinates`, or the members of a GeometryCollection. A null geometry adds nothing.
/// The geometry was written by RustGIS from a layer whose nesting the reader bounded.
fn extent(geometry: &Json, acc: &mut Option<[f64; 4]>) {
    match geometry {
        Json::Array(items) if items.first().is_some_and(Json::is_number) => {
            if let (Some(x), Some(y)) = (items.first().and_then(Json::as_f64), items.get(1).and_then(Json::as_f64)) {
                grow(acc, [x, y, x, y]);
            }
        }
        Json::Array(items) => items.iter().for_each(|item| extent(item, acc)),
        Json::Object(object) => {
            if let Some(coordinates) = object.get("coordinates") {
                extent(coordinates, acc);
            }
            if let Some(Json::Array(members)) = object.get("geometries") {
                members.iter().for_each(|member| extent(member, acc));
            }
        }
        _ => {}
    }
}

/// Widens `acc` to cover `other`, both `[west, south, east, north]`.
fn grow(acc: &mut Option<[f64; 4]>, [w, s, e, n]: [f64; 4]) {
    let [aw, as_, ae, an] = *acc.get_or_insert([w, s, e, n]);
    *acc = Some([aw.min(w), as_.min(s), ae.max(e), an.max(n)]);
}

/// Keeps the original JSON number type where it says exactly the same decimal value. Geometry
/// coordinates are held as f64, but an untouched source `1` need not become `1.0`. Decimal text
/// is compared without rounding through f64; exponent forms match only as identical Numbers.
fn keep_numbers(out: &mut Json, original: &Json) -> std::result::Result<(), String> {
    crate::progress::check().map_err(|e| e.to_string())?;
    match (out, original) {
        (Json::Number(a), Json::Number(b)) => {
            if a == b {
                *a = b.clone();
            } else {
                let (a_text, b_text) = (a.to_string(), b.to_string());
                if super::decimal(a_text.as_bytes()).zip(super::decimal(b_text.as_bytes())).is_some_and(|(a, b)| a == b) {
                    *a = b.clone();
                }
            }
        }
        (Json::Object(out), Json::Object(original)) => {
            for (key, value) in out {
                if let Some(old) = original.get(key) {
                    keep_numbers(value, old)?;
                }
            }
        }
        (Json::Array(out), Json::Array(original)) => {
            for (value, old) in out.iter_mut().zip(original) {
                keep_numbers(value, old)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Names the first member or array position a rebuild would change, without printing its
/// value: a member whose JSON type changes says from what to what, an array whose length changes
/// says so. Parsed JSON is depth-limited by serde; each key in this diagnostic is bounded.
fn difference(a: &Json, b: &Json) -> Option<String> {
    let (path, what) = differs(a, b)?;
    let path = if path.is_empty() { "the document".to_string() } else { path.join(".") };
    Some(match what {
        Some(what) => format!("{path}, whose {what}"),
        None => path,
    })
}

/// [`difference`]: the path to the first change, and what changes there beside the value.
fn differs(a: &Json, b: &Json) -> Option<(Vec<String>, Option<String>)> {
    if a == b {
        return None;
    }
    let kind = |v: &Json| match v {
        Json::Null => "null",
        Json::Bool(_) => "true or false",
        Json::Number(_) => "number",
        Json::String(_) => "text",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    };
    let within = |key: String, rest: Option<(Vec<String>, Option<String>)>| {
        rest.map(|(mut path, what)| {
            path.insert(0, key);
            (path, what)
        })
    };
    match (a, b) {
        (Json::Object(a), Json::Object(b)) => {
            for (key, value) in a {
                let shown: String = key.chars().take(80).collect();
                let Some(other) = b.get(key) else { return Some((vec![shown], None)) };
                if let Some(found) = within(shown, differs(value, other)) {
                    return Some(found);
                }
            }
            b.keys().find(|k| !a.contains_key(*k)).map(|k| (vec![k.chars().take(80).collect()], None))
        }
        (Json::Array(a), Json::Array(b)) if a.len() == b.len() => {
            a.iter().zip(b).enumerate().find_map(|(i, (a, b))| within(format!("[{i}]"), differs(a, b)))
        }
        (Json::Array(_), Json::Array(_)) => Some((Vec::new(), Some("length changes".into()))),
        _ if kind(a) != kind(b) => Some((Vec::new(), Some(format!("type changes ({} to {})", kind(a), kind(b))))),
        _ => Some((Vec::new(), None)),
    }
}

/// Refuses number text that a JSON writer would spell differently (including -0, trailing
/// decimal zeros and noncanonical exponent notation). Quotes and escapes are skipped, so numbers inside a
/// text attribute are untouched. JSON validity is checked separately by the deserializer.
fn canonical_numbers(text: &str) -> std::result::Result<(), String> {
    let b = text.as_bytes();
    let (mut i, mut quoted, mut checked) = (0usize, false, 0usize);
    while let Some(&c) = b.get(i) {
        if i.saturating_sub(checked) >= 65536 {
            crate::progress::check().map_err(|e| e.to_string())?;
            checked = i;
        }
        if quoted {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                quoted = false;
            }
        } else if c == b'"' {
            quoted = true;
        } else if c == b'-' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            while b.get(i).is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-')) {
                i += 1;
            }
            let raw = text.get(start..i).ok_or("a number's text isn't valid")?;
            let number: serde_json::Number = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            let spelled = number.to_string();
            if spelled != raw {
                let clip = |t: &str| if t.len() > 40 { format!("{}…", t.chars().take(40).collect::<String>()) } else { t.to_string() };
                return Err(format!("a number at byte {} ({}) would be spelled {} by a rewrite", start + 1, clip(raw), clip(&spelled)));
            }
            continue;
        }
        i += 1;
    }
    Ok(())
}

/// Rejects duplicate decoded keys at every depth without allocating a second JSON tree. The
/// caller first parses `text` as a Value with serde's depth limit, which bounds this traversal;
/// recursively borrowed RawValues are already valid JSON. Containers check Cancel per member.
fn unique_keys(text: &str) -> std::result::Result<(), String> {
    struct Keys;
    impl<'de> serde::de::Visitor<'de> for Keys {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a JSON object or array")
        }
        /// Decodes each key once and refuses duplicates before visiting its raw value.
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
            let mut keys = std::collections::HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                crate::progress::check().map_err(serde::de::Error::custom)?;
                if !keys.insert(key.clone()) {
                    return Err(serde::de::Error::custom(format!("duplicate key '{}'", key.chars().take(80).collect::<String>())));
                }
                let raw: &serde_json::value::RawValue = map.next_value()?;
                unique_keys(raw.get()).map_err(serde::de::Error::custom)?;
            }
            Ok(())
        }
        /// Borrows each element's JSON so the traversal doesn't allocate another array.
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
            while let Some(raw) = seq.next_element::<&serde_json::value::RawValue>()? {
                crate::progress::check().map_err(serde::de::Error::custom)?;
                unique_keys(raw.get()).map_err(serde::de::Error::custom)?;
            }
            Ok(())
        }
    }
    if matches!(text.trim_start().as_bytes().first(), Some(b'{' | b'[')) {
        serde::de::Deserializer::deserialize_any(&mut serde_json::Deserializer::from_str(text), Keys).map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    const SOURCE: &str = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"owner":"Smith"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;

    /// A whole-read GeoJSON and the same sync point Add Data takes.
    fn loaded(text: &str) -> (tempfile::TempDir, std::path::PathBuf, Dataset, Synced) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.geojson");
        std::fs::write(&path, text).unwrap();
        let ds = crate::io::read_path(&path).unwrap().0;
        let sync = Synced::record(FileSync::read(&path).unwrap(), &ds).unwrap();
        (dir, path, ds, sync)
    }

    /// GeoJSON publishing refuses an oversized original with vector recovery advice.
    #[test]
    fn oversized_geojson_publish_gives_vector_advice() {
        use crate::io::{FileSizeAdvice, file_size_error_args, max_bytes};

        let (_dir, path, ds, mut sync) = loaded(SOURCE);
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(max_bytes() + 1).unwrap();
        sync.files = FileSync::read(&path).unwrap();

        let error = geojson(&ds, &path, &sync).err().unwrap().to_string();
        let limit = (max_bytes() >> 20).to_string();
        assert_eq!(file_size_error_args(&error), Some((path.to_str().unwrap(), limit.as_str(), FileSizeAdvice::Vector)));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), max_bytes() + 1);
    }

    /// Mutates an attribute through the shared feature container's copy-on-write API.
    fn rename_owner(ds: &mut Dataset, owner: &str) {
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows.first_mut().unwrap().attrs[0] = Value::Text(owner.into());
        ds.features = rows.into();
    }

    #[test]
    fn an_attribute_edit_keeps_source_members_and_integer_coordinates() {
        let (_dir, path, mut ds, sync) = loaded(SOURCE);
        rename_owner(&mut ds, "Jones");
        let out = geojson(&ds, &path, &sync).unwrap();
        assert!(out.notes.is_empty());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(serde_json::from_str::<Json>(&text).unwrap(), serde_json::from_str::<Json>(&SOURCE.replace("Smith", "Jones")).unwrap());
        assert!(text.contains("[1,2]"));
        assert!(out.files.changed(&path).unwrap().is_empty());
        let sync = Synced { files: out.files, rows: out.rows, native: None };
        rename_owner(&mut ds, "Taylor");
        geojson(&ds, &path, &sync).unwrap();
    }

    /// A link present at load or introduced after the first fingerprint check stays intact.
    #[cfg(unix)]
    #[test]
    fn a_linked_source_is_refused_even_when_introduced_during_preparation() {
        struct Linked {
            path: std::path::PathBuf,
            target: std::path::PathBuf,
            changed: std::sync::atomic::AtomicBool,
        }
        impl Linked {
            /// Replaces the entry with a link to identical bytes, so fingerprint checks alone pass.
            fn link(&self) {
                std::fs::rename(&self.path, &self.target).unwrap();
                std::os::unix::fs::symlink(&self.target, &self.path).unwrap();
            }
        }
        impl crate::progress::Probe for Linked {
            /// Changes the source entry without requesting cancellation.
            fn cancelled(&self) -> bool {
                false
            }
            /// Installs the link when row hashing starts, after the initial file check.
            fn begin(&self, total: u64) {
                if total == 1 && !self.changed.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    self.link();
                }
            }
            /// Progress is irrelevant to this source-change probe.
            fn advance(&self, _: u64) {}
        }
        for during in [false, true] {
            let (dir, path, mut ds, sync) = loaded(SOURCE);
            rename_owner(&mut ds, "Jones");
            let target = dir.path().join("original.geojson");
            let probe = std::sync::Arc::new(Linked { path: path.clone(), target: target.clone(), changed: false.into() });
            if !during {
                probe.link();
            }
            let e = {
                let _installed = crate::progress::install(during.then_some(probe as std::sync::Arc<dyn crate::progress::Probe>));
                geojson(&ds, &path, &sync).err().unwrap().to_string()
            };
            assert!(e.contains("symbolic link") && e.contains("nothing was written"), "{e}");
            assert_eq!(std::fs::read_link(&path).unwrap(), target);
            assert_eq!(std::fs::read_to_string(target).unwrap(), SOURCE);
            assert!(sync.files.changed(&path).unwrap().is_empty());
        }
    }

    /// A file with a legacy `crs` member keeps it, and is written back in its system: an edited
    /// point lands where it was put, in the file's own coordinates.
    #[test]
    fn a_legacy_crs_member_is_kept_and_the_layer_written_back_in_it() {
        let lv95 = r#"{"type":"FeatureCollection","crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::2056"}},"features":[{"type":"Feature","properties":{"owner":"Smith"},"geometry":{"type":"Point","coordinates":[2600000,1200000]}}]}"#;
        let (_dir, path, mut ds, sync) = loaded(lv95);
        let system = crate::raster::Crs::from_epsg(2056).unwrap();
        assert_eq!(ds.coord_crs(), system);
        rename_owner(&mut ds, "Jones");
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows.first_mut().unwrap().geometry = Some(crate::Geometry::Point(rustgis_geom::Coord::new(2_600_010.5, 1_200_020.0)));
        ds.features = rows.into();
        let out = geojson(&ds, &path, &sync).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let want = lv95.replace("Smith", "Jones").replace("[2600000,1200000]", "[2600010.5,1200020.0]");
        assert_eq!(serde_json::from_str::<Json>(&text).unwrap(), serde_json::from_str::<Json>(&want).unwrap());
        assert_eq!(out.read.coord_crs(), system);
        // And again from what it wrote.
        let sync = Synced { files: out.files, rows: out.rows, native: out.native };
        rename_owner(&mut ds, "Taylor");
        geojson(&ds, &path, &sync).unwrap();
        // A `crs` of null says nothing and is kept as it is.
        let null = SOURCE.replace("\"features\":[", "\"crs\":null,\"features\":[");
        let (_dir, path, mut ds, sync) = loaded(&null);
        rename_owner(&mut ds, "Jones");
        geojson(&ds, &path, &sync).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"crs\":null"));
        // A layer whose system was defined again isn't written into a file that says the old one.
        let (_dir, path, ds, sync) = loaded(lv95);
        let redefined = crate::io::define_projection(&ds, crate::raster::Crs::from_epsg(21781).unwrap()).unwrap();
        let e = geojson(&redefined, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("EPSG:2056") && e.contains("EPSG:21781") && e.contains("nothing was written"), "{e}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), lv95);
    }

    #[test]
    fn a_changed_source_is_left_untouched() {
        let (_dir, path, mut ds, sync) = loaded(SOURCE);
        rename_owner(&mut ds, "Jones");
        let changed = SOURCE.replace("Smith", "Someone else");
        std::fs::write(&path, &changed).unwrap();
        assert!(geojson(&ds, &path, &sync).err().unwrap().to_string().contains("changed since"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), changed);
    }

    #[test]
    fn a_lossy_rewrite_is_refused_without_changing_the_file() {
        for text in [
            SOURCE.replace("\"type\":\"FeatureCollection\"", "\"type\":\"FeatureCollection\",\"custom\":42"),
            SOURCE.replace("\"type\":\"Point\"", "\"type\":\"Point\",\"bbox\":[1,2,1,2]"),
            SOURCE.replace("[1,2]", "[1,2,3]"),
            SOURCE.replace("\"features\":[", "\"bbox\":[0,0,9,9],\"features\":["),
            SOURCE.replace("[1,2]", "[1.00,2]"),
            SOURCE.replace("\"owner\":\"Smith\"", "\"owner\":\"Lost\",\"owner\":\"Smith\""),
            SOURCE.replace("\"owner\":\"Smith\"", "\"owner\":{\"name\":\"Smith\"}"),
        ] {
            let (_dir, path, ds, sync) = loaded(&text);
            let error = geojson(&ds, &path, &sync).err().unwrap().to_string();
            assert!(error.contains("nothing was written"), "{error}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    /// The refusals say why: a member whose type a rewrite changes says from what to what, number
    /// text names the writer's spelling, and a file the reader can't read whole says what it said
    /// rather than blaming rows the layer changed.
    #[test]
    fn refusals_give_the_reason() {
        let mixed = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"age":4},"geometry":{"type":"Point","coordinates":[1,2]}},{"type":"Feature","properties":{"age":"old"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;
        let (_dir, path, ds, sync) = loaded(mixed);
        let error = geojson(&ds, &path, &sync).err().unwrap().to_string();
        assert!(error.contains("would change features.[0].properties.age, whose type changes (number to text);"), "{error}");
        assert!(!error.contains("value or size"), "{error}");

        let small = SOURCE.replace("[1,2]", "[1,0.000004]");
        let (_dir, path, ds, sync) = loaded(&small);
        let error = geojson(&ds, &path, &sync).err().unwrap().to_string();
        assert!(error.contains("(0.000004) would be spelled 4e-6 by a rewrite"), "{error}");

        let unreadable = SOURCE
            .replace("]}", r#",{"type":"Feature","properties":{"owner":"Gone"},"geometry":{"type":"Point","coordinates":"nowhere"}}]}"#);
        let (_dir, path, ds, sync) = loaded(&unreadable);
        let error = geojson(&ds, &path, &sync).err().unwrap().to_string();
        assert!(error.contains("reading it isn't lossless") && !error.contains("reordered"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), unreadable);
    }

    #[test]
    fn a_bbox_is_recomputed_from_the_edited_geometry() {
        let text = r#"{"type":"FeatureCollection","bbox":[1,2,5,6],"features":[{"type":"Feature","bbox":[1,2,1,2],"properties":{"owner":"Smith"},"geometry":{"type":"Point","coordinates":[1,2]}},{"type":"Feature","bbox":[3,4,5,6],"properties":{"owner":"Jones"},"geometry":{"type":"LineString","coordinates":[[3,4],[5,6]]}}]}"#;
        let (_dir, path, mut ds, sync) = loaded(text);
        rename_owner(&mut ds, "Taylor");
        geojson(&ds, &path, &sync).unwrap();
        let parsed = |t: &str| serde_json::from_str::<Json>(t).unwrap();
        assert_eq!(parsed(&std::fs::read_to_string(&path).unwrap()), parsed(&text.replace("Smith", "Taylor")));
        let (_dir, path, mut ds, sync) = loaded(text);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows.first_mut().unwrap().geometry = Some(crate::Geometry::Point(crate::Coord::new(7.5, 0.0)));
        ds.features = rows.into();
        geojson(&ds, &path, &sync).unwrap();
        let written: Json = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["bbox"], json!([3.0, 0.0, 7.5, 6]));
        assert_eq!(written["features"][0]["bbox"], json!([7.5, 0.0, 7.5, 0.0]));
        assert_eq!(written["features"][1]["bbox"], json!([3, 4, 5, 6]));
        // A row removed shrinks the collection's box; one added gets a box of its own.
        let (_dir, path, mut ds, sync) = loaded(text);
        let mut rows: Vec<_> = ds.features.iter().skip(1).cloned().collect();
        let mut added = rows.first().unwrap().clone();
        added.geometry = Some(crate::Geometry::Point(crate::Coord::new(4.0, 5.0)));
        rows.push(added);
        ds.features = rows.into();
        geojson(&ds, &path, &sync).unwrap();
        let written: Json = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["bbox"], json!([3.0, 4.0, 5, 6]));
        assert_eq!(written["features"][1]["bbox"], json!([4.0, 5.0, 4.0, 5.0]));
    }

    #[test]
    fn a_feature_without_a_bbox_keeps_none_beside_one_that_has_it() {
        let text = r#"{"type":"FeatureCollection","features":[{"type":"Feature","bbox":[1,2,1,2],"properties":{"owner":"Smith"},"geometry":{"type":"Point","coordinates":[1,2]}},{"type":"Feature","properties":{"owner":"Jones"},"geometry":{"type":"Point","coordinates":[3,4]}}]}"#;
        let (_dir, path, mut ds, sync) = loaded(text);
        rename_owner(&mut ds, "Taylor");
        geojson(&ds, &path, &sync).unwrap();
        let written: Json = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["features"][0]["bbox"], json!([1, 2, 1, 2]));
        assert!(written["features"][1].get("bbox").is_none());
    }

    #[test]
    fn a_collection_bbox_with_nothing_left_to_bound_is_refused() {
        let text = r#"{"type":"FeatureCollection","bbox":[1,2,1,2],"features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;
        let (_dir, path, mut ds, sync) = loaded(text);
        ds.features = Vec::new().into();
        let error = geojson(&ds, &path, &sync).err().unwrap().to_string();
        assert!(error.contains("no geometry is left to bound"), "{error}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn renamed_properties_are_reported_and_an_empty_schema_is_refused() {
        let (_dir, path, mut ds, sync) = loaded(SOURCE);
        ds.fields.first_mut().unwrap().name = "new_owner".into();
        let published = geojson(&ds, &path, &sync).unwrap();
        assert_eq!(published.notes.len(), 1);
        assert!(published.notes.first().unwrap().contains("1 properties left out"));
        assert!(published.notes.first().unwrap().contains("owner"));
        let written = std::fs::read_to_string(&path).unwrap();
        let sync = Synced { files: published.files, rows: published.rows, native: None };
        ds.features = Vec::new().into();
        assert!(geojson(&ds, &path, &sync).err().unwrap().to_string().contains("empty collection has no schema"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), written);
    }

    /// Changes the real source as its first row digest starts, after the initial fingerprint
    /// check. The final source check must keep those external bytes rather than place the stage.
    #[test]
    fn a_source_changed_during_validation_is_not_overwritten() {
        struct ChangedDuringRead {
            path: std::path::PathBuf,
            text: String,
            changed: std::sync::atomic::AtomicBool,
        }
        impl crate::progress::Probe for ChangedDuringRead {
            fn cancelled(&self) -> bool {
                false
            }
            /// Simulates another writer when row hashing starts after the initial file check.
            fn begin(&self, total: u64) {
                if total == 1 && !self.changed.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    std::fs::write(&self.path, &self.text).unwrap();
                }
            }
            fn advance(&self, _: u64) {}
        }
        let (_dir, path, mut ds, sync) = loaded(SOURCE);
        rename_owner(&mut ds, "Jones");
        let text = SOURCE.replace("Smith", "Other");
        {
            let probe = std::sync::Arc::new(ChangedDuringRead { path: path.clone(), text: text.clone(), changed: false.into() });
            let _installed = crate::progress::install(Some(probe));
            assert!(geojson(&ds, &path, &sync).err().unwrap().to_string().contains("changed since"));
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn external_feature_ids_survive_edits_but_refuse_row_changes() {
        let text = SOURCE.replace("\"properties\":", "\"id\":\"parcel-17\",\"properties\":");
        let (_dir, path, mut ds, sync) = loaded(&text);
        rename_owner(&mut ds, "Jones");
        let out = geojson(&ds, &path, &sync).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("parcel-17"));
        let sync = Synced { files: out.files, rows: out.rows, native: None };
        ds.features = Vec::new().into();
        let error = geojson(&ds, &path, &sync).err().unwrap().to_string();
        assert!(error.contains("feature ids"), "{error}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), written);
    }

    #[test]
    fn mixed_property_shapes_refuse_row_changes() {
        let text = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]}},{"type":"Feature","properties":null,"geometry":{"type":"Point","coordinates":[3,4]}}]}"#;
        let (_dir, path, mut ds, sync) = loaded(text);
        assert!(ds.fields.is_empty());
        ds.features = ds.features.iter().skip(1).cloned().collect::<Vec<_>>().into();
        let error = geojson(&ds, &path, &sync).err().unwrap().to_string();
        assert!(error.contains("mix absent, null and empty properties"), "{error}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn a_single_feature_and_bare_geometry_keep_their_document_type() {
        for text in [
            r#"{"type":"Feature","properties":{"owner":"Smith"},"geometry":{"type":"Point","coordinates":[1,2]}}"#,
            r#"{"type":"Point","coordinates":[1,2]}"#,
        ] {
            let (_dir, path, mut ds, sync) = loaded(text);
            let mut rows: Vec<_> = ds.features.iter().cloned().collect();
            rows.first_mut().unwrap().geometry = Some(crate::Geometry::Point(crate::Coord::new(3.0, 4.0)));
            ds.features = rows.into();
            geojson(&ds, &path, &sync).unwrap();
            let original: Json = serde_json::from_str(text).unwrap();
            let written: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(written.get("type"), original.get("type"));
        }
    }
}
