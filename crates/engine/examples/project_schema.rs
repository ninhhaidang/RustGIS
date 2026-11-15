//! Writes the project file's JSON Schema (`rustgis_engine::schema::PATH`), or with `--check`
//! fails when the committed file differs from what the types generate, or when a fixture project
//! in [`FIXTURES`] doesn't open. Run through `cargo xtask schema [--check]`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rustgis_engine::project::Project;
use rustgis_engine::schema;

/// Fixture projects kept beside the schema: each must open, and `cargo xtask schema --check`'s
/// pre-commit companion validates them against the schema.
const FIXTURES: &str = "schema/fixtures";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn check(root: &Path) -> Result<(), String> {
    let path = root.join(schema::PATH);
    let saved = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", schema::PATH))?;
    if saved.replace("\r\n", "\n") != schema::project_text()? {
        return Err(format!(
            "{} is out of date with the types the project is saved from: run `cargo xtask schema` and commit it",
            schema::PATH
        ));
    }
    let mut fixtures = 0;
    for entry in std::fs::read_dir(root.join(FIXTURES)).map_err(|e| format!("{FIXTURES}: {e}"))? {
        let path = entry.map_err(|e| format!("{FIXTURES}: {e}"))?.path();
        if path.extension().is_some_and(|x| x == "rgis") {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            Project::from_json(&text).map_err(|e| format!("{} doesn't open: {e}", path.display()))?;
            fixtures += 1;
        }
    }
    if fixtures == 0 {
        return Err(format!("no fixture projects (.rgis) in {FIXTURES}"));
    }
    println!("{} matches the types; {fixtures} fixture projects open", schema::PATH);
    Ok(())
}

fn main() -> ExitCode {
    let root = root();
    let result = match std::env::args().nth(1).as_deref() {
        None => schema::project_text().and_then(|text| {
            std::fs::write(root.join(schema::PATH), text)
                .map(|()| println!("wrote {}", schema::PATH))
                .map_err(|e| format!("{}: {e}", schema::PATH))
        }),
        Some("--check") => check(&root),
        Some(other) => Err(format!("unknown argument {other}: usage: cargo xtask schema [--check]")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
