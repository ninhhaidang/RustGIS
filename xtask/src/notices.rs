//! `cargo xtask notices [--check]`: the third-party licence notices shipped with RustGIS.
//!
//! The crates RustGIS is built from are MIT, BSD, ISC, Apache and so on, and those licences ask
//! for their copyright and licence text to go with every copy, binaries included. This writes
//! them, from `cargo about` (see [`INSTALL`], configured by `about.toml`),
//! into a plain-text file next to each LICENSE the packages ship:
//!
//! - `THIRD-PARTY-LICENSES.txt`: every crate in the workspace (the desktop app, the command line
//!   and the web app);
//! - `apps/rustgis-python/THIRD-PARTY-LICENSES.txt`: the crates in the Python package.
//!
//! Each distinct licence text appears once, with the crates under it (by name, not version, so a
//! routine version bump leaves the files alone), followed by the crates' NOTICE files (Apache-2.0
//! section 4(d)), then the code and data RustGIS's own crates take from other projects
//! ([`INCLUDED`]), with their licence texts from `third-party/`. `--check` fails when a file is out
//! of date instead of writing it.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// How to install the cargo-about the files were generated with (CI pins the same version).
pub const INSTALL: &str = "cargo install --locked --features cli cargo-about@0.9.2";

/// The `--check` exit code for files that are out of date (other failures exit with 1).
pub const STALE: u8 = 2;

/// (output file, package manifest or "" for the whole workspace, what it covers).
const FILES: &[(&str, &str, &str)] = &[
    ("THIRD-PARTY-LICENSES.txt", "", "RustGIS (the desktop app, the command line and the web app)"),
    ("apps/rustgis-python/THIRD-PARTY-LICENSES.txt", "apps/rustgis-python/Cargo.toml", "the rustgis Python package"),
];

/// Code or data that one of RustGIS's own crates takes from another project. Cargo doesn't see
/// these, so they are listed here, and their notices go in the files of every package that
/// compiles the crate in.
struct Included {
    /// What it is, where it comes from and what was done to it.
    what: &'static str,
    /// The RustGIS crate it is compiled into.
    krate: &'static str,
    /// The files it is in, from the repository root.
    paths: &'static [&'static str],
    /// The licence texts and notices its licences ask to go with every copy, in `third-party/`,
    /// as the upstream projects publish them (the notices file tidies their line ends and
    /// trailing spaces, as it does the crates' licences). Empty for data whose terms ask only
    /// for the acknowledgement in `what`.
    notices: &'static [&'static str],
}

