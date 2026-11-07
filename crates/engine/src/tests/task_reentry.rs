//! Task re-entry stays bounded without changing finite scripts or outer transactions.
use super::*;
use crate::policy::{AgentAccess, Call, Effect, Effects, Outcome, Revocation, Scope};
use std::sync::Arc;

struct Live;
impl Revocation for Live {
    fn allows(&self, _: Effects) -> bool {
        true
    }
}

/// A live caller whose access setting is checked by the ordinary permission table.
fn agent(access: AgentAccess) -> Scope {
    Scope::agent(access, true, "metadata-review", Some("metadata-review".into()), Arc::new(Live))
}

/// Create a locally trusted task through the public API, keeping the trust policy in the test.
fn task(s: &mut Session, name: &str, steps: Value) {
    s.run("task.new", &json!({"name": name, "steps": steps})).unwrap();
}

/// A script step that indirectly enters the named task, including a different task.
fn reenter(name: &str) -> Value {
    json!({"title":"again", "command":"script.run", "params":{"script":format!("task.run {}", json!({"task":name}))}})
}

/// A task cannot re-enter itself or another task through a script; cleanup permits a later run.
#[test]
fn task_reentry_through_script_is_refused_and_cleared() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    task(&mut s, "Loop", json!([reenter("Loop")]));
    task(&mut s, "Other", json!([]));
    task(&mut s, "Outer", json!([reenter("Other")]));
    for name in ["Loop", "Outer"] {
        let error = s.run("task.run", &json!({"task":name})).unwrap_err();
        assert!(error.contains("step 1") && error.contains("line 1") && error.contains("already running"), "{error}");
        assert!(!s.task_running);
        assert!(s.run("task.run", &json!({"task":"Other"})).is_ok());
    }
}

/// Successful script/task composition still changes the project in one undoable operation.
#[test]
fn task_reentry_guard_keeps_finite_scripts_and_single_undo() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    let before = s.map().name.clone();
    task(&mut s, "Finite", json!([{"title":"rename", "command":"script.run", "params":{"script":"map.rename {\"name\":\"Changed\"}"}}]));
    let undo = s.undo_depth();
    s.run("script.run", &json!({"script":"task.run {\"task\":\"Finite\"}"})).unwrap();
    assert_eq!(s.map().name, "Changed");
    assert_eq!(s.undo_depth(), undo + 1);
    assert!(!s.task_running);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().name, before);
    s.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(s.map().name, "Changed");
    assert!(!s.task_running, "undo/redo do not restore execution state");
}

/// A refusal after an earlier project mutation restores the project without an extra undo.
#[test]
fn task_reentry_failure_rolls_back_outer_project_transaction() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    s.registry.commands.push(crate::CommandSpec::new("test.countEdit", "Count Edit", "Test", |s, _| {
        s.edit_counts.entry(7).or_default().modified += 1;
        Ok(Value::Null)
    }));
    task(
        &mut s,
        "Loop",
        json!([
            {"title":"rename", "command":"map.rename", "params":{"name":"Changed"}},
            {"title":"count", "command":"test.countEdit"},
            reenter("Loop")
        ]),
    );
    let undo = s.undo_depth();
    s.mark_saved();
    let before = serde_json::to_value(&s.project).unwrap();
    assert!(s.run("task.run", &json!({"task":"Loop"})).unwrap_err().contains("already running"));
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    assert_eq!(s.undo_depth(), undo);
    assert!(s.edit_counts.is_empty());
    assert!(!s.dirty() && !s.task_running);
}

/// Invalid parameters and imported/direct-step refusals leave the guard usable and trust intact.
#[test]
fn task_reentry_guard_preserves_existing_refusals() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    task(&mut s, "Direct", json!([{"title":"again", "command":"task.run", "params":{"task":"Direct"}}]));
    s.run("task.new", &json!({"name":"Imported", "imported":true, "steps":[reenter("Direct")]})).unwrap();
    for (params, message) in [
        (json!({"task":"missing"}), "no task"),
        (json!({"task":"Direct", "step":999}), "steps"),
        (json!({"task":"Direct"}), "another task"),
        (json!({"task":"Imported"}), "from a file"),
    ] {
        let error = s.run("task.run", &params).unwrap_err();
        assert!(error.contains(message), "{error}");
        assert!(!s.task_running);
    }
}

/// Even a condition panic outside the step's own catch clears the outer task marker.
#[test]
fn task_reentry_guard_clears_after_condition_panic() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    s.registry.commands.push(
        crate::CommandSpec::new("test.conditionPanic", "Panic", "Test", |_, _| Ok(Value::Null)).when(|_| panic!("test condition panic")),
    );
    task(&mut s, "Panic", json!([{"title":"panic", "command":"test.conditionPanic"}]));
    let error = s.run("task.run", &json!({"task":"Panic"})).unwrap_err();
    assert!(error.contains("failed unexpectedly"), "{error}");
    assert!(!s.task_running);
    task(&mut s, "Fine", json!([]));
    s.run("task.run", &json!({"task":"Fine"})).unwrap();
}

