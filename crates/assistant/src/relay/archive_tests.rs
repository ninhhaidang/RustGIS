//! Real archive reads across the relay's approval, worker and completion boundaries.
#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;
use rustgis_engine::{
    Project,
    cmd::reads,
    render::rustgis_tiles::{mvt, pmtiles},
};
use serde_json::json;
use std::sync::mpsc::Receiver;

fn archive(path: &std::path::Path, credit: &str) {
    let metadata = pmtiles::NewArchive {
        tile_type: pmtiles::TileType::Mvt,
        gzipped_tiles: false,
        bounds: [-180.0, -85.0, 180.0, 85.0],
        metadata: json!({"attribution":credit}),
    };
    let bytes = pmtiles::write(vec![(0, 0, 0, mvt::encode(&mvt::Tile { layers: Vec::new() }))], &metadata).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn fixture() -> (tempfile::TempDir, Session, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.pmtiles");
    let replacement = root.path().join("replacement.pmtiles");
    archive(&original, "Original");
    archive(&replacement, "Replacement");
    let mut session = Session::new(Project::default());
    session.run("map.addTiles", &json!({"url":original,"name":"Linked","overlay":true})).unwrap();
    (root, session, original, replacement)
}

fn command(id: u64, name: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"run_command","arguments":{"command":name,"params":params}}})
}

fn queue(relay: &Relay, grant: &Grant, message: Value) -> (u64, Receiver<Option<String>>) {
    let (reply, rx) = sync_channel(1);
    let id = relay.owner.shared.next_id.fetch_add(1, SeqCst);
    lock(&relay.owner.shared.requests).push_back(Request {
        plans: Vec::new(),
        approval_required: false,
        held: Arc::default(),
        id,
        decision: Arc::new(AtomicU8::new(PENDING)),
        line: message.to_string(),
        token: grant.token.clone(),
        asked: true,
        reply,
        state: Arc::new(AtomicU8::new(PENDING)),
    });
    (id, rx)
}

fn worker(ahead: reads::ReadAhead) -> Result<reads::Read, String> {
    std::thread::spawn(move || ahead.run(&rustgis_engine::analysis::control::Control::stoppable())).join().unwrap()
}

fn finish(relay: &Relay, session: &mut Session, done: Result<reads::Read, String>) {
    relay.begin_archive_landing(session);
    let result = done.and_then(|read| reads::read_land(session, read));
    relay.finish_archive_read(session, result);
}

fn reply(rx: &Receiver<Option<String>>) -> Value {
    serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap()).unwrap()
}