/// Everything RustGIS's own crates take from other projects. Code or data adapted from another
/// project gets an entry here, with its licence text in `third-party/`; a test checks the files
/// are there.
const INCLUDED: &[Included] = &[
    Included {
        what: "Colour maps. Scientific colour maps sampled at evenly spaced stops (scientific.rs): Viridis, \
               Plasma, Inferno and Magma by Stéfan van der Walt and Nathaniel Smith (CC0-1.0), and Cividis by \
               Jamie R. Nuñez, Christopher R. Anderton and Ryan S. Renslow, as matplotlib ships them; Turbo by \
               Anton Mikhailov, Copyright 2019 Google LLC (Apache-2.0, whose text is in LICENSE); Batlow, Vik, \
               Roma and Oleron from Fabio Crameri's Scientific colour maps, through cmcrameri (MIT); Balance, \
               Haline, Deep and Topo from cmocean (MIT). The matplotlib colour map library (matplotlib.rs), from \
               rio-tiler's 256-colour tables (https://github.com/cogeotiff/rio-tiler), each reduced to the \
               fewest stops that reproduce it: matplotlib's maps (among them the ColorBrewer maps, Apache-2.0, \
               and the gist maps from Yorick), cmocean's, and rio-tiler's cfastie, rplumbo and schwarzwald.",
        krate: "rustgis-map",
        paths: &["crates/map/src/color/scientific.rs", "crates/map/src/color/matplotlib.rs"],
        notices: &[
            "matplotlib-LICENSE.txt",
            "matplotlib-LICENSE_COLORBREWER.txt",
            "matplotlib-LICENSE_YORICK.txt",
            "rio-tiler-LICENSE.txt",
            "cmcrameri-LICENSE.txt",
            "cmocean-LICENSE.txt",
        ],
    },
    Included {
        what: "The meridian distance of the sinusoidal projection and its inverse, after pj_enfn, pj_mlfn and \
               pj_inv_mlfn in PROJ (https://proj.org).",
        krate: "rustgis-geom",
        paths: &["crates/geom/src/sinusoidal.rs"],
        notices: &["PROJ-COPYING.txt"],
    },
    Included {
        what: "Where a label goes in an area (pole_of_inaccessibility), after polylabel 1.x \
               (https://github.com/mapbox/polylabel).",
        krate: "rustgis-tiles",
        paths: &["crates/tiles/src/symbols.rs"],
        notices: &["polylabel-LICENSE.txt"],
    },
    Included {
        what: "Telling a style's expression filters from its legacy ones (is_expression_filter), after \
               isExpressionFilter in the MapLibre style specification \
               (https://github.com/maplibre/maplibre-style-spec).",
        krate: "rustgis-tiles",
        paths: &["crates/tiles/src/style.rs"],
        notices: &["maplibre-style-spec-LICENSE.txt"],
    },
    Included {
        what: "ESRI coordinate system codes: Esri's projected coordinate systems from projection-engine-db-doc \
               (https://github.com/Esri/projection-engine-db-doc), as imported into the PROJ 9.7.1 database and \
               written as PROJ definitions by PROJ 9.7.1 (PROJ's licence is above). The notice below is the \
               licensing section of the project's README, at commit aa34139.",
        krate: "rustgis-data",
        paths: &["crates/data/src/raster/esri.rs"],
        notices: &["projection-engine-db-doc-README-licensing.txt"],
    },
    Included {
        what: "ArcGIS feature-service answers as protocol buffers (esripbf.rs), after Terrio's ESRIPBF reader \
               (https://github.com/opengeos/terrio), adapted to RustGIS's features. Both follow Esri's published \
               FeatureCollection.proto (https://github.com/Esri/arcgis-pbf, Apache-2.0, whose text is in \
               LICENSE).",
        krate: "rustgis-data",
        paths: &["crates/data/src/esripbf.rs"],
        notices: &["terrio-LICENSE.txt"],
    },
    Included {
        what: "Coordinate systems by EPSG code: the definitions of the crs-definitions crate and of epsg_gap.rs, \
               and the axis orders of epsg_axes.rs, are derived from the EPSG Geodetic Parameter Dataset, which \
               is owned by IOGP (the International Association of Oil & Gas Producers) and published under its \
               terms of use, https://epsg.org/terms-of-use.html. The definitions were converted to PROJ strings \
               by PROJ (crs-definitions' through PostGIS) and the axis orders read through PROJ: they are not \
               the EPSG Dataset itself, and some definitions only approximate it. The EPSG Dataset is provided \
               \"as is\", without warranty of any kind.",
        krate: "rustgis-data",
        paths: &["crates/data/src/raster/epsg_gap.rs", "crates/data/src/raster/epsg_axes.rs"],
        notices: &[],
    },
];

/// Text of `v[key]`, or "".
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A licence text with Windows line ends and trailing spaces gone, and no blank lines at either end.
fn tidy(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim_matches('\n').to_string()
}

/// `names` as comma-separated lines no wider than 100 columns.
fn wrap(names: &[String]) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for (i, n) in names.iter().enumerate() {
        let item = if i + 1 < names.len() { format!("{n},") } else { n.clone() };
        if !line.is_empty() && line.len() + 1 + item.len() > 100 {
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&item);
    }
    out.push_str(&line);
    out
}

