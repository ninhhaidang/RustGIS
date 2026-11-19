//! `cargo xtask memory [--cli <rustgis-cli>] [--update]`: checks that the command line's peak
//! memory for a fixed set of workflows has not grown (Linux only).
//!
//! It writes a generated point table (`ROWS` rows, 60 mixed fields) and a join table into a
//! temporary directory and has the command line turn the point table into FlatGeobuf, whose reader
//! peaks little above the table it reads (reading CSV briefly holds about twice the table, which
//! would hide a later copy under the load's peak). Then it runs `rustgis-cli run` once per
//! scenario, each in its own process under GNU `time` (`/usr/bin/time`, which reports the child's
//! peak resident memory), and divides each scenario's peak by the peak of the scenario that only
//! loads the table. Those ratios depend less on the machine than megabytes do; they are checked
//! against `xtask/memory-limits.txt`.
//!
//! A ratio over its limit fails the check: something now holds more memory than it did. A ratio
//! well under its limit is reported, so the limit can come down with the fix that lowered it
//! (`--update` writes the measured ratios plus the margin). The limits only ever move down.
//!
//! Without `--cli`, it builds the release `rustgis-cli` first.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Rows in the generated table: enough that the table dominates the process's own memory, few
/// enough that every scenario takes a few seconds.
const ROWS: usize = 60_000;
/// Rows in the join table, keyed on every other row of the main table.
const JOIN_ROWS: usize = ROWS / 2;
/// Attribute fields besides the coordinates: text, whole numbers and decimals in turn.
const FIELDS: usize = 60;
/// The memory the command line plans its limits for (`RUSTGIS_MEMORY_BYTES`), whatever the
/// machine has: an 8 GB laptop's, the computer RustGIS should work on at county scale. Its Undo
/// history then keeps up to 2 GiB of copies.
const MEMORY: u64 = 8 << 30;
/// Threads the command line may use, so a runner's core count doesn't change the peak.
const THREADS: &str = "4";
/// Headroom over a measured ratio: 0.10 plus 5% of it. Repeated runs of one binary agree within
/// 0.06, and the dev-profile binary CI runs measures up to 4% under the release one (its larger
/// program raises the baseline). One more copy of the table adds about 0.75, more than the margin
/// of every ratio but the full Undo history's (0.94 at 17); there `edits_growth`, near 0, catches
/// a history that grows past its budget, and `edits_2` an edit that copies more.
fn margin(ratio: f64) -> f64 {
    0.10 + ratio * 0.05
}

/// The one absolute limit: loading's peak in MiB. Every ratio divides by it, so a loader that
/// held twice as much would lower them all and pass; the generated table is the same every run,
/// so its load peak is too (168 MiB here, 172 on a GitHub runner's dev build).
const LOAD: &str = "load_mib";

/// Headroom over a measured value: 15% for the load peak in MiB, else [`margin`].
fn headroom(name: &str, value: f64) -> f64 {
    if name == LOAD { value * 0.15 } else { margin(value) }
}

/// How a value reads in messages: MiB for the load peak, else a multiple of it.
fn unit(name: &str) -> &'static str {
    if name == LOAD { " MiB" } else { "× the baseline" }
}
/// The file of limits, from the workspace root.
pub(crate) const LIMITS: &str = "xtask/memory-limits.txt";

/// One workflow, run in its own process: the commands after loading the table.
struct Scenario {
    name: &'static str,
    what: &'static str,
    cmds: Vec<String>,
    save: bool,
}

