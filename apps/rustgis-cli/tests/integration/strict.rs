//! `run --strict` stops at the first command that loses data no `--allow` accepts (#2371).

use std::process::{Command, Output};

/// Removes the test's folder even when an assertion fails.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rustgis-cli")).args(["run", "--no-plugins"]).args(args).output().unwrap()
}

/// A GeoRSS feed of one polygon with this hole.
fn feed(hole: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><rss version="2.0" xmlns:georss="http://www.georss.org/georss" xmlns:gml="http://www.opengis.net/gml"><channel><title>t</title><item><title>i</title><georss:where><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>0 0 0 4 4 4 0 0</gml:posList></gml:LinearRing></gml:exterior><gml:interior><gml:LinearRing><gml:posList>{hole}</gml:posList></gml:LinearRing></gml:interior></gml:Polygon></georss:where></item></channel></rss>"#
    )
}

#[test]
fn a_strict_run_stops_at_a_loss_it_does_not_allow() {
    let dir = TempDir(std::env::temp_dir().join(format!("rustgis-cli-strict-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let lossy = dir.0.join("lossy.rss");
    let clean = dir.0.join("clean.rss");
    std::fs::write(&lossy, feed("1 x")).unwrap();
    std::fs::write(&clean, feed("1 1 1 2 2 2 1 1")).unwrap();
    let saved = dir.0.join("out.rgis");
    let (lossy, clean, saved) = (lossy.to_str().unwrap(), clean.to_str().unwrap(), saved.to_str().unwrap());
    let run = |input: &str, strict: &[&str]| {
        let _ = std::fs::remove_file(saved);
        let mut args = vec!["--data", input, "--cmd", "gp.threads", "--save", saved];
        args.extend(strict);
        let out = cli(&args);
        (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned(), !out.stdout.is_empty(), std::path::Path::new(saved).exists())
    };

    // Without --strict the loss is said and the run goes on.
    let (code, err, printed, wrote) = run(lossy, &[]);
    assert_eq!((code, printed, wrote), (Some(0), true, true), "{err}");
    // Strict: it stops after reading, before the next command and the save, and says the code.
    let (code, err, printed, wrote) = run(lossy, &["--strict"]);
    assert_eq!((code, printed, wrote), (Some(3), false, false), "{err}");
    assert!(err.contains("stopped after layer.addData") && err.contains("dropped.ring (1 notice): Correct the unreadable"), "{err}");
    // Allowed by its code or its kind, it runs to the end; another kind doesn't allow it.
    for allow in [&["--allow", "dropped.ring"][..], &["--allow", "dropped"], &["--allow=dropped.ring"]] {
        let (code, err, _, wrote) = run(lossy, &[&["--strict"][..], allow].concat());
        assert_eq!((code, wrote), (Some(0), true), "{allow:?}: {err}");
    }
    let (code, err, _, _) = run(lossy, &["--strict", "--allow", "changed"]);
    assert_eq!(code, Some(3), "{err}");
    // Clean input passes.
    let (code, err, printed, wrote) = run(clean, &["--strict"]);
    assert_eq!((code, printed, wrote), (Some(0), true, true), "{err}");
}

#[test]
fn a_strict_run_stops_at_a_command_that_loses_data() {
    let out = cli(&[
        "--strict",
        "--cmd",
        r#"layer.addDataText={"fileName":"records.csv","text":"n\n1\n2\n"}"#,
        "--cmd",
        r#"edit.enable={"on":true}"#,
        "--cmd",
        r#"edit.calculateField={"table":"records","field":"n","expression":"n + 1.5"}"#,
        "--cmd",
        "gp.threads",
    ]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(err.contains("stopped after edit.calculateField") && err.contains("changed.truncated (1 notice)"), "{err}");
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(!printed.contains("\"cores\""), "the command after it didn't run: {printed}");
}

/// A failed command still reports the losses its rollback can't take back (a counted omission):
/// strict says them and exits 3, after the failure.
#[test]
fn a_strict_run_reports_the_losses_of_a_command_that_failed() {
    const WMS: &str = r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Title>Test WMS</Title></Service>
<Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://maps.example.com/wms?"/></Get></HTTP></DCPType></GetMap></Request>
<Layer><CRS>EPSG:3857</CRS><Layer><Name>roads</Name><Title>Roads</Title>
<Style><Name>default</Name><LegendURL><Format>image/png</Format><OnlineResource xlink:href="LEGEND"/></LegendURL></Style></Layer></Layer></Capability></WMS_Capabilities>"#;
    let legend = format!("https://maps.example.com/legends/{}.png", "a".repeat(5000));
    let add = serde_json::json!({"url": "https://maps.example.com/wms", "layer": "roads", "text": WMS.replace("LEGEND", &legend)});
    let fail = serde_json::json!({"fileName": "broken.geojson", "text": "{"});
    let script = serde_json::json!({"script": format!("service.add {add}\nlayer.addDataText {fail}")});
    let out = cli(&["--strict", "--cmd", &format!("script.run={script}")]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(err.contains("stopped after script.run") && err.contains("dropped.legend (1 notice)"), "{err}");
    assert!(err.contains("line 2: layer.addDataText"), "the failure is said too: {err}");
}

#[test]
fn allow_takes_a_known_kind_or_code_and_needs_strict() {
    for (args, said) in [
        (&["--strict", "--allow", "dropped.everything"][..], "--allow dropped.everything: not a loss kind"),
        (&["--strict", "--allow"][..], "--allow needs a loss kind or code"),
        (&["--allow", "dropped"][..], "--allow needs --strict"),
        (&["--strict", "--allow=dropped.everything"][..], "--allow dropped.everything: not a loss kind"),
        (&["--strict", "--allow="][..], "--allow needs a loss kind or code"),
        (&["--strict=yes"][..], "--strict takes no value"),
    ] {
        let out = cli(&[args, &["--cmd", "gp.threads"]].concat());
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(err.contains(said), "{args:?}: {err}");
    }
}
