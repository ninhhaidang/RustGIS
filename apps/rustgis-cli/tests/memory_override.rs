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
    let (mut plain, quiet) = run(None);
    assert_ne!(plain["memorySource"], json!("override"));
    assert!(!quiet.contains("RUSTGIS_MEMORY_BYTES"), "{quiet}");
    // What the computer has free is read live (#2431), so it can differ between the two runs, and
    // the limits it bounds too: they are compared only where much is free.
    let mut junk = run(Some("8 GiB")).0;
    let (a, b) = (plain["freeBytes"].take(), junk["freeBytes"].take());
    let roomy = |free: &Value| free.as_u64().is_none_or(|f| f > plain["memory"].as_u64().unwrap_or(0) / 2);
    if roomy(&a) && roomy(&b) {
        assert_eq!(junk, plain);
    }
}

/// What the process holds reaches `gp.env` (#2431): with the sample project open it is more than
/// nothing, and on a computer that size it is far below half the memory, so the limits are the
/// ones with nothing held.
#[test]
fn gp_env_says_what_is_held() {
    let out = Command::new(env!("CARGO_BIN_EXE_rustgis-cli"))
        .args(["run", "--no-plugins", "--worldview", "default", "--sample", "--cmd", "gp.env"])
        .env("RUSTGIS_MEMORY_BYTES", "17179869184")
        .env_remove("RUSTGIS_WORLDVIEW")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let limits = serde_json::from_slice::<Value>(&out.stdout).unwrap()["limits"].clone();
    let held = limits["heldBytes"].as_u64().unwrap();
    assert!(held > 0 && held < 8 << 30, "{held}");
    // With the override, what else runs on the computer doesn't change the limits.
    assert_eq!(limits["freeBytes"], Value::Null);
    // 16 GiB plans for 4 GiB: 512 MiB of file.
    assert_eq!(limits["fileBytes"], json!(512_u64 << 20));
}