#[test]
fn scoped_archive_reply_reports_only_changes_made_when_it_lands() {
    let (_root, mut s, _, replacement) = fixture();
    let relay = Relay::start(0).unwrap();
    let grant = relay.grant(true).unwrap();
    let (_, rx) = queue(&relay, &grant, command(1, "map.repairTiles", json!({"index":0,"path":replacement})));
    assert_eq!(relay.poll(&mut s), 0);
    let done = worker(relay.take_archive_read().unwrap());
    // Work the queue ran between admission and landing belongs to its own caller.
    s.run("layer.addSample", &json!({"name":"countries"})).unwrap();
    s.notify("Unrelated queued notice", false);
    finish(&relay, &mut s, done);
    let answer = reply(&rx);
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let text = answer["result"]["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains("layersAdded") && !text.contains("Countries"), "{text}");
    assert!(!text.contains("Unrelated queued notice"), "{text}");
    assert!(text.contains("replacement") || text.contains("Repaired"), "{text}");
    relay.stop();
}

#[test]
fn scoped_archive_capture_rejects_scripts_other_commands_and_nested_ingress() {
    use rustgis_engine::policy::Call;
    let (_root, mut s, _, _) = fixture();
    for call in [Call::script("map.reloadTiles {\"index\":0}"), Call::command("map.tiles", json!({}))] {
        assert!(s.start_scoped_archive_read(Scope::peer(), &call, None).is_err());
    }
    let call = Call::command("map.reloadTiles", json!({"index":0}));
    let nested = s.with_scope(Scope::peer(), |s| s.start_scoped_archive_read(Scope::peer(), &call, None)).unwrap();
    assert!(nested.is_err(), "a nested owner must not create a new pre-handler escape");
}

#[test]
fn direct_allowed_archive_reload_keeps_its_scoped_owner_without_a_permit() {
    let (_root, mut s, original, _) = fixture();
    let relay = Relay::start(0).unwrap();
    let grant = relay.grant(true).unwrap();
    let retained = pmtiles::info_blocking(original.to_str().unwrap(), Duration::from_secs(10)).unwrap();
    archive(&original, "Allowed scoped reload");
    let (_, rx) = queue(&relay, &grant, command(1, "map.reloadTiles", json!({"index":0})));
    assert_eq!(relay.poll(&mut s), 0);
    assert!(grant.pending().is_empty());
    let done = worker(relay.take_archive_read().unwrap());
    assert!(rx.try_recv().is_err());
    assert!(Arc::ptr_eq(&retained, &pmtiles::info_blocking(original.to_str().unwrap(), Duration::from_secs(10)).unwrap()));
    finish(&relay, &mut s, done);
    let answer = reply(&rx);
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    assert!(!Arc::ptr_eq(&retained, &pmtiles::info_blocking(original.to_str().unwrap(), Duration::from_secs(10)).unwrap()));
    relay.stop();
}

#[test]
fn reviewed_direct_archive_repair_waits_for_real_completion_and_keeps_original_actor_and_undo() {
    let (root, mut s, original, replacement) = fixture();
    let relay = Relay::start(0).unwrap();
    let log = root.path().join("audit");
    relay.set_audit_dir(Some(log.clone()));
    let grant = relay.grant_reviewed(false).unwrap();
    let depth = s.undo_depth();
    let (id, rx) = queue(&relay, &grant, command(1, "map.repairTiles", json!({"index":0,"path":replacement})));
    assert_eq!(relay.poll(&mut s), 0);
    assert_eq!(grant.pending().len(), 1);
    assert!(relay.take_archive_read().is_none(), "approval must precede even worker creation");
    assert_eq!(s.map().tiles[0].url, original.to_str().unwrap());
    assert!(rx.try_recv().is_err());
    grant.decide(id, true);
    assert_eq!(relay.poll(&mut s), 0);
    let ahead = relay.take_archive_read().unwrap();
    assert!(ahead.reads());
    assert!(relay.take_archive_read().is_none(), "one admitted invocation, not redispatch");
    let done = worker(ahead);
    assert_eq!(s.undo_depth(), depth);
    assert_eq!(s.map().tiles[0].url, original.to_str().unwrap());
    assert!(rx.try_recv().is_err(), "worker completion is not command completion");
    assert!(!log.exists() || std::fs::read_dir(&log).unwrap().next().is_none(), "no premature audit");
    s.actor = Some("intervening user".into());
    finish(&relay, &mut s, done);
    let answer = reply(&rx);
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    assert_eq!(s.map().tiles[0].url, replacement.to_str().unwrap());
    assert_eq!(s.undo_depth(), depth + 1);
    assert!(s.undo_actor().flatten().is_some_and(|actor| actor.starts_with("agent 1 (relay run ")), "{:?}", s.undo_actor());
    let files: Vec<_> = std::fs::read_dir(&log).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let audit = std::fs::read_to_string(files[0].path()).unwrap();
    let entry: Value = serde_json::from_str(audit.trim()).unwrap();
    assert_eq!(entry["success"], true, "{entry}");
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().tiles[0].url, original.to_str().unwrap());
    relay.stop();
}

