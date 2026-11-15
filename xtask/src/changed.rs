//! `cargo xtask changed [--list] [base]`: the checks of `cargo xtask ci`, on only the packages a
//! branch changes and those that depend on them.
//!
//! - The base is the merge-base of `HEAD` with `base`, or with whichever `<remote>/main` (else
//!   `main`) leaves the fewest commits ahead, so it works on a fork and on a stacked branch.
//! - The changes are the diff from the base to the working tree, plus untracked files.
//! - A file inside a package changes that package. A file outside every package changes the
//!   packages whose sources name it for the build to read (`include_str!`, `include_bytes!`,
//!   `include!`, `#[path]`, `rerun-if-changed`); any other file outside them (`Cargo.lock`, a
//!   workspace setting) changes everything. A file a test opens only at run time, or one a macro
//!   names only from its argument, isn't seen: a change to it checks what the rules above find,
//!   which may leave its reader out.
//! - Every package that depends on a changed one, as a dependency of any kind, is checked too. So
//!   a changed package gets the features every workspace package turns on in it; a feature that a
//!   package left out turns on in an outside crate isn't, which can make a result differ from CI's.
//!
//! `--list` prints the packages it would check, one a line, or `all`, and checks nothing. CI still
//! runs the whole workspace: a pass here says nothing about the packages left out.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// A workspace package: its name, its directory (`/`-separated, from the workspace root) and the
/// names of the packages it depends on.
pub(crate) struct Package {
    pub name: String,
    pub dir: String,
    pub deps: Vec<String>,
}

/// What a set of changed files needs checked.
#[derive(Debug, PartialEq)]
pub(crate) enum Selection {
    /// No file changed.
    Nothing,
    /// The whole workspace, because of this file.
    All(String),
    /// The packages changed, and those plus every package that depends on them.
    Packages { changed: BTreeSet<String>, checked: BTreeSet<String> },
}

/// The package whose directory holds `file`: the longest that does.
fn owner<'a>(file: &str, packages: &'a [Package]) -> Option<&'a Package> {
    packages.iter().filter(|p| file.starts_with(&format!("{}/", p.dir))).max_by_key(|p| p.dir.len())
}

/// What `files` (`/`-separated, from the workspace root) need checked. `reads` lists, for a file
/// or directory, the packages that include it at compile time.
pub(crate) fn select(files: &[String], packages: &[Package], reads: &BTreeMap<String, BTreeSet<String>>) -> Selection {
    if files.is_empty() {
        return Selection::Nothing;
    }
    let mut changed = BTreeSet::new();
    for file in files {
        let readers: Vec<&String> =
            reads.iter().filter(|(r, _)| file == *r || file.starts_with(&format!("{r}/"))).flat_map(|(_, p)| p).collect();
        match owner(file, packages) {
            Some(p) => {
                changed.insert(p.name.clone());
            }
            None if readers.is_empty() => return Selection::All(file.clone()),
            None => {}
        }
        changed.extend(readers.into_iter().cloned());
    }
    let mut checked = changed.clone();
    loop {
        let before = checked.len();
        for p in packages {
            if p.deps.iter().any(|d| checked.contains(d)) {
                checked.insert(p.name.clone());
            }
        }
        if checked.len() == before {
            return Selection::Packages { changed, checked };
        }
    }
}