/// `text` broken into lines no wider than 100 columns, at spaces.
fn fill(text: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for word in text.split_whitespace() {
        if width > 0 && width + 1 + word.chars().count() > 100 {
            out.push('\n');
            width = 0;
        } else if width > 0 {
            out.push(' ');
            width += 1;
        }
        out.push_str(word);
        width += word.chars().count();
    }
    out
}

/// The [`INCLUDED`] entries, as (RustGIS crate, heading, licence texts) for [`render`], with the
/// texts read from `third-party/` under `root`. An error names a listed file that isn't there.
fn included(root: &Path) -> Result<Vec<(String, String, Vec<String>)>, String> {
    let mut out = Vec::new();
    for inc in INCLUDED {
        for path in inc.paths {
            if !root.join(path).is_file() {
                return Err(format!("notices: {path} is listed in INCLUDED (xtask/src/notices.rs) but isn't there"));
            }
        }
        let mut texts = Vec::new();
        for file in inc.notices {
            let body = std::fs::read_to_string(root.join("third-party").join(file)).unwrap_or_default();
            if body.trim().is_empty() {
                return Err(format!("notices: third-party/{file}, listed in INCLUDED (xtask/src/notices.rs), is missing or empty"));
            }
            texts.push(body);
        }
        out.push((inc.krate.to_string(), format!("{}\nIn {}: {}", fill(inc.what), inc.krate, inc.paths.join(", ")), texts));
    }
    Ok(out)
}

/// The notices file for `cargo about`'s JSON report `about`; `what` names what it covers.
/// `notice` reads a crate's NOTICE files from its manifest path, as (file name, contents).
/// `included` is code and data from other projects in RustGIS's crates, as (RustGIS crate,
/// heading, licence texts): those of the crates in the report go at the end.
pub fn render(
    about: &Value,
    what: &str,
    notice: &dyn Fn(&str) -> Vec<(String, String)>,
    included: &[(String, String, Vec<String>)],
) -> String {
    let empty = Vec::new();
    // Licence texts, keyed by (licence id, text with its whitespace collapsed): one entry for the
    // same text that differs only in wrapping. Value: (name, text, crate names).
    let mut texts: BTreeMap<(String, String), (String, String, Vec<String>)> = BTreeMap::new();
    for lic in about.get("licenses").and_then(Value::as_array).unwrap_or(&empty) {
        let body = tidy(text(lic, "text"));
        let key = (text(lic, "id").to_string(), body.split_whitespace().collect::<Vec<_>>().join(" "));
        let entry = texts.entry(key).or_insert_with(|| (text(lic, "name").to_string(), body, Vec::new()));
        for used in lic.get("used_by").and_then(Value::as_array).unwrap_or(&empty) {
            let krate = used.get("crate").unwrap_or(&Value::Null);
            // Path crates (`source` null) are RustGIS's own.
            if !krate.get("source").is_some_and(Value::is_null) {
                entry.2.push(text(krate, "name").to_string());
            }
        }
    }
    let mut texts: Vec<_> = texts.into_values().filter(|(_, _, names)| !names.is_empty()).collect();
    for (_, _, names) in &mut texts {
        names.sort();
        names.dedup();
    }
    texts.sort_by(|a, b| (&a.0, &a.2).cmp(&(&b.0, &b.2)));

    let mut notices: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    // RustGIS's own crates in this package.
    let mut own = Vec::new();
    for c in about.get("crates").and_then(Value::as_array).unwrap_or(&empty) {
        let p = c.get("package").unwrap_or(&Value::Null);
        if p.get("source").is_some_and(Value::is_null) {
            own.push(text(p, "name"));
            continue;
        }
        let found = notice(text(p, "manifest_path"));
        if !found.is_empty() {
            notices.entry(text(p, "name").to_string()).or_insert(found);
        }
    }

    let rule = "=".repeat(100);
    let mut out = format!(
        "Third-party software notices for {what}\n\n\
         RustGIS is licensed under the Apache License 2.0 (see LICENSE). It is built with the\n\
         open-source Rust crates below, each under its own licence, whose copyright notices and terms\n\
         are reproduced here, followed by the NOTICE files the crates ship, then by the code and data\n\
         from other projects in RustGIS's own crates. Generated from Cargo.lock and\n\
         xtask/src/notices.rs by `cargo xtask notices`.\n"
    );
    for (name, body, names) in &texts {
        out.push_str(&format!("\n{rule}\n{name}\nUsed by: {}\n{rule}\n\n{body}\n", wrap(names)));
    }
    for (krate, files) in &notices {
        for (file, body) in files {
            out.push_str(&format!("\n{rule}\n{file} of {krate}\n{rule}\n\n{}\n", tidy(body)));
        }
    }
    let dashes = "-".repeat(100);
    for (_, heading, texts) in included.iter().filter(|(krate, _, _)| own.contains(&krate.as_str())) {
        out.push_str(&format!("\n{rule}\n{heading}\n{rule}\n"));
        for (i, body) in texts.iter().enumerate() {
            if i > 0 {
                out.push_str(&format!("\n{dashes}\n"));
            }
            out.push_str(&format!("\n{}\n", tidy(body)));
        }
    }
    out
}

