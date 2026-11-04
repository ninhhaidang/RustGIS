//! Python environments for process plugins that declare `"python"` in `plugin.json` (desktop
//! and command line), so their users install nothing themselves.
//!
//! RustGIS downloads [uv](https://github.com/astral-sh/uv) once: a pinned version's wheel from
//! PyPI, checked against the SHA-256 pinned here. uv then makes each such plugin an environment
//! of its own, with a Python it downloads itself and the packages the plugin's requirements file
//! lists. The plugin's `run` names `python` for that environment's interpreter.
//!
//! ```text
//! <plugins>/tools/uv-<version>/uv    uv
//! <plugins>/tools/python/            the Pythons uv installed
//! <plugins>/tools/cache/             uv's download cache
//! <plugins>/envs/<id>/               a plugin's environment (`.rustgis-env` names what it holds)
//! ```
//!
//! An environment is made again when the plugin's Python version or requirements change.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::manifest::{Manifest, PythonDecl};

/// The uv release RustGIS uses.
pub const UV_VERSION: &str = "0.13.1";

/// uv's wheels on PyPI by platform: (OS, architecture, a part of the wheel's file name, its
/// SHA-256). The download must match the SHA-256 pinned here.
const UV_WHEELS: &[(&str, &str, &str, &str)] = &[
    ("linux", "x86_64", "manylinux_2_17_x86_64", "696dcf6d807d85da978f4db8472da81dccc1254b18b52cbd1367c4d923c421e0"),
    ("linux", "aarch64", "manylinux_2_17_aarch64", "f2518450da2f6d0ea8a943c9f402abda63d5a66acc9999216c2792115b922ba4"),
    ("macos", "aarch64", "macosx_11_0_arm64", "cfa719fbbd23f26713394dea2692b7f146aa1b5713ce5e776e878701214ea52c"),
    ("macos", "x86_64", "macosx_10_12_x86_64", "7e60223e3e9636a3734d6c11b3552aeed987c40f1ca2497a546af58f2f883d56"),
    ("windows", "x86_64", "win_amd64", "4f693cd4feb52f08f30474c4612f24be2050472609be65b9960b635f286da284"),
    ("windows", "aarch64", "win_arm64", "b173413a5dc18437b2d28266a02be636f35ac7e97741773aed163ed2143abfaf"),
];

/// The environment variable naming a uv to use instead of downloading one (offline or managed
/// installs).
pub const UV_ENV: &str = "RUSTGIS_UV";

/// The file in an environment naming what it was made for.
const MADE_FOR: &str = ".rustgis-env";

/// The largest uv wheel accepted (they are about 17 MB).
const MAX_WHEEL: u64 = 100 * 1024 * 1024;

fn plugins_dir() -> Result<PathBuf, String> {
    crate::store::dir().ok_or_else(|| "there is no folder for plugins".to_string())
}

/// Where plugin `id`'s environment is.
pub fn env_dir(id: &str) -> Option<PathBuf> {
    crate::store::dir().map(|d| d.join("envs").join(id))
}

/// The interpreter in an environment.
pub fn python_in(env: &Path) -> PathBuf {
    if cfg!(windows) { env.join("Scripts").join("python.exe") } else { env.join("bin").join("python") }
}