/// `edit.setValue` on the first `k` rows, one command each.
fn edits(k: usize) -> Vec<String> {
    (0..k).map(|i| format!(r#"edit.setValue={{"layer":"rows","row":{i},"field":"f1","value":{i}}}"#)).collect()
}

/// `path` as a JSON string.
fn quoted(path: &Path) -> String {
    serde_json::Value::String(path.display().to_string()).to_string()
}

fn scenarios(dir: &Path) -> Vec<Scenario> {
    let path = |f: &str| quoted(&dir.join(f));
    let join = format!(r#"layer.addData={{"path":{},"name":"j"}}"#, path("join.csv"));
    vec![
        Scenario { name: "load", what: "load the table (the baseline)", cmds: vec![], save: false },
        Scenario {
            name: "join",
            what: "Join Field from a second table into a new layer",
            cmds: vec![
                join,
                r#"gp.run={"tool":"joinField","params":{"target":"rows","targetField":"key","joinTable":"j","joinField":"key","fields":"j1,j2,j3","duplicates":"First","output":"joined"}}"#.into(),
            ],
            save: false,
        },
        Scenario {
            name: "calculate",
            what: "Add Field, then Calculate Field",
            cmds: vec![
                r#"edit.addField={"layer":"rows","name":"calc","type":"double"}"#.into(),
                r#"edit.calculateField={"layer":"rows","field":"calc","expression":"f1 * 2 + f2"}"#.into(),
            ],
            save: false,
        },
        Scenario {
            name: "export",
            what: "Export Features with a filter, written to CSV",
            cmds: vec![
                r#"gp.run={"tool":"exportFeatures","params":{"input":"rows","where":"f1 < 500000","output":"half"}}"#.into(),
                format!(r#"share.exportLayer={{"layer":"half","path":{}}}"#, path("half.csv")),
            ],
            save: false,
        },
        Scenario { name: "edits_2", what: "2 one-cell edits", cmds: edits(2), save: false },
        Scenario { name: "edits_24", what: "24 one-cell edits: Undo holds as much as its budget", cmds: edits(24), save: false },
        Scenario { name: "edits_32", what: "32 one-cell edits", cmds: edits(32), save: false },
        Scenario { name: "save", what: "1 one-cell edit, then save the project", cmds: edits(1), save: true },
    ]
}

/// A small deterministic generator (SplitMix64), so every run writes the same tables.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Writes `rows.csv` (points with a `key` and `FIELDS` attributes) and `join.csv` into `dir`.
/// CSV because it is simple to write here; the scenarios read the points from FlatGeobuf.
fn write_tables(dir: &Path) -> Result<(), String> {
    let mut rng = Rng(1);
    let mut csv = String::from("x,y,key");
    for i in 0..FIELDS {
        let _ = write!(csv, ",f{i}");
    }
    csv.push('\n');
    for row in 0..ROWS {
        let x = -93.5 + rng.below(400_000) as f64 / 1e6;
        let y = 44.8 + rng.below(400_000) as f64 / 1e6;
        let _ = write!(csv, "{x:.6},{y:.6},{row}");
        for i in 0..FIELDS {
            let _ = match i % 3 {
                0 => write!(csv, ",text {}", rng.below(1000)),
                1 => write!(csv, ",{}", rng.below(1_000_000)),
                _ => write!(csv, ",{:.3}", rng.below(10_000_000) as f64 / 1000.0),
            };
        }
        csv.push('\n');
    }
    std::fs::write(dir.join("rows.csv"), csv).map_err(|e| format!("rows.csv: {e}"))?;
    let mut join = String::from("key,j1,j2,j3\n");
    for k in 0..JOIN_ROWS {
        let _ = writeln!(join, "{},{},code {},{:.2}", k * 2, rng.below(1_000_000), rng.below(100), rng.below(100_000) as f64 / 100.0);
    }
    std::fs::write(dir.join("join.csv"), join).map_err(|e| format!("join.csv: {e}"))
}

/// `rustgis-cli run`'s options for every measured run: no installed plugins, the built-in
/// worldview (another one is downloaded first), and a fixed thread count.
const RUN: [&str; 6] = ["run", "--no-plugins", "--worldview", "default", "--threads", THREADS];

/// RustGIS's own directory and worldview overrides, which would bring the person's plugins,
/// settings or downloads into the run.
const UNSET: [&str; 6] =
    ["RUSTGIS_PLUGINS_DIR", "RUSTGIS_APP_DATA_DIR", "RUSTGIS_CONFIG_DIR", "RUSTGIS_CACHE_DIR", "RUSTGIS_WORLDVIEW", "RUSTGIS_LANGUAGE"];

/// Gives `cmd` a clean home in `dir` and [`MEMORY`], so neither the person's settings (threads,
/// plugins) nor the machine's memory change the result.
fn isolate(cmd: &mut Command, dir: &Path) -> Result<(), String> {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    cmd.env("HOME", &home).env("XDG_CONFIG_HOME", home.join(".config")).env("XDG_DATA_HOME", home.join(".local/share"));
    for var in UNSET {
        cmd.env_remove(var);
    }
    cmd.env("RUSTGIS_MEMORY_BYTES", MEMORY.to_string());
    Ok(())
}

/// Prints the notices a successful run gave (fields cut, rows skipped, a CRS assumed), so a
/// change in what the generated tables load as shows in the log beside the ratios.
fn echo_notices(label: &str, stderr: &[u8]) {
    for line in String::from_utf8_lossy(stderr).lines().filter(|l| !l.trim().is_empty()) {
        println!("  {label}: {line}");
    }
}

/// Runs one scenario and returns its peak resident memory in KiB.
fn peak_kib(cli: &Path, dir: &Path, s: &Scenario) -> Result<u64, String> {
    let report = dir.join(format!("{}.peak", s.name));
    let mut cmd = Command::new("/usr/bin/time");
    cmd.args(["-f", "%M", "-o"]).arg(&report).arg(cli).args(RUN);
    isolate(&mut cmd, dir)?;
    cmd.arg("--cmd").arg(format!(r#"layer.addData={{"path":{},"name":"rows"}}"#, quoted(&dir.join("rows.fgb"))));
    for c in &s.cmds {
        cmd.arg("--cmd").arg(c);
    }
    if s.save {
        cmd.arg("--save").arg(dir.join("saved.rgis"));
    }
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("/usr/bin/time: {e}; this check needs GNU time (on Debian or Ubuntu: apt install time)"))?;
    let text = std::fs::read_to_string(&report).unwrap_or_default();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(20).collect();
        return Err(format!("scenario {} failed ({}):\n{}", s.name, out.status, tail.into_iter().rev().collect::<Vec<_>>().join("\n")));
    }
    echo_notices(s.name, &out.stderr);
    text.lines()
        .rev()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .ok_or_else(|| format!("scenario {}: no peak memory in /usr/bin/time's report: {text:?}", s.name))
}

/// The limits file: `name = ratio` lines; `#` starts a comment.
pub(crate) fn parse_limits(text: &str) -> Result<Vec<(String, f64)>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once('=').ok_or_else(|| format!("{LIMITS}:{}: expected `name = ratio`", n + 1))?;
        let value: f64 = value.trim().parse().map_err(|e| format!("{LIMITS}:{}: {e}", n + 1))?;
        if !(value.is_finite() && value > 0.0) {
            return Err(format!("{LIMITS}:{}: the ratio must be a positive number", n + 1));
        }
        out.push((name.trim().to_string(), value));
    }
    Ok(out)
}

/// The ratios to check: each scenario's peak over the baseline's, and the growth from 24 to 32
/// edits. Each edit's Undo step holds a copy of the table (about 0.75 of the baseline), and from
/// about 21 edits on the 2 GiB the Undo history keeps at [`MEMORY`] is full, so more edits should
/// add nothing.
pub(crate) fn ratios(peaks: &[(&str, u64)]) -> Result<Vec<(String, f64)>, String> {
    let get = |n: &str| peaks.iter().find(|(p, _)| *p == n).map(|(_, kib)| *kib as f64);
    let base = get("load").filter(|b| *b > 0.0).ok_or("no baseline peak")?;
    let mut out: Vec<(String, f64)> = vec![(LOAD.to_string(), base / 1024.0)];
    out.extend(peaks.iter().filter(|(n, _)| *n != "load").map(|(n, kib)| (n.to_string(), *kib as f64 / base)));
    if let (Some(full), Some(more)) = (get("edits_24"), get("edits_32")) {
        out.push(("edits_growth".into(), (more - full).max(0.0) / base));
    }
    Ok(out)
}

/// Compares measured ratios with the limits: the failures, and the notes on limits that could
/// come down.
pub(crate) fn compare(measured: &[(String, f64)], limits: &[(String, f64)]) -> (Vec<String>, Vec<String>) {
    let (mut over, mut under) = (Vec::new(), Vec::new());
    for (name, ratio) in measured {
        match limits.iter().find(|(n, _)| n == name) {
            None => over.push(format!("{name}: {ratio:.2}{}, but {LIMITS} has no limit for it", unit(name))),
            Some((_, limit)) if *ratio > *limit => over.push(format!(
                "{name}: {ratio:.2}{u}, over its limit of {limit:.2}{u} by {:.2} (+{:.0}%)",
                ratio - limit,
                (ratio / limit - 1.0) * 100.0,
                u = unit(name)
            )),
            Some((_, limit)) if *ratio + 2.0 * headroom(name, *ratio) < *limit => {
                under.push(format!("{name}: {ratio:.2}{u}, well under its limit of {limit:.2}{u}", u = unit(name)))
            }
            Some(_) => {}
        }
    }
    for (name, _) in limits {
        if !measured.iter().any(|(n, _)| n == name) {
            over.push(format!("{name}: in {LIMITS} but not measured; remove it"));
        }
    }
    (over, under)
}

/// The limits file `--update` writes: each measured value plus its headroom, rounded up (to 0.05,
/// or 5 MiB for the load peak), never above the current limit.
fn updated(text: &str, measured: &[(String, f64)], limits: &[(String, f64)]) -> String {
    let mut out: String = text.lines().take_while(|l| l.starts_with('#') || l.trim().is_empty()).map(|l| format!("{l}\n")).collect();
    for (name, ratio) in measured {
        let step = if name == LOAD { 0.2 } else { 20.0 };
        let new = ((ratio + headroom(name, *ratio)) * step).ceil() / step;
        let limit = limits.iter().find(|(n, _)| n == name).map_or(new, |(_, l)| l.min(new));
        let _ = writeln!(out, "{name} = {limit:.2}");
    }
    out
}

/// Builds the release `rustgis-cli` when no `--cli` is given, and returns the executable Cargo
/// reports for it, wherever the target directory is configured.
fn build_cli(root: &Path) -> Result<PathBuf, String> {
    println!("$ cargo build --release --locked -p rustgis-cli");
    let out = Command::new(env!("CARGO"))
        .args(["build", "--release", "--locked", "-p", "rustgis-cli", "--message-format=json-render-diagnostics"])
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("cargo build --release -p rustgis-cli failed".into());
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "rustgis-cli")
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| "cargo built rustgis-cli but reported no executable for it".into())
}

