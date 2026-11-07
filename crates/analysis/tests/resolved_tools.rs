//! A delayed run must keep its original tool schema and runner after a suite reload.

use std::sync::atomic::{AtomicBool, Ordering};

use rustgis_analysis::{Inputs, Output, ParamKind, ParamSpec, ToolResult, ToolSpec};
use serde_json::{Value, json};

// Both fixtures publish into the process-wide registry; discovery visits every suite.
static REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

static DISCOVERIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static RELOADED: AtomicBool = AtomicBool::new(false);

/// No layer input is needed by the synthetic suite.
struct Empty;

impl Inputs for Empty {
    /// The synthetic runner receives no borrowed project data.
    fn dataset(&self, _: &str) -> Option<&rustgis_data::Dataset> {
        None
    }

    /// No selected rows can affect this fixture's output.
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
}

const OLD: &[ParamSpec] = &[ParamSpec { name: "value", label: "Value", kind: ParamKind::Number, required: false, default: "1", help: "" }];
const NEW: &[ParamSpec] = &[ParamSpec { name: "value", label: "Value", kind: ParamKind::Number, required: false, default: "2", help: "" }];

/// Reloading changes both the schema default and the runner-visible specification.
fn provider() -> Vec<ToolSpec> {
    DISCOVERIES.fetch_add(1, Ordering::SeqCst);
    let reloaded = RELOADED.load(Ordering::Relaxed);
    vec![ToolSpec {
        id: "resolved-test-tool",
        name: rustgis_analysis::TOOLS[0].name,
        toolbox: "Test",
        summary: if reloaded { "new" } else { "old" },
        params: if reloaded { NEW } else { OLD },
        run: rustgis_analysis::external_placeholder,
    }]
}

/// Reports which captured schema and defaults actually reached the runner.
fn runner(spec: &ToolSpec, _: &dyn Inputs, params: &Value) -> Result<ToolResult, String> {
    Ok(ToolResult { output: Output::Layers(Vec::new()), messages: vec![format!("{}:{}", spec.summary, params["value"])] })
}

#[test]
fn resolved_tool_keeps_schema_after_reload_and_honors_cancellation() {
    let _registry = REGISTRY.lock().unwrap();
    rustgis_analysis::register_external("resolved-test-suite", provider, runner);
    assert!(rustgis_analysis::resolve_published("resolved-test-tool").is_none());
    assert!(rustgis_analysis::published_external_tools_of("resolved-test-suite").is_empty());
    assert!(!rustgis_analysis::published_tools().any(|t| t.id == "resolved-test-tool"));
    assert_eq!(DISCOVERIES.load(Ordering::SeqCst), 0);
    let original = rustgis_analysis::resolve("RESOLVED-TEST-TOOL").unwrap();
    assert_eq!(original.source(), Some("resolved-test-suite"));
    assert_eq!(original.resolve_params(&json!({})).unwrap(), json!({"value": 1}));
    // Built-in names still win over the external name; ids are considered first.
    let builtin = &rustgis_analysis::TOOLS[0];
    assert_eq!(rustgis_analysis::resolve(builtin.name).unwrap().spec().id, builtin.id);

    RELOADED.store(true, Ordering::Relaxed);
    rustgis_analysis::reload_external_of("resolved-test-suite");
    assert!(!original.is_current());
    assert!(rustgis_analysis::resolve_published("resolved-test-tool").is_none());
    assert_eq!(DISCOVERIES.load(Ordering::SeqCst), 1, "current/review must not rediscover");
    let current = rustgis_analysis::resolve("resolved-test-tool").unwrap();
    let control = rustgis_analysis::control::Control::new();
    assert_eq!(rustgis_analysis::run_resolved(&original, &Empty, &json!({}), &control).unwrap().messages, ["old:1"]);
    assert_eq!(rustgis_analysis::run_resolved(&current, &Empty, &json!({}), &control).unwrap().messages, ["new:2"]);

    control.cancel();
    assert!(rustgis_analysis::run_resolved(&original, &Empty, &json!({}), &control).unwrap_err().contains("cancelled before"));
}

/// A provider finishing across reload cannot publish its superseded snapshot as current.
#[test]
fn permission_policy_provider_publication_rejects_in_flight_reload() {
    let _registry = REGISTRY.lock().unwrap();
    use std::sync::{Condvar, Mutex};
    static STATE: (Mutex<(bool, bool)>, Condvar) = (Mutex::new((false, false)), Condvar::new());
    fn held() -> Vec<ToolSpec> {
        let (state, ready) = &STATE;
        let mut state = state.lock().unwrap();
        state.0 = true;
        ready.notify_all();
        while !state.1 {
            state = ready.wait(state).unwrap();
        }
        vec![ToolSpec {
            id: "stale-publication-tool",
            name: "Stale publication",
            toolbox: "Test",
            summary: "held",
            params: OLD,
            run: rustgis_analysis::external_placeholder,
        }]
    }
    rustgis_analysis::register_external("held-publication", held, runner);
    let loading = std::thread::spawn(|| rustgis_analysis::external_tools_of("held-publication"));
    let (state, ready) = &STATE;
    let mut state = state.lock().unwrap();
    while !state.0 {
        state = ready.wait(state).unwrap();
    }
    rustgis_analysis::reload_external_of("held-publication");
    state.1 = true;
    ready.notify_all();
    drop(state);
    assert!(loading.join().unwrap().is_empty());
    assert!(rustgis_analysis::resolve_published("stale-publication-tool").is_none());
    assert!(rustgis_analysis::published_external_tools_of("held-publication").is_empty());
    assert_eq!(rustgis_analysis::external_tools_of("held-publication").len(), 1);
    assert!(rustgis_analysis::resolve_published("stale-publication-tool").unwrap().is_current());
}
