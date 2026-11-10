//! Public dispatch, frozen consent and owned late work share the engine's permission outcome.
use super::*;
use crate::policy::{Call, Effects, Outcome, Pending, Revocation, Scope};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Live(AtomicBool);
impl Revocation for Live {
    fn allows(&self, _: Effects) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
fn assistant(id: u64) -> (Scope, Arc<Live>) {
    let live = Arc::new(Live(AtomicBool::new(true)));
    (Scope::assistant(id, live.clone()), live)
}
fn pending(s: &Session, scope: &Scope, call: &Call) -> Pending {
    match s.review(scope, call) {
        Outcome::NeedsApproval(p) => p,
        Outcome::Refused(e) => panic!("refused instead of a review: {e}"),
        Outcome::Allowed => panic!("the external operation needs approval"),
    }
}

/// Reload's implicit local-file access uses the same grants as explicit file commands.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_reload_requires_file_authority_in_every_ingress() {
    if isolate_process_test("tests::permission_policy::permission_policy_reload_requires_file_authority_in_every_ingress") {
        return;
    }
    use crate::policy::{AgentAccess, Effect, command_effects};
    let path = grid_fgb("policy-reload-grants", 2);
    let mut s = Session::new(Project::default());
    let id = s.run("layer.addData", &json!({"path":path,"stream":true})).unwrap()["layer"].as_u64().unwrap() as u32;
    let params = json!({"layer":id});
    let call = Call::command("layer.reloadData", params.clone());
    assert!(command_effects(s.registry.get("layer.reloadData").unwrap(), &params).contains(Effect::Files));
    let source = s.map().layer(id).unwrap().source.clone().unwrap();
    let depth = s.undo_depth();
    let (scope, _) = assistant(14770);
    let approved = pending(&s, &scope, &call);
    assert!(approved.review().steps.iter().any(|step| step.starts_with("layer.reloadData ")));
    assert!(s.run_scoped(scope.clone(), &call).is_err());
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));
    assert_eq!(s.undo_depth(), depth);
    s.run_permitted(scope.clone(), approved.allow(), &call).unwrap();
    assert!(!s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));

    let task = json!({"name":"Reload linked data","steps":[{"title":"Reload","command":"layer.reloadData","params":params}]});
    s.run("task.new", &task).unwrap();
    let task_call = Call::command("task.run", json!({"task":"Reload linked data"}));
    let approved = pending(&s, &scope, &task_call);
    assert!(s.run_scoped(scope.clone(), &task_call).is_err(), "a trusted task does not widen its assistant caller");
    s.run_permitted(scope, approved.allow(), &task_call).unwrap();
    let mut imported = task;
    imported["name"] = json!("Imported reload");
    imported["imported"] = json!(true);
    s.run("task.new", &imported).unwrap();
    let full = Scope::agent(AgentAccess::Full, false, "reload-imported-full", None, Arc::new(Live(AtomicBool::new(true))));
    assert!(s.run_scoped(full, &Call::command("task.run", json!({"task":"Imported reload"}))).is_err());

    plugin("policy-reload-edit", &["edit"]);
    let source = s.map().layer(id).unwrap().source.clone().unwrap();
    let error = forward(&mut s, "policy-reload-edit", "layer.reloadData", params.clone()).unwrap_err();
    assert!(error.contains("'files' permission"), "{error}");
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));
    plugin("policy-reload-files", &["edit", "files"]);
    forward(&mut s, "policy-reload-files", "layer.reloadData", params.clone()).unwrap();
    assert!(!s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));
    let source = s.map().layer(id).unwrap().source.clone().unwrap();
    s.run("layer.reloadData", &params).unwrap();
    assert!(!s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source), "native User dispatch remains trusted");
    rustgis_plugins::unload("policy-reload-edit");
    rustgis_plugins::unload("policy-reload-files");
    std::fs::remove_file(path).unwrap();
}

/// Admission precedes preparation, and preparation itself cannot read a file before its worker.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_reload_preparation_requires_approval_before_file_work() {
    use crate::cmd::reads::{read_ahead, read_land};
    use crate::policy::AgentAccess;
    use rustgis_analysis::control::Control;
    let path = grid_fgb("policy-reload-preparation", 2);
    let bytes = std::fs::read(&path).unwrap();
    let mut s = Session::new(Project::default());
    let id = s.run("layer.addData", &json!({"path":path,"stream":true})).unwrap()["layer"].as_u64().unwrap() as u32;
    let params = json!({"layer":id});
    let source = s.map().layer(id).unwrap().source.clone().unwrap();
    let depth = s.undo_depth();
    std::fs::remove_file(&path).unwrap();
    let (scope, _) = assistant(14771);
    let error = s.with_scope(scope, |s| read_ahead(s, "layer.reloadData", &params)).unwrap().err().unwrap();
    assert!(error.contains("approval"), "{error}");
    let refused = Scope::agent(AgentAccess::Refuse, false, "reload-refused", None, Arc::new(Live(AtomicBool::new(true))));
    assert!(s.with_scope(refused, |s| read_ahead(s, "layer.reloadData", &params)).unwrap().is_err());
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));
    assert_eq!(s.undo_depth(), depth);
    // An authorized picker can capture its work while the file is absent; only the worker
    // opens it, after publication below. An unapproved picker above cannot return any work.
    let ahead = read_ahead(&mut s, "layer.reloadData", &params).unwrap();
    assert!(ahead.reads());
    std::fs::write(&path, bytes).unwrap();
    read_land(&mut s, ahead.run(&Control::new()).unwrap()).unwrap();
    assert!(!s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));
    std::fs::remove_file(path).unwrap();
}

/// A prepared reload retains its initiating scope and target through cancellation and delivery.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_reload_worker_keeps_its_owner_without_native_fallback() {
    use crate::cmd::reads::{read_ahead, read_land};
    use crate::policy::AgentAccess;
    use rustgis_analysis::control::Control;
    let path = grid_fgb("policy-reload-owner", 2);
    let mut s = Session::new(Project::default());
    let id = s.run("layer.addData", &json!({"path":path,"stream":true})).unwrap()["layer"].as_u64().unwrap() as u32;
    let other = s.run("layer.addData", &json!({"path":path,"stream":true})).unwrap()["layer"].as_u64().unwrap() as u32;
    let live = Arc::new(Live(AtomicBool::new(true)));
    let scope = Scope::agent(AgentAccess::Allow, false, "reload-owner", None, live.clone());
    let params = json!({"layer":id});
    let source = s.map().layer(id).unwrap().source.clone().unwrap();
    let depth = s.undo_depth();
    let ahead = s.with_scope(scope.clone(), |s| read_ahead(s, "layer.reloadData", &params)).unwrap().unwrap();
    live.0.store(false, Ordering::SeqCst);
    assert!(ahead.run(&Control::new()).err().unwrap().contains("revoked"), "revocation refuses worker entry");
    live.0.store(true, Ordering::SeqCst);
    let ahead = s.with_scope(scope.clone(), |s| read_ahead(s, "layer.reloadData", &params)).unwrap().unwrap();
    let control = Control::new();
    control.cancel();
    assert!(ahead.run(&control).err().unwrap().contains("cancelled"));
    let read = s.with_scope(scope.clone(), |s| read_ahead(s, "layer.reloadData", &params)).unwrap().unwrap().run(&Control::new()).unwrap();
    let (foreign, _) = assistant(14772);
    assert!(s.with_scope(foreign, |s| read_land(s, read)).unwrap().unwrap_err().contains("another caller"));
    let read = s.with_scope(scope.clone(), |s| read_ahead(s, "layer.reloadData", &params)).unwrap().unwrap().run(&Control::new()).unwrap();
    live.0.store(false, Ordering::SeqCst);
    assert!(read_land(&mut s, read).unwrap_err().contains("revoked"), "root delivery cannot replace the revoked owner");
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));
    assert_eq!(s.undo_depth(), depth);
    live.0.store(true, Ordering::SeqCst);
    s.active_layer = Some(id);
    let ahead = s.with_scope(scope.clone(), |s| read_ahead(s, "layer.reloadData", &json!({}))).unwrap().unwrap();
    s.active_layer = Some(other);
    let other_source = s.map().layer(other).unwrap().source.clone().unwrap();
    let read = ahead.run(&Control::new()).unwrap();
    s.with_scope(scope, |s| read_land(s, read)).unwrap().unwrap();
    assert!(!s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));
    assert!(s.map().layer(other).unwrap().source.as_ref().unwrap().same_opening(&other_source));
    assert_eq!(s.undo_depth(), depth + 1);
    std::fs::remove_file(path).unwrap();
}

/// Training chooses its raster and polygons before a model exists; approval must use that branch.
#[test]
fn permission_policy_training_classification_binds_its_actual_inputs() {
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName":"scene.tif","data":two_class_b64()})).unwrap();
    assert!(s.project.dl_models.is_empty());
    let (scope, _) = assistant(14610);
    let call = Call::script(&format!(
        "dl.classify {}\nmap.addTiles {}",
        json!({"layer":"scene","training":"Countries","classField":"name","model":"New classification","epochs":1}),
        json!({"url":"https://classification-review.invalid/{z}/{x}/{y}.png"})
    ));
    let approved = pending(&s, &scope, &call);
    s.run("layer.remove", &json!({"layer":"Countries"})).unwrap();
    let error = s.run_permitted(scope, approved.allow(), &call).unwrap_err();
    assert!(!error.is_empty());
    assert!(s.project.dl_models.is_empty(), "a changed training input is refused before fitting");
}

/// The save executor's trimmed names cannot turn an imported model into user-authored content.
#[test]
fn permission_policy_model_save_uses_the_executors_normalized_names() {
    let mut s = world();
    let graph = json!({"nodes":[
        {"id":"input","kind":"input","layer":"Cities"},
        {"id":"buffer","kind":"tool","tool":"buffer","params":{"distance":10}}
    ],"edges":[{"from":"input","fromPort":"out","to":"buffer","toPort":"input"}]});
    s.run("model.new", &json!({"name":"Imported","graph":graph,"imported":true})).unwrap();
    assert!(!s.item_trusted("model", "Imported"));
    let result = s.run("model.save", &json!({"name":" Imported ","graph":graph,"rename":"Renamed"})).unwrap();
    assert_eq!(result["model"], "Renamed");
    assert!(!s.item_trusted("model", "Renamed"), "normalizing an imported name cannot confer trust");
    let result = s.run("model.save", &json!({"name":" Fresh ","graph":graph,"rename":" Normalized "})).unwrap();
    assert_eq!(result["model"], "Normalized");
    assert!(s.item_trusted("model", "Normalized"), "fresh user content records the actual saved name");
}

