//! `cargo xtask ci | changed [--list] [base] | layers | attribution | memory [--cli <path>] [--update] | rows [--update] [--list] | commands | test-hygiene | notices [--check] | schema [--check] | web-pwa <dir>`
//!
//! - `layers`: enforces the crate layering (nothing below L6 may depend on egui/eframe/rfd, a
//!   crate may only depend on lower layers, and every crate under `crates/` has a layer).
//! - `attribution`: every image and font file has a row in `ATTRIBUTION.md` (see `attribution.rs`).
//! - `memory [--cli <path>] [--update]`: the command line's peak memory on generated data, per
//!   workflow and as a multiple of loading alone, checked against `xtask/memory-limits.txt`
//!   (Linux; see `memory.rs`).
//! - `rows [--update] [--list]`: direct uses of a layer's rows outside `rustgis-data`, which
//!   may not rise above the count in `xtask/row-uses.txt` (see `rows.rs`).
//! - `commands`: writes the snapshot of command ids and parameters
//!   (`crates/engine/command-surface.txt`) from the registry; an engine test fails when the
//!   committed file and the registry disagree.
//! - `test-hygiene`: timer waits in tests need same-line reasons; ignored tests need reasons.
//! - `ci`: test hygiene, fmt, clippy -D warnings, tests, layering, direct row uses and the
//!   project schema check.
//! - `changed [--list] [base]`: the same, on only the packages a branch changes and those that
//!   depend on them (see `changed.rs`).
//! - `notices [--check]`: writes the third-party licence notices the packages ship (see
//!   `notices.rs`), or checks they are up to date (exit code 2 when they are not).
//! - `schema [--check]`: writes the project file's JSON Schema (`schema/project.schema.json`)
//!   from the types it is saved from, or checks it is up to date and the fixture projects open.
//! - `web-pwa <dir>`: writes the web app's service worker into a `trunk build` output directory
//!   (`dist/web`) from the template `apps/rustgis-web/sw.js`, listing the built files to cache.

mod attribution;
mod changed;
mod memory;
mod notices;
mod rows;
mod test_hygiene;

use serde_json::Value;
use std::process::{Command, ExitCode};

const LAYERS: &[(&str, u8)] = &[
    ("geom", 0),
    ("data", 1),
    ("map", 2),
    ("analysis", 2),
    ("tiles", 2),
    ("keychain", 2),
    ("render", 3),
    ("geolibre", 3),
    ("plugins", 3),
    ("engine", 4),
    ("assistant", 5),
    ("ui-egui", 6),
    ("ui-touch", 7),
];
const UI_ONLY: &[&str] = &["egui", "eframe", "egui_extras", "egui-wgpu", "egui_kittest", "rfd", "wgpu", "winit"];

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

/// `cargo xtask layers`: the layering of the crates under `crates/`, read from `cargo metadata`.
fn layers() -> Result<(), String> {
    let errs = layer_errors(&crate_deps(&changed::metadata(&root())?));
    if errs.is_empty() {
        println!("layers: ok");
        Ok(())
    } else {
        Err(errs.join("\n"))
    }
}

