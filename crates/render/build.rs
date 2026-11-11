//! Fetches the Natural Earth sample data (public domain) that RustGIS embeds for its basemap
//! outline and World Atlas sample. The files live in opengeos/rustgis-assets, not in this repo.
//! The countries in other worldviews aren't embedded: the app downloads them when one is chosen.
//!
//! Set `RUSTGIS_DATA_DIR` to a folder holding the files to build offline.

use std::path::{Path, PathBuf};
use std::process::Command;

const BASE: &str = "https://assets.rustgis.app/data";
const FILES: [&str; 4] = ["countries.geojson", "cities.geojson", "rivers.geojson", "lakes.geojson"];

fn valid(p: &Path) -> bool {
    std::fs::read(p).is_ok_and(|b| b.len() > 2 && b.first() == Some(&b'{'))
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTGIS_DATA_DIR");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    let local = std::env::var_os("RUSTGIS_DATA_DIR").map(PathBuf::from);
    for f in FILES {
        let dest = out.join(f);
        if let Some(dir) = &local {
            let src = dir.join(f);
            if std::fs::copy(&src, &dest).is_err() || !valid(&dest) {
                panic!("RUSTGIS_DATA_DIR: {} is missing or not GeoJSON", src.display());
            }
            continue;
        }
        if valid(&dest) {
            continue;
        }
        let url = format!("{BASE}/{f}");
        // curl's own --retry skips some transient failures (a connection reset, error 35), so
        // retry here too, with a growing pause.
        let mut ok = false;
        for attempt in 1..=5u64 {
            ok = Command::new("curl")
                .args(["-fsSL", "--retry", "3", "--connect-timeout", "20", "--max-time", "120", "-o"])
                .arg(&dest)
                .arg(&url)
                .status()
                .is_ok_and(|s| s.success())
                && valid(&dest);
            if ok {
                break;
            }
            let _ = std::fs::remove_file(&dest);
            println!("cargo:warning=downloading {url} failed (attempt {attempt} of 5)");
            std::thread::sleep(std::time::Duration::from_secs(3 * attempt));
        }
        if !ok {
            let _ = std::fs::remove_file(&dest);
            panic!("could not download {url}\n(check the network, or set RUSTGIS_DATA_DIR to a folder holding {})", FILES.join(", "));
        }
    }
}