/// Project replacement must not clear the active marker and allow a newly created task to enter.
#[test]
fn task_reentry_guard_survives_project_replacement() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    let script =
        format!("project.new {{\"name\":\"Replacement\"}}\ntask.new {}\ntask.run {{\"task\":\"New\"}}", json!({"name":"New", "steps":[]}));
    task(&mut s, "Original", json!([{"title":"replace", "command":"script.run", "params":{"script":script}}]));
    let before = serde_json::to_value(&s.project).unwrap();
    assert!(s.run("task.run", &json!({"task":"Original"})).unwrap_err().contains("already running"));
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    assert!(!s.task_running);
}

/// A step-body panic also releases the marker, preserving the existing contextual task error.
#[test]
fn task_reentry_guard_clears_after_step_panic() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    s.registry.commands.push(crate::CommandSpec::new("test.stepPanic", "Panic", "Test", |_, _| panic!("test step panic")));
    task(&mut s, "Panic", json!([{"title":"panic", "command":"test.stepPanic"}]));
    let error = s.run("task.run", &json!({"task":"Panic"})).unwrap_err();
    assert!(error.contains("step 1") && error.contains("failed unexpectedly"), "{error}");
    assert!(!s.task_running);
    task(&mut s, "Fine", json!([]));
    s.run("task.run", &json!({"task":"Fine"})).unwrap();
}

/// Notebook cells keep their error-reporting contract when a task would indirectly enter tasks.
#[test]
fn task_reentry_guard_covers_notebook_paths() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    task(&mut s, "Loop", json!([reenter("Loop")]));
    s.run("notebook.new", &json!({"name":"Notebook"})).unwrap();
    s.run("notebook.addCell", &json!({"notebook":"Notebook", "source":"task.run {\"task\":\"Loop\"}"})).unwrap();
    s.run("notebook.run", &json!({"notebook":"Notebook"})).unwrap();
    assert!(s.project.notebooks[0].cells.iter().any(|c| c.error && c.output.as_deref().is_some_and(|o| o.contains("already running"))));
    task(&mut s, "ViaNotebook", json!([{"title":"notebook", "command":"notebook.run", "params":{"notebook":"Notebook"}}]));
    // A second run must produce its own cell error, rather than pass on the first run's output.
    for cell in &mut s.project.notebooks[0].cells {
        cell.output = None;
        cell.error = false;
    }
    let result = s.run("task.run", &json!({"task":"ViaNotebook"})).unwrap();
    assert!(result["ran"][0]["result"]["error"].is_u64(), "the task keeps the notebook's cell-error result: {result}");
    assert!(s.project.notebooks[0].cells.iter().any(|c| c.error && c.output.as_deref().is_some_and(|o| o.contains("already running"))));
    assert!(!s.task_running && !s.notebook_running);
}

/// Running locally authored notebook cells requires a fresh approval, as other automation does.
#[test]
fn permission_review_notebook_automation_asks_each_time() {
    #[cfg(not(target_arch = "wasm32"))]
    if isolate_process_test("tests::task_reentry::permission_review_notebook_automation_asks_each_time") {
        return;
    }
    let mut s = world();
    s.run("notebook.new", &json!({"name":"Local automation"})).unwrap();
    s.run("notebook.editCell", &json!({"notebook":"Local automation","cell":1,"source":"map.rename {\"name\":\"Notebook result\"}"}))
        .unwrap();
    let scope = Scope::assistant(1481, Arc::new(Live));
    let call = Call::command("notebook.run", json!({"notebook":"Local automation","cell":1}));
    let Outcome::NeedsApproval(pending) = s.review(&scope, &call) else { panic!("notebook automation needs approval") };
    assert!(!pending.review().remember, "a notebook run cannot be approved for the whole conversation");
    s.run_permitted(scope, pending.allow(), &call).unwrap();
    assert_eq!(s.map().name, "Notebook result");
}

/// A plugin's file/edit permissions do not grant access to a user's notebook automation.
#[test]
fn permission_review_notebook_automation_is_refused_to_plugins() {
    #[cfg(not(target_arch = "wasm32"))]
    if isolate_process_test("tests::task_reentry::permission_review_notebook_automation_is_refused_to_plugins") {
        return;
    }
    let mut s = world();
    s.run("notebook.new", &json!({"name":"User notebook"})).unwrap();
    s.run("notebook.editCell", &json!({"notebook":"User notebook","cell":1,"source":"map.rename {\"name\":\"Notebook result\"}"})).unwrap();
    let plugin_id = "review-notebook-automation";
    plugin(plugin_id, &["files", "edit"]);
    let before = serde_json::to_value(&s.project).unwrap();
    let result = forward(&mut s, plugin_id, "notebook.run", json!({"notebook":"User notebook","cell":1}));
    rustgis_plugins::unload(plugin_id);
    assert!(result.is_err(), "a plugin cannot start notebook automation: {result:?}");
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    s.run("notebook.run", &json!({"notebook":"User notebook","cell":1})).unwrap();
    assert_eq!(s.map().name, "Notebook result", "native notebook execution remains available");
}

