//! Writes the command surface snapshot (`rustgis_engine::surface::PATH`) from the registry.
//! Run through `cargo xtask commands`.

use std::path::Path;
use std::process::ExitCode;

use rustgis_engine::{cmd, surface};

/// Rewrites the snapshot file; fails when it can't be written.
fn main() -> ExitCode {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(surface::PATH);
    let commands = cmd::all();
    match std::fs::write(&path, surface::text(&commands)) {
        Ok(()) => {
            println!("wrote {} ({} commands)", surface::PATH, commands.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", surface::PATH);
            ExitCode::FAILURE
        }
    }
}