#[test]
fn scoped_archive_revocation_before_worker_and_before_landing_has_no_effect() {
    for before_start in [true, false] {
        let (_root, mut s, original, replacement) = fixture();
        let relay = Relay::start(0).unwrap();
        let grant = relay.grant_reviewed(false).unwrap();
        let depth = s.undo_depth();
        let (id, rx) = queue(&relay, &grant, command(1, "map.repairTiles", json!({"index":0,"path":replacement})));
        relay.poll(&mut s);
        grant.decide(id, true);
        relay.poll(&mut s);
        let ahead = relay.take_archive_read().unwrap();
        let done = if before_start {
            drop(grant);
            let done = worker(ahead);
            assert!(done.is_err(), "revoked authority must not start its read");
            done
        } else {
            let done = worker(ahead);
            assert!(done.is_ok());
            drop(grant);
            done
        };
        finish(&relay, &mut s, done);
        let answer = reply(&rx);
        assert_eq!(answer["result"]["isError"], true, "{answer}");
        assert_eq!(s.map().tiles[0].url, original.to_str().unwrap());
        assert_eq!(s.undo_depth(), depth);
        relay.stop();
    }
}

#[test]
fn scoped_archive_read_cannot_land_on_a_stale_target() {
    let (_root, mut s, original, replacement) = fixture();
    let relay = Relay::start(0).unwrap();
    let grant = relay.grant_reviewed(false).unwrap();
    let (id, rx) = queue(&relay, &grant, command(1, "map.repairTiles", json!({"index":0,"path":replacement})));
    relay.poll(&mut s);
    grant.decide(id, true);
    relay.poll(&mut s);
    let done = worker(relay.take_archive_read().unwrap());
    s.run("map.setTiles", &json!({"index":0,"opacity":0.25})).unwrap();
    let depth = s.undo_depth();
    finish(&relay, &mut s, done);
    let answer = reply(&rx);
    assert_eq!(answer["result"]["isError"], true, "{answer}");
    assert_eq!(s.map().tiles[0].url, original.to_str().unwrap());
    assert_eq!(s.map().tiles[0].opacity, 0.25);
    assert_eq!(s.undo_depth(), depth);
    relay.stop();
}

#[test]
fn batch_reload_holds_observer_and_later_requests_until_real_landing() {
    let (_root, mut s, original, _) = fixture();
    let relay = Relay::start(0).unwrap();
    let grant = relay.grant(true).unwrap();
    // A real retained metadata object distinguishes validation from installation.
    let retained = pmtiles::info_blocking(original.to_str().unwrap(), Duration::from_secs(10)).unwrap();
    archive(&original, "Fresh metadata after manual reload");
    s.map_mut().tiles[0].error = Some("Observer must not see the retained error".into());
    let (_, rx) = queue(&relay, &grant, json!([command(7, "map.reloadTiles", json!({"index":0})), command(7, "map.tiles", json!({}))]));
    assert_eq!(relay.poll(&mut s), 0);
    let done = worker(relay.take_archive_read().unwrap());
    assert_eq!(s.map().tiles[0].error.as_deref(), Some("Observer must not see the retained error"));
    assert!(Arc::ptr_eq(&retained, &pmtiles::info_blocking(original.to_str().unwrap(), Duration::from_secs(10)).unwrap()));
    let (_, later) = queue(&relay, &grant, json!({"jsonrpc":"2.0","id":9,"method":"ping"}));
    assert_eq!(relay.poll(&mut s), 0);
    assert!(later.try_recv().is_err());
    assert!(rx.try_recv().is_err());
    finish(&relay, &mut s, done);
    let answer = reply(&rx);
    assert_eq!(answer.as_array().unwrap().len(), 2, "{answer}");
    assert!(answer.as_array().unwrap().iter().all(|r| r["id"] == 7 && r["result"]["isError"] == false), "{answer}");
    let observed = answer[1]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(observed.contains("Linked"), "{observed}");
    assert!(!observed.contains("Observer must not see the retained error"), "{observed}");
    let fresh = pmtiles::info_blocking(original.to_str().unwrap(), Duration::from_secs(10)).unwrap();
    assert!(!Arc::ptr_eq(&retained, &fresh));
    relay.poll(&mut s);
    assert_eq!(reply(&later)["id"], 9);
    relay.stop();
}

