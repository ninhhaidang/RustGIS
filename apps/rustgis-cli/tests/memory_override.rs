//! `RUSTGIS_MEMORY_BYTES` stands in for the computer's memory, so the limits `gp.env` reports
//! follow it (`rustgis_data::memory::physical`), and both `gp.env` and the command line say so.

use serde_json::{Value, json};
use std::process::Command;

/// `gp.env`'s `limits` and the command line's stderr from a fresh CLI process, with the override
/// set to `memory` or removed.
fn run(memory: Option<&str>) -> (Value, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rustgis-cli"));
    cmd.args(["run", "--no-plugins", "--worldview", "default", "--cmd", "gp.env"])
        .env_remove("RUSTGIS_MEMORY_BYTES")
        .env_remove("RUSTGIS_WORLDVIEW");
    if let Some(m) = memory {
        cmd.env("RUSTGIS_MEMORY_BYTES", m);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let env: Value = serde_json::from_slice(&out.stdout).unwrap();
    (env["limits"].clone(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn the_memory_override_sets_the_limits_and_says_so() {
    // 8 GiB: a quarter of it, 2 GiB, for the undo history.
    let (limits, said) = run(Some("8589934592"));
    assert_eq!(limits["undoBytes"], json!(2_u64 << 30));
    assert_eq!(limits["memorySource"], json!("override"));
    assert!(said.contains("memory limits follow RUSTGIS_MEMORY_BYTES (8.0 GiB)"), "{said}");
    // 1 GiB: a quarter is the undo history's 256 MiB floor.
    assert_eq!(run(Some("1073741824")).0["undoBytes"], json!(256_u64 << 20));
    // Without it, or with a value that isn't a positive number of bytes, nothing changes or is said.
    let (plain, quiet) = run(None);
    assert_ne!(plain["memorySource"], json!("override"));
    assert!(!quiet.contains("RUSTGIS_MEMORY_BYTES"), "{quiet}");
    assert_eq!(run(Some("8 GiB")).0, plain);
}
