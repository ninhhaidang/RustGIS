//! The app's own folders on this computer: settings, app data and cache, each in the platform's
//! usual place unless an environment variable names it (a portable install, a test).
//!
//! | Folder | Override | Windows | macOS | Linux |
//! |---|---|---|---|---|
//! | [`config_dir`] | `RUSTGIS_CONFIG_DIR` | `%APPDATA%\RustGIS` | `~/Library/Application Support/RustGIS` | `$XDG_CONFIG_HOME/rustgis` (`~/.config/rustgis`) |
//! | [`app_data_dir`] | `RUSTGIS_APP_DATA_DIR` | `%LOCALAPPDATA%\RustGIS` | `~/Library/Application Support/RustGIS` | `$XDG_DATA_HOME/rustgis` (`~/.local/share/rustgis`) |
//! | [`cache_dir`] | `RUSTGIS_CACHE_DIR` | `%LOCALAPPDATA%\RustGIS\cache` | `~/Library/Caches/RustGIS` | `$XDG_CACHE_HOME/rustgis` (`~/.cache/rustgis`) |
//!
//! An override is used as is. There are none in the browser, where these give `None`.
//!
//! An app whose system gives it one folder of its own and no environment to name it in (Android)
//! calls [`set_root`] once at start-up: the folders are then `config`, `data` and `cache` in it,
//! unless an override names one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The app's own folder, where the system gives it one ([`set_root`]).
static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Puts the three folders in `root` (`config`, `data` and `cache`) instead of the platform's usual
/// places. For an app the system gives one folder of its own, called once before anything asks
/// for a folder. Returns whether it took: the first call wins, a later one changes nothing.
pub fn set_root(root: PathBuf) -> bool {
    ROOT.set(root).is_ok()
}

/// The app's own folder, where the system gave it one ([`set_root`]): a phone's app storage.
/// `None` on a computer, whose folders follow its own conventions.
pub fn root() -> Option<PathBuf> {
    ROOT.get().cloned()
}

/// The environment variable that names the settings folder.
pub const CONFIG_ENV: &str = "RUSTGIS_CONFIG_DIR";
/// The environment variable that names the app-data folder.
pub const APP_DATA_ENV: &str = "RUSTGIS_APP_DATA_DIR";
/// The environment variable that names the cache folder.
pub const CACHE_ENV: &str = "RUSTGIS_CACHE_DIR";

/// One of the app's folders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Config,
    AppData,
    Cache,
}

/// The operating systems with their own conventions; every other one follows Linux's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Os {
    Windows,
    Mac,
    Other,
}

const OS: Os = if cfg!(target_os = "windows") {
    Os::Windows
} else if cfg!(target_os = "macos") {
    Os::Mac
} else {
    Os::Other
};

/// Settings: preferences, the crash log, AI Assistant sign-ins and skills.
pub fn config_dir() -> Option<PathBuf> {
    rooted(Kind::Config, OS, &env, ROOT.get().map(PathBuf::as_path))
}

/// App data kept on this computer: installed plugins and programs, recovery copies of unsaved work.
pub fn app_data_dir() -> Option<PathBuf> {
    rooted(Kind::AppData, OS, &env, ROOT.get().map(PathBuf::as_path))
}

/// Files that can be made again, such as format conversions.
pub fn cache_dir() -> Option<PathBuf> {
    rooted(Kind::Cache, OS, &env, ROOT.get().map(PathBuf::as_path))
}

/// An environment variable, unless it is unset or empty.
fn env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

/// The environment variable that overrides the folder `kind`.
fn override_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Config => CONFIG_ENV,
        Kind::AppData => APP_DATA_ENV,
        Kind::Cache => CACHE_ENV,
    }
}

/// The folder `kind`: its override, else its place in the app's own folder `root` when there is
/// one, else the platform's usual place.
fn rooted(kind: Kind, os: Os, env: &dyn Fn(&str) -> Option<OsString>, root: Option<&Path>) -> Option<PathBuf> {
    match root {
        Some(root) if env(override_name(kind)).is_none() => Some(root.join(match kind {
            Kind::Config => "config",
            Kind::AppData => "data",
            Kind::Cache => "cache",
        })),
        _ => resolve(kind, os, env),
    }
}

