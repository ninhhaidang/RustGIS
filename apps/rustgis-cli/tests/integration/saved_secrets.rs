//! One real CLI check of the saved-connection opt-in boundary; no network or keychain reads.

use serde_json::{Value, json};
use std::process::Command;

/// Removes the test's settings folder even when an assertion fails.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn saved_connections_are_loaded_only_with_the_opt_in() {
    let dir = TempDir(std::env::temp_dir().join(format!("rustgis-cli-opt-in-{}", std::process::id())));
    let dir = &dir.0;
    std::fs::create_dir_all(dir).unwrap();
    let connections = json!({"connections": [{
        "id": "c1", "kind": "postgres", "name": "Work",
        "address": "postgres:host=127.0.0.1 port=1 user=u dbname=gis", "remembered": true
    }]});
    std::fs::write(dir.join("prefs.json"), json!({"connections": connections.to_string()}).to_string()).unwrap();
    let list = |opted_in| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rustgis-cli"));
        command.args(["run", "--no-plugins", "--cmd", "connection.list"]).env("RUSTGIS_CONFIG_DIR", dir);
        if opted_in {
            command.arg("--use-saved-secrets");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    assert_eq!(list(false)["connections"], json!([]));
    assert_eq!(list(true)["connections"][0]["name"], "Work");
}