/// What an environment for `decl` holds: its Python version and its requirements file's
/// contents, hashed. The environment is made again when this changes.
fn made_for(decl: &PythonDecl, plugin_dir: &Path) -> Result<String, String> {
    let mut h = Sha256::new();
    h.update(decl.version.as_bytes());
    h.update(b"\n");
    if !decl.requirements.is_empty() {
        let path = plugin_dir.join(&decl.requirements);
        h.update(std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The check a process plugin's calls make before they start: is its environment there, and
/// made for what it now needs?
#[derive(Clone, Debug)]
pub struct EnvCheck {
    pub id: String,
    pub env: PathBuf,
    decl: PythonDecl,
    /// The plugin's folder, where its requirements file is. Read when checked, not when the
    /// plugin is loaded: an installed package is unpacked after it's checked.
    plugin_dir: PathBuf,
}

impl EnvCheck {
    /// The check for a plugin, if it declares a Python environment.
    pub fn of(manifest: &Manifest, plugin_dir: &Path) -> Result<Option<EnvCheck>, String> {
        let Some(decl) = &manifest.python else { return Ok(None) };
        let env = env_dir(&manifest.id).ok_or("there is no folder for plugins")?;
        Ok(Some(EnvCheck { id: manifest.id.clone(), env, decl: decl.clone(), plugin_dir: plugin_dir.to_path_buf() }))
    }

    /// What the environment must be made for now (see [`made_for`]).
    fn made_for(&self) -> Result<String, String> {
        made_for(&self.decl, &self.plugin_dir)
    }

    /// Whether the environment is ready to run the plugin.
    pub fn ready(&self) -> bool {
        python_in(&self.env).is_file()
            && self.made_for().is_ok_and(|want| std::fs::read_to_string(self.env.join(MADE_FOR)).is_ok_and(|s| s.trim() == want))
    }
}

/// Whether a loaded plugin needs a Python environment, and whether it's ready: `None` for a
/// plugin that doesn't declare one.
pub fn status(id: &str) -> Option<bool> {
    crate::get(id)?.env_check().map(EnvCheck::ready)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .tls_config(rustgis_data::http::tls_config())
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    agent.get(url).header("User-Agent", concat!("RustGIS/", env!("CARGO_PKG_VERSION"))).call().map_err(|e| format!("{url}: {e}"))
}

/// uv: the one [`UV_ENV`] names, else the one downloaded before, else a fresh download.
/// `progress` hears what's happening.
pub fn uv(progress: &mut dyn FnMut(&str)) -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(UV_ENV).filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let dir = plugins_dir()?.join("tools").join(format!("uv-{UV_VERSION}"));
    let exe = dir.join(if cfg!(windows) { "uv.exe" } else { "uv" });
    if exe.is_file() {
        return Ok(exe);
    }
    let (_, _, tag, sha256) = UV_WHEELS
        .iter()
        .find(|(os, arch, _, _)| *os == std::env::consts::OS && *arch == std::env::consts::ARCH)
        .ok_or("RustGIS can't make Python environments on this platform")?;
    progress(&format!("Downloading uv {UV_VERSION}"));
    let agent = agent();
    // The wheel's address from PyPI; what it serves must match the SHA-256 pinned here.
    let index = format!("https://pypi.org/pypi/uv/{UV_VERSION}/json");
    let text = get(&agent, &index)?.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let info: Value = serde_json::from_str(&text).map_err(|e| format!("{index}: {e}"))?;
    let url = info
        .get("urls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|u| u.get("filename").and_then(Value::as_str).is_some_and(|f| f.contains(tag) && f.ends_with(".whl")))
        .and_then(|u| u.get("url").and_then(Value::as_str))
        .ok_or_else(|| format!("PyPI has no uv {UV_VERSION} for {tag}"))?
        .to_string();
    let mut bytes = Vec::new();
    get(&agent, &url)?.body_mut().as_reader().take(MAX_WHEEL + 1).read_to_end(&mut bytes).map_err(|e| format!("{url}: {e}"))?;
    if bytes.len() as u64 > MAX_WHEEL {
        return Err(format!("{url} is unexpectedly large"));
    }
    let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual != *sha256 {
        return Err(format!("uv {UV_VERSION}: the download doesn't match its pinned checksum"));
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("the uv wheel: {e}"))?;
    let name = format!("uv-{UV_VERSION}.data/scripts/{}", if cfg!(windows) { "uv.exe" } else { "uv" });
    let mut program = Vec::new();
    zip.by_name(&name)
        .map_err(|_| format!("the uv wheel has no {name}"))?
        .take(MAX_WHEEL)
        .read_to_end(&mut program)
        .map_err(|e| format!("{name}: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    rustgis_data::io::write_atomic(&exe, &program).map_err(|e| format!("{}: {e}", exe.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("{}: {e}", exe.display()))?;
    }
    Ok(exe)
}

/// Runs uv with `args`, passing what it reports to `progress`; its last lines name a failure.
fn run_uv(uv: &Path, args: &[&std::ffi::OsStr], progress: &mut dyn FnMut(&str)) -> Result<(), String> {
    let tools = plugins_dir()?.join("tools");
    let mut c = Command::new(uv);
    c.args(args)
        // uv's Pythons and cache in RustGIS's folder, and only Pythons uv installed: a plugin
        // gets the same Python on every computer, whatever else is installed.
        .env("UV_PYTHON_INSTALL_DIR", tools.join("python"))
        .env("UV_CACHE_DIR", tools.join("cache"))
        .env("UV_PYTHON_PREFERENCE", "only-managed")
        .env("UV_NO_PROGRESS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = c.spawn().map_err(|e| format!("couldn't start uv ({e})"))?;
    // Standard output is drained on a thread, so a full pipe can't stall uv.
    let out = child.stdout.take().map(|o| {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut BufReader::new(o), &mut std::io::sink());
        })
    });
    let mut last = std::collections::VecDeque::new();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            let line = line.trim().chars().take(500).collect::<String>();
            if line.is_empty() {
                continue;
            }
            progress(&line);
            last.push_back(line);
            if last.len() > 5 {
                last.pop_front();
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if let Some(t) = out {
        let _ = t.join();
    }
    if status.success() { Ok(()) } else { Err(format!("uv failed: {}", Vec::from(last).join(" / "))) }
}

/// Makes plugin `id`'s Python environment if it isn't ready: uv, then a Python of the declared
/// version, then the packages the requirements file lists. Returns the interpreter. `progress`
/// hears each step and what uv reports. It can take minutes the first time (downloads).
pub fn prepare(id: &str, progress: &mut dyn FnMut(&str)) -> Result<PathBuf, String> {
    let plugin = crate::get(id).ok_or_else(|| format!("no plugin '{id}'"))?;
    let m = &plugin.manifest;
    let decl = m.python.as_ref().ok_or_else(|| format!("{} doesn't use a Python environment", m.name))?;
    let check = plugin.env_check().ok_or_else(|| format!("{} doesn't use a Python environment", m.name))?;
    let plugin_dir = plugin.dir().ok_or_else(|| format!("{} has no folder", m.name))?;
    if check.ready() {
        return Ok(python_in(&check.env));
    }
    let parent = check.env.parent().ok_or("there is no folder for plugins")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    // One at a time per plugin, across processes too (the app and rustgis-cli): an OS file
    // lock, which the OS releases if its holder dies. Whoever waited finds the environment the
    // other made, and is done.
    let lock_path = parent.join(format!(".{id}.lock"));
    let lock = std::fs::File::create(&lock_path).map_err(|e| format!("{}: {e}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            progress("Waiting for another preparation of this environment to finish");
            lock.lock().map_err(|e| format!("{}: {e}", lock_path.display()))?;
        }
        // A folder whose file system can't lock (some network mounts): prepared unlocked, as
        // before there was a lock, rather than not at all.
        Err(std::fs::TryLockError::Error(e)) => log::warn!("{}: can't lock ({e}); preparing without it", lock_path.display()),
    }
    if check.ready() {
        return Ok(python_in(&check.env));
    }
    let want = check.made_for()?;
    let uv = uv(progress)?;
    // Made beside it, then swapped in: a failure leaves the old environment (or none).
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = parent.join(format!(".{id}-{nanos}"));
    let res = (|| {
        progress(&format!("Installing Python {}", decl.version));
        run_uv(&uv, &["venv".as_ref(), "--relocatable".as_ref(), "--python".as_ref(), decl.version.as_ref(), tmp.as_os_str()], progress)?;
        if !decl.requirements.is_empty() {
            progress(&format!("Installing the packages in {}", decl.requirements));
            let python = python_in(&tmp);
            let reqs = plugin_dir.join(&decl.requirements);
            run_uv(
                &uv,
                &["pip".as_ref(), "install".as_ref(), "--python".as_ref(), python.as_os_str(), "-r".as_ref(), reqs.as_os_str()],
                progress,
            )?;
        }
        rustgis_data::io::write_atomic(&tmp.join(MADE_FOR), want.as_bytes()).map_err(|e| e.to_string())?;
        let old = parent.join(format!(".{id}-{nanos}-old"));
        let had_old = check.env.exists();
        if had_old {
            std::fs::rename(&check.env, &old).map_err(|e| format!("{}: {e}", check.env.display()))?;
        }
        if let Err(e) = std::fs::rename(&tmp, &check.env) {
            if had_old {
                let _ = std::fs::rename(&old, &check.env);
            }
            return Err(format!("{}: {e}", check.env.display()));
        }
        if had_old {
            let _ = std::fs::remove_dir_all(&old);
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    res?;
    progress("Ready");
    Ok(python_in(&check.env))
}

/// Removes plugin `id`'s environment, if it has one.
///
/// Its lock file (`.<id>.lock`) stays: deleting it while a preparation holds it would let another
/// lock a new file and run alongside. It's empty.
pub fn remove(id: &str) -> Result<(), String> {
    match env_dir(id).filter(|d| d.is_dir()) {
        Some(d) => std::fs::remove_dir_all(&d).map_err(|e| format!("{}: {e}", d.display())),
        None => Ok(()),
    }
}