/// The NOTICE files next to the manifest at `manifest_path`.
fn notice_files(manifest_path: &str) -> Vec<(String, String)> {
    let Some(dir) = Path::new(manifest_path).parent() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let ok = name.to_ascii_uppercase().starts_with("NOTICE") && e.file_type().is_ok_and(|t| t.is_file());
            ok.then(|| std::fs::read_to_string(e.path()).ok().map(|body| (name, body))).flatten()
        })
        .collect();
    found.sort();
    found
}

/// `cargo about`'s JSON report for `manifest` (the workspace when empty).
fn about(root: &Path, manifest: &str) -> Result<Value, String> {
    let mut cmd = Command::new("cargo");
    // --fail: a crate whose licence cargo-about can't work out is an error, not a gap in the file.
    cmd.args(["about", "generate", "--locked", "--fail", "--format", "json"]).current_dir(root);
    if !manifest.is_empty() {
        cmd.args(["--manifest-path", manifest]);
    }
    println!(
        "$ cargo about generate --locked --fail --format json{}",
        if manifest.is_empty() { String::new() } else { format!(" --manifest-path {manifest}") }
    );
    let out = cmd.output().map_err(|e| format!("cargo about: {e} (install it with `{INSTALL}`)"))?;
    if !out.status.success() {
        return Err(format!("cargo about failed (install it with `{INSTALL}`):\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo about's report: {e}"))
}

/// Writes the files, or with `check` reports the ones out of date: `Err((STALE, …))` for those,
/// `Err((1, …))` for any other failure.
pub fn notices(root: &Path, check: bool) -> Result<(), (u8, String)> {
    let included = included(root).map_err(|e| (1, e))?;
    let mut stale = Vec::new();
    for (file, manifest, what) in FILES {
        let wanted = render(&about(root, manifest).map_err(|e| (1, e))?, what, &notice_files, &included);
        let path = root.join(file);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(wanted.as_str()) {
            println!("notices: {file} is up to date");
        } else if check {
            stale.push(*file);
        } else {
            std::fs::write(&path, wanted).map_err(|e| (1, format!("{file}: {e}")))?;
            println!("notices: wrote {file}");
        }
    }
    if stale.is_empty() {
        Ok(())
    } else {
        Err((STALE, format!("out of date: {}; run `cargo xtask notices` and commit the result", stale.join(", "))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_merges_texts_and_skips_rustgis_crates() {
        let krate = |name: &str, source: Value| serde_json::json!({"crate": {"name": name, "version": "1.0.0", "source": source}});
        let reg = Value::from("registry+https://github.com/rust-lang/crates.io-index");
        let about = serde_json::json!({
            "licenses": [
                {"name": "MIT License", "id": "MIT", "text": "Copyright A\r\n\r\nPermission is granted.  \r\n", "used_by": [krate("b", reg.clone()), krate("a", reg.clone())]},
                // The same text wrapped differently: one entry.
                {"name": "MIT License", "id": "MIT", "text": "Copyright A\n\nPermission\nis granted.", "used_by": [krate("a", reg.clone()), krate("c", reg.clone())]},
                {"name": "Apache License 2.0", "id": "Apache-2.0", "text": "Apache text", "used_by": [krate("rustgis-geom", Value::Null)]},
            ],
            "crates": [
                {"package": {"name": "a", "source": reg.clone(), "manifest_path": "/a/Cargo.toml"}},
                {"package": {"name": "rustgis-geom", "source": Value::Null, "manifest_path": "/g/Cargo.toml"}},
            ],
        });
        let notice = |m: &str| if m == "/a/Cargo.toml" { vec![("NOTICE".to_string(), "Notice of a\n".to_string())] } else { Vec::new() };
        let out = render(&about, "RustGIS", &notice, &[]);
        assert!(out.starts_with("Third-party software notices for RustGIS\n\nRustGIS is licensed"));
        assert_eq!(out.matches("MIT License\nUsed by: a, b, c\n").count(), 1);
        assert!(out.contains("Copyright A\n\nPermission is granted.\n"));
        // Only RustGIS's own crates use that Apache text, so it is left out.
        assert!(!out.contains("Apache text") && !out.contains("rustgis-geom"));
        assert!(out.ends_with(&format!("NOTICE of a\n{}\n\nNotice of a\n", "=".repeat(100))));
    }

    /// Code and data from other projects go in the files of the packages whose RustGIS crates
    /// hold them, after the crates' notices, with each licence text tidied.
    #[test]
    fn render_adds_included_material_of_the_package_crates() {
        let about = serde_json::json!({
            "licenses": [],
            "crates": [{"package": {"name": "rustgis-map", "source": Value::Null, "manifest_path": "/m/Cargo.toml"}}],
        });
        let inc = |krate: &str, heading: &str, texts: &[&str]| {
            (krate.to_string(), heading.to_string(), texts.iter().map(|t| t.to_string()).collect())
        };
        let included = [
            inc("rustgis-map", "Colour maps\nIn rustgis-map: a.rs", &["MIT text  \r\n", "BSD text"]),
            inc("rustgis-tiles", "Labels", &["ISC text"]),
        ];
        let out = render(&about, "RustGIS", &|_| Vec::new(), &included);
        let rule = "=".repeat(100);
        let dashes = "-".repeat(100);
        assert!(out.ends_with(&format!("{rule}\nColour maps\nIn rustgis-map: a.rs\n{rule}\n\nMIT text\n\n{dashes}\n\nBSD text\n")));
        // rustgis-tiles isn't in this package.
        assert!(!out.contains("Labels") && !out.contains("ISC text"));
    }

    /// Every file `INCLUDED` names is in the repository, and every licence text it names is in
    /// `third-party/`: renaming or removing one means updating the list.
    #[test]
    fn included_files_exist() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let entries = included(root).unwrap();
        assert_eq!(entries.len(), INCLUDED.len());
        for inc in INCLUDED {
            assert!(inc.krate.starts_with("rustgis-") && !inc.paths.is_empty(), "{}", inc.what);
            let manifest = root.join("crates").join(inc.krate.trim_start_matches("rustgis-")).join("Cargo.toml");
            assert!(manifest.is_file(), "no crate {}", inc.krate);
            let dir = format!("crates/{}/", inc.krate.trim_start_matches("rustgis-"));
            assert!(inc.paths.iter().all(|p| p.starts_with(&dir)), "{:?} aren't all in {dir}", inc.paths);
        }
    }

    #[test]
    fn wrap_keeps_lines_short() {
        let names: Vec<String> = (0..40).map(|i| format!("crate-{i}")).collect();
        let w = wrap(&names);
        assert!(w.lines().all(|l| l.len() <= 100) && w.lines().count() > 1);
        assert!(w.ends_with("crate-39") && w.contains("crate-0, crate-1,"));
    }
}
