use serde_json::{Value, json};

use crate::manifest::{Manifest, version_cmp};
use crate::{Host, Package, registry, testing};

/// Remembers the requests it gets and answers `host.echo` with its params.
#[derive(Default)]
struct Recorder {
    ops: Vec<String>,
    logs: Vec<String>,
}

impl Host for Recorder {
    fn call(&mut self, op: &str, params: &Value) -> Result<Value, String> {
        self.ops.push(op.to_string());
        match op {
            "host.echo" => Ok(params.clone()),
            _ => Err(format!("no '{op}'")),
        }
    }
    fn log(&mut self, text: &str) {
        self.logs.push(text.to_string());
    }
}

fn load(id: &str, perms: &[&str]) -> std::sync::Arc<crate::Plugin> {
    let pkg = Package::from_zip(&testing::package(id, perms)).unwrap();
    crate::load(pkg, "test").unwrap()
}

#[test]
fn packages_load_and_commands_run() {
    let p = load("unit-hello", &[]);
    assert_eq!(p.manifest.commands.len(), 6);
    let cmd = crate::command("plugin:unit-hello/h").unwrap();
    assert!(!cmd.mutates(), "no edit permission: a view command");
    let mut host = Recorder::default();
    assert_eq!(crate::run_command(&cmd, &mut host, &json!({})).unwrap(), json!({"hello": "world"}));
    assert_eq!(host.logs, vec!["hi"]);
    // Errors come back as text.
    let e = crate::command("plugin:unit-hello/e").unwrap();
    assert!(crate::run_command(&e, &mut host, &Value::Null).unwrap_err().contains("nope"));
    assert!(crate::commands().iter().any(|c| c.id == "plugin:unit-hello/f"));
    assert!(crate::unload("unit-hello"));
    assert!(crate::command("plugin:unit-hello/h").is_none());
}

#[test]
fn plugins_reach_the_host_through_requests() {
    load("unit-forward", &["edit"]);
    let cmd = crate::command("plugin:unit-forward/f").unwrap();
    assert!(cmd.mutates());
    let mut host = Recorder::default();
    let v = crate::run_command(&cmd, &mut host, &json!({"op": "host.echo", "params": {"a": [1, 2]}})).unwrap();
    assert_eq!(v, json!({"a": [1, 2]}));
    let e = crate::run_command(&cmd, &mut host, &json!({"op": "nope", "params": {}})).unwrap_err();
    assert!(e.contains("no 'nope'"), "{e}");
    assert_eq!(host.ops, vec!["host.echo", "nope"]);
    crate::unload("unit-forward");
}

/// Skills packages carry validated skills without code; coded plugins may carry skills too.
#[test]
fn plugins_ship_skills_and_skills_packages_have_no_code() {
    // A skills package: no module and no commands; its skills come from the package.
    let pkg = Package::from_zip(&testing::skills_package("unit-skills", &["flood-map", "gauge-report"])).unwrap();
    assert!(pkg.wasm.is_empty());
    assert_eq!(pkg.skills.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["flood-map", "gauge-report"]);
    let p = crate::load(pkg, "test").unwrap();
    assert_eq!(p.info()["runtime"], "skills");
    assert_eq!(p.info()["skills"], json!(["unit-skills:flood-map", "unit-skills:gauge-report"]));
    assert!(crate::skills().iter().any(|s| s.plugin == "unit-skills" && s.name == "flood-map" && s.text.contains("Do flood-map.")));
    assert!(crate::call(&p, &mut Recorder::default(), &json!({}), false).is_err());
    // Its skills go when it's disabled.
    crate::set_enabled("unit-skills", false).unwrap();
    assert!(!crate::skills().iter().any(|s| s.plugin == "unit-skills"));
    assert!(crate::unload("unit-skills"));
    // A plugin with code can ship skills too.
    let mut m: Value = serde_json::from_str(&testing::manifest("unit-coded", &[])).unwrap();
    m["skills"] = json!(["one"]);
    let m = m.to_string();
    let skill: &[u8] = b"---\nname: one\ndescription: d\n---\nbody";
    let zip = testing::zip_of(&[("plugin.json", m.as_bytes()), ("plugin.wasm", &testing::wasm()), ("skills/one/SKILL.md", skill)]);
    assert_eq!(Package::from_zip(&zip).unwrap().skills, vec![("one".to_string(), String::from_utf8_lossy(skill).into_owned())]);
    // Refused: a listed skill that isn't in the package, or one too large.
    let missing = testing::zip_of(&[("plugin.json", m.as_bytes()), ("plugin.wasm", &testing::wasm())]);
    assert!(Package::from_zip(&missing).unwrap_err().contains("skills/one/SKILL.md"));
    let huge = vec![b'x'; crate::package::MAX_SKILL + 1];
    let large = testing::zip_of(&[("plugin.json", m.as_bytes()), ("plugin.wasm", &testing::wasm()), ("skills/one/SKILL.md", &huge)]);
    assert!(Package::from_zip(&large).unwrap_err().contains("larger than 64 KB"), "a small limit reads in KB");
    // A skills package has skills and nothing else.
    let skills_only = |extra: Value| {
        let mut m = json!({"id": "s", "name": "S", "version": "1.0.0", "runtime": "skills", "skills": ["a"]});
        for (k, v) in extra.as_object().into_iter().flatten() {
            m[k] = v.clone();
        }
        Manifest::parse(&m.to_string())
    };
    assert!(skills_only(json!({})).is_ok());
    for bad in [
        json!({"skills": []}),
        json!({"skills": ["Bad Name"]}),
        json!({"skills": ["a", "a"]}),
        json!({"permissions": ["files"]}),
        json!({"commands": [{"id": "c", "label": "C"}]}),
        json!({"run": ["x"]}),
    ] {
        assert!(skills_only(bad.clone()).is_err(), "{bad}");
    }
}

/// Fuel, memory and reply bounds stop hostile modules and return errors to the host.
#[test]
fn hostile_modules_are_stopped_not_crashes() {
    load("unit-hostile", &[]);
    let mut host = Recorder::default();
    for (id, want) in [("s", "too long"), ("g", "out of bounds"), ("o", "error")] {
        let cmd = crate::command(&format!("plugin:unit-hostile/{id}")).unwrap();
        let e = crate::run_command(&cmd, &mut host, &json!({})).unwrap_err();
        assert!(e.contains(want), "{id}: {e}");
    }
    crate::unload("unit-hostile");
}

/// Invalid manifests, archives and module exports are refused during package loading.
#[test]
fn bad_packages_are_refused() {
    let wasm = testing::wasm();
    let man = |v: Value| testing::zip_of(&[("plugin.json", v.to_string().as_bytes()), ("plugin.wasm", &wasm)]);
    let ok = json!({"id": "x-ok", "name": "X", "version": "1.0.0"});
    assert!(Package::from_zip(&man(ok.clone())).is_ok());
    for (bad, why) in [
        (json!({"id": "Bad Id", "name": "X", "version": "1.0.0"}), "id"),
        (json!({"id": "x", "name": "", "version": "1.0.0"}), "name"),
        (json!({"id": "x", "name": "X", "version": "one"}), "version"),
        (json!({"id": "x", "name": "X", "version": "1.0.0", "entry": "../evil.wasm"}), "entry"),
        (json!({"id": "x", "name": "X", "version": "1.0.0", "abi": 99}), "ABI"),
        (json!({"id": "x", "name": "X", "version": "1.0.0", "minRustgisVersion": "999.0.0"}), "needs RustGIS"),
        (json!({"id": "x", "name": "X", "version": "1.0.0", "permissions": ["root"]}), "plugin.json"),
        (
            json!({"id": "x", "name": "X", "version": "1.0.0", "tools": [{"id": "t", "name": "T", "params": [{"name": "a", "label": "A", "kind": "laser"}]}]}),
            "kind",
        ),
        (
            json!({"id": "x", "name": "X", "version": "1.0.0", "commands": [{"id": "c", "label": "C"}, {"id": "c", "label": "D"}]}),
            "repeated",
        ),
    ] {
        let e = Package::from_zip(&man(bad.clone())).unwrap_err();
        assert!(e.contains(why), "{bad}: {e}");
    }
    assert!(Package::from_zip(b"not a zip").is_err());
    assert!(Package::from_zip(&testing::zip_of(&[("readme.txt", b"hi")])).unwrap_err().contains("no plugin.json"));
    // A package in one top-level folder is fine.
    let nested = testing::zip_of(&[("x/plugin.json", ok.to_string().as_bytes()), ("x/plugin.wasm", &wasm)]);
    assert!(Package::from_zip(&nested).is_ok());
    // A module that isn't one, or lacks the ABI's exports, doesn't load.
    let junk = testing::zip_of(&[("plugin.json", ok.to_string().as_bytes()), ("plugin.wasm", b"\0asm junk")]);
    assert!(crate::load(Package::from_zip(&junk).unwrap(), "t").is_err());
    let empty = wat::parse_str("(module)").unwrap();
    let e = testing::zip_of(&[("plugin.json", ok.to_string().as_bytes()), ("plugin.wasm", &empty)]);
    assert!(crate::load(Package::from_zip(&e).unwrap(), "t").unwrap_err().contains("doesn't export"));
    let wasi = wat::parse_str(r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32))) (memory (export "memory") 1) (func (export "rustgis_alloc") (param i32) (result i32) i32.const 0) (func (export "rustgis_call") (param i32 i32) (result i64) i64.const 0))"#).unwrap();
    let e = testing::zip_of(&[("plugin.json", ok.to_string().as_bytes()), ("plugin.wasm", &wasi)]);
    assert!(crate::load(Package::from_zip(&e).unwrap(), "t").unwrap_err().contains("doesn't provide"));
}

/// Catalog plugins can cross the old limit, reach the new one and run their last command.
#[test]
fn plugins_accept_up_to_256_commands() {
    let wasm = testing::wasm();
    let with_commands = |count: usize| {
        json!({"id": "unit-command-limit", "name": "Catalog", "version": "1.0.0",
            "commands": (0..count).map(|i| json!({"id": format!("h{i}"), "label": format!("Layer {i}")})).collect::<Vec<_>>()})
    };
    for count in [64, 65, 256] {
        let text = with_commands(count).to_string();
        let zip = testing::zip_of(&[("plugin.json", text.as_bytes()), ("plugin.wasm", &wasm)]);
        let pkg = Package::from_zip(&zip).unwrap();
        assert_eq!(pkg.manifest.commands.len(), count);
        if count == 256 {
            crate::load(pkg, "test").unwrap();
            let cmd = crate::command("plugin:unit-command-limit/h255").unwrap();
            assert_eq!(crate::run_command(&cmd, &mut Recorder::default(), &json!({})).unwrap(), json!({"hello": "world"}));
            assert!(crate::unload("unit-command-limit"));
        }
    }
    let e = Manifest::parse(&with_commands(257).to_string()).unwrap_err();
    assert!(e.contains("at most 256 commands and 64 tools"), "{e}");

    // Even a large catalog still needs unique command ids.
    let mut repeated = with_commands(256);
    repeated["commands"][255]["id"] = json!("h0");
    let e = Manifest::parse(&repeated.to_string()).unwrap_err();
    assert!(e.contains("command id 'h0' is invalid or repeated"), "{e}");
}