/// `cargo xtask memory`: writes the data, runs every scenario once under GNU time with the memory
/// override, prints each ratio against its limit, and fails when one is over; `--update` lowers
/// limits that fell, never raising one.
pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    const USAGE: &str = "usage: cargo xtask memory [--cli <rustgis-cli>] [--update]";
    if !cfg!(target_os = "linux") {
        return Err("cargo xtask memory runs on Linux only (it reads peak memory through GNU time)".into());
    }
    let (mut cli, mut update) = (None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cli" => cli = Some(PathBuf::from(it.next().ok_or(USAGE)?)),
            "--update" => update = true,
            _ => return Err(USAGE.into()),
        }
    }
    let cli = match cli {
        Some(c) => c,
        None => build_cli(root)?,
    };
    let limits_path = root.join(LIMITS);
    let limits_text = std::fs::read_to_string(&limits_path).map_err(|e| format!("{LIMITS}: {e}"))?;
    let limits = parse_limits(&limits_text)?;

    let dir = std::env::temp_dir().join(format!("rustgis-memory-{}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let result = measure(&cli, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    let peaks = result?;

    let named: Vec<(&str, u64)> = peaks.iter().map(|(s, kib)| (s.name, *kib)).collect();
    let measured = ratios(&named)?;
    println!("{:<14} {:>9} {:>8} {:>8}  what", "scenario", "peak MiB", "ratio", "limit");
    for (s, kib) in &peaks {
        let ratio = measured.iter().find(|(n, _)| n == s.name).map(|(_, r)| *r);
        let key = if s.name == "load" { LOAD } else { s.name };
        let limit = limits.iter().find(|(n, _)| n == key).map(|(_, l)| *l);
        let fmt = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.2}"));
        println!("{:<14} {:>9.0} {:>8} {:>8}  {}", s.name, *kib as f64 / 1024.0, fmt(ratio), fmt(limit), s.what);
    }
    if let Some((_, g)) = measured.iter().find(|(n, _)| n == "edits_growth") {
        let limit = limits.iter().find(|(n, _)| n == "edits_growth").map_or("-".to_string(), |(_, l)| format!("{l:.2}"));
        println!("{:<14} {:>9} {:>8.2} {:>8}  the peak's growth from 24 to 32 edits", "edits_growth", "", g, limit);
    }

    let (mut over, under) = compare(&measured, &limits);
    if update {
        let text = updated(&limits_text, &measured, &limits);
        std::fs::write(&limits_path, text).map_err(|e| format!("{LIMITS}: {e}"))?;
        println!("memory: wrote {LIMITS}; a limit only comes down, so check the diff");
        for (name, _) in limits.iter().filter(|(n, _)| !measured.iter().any(|(m, _)| m == n)) {
            println!("memory: dropped {name} from {LIMITS}: no scenario measures it");
        }
        // The file now matches the scenarios, new ones included; a value over its limit still fails.
        over.retain(|o| !o.ends_with("not measured; remove it") && !o.ends_with("has no limit for it"));
    }
    for u in &under {
        println!("memory: {u}: if a change lowered it on purpose, run `cargo xtask memory --update` and commit {LIMITS}");
    }
    if over.is_empty() {
        println!("memory: ok");
        Ok(())
    } else {
        Err(format!(
            "memory: the peak grew past its limit\n  {}\nA scenario holds more memory than it did: look for a new copy of the layer's rows in the commands it runs. \
             Limits only come down; if the growth is deliberate and unavoidable, raise its line in {LIMITS} and say why in the pull request.",
            over.join("\n  ")
        ))
    }
}