/// Equal display redactions do not confer consent for different raw addresses or defaults.
#[test]
fn permission_policy_consent_binds_raw_params_and_default_targets() {
    let mut s = world();
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    let (scope, _) = assistant(1);
    let a = Call::command("layer.addUrl", json!({"url":"https://example.invalid/data.geojson?token=raw-one"})).with_request("r");
    let b = Call::command("layer.addUrl", json!({"url":"https://example.invalid/data.geojson?token=raw-two"})).with_request("r");
    let p = pending(&s, &scope, &a);
    assert!(!p.review().what.contains("raw-one"));
    assert!(s.run_permitted(scope.clone(), p.allow(), &b).unwrap_err().contains("changed"));
    assert!(s.take_url_requests().is_empty());
    let p = pending(&s, &scope, &a);
    s.active_layer = s.map().layer_by_name("Cities").map(|l| l.id);
    assert!(s.run_permitted(scope.clone(), p.allow(), &a).unwrap_err().contains("changed"));
    let p = pending(&s, &scope, &a);
    s.run_permitted(scope, p.allow(), &a).unwrap();
    assert_eq!(s.take_url_requests().len(), 1);
    let (scope, _) = assistant(127);
    let nested = Call::command(
        "layer.addUrl",
        json!({"url":"https://example.invalid/clean.csv", "token":"RGSYNTH_direct",
        "note":format!("message {}", json!({"script":format!("layer.addUrl {}", json!({"password":"RGSYNTH_nested","url":"https://example.invalid/x?token=RGSYNTH_url"}))}))}),
    );
    let shown = pending(&s, &scope, &nested);
    assert!(shown.review().what.contains("clean.csv"));
    assert!(!shown.review().what.contains("RGSYNTH_"));
    let long = Call::command("layer.addUrl", json!({"url":"https://example.invalid/clean.csv","note":"safe".repeat(1500)}));
    assert!(matches!(s.review(&scope, &long), Outcome::Refused(e) if e.contains("too long")));

    // A single-layer owner falls back to the active layer for a non-scalar selector.
    // Review diagram and manual-label operations against their actual effect in an asking script.
    let mut s = world();
    for name in ["Diagram A", "Diagram B"] {
        s.run(
            "layer.addDataText",
            &json!({"name":name,"fileName":"diagram.geojson","text":r#"{"type":"Feature","properties":{"n":1},"geometry":{"type":"Point","coordinates":[0,0]}}"#}),
        )
        .unwrap();
    }
    let (scope, _) = assistant(130);
    for selector in [json!({"layer":["Diagram A"]}), json!({"layer":"Diagram A"}), json!({})] {
        for name in ["Diagram A", "Diagram B"] {
            s.run("layer.diagram", &json!({"layer":name,"kind":"pie","fields":["n"],"size":8})).unwrap();
            let feature = s.map().layer_by_name(name).unwrap().dataset.features.first().unwrap().id;
            s.run("labels.move", &json!({"layer":name,"feature":feature,"to":[1,2]})).unwrap();
        }
        s.run("layer.select", &json!({"layer":"Diagram B"})).unwrap();
        let expected = if selector["layer"].is_string() { "Diagram A" } else { "Diagram B" };
        let other = if expected == "Diagram A" { "Diagram B" } else { "Diagram A" };
        let mut set = selector.clone();
        set["kind"] = json!("bar");
        set["fields"] = json!(["n"]);
        set["size"] = json!(12);
        let mut moved = selector.clone();
        moved["feature"] = json!(s.map().layer_by_name(expected).unwrap().dataset.features.first().unwrap().id);
        moved["to"] = json!([3, 4]);
        let call = Call::script(&format!(
            "layer.diagram {set}\nlayer.diagramInfo {selector}\nlayer.clearDiagram {selector}\nlabels.move {moved}\nlabels.resetMoved {selector}\nmap.addTiles {{\"url\":\"https://diagram.invalid/{{z}}/{{x}}/{{y}}.png\"}}"
        ));
        let p = pending(&s, &scope, &call);
        let details = p.review().details.clone();
        let defaults = p.review().defaults.clone();
        s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
        assert!(s.map().layer_by_name(expected).unwrap().diagram.is_none(), "the executor clears its actual target");
        assert!(s.map().layer_by_name(other).unwrap().diagram.is_some(), "the other layer retains its diagram");
        assert!(s.map().layer_by_name(expected).unwrap().labels.default.moved.is_empty(), "the label owner clears its actual target");
        assert_eq!(s.map().layer_by_name(other).unwrap().labels.default.moved.len(), 1, "the other layer keeps its saved label move");
        let commands = ["layer.diagram", "layer.diagramInfo", "layer.clearDiagram", "labels.move", "labels.resetMoved"];
        if selector["layer"].is_string() {
            for id in commands {
                let detail = details.iter().find(|d| d.operation == id).unwrap();
                assert_eq!(
                    detail.targets,
                    [format!("{} / {expected}", s.map().name)],
                    "single-layer review target differs from execution: {id}"
                );
            }
        } else {
            // Defaults are named once in Starting selection, not repeated as explicit targets.
            assert_eq!(defaults, Some((Some(expected.to_string()), vec![expected.to_string()])));
            for detail in details.iter().filter(|d| commands.contains(&d.operation.as_str())) {
                assert_eq!(detail.targets, [format!("{} / {expected}", s.map().name)], "no contradictory target may appear for a default");
            }
        }
    }
    // A scalar name created or renamed by an earlier step remains a bound in-run target.
    for (first, params, name, named) in [
        (
            "layer.addDataText",
            json!({"name":"Fresh created","fileName":"fresh.geojson","text":r#"{"type":"Feature","properties":{"n":1},"geometry":{"type":"Point","coordinates":[0,0]}}"#}),
            "Fresh created",
            true,
        ),
        ("layer.rename", json!({"layer":"Diagram A","name":"Fresh renamed"}), "Fresh renamed", true),
        (
            "layer.addDataText",
            json!({"name":"Fresh default","fileName":"fresh.geojson","text":r#"{"type":"Feature","properties":{"n":1},"geometry":{"type":"Point","coordinates":[0,0]}}"#}),
            "Fresh default",
            false,
        ),
    ] {
        let selector = if named { json!({"layer":name}) } else { json!({}) };
        let mut diagram = selector.clone();
        diagram["kind"] = json!("pie");
        diagram["fields"] = json!(["n"]);
        diagram["size"] = json!(8);
        let mut moved = selector.clone();
        moved["feature"] = json!(0);
        moved["to"] = Value::Null;
        let call = Call::script(&format!(
            "{first} {params}\nlayer.diagram {diagram}\nlayer.diagramInfo {selector}\nlayer.clearDiagram {selector}\nlabels.move {moved}\nlabels.resetMoved {selector}\nmap.addTiles {{\"url\":\"https://diagram.invalid/{{z}}/{{x}}/{{y}}.png\"}}"
        ));
        let p = pending(&s, &scope, &call);
        s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
        assert!(s.map().layer_by_name(name).unwrap().diagram.is_none(), "the in-run target is created, styled and cleared");
    }
    let unknown =
        Call::script("layer.diagramInfo {\"layer\":\"Never made\"}\nmap.addTiles {\"url\":\"https://diagram.invalid/{z}/{x}/{y}.png\"}");
    assert!(matches!(s.review(&scope, &unknown), Outcome::Refused(why) if why.contains("no earlier output")));
}

/// A displayed task is frozen by content, including its project-only edits; undo restores trust.
#[test]
fn permission_policy_task_content_and_trust_follow_undo() {
    let mut s = world();
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    s.run("task.new", &json!({"name":"Reviewed", "steps":[{"title":"data", "command":"layer.addUrl", "params":{"url":"https://example.invalid/data.geojson"}}]})).unwrap();
    assert!(s.item_trusted("task", "Reviewed"));
    let (scope, _) = assistant(2);
    let call = Call::command("task.run", json!({"task":"Reviewed"}));
    let p = pending(&s, &scope, &call);
    s.run_scoped(scope.clone(), &Call::command("task.editStep", json!({"task":"Reviewed", "step":0, "title":"agent changed"}))).unwrap();
    assert!(!s.item_trusted("task", "Reviewed"));
    assert!(s.run_permitted(scope.clone(), p.allow(), &call).is_err());
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.item_trusted("task", "Reviewed"));
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope, p.allow(), &call).unwrap();
    assert_eq!(s.take_url_requests().len(), 1);
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(!s.item_trusted("task", "Reviewed"), "undo of creation removes its content record");
}

/// A drain counts revoked requests once while preserving a current owner's queued request.
#[test]
fn permission_policy_revoked_url_requests_report_one_counted_notice() {
    let mut s = world();
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    let (revoked, live) = assistant(3401);
    let (current, _) = assistant(3402);
    for (scope, file) in [(&revoked, "revoked-first"), (&current, "current"), (&revoked, "revoked-second")] {
        let call = Call::command("layer.addUrl", json!({"url":format!("https://example.invalid/{file}.geojson")}));
        let p = pending(&s, scope, &call);
        s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    }
    live.0.store(false, Ordering::SeqCst);
    let mark = s.notice_mark();
    let requests = s.take_url_requests();
    assert_eq!(requests.len(), 1, "only the current owner's request survives");
    assert_eq!(requests[0].params()["url"], "https://example.invalid/current.geojson");
    let notices = s.notices_since(mark);
    assert_eq!(notices.len(), 1, "one notice for both revoked requests: {notices:?}");
    assert!(notices[0].error);
    assert_eq!(notices[0].text, "2 deferred URL requests were revoked before they started");
    let id = s.with_scope(current.clone(), |s| s.download_started_for(&requests[0])).unwrap().unwrap();
    assert!(s.download_continues(id).is_ok(), "the survivor retains its original owner");
    assert!(s.queued_authority().is_empty(), "revoked keys were retired and the survivor was claimed");
    let mark = s.notice_mark();
    assert!(s.take_url_requests().is_empty());
    assert!(s.notices_since(mark).is_empty(), "a repeated empty drain is quiet");

    let call = Call::command("layer.addUrl", json!({"url":"https://example.invalid/clean.geojson"}));
    let p = pending(&s, &current, &call);
    s.run_permitted(current, p.allow(), &call).unwrap();
    let mark = s.notice_mark();
    assert_eq!(s.take_url_requests().len(), 1);
    assert!(s.notices_since(mark).is_empty(), "a clean drain is quiet");
}

/// A request has one opaque claim, refuses a foreign completion, and revokes before sending.
#[test]
fn permission_policy_deferred_url_claim_keeps_owner_and_generation() {
    let mut s = world();
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    let (scope, live) = assistant(3);
    let (foreign, _) = assistant(4);
    let url = "https://example.invalid/data.geojson?token=RGSYNTH_pending";
    let call = Call::command("layer.addUrl", json!({"url":url}));
    let p = pending(&s, &scope, &call);
    let reported = s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    let mark = s.notice_mark();
    assert!(s.with_scope(foreign.clone(), |s| s.take_url_requests()).unwrap().is_empty());
    assert!(s.notices_since(mark).is_empty(), "a foreign drain cannot retire the owner's queued request");
    let q = s.take_url_requests().pop().unwrap();
    assert_eq!(q.params()["url"], url, "opaque transport parameters retain the exact private address");
    assert!(!reported.to_string().contains("RGSYNTH_pending"), "the public pending response must omit credentials");
    let logged = s.log.entries().last().unwrap();
    assert_eq!(logged.command, "layer.addUrl");
    assert!(!serde_json::to_string(logged).unwrap().contains("RGSYNTH_pending"), "the session log must omit credentials");
    let id = s.download_started_for(&q).unwrap();
    assert!(s.download_started_for(&q).is_err());
    let still = s.download_still(id).unwrap();
    assert!(still());
    let error = s.run_scoped(foreign, &Call::command("layer.addDownload", json!({"id":id}))).unwrap_err();
    assert!(error.contains("another caller"), "{error}");
    assert!(s.download_continues(id).is_ok(), "foreign call does not consume the owner's result");
    live.0.store(false, Ordering::SeqCst);
    assert!(!still());
    assert!(s.download_continues(id).is_err());
    assert!(!still());
    let (scope, _) = assistant(5);
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope, p.allow(), &call).unwrap();
    let q = s.take_url_requests().pop().unwrap();
    let id = s.download_started_for(&q).unwrap();
    let still = s.download_still(id).unwrap();
    s.run("project.new", &json!({"name":"Replacement"})).unwrap();
    assert!(!still());
    assert!(s.download_continues(id).is_err());
}

/// Native tools check revocation on actual worker entry and again before applying a result.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_native_tool_revocation_and_foreign_finish() {
    let mut s = world();
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    let (scope, live) = assistant(6);
    let run = s
        .with_scope(scope.clone(), |s| crate::cmd::gp::tool_start(s, "buffer", &json!({"input":"Cities", "distance":10})))
        .unwrap()
        .unwrap();
    let id = run.id;
    live.0.store(false, Ordering::SeqCst);
    let before = s.map().layers.len();
    let done = run.run(&rustgis_analysis::control::Control::new());
    assert!(crate::cmd::gp::tool_complete(&mut s, id, done).is_err());
    assert_eq!(s.map().layers.len(), before);
    let (scope, _) = assistant(7);
    let run = s
        .with_scope(scope.clone(), |s| crate::cmd::gp::tool_start(s, "buffer", &json!({"input":"Cities", "distance":10})))
        .unwrap()
        .unwrap();
    let id = run.id;
    let (foreign, _) = assistant(8);
    assert!(
        s.with_scope(foreign.clone(), |s| crate::cmd::gp::plugin_complete(s, id, Err("foreign result".into())))
            .unwrap()
            .unwrap_err()
            .contains("another caller")
    );
    crate::cmd::gp::tool_complete(&mut s, id, run.run(&rustgis_analysis::control::Control::new())).unwrap();
    assert!(s.run_scoped(foreign, &Call::command("gp.finish", json!({"id":id}))).unwrap_err().contains("another caller"));
    s.with_scope(scope, |s| s.run("gp.finish", &json!({"id":id}))).unwrap().unwrap();
    assert_eq!(s.map().layers.len(), before + 1);
    assert!(s.finish_gp(id).is_err());
}

/// Presence is a registry invariant, not evidence that each classification is correct.
#[test]
fn permission_policy_metadata_presence_and_unreviewed_default() {
    for spec in crate::cmd::all() {
        assert!(!spec.effects.contains(crate::policy::Effect::Unreviewed), "{}", spec.id);
        assert!(spec.roles.is_some(), "{}", spec.id);
    }
    let mut s = world();
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    s.registry.commands.push(crate::CommandSpec::new("test.unreviewed", "Unreviewed", "Test", |s, _| {
        s.map_mut().name = "Changed".into();
        Ok(Value::Null)
    }));
    let (scope, _) = assistant(9);
    let call = Call::command("test.unreviewed", Value::Null);
    assert!(matches!(s.review(&scope, &call), Outcome::Refused(_)));
    assert!(s.run_scoped(scope, &call).is_err());
    assert_ne!(s.map().name, "Changed");
    s.run("test.unreviewed", &Value::Null).unwrap();
    let (scope, _) = assistant(13);
    let before = s.map().name.clone();
    let huge = Call::script(&"layer.addData {\"path\":\"never read.gpkg\"}\n".repeat(10_000));
    assert!(matches!(s.review(&scope, &huge), Outcome::Refused(e) if e.contains("too large")));
    assert_eq!(s.map().name, before);
}

/// Actual execution families retain additive reach and every plugin call asks separately.
#[test]
fn permission_policy_effects_and_real_plugin_approval() {
    #[cfg(not(target_arch = "wasm32"))]
    if isolate_process_test("tests::permission_policy::permission_policy_effects_and_real_plugin_approval") {
        return;
    }
    use crate::policy::{Effect, command_effects};
    let mut s = world();
    for id in ["layer.addSql", "cube.reduce", "cube.trend"] {
        let effects = command_effects(s.registry.get(id).unwrap(), &Value::Null);
        assert!(effects.contains(Effect::Files) && effects.contains(Effect::Network), "{id}: {effects:?}");
    }
    assert!(command_effects(s.registry.get("project.open").unwrap(), &Value::Null).contains(Effect::Replaces));
    assert!(command_effects(s.registry.get("notebook.run").unwrap(), &Value::Null).contains(Effect::Files));
    assert!(command_effects(s.registry.get("gp.run").unwrap(), &json!({"tool":"buffer"})).contains(Effect::Edits));
    let mut pkg = rustgis_plugins::Package::from_zip(&rustgis_plugins::testing::package("eng-policy-approval", &["edit"])).unwrap();
    pkg.manifest.tools.iter_mut().find(|t| t.id == "x").unwrap().name = "Permission selection".into();
    rustgis_plugins::load(pkg, "test").unwrap();
    let tool = "plugin:eng-policy-approval/x";
    let effects = command_effects(s.registry.get("gp.run").unwrap(), &json!({"tool":tool,"params":{"input":"Cities"}}));
    assert!(effects.contains(Effect::Edits) && effects.contains(Effect::Plugin));
    let (scope, _) = assistant(14);
    for name in [tool, "Permission selection"] {
        let call = Call::tool(name, json!({"input":"Cities"}));
        let p = pending(&s, &scope, &call);
        assert!(!p.review().remember);
        s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
        assert_eq!(s.map().layer_by_name("Cities").unwrap().selection.len(), 2);
        assert!(!pending(&s, &scope, &call).review().remember);
    }
    rustgis_plugins::unload("eng-policy-approval");
}

/// The newly upstreamed native classification route carries its owner through work and `ran`.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_native_classification_keeps_scope_through_completion() {
    use crate::cmd::workflows::{raster_land, raster_start};
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName":"scene.tif", "data":two_class_b64()})).unwrap();
    let params = json!({"layer":"scene", "classes":3});
    let (scope, live) = assistant(10);
    let run = s.with_scope(scope, |s| raster_start(s, "raster.classify", &params)).unwrap().unwrap();
    live.0.store(false, Ordering::SeqCst);
    assert!(run.run(&rustgis_analysis::control::Control::new()).err().unwrap().contains("revoked"));
    let (scope, _) = assistant(11);
    let run = s.with_scope(scope.clone(), |s| raster_start(s, "raster.classify", &params)).unwrap().unwrap();
    let id = run.id;
    let made = run.run(&rustgis_analysis::control::Control::new()).unwrap();
    raster_land(&mut s, id, made).unwrap();
    let (foreign, _) = assistant(12);
    let finish = Call::command("raster.classify", json!({"ran":id}));
    assert!(s.run_scoped(foreign, &finish).unwrap_err().contains("another caller"));
    s.run_scoped(scope.clone(), &finish).unwrap();
    assert!(s.run_scoped(scope, &finish).is_err());
}

/// Background preparers reject non-object params before they can create a raster completion.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_native_raster_rejects_non_object_params_before_queuing() {
    use crate::cmd::workflows::{raster_start, runs_in_background};
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName":"scene.tif", "data":two_class_b64()})).unwrap();
    let source = s.map().layer_by_name("scene").unwrap().id;
    let commands = s.registry.commands.iter().filter(|c| runs_in_background(c.id).is_some()).map(|c| c.id).collect::<Vec<_>>();
    assert!(!commands.is_empty());
    for command in commands {
        s.run("layer.select", &json!({"layer":source})).unwrap();
        for params in [json!(7), json!([]), json!("defaults"), json!(false), Value::Null] {
            assert!(raster_start(&mut s, command, &params).is_err(), "{command}: {params}");
            assert!(s.raster_runs.is_empty(), "{command}: a rejected request queued no worker");
        }
    }
}

