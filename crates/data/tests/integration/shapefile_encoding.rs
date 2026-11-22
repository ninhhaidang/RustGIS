//! Shapefile text is decoded at the same public boundaries used by desktop and browser imports.

use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value, io};

/// Opens a Shapefile in place, with its index in this process's own temporary cache rather than
/// the user's (#1230).
#[cfg(not(target_arch = "wasm32"))]
fn open_shp(path: &std::path::Path, name: &str) -> rustgis_data::Result<rustgis_data::source::shp::ShpSource> {
    rustgis_data::source::shp::use_temporary_index_dir();
    rustgis_data::source::shp::ShpSource::open_path(path, name)
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn shapefile_indexes_go_to_the_temporary_cache() {
    let cache = rustgis_data::source::shp::use_temporary_index_dir();
    let indexes = || match std::fs::read_dir(&cache) {
        Ok(entries) => entries.filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".sqlite")).count(),
        Err(_) => 0,
    };
    let before = indexes();
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in parts(b"Bogota", 0x57, None) {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    // Held open, so no sweep can evict its index before it is counted.
    let _source = open_shp(&dir.path().join("places.shp"), "places").unwrap();
    assert!(indexes() > before, "no new index in {}", cache.display());
}

/// A single point with one 32-byte character field. The bytes are supplied directly rather
/// than encoded by the decoder under test; byte 29 is the DBF language-driver identifier.
fn parts(text: &[u8], ldid: u8, cpg: Option<&[u8]>) -> Vec<(String, Vec<u8>)> {
    named_parts(b"name", text, ldid, cpg, 1)
}