/// An approval freezes the state its card showed. A batch suffix whose reviewed input the earlier
/// entry then legitimately changes (a retained load error the reload clears) is refused at its own
/// turn, after the reload landed, instead of running on stale consent.
#[test]
fn reviewed_batch_suffix_keeps_its_preflight_state_across_a_landed_reload() {
    let (_root, mut s, _, _) = fixture();
    let relay = Relay::start(0).unwrap();
    let grant = relay.grant_reviewed(false).unwrap();
    s.map_mut().tiles[0].error = Some("retained".into());
    let (id, rx) = queue(&relay, &grant, json!([command(7, "map.reloadTiles", json!({"index":0})), command(8, "map.tiles", json!({}))]));
    relay.poll(&mut s);
    grant.decide(id, true);
    relay.poll(&mut s);
    let done = worker(relay.take_archive_read().unwrap());
    assert!(rx.try_recv().is_err());
    finish(&relay, &mut s, done);
    let answer = reply(&rx);
    assert_eq!(answer[0]["result"]["isError"], false, "{answer}");
    assert_eq!(answer[1]["result"]["isError"], true, "{answer}");
    assert!(s.map().tiles[0].error.is_none(), "the observer's turn came after the reload landed");
    relay.stop();
}

#[test]
fn padded_archive_commands_use_the_worker_and_keep_admission_and_landing_boundaries() {
    for reviewed in [false, true] {
        for repair in [false, true] {
            let (_root, mut s, original, replacement) = fixture();
            let retained = pmtiles::info_blocking(original.to_str().unwrap(), Duration::from_secs(10)).unwrap();
            archive(&original, "Reloaded");
            let relay = Relay::start(0).unwrap();
            let grant = if reviewed { relay.grant_reviewed(false).unwrap() } else { relay.grant(true).unwrap() };
            let name = if repair { " map.repairTiles " } else { " map.reloadTiles " };
            let params = if repair { json!({"index":0,"path":replacement}) } else { json!({"index":0}) };
            let depth = s.undo_depth();
            let (id, rx) = queue(&relay, &grant, command(1, name, params));
            assert_eq!(relay.poll(&mut s), 0);
            if reviewed {
                assert_eq!(grant.pending().len(), 1);
                assert!(relay.take_archive_read().is_none());
                grant.decide(id, true);
                assert_eq!(relay.poll(&mut s), 0);
            }
            let ahead = relay.take_archive_read().unwrap();
            assert!(ahead.reads(), "{name} must use the archive worker");
            let done = worker(ahead);
            assert!(rx.try_recv().is_err());
            assert_eq!(s.undo_depth(), depth);
            assert_eq!(s.map().tiles[0].url, original.to_str().unwrap());
            assert!(Arc::ptr_eq(&retained, &pmtiles::info_blocking(original.to_str().unwrap(), Duration::from_secs(10)).unwrap()));
            finish(&relay, &mut s, done);
            let answer = reply(&rx);
            assert_eq!(answer["result"]["isError"], false, "{answer}");
            assert_eq!(s.undo_depth(), depth + 1);
            assert_eq!(s.map().tiles[0].url, if repair { replacement.to_str().unwrap() } else { original.to_str().unwrap() });
            relay.stop();
        }
    }
}

#[test]
fn padded_archive_commands_without_authorization_do_not_start_a_read() {
    for repair in [false, true] {
        let (_root, mut s, original, replacement) = fixture();
        let relay = Relay::start(0).unwrap();
        let grant = relay.grant(false).unwrap();
        let name = if repair { " map.repairTiles " } else { " map.reloadTiles " };
        let params = if repair { json!({"index":0,"path":replacement}) } else { json!({"index":0}) };
        let (_, rx) = queue(&relay, &grant, command(1, name, params));
        assert_eq!(relay.poll(&mut s), 1);
        assert!(relay.take_archive_read().is_none());
        let answer = reply(&rx);
        assert_eq!(answer["result"]["isError"], true, "{answer}");
        assert_eq!(s.map().tiles[0].url, original.to_str().unwrap());
        relay.stop();
    }
}