/// More commands do not raise the limits on tools or their parameters.
#[test]
fn plugin_tool_and_parameter_limits_stay_bounded() {
    let mut m = json!({"id": "bounded", "name": "Bounded", "version": "1.0.0",
        "commands": (0..256).map(|i| json!({"id": format!("c{i}"), "label": "Layer"})).collect::<Vec<_>>(),
        "tools": (0..64).map(|i| json!({"id": format!("t{i}"), "name": "Tool"})).collect::<Vec<_>>()});
    m["tools"][0]["params"] =
        json!((0..32).map(|i| json!({"name": format!("p{i}"), "label": "Parameter", "kind": "text"})).collect::<Vec<_>>());
    assert!(Manifest::parse(&m.to_string()).is_ok());
    let mut too_many_tools = m.clone();
    too_many_tools["tools"].as_array_mut().unwrap().push(json!({"id": "t64", "name": "Tool"}));
    let e = Manifest::parse(&too_many_tools.to_string()).unwrap_err();
    assert!(e.contains("at most 256 commands and 64 tools"), "{e}");
    m["tools"][0]["params"].as_array_mut().unwrap().push(json!({"name": "p32", "label": "Parameter", "kind": "text"}));
    let e = Manifest::parse(&m.to_string()).unwrap_err();
    assert!(e.contains("more than 32 parameters"), "{e}");
}

/// Enabled plugin tools register with geoprocessing and return layers or selections.
#[test]
fn tools_join_the_geoprocessing_tools() {
    crate::tools::register();
    load("unit-tools", &[]);
    let spec = rustgis_analysis::tool("plugin:unit-tools/m").unwrap();
    assert_eq!(spec.toolbox, "Plugins › Test unit-tools");
    assert_eq!(rustgis_analysis::external_source(spec), Some(crate::tools::SOURCE));
    assert!(matches!(spec.params.get(2).map(|p| p.kind), Some(rustgis_analysis::ParamKind::Choice(c)) if c == ["a", "b"]));
    struct One(rustgis_data::Dataset);
    impl rustgis_analysis::Inputs for One {
        fn dataset(&self, l: &str) -> Option<&rustgis_data::Dataset> {
            (l == "Cities").then_some(&self.0)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
    }
    let (ds, _) = rustgis_data::io::read_geojson(r#"{"type":"Point","coordinates":[3,4]}"#, "Cities").unwrap();
    let inputs = One(ds);
    let r = rustgis_analysis::run("plugin:unit-tools/m", &inputs, &json!({"input": "Cities"})).unwrap();
    assert_eq!(r.messages, vec!["made"]);
    match r.output {
        rustgis_analysis::Output::Layers(v) => {
            assert!(matches!(v.as_slice(), [rustgis_analysis::OutputLayer::Vector(d)] if d.name == "Made" && d.features.len() == 1))
        }
        o => panic!("{o:?}"),
    }
    let r = rustgis_analysis::run("plugin:unit-tools/x", &inputs, &json!({"input": "Cities"})).unwrap();
    assert!(matches!(r.output, rustgis_analysis::Output::Selection { ref layer, ref rows } if layer == "Cities" && rows == &[0, 2]));
    assert!(rustgis_analysis::run("plugin:unit-tools/m", &inputs, &json!({"input": "Nope"})).unwrap_err().contains("no layer"));
    // Disabled plugins offer no tools.
    crate::set_enabled("unit-tools", false).unwrap();
    assert!(rustgis_analysis::tool("plugin:unit-tools/m").is_none());
    crate::unload("unit-tools");
}

/// Old prepared tool jobs cannot run after their plugin is toggled, replaced or removed.
#[test]
fn prepared_tools_keep_their_load_and_refuse_revoked_handles() {
    struct Empty;
    impl rustgis_analysis::Inputs for Empty {
        /// The fixed-output plugin needs no project dataset.
        fn dataset(&self, _: &str) -> Option<&rustgis_data::Dataset> {
            None
        }
        /// The fixture gives the plugin no selected rows.
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
    }

    let plugin = load("unit-pinned-tool", &[]);
    let job = crate::tools::prepare_pinned(&plugin, "m", &Empty, &json!({})).unwrap();
    assert!(crate::is_current(&plugin));
    assert_eq!(job.run(&crate::tools::Control::default()).unwrap().messages, ["made"]);

    crate::set_enabled("unit-pinned-tool", false).unwrap();
    assert!(!crate::is_current(&plugin));
    assert!(job.run(&crate::tools::Control::default()).unwrap_err().contains("changed or was disabled"));
    crate::set_enabled("unit-pinned-tool", true).unwrap();
    let enabled = crate::get("unit-pinned-tool").unwrap();
    assert!(crate::is_current(&enabled));
    assert!(!crate::is_current(&plugin), "enabling cannot revive an old load");
    let job = crate::tools::prepare_pinned(&enabled, "m", &Empty, &json!({})).unwrap();

    let replacement = load("unit-pinned-tool", &[]);
    assert!(crate::is_current(&replacement));
    assert!(!crate::is_current(&enabled), "identical package bytes still make a new load");
    assert!(job.run(&crate::tools::Control::default()).is_err());
    let err = crate::tools::prepare_pinned(&enabled, "m", &Empty, &json!({"input": "Missing"})).err().unwrap();
    assert!(err.contains("updated or removed"), "refuse before reading inputs: {err}");
    let job = crate::tools::prepare_pinned(&replacement, "m", &Empty, &json!({})).unwrap();
    crate::unload("unit-pinned-tool");
    assert!(!crate::is_current(&replacement));
    assert!(job.run(&crate::tools::Control::default()).is_err());

    let withdrawn = load("unit-pinned-withdrawn", &[]);
    let job = crate::tools::prepare_pinned(&withdrawn, "m", &Empty, &json!({})).unwrap();
    crate::apply_blocklist(
        "https://pinned-test.invalid/registry.json",
        vec![registry::Blocked { id: "unit-pinned-withdrawn".into(), sha256: None, reason: "test withdrawal".into() }],
    );
    assert!(!crate::is_current(&withdrawn));
    assert!(job.run(&crate::tools::Control::default()).is_err());
}

/// A schema captured from one registry snapshot cannot be paired with another plugin load.
#[test]
fn pinned_tool_schema_refuses_changed_defaults_display_and_stale_load() {
    // This schema-comparison scenario holds its own publication; other fixtures reload the
    // process-wide provider, which may legitimately make a concurrent lookup unavailable.
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var_os("RUSTGIS_SCHEMA_PIN_TEST_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::pinned_tool_schema_refuses_changed_defaults_display_and_stale_load", "--nocapture"])
            .env("RUSTGIS_SCHEMA_PIN_TEST_CHILD", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stdout).contains("test result: ok. 1 passed; 0 failed;"));
        return;
    }
    crate::tools::register();
    let first = load("unit-schema-pin", &[]);
    let spec = rustgis_analysis::resolve("plugin:unit-schema-pin/m").unwrap();
    crate::tools::matches_pinned(&first, spec.spec()).unwrap();
    let mut manifest: Value = serde_json::from_str(&testing::manifest("unit-schema-pin", &[])).unwrap();
    manifest["tools"][0]["name"] = json!("Different operation");
    manifest["tools"][0]["params"][1]["default"] = json!(42);
    let text = manifest.to_string();
    let bytes = testing::zip_of(&[("plugin.json", text.as_bytes()), ("plugin.wasm", &testing::wasm())]);
    let second = crate::load(Package::from_zip(&bytes).unwrap(), "test").unwrap();
    assert!(crate::tools::matches_pinned(&first, spec.spec()).unwrap_err().contains("updated or removed"));
    assert!(crate::tools::matches_pinned(&second, spec.spec()).unwrap_err().contains("schema or display"));
    let current = rustgis_analysis::resolve("plugin:unit-schema-pin/m").unwrap();
    crate::tools::matches_pinned(&second, current.spec()).unwrap();
    assert_eq!(current.resolve_params(&json!({"input":"Cities"})).unwrap()["size"], 42);
    crate::set_enabled("unit-schema-pin", false).unwrap();
    assert!(crate::tools::matches_pinned(&second, current.spec()).is_err());
    crate::unload("unit-schema-pin");
}