/// Each package under `crates/` as (its directory name, the packages it depends on). A dependency
/// counts whatever its target (`[target.'cfg(…)'.dependencies]`) and whether or not it is optional.
/// Dev- and build-dependencies don't: neither is in what the crate ships, and AGENTS.md's rule
/// ("nothing below L6 depends on egui/eframe/rfd") doesn't clearly cover them.
fn crate_deps(meta: &Value) -> Vec<(String, Vec<String>)> {
    let workspace = std::path::Path::new(meta["workspace_root"].as_str().unwrap_or_default());
    let mut out = Vec::new();
    for package in meta["packages"].as_array().into_iter().flatten() {
        let manifest = std::path::Path::new(package["manifest_path"].as_str().unwrap_or_default());
        let dir = manifest.parent().and_then(|d| d.strip_prefix(workspace).ok()).and_then(|d| d.strip_prefix("crates").ok());
        // A package nested below a crate's directory is kept, with its path: it has no layer.
        let Some(dir) = dir.and_then(|d| d.to_str()).filter(|d| !d.is_empty()) else { continue };
        let deps = package["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["kind"].is_null())
            .filter_map(|d| d["name"].as_str().map(str::to_string))
            .collect();
        out.push((dir.replace('\\', "/"), deps));
    }
    out
}

/// What is wrong with the layering of `crates`: a crate with no layer in [`LAYERS`] (a new crate
/// must be given one), a layer for a crate that isn't there, a UI crate below L6, and a crate that
/// depends on one at or above its own layer.
fn layer_errors(crates: &[(String, Vec<String>)]) -> Vec<String> {
    let mut errs = Vec::new();
    for (dir, _) in crates {
        if !LAYERS.iter().any(|(k, _)| k == dir) {
            errs.push(format!("crates/{dir} has no layer: add it to LAYERS in xtask/src/main.rs"));
        }
    }
    for (krate, level) in LAYERS {
        let Some((_, deps)) = crates.iter().find(|(dir, _)| dir == krate) else {
            errs.push(format!("LAYERS has {krate}, but there is no crates/{krate}: take it out of xtask/src/main.rs"));
            continue;
        };
        for name in deps {
            if *level < 6 && UI_ONLY.contains(&name.as_str()) {
                errs.push(format!("{krate} (L{level}) depends on UI crate {name}"));
            }
            if let Some(dep) = name.strip_prefix("rustgis-")
                && let Some((_, dl)) = LAYERS.iter().find(|(k, _)| *k == dep)
                && dl >= level
            {
                errs.push(format!("{krate} (L{level}) depends on {dep} (L{dl})"));
            }
        }
    }
    errs
}

fn cargo(args: &[&str]) -> Result<(), String> {
    println!("$ cargo {}", args.join(" "));
    let st = Command::new(env!("CARGO")).args(args).current_dir(root()).status().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err(format!("cargo {} failed", args.join(" "))) }
}

/// `cargo xtask ci`: test hygiene, fmt, clippy with warnings as errors, tests, layering, attribution
/// rows, direct row uses and the project schema check, on the whole workspace.
fn ci() -> Result<(), String> {
    test_hygiene::run(&root())?;
    cargo(&["fmt", "--all", "--check"])
        .and_then(|_| cargo(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]))
        .and_then(|_| cargo(&["test", "--workspace", "-q"]))
        .and_then(|_| layers())
        .and_then(|_| attribution::run(&root()))
        .and_then(|_| rows::run(&root(), &[]))
        .and_then(|_| schema(true))
}

/// Files in a web build the service worker must not cache: itself, the GitHub Pages
/// configuration the deploy adds, and the licence notices (served, but not needed offline).
const NOT_SHELL: &[&str] = &["sw.js", "CNAME", ".nojekyll", "LICENSE", "THIRD-PARTY-LICENSES.txt"];

/// The files under `dir` (a `trunk build` output) for the service worker to cache, as sorted
/// `/`-separated paths relative to `dir`, plus a version that changes whenever any of them or the
/// service worker `template` does.
fn web_shell(dir: &std::path::Path, template: &str) -> Result<(Vec<String>, String), String> {
    fn walk(dir: &std::path::Path, base: &str, out: &mut Vec<String>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if base.is_empty() { name.clone() } else { format!("{base}/{name}") };
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                // Trunk's staging directory, left behind by an interrupted build.
                if name != ".stage" {
                    walk(&entry.path(), &rel, out)?;
                }
            } else if kind.is_file() && !(base.is_empty() && NOT_SHELL.contains(&name.as_str())) {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, "", &mut files)?;
    files.sort();
    if !files.iter().any(|f| f == "index.html") {
        return Err(format!("{}: no index.html; run `trunk build --release` first", dir.display()));
    }
    // FNV-1a over every path and its contents: the same on every platform and Rust version.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in template.bytes().chain([0]) {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    for f in &files {
        let bytes = std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))?;
        for b in f.bytes().chain([0]).chain(bytes) {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    Ok((files, format!("{h:016x}")))
}