/// The folder `kind` on `os`, reading the environment through `env`.
fn resolve(kind: Kind, os: Os, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(d) = env(override_name(kind)) {
        return Some(PathBuf::from(d));
    }
    let var = |name: &str| env(name).map(PathBuf::from);
    let home = || var("HOME");
    match os {
        Os::Windows => match kind {
            Kind::Config => var("APPDATA").map(|d| d.join("RustGIS")),
            Kind::AppData => var("LOCALAPPDATA").or_else(|| var("APPDATA")).map(|d| d.join("RustGIS")),
            Kind::Cache => var("LOCALAPPDATA").map(|d| d.join("RustGIS").join("cache")),
        },
        Os::Mac => home().map(|h| match kind {
            Kind::Config | Kind::AppData => h.join("Library/Application Support/RustGIS"),
            Kind::Cache => h.join("Library/Caches/RustGIS"),
        }),
        Os::Other => {
            let (xdg, fallback) = match kind {
                Kind::Config => ("XDG_CONFIG_HOME", ".config"),
                Kind::AppData => ("XDG_DATA_HOME", ".local/share"),
                Kind::Cache => ("XDG_CACHE_HOME", ".cache"),
            };
            var(xdg).or_else(|| home().map(|h| h.join(fallback))).map(|d| d.join("rustgis"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let vars: Vec<(String, String)> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| vars.iter().find(|(k, v)| k == name && !v.is_empty()).map(|(_, v)| OsString::from(v))
    }

    fn all(os: Os, env: &dyn Fn(&str) -> Option<OsString>) -> [Option<PathBuf>; 3] {
        [Kind::Config, Kind::AppData, Kind::Cache].map(|k| resolve(k, os, env))
    }

    fn paths(p: [&str; 3]) -> [Option<PathBuf>; 3] {
        p.map(|p| Some(PathBuf::from(p)))
    }

    #[test]
    fn platform_folders() {
        let win = with(&[("APPDATA", r"C:\U\Roaming"), ("LOCALAPPDATA", r"C:\U\Local")]);
        let r = all(Os::Windows, &win);
        assert_eq!(r[0], Some(PathBuf::from(r"C:\U\Roaming").join("RustGIS")));
        assert_eq!(r[1], Some(PathBuf::from(r"C:\U\Local").join("RustGIS")));
        assert_eq!(r[2], Some(PathBuf::from(r"C:\U\Local").join("RustGIS").join("cache")));
        // Without LOCALAPPDATA, app data falls back to the roaming folder, the cache to nothing.
        let r = all(Os::Windows, &with(&[("APPDATA", r"C:\U\Roaming")]));
        assert_eq!(r[1], Some(PathBuf::from(r"C:\U\Roaming").join("RustGIS")));
        assert_eq!(r[2], None);

        let mac = with(&[("HOME", "/Users/a")]);
        assert_eq!(
            all(Os::Mac, &mac),
            paths([
                "/Users/a/Library/Application Support/RustGIS",
                "/Users/a/Library/Application Support/RustGIS",
                "/Users/a/Library/Caches/RustGIS"
            ])
        );

        let linux = with(&[("HOME", "/home/a")]);
        assert_eq!(all(Os::Other, &linux), paths(["/home/a/.config/rustgis", "/home/a/.local/share/rustgis", "/home/a/.cache/rustgis"]));
        let xdg = with(&[("HOME", "/home/a"), ("XDG_CONFIG_HOME", "/x/c"), ("XDG_DATA_HOME", "/x/d"), ("XDG_CACHE_HOME", "/x/k")]);
        assert_eq!(all(Os::Other, &xdg), paths(["/x/c/rustgis", "/x/d/rustgis", "/x/k/rustgis"]));

        assert_eq!(all(Os::Other, &with(&[])), [None, None, None]);
        assert_eq!(all(Os::Mac, &with(&[("HOME", "")])), [None, None, None]);
    }

    #[test]
    fn overrides_win_everywhere() {
        let env = with(&[
            ("HOME", "/home/a"),
            ("APPDATA", "/r"),
            ("LOCALAPPDATA", "/l"),
            (CONFIG_ENV, "/p/config"),
            (APP_DATA_ENV, "/p/data"),
            (CACHE_ENV, "/p/cache"),
        ]);
        for os in [Os::Windows, Os::Mac, Os::Other] {
            assert_eq!(all(os, &env), paths(["/p/config", "/p/data", "/p/cache"]), "{os:?}");
        }
        // An empty override is no override.
        let env = with(&[("HOME", "/home/a"), (CONFIG_ENV, "")]);
        assert_eq!(resolve(Kind::Config, Os::Other, &env), Some(PathBuf::from("/home/a/.config/rustgis")));
    }

    #[test]
    fn an_app_folder_holds_all_three_unless_overridden() {
        let root = Path::new("/app/files");
        let folders = |env: &dyn Fn(&str) -> Option<OsString>| {
            [Kind::Config, Kind::AppData, Kind::Cache].map(|k| rooted(k, Os::Other, env, Some(root)))
        };
        // No HOME, as on Android: the root alone places them.
        assert_eq!(folders(&with(&[])), paths(["/app/files/config", "/app/files/data", "/app/files/cache"]));
        // An override still names its own folder; the others stay in the root.
        assert_eq!(folders(&with(&[(CACHE_ENV, "/p/cache")])), paths(["/app/files/config", "/app/files/data", "/p/cache"]));
        // Without a root, the platform's places.
        let home = with(&[("HOME", "/home/a")]);
        assert_eq!(rooted(Kind::Config, Os::Other, &home, None), Some(PathBuf::from("/home/a/.config/rustgis")));
    }
}