fn layers(s: &mut Session) -> (u32, u32) {
    for name in ["A", "B"] {
        s.run("layer.template", &json!({"template":"points","name":name})).unwrap();
    }
    let a = s.map().layer_by_name("A").unwrap().id;
    let b = s.map().layer_by_name("B").unwrap().id;
    s.run("layer.select", &json!({"layer":a})).unwrap();
    (a, b)
}

#[test]
fn selecting_the_active_layer_is_owned_before_each_archive_pause() {
    for second_pause in [false, true] {
        let (_root, mut s, _, _) = fixture();
        let (a, b) = layers(&mut s);
        let before_a = s.map().layer(a).unwrap().renderer.clone();
        let before_b = s.map().layer(b).unwrap().renderer.clone();
        let relay = Relay::start(0).unwrap();
        let grant = relay.grant(true).unwrap();
        let mut batch = Vec::new();
        if second_pause {
            batch.push(command(1, "map.reloadTiles", json!({"index":0})));
        }
        batch.extend([
            command(2, "layer.select", json!({"layer":a})),
            // An unrelated failure must not erase a selection that actually succeeded.
            command(3, "layer.select", json!({"layer":"Missing"})),
            command(4, "map.reloadTiles", json!({"index":0})),
            command(5, "layer.color", json!({"color":"#123456"})),
        ]);
        let (_, rx) = queue(&relay, &grant, json!(batch));
        assert_eq!(relay.poll(&mut s), 0);
        if second_pause {
            let done = worker(relay.take_archive_read().unwrap());
            finish(&relay, &mut s, done);
            assert!(rx.try_recv().is_err());
        }
        assert_eq!(s.active_layer, Some(a));
        assert_eq!(lock(&relay.owner.shared.agents).get(&grant.token).unwrap().active, Some(a));
        let done = worker(relay.take_archive_read().unwrap());
        s.run("layer.select", &json!({"layer":b})).unwrap();
        finish(&relay, &mut s, done);
        let answer = reply(&rx);
        let replies = answer.as_array().unwrap();
        assert_eq!(replies.last().unwrap()["result"]["isError"], false, "{answer}");
        assert_ne!(s.map().layer(a).unwrap().renderer, before_a, "suffix must change the selected A");
        assert_eq!(s.map().layer(b).unwrap().renderer, before_b, "user-selected B must not be changed");
        assert_eq!(s.active_layer, Some(b), "the user's active layer is restored");
        relay.stop();
    }
}

#[test]
fn failed_or_unexecuted_selection_does_not_own_a_layer_at_archive_pause() {
    let (_root, mut s, _, _) = fixture();
    let (a, b) = layers(&mut s);
    let before_a = s.map().layer(a).unwrap().renderer.clone();
    let before_b = s.map().layer(b).unwrap().renderer.clone();
    let relay = Relay::start(0).unwrap();
    let grant = relay.grant(true).unwrap();
    let (_, rx) = queue(
        &relay,
        &grant,
        json!([
            command(1, "layer.select", json!({"layer":"Missing"})),
            command(2, "map.reloadTiles", json!({"index":0})),
            command(3, "layer.color", json!({"color":"#123456"})),
            command(4, "layer.select", json!({"layer":a})),
        ]),
    );
    assert_eq!(relay.poll(&mut s), 0);
    assert_eq!(lock(&relay.owner.shared.agents).get(&grant.token).unwrap().active, None);
    let done = worker(relay.take_archive_read().unwrap());
    s.run("layer.select", &json!({"layer":b})).unwrap();
    finish(&relay, &mut s, done);
    let answer = reply(&rx);
    assert_eq!(answer[0]["result"]["isError"], true, "{answer}");
    assert_eq!(answer[2]["result"]["isError"], false, "{answer}");
    assert_eq!(s.map().layer(a).unwrap().renderer, before_a);
    assert_ne!(s.map().layer(b).unwrap().renderer, before_b, "without an executed selection the suffix follows the user");
    assert_eq!(s.active_layer, Some(b));
    relay.stop();
}