/// A JavaScript string literal holding `s`.
fn js_string(s: &str) -> String {
    let mut q = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                q.push('\\');
                q.push(c);
            }
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => q.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

/// The service worker for a web build: the template with its file list and version filled in.
fn web_sw(template: &str, files: &[String], version: &str) -> Result<String, String> {
    if !template.contains("\"__VERSION__\"") || !template.contains("__SHELL__") {
        return Err("apps/rustgis-web/sw.js: the __VERSION__ or __SHELL__ placeholder is missing".into());
    }
    let list = format!("[\n{}\n]", files.iter().map(|f| format!("  {},", js_string(f))).collect::<Vec<_>>().join("\n"));
    Ok(template.replace("\"__VERSION__\"", &js_string(version)).replace("__SHELL__", &list))
}

fn web_pwa(dir: Option<String>) -> Result<(), String> {
    let dir = std::path::PathBuf::from(dir.ok_or("usage: cargo xtask web-pwa <trunk build output, e.g. dist/web>")?);
    let template = std::fs::read_to_string(root().join("apps/rustgis-web/sw.js")).map_err(|e| format!("apps/rustgis-web/sw.js: {e}"))?;
    let (files, version) = web_shell(&dir, &template)?;
    let sw = web_sw(&template, &files, &version)?;
    std::fs::write(dir.join("sw.js"), sw).map_err(|e| format!("{}: {e}", dir.display()))?;
    println!("web-pwa: {} caches {} files (version {version})", dir.join("sw.js").display(), files.len());
    Ok(())
}

/// Writes the project file's JSON Schema from the types it is saved from (`schema/`), or with
/// `check` fails when the committed one is out of date or a fixture project doesn't open, after
/// linting the code behind the `schema` feature, which the workspace's clippy run leaves out.
/// (CI's "Project schema" job also validates the fixtures against the schema with
/// `check-jsonschema`.)
fn schema(check: bool) -> Result<(), String> {
    let mut args = vec!["run", "-q", "-p", "rustgis-engine", "--features", "schema", "--example", "project_schema"];
    if check {
        cargo(&[
            "clippy",
            "--locked",
            "-p",
            "rustgis-geom",
            "-p",
            "rustgis-data",
            "-p",
            "rustgis-engine",
            "--features",
            "rustgis-engine/schema",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])?;
        // A Cargo.lock out of date with the manifests fails here too.
        args.extend(["--locked", "--", "--check"]);
    }
    cargo(&args)
}