/// Runs every scenario on freshly written tables.
fn measure(cli: &Path, dir: &Path) -> Result<Vec<(Scenario, u64)>, String> {
    write_tables(dir)?;
    let mut convert = Command::new(cli);
    isolate(&mut convert, dir)?;
    let out = convert
        .args(RUN)
        .arg("--cmd")
        .arg(format!(r#"layer.addData={{"path":{},"name":"rows"}}"#, quoted(&dir.join("rows.csv"))))
        .arg("--cmd")
        .arg(format!(r#"share.exportLayer={{"layer":"rows","path":{}}}"#, quoted(&dir.join("rows.fgb"))))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", cli.display()))?;
    if !out.status.success() {
        return Err(format!("writing rows.fgb failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    echo_notices("rows.fgb", &out.stderr);
    let mut out = Vec::new();
    for s in scenarios(dir) {
        let kib = peak_kib(cli, dir, &s)?;
        out.push((s, kib));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_limits_cover_every_scenario() {
        let text = include_str!("../memory-limits.txt");
        let limits = parse_limits(text).unwrap();
        let mut names: Vec<String> = vec![LOAD.to_string()];
        names.extend(scenarios(Path::new("/x")).iter().filter(|s| s.name != "load").map(|s| s.name.to_string()));
        names.push("edits_growth".into());
        let limited: Vec<String> = limits.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, limited);
    }

    #[test]
    fn ratios_compare_with_their_limits() {
        let peaks = [("load", 100), ("edits_2", 150), ("edits_24", 150), ("edits_32", 300)];
        let measured = ratios(&peaks).unwrap();
        assert_eq!(
            measured,
            vec![
                (LOAD.into(), 100.0 / 1024.0),
                ("edits_2".into(), 1.5),
                ("edits_24".into(), 1.5),
                ("edits_32".into(), 3.0),
                ("edits_growth".into(), 1.5)
            ]
        );
        let limits =
            parse_limits("# c\nload_mib = 0.12\nedits_2 = 2.0\nedits_24 = 1.6\nedits_32 = 2.5 # grows\nedits_growth = 1.6\n").unwrap();
        let (over, under) = compare(&measured, &limits);
        assert_eq!(over.len(), 1, "{over:?}");
        assert!(over[0].starts_with("edits_32: 3.00× the baseline, over its limit of 2.50×"), "{over:?}");
        assert_eq!(under.len(), 1, "{under:?}");
        assert!(under[0].starts_with("edits_2"), "{under:?}");
        assert!(parse_limits("edits_2 = -1").is_err() && parse_limits("edits_2").is_err());

        // A loader holding twice as much lowers every ratio, but its own limit catches it.
        let (over, _) = compare(&[(LOAD.into(), 344.0)], &[(LOAD.into(), 200.0)]);
        assert!(over[0].starts_with("load_mib: 344.00 MiB, over its limit of 200.00 MiB"), "{over:?}");
    }

    #[test]
    fn update_only_lowers_limits() {
        let text = "# head\n\nedits_2 = 2.00\nedits_8 = 2.50\n";
        let limits = parse_limits(text).unwrap();
        let measured = vec![("edits_2".to_string(), 1.5), ("edits_8".to_string(), 3.0)];
        assert_eq!(updated(text, &measured, &limits), "# head\n\nedits_2 = 1.70\nedits_8 = 2.50\n");
        // The load peak gets 15% and rounds up to 5 MiB.
        assert_eq!(updated("", &[(LOAD.into(), 172.0)], &[]), "load_mib = 200.00\n");
    }

    #[test]
    fn the_tables_are_the_same_every_time() {
        let mut a = Rng(1);
        let mut b = Rng(1);
        assert!((0..100).all(|_| a.next() == b.next()));
    }
}