#[test]
fn archive_batch_rejects_changed_context_before_agent_reentry_at_every_pause() {
    for second_pause in [false, true] {
        for replace_project in [false, true] {
            for completion in ["stale", "cancelled", "success"] {
                let (_root, mut s, _, _) = fixture();
                let (a, _) = layers(&mut s);
                let map_a = s.map().name.clone();
                let renderer_a = s.map().layer(a).unwrap().renderer.clone();
                s.run("map.new", &json!({"name":"Other"})).unwrap();
                let (other, user) = layers(&mut s);
                assert_eq!(a, other, "layer IDs are map-local");
                s.run("map.activate", &json!({"name":map_a})).unwrap();
                let relay = Relay::start(0).unwrap();
                let grant = relay.grant(true).unwrap();
                let mut batch = vec![command(1, "layer.select", json!({"layer":a}))];
                if second_pause {
                    batch.push(command(2, "map.reloadTiles", json!({"index":0})));
                }
                batch.extend([
                    command(7, "map.reloadTiles", json!({"index":0})),
                    command(7, "layer.color", json!({"color":"#123456"})),
                    json!({"jsonrpc":"2.0","id":"observer","method":"ping"}),
                    json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"run_command","arguments":{"command":"layer.color","params":{"color":"#654321"}}}}),
                    json!({"jsonrpc":"2.0","id":"response","result":{}}),
                    command(9, "map.reloadTiles", json!({"index":0})),
                ]);
                let (_, rx) = queue(&relay, &grant, json!(batch));
                assert_eq!(relay.poll(&mut s), 0);
                if second_pause {
                    let done = worker(relay.take_archive_read().unwrap());
                    finish(&relay, &mut s, done);
                    assert!(rx.try_recv().is_err());
                }
                let done = worker(relay.take_archive_read().unwrap());
                if replace_project {
                    let opened = s.opened;
                    s.run("project.new", &json!({"name":"Replacement"})).unwrap();
                    assert_ne!(s.opened, opened);
                    let (replacement, _) = layers(&mut s);
                    assert_eq!(replacement, a);
                } else {
                    s.run("map.activate", &json!({"name":"Other"})).unwrap();
                }
                s.run("layer.select", &json!({"layer":user})).unwrap();
                s.actor = Some("current user".into());
                let before = serde_json::to_value(&s.project).unwrap();
                let agent_active = lock(&relay.owner.shared.agents).get(&grant.token).unwrap().active;
                relay.begin_archive_landing(&s);
                let result = match completion {
                    "stale" => done.and_then(|read| reads::read_land(&mut s, read)),
                    "cancelled" => Err("Cancelled archive fixture".into()),
                    _ => Ok(json!({"archive":"actual successful result"})),
                };
                let failed = result.is_err();
                let expected = result.as_ref().err().cloned().unwrap_or_else(|| "actual successful result".into());
                relay.finish_archive_read(&mut s, result);
                let answer = reply(&rx);
                let replies = answer.as_array().unwrap();
                let pending = if second_pause { 2 } else { 1 };
                assert_eq!(replies.len(), pending + 4, "{answer}");
                assert_eq!(answer[0]["id"], 1);
                assert_eq!(answer[0]["result"]["isError"], false, "{answer}");
                if second_pause {
                    assert_eq!(answer[1]["id"], 2);
                    assert_eq!(answer[1]["result"]["isError"], false, "{answer}");
                }
                assert_eq!(answer[pending]["id"], 7);
                assert_eq!(answer[pending]["result"]["isError"], failed, "{answer}");
                assert!(answer[pending]["result"]["content"][0]["text"].as_str().unwrap().contains(&expected), "{answer}");
                for (reply, id) in replies[pending + 1..].iter().zip([json!(7), json!("observer"), json!(9)]) {
                    assert_eq!(reply["id"], id, "{answer}");
                    assert!(reply.get("error").is_some() || reply["result"]["isError"] == true, "{answer}");
                    assert!(reply.to_string().contains("context changed"), "{answer}");
                }
                assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "no suffix command may mutate either map");
                if !replace_project {
                    assert_eq!(s.project.maps[0].layer(a).unwrap().renderer, renderer_a);
                    assert_eq!(s.map().name, "Other");
                }
                assert_eq!(s.active_layer, Some(user));
                assert_eq!(s.actor.as_deref(), Some("current user"));
                assert_eq!(lock(&relay.owner.shared.agents).get(&grant.token).unwrap().active, agent_active);
                assert!(relay.take_archive_read().is_none(), "the suffix's second archive command must not dispatch");
                relay.finish_archive_read(&mut s, Err("duplicate completion".into()));
                assert!(rx.try_recv().is_err(), "one terminal reply only");
                relay.stop();
            }
        }
    }
}