/// The new export worker cannot start after revocation or land in a replacement project;
/// native snapshots still report after the person switches maps.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_export_worker_keeps_its_initiating_owner() {
    use crate::cmd::exports::{export_land, export_start};
    use crate::policy::AgentAccess;
    use rustgis_analysis::control::Control;
    let mut s = world();
    let dir = std::env::temp_dir().join(format!("rustgis-export-owner-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |name: &str| dir.join(name);
    let params = |name: &str| json!({"layer":"Cities", "path":path(name)});
    let (assistant, _) = assistant(129);
    assert!(s.with_scope(assistant, |s| export_start(s, "share.exportLayer", &params("unapproved.geojson"))).unwrap().is_err());
    let live = Arc::new(Live(AtomicBool::new(true)));
    let scope = Scope::agent(AgentAccess::Allow, false, "export-owner", None, live.clone());
    let run = s.with_scope(scope.clone(), |s| export_start(s, "share.exportLayer", &params("revoked.geojson"))).unwrap().unwrap();
    live.0.store(false, Ordering::SeqCst);
    assert!(std::thread::spawn(move || run.run(&Control::new())).join().unwrap().is_err());
    assert!(!path("revoked.geojson").exists());
    live.0.store(true, Ordering::SeqCst);
    let run = s.with_scope(scope, |s| export_start(s, "share.exportLayer", &params("completed.geojson"))).unwrap().unwrap();
    let started = run.started();
    let wrote = run.run(&Control::new());
    assert!(wrote.is_ok(), "the worker committed its file: {:?}", wrote.as_ref().err());
    s.run("project.new", &Value::Null).unwrap();
    let replacement = serde_json::to_value(&s.project).unwrap();
    let mark = s.notice_mark();
    let error = export_land(&mut s, started, wrote).unwrap_err();
    assert!(error.contains("replaced project"));
    assert!(error.contains(&path("completed.geojson").display().to_string()), "the completed file is reported: {error}");
    let errors: Vec<_> = s.notices_since(mark).iter().filter(|n| n.error).map(|n| n.text.as_str()).collect();
    assert_eq!(errors, [error.as_str()], "the completed file and delivery refusal are reported once");
    assert_eq!(serde_json::to_value(&s.project).unwrap(), replacement, "delivery leaves the replacement project alone");
    assert!(path("completed.geojson").exists(), "a completed write is not undone");
    // Drawn exports can also carry a failed write as an owned result: only a committed file
    // may be reported when delivery is refused, and neither case may run its finalizer.
    for committed in [false, true] {
        let output = if committed { path("completed.png") } else { path("missing").join("not-written.png") };
        let run = export_start(&mut s, "share.exportMap", &json!({"path":output,"width":64,"height":48})).unwrap();
        let started = run.started();
        let wrote = run.run(&Control::new());
        assert!(wrote.is_ok(), "drawing delivers its write outcome: {:?}", wrote.as_ref().err());
        assert_eq!(output.exists(), committed);
        s.run("project.new", &Value::Null).unwrap();
        let replacement = serde_json::to_value(&s.project).unwrap();
        let mark = s.notice_mark();
        let error = export_land(&mut s, started, wrote).unwrap_err();
        assert!(error.contains("replaced project"), "{error}");
        assert_eq!(error.contains("Exported "), committed, "{error}");
        if committed {
            assert!(error.contains(&output.display().to_string()), "the committed drawing is reported: {error}");
        }
        let notices: Vec<_> = s.notices_since(mark).iter().map(|notice| (notice.error, notice.text.as_str())).collect();
        assert_eq!(notices, [(true, error.as_str())], "only the refusal is delivered; its finalizer stays blocked");
        assert_eq!(serde_json::to_value(&s.project).unwrap(), replacement);
    }
    s.run("project.sample", &Value::Null).unwrap();
    let run = export_start(&mut s, "share.exportLayer", &params("native.geojson")).unwrap();
    let started = run.started();
    s.run("map.new", &json!({"name":"Other"})).unwrap();
    let wrote = run.run(&Control::new());
    let count = s.map().layers.len();
    assert_eq!(export_land(&mut s, started, wrote).unwrap()["path"], json!(path("native.geojson")));
    assert_eq!(s.map().layers.len(), count);
    // A completion cannot borrow another invocation's command/log envelope.
    s.run("map.activate", &json!({"name":s.project.maps[0].name.clone()})).unwrap();
    let a = export_start(&mut s, "share.exportLayer", &params("owner-a.geojson")).unwrap();
    let b = export_start(&mut s, "share.exportLayer", &params("owner-b.geojson")).unwrap();
    assert!(export_land(&mut s, b.started(), a.run(&Control::new())).unwrap_err().contains("another run"));
    std::fs::remove_dir_all(dir).unwrap();
}

/// The delayed UI token inherits a scoped caller and cannot escape a handler's transaction.
#[test]
fn permission_policy_prepared_plugin_keeps_scope_and_pre_handler_boundary() {
    use crate::policy::AgentAccess;
    let mut s = world();
    plugin("eng-prepared-owner", &["edit"]);
    let id = "plugin:eng-prepared-owner/f";
    let params = json!({"op":"map.rename", "params":{"name":"Queued"}});
    let before = s.map().name.clone();
    let live = Arc::new(Live(AtomicBool::new(true)));
    let scope = Scope::agent(AgentAccess::Allow, false, "prepared-owner", None, live.clone());
    let prepared = s.with_scope(scope, |s| s.prepare_command(id, &params)).unwrap().unwrap();
    live.0.store(false, Ordering::SeqCst);
    assert!(s.run_prepared(prepared).unwrap_err().contains("revoked"));
    assert!(!s.leave_downloads, "a refused prepared command restores the caller's download mode");
    assert_eq!(s.map().name, before);
    let mut preparation_error = None;
    let transaction = s.guarded("Transaction", true, |s| {
        preparation_error = s.prepare_command(id, &params).err();
        Err("a later transaction failure".into())
    });
    assert_eq!(transaction.unwrap_err(), "a later transaction failure");
    assert!(preparation_error.as_deref().is_some_and(|e| e.contains("before handler")), "{preparation_error:?}");
    assert_eq!(s.map().name, before);
    rustgis_plugins::unload("eng-prepared-owner");
}

/// Public creation returns a numeric notebook handle; trust follows its exact content and undo.
#[test]
fn permission_policy_notebook_creator_edit_and_import_provenance() {
    let mut s = world();
    let made = s.run("notebook.new", &json!({"name":"Local"})).unwrap();
    assert!(made["notebook"].is_number());
    assert!(s.item_trusted("notebook", "Local"));
    s.run("notebook.editCell", &json!({"notebook":"Local","cell":1,"source":"map.rename {\"name\":\"Notebook map\"}"})).unwrap();
    assert!(s.item_trusted("notebook", "Local"));
    let (scope, _) = assistant(101);
    let call = Call::command("notebook.run", json!({"notebook":"Local","cell":1}));
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    assert_eq!(s.map().name, "Notebook map");
    plugin("policy-notebook", &["edit", "files"]);
    forward(
        &mut s,
        "policy-notebook",
        "notebook.editCell",
        json!({"notebook":"Local","cell":1,"source":"map.rename {\"name\":\"plugin edit\"}"}),
    )
    .unwrap();
    assert!(!s.item_trusted("notebook", "Local"));
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.item_trusted("notebook", "Local"));
    let text = json!({"name":"Imported","cells":[{"kind":"code","source":"map.zoomFull {}"}]}).to_string();
    s.run("notebook.open", &json!({"text":text})).unwrap();
    s.run("notebook.editCell", &json!({"notebook":"Imported","cell":0,"source":"map.zoomIn {}"})).unwrap();
    assert!(!s.item_trusted("notebook", "Imported"));
    rustgis_plugins::unload("policy-notebook");
}