/// Registry and blocklist parsing validates entries and resolves package addresses safely.
#[test]
fn registries_and_blocklists_are_read_defensively() {
    let sha = "a".repeat(64);
    let text = json!({"version": 1, "plugins": [
        {"id": "b-plugin", "name": "B", "description": "Long", "version": "1.2.0", "package": "packages/b/b-1.2.0.rgplugin", "sha256": sha,
         "i18n": {"de": {"B": "Be", "Other": "Andere", "Long": "x".repeat(5000)}, "fr": {"Other": "Autre"}}},
        {"id": "a-plugin", "name": "A", "version": "0.1.0", "package": "https://example.com/a.rgplugin", "sha256": sha, "homepage": "javascript:alert(1)"},
        {"id": "bad", "name": "Bad", "version": "1.0.0", "package": "http://insecure.example.com/x.rgplugin", "sha256": sha},
        {"id": "nohash", "name": "No hash", "version": "1.0.0", "package": "x.rgplugin", "sha256": "abc"},
        {"id": "Bad Id", "name": "X", "version": "1.0.0", "package": "x.rgplugin", "sha256": sha},
        7
    ]})
    .to_string();
    let v = registry::parse(&text, "https://plugins.rustgis.app/registry.json").unwrap();
    assert_eq!(v.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a-plugin", "b-plugin"]);
    assert_eq!(v[1].package, "https://plugins.rustgis.app/packages/b/b-1.2.0.rgplugin");
    assert!(v[0].homepage.is_empty(), "only http(s) homepages");
    // Only the name and description keep translations.
    assert_eq!(v[1].translate("de", "B"), "Be");
    assert_eq!(v[1].i18n.get("de").map(|t| t.len()), Some(1));
    assert!(!v[1].i18n.contains_key("fr"));
    // Languages with nothing to keep don't use up the 64 allowed: `de` sorts after 70 of them.
    let mut i18n = serde_json::Map::new();
    for i in 0..70 {
        i18n.insert(format!("a{i:02}"), json!({"Other": "x"}));
    }
    i18n.insert("de".into(), json!({"C": "Ce"}));
    let one = json!([{"id": "c-plugin", "name": "C", "version": "1.0.0", "package": "c.rgplugin", "sha256": sha, "i18n": i18n}]);
    let v = registry::parse(&one.to_string(), "https://plugins.rustgis.app/registry.json").unwrap();
    assert_eq!(v[0].translate("de", "C"), "Ce");
    assert_eq!(v[0].i18n.len(), 1);
    assert!(registry::parse("[]", "x").unwrap().is_empty());
    assert!(registry::parse("{", "x").is_err());
    assert_eq!(registry::resolve("http://localhost:8000/r/registry.json", "/p.rgplugin"), "http://localhost:8000/p.rgplugin");
    for (url, ok) in [
        ("https://plugins.rustgis.app/x", true),
        ("http://localhost:8000/registry.json", true),
        ("http://127.0.0.1/registry.json", true),
        ("http://localhost", true),
        ("http://localhost.evil.com/x", false),
        ("http://127.0.0.1.evil.com/x", false),
        ("http://localhost:80@evil.example/pkg.rgplugin", false),
        ("http://127.0.0.1:x@evil.example/", false),
        ("http://localhost:8000/a@b.rgplugin", true),
        ("http://example.com/x", false),
        ("file:///etc/passwd", false),
    ] {
        assert_eq!(registry::allowed_url(url), ok, "{url}");
    }
    assert_eq!(registry::blocklist_url("https://plugins.rustgis.app/registry.json"), "https://plugins.rustgis.app/blocklist.json");
    let list =
        registry::parse_blocklist(&json!({"blocked": [{"id": "x", "reason": "bad"}, {"id": "y", "sha256": sha}]}).to_string()).unwrap();
    assert!(registry::blocked(&list, "x", "anything").is_some());
    assert!(registry::blocked(&list, "y", &sha).is_some());
    assert!(registry::blocked(&list, "y", &"b".repeat(64)).is_none());
}

/// Blocklisting unloads affected plugins and prevents them from being loaded again.
#[test]
fn blocked_plugins_are_unloaded_and_refused() {
    load("unit-blocked", &[]);
    let pulled = crate::set_blocklist(vec![registry::Blocked { id: "unit-blocked".into(), sha256: None, reason: "malware".into() }]);
    assert_eq!(pulled, vec!["Test unit-blocked"]);
    assert!(crate::get("unit-blocked").is_none());
    let pkg = Package::from_zip(&testing::package("unit-blocked", &[])).unwrap();
    assert!(crate::load(pkg, "t").unwrap_err().contains("malware"));
    crate::set_blocklist(Vec::new());
    // Another registry's blocklist only adds: it can't bring back what the hosted one withdrew.
    load("unit-blocked", &[]);
    let pulled = crate::apply_blocklist(
        "https://example.com/registry.json",
        vec![registry::Blocked { id: "unit-blocked".into(), sha256: None, reason: "x".into() }],
    );
    assert_eq!(pulled, vec!["Test unit-blocked"]);
    crate::apply_blocklist("https://example.com/registry.json", Vec::new());
    assert!(crate::blocked("unit-blocked", "any").is_some(), "an empty list from another registry unblocks nothing");
}

/// Version comparisons and translation fallbacks preserve the plugin's declared behavior.
#[test]
fn versions_and_translations() {
    assert!(version_cmp("1.10.0", "1.9.3").is_gt());
    assert!(version_cmp("v2.0", "2.0.0").is_eq());
    assert!(version_cmp("1.0.0-beta.1", "1.0.0").is_eq());
    let m = Manifest::parse(&testing::manifest("tr", &[])).unwrap();
    assert_eq!(m.translate("de-AT", "Hello"), "Hallo");
    assert_eq!(m.translate("fr", "Hello"), "Hello");
    // The interface's `zh` finds a plugin's `zh-CN`; an exact match wins over another region.
    let mut m = m;
    m.i18n.insert("zh-TW".into(), [("Hello".to_string(), "哈囉".to_string())].into());
    m.i18n.insert("zh-CN".into(), [("Hello".to_string(), "你好".to_string())].into());
    assert_eq!(m.translate("zh", "Hello"), "你好");
    assert_eq!(m.translate("zh-TW", "Hello"), "哈囉");
    // Loaded plugins translate for the interface (tool names, parameters), by language.
    load("unit-i18n", &[]);
    assert_eq!(crate::translate("de", "Hello").as_deref(), Some("Hallo"));
    assert_eq!(crate::translate("fr", "Hello"), None);
    assert_eq!(crate::translate("de", "Not a plugin string"), None);
}

fn parse_tab(tab: Value) -> Result<Manifest, String> {
    let mut m: Value = serde_json::from_str(&testing::manifest("tabbed", &[])).unwrap();
    m["tab"] = tab;
    Manifest::parse(&m.to_string())
}

fn nested_menu(mut item: Value, levels: usize) -> Value {
    for _ in 0..levels {
        item = json!({"menu": "Nested", "items": [item]});
    }
    item
}

/// Ribbon tabs accept declared entries and enforce reserved names, shapes and size limits.
#[test]
fn plugin_tabs_are_checked() {
    let with_tab = parse_tab;
    let ok = with_tab(json!({"label": "Hello", "groups": [
        {"label": "Run", "items": ["h", "tool:m", {"menu": "More", "icon": "stats", "items": ["f", "tool:x"]}]}
    ]}))
    .unwrap();
    let tab = ok.tab.as_ref().unwrap();
    assert!(matches!(&tab.groups[0].items[2], crate::ItemDecl::Menu(m) if m.items.len() == 2));
    assert_eq!(tab.groups[0].layout, crate::Layout::Large);
    assert!(matches!(&tab.groups[0].items[2], crate::ItemDecl::Menu(m) if m.layout.is_none()));
    assert!(matches!(ok.entry("tool:m"), Some(crate::Entry::Tool(t)) if t.id == "m"));
    assert!(matches!(ok.entry("h"), Some(crate::Entry::Command(c)) if c.id == "h"));
    for (bad, why) in [
        (json!({"label": "Map", "groups": [{"label": "G", "items": ["h"]}]}), "RustGIS's tabs"),
        (json!({"label": "help", "groups": [{"label": "G", "items": ["h"]}]}), "RustGIS's tabs"),
        (json!({"label": "Style", "groups": [{"label": "G", "items": ["h"]}]}), "RustGIS's tabs"),
        (json!({"label": "publish", "groups": [{"label": "G", "items": ["h"]}]}), "RustGIS's tabs"),
        (json!({"label": "", "groups": [{"label": "G", "items": ["h"]}]}), "label"),
        (json!({"label": "T", "groups": []}), "groups"),
        (json!({"label": "T", "groups": vec![json!({"label": "G", "items": ["h"]}); 9]}), "1 to 8 groups"),
        (json!({"label": "T", "groups": [{"label": "G", "items": []}]}), "items"),
        (json!({"label": "T", "groups": [{"label": "G", "items": vec!["h"; 17]}]}), "1 to 16 items"),
        (json!({"label": "T", "groups": [{"label": "G", "items": ["nope"]}]}), "isn't one of"),
        (json!({"label": "T", "groups": [{"label": "G", "items": ["tool:h"]}]}), "isn't one of"),
        (json!({"label": "T", "groups": [{"label": "G", "items": [{"menu": "M", "items": []}]}]}), "needs 1 to"),
        (json!({"label": "T", "groups": [{"label": "G", "items": [{"menu": "M", "items": vec!["h"; 33]}]}]}), "1 to 32 items"),
        (json!({"label": "T", "groups": [{"label": "G", "items": [{"menu": "M", "items": ["zz"]}]}]}), "isn't one of"),
        (json!({"label": "T", "groups": [{"label": "G", "items": [7]}]}), "plugin.json"),
    ] {
        let e = with_tab(bad.clone()).unwrap_err();
        assert!(e.contains(why), "{bad}: {e}");
    }
}

#[test]
fn plugin_tab_layout_defaults_and_overrides() {
    use crate::{ItemDecl, Layout};

    let m = parse_tab(json!({"label": "Layouts", "groups": [
        {"label": "Default", "items": [{"menu": "Inherit", "items": ["h"]}]},
        {"label": "Compact", "layout": "small", "items": [
            "h",
            {"menu": "Inherit", "items": ["tool:m"]},
            {"menu": "Large", "layout": "large", "items": ["f"]},
            {"menu": "Small", "layout": "small", "items": ["h"]}
        ]},
        {"label": "Large", "layout": "large", "items": [
            {"menu": "Small", "layout": "small", "items": ["h"]},
            {"menu": "Inherit", "layout": null, "items": ["f"]}
        ]}
    ]}))
    .unwrap();
    let groups = &m.tab.as_ref().unwrap().groups;
    assert_eq!(groups[0].layout, Layout::Large);
    assert_eq!(groups[1].layout, Layout::Small);
    assert_eq!(groups[2].layout, Layout::Large);
    for (group, item, declared) in
        [(0, 0, None), (1, 1, None), (1, 2, Some(Layout::Large)), (1, 3, Some(Layout::Small)), (2, 0, Some(Layout::Small)), (2, 1, None)]
    {
        let ItemDecl::Menu(menu) = &groups[group].items[item] else { panic!("expected a menu") };
        assert_eq!(menu.layout, declared);
    }
    assert_eq!(Manifest::parse(&serde_json::to_string(&m).unwrap()).unwrap(), m);

    for bad in [json!("medium"), json!("Small"), json!(7), Value::Null] {
        let tab = json!({"label": "Layouts", "groups": [{"label": "G", "layout": bad, "items": ["h"]}]});
        assert!(parse_tab(tab).is_err(), "invalid group layout: {bad}");
    }
    for bad in [json!("medium"), json!("Small"), json!(7)] {
        for levels in [0, 3] {
            let menu = nested_menu(json!({"menu": "Bad", "layout": bad, "items": ["h"]}), levels);
            let tab = json!({"label": "Layouts", "groups": [{"label": "G", "items": [menu]}]});
            assert!(parse_tab(tab).is_err(), "invalid menu layout: {bad}");
        }
    }
}

#[test]
fn plugin_tab_group_layout_limits() {
    for (layout, max) in [("large", 16), ("small", 48)] {
        for count in [1, max] {
            let group = json!({"label": "G", "layout": layout, "items": vec!["h"; count]});
            assert!(parse_tab(json!({"label": "Limits", "groups": vec![group; 8]})).is_ok());
        }
        for count in [0, max + 1] {
            let tab = json!({"label": "Limits", "groups": [{"label": "G", "layout": layout, "items": vec!["h"; count]}]});
            let err = parse_tab(tab).unwrap_err();
            assert!(err.contains(&format!("1 to {max} items")), "{err}");
        }
    }
}

#[test]
fn plugin_tab_nested_menus_are_checked() {
    let tab = |item: Value| json!({"label": "Nested", "groups": [{"label": "G", "items": [item]}]});
    for entry in ["h", "tool:m"] {
        parse_tab(tab(nested_menu(json!(entry), 4))).unwrap();
        let err = parse_tab(tab(nested_menu(json!(entry), 5))).unwrap_err();
        assert!(err.contains("at most 4 levels"), "{err}");
    }
    for entry in ["unknown", "tool:unknown", "tool:h", "panel"] {
        let err = parse_tab(tab(nested_menu(json!(entry), 4))).unwrap_err();
        assert!(err.contains("isn't one of"), "{entry}: {err}");
    }

    // A panel reference is resolved at the deepest level just like a command or tool.
    let mut paneled: Value = serde_json::from_str(&testing::panel_manifest("nested-panel")).unwrap();
    paneled["tab"] = tab(nested_menu(json!("panel"), 4));
    assert!(Manifest::parse(&paneled.to_string()).is_ok());
    paneled["panel"] = Value::Null;
    assert!(Manifest::parse(&paneled.to_string()).unwrap_err().contains("isn't one of"));

    let long = "x".repeat(2001);
    for (menu, why) in [
        (json!({"menu": "", "items": ["h"]}), "menu label"),
        (json!({"menu": long, "items": ["h"]}), "menu label"),
        (json!({"menu": "M", "help": long, "items": ["h"]}), "menu help"),
        (json!({"menu": "M", "icon": long, "items": ["h"]}), "menu icon"),
        (json!({"menu": "M", "items": []}), "1 to 32 items"),
        (json!({"menu": "M", "items": vec!["h"; 33]}), "1 to 32 items"),
    ] {
        let err = parse_tab(tab(nested_menu(menu, 3))).unwrap_err();
        assert!(err.contains(why), "{err}");
    }
    let full = json!({"menu": "Full", "layout": "small", "items": vec!["h"; 32]});
    parse_tab(tab(nested_menu(full, 3))).unwrap();
}

#[test]
fn plugin_tab_total_item_limit_counts_menus_and_entries_across_groups() {
    // Preserve the full capacity of existing flat manifests and compact groups.
    for (layout, count) in [("large", 16), ("small", 48)] {
        let menu = json!({"menu": "M", "items": vec!["h"; 32]});
        let group = json!({"label": "G", "layout": layout, "items": vec![menu; count]});
        assert!(parse_tab(json!({"label": "Total", "groups": vec![group; 8]})).is_ok());
    }

    // Each subtree has 1 root + 28 entries + 3 × (1 submenu + 32 entries) = 128 items.
    // 8 groups × 16 subtrees × 128 items = 16,384 items.
    let mut children = vec![json!("h"); 28];
    children.extend(vec![json!({"menu": "Child", "items": vec!["h"; 32]}); 3]);
    let menu = json!({"menu": "M", "items": children});
    let group = json!({"label": "G", "items": vec![menu; 16]});
    let mut tab = json!({"label": "Total", "groups": vec![group; 8]});
    parse_tab(tab.clone()).unwrap();
    tab["groups"][0]["items"][0]["items"].as_array_mut().unwrap().push(json!("h"));
    let err = parse_tab(tab).unwrap_err();
    assert!(err.contains("more than 16384 total items"), "{err}");

    // A single group can exhaust the same tab-wide budget through nested menus.
    let branch = json!({"menu": "Branch", "items": vec![json!({"menu": "Leaf", "items": vec!["h"; 32]}); 32]});
    let tab = json!({"label": "Total", "groups": [{"label": "G", "items": vec![branch; 16]}]});
    let err = parse_tab(tab).unwrap_err();
    assert!(err.contains("more than 16384 total items"), "{err}");
}

#[test]
fn plugin_panels_are_checked() {
    let ok = Manifest::parse(&testing::panel_manifest("paneled")).unwrap();
    let panel = ok.panel.as_ref().unwrap();
    assert_eq!(panel.items.len(), 7);
    // A button's id is its command's unless it has one; a tab's `panel` opens the panel.
    assert!(panel.item("q-newer").is_none() && panel.item("newer").is_some_and(|i| i.command == "q-newer"));
    assert!(matches!(ok.entry("panel"), Some(crate::Entry::Panel(_))));
    let mut tabbed: Value = serde_json::from_str(&testing::panel_manifest("paneled")).unwrap();
    tabbed["tab"] = json!({"label": "Wayback", "groups": [{"label": "G", "items": ["panel", {"menu": "M", "items": ["panel", "h"]}]}]});
    assert!(Manifest::parse(&tabbed.to_string()).is_ok());
    // Without a panel, `panel` on a tab is a command of that name, as before.
    let mut plain: Value = serde_json::from_str(&testing::manifest("plain", &[])).unwrap();
    plain["tab"] = json!({"label": "Plain", "groups": [{"label": "G", "items": ["panel"]}]});
    assert!(Manifest::parse(&plain.to_string()).unwrap_err().contains("isn't one of"));
    plain["commands"].as_array_mut().unwrap().push(json!({"id": "panel", "label": "Panel"}));
    assert!(Manifest::parse(&plain.to_string()).is_ok());
    let with_panel = |panel: Value| {
        let mut m: Value = serde_json::from_str(&testing::manifest("paneled", &[])).unwrap();
        m["panel"] = panel;
        Manifest::parse(&m.to_string())
    };
    let item = |i: Value| json!({"title": "P", "items": [i]});
    let long = "x".repeat(3000);
    for (bad, why) in [
        (json!({"title": "", "items": [{"kind": "button", "command": "h"}]}), "title"),
        (json!({"title": "Two\nlines", "items": [{"kind": "button", "command": "h"}]}), "title"),
        (json!({"title": "x".repeat(49), "items": [{"kind": "button", "command": "h"}]}), "title"),
        (json!({"title": "P", "items": []}), "1 to 32 items"),
        (
            json!({"title": "P", "items": (0..33).map(|i| json!({"kind": "text", "id": format!("t{i}")})).collect::<Vec<_>>()}),
            "1 to 32 items",
        ),
        (json!({"title": "P", "state": "nope", "items": [{"kind": "button", "command": "h"}]}), "state command"),
        (item(json!({"kind": "dial", "id": "d"})), "isn't text, checkbox"),
        (item(json!({"kind": "text"})), "invalid or repeated"),
        (item(json!({"kind": "text", "id": "Bad Id"})), "invalid or repeated"),
        (json!({"title": "P", "items": [{"kind": "text", "id": "t"}, {"kind": "text", "id": "t"}]}), "invalid or repeated"),
        (json!({"title": "P", "items": [{"kind": "button", "command": "h"}, {"kind": "button", "command": "h"}]}), "invalid or repeated"),
        (item(json!({"kind": "button"})), "invalid or repeated"),
        (item(json!({"kind": "button", "id": "b"})), "needs a command"),
        (item(json!({"kind": "button", "command": "zz"})), "isn't one of the plugin's commands"),
        (item(json!({"kind": "button", "command": "m"})), "isn't one of the plugin's commands"),
        (item(json!({"kind": "text", "id": "t", "command": "h"})), "runs no command"),
        (item(json!({"kind": "checkbox", "id": "c"})), "needs a label"),
        (item(json!({"kind": "checkbox", "id": "c", "label": "C", "default": 1})), "true or false"),
        (item(json!({"kind": "slider", "id": "s", "min": 5, "max": 1})), "min must not be above"),
        (item(json!({"kind": "slider", "id": "s", "min": 0, "max": 2_000_000_000i64})), "within"),
        (item(json!({"kind": "slider", "id": "s", "max": 3, "default": 9})), "from min to max"),
        (item(json!({"kind": "slider", "id": "s", "max": 3, "default": "1"})), "from min to max"),
        (item(json!({"kind": "slider", "id": "s", "max": 1.5})), "plugin.json"),
        (item(json!({"kind": "button", "command": "h", "max": 3})), "for sliders"),
        (item(json!({"kind": "checkbox", "id": "c", "label": "C", "steps": true})), "for sliders"),
        (item(json!({"kind": "button", "command": "h", "default": true})), "only checkboxes and sliders"),
        (item(json!({"kind": "button", "command": "h", "text": "t"})), "for texts"),
        (item(json!({"kind": "text", "id": "t", "text": long})), "too long"),
        (item(json!({"kind": "text", "id": "t", "label": 7})), "plugin.json"),
        (json!({"items": [{"kind": "button", "command": "h"}]}), "plugin.json"),
        (json!("panel"), "plugin.json"),
    ] {
        let e = with_panel(bad.clone()).unwrap_err();
        assert!(e.contains(why), "{bad}: {e}");
    }
    // A command called `panel` beside a panel would be ambiguous on a tab.
    let mut m: Value = serde_json::from_str(&testing::panel_manifest("paneled")).unwrap();
    m["commands"].as_array_mut().unwrap().push(json!({"id": "panel", "label": "Panel"}));
    assert!(Manifest::parse(&m.to_string()).unwrap_err().contains("called 'panel'"));
    // Skills packages have none.
    let skills = json!({"id": "sk", "name": "S", "version": "1.0.0", "runtime": "skills", "skills": ["a"],
        "panel": {"title": "P", "items": [{"kind": "text", "id": "t"}]}});
    assert!(Manifest::parse(&skills.to_string()).unwrap_err().contains("panel"));
}

#[test]
fn plugin_panel_state_round_trips() {
    use crate::PanelState;
    let pkg = Package::from_zip(&testing::panel_package("unit-panel")).unwrap();
    let p = crate::load(pkg, "test").unwrap();
    let decl = p.manifest.panel.clone().unwrap();
    let mut st = PanelState::new(&decl);
    assert_eq!(st.values, [("local".to_string(), json!(false)), ("release".to_string(), json!(0))].into_iter().collect());
    assert_eq!(st.items["selected"].text, "None yet");
    // The state command answers the panel's state; the host takes it in.
    let cmd = crate::command("plugin:unit-panel/p").unwrap();
    let answer = crate::run_command_with_panel(&cmd, &mut Recorder::default(), &json!({}), Some(st.request(&[]))).unwrap();
    st.apply(&decl, &answer["panel"]);
    assert_eq!(st.slider("release"), 12);
    assert_eq!((st.items["release"].min, st.items["release"].max), (0, 12));
    assert_eq!(st.slider_label("release", 0), "2014-02-20");
    assert_eq!(st.slider_label("release", 12), "2025-12-18");
    assert_eq!(st.slider_label("release", 99), "99", "past its labels, the number");
    assert!(!st.items["newer"].enabled && st.items["older"].enabled);
    assert!(st.items["metadata"].text.contains("Maxar"));
    // The values go with every request: the echo command shows what the plugin got.
    let changed = st.set_values(&decl, &json!({"release": 3, "local": true, "selected": "x", "nope": 1}));
    assert_eq!(changed, ["local", "release"]);
    let echo = crate::command("plugin:unit-panel/q").unwrap();
    let got = crate::run_command_with_panel(&echo, &mut Recorder::default(), &json!({"a": 1}), Some(st.request(&changed))).unwrap();
    assert_eq!(got["panel"], json!({"values": {"local": true, "release": 3}, "changed": ["local", "release"]}));
    assert_eq!(got["params"], json!({"a": 1}));
    assert_eq!(got["kind"], "command");
    // Without a panel, no `panel` in the request.
    let got = crate::run_command(&echo, &mut Recorder::default(), &json!({})).unwrap();
    assert!(got.get("panel").is_none());
    crate::unload("unit-panel");
}

#[test]
fn panel_texts_are_limited_in_characters() {
    // Two bytes a character: within the limit in characters though past it in bytes.
    let with_help = |n: usize| {
        let mut m: Value = serde_json::from_str(&testing::panel_manifest("chars-panel")).unwrap();
        m["panel"]["help"] = json!("é".repeat(n));
        Manifest::parse(&m.to_string())
    };
    assert!(with_help(crate::panel::MAX_DECLARED).is_ok());
    assert!(with_help(crate::panel::MAX_DECLARED + 1).unwrap_err().contains("too long"));
}

#[test]
fn a_panel_state_matches_only_its_declaration() {
    use crate::PanelState;
    let m = Manifest::parse(&testing::panel_manifest("match-panel")).unwrap();
    let decl = m.panel.unwrap();
    let state = PanelState::new(&decl);
    assert!(state.matches(&decl));
    // The plugin updated: a widget fewer, or a checkbox become a slider of the same id.
    let mut fewer = decl.clone();
    fewer.items.pop();
    assert!(!state.matches(&fewer));
    let mut retyped = decl.clone();
    if let Some(i) = retyped.items.iter_mut().find(|i| i.kind == "checkbox") {
        i.kind = "slider".into();
    }
    assert!(!state.matches(&retyped));
    // A slider's declared range changed: matched again only by a state made from it.
    let mut ranged = decl.clone();
    if let Some(i) = ranged.items.iter_mut().find(|i| i.kind == "slider") {
        i.max += 10;
    }
    assert!(!state.matches(&ranged));
    assert!(PanelState::new(&ranged).matches(&ranged));
}

#[test]
fn hostile_panel_states_are_tamed() {
    use crate::PanelState;
    let m = Manifest::parse(&testing::panel_manifest("hostile-panel")).unwrap();
    let decl = m.panel.unwrap();
    let fresh = PanelState::new(&decl);
    let huge = "y".repeat(100_000);
    // Past both slider label limits (count and length), without holding a gigabyte of text.
    let long_label = "y".repeat(crate::panel::MAX_SLIDER_LABEL * 2);
    let many: Vec<Value> = (0..crate::panel::MAX_SLIDER_LABELS * 2).map(|i| json!(format!("{i}{long_label}"))).collect();
    for junk in [
        Value::Null,
        json!(7),
        json!("panel"),
        json!([1, 2]),
        json!({"values": 5, "items": "x"}),
        json!({"values": {"release": "9", "local": 1, "selected": true, "metadata": {}}}),
        json!({"values": {"release": 1e300, "local": null}}),
        json!({"values": {"release": -1e18}}),
        json!({"values": {"release": 2.5}}),
        json!({"items": {"release": {"min": 10, "max": 2}}}),
        json!({"items": {"release": {"min": -5e18, "max": 5e18}}}),
        json!({"items": {"release": {"min": "0", "max": [1], "labels": "a"}}}),
        json!({"items": {"release": {"labels": many}}}),
        json!({"items": {"selected": {"text": huge, "label": huge, "enabled": "no", "visible": 0}}}),
        json!({"items": {"selected": {"text": 5, "label": {"x": 1}}, "older": {"text": "a button has none", "min": 3}}}),
        json!({"items": {"nope": {"text": "x"}, "": {}, "local": {"label": null}}}),
    ] {
        let mut st = fresh.clone();
        st.apply(&decl, &junk);
        let _ = st.set_values(&decl, &junk);
        let r = &st.items["release"];
        assert!(r.min <= r.max && (r.min..=r.max).contains(&st.slider("release")), "{junk}");
        assert!(st.items["selected"].text.chars().count() <= crate::panel::MAX_STATE_TEXT);
        assert!(st.items["selected"].label.as_ref().is_none_or(|l| l.chars().count() <= crate::panel::MAX_STATE_LABEL));
        assert!(
            r.labels.len() <= crate::panel::MAX_SLIDER_LABELS
                && r.labels.iter().all(|l| l.chars().count() <= crate::panel::MAX_SLIDER_LABEL)
        );
        assert!(st.items["older"].text.is_empty(), "a button has no text");
        assert!(!st.values.contains_key("selected"), "a text has no value");
        assert!(st.values["local"].is_boolean() && st.values["release"].is_i64());
        let _ = st.slider_label("release", i64::MIN);
        let _ = st.slider_label("release", i64::MAX);
        let _ = st.slider_label("nope", 3);
    }
    // A narrower range keeps the value in it.
    let mut st = fresh.clone();
    st.apply(&decl, &json!({"items": {"release": {"min": 0, "max": 100}}, "values": {"release": 80}}));
    assert_eq!(st.slider("release"), 80);
    st.apply(&decl, &json!({"items": {"release": {"max": 10}}}));
    assert_eq!(st.slider("release"), 10);
    st.apply(&decl, &json!({"values": {"release": -4}}));
    assert_eq!(st.slider("release"), 0);
    // `label: null` brings back the declared label.
    st.apply(&decl, &json!({"items": {"local": {"label": "Mine"}}}));
    assert_eq!(st.items["local"].label.as_deref(), Some("Mine"));
    st.apply(&decl, &json!({"items": {"local": {"label": null}}}));
    assert_eq!(st.items["local"].label, None);
}

/// The test panel with a catalogue tree added.
fn tree_panel() -> crate::PanelDecl {
    let mut m: Value = serde_json::from_str(&testing::panel_manifest("tree-panel")).unwrap();
    let tree = json!({"kind": "tree", "id": "catalogue", "label": "Catalogue", "command": "q", "search": true});
    m["panel"]["items"].as_array_mut().unwrap().push(tree);
    Manifest::parse(&m.to_string()).unwrap().panel.unwrap()
}

/// A tree takes its nodes from the plugin, and the chosen node's id as its value: one of its
/// nodes, or none (#1050).
#[test]
fn a_panel_tree_takes_nodes_and_a_chosen_one() {
    use crate::PanelState;
    let decl = tree_panel();
    let mut st = PanelState::new(&decl);
    assert_eq!(st.values["catalogue"], Value::Null);
    assert_eq!(st.request(&[])["values"]["catalogue"], Value::Null, "none chosen goes as null");
    let nodes = json!([
        {"id": "env", "label": "Ambiente", "count": 2, "children": [{"id": "501", "label": "Árboles"}, {"id": "502"}]},
        {"id": "farm", "label": "Agricultura", "count": 1, "children": [{"id": "601", "label": "Productividad Rural"}]},
    ]);
    st.apply(&decl, &json!({"items": {"catalogue": {"nodes": nodes}}, "values": {"catalogue": "601"}}));
    let tree = &st.items["catalogue"].nodes;
    assert_eq!(tree.len(), 2);
    assert_eq!((tree[0].count, tree[0].children[1].label.as_str()), (Some(2), "502"), "a label defaults to the id");
    assert_eq!(st.chosen("catalogue"), Some("601"), "the nodes first, then the value among them");
    // The pane or a script chooses another by id; one not in the tree, or not a string, is ignored.
    assert_eq!(st.set_values(&decl, &json!({"catalogue": "501"})), ["catalogue"]);
    assert_eq!(st.chosen("catalogue"), Some("501"));
    assert!(st.set_values(&decl, &json!({"catalogue": "999"})).is_empty());
    assert!(st.set_values(&decl, &json!({"catalogue": 501})).is_empty());
    assert_eq!(st.chosen("catalogue"), Some("501"));
    assert_eq!(st.request(&[])["values"]["catalogue"], "501");
    // Nodes of the wrong type are ignored, as any value of the wrong type is: the tree stays.
    for junk in [json!("oops"), json!({"id": "a"}), Value::Null, json!(5)] {
        let mut kept = st.clone();
        kept.apply(&decl, &json!({"items": {"catalogue": {"nodes": junk}}}));
        assert_eq!((kept.items["catalogue"].nodes.len(), kept.chosen("catalogue")), (2, Some("501")), "{junk}");
    }
    // New nodes without the chosen one: none chosen.
    st.apply(&decl, &json!({"items": {"catalogue": {"nodes": [{"id": "env"}]}}}));
    assert_eq!(st.chosen("catalogue"), None);
    assert_eq!(st.values["catalogue"], Value::Null);
}

/// A tree's nodes are hostile input: past the limits they are cut, a node without an id or with
/// one taken already is left out with its children, and junk is ignored.
#[test]
fn hostile_panel_trees_are_tamed() {
    use crate::PanelState;
    use crate::panel::{MAX_STATE_LABEL, MAX_TREE_DEPTH, MAX_TREE_ID, MAX_TREE_NODES, TreeNode};
    fn count(n: &[TreeNode]) -> usize {
        n.iter().map(|n| 1 + count(&n.children)).sum()
    }
    fn depth(n: &[TreeNode]) -> usize {
        n.iter().map(|n| 1 + depth(&n.children)).max().unwrap_or(0)
    }
    let decl = tree_panel();
    let fresh = PanelState::new(&decl);
    let nodes = |v: Value| {
        let mut st = fresh.clone();
        st.apply(&decl, &json!({"items": {"catalogue": {"nodes": v}}}));
        st.items["catalogue"].nodes.clone()
    };
    // Too many: the first ones kept.
    let many: Vec<Value> = (0..MAX_TREE_NODES * 2).map(|i| json!({"id": i.to_string()})).collect();
    assert_eq!(count(&nodes(json!(many))), MAX_TREE_NODES);
    // Too deep: cut at the limit.
    let mut deep = json!({"id": "leaf"});
    for i in 0..MAX_TREE_DEPTH * 3 {
        deep = json!({"id": format!("n{i}"), "children": [deep]});
    }
    assert_eq!(depth(&nodes(json!([deep]))), MAX_TREE_DEPTH);
    // Ids missing, empty, too long, not text or taken; labels clipped.
    let got = nodes(json!([
        {"label": "no id"}, {"id": ""}, {"id": "x".repeat(MAX_TREE_ID + 1)}, {"id": 7},
        {"id": "a", "label": "y".repeat(MAX_STATE_LABEL * 3), "count": -1},
        {"id": "two-lines", "label": "Ambiente\nAgua\tRíos"},
        {"id": "blank", "label": "  "},
        {"id": "id\nonly"},
        {"id": "b", "children": [{"id": "a"}, {"id": "c", "children": "junk"}]},
    ]));
    assert_eq!(got.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["a", "two-lines", "blank", "id\nonly", "b"]);
    assert_eq!(got[3].label, "id only", "a label from the id is one line too");
    assert_eq!(got[1].label, "Ambiente Agua Ríos", "a label is one line, control characters as spaces");
    assert_eq!(got[2].label, "blank", "a blank label is the id");
    assert_eq!(got[0].label.chars().count(), MAX_STATE_LABEL);
    assert_eq!(got[0].count, None, "a count is a whole number from 0");
    assert_eq!(got[4].children.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["c"], "an id taken already is left out");
    for junk in [Value::Null, json!(5), json!("nodes"), json!({"id": "a"}), json!([1, null, "a", [[]]])] {
        assert!(nodes(junk.clone()).is_empty(), "{junk}");
    }
    // `search` is for trees, and a tree has no default.
    let mut m: Value = serde_json::from_str(&testing::panel_manifest("bad-tree")).unwrap();
    m["panel"]["items"][0]["search"] = json!(true);
    assert!(Manifest::parse(&m.to_string()).unwrap_err().contains("search is for trees"));
    let mut m: Value = serde_json::from_str(&testing::panel_manifest("bad-tree")).unwrap();
    m["panel"]["items"].as_array_mut().unwrap().push(json!({"kind": "tree", "id": "t", "default": "a"}));
    assert!(Manifest::parse(&m.to_string()).unwrap_err().contains("only checkboxes and sliders have a default"));
}

#[test]
fn registry_entries_say_how_a_plugin_runs() {
    use crate::manifest::Runtime;
    let sha = "a".repeat(64);
    let base = |id: &str| json!({"id": id, "name": id, "version": "1.0.0", "package": format!("{id}.rgplugin"), "sha256": sha});
    let mut py = base("py");
    py["runtime"] = json!("process");
    py["python"] = json!("3.12");
    let mut exe = base("exe");
    exe["runtime"] = json!("process");
    let mut skills = base("skills");
    skills["runtime"] = json!("skills");
    // A Python version only means something for a process plugin, and must look like one.
    let mut wasm_py = base("wasm-py");
    wasm_py["python"] = json!("3.12");
    let mut bad_py = base("bad-py");
    bad_py["runtime"] = json!("process");
    bad_py["python"] = json!("latest; rm -rf /");
    // A runtime this version doesn't know is skipped, not offered as WebAssembly.
    let mut future = base("future");
    future["runtime"] = json!("docker");
    let text = json!([base("wasm"), py, exe, skills, wasm_py, bad_py, future]).to_string();
    let v = registry::parse(&text, "https://plugins.rustgis.app/registry.json").unwrap();
    let got: Vec<(&str, Runtime, Option<&str>)> = v.iter().map(|e| (e.id.as_str(), e.runtime, e.python.as_deref())).collect();
    assert_eq!(
        got,
        [
            ("bad-py", Runtime::Process, None),
            ("exe", Runtime::Process, None),
            ("py", Runtime::Process, Some("3.12")),
            ("skills", Runtime::Skills, None),
            ("wasm", Runtime::Wasm, None),
            ("wasm-py", Runtime::Wasm, None),
        ]
    );
    // Round trips: a WebAssembly entry is written as it was (no `runtime`, no `python`); a
    // process one keeps both.
    for e in &v {
        let back: registry::Entry = serde_json::from_value(serde_json::to_value(e).unwrap()).unwrap();
        assert_eq!(&back, e);
    }
    let wasm = serde_json::to_value(v.iter().find(|e| e.id == "wasm").unwrap()).unwrap();
    assert!(wasm.get("runtime").is_none() && wasm.get("python").is_none(), "{wasm}");
    let py = serde_json::to_value(v.iter().find(|e| e.id == "py").unwrap()).unwrap();
    assert_eq!((&py["runtime"], &py["python"]), (&json!("process"), &json!("3.12")));
}

#[test]
fn large_registries_are_read_quickly() {
    // 20,000 plugins (and a duplicate of each, which is dropped) parse in a fraction of a second;
    // the bound is generous so a busy CI machine doesn't fail it, while a quadratic scan
    // (minutes at this size) still would.
    let sha = "a".repeat(64);
    let one = |i: usize| json!({"id": format!("p{i}"), "name": format!("Plugin {i}"), "version": "1.0.0", "package": format!("https://packages.example.com/p{i}.rgplugin"), "sha256": sha});
    let list: Vec<Value> = (0..20_000).chain(0..20_000).map(one).collect();
    let text = json!({"plugins": list}).to_string();
    let t0 = std::time::Instant::now();
    let v = registry::parse(&text, "https://plugins.rustgis.app/registry.json").unwrap();
    assert_eq!(v.len(), 20_000);
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "{:?}", t0.elapsed());
}