/// Writes the command surface snapshot from the registry (`crates/engine/command-surface.txt`).
/// The engine's `command_surface_matches_the_snapshot` test, which `cargo xtask ci` runs, is the
/// check.
fn commands() -> Result<(), String> {
    cargo(&["run", "-q", "--locked", "-p", "rustgis-engine", "--example", "command_surface"])
}

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let r = match task.as_str() {
        "layers" => layers(),
        "attribution" => attribution::run(&root()),
        "test-hygiene" => test_hygiene::run(&root()),
        "rows" => rows::run(&root(), &std::env::args().skip(2).collect::<Vec<_>>()),
        "memory" => memory::run(&root(), &std::env::args().skip(2).collect::<Vec<_>>()),
        "commands" => commands(),
        "ci" => ci(),
        "changed" => changed::run(&root(), &std::env::args().skip(2).collect::<Vec<_>>()),
        "notices" => {
            let check = match std::env::args().nth(2).as_deref() {
                None => false,
                Some("--check") => true,
                Some(other) => {
                    eprintln!("unknown argument {other}: usage: cargo xtask notices [--check]");
                    return ExitCode::FAILURE;
                }
            };
            return match notices::notices(&root(), check) {
                Ok(()) => ExitCode::SUCCESS,
                Err((code, e)) => {
                    eprintln!("{e}");
                    ExitCode::from(code)
                }
            };
        }
        "schema" => match std::env::args().nth(2).as_deref() {
            None => schema(false),
            Some("--check") => schema(true),
            Some(other) => Err(format!("unknown argument {other}: usage: cargo xtask schema [--check]")),
        },
        "web-pwa" => web_pwa(std::env::args().nth(2)),
        _ => Err("usage: cargo xtask ci | changed [--list] [base] | layers | attribution | memory [--cli <path>] [--update] | rows [--update] [--list] | commands | test-hygiene | notices [--check] | schema [--check] | web-pwa <dir>".into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_sw_fills_the_real_template() {
        let template = include_str!("../../apps/rustgis-web/sw.js");
        let files = vec!["index.html".to_string(), "rustgis-web-1a2b.js".to_string(), "odd\"name\\.wasm".to_string()];
        let sw = web_sw(template, &files, "00ff").unwrap();
        assert!(!sw.contains("__VERSION__") && !sw.contains("__SHELL__"));
        assert!(sw.contains("const VERSION = \"00ff\";"));
        assert!(sw.contains("  \"index.html\",\n  \"rustgis-web-1a2b.js\",\n  \"odd\\\"name\\\\.wasm\",\n]"));
        assert!(web_sw("no placeholders", &files, "00ff").is_err());
    }

    // A page opened as a local file cannot start the app: it says so instead of spinning.
    #[test]
    fn web_page_explains_opening_it_as_a_local_file() {
        let page = include_str!("../../apps/rustgis-web/index.html");
        assert!(page.contains("<div id=\"rustgis_local\" role=\"alert\" hidden>"));
        assert!(page.contains("python3 -m http.server 8080"));
        let check = page.find("location.protocol === \"file:\"").unwrap();
        assert!(page.find("id=\"rustgis_local\" role").unwrap() < check, "the script needs the notice in the page");
        assert!(page[check..].contains("document.getElementById(\"rustgis_local\").hidden = false"));
    }

    #[test]
    fn web_shell_lists_the_app_files_and_versions_them() {
        let dir = std::env::temp_dir().join(format!("rustgis-xtask-web-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("snippets/x")).unwrap();
        std::fs::create_dir_all(dir.join(".stage")).unwrap();
        for f in [
            "index.html",
            "app-1.js",
            "app-1_bg.wasm",
            "snippets/x/inline0.js",
            "sw.js",
            "CNAME",
            ".nojekyll",
            "LICENSE",
            "THIRD-PARTY-LICENSES.txt",
            ".stage/tmp",
        ] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        let (files, v1) = web_shell(&dir, "sw").unwrap();
        assert_eq!(files, ["app-1.js", "app-1_bg.wasm", "index.html", "snippets/x/inline0.js"]);
        assert_eq!(web_shell(&dir, "sw").unwrap().1, v1);
        // A new service worker alone is a new version too.
        assert_ne!(web_shell(&dir, "sw 2").unwrap().1, v1);
        std::fs::write(dir.join("index.html"), "changed").unwrap();
        assert_ne!(web_shell(&dir, "sw").unwrap().1, v1);
        std::fs::remove_file(dir.join("index.html")).unwrap();
        assert!(web_shell(&dir, "sw").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dependency as (name, kind, target), as `cargo metadata` gives them.
    type Dep<'a> = (&'a str, Option<&'a str>, Option<&'a str>);

    /// A `cargo metadata` document with one package under `crates/` for each (directory, dependencies),
    /// a dependency being (name, kind, target).
    fn metadata(crates: &[(&str, &[Dep])]) -> Value {
        let packages: Vec<Value> = crates
            .iter()
            .map(|(dir, deps)| {
                let deps: Vec<Value> = deps.iter().map(|(name, kind, target)| serde_json::json!({"name": name, "kind": kind, "target": target})).collect();
                serde_json::json!({"name": format!("rustgis-{dir}"), "manifest_path": format!("/w/crates/{dir}/Cargo.toml"), "dependencies": deps})
            })
            .collect();
        serde_json::json!({"workspace_root": "/w", "packages": packages})
    }

    /// Every crate in [`LAYERS`], with no dependencies, plus these changes.
    fn all_crates(changes: &[(&str, &[Dep])]) -> Value {
        let mut crates: Vec<(&str, &[Dep])> = LAYERS.iter().map(|(k, _)| (*k, &[][..])).collect();
        for (dir, deps) in changes {
            crates.retain(|(k, _)| k != dir);
            crates.push((dir, deps));
        }
        metadata(&crates)
    }

    #[test]
    fn layers_accepts_the_layering_as_it_is() {
        let ok = all_crates(&[("engine", &[("rustgis-data", None, None), ("serde", None, None)]), ("ui-egui", &[("egui", None, None)])]);
        assert_eq!(layer_errors(&crate_deps(&ok)), Vec::<String>::new());
    }

    #[test]
    fn layers_reads_dependencies_that_are_only_for_some_targets() {
        let plain = all_crates(&[("engine", &[("rfd", None, None)])]);
        let targeted = all_crates(&[("engine", &[("rfd", None, Some("cfg(not(target_arch = \"wasm32\"))"))])]);
        for meta in [plain, targeted] {
            assert_eq!(layer_errors(&crate_deps(&meta)), vec!["engine (L4) depends on UI crate rfd".to_string()]);
        }
        let upward = all_crates(&[("data", &[("rustgis-engine", None, Some("cfg(windows)"))])]);
        assert_eq!(layer_errors(&crate_deps(&upward)), vec!["data (L1) depends on engine (L4)".to_string()]);
    }

    #[test]
    fn layers_leaves_dev_and_build_dependencies_out() {
        let meta = all_crates(&[("geom", &[("egui_kittest", Some("dev"), None), ("rustgis-engine", Some("build"), None)])]);
        assert_eq!(layer_errors(&crate_deps(&meta)), Vec::<String>::new());
    }

    #[test]
    fn layers_refuses_a_crate_without_a_layer_and_a_layer_without_a_crate() {
        let mut crates: Vec<(&str, &[Dep])> = LAYERS.iter().filter(|(k, _)| *k != "keychain").map(|(k, _)| (*k, &[][..])).collect();
        crates.push(("newcomer", &[]));
        let errs = layer_errors(&crate_deps(&metadata(&crates)));
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert!(errs.iter().any(|e| e.starts_with("crates/newcomer has no layer")));
        assert!(errs.iter().any(|e| e.starts_with("LAYERS has keychain, but there is no crates/keychain")));
    }

    #[test]
    fn layers_sees_a_package_nested_below_a_crate() {
        let mut crates: Vec<(&str, &[Dep])> = LAYERS.iter().map(|(k, _)| (*k, &[][..])).collect();
        crates.push(("data/inner", &[]));
        assert_eq!(
            layer_errors(&crate_deps(&metadata(&crates))),
            vec!["crates/data/inner has no layer: add it to LAYERS in xtask/src/main.rs".to_string()]
        );
    }

    #[test]
    fn layers_ignores_packages_outside_crates() {
        let mut meta = all_crates(&[]);
        let app = serde_json::json!({"name": "rustgis", "manifest_path": "/w/apps/rustgis/Cargo.toml", "dependencies": [{"name": "egui", "kind": null, "target": null}]});
        meta["packages"].as_array_mut().unwrap().push(app);
        assert_eq!(layer_errors(&crate_deps(&meta)), Vec::<String>::new());
    }
}