/// Plans accept their own new outputs/renames while refusing unknown producers and stale cards.
#[test]
fn permission_policy_ordered_outputs_rename_and_default_cursor() {
    let mut s = world();
    let (scope, _) = assistant(102);
    s.run(
        "task.new",
        &json!({"name":"Make then use","steps":[
            {"title":"Step","command":"gp.run","params":{"tool":"buffer","params":{"input":"Cities","distance":10,"output":"Fresh"}}},
            {"title":"Step","command":"layer.rename","params":{"layer":"Fresh","name":"Renamed"}},
            {"title":"Step","command":"layer.visibility","params":{"visible":false}}
        ]}),
    )
    .unwrap();
    let call = Call::command("task.run", json!({"task":"Make then use"}));
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    assert!(!s.map().layer_by_name("Renamed").unwrap().visible);
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.map().layer_by_name("Renamed").is_none());
    s.run(
        "layer.addDataText",
        &json!({"name":"Numeric","fileName":"Numeric.geojson","text":json!({"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"v":2},"geometry":{"type":"Point","coordinates":[0,0]}},
        {"type":"Feature","properties":{"v":4},"geometry":{"type":"Point","coordinates":[1,1]}}
    ]}).to_string()}),
    )
    .unwrap();
    let origin = s.map().name.clone();
    // gp.run retains its existing nonempty-map enabled check; the input below is still
    // the produced project-global table, with no same-named layer in this destination.
    s.run("map.duplicate", &json!({"name":"Table destination"})).unwrap();
    s.run("map.activate", &json!({"name":origin})).unwrap();
    s.run("task.new", &json!({"name":"Produce table then use","steps":[
        {"title":"Statistics","command":"gp.run","params":{"tool":"summaryStatistics","params":{"input":"Numeric","field":"v","output":"Numeric_Statistics"}}},
        {"title":"Switch","command":"map.activate","params":{"name":"Table destination"}},
        {"title":"Use global table","command":"gp.run","params":{"tool":"summaryStatistics","params":{"input":"Numeric_Statistics","field":"FREQUENCY","output":"Numeric_Statistics_Statistics"}}}
    ]})).unwrap();
    let table_call = Call::command("task.run", json!({"task":"Produce table then use"}));
    let approved = pending(&s, &scope, &table_call);
    s.run_permitted(scope.clone(), approved.allow(), &table_call).unwrap();
    assert!(s.project.tables.iter().any(|t| t.dataset.name == "Numeric_Statistics_Statistics"));
    assert!(s.map().layer_by_name("Numeric_Statistics").is_none());
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().name, origin);
    assert!(!s.project.tables.iter().any(|t| t.dataset.name == "Numeric_Statistics"));
    let missing = Call::script("layer.rename {\"layer\":\"Absent\",\"name\":\"No\"}\nlayer.addData {\"path\":\"never.gpkg\"}");
    assert!(matches!(s.review(&scope, &missing), Outcome::Refused(_)));
    s.run("layer.select", &json!({"layer":"Cities"})).unwrap();
    s.run(
        "task.new",
        &json!({"name":"Rename existing","steps":[
            {"title":"Step","command":"layer.rename","params":{"layer":"Cities","name":"Same id"}},
            {"title":"Step","command":"layer.visibility","params":{"visible":false}}
        ]}),
    )
    .unwrap();
    let call = Call::command("task.run", json!({"task":"Rename existing"}));
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    assert!(!s.map().layer_by_name("Same id").unwrap().visible);
    s.run("edit.undo", &Value::Null).unwrap();
    s.run(
        "task.new",
        &json!({"name":"Remove default","steps":[
            {"title":"Step","command":"layer.remove","params":{"layer":"Cities"}},
            {"title":"Step","command":"layer.visibility","params":{"visible":false}}
        ]}),
    )
    .unwrap();
    let call = Call::command("task.run", json!({"task":"Remove default"}));
    let p = pending(&s, &scope, &call);
    assert!(s.run_permitted(scope, p.allow(), &call).is_err());
    assert!(s.map().layer_by_name("Cities").is_some(), "transaction rolls back the removal");
    // Layer ids are per-map: activation resolves later defaults on the destination, while a
    // changed destination before approval use never inherits a same-numbered source layer.
    let original = s.map().name.clone();
    let original_map = s.map().instance_id();
    let shared_id = s.map().layers.first().unwrap().id;
    s.run("map.duplicate", &json!({"name":"Destination"})).unwrap();
    let destination = s.map().instance_id();
    assert_ne!(original_map, destination);
    assert_eq!(s.map().layers.first().unwrap().id, shared_id);
    s.run("map.activate", &json!({"name":original})).unwrap();
    s.run(
        "task.new",
        &json!({"name":"Activate then edit","steps":[
            {"title":"Step","command":"map.activate","params":{"name":"Destination"}},
            {"title":"Step","command":"layer.visibility","params":{"visible":false}}
        ]}),
    )
    .unwrap();
    let call = Call::command("task.run", json!({"task":"Activate then edit"}));
    let (scope, _) = assistant(122);
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    assert_eq!(s.map().instance_id(), destination);
    assert!(!s.map().layer(shared_id).unwrap().visible);
    assert!(s.project.maps.iter().find(|m| m.instance_id() == original_map).unwrap().layer(shared_id).unwrap().visible);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().instance_id(), original_map);
    let p = pending(&s, &scope, &call);
    let dest = s.project.maps.iter_mut().find(|m| m.instance_id() == destination).unwrap();
    dest.layer_mut(shared_id).unwrap().renew_instance_id();
    assert!(s.run_permitted(scope, p.allow(), &call).unwrap_err().contains("changed"));
    assert_eq!(s.map().instance_id(), original_map);
    // Renaming keeps default identities, but later named-map choices cannot use the old namespace.
    // The executor makes an existing destination name unique; planning must not bind that old map.
    for renamed in ["Approved renamed", "Destination"] {
        let names = s.project.maps.iter().map(|m| m.name.clone()).collect::<Vec<_>>();
        let call = Call::script(&format!(
            "map.rename {}\nmap.activate {}\nshare.exportMap {{}}",
            json!({"name":renamed}),
            json!({"name":original})
        ));
        let (scope, _) = assistant(128);
        assert!(matches!(s.review(&scope, &call), Outcome::Refused(why) if why.contains("namespace")));
        assert_eq!(s.project.maps.iter().map(|m| m.name.clone()).collect::<Vec<_>>(), names);
    }
    // Planning streamed reach must use the borrowed destination, not the initial in-memory map.
    let mut source = rustgis_map::source::SourceLayer::default();
    source.path = Some("https://example.invalid/destination.fgb".into());
    let dest = s.project.maps.iter_mut().find(|m| m.instance_id() == destination).unwrap();
    let city = dest.layer_by_name("Cities").unwrap().id;
    dest.layer_mut(city).unwrap().source = Some(source);
    s.run(
        "task.new",
        &json!({"name":"Destination stream","steps":[
            {"title":"Activate","command":"map.activate","params":{"name":"Destination"}},
            {"title":"Buffer","command":"gp.run","params":{"tool":"buffer","params":{"input":"Cities","distance":10}}}
        ]}),
    )
    .unwrap();
    let (scope, live) = assistant(126);
    let streamed = Call::command("task.run", json!({"task":"Destination stream"}));
    let approved = pending(&s, &scope, &streamed);
    assert!(approved.review().steps.iter().any(|line| line.starts_with("buffer ")), "the destination's streamed tool is an external step");
    live.0.store(false, Ordering::SeqCst);
    assert!(s.run_permitted(scope, approved.allow(), &streamed).is_err());
    assert_eq!(s.map().instance_id(), original_map);
}

/// External dataset/sheet names are frozen with their origin without opening it for review.
#[test]
fn permission_policy_external_selectors_and_dataset_precedence() {
    let mut s = world();
    let (scope, _) = assistant(103);
    for (id, origin) in [("layer.addData", "path"), ("layer.addDatabase", "database"), ("info.fileColumns", "path")] {
        for name in ["absent dataset", ""] {
            let call = Call::command(id, json!({origin:"never-opened.gpkg","layer":name}));
            assert!(pending(&s, &scope, &call).review().why.contains("files"));
        }
    }
    let ds = s.map().layer_by_name("Cities").unwrap().dataset.clone();
    let table = s.project.add_table(ds);
    s.run("task.new", &json!({"name":"Colliding table","steps":[{"title":"Step","command":"gp.run","params":{"tool":"buffer","params":{"input":"Cities","distance":10}}}]})).unwrap();
    let call = Call::command("task.run", json!({"task":"Colliding table"}));
    let p = pending(&s, &scope, &call);
    s.project.tables.iter_mut().find(|t| t.id == table).unwrap().dataset.source = Some("changed unrelated table".into());
    // Replanning conservatively notices project changes, but the tool's actual target is the layer.
    let p2 = pending(&s, &scope, &call);
    assert_eq!(p.review().what, p2.review().what);
    s.run_permitted(scope, p2.allow(), &call).unwrap();
    assert!(s.map().layers.len() > 3);
}

/// A same-public-URL account replacement invalidates a held card and its owned send predicate.
#[test]
fn permission_policy_exact_runtime_account_rotation() {
    // Account stores are process-wide: isolate mutation from every parallel plan reader.
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var_os("RUSTGIS_POLICY_ACCOUNT_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::permission_policy::permission_policy_exact_runtime_account_rotation", "--nocapture"])
            .env("RUSTGIS_POLICY_ACCOUNT_CHILD", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }

    use crate::render::rustgis_tiles::earthdata;
    let mut s = world();
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    let (scope, _) = assistant(104);
    earthdata::set_token(Some("synthetic-policy-first")).unwrap();
    let call = Call::command("layer.addUrl", json!({"url":"https://example.invalid/unchanged.geojson"}));
    let p = pending(&s, &scope, &call);
    earthdata::set_token(Some("synthetic-policy-second")).unwrap();
    assert!(s.run_permitted(scope.clone(), p.allow(), &call).unwrap_err().contains("changed"));
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope, p.allow(), &call).unwrap();
    let request = s.take_url_requests().pop().unwrap();
    let id = s.download_started_for(&request).unwrap();
    let still = s.download_still(id).unwrap();
    assert!(still());
    earthdata::set_token(Some("synthetic-policy-third")).unwrap();
    assert!(!still());
    assert!(s.download_continues(id).is_err());
    earthdata::set_token(None).unwrap();
    use crate::render::rustgis_tiles::arcgis_auth;
    let url = "https://policy-identity.invalid/server/rest/services/Data/FeatureServer/0";
    let root = arcgis_auth::add_token(url, "synthetic-policy-server-first").unwrap();
    let (scope, _) = assistant(125);
    let call = Call::command("layer.addUrl", json!({"url":url}));
    let p = pending(&s, &scope, &call);
    arcgis_auth::add_token(url, "synthetic-policy-server-second").unwrap();
    assert!(s.run_permitted(scope, p.allow(), &call).unwrap_err().contains("changed"));
    arcgis_auth::sign_out(Some(root.trim_end_matches('/')));
}

/// Foreign in-place completions cannot borrow/consume ownership; omitted options retain defaults.
#[test]
fn permission_policy_download_refinements_rollback_and_abandonment() {
    let mut s = world();
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    let (scope, _) = assistant(105);
    let (foreign, _) = assistant(106);
    let call = Call::command(
        "layer.addUrl",
        json!({"url":"https://example.invalid/data.csv","name":"Frozen","x":"east","y":"north","zoom":false}),
    );
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    let q = s.take_url_requests().pop().unwrap();
    let id = s.download_started_for(&q).unwrap();
    let parameters = q.params().clone();
    assert!(s.with_scope(foreign, |s| s.add_in_place_for(id, &parameters)).unwrap().unwrap_err().contains("another caller"));
    assert!(s.download_still(id).unwrap()());
    assert!(s.download_landed(id, b"east,north\n1,2\n".to_vec()));
    s.complete_download(id, &json!({"id":id})).unwrap();
    assert_eq!(s.map().layer_by_name("Frozen").unwrap().dataset.features.len(), 1);
    assert!(s.complete_download(id, &json!({"id":id})).is_err());
    assert!(s.run("layer.addUrl", &json!({"url":"https://example.invalid/project.json","geolibreProject":true})).is_err());
    for i in 0..32 {
        let failed = format!("layer.addUrl {{\"url\":\"https://example.invalid/rollback-{i}.csv\"}}\nmissing.command {{}}");
        assert!(s.run("script.run", &json!({"script":failed})).is_err());
        assert!(s.take_url_requests().is_empty());
    }
    s.request_url(&json!({"url":"https://example.invalid/earlier.csv"})).unwrap();
    let earlier = s.take_url_requests().pop().unwrap();
    let mut taken = None;
    assert!(
        s.guarded("Retained failed request", false, |s| {
            s.request_url(&json!({"url":"https://example.invalid/failed-retained.csv"}))?;
            taken = s.take_url_requests().pop();
            Err("fail after the owner took the request".into())
        })
        .is_err()
    );
    assert!(s.download_started_for(&taken.unwrap()).is_err());
    let id = s.download_started_for(&earlier).unwrap();
    assert!(s.download_continues(id).is_ok());
    s.download_failed(id);
    let mut held = Vec::new();
    for i in 0..crate::MAX_URL_REQUESTS {
        s.request_url(&json!({"url":format!("https://example.invalid/held-{i}.csv")})).unwrap();
        held.extend(s.take_url_requests());
    }
    assert!(s.request_url(&json!({"url":"https://example.invalid/over.csv"})).is_err());
    drop(held);
    s.request_url(&json!({"url":"https://example.invalid/fresh.csv"})).unwrap();
    assert_eq!(s.take_url_requests().len(), 1);
}