#[test]
fn file_parameters_need_files_and_hand_tools_the_file() {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    // Declared only with `files`, and with plain extensions.
    let mut m: Value = serde_json::from_str(&testing::manifest("file-param", &["files"])).unwrap();
    assert!(Manifest::parse(&m.to_string()).is_ok());
    m["permissions"] = json!([]);
    assert!(Manifest::parse(&m.to_string()).unwrap_err().contains("'files' permission"));
    m["permissions"] = json!(["files"]);
    for bad in [json!([".nc"]), json!([""]), json!(["n c"]), json!(vec!["nc"; 17])] {
        m["tools"][2]["params"][1]["extensions"] = bad.clone();
        assert!(Manifest::parse(&m.to_string()).unwrap_err().contains("extensions"), "{bad}");
    }
    let m = Manifest::parse(&testing::manifest("file-param", &["files"])).unwrap();
    let decl = m.tools.iter().find(|t| t.id == "r").unwrap();

    // A path is read; the parameter keeps the file's name and the contents go beside it.
    let dir = std::env::temp_dir().join(format!("rustgis-test-file-param-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("model.nc");
    std::fs::write(&path, b"CDF\x01").unwrap();
    let r = crate::tools::request(decl, &json!({"model": path.display().to_string()}), Default::default()).unwrap();
    assert_eq!(r["params"]["model"], "model.nc");
    assert_eq!(r["files"]["model"]["name"], "model.nc");
    assert_eq!(b64.decode(r["files"]["model"]["data"].as_str().unwrap()).unwrap(), b"CDF\x01");
    // A file picked in the browser comes with its contents, its name stripped of any folder.
    let picked = json!({"model": {"name": "C:\\runs\\pism.nc", "data": b64.encode("abc")}});
    let r = crate::tools::request(decl, &picked, Default::default()).unwrap();
    assert_eq!(r["params"]["model"], "pism.nc");
    assert_eq!(r["files"]["model"]["data"], b64.encode("abc"));
    // No file, no `files`; a missing file, a folder, a non-text value or too large a file is
    // an error, never a crash.
    assert!(crate::tools::request(decl, &json!({}), Default::default()).unwrap().get("files").is_none());
    for bad in [json!(dir.join("missing.nc").display().to_string()), json!(dir.display().to_string()), json!(7), json!({"data": 7})] {
        assert!(crate::tools::request(decl, &json!({"model": bad}), Default::default()).is_err(), "{bad}");
    }
    for bad in ["not base64!", "abc", "a=bc", "===="] {
        assert!(crate::tools::request(decl, &json!({"model": {"name": "m.nc", "data": bad}}), Default::default()).is_err(), "{bad}");
    }
    // The limit is on the decoded size, to the byte: base64 of MAX_FILE bytes passes, and of
    // one more doesn't.
    let max = crate::tools::MAX_FILE;
    let at = |n: usize| json!({"model": {"name": "big.nc", "data": b64.encode(vec![0u8; n])}});
    assert!(crate::tools::request(decl, &at(max), Default::default()).is_ok());
    assert!(crate::tools::request(decl, &at(max + 1), Default::default()).unwrap_err().contains("larger"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// process plugins

/// Held by the tests that set the plugins folder, a global: one at a time.
static STORE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn store_lock() -> std::sync::MutexGuard<'static, ()> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// This test binary, started as the process test plugin, answers one request and exits.
#[test]
fn process_child() {
    testing::serve_if_child();
}

/// A folder holding the process test plugin's manifest (played by this binary).
fn process_folder(id: &str, perms: &[&str]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rustgis-test-process-{id}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = testing::child_run("tests::process_child");
    std::fs::write(dir.join("plugin.json"), testing::process_manifest(id, perms, &run)).unwrap();
    dir
}

#[test]
fn process_plugins_answer_like_modules() {
    let dir = process_folder("proc", &["files"]);
    let pkg = Package::from_dir(&dir).unwrap();
    assert!(pkg.wasm.is_empty() && pkg.dir.as_deref() == Some(dir.as_path()));
    let p = crate::prepare(pkg, "test").unwrap();
    assert_eq!(p.info()["runtime"], "process");
    let request = |id: &str, params: Value| json!({"abi": 1, "kind": "command", "id": id, "params": params, "lang": "en"});

    let mut host = Recorder::default();
    assert_eq!(crate::call(&p, &mut host, &request("h", json!({})), false).unwrap(), json!({"hello": "world"}));
    assert_eq!(host.logs, ["hi"]);
    // Host requests go through the host, and their answers come back.
    let echoed = crate::call(&p, &mut host, &request("f", json!({"op": "host.echo", "params": {"a": [1, 2]}})), false).unwrap();
    assert_eq!(echoed, json!({"a": [1, 2]}));
    assert_eq!(host.ops, ["host.echo"]);
    let e = crate::call(&p, &mut host, &request("f", json!({"op": "nope.op", "params": {}})), false).unwrap_err();
    assert!(e.contains("no 'nope.op'"), "{e}");
    assert_eq!(crate::call(&p, &mut host, &request("e", json!({})), false).unwrap_err(), "nope");
    // Other output is skipped; a crash is an error naming what it wrote last, never a hang.
    assert_eq!(crate::call(&p, &mut host, &request("n", json!({})), false).unwrap(), json!({"n": 1}));
    let e = crate::call(&p, &mut host, &request("c", json!({})), false).unwrap_err();
    assert!(e.contains("exited without answering") && e.contains("boom"), "{e}");
    // Tools run the same way, files included.
    let tool = json!({"abi": 1, "kind": "tool", "id": "r", "params": {"model": "run.nc"}, "files": {"model": {"name": "run.nc", "data": ""}}, "layers": {}, "lang": "en"});
    assert_eq!(crate::call(&p, &mut host, &tool, true).unwrap()["messages"], json!(["run.nc"]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn process_plugins_that_hang_are_stopped() {
    let dir = std::env::temp_dir();
    let spec = crate::process::Spec::new(&testing::child_run("tests::process_child"), &dir).unwrap();
    let t0 = std::time::Instant::now();
    let req = json!({"abi": 1, "kind": "command", "id": "s", "params": {}, "lang": "en"});
    let e = crate::process::call(&spec, &mut Recorder::default(), &req, Some(std::time::Duration::from_secs(2))).unwrap_err();
    assert!(e.contains("too long") && t0.elapsed() < std::time::Duration::from_secs(20), "{e}");
    // What it started is stopped with it: here, a copy of itself that never answers.
    #[cfg(unix)]
    {
        let mut host = Recorder::default();
        let req = json!({"abi": 1, "kind": "command", "id": "g", "params": {}, "lang": "en"});
        assert!(crate::process::call(&spec, &mut host, &req, Some(std::time::Duration::from_secs(3))).is_err());
        let pid = host.logs.first().and_then(|p| p.parse::<i32>().ok()).and_then(rustix::process::Pid::from_raw).unwrap();
        let t0 = std::time::Instant::now();
        while rustix::process::test_kill_process(pid).is_ok() && t0.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(rustix::process::test_kill_process(pid).is_err(), "the grandchild {pid:?} still runs");
    }
    // A plugin folder given by a relative path still finds its program in it.
    let rel = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let spec = crate::process::Spec::new(&["rustgis-no-such-program".to_string()], std::path::Path::new(".")).unwrap();
    assert!(spec.dir.is_absolute() && spec.dir.ends_with(rel.file_name().unwrap()), "{:?}", spec.dir);
    // A program that can't be started is an error too.
    let missing = crate::process::Spec::new(&["rustgis-no-such-program".to_string()], &dir).unwrap();
    assert!(crate::process::call(&missing, &mut Recorder::default(), &req, None).unwrap_err().contains("couldn't start"));
}

#[test]
fn process_tools_show_progress_and_can_be_cancelled() {
    let dir = process_folder("proc-cancel", &[]);
    let p = crate::load(Package::from_dir(&dir).unwrap(), "test").unwrap();
    let job = crate::tools::Job {
        plugin: p.clone(),
        name: "Work Forever".into(),
        request: json!({"abi": 1, "kind": "tool", "id": "w", "params": {}, "layers": {}, "lang": "en"}),
    };
    let control = std::sync::Arc::new(crate::tools::Control::default());
    let ctl = control.clone();
    let run = std::thread::spawn(move || job.run(&ctl));
    // What it logs shows while it runs.
    let t0 = std::time::Instant::now();
    while control.last_log().is_none() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    while control.last_log().as_deref() != Some("step 1 of 3") && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(control.last_log().as_deref(), Some("step 1 of 3"));
    // A cancel stops it promptly, and the run fails as cancelled.
    let t0 = std::time::Instant::now();
    control.cancel();
    let e = run.join().unwrap().unwrap_err();
    assert!(e == "Work Forever: cancelled" && t0.elapsed() < std::time::Duration::from_secs(10), "{e}");
    // Progress shows while a tool runs, but isn't kept in its messages as log lines are.
    let job = crate::tools::Job {
        plugin: p,
        name: "Make Points".into(),
        request: json!({"abi": 1, "kind": "tool", "id": "m", "params": {}, "layers": {}, "lang": "en"}),
    };
    let control = crate::tools::Control::default();
    assert_eq!(job.run(&control).unwrap().messages, vec!["made"]);
    assert_eq!(control.last_log().as_deref(), Some("making"));
    crate::unload("proc-cancel");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn process_manifests_are_checked() {
    let base = |extra: Value| {
        let mut m = json!({"id": "p", "name": "P", "version": "1.0.0", "commands": [{"id": "h", "label": "H"}]});
        for (k, v) in extra.as_object().unwrap() {
            m[k] = v.clone();
        }
        Manifest::parse(&m.to_string())
    };
    assert!(base(json!({"runtime": "process", "run": ["python3", "main.py"]})).is_ok());
    assert!(base(json!({"runtime": "process"})).unwrap_err().contains("needs 'run'"));
    assert!(base(json!({"runtime": "process", "run": []})).is_err());
    assert!(base(json!({"runtime": "process", "run": [" "]})).is_err());
    assert!(base(json!({"runtime": "process", "run": {"windos": ["x"]}})).unwrap_err().contains("names no program"));
    assert!(base(json!({"runtime": "process", "run": vec!["a"; 65]})).is_err());
    assert!(base(json!({"run": ["python3"]})).unwrap_err().contains("process plugins"));
    assert!(base(json!({"runtime": "native"})).is_err());
    // A program by a relative path stays in the plugin's folder.
    assert!(base(json!({"runtime": "process", "run": ["bin/tool"]})).is_ok());
    for bad in ["../../bin/sh", "bin/../../x", "bin\\tool.exe", "./tool"] {
        assert!(base(json!({"runtime": "process", "run": [bad]})).unwrap_err().contains("plugin's folder"), "{bad}");
    }
    let abs = if cfg!(windows) { "C:\\Python\\python.exe" } else { "/usr/bin/python3" };
    assert!(base(json!({"runtime": "process", "run": [abs, "main.py"]})).is_ok());
    // By platform: this one's, else the default; none is an error when it's prepared.
    let m = base(json!({"runtime": "process", "run": {"windows": ["w"], "macos": ["m"], "linux": ["l"]}})).unwrap();
    let mine = if cfg!(windows) {
        "w"
    } else if cfg!(target_os = "macos") {
        "m"
    } else {
        "l"
    };
    assert_eq!(m.run.as_ref().unwrap().for_this_platform().unwrap(), [mine.to_string()]);
    let m = base(json!({"runtime": "process", "run": {"default": ["d"]}})).unwrap();
    assert_eq!(m.run.as_ref().unwrap().for_this_platform().unwrap(), ["d".to_string()]);
    let other = if cfg!(windows) { "linux" } else { "windows" };
    let m = base(json!({"runtime": "process", "run": {other: ["x"]}})).unwrap();
    let pkg = Package { manifest: m, wasm: Vec::new(), sha256: String::new(), dir: Some(std::env::temp_dir()), skills: Vec::new() };
    assert!(crate::prepare(pkg, "test").unwrap_err().contains("doesn't run on this platform"));
}

#[test]
fn process_packages_unpack_safely() {
    use std::io::Write;
    let plugins = std::env::temp_dir().join(format!("rustgis-test-unpack-{}", std::process::id()));
    let _store = store_lock();
    crate::store::set_dir(Some(plugins.clone()));
    let run = testing::child_run("tests::process_child");
    let manifest = testing::process_manifest("unpacked", &[], &run);
    let zip = |files: &[(&str, &[u8], u32)]| {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes, mode) in files {
            let opts = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap().into_inner()
    };
    // In one top-level folder, with an executable.
    let good =
        zip(&[("u/plugin.json", manifest.as_bytes(), 0o644), ("u/bin/tool.sh", b"#!/bin/sh\n", 0o755), ("u/data/a.txt", b"a", 0o644)]);
    let p = crate::store::install_bytes(&good).unwrap();
    let dir = plugins.join("unpacked/unpacked");
    assert_eq!(std::fs::read(dir.join("data/a.txt")).unwrap(), b"a");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.join("bin/tool.sh")).unwrap().permissions().mode() & 0o777, 0o755);
    }
    // It runs from there.
    let req = json!({"abi": 1, "kind": "command", "id": "h", "params": {}, "lang": "en"});
    assert_eq!(crate::call(&p, &mut Recorder::default(), &req, false).unwrap(), json!({"hello": "world"}));
    // The same package again isn't unpacked again; a changed one replaces the folder.
    std::fs::write(dir.join("data/a.txt"), b"edited").unwrap();
    crate::store::install_bytes(&good).unwrap();
    assert_eq!(std::fs::read(dir.join("data/a.txt")).unwrap(), b"edited");
    let changed = zip(&[("plugin.json", manifest.as_bytes(), 0o644), ("data/b.txt", b"b", 0o644)]);
    crate::store::install_bytes(&changed).unwrap();
    assert!(dir.join("data/b.txt").is_file() && !dir.join("data/a.txt").exists());
    // A package that can't run here is refused before it touches the installed copy.
    let mut elsewhere: Value = serde_json::from_str(&manifest).unwrap();
    elsewhere["run"] = json!({if cfg!(windows) { "linux" } else { "windows" }: ["x"]});
    let elsewhere = zip(&[("plugin.json", elsewhere.to_string().as_bytes(), 0o644), ("data/c.txt", b"c", 0o644)]);
    assert!(crate::store::install_bytes(&elsewhere).unwrap_err().contains("doesn't run on this platform"));
    assert!(dir.join("data/b.txt").is_file() && !dir.join("data/c.txt").exists());
    let p = crate::get("unpacked").unwrap();
    assert_eq!(crate::call(&p, &mut Recorder::default(), &req, false).unwrap(), json!({"hello": "world"}));
    // Paths out of the folder are refused, and nothing is left behind.
    let evil = zip(&[("plugin.json", manifest.as_bytes(), 0o644), ("../evil.txt", b"x", 0o644)]);
    assert!(crate::store::install_bytes(&evil).unwrap_err().contains("unsafe path"));
    // ...and the package that was installed stays the one kept, so the next start loads it.
    assert!(crate::store::load_installed().is_empty(), "{:?}", crate::store::load_installed());
    assert!(dir.join("data/b.txt").is_file());
    assert!(!plugins.join("unpacked/evil.txt").exists() && !plugins.join("evil.txt").exists());
    assert!(dir.join("data/b.txt").is_file(), "the installed copy stays");
    // Uninstalling removes the files.
    crate::store::uninstall("unpacked").unwrap();
    assert!(!dir.exists());
    let _ = std::fs::remove_dir_all(&plugins);
}

// ---------------------------------------------------------------------------------------------
// Python environments

#[test]
fn python_environments_are_declared_checked_and_required() {
    let base = |python: Value| {
        let m = json!({"id": "py", "name": "Py", "version": "1.0.0", "runtime": "process", "run": ["python", "main.py"],
            "python": python, "commands": [{"id": "h", "label": "H"}]});
        Manifest::parse(&m.to_string())
    };
    assert_eq!(base(json!({})).unwrap().python.unwrap().version, "3.12");
    assert!(base(json!({"version": "3.11", "requirements": "req/requirements.txt"})).is_ok());
    for bad in [json!({"version": "2.7"}), json!({"version": "3.x"}), json!({"version": ""}), json!({"requirements": "../r.txt"})] {
        assert!(base(bad.clone()).is_err(), "{bad}");
    }
    let wasm = json!({"id": "w", "name": "W", "version": "1.0.0", "python": {}, "commands": [{"id": "h", "label": "H"}]});
    assert!(Manifest::parse(&wasm.to_string()).unwrap_err().contains("process plugins"));

    // Loaded, `python` is the environment's interpreter, and calls wait for it to be ready.
    let plugins = std::env::temp_dir().join(format!("rustgis-test-pyenv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&plugins);
    let _store = store_lock();
    crate::store::set_dir(Some(plugins.clone()));
    let dir = plugins.join("src");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("requirements.txt"), "numpy\n").unwrap();
    let mut m: Value = serde_json::from_str(&testing::manifest("pyenv-test", &[])).unwrap();
    m["runtime"] = json!("process");
    m["run"] = json!(["python", "main.py"]);
    m["python"] = json!({"version": "3.12", "requirements": "requirements.txt"});
    std::fs::write(dir.join("plugin.json"), m.to_string()).unwrap();
    let p = crate::load(Package::from_dir(&dir).unwrap(), "test").unwrap();
    let check = p.env_check().unwrap().clone();
    assert_eq!(check.env, plugins.join("envs/pyenv-test"));
    assert_eq!(crate::pyenv::status("pyenv-test"), Some(false));
    let req = json!({"abi": 1, "kind": "command", "id": "h", "params": {}, "lang": "en"});
    let e = crate::call(&p, &mut Recorder::default(), &req, false).unwrap_err();
    assert!(e.contains("Python environment isn't ready") && e.contains("plugins prepare pyenv-test"), "{e}");

    // An environment made for these requirements is ready; changed requirements need another.
    let python = crate::pyenv::python_in(&check.env);
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "").unwrap();
    let made_for = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"3.12\nnumpy\n");
        h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    std::fs::write(check.env.join(".rustgis-env"), &made_for).unwrap();
    assert!(check.ready() && crate::pyenv::status("pyenv-test") == Some(true));
    std::fs::write(dir.join("requirements.txt"), "numpy\nscipy\n").unwrap();
    let p = crate::load(Package::from_dir(&dir).unwrap(), "test").unwrap();
    assert!(!p.env_check().unwrap().ready(), "changed requirements need a new environment");
    assert_eq!(crate::pyenv::status("h-not-loaded"), None);
    crate::unload("pyenv-test");
    // An installed package (unpacked only after it's checked) loads; the environment made above
    // for the same requirements serves it, and uninstalling removes it.
    let pkg = testing::zip_of(&[("plugin.json", m.to_string().as_bytes()), ("requirements.txt", b"numpy\n")]);
    let p = crate::store::install_bytes(&pkg).unwrap();
    assert!(p.dir().unwrap().join("requirements.txt").is_file());
    assert_eq!(crate::pyenv::status("pyenv-test"), Some(true));
    crate::store::uninstall("pyenv-test").unwrap();
    assert!(!check.env.exists());
    let _ = std::fs::remove_dir_all(&plugins);
}

/// Downloads uv and a Python and runs a plugin in its environment (network; run by hand:
/// `cargo test -p rustgis-plugins --features testing -- --ignored python_environments_are_made`).
#[test]
#[ignore]
fn python_environments_are_made() {
    let plugins = std::env::temp_dir().join(format!("rustgis-test-pyenv-made-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&plugins);
    let _store = store_lock();
    crate::store::set_dir(Some(plugins.clone()));
    let dir = plugins.join("src");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("requirements.txt"), "six\n").unwrap();
    std::fs::write(
        dir.join("main.py"),
        "import json, sys, six\nreq = json.loads(sys.stdin.readline())\nprint(json.dumps({'ok': {'python': sys.version_info[:2], 'six': six.__version__}}), flush=True)\n",
    )
    .unwrap();
    let mut m: Value = serde_json::from_str(&testing::manifest("pyenv-made", &[])).unwrap();
    m["runtime"] = json!("process");
    m["run"] = json!(["python", "main.py"]);
    m["python"] = json!({"version": "3.12", "requirements": "requirements.txt"});
    std::fs::write(dir.join("plugin.json"), m.to_string()).unwrap();
    let p = crate::load(Package::from_dir(&dir).unwrap(), "test").unwrap();
    let mut said = Vec::new();
    let python = crate::pyenv::prepare("pyenv-made", &mut |l| said.push(l.to_string())).unwrap();
    assert!(python.is_file() && said.last().map(String::as_str) == Some("Ready"), "{said:?}");
    let req = json!({"abi": 1, "kind": "command", "id": "h", "params": {}, "lang": "en"});
    let out = crate::call(&p, &mut Recorder::default(), &req, false).unwrap();
    assert_eq!(out["python"], json!([3, 12]));
    // Ready now: preparing again does nothing.
    let t0 = std::time::Instant::now();
    crate::pyenv::prepare("pyenv-made", &mut |_| {}).unwrap();
    assert!(t0.elapsed() < std::time::Duration::from_secs(2));
    crate::unload("pyenv-made");
    let _ = std::fs::remove_dir_all(&plugins);
}

/// A preparation waits for the lock another holds (here, the test), then finds the environment
/// ready and is done: no download, so it runs offline.
#[test]
fn python_environment_preparations_wait_for_the_lock() {
    use std::sync::{Arc, Mutex};
    let plugins = std::env::temp_dir().join(format!("rustgis-test-pyenv-wait-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&plugins);
    let _store = store_lock();
    crate::store::set_dir(Some(plugins.clone()));
    let dir = plugins.join("src");
    std::fs::create_dir_all(&dir).unwrap();
    let mut m: Value = serde_json::from_str(&testing::manifest("pyenv-wait", &[])).unwrap();
    m["runtime"] = json!("process");
    m["run"] = json!(["python", "main.py"]);
    m["python"] = json!({"version": "3.12"});
    std::fs::write(dir.join("plugin.json"), m.to_string()).unwrap();
    let p = crate::load(Package::from_dir(&dir).unwrap(), "test").unwrap();
    let env = p.env_check().unwrap().env.clone();
    std::fs::create_dir_all(env.parent().unwrap()).unwrap();
    let held = std::fs::File::create(env.parent().unwrap().join(".pyenv-wait.lock")).unwrap();
    held.lock().unwrap();

    let said = Arc::new(Mutex::new(Vec::<String>::new()));
    let heard = said.clone();
    let prep = std::thread::spawn(move || crate::pyenv::prepare("pyenv-wait", &mut |l| heard.lock().unwrap().push(l.to_string())));
    let t0 = std::time::Instant::now();
    while said.lock().unwrap().is_empty() && t0.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!prep.is_finished(), "it waits while the lock is held");
    let waiting = said.lock().unwrap().clone();
    assert_eq!(waiting, ["Waiting for another preparation of this environment to finish"]);

    // Meanwhile, "the other preparation" makes the environment, then lets go.
    let python = crate::pyenv::python_in(&env);
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "").unwrap();
    let made_for = {
        use sha2::{Digest, Sha256};
        Sha256::digest(b"3.12\n").iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    std::fs::write(env.join(".rustgis-env"), made_for).unwrap();
    drop(held);
    assert_eq!(prep.join().unwrap().unwrap(), python);
    let said = said.lock().unwrap().clone();
    assert_eq!(said.len(), 1, "nothing else was done: {said:?}");
    crate::unload("pyenv-wait");
    let _ = std::fs::remove_dir_all(&plugins);
}

/// Two preparations of one environment at once (the app and rustgis-cli): the second waits for
/// the first and finds its environment (network; run by hand with `--ignored`).
#[test]
#[ignore]
fn python_environments_are_prepared_one_at_a_time() {
    let plugins = std::env::temp_dir().join(format!("rustgis-test-pyenv-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&plugins);
    let _store = store_lock();
    crate::store::set_dir(Some(plugins.clone()));
    let dir = plugins.join("src");
    std::fs::create_dir_all(&dir).unwrap();
    let mut m: Value = serde_json::from_str(&testing::manifest("pyenv-lock", &[])).unwrap();
    m["runtime"] = json!("process");
    m["run"] = json!(["python", "main.py"]);
    m["python"] = json!({"version": "3.12"});
    std::fs::write(dir.join("plugin.json"), m.to_string()).unwrap();
    crate::load(Package::from_dir(&dir).unwrap(), "test").unwrap();
    let runs: Vec<_> = (0..2)
        .map(|_| {
            std::thread::spawn(|| {
                let mut said = Vec::new();
                let r = crate::pyenv::prepare("pyenv-lock", &mut |l| said.push(l.to_string()));
                (r, said)
            })
        })
        .collect();
    let results: Vec<_> = runs.into_iter().map(|t| t.join().unwrap()).collect();
    for (r, said) in &results {
        assert!(r.as_ref().is_ok_and(|p| p.is_file()), "{r:?} {said:?}");
    }
    // Exactly one of them made it; the other waited and found it.
    let made = results.iter().filter(|(_, said)| said.iter().any(|l| l.starts_with("Installing Python"))).count();
    assert_eq!(made, 1, "{:?}", results.iter().map(|(_, s)| s).collect::<Vec<_>>());
    assert_eq!(crate::pyenv::status("pyenv-lock"), Some(true));
    crate::unload("pyenv-lock");
    let _ = std::fs::remove_dir_all(&plugins);
}
