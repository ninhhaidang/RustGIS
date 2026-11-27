//! A flag that takes a value and gets none, and a `render` size that isn't a number, are refused
//! with status 2 instead of being ignored (#2917).

use std::path::PathBuf;
use std::process::{Command, Output};

/// A temporary folder the program runs in, removed even when an assertion fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("rustgis-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cli(dir: &TempDir, args: &[&str]) -> Output {
    // The program's settings, plugins, data and cache folders are the test's own, so no installed
    // plugin or setting of the person running the test is read, and `RUSTGIS_WORLDVIEW` doesn't
    // choose a view to download. (`--no-plugins` would sit where `render` and `logo` read their
    // first argument.)
    Command::new(env!("CARGO_BIN_EXE_rustgis-cli"))
        .current_dir(&dir.0)
        .env("RUSTGIS_CONFIG_DIR", dir.0.join("config"))
        .env("RUSTGIS_PLUGINS_DIR", dir.0.join("plugins"))
        .env("RUSTGIS_APP_DATA_DIR", dir.0.join("data"))
        .env("RUSTGIS_CACHE_DIR", dir.0.join("cache"))
        .env_remove("RUSTGIS_WORLDVIEW")
        .args(args)
        .output()
        .unwrap()
}

fn said(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_flag_without_its_value_is_refused_not_ignored() {
    let dir = TempDir::new("flag-values");
    let cases: [(&[&str], &str); 11] = [
        (&["run", "--cmd", "gp.threads", "--save"], "run --save: expected a value"),
        (&["run", "--cmd", "gp.threads", "--save", "--cmd", "gp.threads"], "run --save: expected a value, found the option --cmd"),
        (&["run", "--data"], "run --data: expected a value"),
        (&["run", "--cmd"], "run --cmd: expected a value"),
        (&["run", "--plugin"], "run --plugin: expected a value"),
        (&["run", "--worldview"], "run --worldview: expected a value"),
        (&["run", "--save", ""], "run --save: expected a value"),
        (&["render", "a.geojson", "o.png", "--basemap"], "render --basemap: expected a value"),
        (&["logo", "l.png", "--size"], "logo --size: expected a value"),
        (&["mcp", "--attach", "5000", "--timeout"], "mcp --timeout: expected a value"),
        (&["plugins", "list", "--registry"], "plugins --registry: expected a value"),
    ];
    for (args, want) in cases {
        let out = cli(&dir, args);
        let err = said(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(err.contains(want), "{args:?}: {err}");
    }
    // The flag that followed `--save` was never taken as the file to save.
    assert!(!dir.0.join("--cmd").exists(), "a project was saved as --cmd");
}

#[test]
fn the_flags_with_a_check_further_on_are_refused_up_front_too() {
    let dir = TempDir::new("flag-values-late");
    // These once passed the up-front check: `mcp-register` refused them with status 1 after
    // plugins loaded, and `--threads` and `--image` only after plugins loaded and a worldview
    // may have been downloaded.
    let cases: [(&[&str], &str); 9] = [
        (&["mcp-register", "--client"], "mcp-register --client: expected a value"),
        (&["mcp-register", "--name"], "mcp-register --name: expected a value"),
        (&["mcp-register", "--port"], "mcp-register --port: expected a value"),
        (&["mcp-register", "--client", "--name", "x"], "mcp-register --client: expected a value, found the option --name"),
        (&["run", "--threads"], "run --threads: expected a value"),
        (&["render", "a.geojson", "o.png", "--threads"], "render --threads: expected a value"),
        (&["ask", "hello", "--threads", "--yes"], "ask --threads: expected a value, found the option --yes"),
        (&["ask", "hello", "--image"], "ask --image: expected a value"),
        (&["mcp", "--attach", "5000", "--threads"], "mcp --threads: expected a value"),
    ];
    for (args, want) in cases {
        let out = cli(&dir, args);
        let err = said(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(err.contains(want), "{args:?}: {err}");
    }
}

#[test]
fn a_dimension_that_is_not_a_positive_number_is_refused() {
    let dir = TempDir::new("dimensions");
    for flag in ["--width", "--height", "--dpi"] {
        for bad in ["abc", "NaN", "inf", "1e400", "0", "-5"] {
            let out = cli(&dir, &["render", "in.geojson", "out.png", flag, bad]);
            let err = said(&out);
            assert_eq!(out.status.code(), Some(2), "{flag} {bad}: {err}");
            assert!(err.contains(&format!("render {flag} {bad}: expected a positive number")), "{flag} {bad}: {err}");
        }
    }
    assert!(!dir.0.join("out.png").exists());
}

#[test]
fn well_formed_flags_render_and_say_nothing() {
    let dir = TempDir::new("render-clean");
    std::fs::write(
        dir.0.join("pt.geojson"),
        r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
    )
    .unwrap();
    let out = cli(&dir, &["render", "pt.geojson", "out.png", "--worldview", "default", "--width", "64", "--height", "64", "--dpi", "96"]);
    let err = said(&out);
    assert!(out.status.success(), "{err}");
    assert!(dir.0.join("out.png").exists());
    assert!(!err.contains("expected a value") && !err.contains("positive number") && !err.contains("a side takes"), "{err}");
}

#[test]
fn a_side_past_its_limit_is_said() {
    let dir = TempDir::new("side-limit");
    std::fs::write(
        dir.0.join("pt.geojson"),
        r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
    )
    .unwrap();
    // 100000 pixels wide is limited to 8000, and the program says so; 8 is raised to 16.
    let out =
        cli(&dir, &["render", "pt.geojson", "out.png", "--worldview", "default", "--width", "100000", "--height", "8", "--dpi", "48"]);
    let err = said(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("rustgis-cli: --width 100000: using 8000 points, the most a side takes (16 to 8000)"), "{err}");
    assert!(err.contains("rustgis-cli: --height 8: using 16 points, the least a side takes (16 to 8000)"), "{err}");
}