/// Async apply retains its initiating actor so only that agent can undo the new step.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_async_actor_and_streamed_preparation() {
    use crate::policy::AgentAccess;
    let mut s = world();
    let live = Arc::new(Live(AtomicBool::new(true)));
    let scope = Scope::agent(AgentAccess::Allow, true, "actor-run", Some("rightful".into()), live);
    let run = s
        .with_scope(scope.clone(), |s| crate::cmd::gp::tool_start(s, "buffer", &json!({"input":"Cities","distance":10})))
        .unwrap()
        .unwrap();
    let id = run.id;
    crate::cmd::gp::tool_complete(&mut s, id, run.run(&rustgis_analysis::control::Control::new())).unwrap();
    s.finish_gp(id).unwrap();
    assert_eq!(s.undo_actor(), Some(Some("rightful")));
    assert_eq!(s.actor, None);
    let foreign = Scope::agent(AgentAccess::Allow, true, "foreign", Some("foreign".into()), Arc::new(Live(AtomicBool::new(true))));
    assert!(s.run_scoped(foreign, &Call::command("edit.undo", Value::Null)).is_err());
    s.run_scoped(scope, &Call::command("edit.undo", Value::Null)).unwrap();
    assert_eq!(s.actor, None);
}

/// Hold the real streamed-input worker before its range send, then revoke its initiating scope.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_streamed_tool_worker_refuses_before_remote_input_send() {
    use std::sync::{Mutex, mpsc};
    struct Held {
        armed: AtomicBool,
        live: AtomicBool,
        polls: std::sync::atomic::AtomicUsize,
        entered: mpsc::SyncSender<()>,
        released: Mutex<mpsc::Receiver<()>>,
    }
    impl Revocation for Held {
        fn allows(&self, _: Effects) -> bool {
            if self.armed.load(Ordering::SeqCst) && self.polls.fetch_add(1, Ordering::SeqCst) == 1 {
                self.entered.send(()).unwrap();
                self.released.lock().unwrap().recv().unwrap();
            }
            self.live.load(Ordering::SeqCst)
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/input.fgb", listener.local_addr().unwrap());
    let mut s = world();
    let cities = s.map().layer_by_name("Cities").unwrap().id;
    let mut source = rustgis_map::source::SourceLayer::default();
    source.path = Some(url);
    s.map_mut().layer_mut(cities).unwrap().source = Some(source);
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let live = Arc::new(Held {
        armed: AtomicBool::new(false),
        live: AtomicBool::new(true),
        polls: 0.into(),
        entered,
        released: Mutex::new(released),
    });
    let scope = Scope::agent(crate::policy::AgentAccess::Allow, false, "streamed-worker", None, live.clone());
    let run = s.with_scope(scope, |s| crate::cmd::gp::tool_start(s, "buffer", &json!({"input":"Cities","distance":10}))).unwrap().unwrap();
    let id = run.id;
    live.armed.store(true, Ordering::SeqCst);
    let worker = std::thread::spawn(move || run.run(&rustgis_analysis::control::Control::new()));
    let entered = waiting.recv_timeout(std::time::Duration::from_secs(3));
    live.live.store(false, Ordering::SeqCst);
    let _ = release.send(());
    assert!(entered.is_ok(), "the actual input preparation must inherit its worker's owned predicate");
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    assert!(crate::cmd::gp::tool_complete(&mut s, id, worker.join().unwrap()).is_err());
}

/// The real rustgis-source thread must carry the initiating context through a held source read.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_source_worker_checks_follow_on_send_and_landing() {
    use rustgis_data::source::{Batch, FeatureSource, Request, SourceInfo};
    use std::{
        io::Write,
        sync::{Mutex, mpsc},
        time::Duration,
    };
    struct HeldSource {
        inner: Box<dyn FeatureSource>,
        first: AtomicBool,
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        url: String,
    }
    impl FeatureSource for HeldSource {
        fn info(&self) -> &SourceInfo {
            self.inner.info()
        }
        fn read(&self, req: &Request) -> rustgis_data::Result<Batch> {
            if self.first.swap(false, Ordering::SeqCst) {
                assert_eq!(std::thread::current().name(), Some("rustgis-source"));
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                crate::render::rustgis_tiles::cache::download_blocking_within(&self.url, 64, Duration::from_secs(2))
                    .map_err(rustgis_data::DataError::Format)?;
            }
            self.inner.read(req)
        }
    }
    for revoke in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/follow-on", listener.local_addr().unwrap());
        let mut s = world();
        let bytes = Arc::new(rustgis_data::source::fgb::write(&s.map().layer_by_name("Cities").unwrap().dataset).unwrap());
        let inner = rustgis_data::source::open_shared("held.fgb", bytes.clone(), None).unwrap();
        let (entered, waiting) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let mut source = rustgis_map::source::SourceLayer::from_bytes(
            "held.fgb",
            bytes,
            None,
            Box::new(HeldSource { inner, first: AtomicBool::new(true), entered, release: Mutex::new(released), url }),
        );
        let (scope, live) = assistant(120 + u64::from(revoke));
        let call = Call::adapter("source export", Effects::NONE.with(crate::policy::Effect::Files), Value::Null);
        let p = pending(&s, &scope, &call);
        let completion = s.defer(scope, &call, Some(p.allow())).unwrap();
        s.finish_deferred(completion, |_| source.start(rustgis_geom::Extent::new(-180.0, -85.0, 180.0, 85.0), false)).unwrap();
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        live.0.store(!revoke, Ordering::SeqCst);
        release.send(()).unwrap();
        if !revoke {
            let started = std::time::Instant::now();
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && started.elapsed() < Duration::from_secs(3) => {
                        std::thread::yield_now()
                    }
                    other => panic!("the allowed actual worker must send: {other:?}"),
                }
            };
            peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
            assert!(matches!(source.poll(true), rustgis_map::source::Polled::Done(_, _)));
        } else {
            assert!(matches!(source.poll(true), rustgis_map::source::Polled::Failed(_)));
            assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
            assert_eq!(source.window().unwrap().kept, 0);
        }
        assert!(rustgis_data::source::remote::current_still().is_none(), "synchronous reentry restores the caller");
    }
}

/// A notebook cannot change its frozen code while running; model wires and cycle refusal use real commands.
#[test]
fn permission_policy_notebook_self_edit_model_outputs_and_intersections() {
    let mut s = world();
    s.run("notebook.new", &json!({"name":"Self"})).unwrap();
    let script = "layer.visibility {\"layer\":\"Cities\",\"visible\":false}\nnotebook.editCell {\"notebook\":\"Self\",\"cell\":1,\"source\":\"map.zoomIn {}\"}\nmap.zoomOut {}";
    s.run("notebook.editCell", &json!({"notebook":"Self","cell":1,"source":script})).unwrap();
    let old = s.map().layer_by_name("Cities").unwrap().visible;
    let (scope, _) = assistant(122);
    let call = Call::command("notebook.run", json!({"notebook":"Self","cell":1}));
    let p = pending(&s, &scope, &call);
    assert!(s.run_permitted(scope.clone(), p.allow(), &call).is_err());
    assert_eq!(s.map().layer_by_name("Cities").unwrap().visible, old);
    assert!(s.project.notebooks.iter().find(|n| n.name == "Self").unwrap().cells[1].source.contains("notebook.editCell"));
    let graph = json!({"nodes":[
        {"id":"i","kind":"input","layer":"Cities"},
        {"id":"b","kind":"tool","tool":"buffer","params":{"distance":10}},
        {"id":"d","kind":"tool","tool":"dissolve"},
        {"id":"o","kind":"output","name":"Policy zones"}],
        "edges":[{"from":"i","fromPort":"out","to":"b","toPort":"input"},{"from":"b","fromPort":"out","to":"d","toPort":"input"},{"from":"d","fromPort":"out","to":"o","toPort":"in"}]});
    s.run("model.new", &json!({"name":"Wired","graph":graph})).unwrap();
    let call = Call::command("model.run", json!({"name":"Wired"}));
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    assert!(s.map().layer_by_name("Policy zones").is_some());
    let mut cycle = graph.clone();
    cycle["edges"].as_array_mut().unwrap().push(json!({"from":"d","fromPort":"out","to":"b","toPort":"input"}));
    s.run("model.new", &json!({"name":"Cycle","graph":cycle})).unwrap();
    assert!(matches!(
        s.review(&scope, &Call::command("model.run", json!({"name":"Cycle"}))),
        Outcome::Refused(reason) if reason.contains("loop")
    ));
    s.run(
        "task.new",
        &json!({"name":"Imported external","imported":true,"steps":[{"title":"Step","command":"layer.addData","params":{"path":"never-read.gpkg"}}]}),
    )
    .unwrap();
    let full = Scope::agent(crate::policy::AgentAccess::Full, false, "full", None, Arc::new(Live(AtomicBool::new(true))));
    assert!(s.run_scoped(full, &Call::command("task.run", json!({"task":"Imported external"}))).is_err());
    let nested = s.with_scope(scope.clone(), |s| s.run_scoped(Scope::peer(), &Call::command("map.zoomIn", Value::Null))).unwrap();
    assert!(nested.is_err(), "nested entry cannot widen or switch owner");
    let external = Call::command("layer.addData", json!({"path":"never-read.gpkg"})).with_request("first");
    let p = pending(&s, &scope, &external);
    assert!(s.run_permitted(scope, p.allow(), &external.with_request("second")).is_err());
}

/// State-dependent declarations follow the source actually used, including saved/default locator paths.
#[test]
fn permission_policy_effects_follow_saved_locator_streamed_and_time_sources() {
    let mut s = world();
    let (scope, _) = assistant(123);
    for p in [json!({"locator":"bare.rgisloc","text":"Austin"}), json!({"text":"Austin"})] {
        let outcome = s.review(&scope, &Call::command("locator.find", p));
        assert!(!matches!(outcome, Outcome::Allowed));
    }
    let id = s.map().layer_by_name("Cities").unwrap().id;
    let mut source = rustgis_map::source::SourceLayer::default();
    source.path = Some("https://example.invalid/data.fgb".into());
    s.map_mut().layer_mut(id).unwrap().source = Some(source);
    assert!(matches!(s.review(&scope, &Call::command("layer.loadAll", json!({"layer":"Cities"}))), Outcome::NeedsApproval(_)));
    s.run("layer.addDataBytes", &json!({"fileName":"scene.tif","data":two_class_b64()})).unwrap();
    s.map_mut().layers.iter_mut().find(|l| l.name == "scene").unwrap().raster.as_mut().unwrap().time_zarr =
        Some(rustgis_map::raster::ZarrTime {
            store: "https://example.invalid/cube.zarr".into(),
            variable: "v".into(),
            dim: "time".into(),
            dates: vec![0., 1.],
            ..Default::default()
        });
    assert!(matches!(s.review(&scope, &Call::command("time.step", json!({"direction":"next"}))), Outcome::NeedsApproval(_)));
}

