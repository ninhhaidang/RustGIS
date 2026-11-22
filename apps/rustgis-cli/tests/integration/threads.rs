//! `--threads` sets the most threads one piece of work uses (#1562), as `gp.threads` reports it.

use serde_json::{Value, json};
use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rustgis-cli")).args(["run", "--no-plugins"]).args(args).output().unwrap()
}

#[test]
fn the_thread_option_reaches_the_environment() {
    let out = cli(&["--threads", "2", "--cmd", "gp.threads"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let env: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(env["threads"], json!(2), "{env}");
    let cores = env["cores"].as_u64().unwrap();
    assert_eq!(env["max"], json!(cores.min(2)), "{env}");

    let out = cli(&["--cmd", "gp.threads"]);
    let env: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(env["threads"], Value::Null, "the default without the option: {env}");

    for bad in ["0", "1025", "four"] {
        let out = cli(&["--threads", bad, "--cmd", "gp.threads"]);
        assert_eq!(out.status.code(), Some(2), "{bad}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--threads") && err.contains("1 to 1024"), "{bad}: {err}");
    }
    // `--threads=N` is read as `--threads N`; a bad one is refused.
    let out = cli(&["--threads=3", "--cmd", "gp.threads"]);
    let env: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(env["threads"], json!(3), "{env}");
    assert_eq!(cli(&["--threads=0", "--cmd", "gp.threads"]).status.code(), Some(2));
    // The option last, with no value: refused too, not run with the default.
    let out = cli(&["--cmd", "gp.threads", "--threads"]);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
}