/// `path`, with `.` and `..` taken out without looking at the disk; `None` if it leaves the root.
fn normalize(path: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

/// The paths `text`, a source file in `dir` of the package in `package_dir`, names for the build to
/// read: what `include_str!`, `include_bytes!` and `include!` take from a string literal, or from
/// a `concat!` that starts with one (its directory, when more follows) or with
/// `env!("CARGO_MANIFEST_DIR")` and one; a `#[path]`; a build script's `rerun-if-changed`. Other
/// forms are left out: `OUT_DIR` holds what the build makes, and a path that is all a macro's
/// argument isn't seen (a macro that names a file outside its package that way is missed).
fn named_paths(text: &str, dir: &Path, package_dir: &Path) -> Vec<PathBuf> {
    /// The string literal at the start of `s`, plain or raw (`r"…"`, `r#"…"#`), and what follows it.
    fn literal(s: &str) -> Option<(&str, &str)> {
        let s = s.trim_start();
        let (s, close) = match s.strip_prefix('r') {
            Some(raw) => {
                let hashes = raw.len() - raw.trim_start_matches('#').len();
                (raw.get(hashes..)?.strip_prefix('"')?, format!("\"{}", "#".repeat(hashes)))
            }
            None => (s.strip_prefix('"')?, "\"".to_string()),
        };
        let end = s.find(&close)?;
        Some((&s[..end], s[end + close.len()..].trim_start()))
    }
    let mut out = Vec::new();
    for marker in ["include_str!(", "include_bytes!(", "include!("] {
        for (at, _) in text.match_indices(marker) {
            let arg = &text[at + marker.len()..];
            if let Some((path, _)) = literal(arg) {
                out.push(dir.join(path));
                continue;
            }
            let Some(parts) = arg.trim_start().strip_prefix("concat!(") else { continue };
            let (base, parts) = match parts.trim_start().strip_prefix("env!(\"CARGO_MANIFEST_DIR\")") {
                Some(rest) => (package_dir, rest.trim_start().strip_prefix(',').unwrap_or(rest)),
                None => (dir, parts),
            };
            let Some((path, rest)) = literal(parts) else { continue };
            let named = base.join(path.trim_start_matches('/'));
            // Only the start of the name is known: the directory it is in.
            let partial = rest.starts_with(',') && !path.ends_with('/');
            out.extend(if partial { named.parent().map(Path::to_path_buf) } else { Some(named) });
        }
    }
    for (at, _) in text.match_indices("#[path") {
        let rest = text[at + "#[path".len()..].trim_start();
        if let Some((path, _)) = rest.strip_prefix('=').and_then(literal) {
            out.push(dir.join(path));
        }
    }
    for (at, _) in text.match_indices("rerun-if-changed=") {
        let rest = &text[at + "rerun-if-changed=".len()..];
        let path = rest.split(['"', '\n']).next().unwrap_or_default();
        match path.split_once('{') {
            Some((start, _)) => out.extend(package_dir.join(start).parent().map(Path::to_path_buf)),
            None => out.push(package_dir.join(path)),
        }
    }
    out
}

/// The files and directories the packages' sources name for the build to read ([`named_paths`]),
/// each with the packages that do, as paths from `root`. A source that can't be read is an error:
/// skipping it could leave out a package that a change needs checked.
pub(crate) fn compile_time_reads(root: &Path, packages: &[Package]) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    /// The `.rs` files under `dir`, leaving out build output and hidden directories.
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            let name = entry.file_name();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if name != "target" && !name.to_string_lossy().starts_with('.') {
                    walk(&path, out)?;
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut reads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in packages {
        let package_dir = root.join(&p.dir);
        let mut sources = Vec::new();
        walk(&package_dir, &mut sources)?;
        for source in sources {
            let text = std::fs::read_to_string(&source).map_err(|e| format!("{}: {e}", source.display()))?;
            let dir = source.parent().unwrap_or(&package_dir);
            for named in named_paths(&text, dir, &package_dir) {
                if let Some(path) = named.strip_prefix(root).ok().and_then(normalize) {
                    reads.entry(path).or_default().insert(p.name.clone());
                }
            }
        }
    }
    Ok(reads)
}

/// The output of `git args` in `root`.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").args(args).current_dir(root).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The base to compare with, its merge-base with `HEAD`, and how many commits `HEAD` is ahead of it.
fn base(root: &Path, asked: Option<&str>) -> Result<(String, String, usize), String> {
    let candidates: Vec<String> = match asked {
        Some(b) => vec![b.to_string()],
        None => {
            let remotes = git(root, &["for-each-ref", "--format=%(refname:short)", "refs/remotes/*/main"])?;
            let mut c: Vec<String> = remotes.lines().map(str::to_string).collect();
            if c.is_empty() {
                c.push("main".into());
            }
            c
        }
    };
    let mut best: Option<(String, String, usize)> = None;
    for c in candidates {
        // A remote's `main` with no history in common is passed over; a base asked for is not.
        let mb = match git(root, &["merge-base", "HEAD", &c]) {
            Ok(mb) => mb.trim().to_string(),
            Err(e) if asked.is_some() => return Err(e),
            Err(_) => continue,
        };
        let count = git(root, &["rev-list", "--count", &format!("{mb}..HEAD")])?;
        let ahead: usize = count.trim().parse().map_err(|e| format!("git rev-list --count: {e}"))?;
        if best.as_ref().is_none_or(|(_, _, a)| ahead < *a) {
            best = Some((c, mb, ahead));
        }
    }
    best.ok_or_else(|| "no base: name one, as in `cargo xtask changed upstream/main`".into())
}

/// The workspace's packages, from `cargo metadata`.
fn packages(root: &Path) -> Result<Vec<Package>, String> {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let meta: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let workspace = PathBuf::from(meta["workspace_root"].as_str().unwrap_or_default());
    let mut list = Vec::new();
    for p in meta["packages"].as_array().into_iter().flatten() {
        let manifest = PathBuf::from(p["manifest_path"].as_str().unwrap_or_default());
        let dir = manifest.parent().and_then(|d| d.strip_prefix(&workspace).ok()).and_then(normalize);
        // One left out would leave its dependents unchecked, so it stops the run.
        let (Some(name), Some(dir)) = (p["name"].as_str(), dir) else {
            return Err(format!("cargo metadata: a package without a name or a directory in the workspace: {}", manifest.display()));
        };
        let deps = p["dependencies"].as_array().into_iter().flatten().filter_map(|d| d["name"].as_str().map(str::to_string)).collect();
        list.push(Package { name: name.to_string(), dir, deps });
    }
    Ok(list)
}

/// Runs `cargo xtask changed` with the arguments after `changed`.
pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    const USAGE: &str = "usage: cargo xtask changed [--list] [base]";
    let list = args.iter().any(|a| a == "--list");
    let others: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--list").collect();
    let asked = match others[..] {
        [] => None,
        [base] if !base.starts_with('-') => Some(base),
        _ => return Err(USAGE.into()),
    };
    let (name, mb, ahead) = base(root, asked)?;
    let mut files: Vec<String> = git(root, &["diff", "--name-only", "--no-renames", "-z", &mb])?.split('\0').map(str::to_string).collect();
    files.extend(git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?.split('\0').map(str::to_string));
    files.retain(|f| !f.is_empty());
    files.sort();
    files.dedup();
    let packages = packages(root)?;
    let selection = select(&files, &packages, &compile_time_reads(root, &packages)?);
    let short = mb.get(..8).unwrap_or(&mb);
    if !list {
        println!("changed: {} files since {name} ({short}, {ahead} commits ahead)", files.len());
    }
    match selection {
        Selection::Nothing => {
            if !list {
                println!("changed: nothing to check");
            }
            Ok(())
        }
        Selection::All(_) if list => {
            println!("all");
            Ok(())
        }
        Selection::All(file) => {
            println!("changed: {file} is outside every package and nothing includes it, so the whole workspace is checked");
            super::ci()?;
            println!("changed: passed. CI also builds the web app for wasm32 and checks licences and the other platforms.");
            Ok(())
        }
        Selection::Packages { checked, .. } if list => {
            checked.iter().for_each(|p| println!("{p}"));
            Ok(())
        }
        Selection::Packages { changed, checked } => {
            println!(
                "changed: checking {} of {} packages: {} (changed: {})",
                checked.len(),
                packages.len(),
                checked.iter().cloned().collect::<Vec<_>>().join(", "),
                changed.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            let selected: Vec<&str> = checked.iter().flat_map(|p| ["-p", p.as_str()]).collect();
            let mut clippy = vec!["clippy"];
            clippy.extend(&selected);
            clippy.extend(["--all-targets", "--", "-D", "warnings"]);
            let mut test = vec!["test"];
            test.extend(&selected);
            test.push("-q");
            super::cargo(&["fmt", "--all", "--check"])
                .and_then(|_| super::cargo(&clippy))
                .and_then(|_| super::cargo(&test))
                .and_then(|_| super::layers())
                // The project schema is generated from types in rustgis-engine and below, so any
                // change there (which checks rustgis-engine too) checks it.
                .and_then(|_| if checked.contains("rustgis-engine") { super::schema(true) } else { Ok(()) })?;
            let left_out = packages.len() - checked.len();
            print!("changed: passed. Not checked here, as CI does: the {left_out} packages left out, licences and the other platforms");
            if checked.contains("rustgis-web") {
                print!(", and the web app's wasm build (`cargo build --target wasm32-unknown-unknown -p rustgis-web`)");
            }
            println!(".");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small workspace shaped like this one: a chain of crates, two apps on top, and `xtask`
    /// beside them depending on none of them.
    fn workspace() -> Vec<Package> {
        let p = |name: &str, dir: &str, deps: &[&str]| Package {
            name: name.into(),
            dir: dir.into(),
            deps: deps.iter().map(|d| d.to_string()).collect(),
        };
        vec![
            p("rustgis-geom", "crates/geom", &["serde"]),
            p("rustgis-data", "crates/data", &["rustgis-geom"]),
            p("rustgis-ui-egui", "crates/ui-egui", &["rustgis-data", "egui"]),
            p("rustgis", "apps/rustgis", &["rustgis-ui-egui"]),
            p("rustgis-web", "apps/rustgis-web", &["rustgis-ui-egui"]),
            p("xtask", "xtask", &["serde_json"]),
        ]
    }

    /// Package names as a set.
    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// Changed file paths.
    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_change_checks_its_package_and_those_that_depend_on_it() {
        let ws = workspace();
        let reads = BTreeMap::new();
        assert_eq!(select(&[], &ws, &reads), Selection::Nothing);
        assert_eq!(
            select(&files(&["crates/ui-egui/src/lib.rs"]), &ws, &reads),
            Selection::Packages { changed: names(&["rustgis-ui-egui"]), checked: names(&["rustgis", "rustgis-ui-egui", "rustgis-web"]) }
        );
        // A change low down reaches everything above it, but not what doesn't depend on it.
        let Selection::Packages { checked, .. } = select(&files(&["crates/geom/src/lib.rs"]), &ws, &reads) else { panic!() };
        assert_eq!(checked, names(&["rustgis", "rustgis-data", "rustgis-geom", "rustgis-ui-egui", "rustgis-web"]));
    }

    #[test]
    fn a_file_outside_the_packages_checks_its_readers_or_everything() {
        let ws = workspace();
        let mut reads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        reads.insert("docs/user-guide/expressions.md".into(), names(&["rustgis-data"]));
        reads.insert("tests/support".into(), names(&["rustgis-geom"]));
        reads.insert("apps/rustgis-web/sw.js".into(), names(&["xtask"]));
        let Selection::Packages { changed, .. } = select(&files(&["docs/user-guide/expressions.md", "tests/support/ogc.rs"]), &ws, &reads)
        else {
            panic!()
        };
        assert_eq!(changed, names(&["rustgis-data", "rustgis-geom"]));
        // A package that includes a file in another package without depending on it.
        let Selection::Packages { changed, .. } = select(&files(&["apps/rustgis-web/sw.js"]), &ws, &reads) else { panic!() };
        assert_eq!(changed, names(&["rustgis-web", "xtask"]));
        for file in ["Cargo.lock", "docs/index.md", "clippy.toml"] {
            assert_eq!(select(&files(&["crates/geom/src/lib.rs", file]), &ws, &reads), Selection::All(file.into()));
        }
    }

    #[test]
    fn reads_the_ways_a_source_names_a_file() {
        let text = r##"
            include_str!( "../a.txt" );
            include_str!(r#"../raw.txt"#);
            #[path="../../../tests/support/b.rs"] mod b;
            include_bytes!(concat!("../pages/page_", $n, ".bin"));
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/gen/c.rs"));
            include_str!(concat!(env!("OUT_DIR"), "/made.json"));
            println!("cargo:rerun-if-changed=../shared/d.csv");
        "##;
        let found: Vec<String> =
            named_paths(text, Path::new("crates/x/src"), Path::new("crates/x")).iter().filter_map(|p| normalize(p)).collect();
        assert_eq!(
            found,
            ["crates/x/a.txt", "crates/x/raw.txt", "crates/x/pages", "crates/x/gen/c.rs", "tests/support/b.rs", "crates/shared/d.csv"]
        );
    }

    // The reads found in this repository's own sources, which the selection depends on.
    #[test]
    fn finds_this_workspaces_compile_time_reads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut ws = workspace();
        ws.push(Package { name: "rustgis-assistant".into(), dir: "crates/assistant".into(), deps: vec![] });
        let reads = compile_time_reads(root, &ws).unwrap();
        assert_eq!(reads.get("apps/rustgis-web/sw.js"), Some(&names(&["xtask"])));
        // `include_str!(concat!("../../../docs/", $path))`: the directory, as the file isn't known.
        assert!(reads.get("docs").is_some_and(|r| r.contains("rustgis-assistant")), "{reads:?}");
        // `include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/…"))`.
        assert!(reads.keys().any(|r| r.starts_with("crates/data/tests/data/")), "{reads:?}");
        assert_eq!(reads.get("docs/user-guide/expressions.md"), Some(&names(&["rustgis-data"])));
        assert!(reads.keys().any(|r| r.starts_with("tests/support/")), "{reads:?}");
        assert_eq!(normalize(Path::new("crates/data/src/../../../docs")), Some("docs".into()));
        assert_eq!(normalize(Path::new("../outside")), None);
    }
}