/// Cross-map frames and detached clipboards bind their actual owners; managed shares bind saved paths.
#[test]
fn permission_policy_stored_cross_map_clipboard_and_managed_share_targets() {
    let mut s = world();
    s.leave_downloads = true;
    let original = s.map().name.clone();
    s.run("map.duplicate", &json!({"name":"Other"})).unwrap();
    s.run("map.activate", &json!({"name":original})).unwrap();
    s.run("layout.new", &json!({"name":"Framed","template":"blank"})).unwrap();
    let frame = s.run("layout.addFrame", &json!({"map":"Other"})).unwrap()["element"].clone();
    s.run(
        "task.new",
        &json!({"name":"Frame export","steps":[
            {"title":"Step","command":"layout.frameZoom","params":{"element":frame,"to":"layer","layer":"Cities"}},
            {"title":"Step","command":"layer.addUrl","params":{"url":"https://example.invalid/no-send.csv"}}
        ]}),
    )
    .unwrap();
    let (scope, _) = assistant(124);
    let call = Call::command("task.run", json!({"task":"Frame export"}));
    let p = pending(&s, &scope, &call);
    s.run("map.activate", &json!({"name":"Other"})).unwrap();
    s.run("layer.remove", &json!({"layer":"Cities"})).unwrap();
    s.run("layer.addDataText", &json!({"name":"Cities","fileName":"Cities.geojson","text":"{\"type\":\"Point\",\"coordinates\":[0,0]}"}))
        .unwrap();
    s.run("map.activate", &json!({"name":original})).unwrap();
    assert!(s.run_permitted(scope.clone(), p.allow(), &call).unwrap_err().contains("changed"));
    assert!(s.take_url_requests().is_empty());
    s.run("layer.copy", &json!({"layer":"Cities"})).unwrap();
    s.run("task.new",&json!({"name":"Paste","steps":[{"title":"Step","command":"layer.paste","params":{"map":"Other"}},{"title":"Step","command":"layer.addUrl","params":{"url":"https://example.invalid/no-send.csv"}}]})).unwrap();
    let call = Call::command("task.run", json!({"task":"Paste"}));
    let p = pending(&s, &scope, &call);
    s.run("layer.copy", &json!({"layer":"Countries"})).unwrap();
    assert!(s.run_permitted(scope.clone(), p.allow(), &call).unwrap_err().contains("changed"));
    let dir = std::env::temp_dir().join(format!("rustgis-policy-stored-{}-{}", std::process::id(), crate::project_log::now_ms()));
    std::fs::create_dir(&dir).unwrap();
    let first = dir.join("first.zip");
    let second = dir.join("second.zip");
    s.run("share.webLayer", &json!({"layer":"Cities","path":first})).unwrap();
    let call = Call::command("share.replaceWebLayer", json!({}));
    let p = pending(&s, &scope, &call);
    assert!(
        p.review()
            .details
            .iter()
            .any(|d| d.targets.iter().any(|n| n.ends_with(" / Cities")) && d.places.iter().any(|n| n == &first.display().to_string()))
    );
    assert!(p.review().defaults.is_none(), "a managed saved source does not act on the unrelated active layer");
    s.run("share.webLayer", &json!({"layer":"Countries","path":second})).unwrap();
    let before = std::fs::read(&second).unwrap();
    assert!(s.run_permitted(scope, p.allow(), &call).unwrap_err().contains("changed"));
    assert_eq!(std::fs::read(&second).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Plugin preparation reads a held streamed source before returning its pinned job.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_plugin_preparation_keeps_owner_before_remote_send() {
    #[cfg(not(target_arch = "wasm32"))]
    if isolate_process_test("tests::permission_policy::permission_policy_plugin_preparation_keeps_owner_before_remote_send") {
        return;
    }
    use rustgis_data::source::{Batch, FeatureSource, Request, SourceInfo};
    use std::{
        io::{Read, Write},
        sync::{Mutex, mpsc},
        time::Duration,
    };
    struct Held {
        inner: Box<dyn FeatureSource>,
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        url: String,
        first: AtomicBool,
    }
    impl FeatureSource for Held {
        fn info(&self) -> &SourceInfo {
            self.inner.info()
        }
        fn read(&self, request: &Request) -> rustgis_data::Result<Batch> {
            if self.first.swap(false, Ordering::SeqCst) {
                assert!(rustgis_data::source::remote::current_still().is_some(), "actual plugin input preparation owns its carrier");
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                crate::render::rustgis_tiles::cache::download_blocking_within(&self.url, 64, Duration::from_secs(2))
                    .map_err(rustgis_data::DataError::Format)?;
            }
            self.inner.read(request)
        }
    }
    let plugin = "policy-preparing-owner";
    rustgis_plugins::load(rustgis_plugins::Package::from_zip(&rustgis_plugins::testing::package(plugin, &["edit"])).unwrap(), "test")
        .unwrap();
    for revoke in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/input-prepare", listener.local_addr().unwrap());
        let mut s = world();
        let id = s.map().layer_by_name("Cities").unwrap().id;
        let bytes = Arc::new(rustgis_data::source::fgb::write(&s.map().layer(id).unwrap().dataset).unwrap());
        let inner = rustgis_data::source::open_shared("input.fgb", bytes.clone(), None).unwrap();
        let (entered, waiting) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        s.map_mut().layer_mut(id).unwrap().source = Some(rustgis_map::source::SourceLayer::from_bytes(
            "input.fgb",
            bytes,
            None,
            Box::new(Held { inner, entered, release: Mutex::new(released), url, first: AtomicBool::new(true) }),
        ));
        let live = Arc::new(Live(AtomicBool::new(true)));
        let scope = Scope::agent(crate::policy::AgentAccess::Allow, false, "plugin-preparation", None, live.clone());
        let worker = std::thread::spawn(move || {
            let result = s
                .with_scope(scope, |s| crate::cmd::gp::plugin_start(s, "plugin:policy-preparing-owner/x", &json!({"input":"Cities"})))
                .unwrap();
            assert!(rustgis_data::source::remote::current_still().is_none());
            result
        });
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        live.0.store(!revoke, Ordering::SeqCst);
        let released_at = std::time::Instant::now();
        release.send(()).unwrap();
        let peer = if !revoke {
            let start = std::time::Instant::now();
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && start.elapsed() < Duration::from_secs(3) => {
                        std::thread::yield_now()
                    }
                    other => panic!("live preparation must actually send: {other:?}"),
                }
            };
            peer.set_nonblocking(false).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let n = peer.read(&mut chunk).unwrap();
                assert!(n > 0 && request.len() + n <= 32_768, "a bounded complete HTTP request");
                request.extend_from_slice(&chunk[..n]);
            }
            assert!(request.starts_with(b"GET /input-prepare HTTP/"));
            peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
            Some(peer)
        } else {
            None
        };
        let result = worker.join().unwrap();
        drop(peer);
        assert_eq!(result.is_err(), revoke, "revoke={revoke}, elapsed={:?}, error={:?}", released_at.elapsed(), result.as_ref().err());
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    }
    rustgis_plugins::unload(plugin);
}

/// Cancellation after a real plugin process starts stops both pinned engine and analysis
/// calls before its valid result is released. The fallback releases and joins a broken run.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_policy_plugin_tools_follow_the_callers_control_while_running() {
    #[cfg(not(target_arch = "wasm32"))]
    if isolate_process_test("tests::permission_policy::permission_policy_plugin_tools_follow_the_callers_control_while_running") {
        return;
    }
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    struct Input(rustgis_data::Dataset);
    impl rustgis_analysis::Inputs for Input {
        fn dataset(&self, name: &str) -> Option<&rustgis_data::Dataset> {
            (name == "Cities").then_some(&self.0)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
    }
    let plugin = "policy-plugin-control";
    let tool = "plugin:policy-plugin-control/m";
    let dir = std::env::temp_dir().join(format!("rustgis-policy-plugin-control-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = rustgis_plugins::testing::child_run("tests::plugin_process_child");
    std::fs::write(dir.join("plugin.json"), rustgis_plugins::testing::process_manifest(plugin, &[], &run)).unwrap();
    rustgis_plugins::load(rustgis_plugins::Package::from_dir(&dir).unwrap(), "test").unwrap();
    let mut outcomes = Vec::new();
    for engine in [true, false] {
        let mut s = world();
        let before = serde_json::to_value(&s.map().layers).unwrap();
        let ready = dir.join(format!("ready-{engine}"));
        let release = dir.join(format!("release-{engine}"));
        let params = json!({"input":"Cities", "control_ready":ready, "control_release":release});
        let control = rustgis_analysis::control::Control::new();
        let caller = control.clone();
        let (finished, done) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let result = if engine {
                crate::cmd::gp::run_tool_with(&mut s, tool, &params, &caller)
            } else {
                let input = Input(s.map().layer_by_name("Cities").unwrap().dataset.clone());
                rustgis_analysis::run_with(tool, &input, &params, &caller).map(|_| Value::Null)
            };
            finished.send(()).unwrap();
            (s, result)
        });
        let start = Instant::now();
        while !ready.is_file() && !worker.is_finished() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let entered = ready.is_file();
        control.cancel();
        let stopped = done.recv_timeout(Duration::from_secs(5)).is_ok();
        std::fs::write(release, b"finish if cancellation was ignored").unwrap();
        let (s, result) = worker.join().unwrap();
        let unchanged = serde_json::to_value(&s.map().layers).unwrap() == before;
        let failed_history = !engine || s.project.gp_history.last().is_some_and(|h| !h.ok && h.outputs.is_empty());
        outcomes.push((engine, entered, stopped, result.err(), unchanged, failed_history));
    }
    let mut clean = world();
    crate::cmd::gp::run_tool_with(&mut clean, tool, &json!({"input":"Cities"}), &rustgis_analysis::control::Control::new()).unwrap();
    assert!(clean.map().layer_by_name("Made").is_some());
    rustgis_plugins::unload(plugin);
    std::fs::remove_dir_all(dir).unwrap();
    for (engine, entered, stopped, error, unchanged, failed_history) in outcomes {
        assert!(
            entered && stopped && error.as_ref().is_some_and(|e| e.contains("cancelled")) && unchanged && failed_history,
            "engine={engine}, entered={entered}, stopped={stopped}, error={error:?}, unchanged={unchanged}, failed_history={failed_history}"
        );
    }
}

/// Choosing another already-active login changes consent without rotating either bearer.
#[test]
fn permission_policy_web_login_reselection_refuses_old_consent_and_landing() {
    use crate::{render::rustgis_tiles::logins, service_logins};
    let mut s = Session::new(Project::default());
    let origin = "https://policy-choice-r15.example.invalid";
    let mut ids = Vec::new();
    for name in ["A", "B"] {
        let id =
            s.run("connection.new", &json!({"kind":"wms","name":name,"address":origin,"auth":"header","keyName":"X-Key"})).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
        s.run("connection.login", &json!({"id":id,"apiKey":format!("RGSYNTH_choice_r15_{name}")})).unwrap();
        ids.push(id);
    }
    s.run("connection.login", &json!({"id":ids[0]})).unwrap();
    let registry = logins::account_identity(s.web_logins.session_id()).unwrap();
    let url = format!("{origin}/roads.geojson");
    let selected_a = service_logins::request(&mut s, None, &url).unwrap();
    let call = Call::command("layer.addUrl", json!({"url":url,"zoom":true})).with_request("choice-r15");
    let (scope, _) = assistant(1501);
    let old = pending(&s, &scope, &call);
    let run = pending(&s, &scope, &call);
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    s.run_permitted(scope.clone(), run.allow(), &call).unwrap();
    let request = s.take_url_requests().pop().unwrap();
    assert_eq!(request.params()["url"], selected_a);
    assert_eq!(request.params()["connection"], ids[0]);
    let claim = s.download_started_for(&request).unwrap();
    s.run("connection.login", &json!({"id":ids[1]})).unwrap();
    assert!(registry.is_current(), "reselection keeps both active generations");
    let selected_b = service_logins::request(&mut s, None, &url).unwrap();
    assert_ne!(selected_a, selected_b);
    assert_eq!(logins::apply(&selected_a).unwrap().unwrap().header.unwrap().1, "RGSYNTH_choice_r15_A");
    assert!(s.run_permitted(scope, old.allow(), &call).unwrap_err().contains("changed"));
    assert!(s.take_url_requests().is_empty(), "the old approval started no fresh request");
    assert!(s.download_continues(claim).unwrap_err().contains("account changed"));
    assert!(s.map().layers.is_empty());
}

/// A session choice naming a connection that is gone is no choice: the one connection left at
/// the origin still lends its login, as with no choice at all.
#[test]
fn permission_policy_web_login_stale_choice_falls_back_to_the_sole_connection() {
    use crate::{connections::WEB, render::rustgis_tiles::logins, service_logins};
    let mut s = Session::new(Project::default());
    let origin = "https://policy-stale-choice.example.invalid";
    let definition = json!({"kind":"wms","name":"Only","address":origin,"auth":"header","keyName":"X-Key"});
    let id = s.run("connection.new", &definition).unwrap()["id"].as_str().unwrap().to_string();
    s.run("connection.login", &json!({"id":id,"apiKey":"RGSYNTH_stale_choice"})).unwrap();
    let url = format!("{origin}/roads.geojson");
    let fresh = service_logins::request(&mut s, None, &url).unwrap();
    assert_ne!(fresh, url, "the sole connection's login is lent");
    s.database_logins.select(WEB, origin, "gone");
    let stale = service_logins::request(&mut s, None, &url).unwrap();
    assert_ne!(stale, url, "a choice of a connection that is gone doesn't read anonymously");
    assert_eq!(logins::apply(&stale).unwrap().unwrap().header.unwrap().1, "RGSYNTH_stale_choice");
}

