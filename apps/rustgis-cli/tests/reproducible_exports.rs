//! `SOURCE_DATE_EPOCH` pins the time a GeoPackage export records, so the same export run twice in
//! separate processes writes the same bytes (#1998). Run as a child process: the variable is
//! process-wide, and setting it in this one would need `unsafe`.

use std::process::Command;

/// Removes the test's folder even when an assertion fails.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Exports the sample's Cities to `path` with `SOURCE_DATE_EPOCH` set to `epoch`.
fn export(path: &std::path::Path, epoch: &str) -> std::process::Output {
    let cmd = format!(r#"share.exportLayer={{"layer":"Cities","path":{}}}"#, serde_json::json!(path));
    Command::new(env!("CARGO_BIN_EXE_rustgis-cli"))
        .args(["run", "--no-plugins", "--sample", "--cmd", &cmd])
        .env("SOURCE_DATE_EPOCH", epoch)
        .output()
        .unwrap()
}

#[test]
fn a_pinned_time_makes_geopackage_exports_identical() {
    let dir = TempDir(std::env::temp_dir().join(format!("rustgis-cli-epoch-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let (a, b) = (dir.0.join("a.gpkg"), dir.0.join("b.gpkg"));
    for path in [&a, &b] {
        let out = export(path, "1000000000");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let bytes = std::fs::read(&a).unwrap();
    assert!(bytes == std::fs::read(&b).unwrap(), "two exports with the same SOURCE_DATE_EPOCH differ");
    let stamp = b"2001-09-09T01:46:40.000Z";
    assert!(bytes.windows(stamp.len()).any(|w| w == stamp), "last_change isn't the pinned time");

    // A value that isn't seconds since 1970 refuses the export and names the variable.
    let out = export(&dir.0.join("c.gpkg"), "yesterday");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("SOURCE_DATE_EPOCH"), "{err}");
}