/// A live network denial must refuse connection admission before any database is opened.
#[test]
fn permission_review_postgis_connect_checks_live_network_authority() {
    struct NoNetwork;
    impl Revocation for NoNetwork {
        fn allows(&self, effects: Effects) -> bool {
            !effects.contains(Effect::Network)
        }
    }
    let s = world();
    let scope = Scope::assistant(1482, Arc::new(NoNetwork));
    let call = Call::command("pg.connect", json!({"host":"127.0.0.1","database":"not-opened"}));
    assert!(
        matches!(s.review(&scope, &call), Outcome::Refused(error) if error.contains("revoked")),
        "connecting must check the caller's live network authority before I/O"
    );
    assert!(matches!(s.review(&scope, &Call::command("pg.status", json!({}))), Outcome::Allowed));
    let live = Scope::assistant(1483, Arc::new(Live));
    assert!(matches!(s.review(&live, &call), Outcome::NeedsApproval(_)), "a live connection request still asks for explicit approval");
}

/// Lists, offline snapshots and projected caches stay available without external access or lost undo.
#[test]
fn permission_review_memory_views_and_offline_work_need_no_external_access() {
    // Undo invalidates geometry caches; every other project field must be restored exactly.
    let payload = |project: &Project| {
        let mut project = project.clone();
        for map in &mut project.maps {
            for layer in &mut map.layers {
                layer.geometry_rev = 0;
            }
        }
        serde_json::to_value(project).unwrap()
    };
    let mut s = world();
    for name in ["Open area", "Sync area", "Remove area"] {
        s.run("offline.download", &json!({"name":name,"extent":[1,48,4,50]})).unwrap();
    }
    s.run("cache.empty", &Value::Null).unwrap();
    let scope = agent(AgentAccess::Refuse);
    let mut errors = Vec::new();
    for (id, params) in [
        ("share.jobs", json!({})),
        ("offline.download", json!({"name":"Created area","extent":[1,48,4,50]})),
        ("offline.open", json!({"name":"Open area"})),
        ("offline.sync", json!({"name":"Sync area"})),
        ("offline.remove", json!({"name":"Remove area"})),
        ("cache.fill", json!({})),
        ("cache.auto", json!({"on":true})),
    ] {
        let (before, undo) = (payload(&s.project), s.undo_depth());
        match s.run_scoped(scope.clone(), &Call::command(id, params)) {
            Err(error) => errors.push((id, error)),
            Ok(result) => {
                if id == "cache.fill" {
                    assert!(result["cached"].as_u64().is_some_and(|n| n > 0), "{result}");
                }
                if id == "cache.auto" {
                    assert!(s.auto_cache);
                }
                if matches!(id, "offline.download" | "offline.open" | "offline.remove") {
                    assert_eq!(s.undo_depth(), undo + 1, "{id} keeps its ordinary edit transaction");
                    s.run("edit.undo", &Value::Null).unwrap();
                    assert_eq!(payload(&s.project), before, "{id} remains undoable");
                }
            }
        }
    }
    assert!(errors.is_empty(), "memory-only work was refused: {errors:?}");
}

/// Changes to saved/favourite preferences require file permission before their lists change.
#[test]
fn permission_review_source_preferences_require_file_permission() {
    #[cfg(not(target_arch = "wasm32"))]
    if isolate_process_test("tests::task_reentry::permission_review_source_preferences_require_file_permission") {
        return;
    }
    // A connection label carries no path-shaped text from which Files could be inferred.
    let source = json!({"kind":"database","address":"Review connection"});
    let mut failures = Vec::new();
    for (id, params) in [
        ("sources.favourite", source.clone()),
        ("sources.favourite", json!({"kind":"database","address":source["address"],"on":false})),
        ("sources.remove", json!({"list":"saved","kind":"database","address":source["address"]})),
    ] {
        let mut s = world();
        s.run("sources.save", &source).unwrap();
        if params.get("on") == Some(&Value::Bool(false)) {
            s.run("sources.favourite", &source).unwrap();
        }
        let before = s.sources.to_json();
        let call = Call::command(id, params);
        if s.run_scoped(agent(AgentAccess::Refuse), &call).is_ok() {
            failures.push(format!("{id} changed preferences without file permission"));
            continue;
        }
        assert_eq!(s.sources.to_json(), before, "a refused preference change leaves its lists intact");
        let scope = Scope::assistant(1484, Arc::new(Live));
        let Outcome::NeedsApproval(pending) = s.review(&scope, &call) else { panic!("{id} needs file approval") };
        s.run_permitted(scope, pending.allow(), &call).unwrap();
        assert_ne!(s.sources.to_json(), before, "explicit approval permits the preference change");
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
}