/// Runtime snapshots include absence and this owner's logout, while another Session is separate.
#[test]
fn permission_policy_web_accounts_ignore_other_sessions_and_revoke_owned_sends() {
    use crate::render::rustgis_tiles::logins;
    let mut s = Session::new(Project::default());
    let origin = "https://policy-owner-r15.example.invalid";
    let definition = json!({"kind":"wms","address":origin,"auth":"header","keyName":"X-Key"});
    let id = s.run("connection.new", &definition).unwrap()["id"].as_str().unwrap().to_string();
    let absent = logins::account_identity(s.web_logins.session_id()).unwrap();
    s.run("connection.login", &json!({"id":id,"apiKey":"fake002"})).unwrap();
    assert!(!absent.is_current(), "a newly covering login is a different runtime identity");
    let registry = logins::account_identity(s.web_logins.session_id()).unwrap();
    let (scope, _) = assistant(1502);
    let call = Call::command("layer.addUrl", json!({"url":format!("{origin}/roads.geojson")})).with_request("owner-r15");
    let approved = pending(&s, &scope, &call);
    let mut other = Session::new(Project::default());
    let other_id = other.run("connection.new", &definition).unwrap()["id"].as_str().unwrap().to_string();
    other.run("connection.login", &json!({"id":other_id,"apiKey":"fake003"})).unwrap();
    assert!(registry.is_current());
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.leave_downloads = true;
    }
    s.run_permitted(scope, approved.allow(), &call).unwrap();
    let request = s.take_url_requests().pop().unwrap();
    let claim = s.download_started_for(&request).unwrap();
    let still = s.download_still(claim).unwrap();
    assert!(still());
    s.run("connection.logout", &json!({"id":id})).unwrap();
    assert!(!registry.is_current());
    assert!(!still(), "the actual send predicate sees this owner's logout");
    assert!(!s.download_landed(claim, b"late".to_vec()));
    assert!(s.map().layers.is_empty());
}

/// Both export owners refer to the original map, even when another map is active.
fn map_export_session() -> Session {
    let mut s = Session::new(Project::default());
    let map = s.map().name.clone();
    s.run("layout.new", &json!({"name":"Tile consent","template":"blank"})).unwrap();
    s.run("layout.addFrame", &json!({"map":map})).unwrap();
    s.run("presentation.new", &json!({"name":"Tile consent"})).unwrap();
    s.run("presentation.mapPage", &json!({"map":map})).unwrap();
    s
}

/// Rendering consent binds private tile addresses, including changes hidden by redaction.
#[test]
fn permission_policy_render_exports_bind_raw_tile_sources() {
    use rustgis_map::{TileKind, TileLayer};
    for (i, command) in ["layout.export", "presentation.export", "share.exportMap"].into_iter().enumerate() {
        let mut s = map_export_session();
        s.map_mut().tiles.push(TileLayer::new(
            "Reviewed tiles",
            TileKind::Raster,
            "https://tiles-r16.invalid/{z}/{x}/{y}.png?token=RGSYNTH_render_one",
        ));
        // The unsupported suffix stops the old handler before rendering or writing a file.
        let call = Call::command(command, json!({"path":"policy-render.unsupported"}));
        let (scope, _) = assistant(1600 + i as u64);
        let approved = pending(&s, &scope, &call);
        assert!(!format!("{:?}", approved.review()).contains("RGSYNTH_render_"));
        s.map_mut().tiles[0].url = "https://tiles-r16.invalid/{z}/{x}/{y}.png?token=RGSYNTH_render_two".into();
        let error = s.run_permitted(scope, approved.allow(), &call).unwrap_err();
        assert!(error.contains("changed"), "{command} admitted a changed tile source: {error}");
    }
}

/// Only maps consumed by the exporter contribute network reach; unrelated maps stay local.
#[test]
fn permission_policy_render_exports_require_referenced_tile_reach() {
    use crate::policy::{AgentAccess, Effect};
    use rustgis_map::{TileKind, TileLayer};
    struct LocalOnly;
    impl Revocation for LocalOnly {
        fn allows(&self, effects: Effects) -> bool {
            !effects.contains(Effect::Network)
        }
    }
    let scope = Scope::agent(AgentAccess::Allow, false, "render-local-r16", None, Arc::new(LocalOnly));
    let mut s = map_export_session();
    s.run("map.duplicate", &json!({"name":"Unused map"})).unwrap();
    let calls = ["layout.export", "presentation.export"].map(|command| Call::command(command, json!({"path":"policy-render.pdf"})));
    for call in &calls {
        assert!(matches!(s.review(&scope, call), Outcome::Allowed), "in-memory maps need no network access");
    }
    s.map_mut().tiles.push(TileLayer::new("Unused tiles", TileKind::Raster, "https://unused-r16.invalid/{z}/{x}/{y}.png"));
    for call in &calls {
        assert!(matches!(s.review(&scope, call), Outcome::Allowed), "an unreferenced map is not rendered");
    }
    let tile = s.map_mut().tiles.pop().unwrap();
    s.project.maps[0].tiles.push(tile);
    for call in &calls {
        assert!(matches!(s.review(&scope, call), Outcome::Refused(_)), "rendering referenced web tiles requires network access");
    }
}

/// Advertised bounds are an in-memory zoom; absent archive/COG bounds require source access.
#[test]
fn permission_policy_zoom_to_tiles_reads_only_unknown_metadata() {
    use crate::policy::AgentAccess;
    use rustgis_map::{TileKind, TileLayer};
    let refuse = Scope::agent(AgentAccess::Refuse, false, "tile-metadata-refuse-r16", None, Arc::new(Live(AtomicBool::new(true))));
    let review = Scope::agent(AgentAccess::Review, false, "tile-metadata-review-r16", None, Arc::new(Live(AtomicBool::new(true))));
    let local = std::env::temp_dir().join("rustgis-policy-r16.mbtiles").display().to_string();
    for (kind, url) in [
        (TileKind::PmTiles, "https://metadata-r16.invalid/map.pmtiles?token=RGSYNTH_metadata"),
        (TileKind::Cog, "https://metadata-r16.invalid/map.tif?token=RGSYNTH_metadata"),
        (TileKind::MbTiles, local.as_str()),
    ] {
        let mut s = Session::new(Project::default());
        s.map_mut().tiles.push(TileLayer::new("Coverage", kind, url));
        let call = Call::command("map.zoomToTiles", json!({"index":0}));
        assert!(matches!(s.review(&refuse, &call), Outcome::Refused(_)), "{kind:?} metadata needs source access");
        let Outcome::NeedsApproval(approved) = s.review(&review, &call) else { panic!("{kind:?} metadata needs review") };
        assert!(!format!("{:?}", approved.review()).contains("RGSYNTH_metadata"));
        s.map_mut().tiles[0].bounds = Some([-12.0, 40.0, 12.0, 60.0]);
        assert!(matches!(s.review(&refuse, &call), Outcome::Allowed), "stored bounds need no source read");
        assert_eq!(s.run_scoped(refuse.clone(), &call).unwrap()["bounds"], json!([-12.0, 40.0, 12.0, 60.0]));
    }
    let mut s = Session::new(Project::default());
    s.map_mut().tiles.push(TileLayer::new("No metadata", TileKind::Raster, "https://metadata-r16.invalid/{z}/{x}/{y}.png"));
    let call = Call::command("map.zoomToTiles", json!({"index":0}));
    assert!(matches!(s.review(&refuse, &call), Outcome::Allowed), "ordinary XYZ tiles have no metadata read");
    assert!(s.run_scoped(refuse, &call).unwrap_err().contains("doesn't say what it covers"));
}

/// Optional measurement rasters stay optional in both strict automation vehicles.
#[test]
fn permission_policy_layerless_measurement_matches_strict_execution() {
    let mut s = world();
    let (scope, _) = assistant(3501);
    for kind in ["distance", "area"] {
        let params = json!({"kind":kind,"points":[[0,0],[1,0],[1,1]]});
        let direct = s.run("raster.measure", &params).unwrap();
        let script = format!("raster.measure {params}");
        let call = Call::command("script.run", json!({"script":script}));
        let p = pending(&s, &scope, &call);
        let result = s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
        assert_eq!(result["ran"][0]["result"], direct);
        let name = format!("Measure {kind}");
        s.run("task.new", &json!({"name":name,"steps":[{"title":"Measure","command":"raster.measure","params":params}]})).unwrap();
        let call = Call::command("task.run", json!({"task":name}));
        let p = pending(&s, &scope, &call);
        s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    }
    s.run("layer.addDataBytes", &json!({"fileName":"scene.tif","data":two_class_b64()})).unwrap();
    let params = json!({"kind":"distance","layer":"scene","points":[[0,0],[1,0]]});
    let call = Call::command("script.run", json!({"script":format!("raster.measure {params}")}));
    let p = pending(&s, &scope, &call);
    s.run("layer.remove", &json!({"layer":"scene"})).unwrap();
    let measurements = s.measurements.len();
    assert!(s.run_permitted(scope.clone(), p.allow(), &call).is_err());
    assert_eq!(s.measurements.len(), measurements, "an explicitly selected missing raster remains bound");
    assert!(s.run("raster.measure", &json!({"kind":"volume","points":[[0,0],[1,0],[1,1]]})).is_err());
}

/// In-run renamed inputs retain external reach in the fast preflight and the strict card.
#[test]
fn permission_policy_renamed_streamed_inputs_keep_their_reach() {
    use crate::policy::AgentAccess;
    let mut outcomes = Vec::new();
    for tool in [true, false] {
        let mut s = world();
        let id = s.map().layer_by_name("Cities").unwrap().id;
        let mut source = rustgis_map::source::SourceLayer::default();
        source.path = Some("https://example.invalid/renamed.fgb".into());
        let layer = s.map_mut().layer_mut(id).unwrap();
        layer.source = Some(source);
        layer.selection.clear();
        let read = if tool {
            "gp.run {\"tool\":\"buffer\",\"params\":{\"input\":\"Renamed source\",\"distance\":10}}"
        } else {
            "layer.loadAll {\"layer\":\"Renamed source\"}"
        };
        let script = format!("layer.rename {{\"layer\":\"Cities\",\"name\":\"Renamed source\"}}\n{read}");
        let (scope, _) = assistant(3502 + u64::from(tool));
        let call = Call::script(&script);
        let needs_approval = matches!(s.review(&scope, &call), Outcome::NeedsApproval(_));
        let strict = Call::command("script.run", json!({"script":script}));
        let p = pending(&s, &scope, &strict);
        let operation = if tool { "buffer " } else { "layer.loadAll " };
        let shown_as_external = p.review().steps.iter().any(|step| step.starts_with(operation));
        outcomes.push((operation, needs_approval, shown_as_external));
        // A late refusal must still restore the earlier rename; no source read is permitted.
        let before = serde_json::to_value(&s.project).unwrap();
        let refuse = Scope::agent(AgentAccess::Refuse, false, "rename-no-io", None, Arc::new(Live(AtomicBool::new(true))));
        assert!(s.run_scoped(refuse, &call).is_err());
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    }
    assert!(outcomes.iter().all(|(_, asks, shown)| *asks && *shown), "{outcomes:?}");
}

/// Training read whole from a layer read in place reaches its source, as Load All does; the
/// same layer in memory reaches nothing.
#[test]
fn permission_policy_training_read_whole_reaches_its_source() {
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName":"scene.tif","data":two_class_b64()})).unwrap();
    let (scope, _) = assistant(3602);
    let calls = [
        Call::command("dl.train", json!({"layer":"scene","training":"Countries","classField":"name"})),
        Call::command("dl.classify", json!({"layer":"scene","training":"Countries","classField":"name"})),
        Call::command("raster.classifySupervised", json!({"layer":"scene","training":"Countries","classField":"name"})),
    ];
    let reaches = |s: &Session, call: &Call| matches!(s.review(&scope, call), Outcome::NeedsApproval(_));
    assert!(calls.iter().all(|call| !reaches(&s, call)), "in memory: nothing to approve");
    let id = s.map().layer_by_name("Countries").unwrap().id;
    let mut source = rustgis_map::source::SourceLayer::default();
    source.path = Some("https://example.invalid/countries.fgb".into());
    s.map_mut().layer_mut(id).unwrap().source = Some(source);
    for call in &calls {
        let approved = pending(&s, &scope, call);
        assert!(approved.review().why.contains("internet"), "{:?}", approved.review().why);
    }
}