#[test]
fn archive_batch_allows_explicit_map_changes_between_pauses() {
    let (_root, mut s, original, _) = fixture();
    let (a, _) = layers(&mut s);
    let map_a = s.map().name.clone();
    let before_a = s.map().layer(a).unwrap().renderer.clone();
    s.run("map.new", &json!({"name":"Other"})).unwrap();
    s.run("map.addTiles", &json!({"url":original,"name":"Other archive","overlay":true})).unwrap();
    let (b, _) = layers(&mut s);
    let before_b = s.map().layer(b).unwrap().renderer.clone();
    s.run("map.activate", &json!({"name":map_a})).unwrap();
    let relay = Relay::start(0).unwrap();
    let grant = relay.grant(true).unwrap();
    let (_, rx) = queue(
        &relay,
        &grant,
        json!([
            command(1, "map.reloadTiles", json!({"index":0})),
            command(2, "map.activate", json!({"name":"Other"})),
            command(3, "layer.select", json!({"layer":b})),
            command(4, "map.reloadTiles", json!({"index":0})),
            command(5, "layer.color", json!({"color":"#123456"})),
        ]),
    );
    assert_eq!(relay.poll(&mut s), 0);
    let done = worker(relay.take_archive_read().unwrap());
    finish(&relay, &mut s, done);
    assert!(rx.try_recv().is_err());
    assert_eq!(s.map().name, "Other");
    let done = worker(relay.take_archive_read().unwrap());
    finish(&relay, &mut s, done);
    let answer = reply(&rx);
    let replies = answer.as_array().unwrap();
    assert_eq!(replies.len(), 5, "{answer}");
    assert!(replies.iter().all(|reply| reply["result"]["isError"] == false), "{answer}");
    assert_eq!(s.project.maps[0].layer(a).unwrap().renderer, before_a);
    assert_ne!(s.map().layer(b).unwrap().renderer, before_b);
    assert_eq!(s.map().name, "Other");
    relay.stop();
}

#[test]
fn archive_batch_resumes_same_context_after_validation_failure_or_cancellation() {
    for error in ["Archive validation failed", "Cancelled"] {
        let (_root, mut s, _, _) = fixture();
        let (a, _) = layers(&mut s);
        let before = s.map().layer(a).unwrap().renderer.clone();
        let relay = Relay::start(0).unwrap();
        let grant = relay.grant(true).unwrap();
        let (_, rx) = queue(
            &relay,
            &grant,
            json!([
                command(1, "layer.select", json!({"layer":a})),
                command(2, "map.reloadTiles", json!({"index":0})),
                command(3, "layer.color", json!({"color":"#123456"})),
            ]),
        );
        assert_eq!(relay.poll(&mut s), 0);
        drop(relay.take_archive_read().unwrap());
        relay.begin_archive_landing(&s);
        relay.finish_archive_read(&mut s, Err(error.into()));
        let answer = reply(&rx);
        assert_eq!(answer[1]["result"]["isError"], true, "{answer}");
        assert!(answer[1]["result"]["content"][0]["text"].as_str().unwrap().contains(error), "{answer}");
        assert_eq!(answer[2]["result"]["isError"], false, "{answer}");
        assert_ne!(s.map().layer(a).unwrap().renderer, before);
        relay.stop();
    }
}