fn named_parts(field: &[u8], text: &[u8], ldid: u8, cpg: Option<&[u8]>, rows: usize) -> Vec<(String, Vec<u8>)> {
    assert!(field.len() <= 10 && text.len() <= 32);
    let mut ds = Dataset::new("places", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for i in 0..rows {
        ds.push(Some(Geometry::Point(rustgis_data::Coord::new(1.0 + i as f64, 2.0))), vec![Value::Text("x".repeat(32))]);
    }
    io::shapefile_parts(&ds)
        .unwrap()
        .0
        .into_iter()
        .filter_map(|(ext, mut bytes)| {
            if ext == "cpg" {
                return cpg.map(|value| ("places.cpg".into(), value.to_vec()));
            }
            if ext == "dbf" {
                bytes[29] = ldid;
                bytes[32..43].fill(0);
                bytes[32..32 + field.len()].copy_from_slice(field);
                let header = usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
                let record = usize::from(u16::from_le_bytes([bytes[10], bytes[11]]));
                for row in 0..rows {
                    let start = header + row * record + 1;
                    bytes[start..start + 32].fill(b' ');
                    bytes[start..start + text.len()].copy_from_slice(text);
                }
            }
            Some((format!("places.{ext}"), bytes))
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn loose_shapefile_honors_cpg_before_dbf_code_page() {
    let dir = tempfile::tempdir().unwrap();
    // LDID 0xF0 claims UTF-8; the sidecar correctly identifies Windows-1252.
    for (name, bytes) in parts(b"Montr\xe9al \x96 Qu\xe9bec", 0xf0, Some(b"1252")) {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let (ds, _) = io::read_path(&dir.path().join("places.shp")).unwrap();
    assert_eq!(ds.value(0, 0), &Value::Text("Montréal – Québec".into()));
}

#[test]
fn zipped_shapefile_honors_its_own_cpg_in_a_folder() {
    let entries: Vec<_> = parts(b"Montr\xe9al \x96 Qu\xe9bec", 0xf0, Some("\u{feff} Windows-1252\r\n".as_bytes()))
        .into_iter()
        .map(|(name, bytes)| (format!("data/{}", name.to_ascii_uppercase()), bytes))
        .chain([("elsewhere/PLACES.CPG".into(), b"UTF-8".to_vec())])
        .collect();
    let zip = rustgis_data::zip::write(&entries).unwrap();
    let layers = io::read_zip("upload.zip", &zip).unwrap();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].0.value(0, 0), &Value::Text("Montréal – Québec".into()));
    assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn streamed_shapefile_decodes_cpg_in_sampled_and_complete_reads() {
    use rustgis_data::source::{FeatureSource, Request};
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in parts(b"Montr\xe9al \x96 Qu\xe9bec", 0xf0, Some(b"1252")) {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let src = open_shp(&dir.path().join("places.shp"), "places").unwrap();
    let expected = vec![Value::Text("Montréal – Québec".into())];
    assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, expected);
    let mut attrs = Vec::new();
    src.visit_chunks(None, 1024, &mut |features| {
        attrs.extend(features.into_iter().map(|f| f.attrs));
        Ok(())
    })
    .unwrap();
    assert_eq!(attrs, vec![expected]);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn shapefile_uses_dbf_code_page_when_cpg_is_absent() {
    use rustgis_data::source::{FeatureSource, Request};
    // Esri documents both 0x03 and 0x57 as Windows-1252; C9 is Windows-1251.
    for (ldid, text, expected) in [
        (0x03, b"Montr\xe9al \x96 Qu\xe9bec".as_slice(), "Montréal – Québec"),
        (0x57, b"Montr\xe9al \x96 Qu\xe9bec".as_slice(), "Montréal – Québec"),
        (0xc9, b"\xcc\xee\xf1\xea\xe2\xe0".as_slice(), "Москва"),
    ] {
        let files = parts(text, ldid, None);
        let zip = rustgis_data::zip::write(&files).unwrap();
        assert_eq!(io::read_zip("places.zip", &zip).unwrap()[0].0.value(0, 0), &Value::Text(expected.into()), "LDID {ldid:02X}");
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("places.shp");
        assert_eq!(io::read_path(&path).unwrap().0.value(0, 0), &Value::Text(expected.into()), "LDID {ldid:02X}");
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, [Value::Text(expected.into())], "LDID {ldid:02X}");
    }
}

/// Language driver IDs dbase doesn't know, as GDAL reads them (#1079): Spanish and Western
/// European ANSI, the Baltic, Korean and Mac pages, and DOS ones by their other numbers.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn language_drivers_dbase_lacks_decode_text_without_cpg_in_all_readers() {
    use rustgis_data::source::{FeatureSource, Request};
    for (ldid, text, expected) in [
        (0x59, b"\xc1guilas".as_slice(), "Águilas"),
        (0x58, b"\xc1guilas".as_slice(), "Águilas"),
        (0xcc, b"\xd0iauliai".as_slice(), "Šiauliai"),
        (0x4e, b"\xbc\xad\xbf\xef".as_slice(), "서울"),
        (0x26, b"\x8c\xae\xe1\xaa\xa2\xa0".as_slice(), "Москва"),
        (0x1f, b"\xacesk\xec".as_slice(), "Český"),
        (0x37, b"Se\xa4or".as_slice(), "Señor"),
        (0x04, b"Caf\x8e".as_slice(), "Café"),
        (0x96, b"\x8a\xe8\xe5\xe2".as_slice(), "Киев"),
    ] {
        let files = parts(text, ldid, None);
        let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
        assert_eq!(layers[0].0.value(0, 0), &Value::Text(expected.into()), "LDID {ldid:02X}");
        assert!(layers[0].1.messages.is_empty(), "LDID {ldid:02X}: {:?}", layers[0].1.messages);
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("places.shp");
        assert_eq!(io::read_path(&path).unwrap().0.value(0, 0), &Value::Text(expected.into()), "LDID {ldid:02X}");
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, [Value::Text(expected.into())], "LDID {ldid:02X}");
        assert!(src.info().notes.is_empty(), "LDID {ldid:02X}: {:?}", src.info().notes);
    }
}

/// A language driver whose code page RustGIS can't decode (DOS 863, French Canadian) is named in
/// the note, and the text read as UTF-8.
#[test]
fn undecodable_dbf_code_page_is_named_in_the_utf8_fallback() {
    let files = parts("Montréal".as_bytes(), 0x6c, None);
    let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
    assert_eq!(layers[0].0.value(0, 0), &Value::Text("Montréal".into()));
    let said = &layers[0].1.messages;
    assert!(said.iter().any(|m| m.contains("0x6C (code page 863, DOS French Canadian)") && m.contains("UTF-8")), "{said:?}");
}

#[test]
fn unknown_dbf_code_page_reports_utf8_fallback() {
    let files = parts("Montréal".as_bytes(), 0xff, None);
    let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
    assert_eq!(layers[0].0.value(0, 0), &Value::Text("Montréal".into()));
    assert!(layers[0].1.messages.iter().any(|m| m.contains("0xFF") && m.contains("UTF-8")), "{:?}", layers[0].1.messages);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn unusable_cpg_reports_fallback_to_dbf_in_all_readers() {
    use rustgis_data::source::{FeatureSource, Request};
    let mut oversized = b"1252".to_vec();
    oversized.resize(2048, b' ');
    for cpg in [Vec::new(), b"not-an-encoding".to_vec(), b"-437".to_vec(), b"UTF-16LE".to_vec(), vec![0xff, 0xfe], oversized] {
        let files = parts(b"Montr\xe9al", 0x03, Some(&cpg));
        let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
        assert_eq!(layers[0].0.value(0, 0), &Value::Text("Montréal".into()));
        assert!(layers[0].1.messages.iter().any(|m| m.contains(".cpg")), "{:?}", layers[0].1.messages);
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("places.shp");
        let (ds, report) = io::read_path(&path).unwrap();
        assert_eq!(ds.value(0, 0), &Value::Text("Montréal".into()));
        assert!(report.messages.iter().any(|m| m.contains(".cpg")), "{:?}", report.messages);
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, [Value::Text("Montréal".into())]);
        assert!(src.info().notes.iter().any(|m| m.contains(".cpg")), "{:?}", src.info().notes);
    }
}

#[test]
fn utf8_shapefile_text_survives_missing_metadata_and_utf8_cpg() {
    for cpg in [None, Some(b"UTF-8".as_slice()), Some(b"65001".as_slice())] {
        let files = parts("München – 東京".as_bytes(), if cpg.is_some() { 0x03 } else { 0 }, cpg);
        let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
        assert_eq!(layers[0].0.value(0, 0), &Value::Text("München – 東京".into()));
        assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_imports_discover_every_cpg_extension_case() {
    use rustgis_data::source::{FeatureSource, Request};
    for ext in ["cpg", "CPG", "Cpg", "cPg", "cpG", "CPg", "CpG", "cPG"] {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in parts(b"Montr\xe9al", 0xf0, Some(b"1252")) {
            let name = if name.ends_with(".cpg") { format!("places.{ext}") } else { name };
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("places.shp");
        let (ds, report) = io::read_path(&path).unwrap();
        assert_eq!(ds.value(0, 0), &Value::Text("Montréal".into()), "extension {ext}");
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, [Value::Text("Montréal".into())], "extension {ext}");
        assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn unreadable_cpg_reports_fallback_without_preventing_native_imports() {
    use rustgis_data::source::{FeatureSource, Request};
    for (ldid, text, expected) in [(0x03, b"Montr\xe9al".as_slice(), "Montréal"), (0, "München".as_bytes(), "München")] {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in parts(text, ldid, None) {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        // A directory at the sidecar path deterministically causes an I/O failure
        // on both Unix and Windows, without depending on the test user's privileges.
        std::fs::create_dir(dir.path().join("places.cpg")).unwrap();
        let path = dir.path().join("places.shp");
        let (ds, report) = io::read_path(&path).unwrap();
        assert_eq!(ds.value(0, 0), &Value::Text(expected.into()));
        assert!(report.messages.iter().any(|m| m.contains(".cpg") && m.contains("read")), "{:?}", report.messages);
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, [Value::Text(expected.into())]);
        let mut attrs = Vec::new();
        src.visit_chunks(None, 1024, &mut |features| {
            attrs.extend(features.into_iter().map(|f| f.attrs));
            Ok(())
        })
        .unwrap();
        assert_eq!(attrs, vec![vec![Value::Text(expected.into())]]);
        assert!(src.info().notes.iter().any(|m| m.contains(".cpg") && m.contains("read")), "{:?}", src.info().notes);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn shapefile_decodes_legacy_field_names_in_all_readers() {
    use rustgis_data::source::FeatureSource;
    let mut files = parts(b"Montr\xe9al", 0x03, None);
    let dbf = &mut files.iter_mut().find(|(n, _)| n.ends_with(".dbf")).unwrap().1;
    dbf[32..43].fill(0);
    dbf[32..36].copy_from_slice(b"caf\xe9");
    let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
    assert_eq!(layers[0].0.fields[0].name, "café");
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let path = dir.path().join("places.shp");
    assert_eq!(io::read_path(&path).unwrap().0.fields[0].name, "café");
    assert_eq!(open_shp(&path, "places").unwrap().info().fields[0].name, "café");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn dos_cpg_decodes_attributes_and_field_names_in_all_readers() {
    use rustgis_data::source::{FeatureSource, Request};
    // The same E0 byte is Greek alpha in CP437 and capital O acute in CP850.
    for (cpg, expected) in [("437", "Café α"), ("CP437", "Café α"), ("850", "Café Ó"), ("IBM850", "Café Ó")] {
        let mut files = parts(b"Caf\x82 \xe0", 0xf0, Some(cpg.as_bytes()));
        let dbf = &mut files.iter_mut().find(|(n, _)| n.ends_with(".dbf")).unwrap().1;
        dbf[32..43].fill(0);
        dbf[32..36].copy_from_slice(b"caf\x82");
        let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
        assert_eq!(layers[0].0.fields[0].name, "café", "CPG {cpg}");
        assert_eq!(layers[0].0.value(0, 0), &Value::Text(expected.into()), "CPG {cpg}");
        assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("places.shp");
        let (ds, report) = io::read_path(&path).unwrap();
        assert_eq!(ds.fields[0].name, "café");
        assert_eq!(ds.value(0, 0), &Value::Text(expected.into()));
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.info().fields[0].name, "café");
        assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, [Value::Text(expected.into())]);
        let mut attrs = Vec::new();
        src.visit_chunks(None, 1024, &mut |features| {
            attrs.extend(features.into_iter().map(|f| f.attrs));
            Ok(())
        })
        .unwrap();
        assert_eq!(attrs, vec![vec![Value::Text(expected.into())]]);
        assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn dos_dbf_markers_decode_text_without_cpg_in_all_readers() {
    use rustgis_data::source::{FeatureSource, Request};
    for (ldid, expected) in [(0x01, "Café α"), (0x02, "Café Ó"), (0x09, "Café α"), (0x0a, "Café Ó")] {
        let files = parts(b"Caf\x82 \xe0", ldid, None);
        let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
        assert_eq!(layers[0].0.value(0, 0), &Value::Text(expected.into()), "LDID {ldid:02X}");
        assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("places.shp");
        let (ds, report) = io::read_path(&path).unwrap();
        assert_eq!(ds.value(0, 0), &Value::Text(expected.into()));
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, [Value::Text(expected.into())]);
        assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
    }
}

#[test]
fn dos_support_preserves_other_encodings_and_lossy_utf8_fallback() {
    for (cpg, ldid, text, expected) in [
        (Some(b"ISO-8859-2".as_slice()), 0xf0, b"\xa1".as_slice(), "Ą"),
        (Some(b"CP932".as_slice()), 0xf0, b"\x93\x8c\x8b\x9e".as_slice(), "東京"),
        (Some(b"UTF-8".as_slice()), 0xf0, b"Caf\xff".as_slice(), "Caf�"),
        // With no encoding metadata, preserve the old UTF-8 fallback's treatment of
        // field content: a leading U+FEFF is data, and invalid bytes are replaced.
        (None, 0, b"\xef\xbb\xbfCaf\xff".as_slice(), "\u{feff}Caf�"),
    ] {
        let files = parts(text, ldid, cpg);
        let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
        assert_eq!(layers[0].0.value(0, 0), &Value::Text(expected.into()));
        assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn utf8_metadata_preserves_leading_bom_in_fields_and_values() {
    use rustgis_data::source::{FeatureSource, Request};
    for (cpg, ldid) in [(None, 0), (None, 0xf0), (Some(b"UTF-8".as_slice()), 0x03), (Some(b"65001".as_slice()), 0x03)] {
        // A BOM inside a fixed-width DBF field is content, not a file signature.
        let mut files = parts(b"\xef\xbb\xbfCaf\xff", ldid, cpg);
        let dbf = &mut files.iter_mut().find(|(n, _)| n.ends_with(".dbf")).unwrap().1;
        dbf[32..43].fill(0);
        dbf[32..39].copy_from_slice(b"\xef\xbb\xbfname");
        let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
        assert_eq!(layers[0].0.fields[0].name, "\u{feff}name");
        assert_eq!(layers[0].0.value(0, 0), &Value::Text("\u{feff}Caf�".into()));
        assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("places.shp");
        let (ds, report) = io::read_path(&path).unwrap();
        assert_eq!(ds.fields[0].name, "\u{feff}name");
        assert_eq!(ds.value(0, 0), &Value::Text("\u{feff}Caf�".into()));
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.info().fields[0].name, "\u{feff}name");
        assert_eq!(src.read(&Request::all()).unwrap().features[0].attrs, [Value::Text("\u{feff}Caf�".into())]);
        let mut attrs = Vec::new();
        src.visit_chunks(None, 1024, &mut |features| {
            attrs.extend(features.into_iter().map(|f| f.attrs));
            Ok(())
        })
        .unwrap();
        assert_eq!(attrs, vec![vec![Value::Text("\u{feff}Caf�".into())]]);
        assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
    }
}

// Independently specified bytes and Unicode, not generated by DbfEncoding or CodePageMark.
// Marker sources and the intentional unsupported pages: docs/user-guide/shapefile-encoding.md.
const SUPPORTED_LDIDS: &[(&[u8], &[u8], &str)] = &[
    // CP437
    (&[0x01, 0x09, 0x0B, 0x0D, 0x0F, 0x11, 0x15, 0x18, 0x19, 0x1B], b"Caf\x82 \xe0", "Café α"),
    // CP850
    (&[0x02, 0x0A, 0x0E, 0x10, 0x12, 0x14, 0x16, 0x1A, 0x1D, 0x25, 0x37], b"Caf\x82 \xe0", "Café Ó"),
    // CP852
    (&[0x1F, 0x22, 0x23, 0x40, 0x64, 0x87], b"\xacesk\xec", "Český"),
    // CP861
    (&[0x67], b"\x8b\xe0", "Ðα"),
    // CP865
    (&[0x08, 0x17, 0x66], b"\x9b\xe0", "øα"),
    // CP866
    (&[0x26, 0x65], b"\x8c\xae\xe1\xaa\xa2\xa0", "Москва"),
    // CP874
    (&[0x50, 0x7C], b"\xe4\xb7\xc2", "ไทย"),
    // CP932
    (&[0x13, 0x7B], b"\x93\x8c\x8b\x9e", "東京"),
    // CP936
    (&[0x4D, 0x7A], b"\xd6\xd0\xce\xc4", "中文"),
    // CP949
    (&[0x4E, 0x79], b"\xbc\xad\xbf\xef", "서울"),
    // CP950
    (&[0x4F, 0x78], b"\xa4\xa4\xa4\xe5", "中文"),
    // Windows-1250
    (&[0xC8], b"\xa3\xf3d\x9f", "Łódź"),
    // Windows-1251
    (&[0xC9], b"\xcc\xee\xf1\xea\xe2\xe0", "Москва"),
    // Windows-1252
    (&[0x03, 0x57, 0x58, 0x59], b"\xc1guilas", "Águilas"),
    // Windows-1253
    (&[0xCB], b"\xc1\xe8\xde\xed\xe1", "Αθήνα"),
    // Windows-1254
    (&[0xCA], b"\xddzmir", "İzmir"),
    // Windows-1255
    (&[0x7D], b"\xf9\xec\xe5\xed", "שלום"),
    // Windows-1256
    (&[0x7E], b"\xda\xd1\xc8\xed", "عربي"),
    // Windows-1257
    (&[0xCC], b"\xd0iauliai", "Šiauliai"),
    // Mac Roman
    (&[0x04], b"Caf\x8e", "Café"),
    // Mac Cyrillic's shared letters with x-mac-cyrillic
    (&[0x96], b"\x8a\xe8\xe5\xe2", "Киев"),
    // Default and explicit UTF-8
    (&[0x00, 0xF0], b"\xc3\xa9\xe4\xba\xac", "é京"),
];

const UNSUPPORTED_LDIDS: &[(u8, &str)] = &[
    (0x1C, "863, DOS French Canadian"),
    (0x6C, "863, DOS French Canadian"),
    (0x24, "860, DOS Portuguese"),
    (0x68, "895, DOS Czech (Kamenický)"),
    (0x69, "620, DOS Polish (Mazovia)"),
    (0x6A, "737, DOS Greek"),
    (0x86, "737, DOS Greek"),
    (0x6B, "857, DOS Turkish"),
    (0x88, "857, DOS Turkish"),
    (0x97, "10029, Mac Central European"),
];

/// Three records make a limit-one read genuinely sampled; names and values use the same bytes.
fn check_ldid_readers(ldid: u8, text: &[u8], expected: &str, cpg: Option<&[u8]>, notes: &[&str]) {
    let files = named_parts(text, text, ldid, cpg, 3);
    let check = |ds: &Dataset, rows: usize| {
        assert_eq!(ds.fields.len(), 1, "LDID {ldid:02X}");
        assert_eq!(ds.fields[0].name, expected, "LDID {ldid:02X} field name");
        assert_eq!(ds.features.len(), rows, "LDID {ldid:02X} records");
        for row in 0..rows {
            assert_eq!(ds.value(row, 0), &Value::Text(expected.into()), "LDID {ldid:02X} row {row}");
        }
    };
    let layers = io::read_zip("places.zip", &rustgis_data::zip::write(&files).unwrap()).unwrap();
    assert_eq!(layers.len(), 1);
    check(&layers[0].0, 3);
    assert_eq!(layers[0].1.skipped, 0);
    assert_eq!(layers[0].1.messages, notes, "LDID {ldid:02X} ZIP notes");
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{FeatureSource, Request};
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("places.shp");
        let (ds, report) = io::read_path(&path).unwrap();
        check(&ds, 3);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.messages, notes, "LDID {ldid:02X} loose-file notes");
        let src = open_shp(&path, "places").unwrap();
        assert_eq!(src.info().fields[0].name, expected, "LDID {ldid:02X} streamed field name");
        assert_eq!(src.info().notes, notes, "LDID {ldid:02X} streamed notes");
        for (request, rows, complete) in [(Request { extent: None, limit: 1 }, 1, false), (Request::all(), 3, true)] {
            let batch = src.read(&request).unwrap();
            assert_eq!(batch.matched, 3);
            assert_eq!(batch.complete, complete);
            assert_eq!(batch.skipped, 0);
            assert_eq!(batch.features.len(), rows);
            for feature in batch.features {
                assert_eq!(feature.attrs, [Value::Text(expected.into())], "LDID {ldid:02X} streamed value");
            }
        }
        let mut values = Vec::new();
        let batch = src
            .visit_chunks(None, 1, &mut |features| {
                values.extend(features.into_iter().map(|f| f.attrs));
                Ok(())
            })
            .unwrap();
        assert!(batch.complete);
        assert_eq!(batch.matched, 3);
        assert_eq!(batch.skipped, 0);
        assert_eq!(values, vec![vec![Value::Text(expected.into())]; 3], "LDID {ldid:02X} chunked values");
    }
}

#[test]
fn every_supported_ldid_decodes_names_and_values_in_all_readers() {
    for &(markers, text, expected) in SUPPORTED_LDIDS {
        for &ldid in markers {
            check_ldid_readers(ldid, text, expected, None, &[]);
        }
    }
}

#[test]
fn documented_unsupported_ldids_and_unknown_markers_keep_lossy_utf8_with_a_diagnostic() {
    for &(ldid, page) in UNSUPPORTED_LDIDS {
        let note = format!("Shapefile: unsupported .dbf code page 0x{ldid:02X} (code page {page}); using UTF-8");
        check_ldid_readers(ldid, b"\xc3\xa9\xff", "é�", None, &[&note]);
    }
    check_ldid_readers(0xFF, b"\xc3\xa9\xff", "é�", None, &["Shapefile: unsupported .dbf code page 0xFF; using UTF-8"]);
}

#[test]
fn cpg_overrides_every_supported_or_unsupported_ldid() {
    let markers =
        SUPPORTED_LDIDS.iter().flat_map(|(ids, _, _)| ids.iter().copied()).chain(UNSUPPORTED_LDIDS.iter().map(|(id, _)| *id)).chain([0xFF]);
    for ldid in markers {
        check_ldid_readers(ldid, b"\xc3\xa9\xe4\xba\xac", "é京", Some(b"UTF-8"), &[]);
    }
}

#[test]
fn unusable_cpg_falls_back_to_each_supported_decoder() {
    let note = "Shapefile: unsupported or empty .cpg; using the .dbf code page, or UTF-8 if unavailable";
    for &(markers, text, expected) in SUPPORTED_LDIDS {
        check_ldid_readers(markers[0], text, expected, Some(b"not-an-encoding"), &[note]);
    }
    check_ldid_readers(0xFF, b"\xc3\xa9\xff", "é�", Some(b""), &[note, "Shapefile: unsupported .dbf code page 0xFF; using UTF-8"]);
}

#[test]
fn esri_57_remains_windows_1252_rather_than_iso_8859_1() {
    check_ldid_readers(0x57, b"\x80\x96\x9f", "€–Ÿ", None, &[]);
}

#[test]
fn malformed_multibyte_fields_replace_bytes_without_dropping_records() {
    for (ldid, text) in [(0x13, b"X\x82".as_slice()), (0x4D, b"X\x81"), (0x4E, b"X\x81"), (0x4F, b"X\xa4"), (0xF0, b"X\xff")] {
        check_ldid_readers(ldid, text, "X�", None, &[]);
    }
}