/// In-memory range reads need Network, while local paths and external drivers retain Files.
#[test]
fn permission_policy_remote_range_reads_need_only_network() {
    let mut s = world();
    let id = s.map().layer_by_name("Cities").unwrap().id;
    s.map_mut().layer_mut(id).unwrap().selection.clear();
    let (scope, _) = assistant(3601);
    for (path, driver, files, network) in [
        ("https://example.invalid/roads.fgb", false, false, true),
        ("http://example.invalid/roads.parquet?subset=all", false, false, true),
        ("s3://example/roads.geoparquet", false, false, true),
        ("/local/roads.fgb", false, true, false),
        ("https://example.invalid/roads.fgb", true, true, true),
    ] {
        let mut source = rustgis_map::source::SourceLayer::default();
        source.path = Some(path.into());
        source.gdal = driver.then(|| rustgis_map::source::GdalOrigin {
            path: "external-driver-source".into(),
            layer: "roads".into(),
            family: None,
            crs: None,
        });
        s.map_mut().layer_mut(id).unwrap().source = Some(source);
        for call in [
            Call::command("layer.loadAll", json!({"layer":"Cities"})),
            Call::tool("buffer", json!({"input":"Cities","distance":10})),
            Call::command("gp.spatialJoin", json!({"target":"Cities","join":"Countries","output":"Joined"})),
        ] {
            let approved = pending(&s, &scope, &call);
            let why = &approved.review().why;
            assert_eq!(why.contains("files"), files, "{path}: {why}");
            assert_eq!(why.contains("internet"), network, "{path}: {why}");
        }
    }
}

/// Unicode folds do not alias distinct executor names, for prior renames or new outputs.
#[test]
fn permission_policy_planned_names_use_executor_ascii_case() {
    let mut s = world();
    let cities = s.map().layer_by_name("Cities").unwrap().id;
    let countries = s.map().layer_by_name("Countries").unwrap().id;
    let (scope, _) = assistant(3602);
    let call = Call::command(
        "script.run",
        json!({"script":
            "layer.rename {\"layer\":\"Cities\",\"name\":\"K\"}\nlayer.rename {\"layer\":\"Countries\",\"name\":\"K\"}\nlayer.visibility {\"layer\":\"k\",\"visible\":false}\nlayer.visibility {\"layer\":\"K\",\"visible\":true}"
        }),
    );
    let approved = pending(&s, &scope, &call);
    s.run_permitted(scope.clone(), approved.allow(), &call).unwrap();
    assert_eq!(s.map().layer_by_name("k").unwrap().id, cities);
    assert!(!s.map().layer(cities).unwrap().visible);
    assert_eq!(s.map().layer_by_name("K").unwrap().id, countries);
    assert!(s.map().layer(countries).unwrap().visible);
    for first in [
        format!(
            "layer.addDataText {}",
            json!({"name":"Kelvin","fileName":"point.geojson","text":"{\"type\":\"Point\",\"coordinates\":[0,0]}"})
        ),
        "gp.run {\"tool\":\"buffer\",\"params\":{\"input\":\"K\",\"distance\":10,\"output\":\"Kelvin\"}}".into(),
    ] {
        for next in [
            "layer.visibility {\"layer\":\"Kelvin\",\"visible\":false}",
            "gp.run {\"tool\":\"buffer\",\"params\":{\"input\":\"Kelvin\",\"distance\":10}}",
        ] {
            let call = Call::command("script.run", json!({"script":format!("{first}\n{next}")}));
            assert!(
                matches!(s.review(&scope, &call), Outcome::Refused(reason)
                    if reason.contains("Kelvin") && (reason.contains("no layer") || reason.contains("no earlier output"))),
                "Unicode-distinct missing outputs must refuse before execution"
            );
        }
    }
}

/// Secondary optional inputs must retain future names for the planner's existing alias resolver.
#[test]
fn permission_policy_stretch_like_keeps_its_renamed_raster_target() {
    let mut s = world();
    for file in ["scene.tif", "template.tif"] {
        s.run("layer.addDataBytes", &json!({"fileName":file,"data":two_class_b64()})).unwrap();
    }
    let (scope, _) = assistant(3504);
    let call = Call::command(
        "script.run",
        json!({"script":
            "layer.rename {\"layer\":\"scene\",\"name\":\"Renamed scene\"}\nlayer.rename {\"layer\":\"template\",\"name\":\"Renamed template\"}\nraster.stretch {\"layer\":\"Renamed scene\",\"like\":\"Renamed template\"}"
        }),
    );
    let p = pending(&s, &scope, &call);
    let detail = p.review().details.iter().find(|d| d.operation == "raster.stretch").unwrap();
    assert!(detail.targets.iter().any(|t| t.ends_with(" / scene")), "{detail:?}");
    assert!(detail.targets.iter().any(|t| t.ends_with(" / template")), "the actual secondary raster must appear: {detail:?}");
    s.run_permitted(scope, p.allow(), &call).unwrap();
    assert!(s.map().layer_by_name("Renamed scene").is_some());
    assert!(s.map().layer_by_name("Renamed template").is_some());
}

/// The save executor already retires the old trust key before the shared post-handler hook.
#[test]
fn permission_policy_model_rename_does_not_leave_old_name_trust() {
    let mut s = world();
    let graph = json!({"nodes":[{"id":"i","kind":"input","layer":"Cities"},{"id":"b","kind":"tool","tool":"buffer","params":{"distance":10}}],
        "edges":[{"from":"i","fromPort":"out","to":"b","toPort":"input"}]});
    s.run("model.new", &json!({"name":"Original","graph":graph})).unwrap();
    let original = s.item_content("model", "Original").unwrap();
    assert!(s.item_trusted("model", "Original"));
    s.run("model.save", &json!({"name":" Original ","rename":" Renamed ","graph":graph})).unwrap();
    assert!(s.item_trusted("model", "Renamed"));
    assert!(!s.trusted_models.contains_key("original"));
    s.run("model.new", &json!({"name":"Original","graph":graph,"imported":true})).unwrap();
    assert_eq!(s.item_content("model", "Original").unwrap(), original, "the recreated executable content is identical");
    assert!(!s.item_trusted("model", "Original"));
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.item_trusted("model", "Renamed"));
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.item_trusted("model", "Original"));
    assert!(s.project.models.iter().all(|m| m.name != "Renamed"));
}

/// An ignored field on a trusted editor cannot confer trust on another item's unchanged code.
#[test]
fn permission_policy_ignored_rename_does_not_promote_another_task() {
    let mut s = world();
    s.run("task.new", &json!({"name":"Authored","steps":[]})).unwrap();
    s.run(
        "task.new",
        &json!({"name":"Foreign","imported":true,"steps":[
            {"title":"Foreign source","command":"layer.addUrl","params":{"url":"https://example.invalid/foreign.csv"}}
        ]}),
    )
    .unwrap();
    let foreign = s.item_content("task", "Foreign").unwrap();
    assert!(s.item_trusted("task", "Authored"));
    assert!(!s.item_trusted("task", "Foreign"));
    s.run("task.addStep", &json!({"task":"Authored","title":"Zoom","command":"map.zoomIn","rename":"Foreign"})).unwrap();
    assert_eq!(s.item_content("task", "Foreign").unwrap(), foreign);
    assert!(!s.item_trusted("task", "Foreign"), "an ignored field must not promote unchanged imported code");
    assert!(s.item_trusted("task", "Authored"), "the actual user-edited task retains its content record");
}

/// Strict consent refuses a name collision instead of guessing which layer execution will select.
#[test]
fn permission_policy_rename_collision_refuses_only_strict_review() {
    let s = world();
    let params = json!({"layer":"Cities","name":" Countries "});
    let call = Call::command("script.run", json!({"script":format!("layer.rename {params}")}));
    let (scope, _) = assistant(3505);
    let before = serde_json::to_value(&s.project).unwrap();
    assert!(matches!(s.review(&scope, &call), Outcome::Refused(e) if e.contains("share another layer's name")));
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "refusal never executes the rename");

    let call = Call::command(
        "script.run",
        json!({"script":
            "layer.rename {\"layer\":\"Cities\",\"name\":\"Fresh\"}\nlayer.rename {\"layer\":\"Countries\",\"name\":\"Fresh\"}"
        }),
    );
    assert!(matches!(s.review(&scope, &call), Outcome::Refused(e) if e.contains("share another layer's name")));
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "planned collisions are refused before either rename");

    let mut s = world();
    let id = s.map().layer_by_name("Cities").unwrap().id;
    let call = Call::command(
        "script.run",
        json!({"script":
            "layer.rename {\"layer\":\"Cities\",\"name\":\"Fresh\"}\nlayer.rename {\"layer\":\"Fresh\",\"name\":\"Fresh\"}"
        }),
    );
    let p = pending(&s, &scope, &call);
    s.run_permitted(scope.clone(), p.allow(), &call).unwrap();
    assert_eq!(s.map().layer(id).unwrap().name, "Fresh", "a repeat rename of the same bound object remains allowed");

    let mut s = world();
    let id = s.map().layer_by_name("Cities").unwrap().id;
    s.run("layer.rename", &params).unwrap();
    assert_eq!(s.map().layer(id).unwrap().name, "Countries", "ordinary User naming remains unchanged");
    let mut s = world();
    let id = s.map().layer_by_name("Cities").unwrap().id;
    let allow =
        Scope::agent(crate::policy::AgentAccess::Allow, false, "rename-collision-allow", None, Arc::new(Live(AtomicBool::new(true))));
    let call = Call::script(&format!("layer.rename {params}"));
    assert!(matches!(s.review(&allow, &call), Outcome::Allowed));
    s.run_scoped(allow, &call).unwrap();
    assert_eq!(s.map().layer(id).unwrap().name, "Countries", "the Allow ingress retains its existing convention");
}

/// Mask and elevation metadata retain names that only exist after an earlier planned rename.
#[test]
fn permission_policy_renamed_mask_and_elevation_keep_their_targets() {
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName":"scene.tif","data":two_class_b64()})).unwrap();
    let cities = s.map().layer_by_name("Cities").unwrap().id;
    let (scope, _) = assistant(3506);
    for params in [json!({"layer":"Cities","mask":"Cities"}), json!({"layer":"Countries","mask":"Countries"})] {
        let call = Call::command("script.run", json!({"script":format!("layer.mask {params}")}));
        assert!(matches!(s.review(&scope, &call), Outcome::Refused(_)), "known nonpolygon and self masks remain refused");
    }
    assert!(s.run("edit.elevation", &json!({"mode":"surface","surface":"Cities"})).is_err());
    let call = Call::command(
        "script.run",
        json!({"script":
            "layer.rename {\"layer\":\"Countries\",\"name\":\"Renamed mask\"}\nlayer.rename {\"layer\":\"Cities\",\"name\":\"Renamed cities\"}\nlayer.rename {\"layer\":\"scene\",\"name\":\"Renamed surface\"}\nlayer.mask {\"layer\":\"Renamed cities\",\"mask\":\"Renamed mask\"}\nedit.elevation {\"mode\":\"surface\",\"surface\":\"Renamed surface\"}"
        }),
    );
    let p = pending(&s, &scope, &call);
    let mask = p.review().details.iter().find(|d| d.operation == "layer.mask").unwrap();
    assert!(mask.targets.iter().any(|t| t.ends_with(" / Countries")), "{mask:?}");
    assert!(mask.targets.iter().any(|t| t.ends_with(" / Cities")), "{mask:?}");
    let elevation = p.review().details.iter().find(|d| d.operation == "edit.elevation").unwrap();
    assert!(elevation.targets.iter().any(|t| t.ends_with(" / scene")), "{elevation:?}");
    s.run_permitted(scope, p.allow(), &call).unwrap();
    assert_eq!(s.map().layer(cities).unwrap().mask.as_deref(), Some("Renamed mask"));
    assert!(matches!(&s.map().editing.settings.elevation, rustgis_map::design::Elevation::Surface(name) if name == "Renamed surface"));
}

/// A failed drain can restore untouched queued work, but cannot revive terminally retired work.
#[test]
fn permission_policy_rollback_keeps_retired_url_requests_consumed() {
    let mut s = world();
    s.leave_downloads = true;
    let (owner, live) = assistant(245);
    let call = Call::command("layer.addUrl", json!({"url":"https://rollback.invalid/revoked.csv"}));
    let p = pending(&s, &owner, &call);
    s.run_permitted(owner, p.allow(), &call).unwrap();
    s.request_url(&json!({"url":"https://rollback.invalid/untouched.csv"})).unwrap();
    live.0.store(false, Ordering::SeqCst);
    let mark = s.notice_mark();
    assert!(
        s.guarded("Failed request drain", true, |s| {
            s.take_url_requests();
            Err("fail after retiring a revoked request".into())
        })
        .is_err()
    );
    live.0.store(true, Ordering::SeqCst);
    let remaining = s.take_url_requests();
    let addresses: Vec<_> = remaining.iter().map(|q| q.params()["url"].as_str().unwrap()).collect();
    assert_eq!(addresses, ["https://rollback.invalid/untouched.csv"], "rollback cannot revive retired authority");
    assert_eq!(s.notices_since(mark).iter().filter(|n| n.text.contains("revoked before it started")).count(), 1);
}
