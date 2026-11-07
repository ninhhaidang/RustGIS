//! Review regressions exercise public admission and ownership without external traffic.
use super::*;
use crate::policy::{AgentAccess, Call, Effect, Effects, Outcome, Revocation, Scope};
use std::sync::Arc;

struct Live;
impl Revocation for Live {
    fn allows(&self, _: Effects) -> bool {
        true
    }
}

/// A live agent whose external access setting is enforced by the normal decision table.
fn agent(access: AgentAccess) -> Scope {
    Scope::agent(access, true, "review-boundaries", None, Arc::new(Live))
}

#[test]
fn permission_review_authentication_changes_are_reserved() {
    let s = world();
    for (id, params) in [
        ("arcgis.signIn", json!({})),
        ("arcgis.token", json!({"url":"https://auth-review.example.invalid/arcgis/rest/services","token":"RGSYNTH_review_auth"})),
        ("arcgis.signOut", json!({})),
        ("arcgis.cancelSignIn", json!({})),
    ] {
        let call = Call::command(id, params);
        assert!(matches!(s.review(&agent(AgentAccess::Allow), &call), Outcome::Refused(_)), "{id} is reserved even with external access");
        assert!(
            matches!(s.review(&Scope::assistant(1461, Arc::new(Live)), &call), Outcome::NeedsApproval(_)),
            "{id} needs explicit approval"
        );
        assert!(matches!(s.review(&agent(AgentAccess::Full), &call), Outcome::Allowed), "full native-client access remains explicit");
    }
}

#[test]
fn permission_review_connected_folder_availability_requires_files() {
    let mut s = world();
    s.project.folders.push("not-opened-review-folder".into());
    let call = Call::command("project.connections", Value::Null);
    assert!(matches!(s.review(&agent(AgentAccess::Refuse), &call), Outcome::Refused(_)));
    assert!(matches!(s.review(&agent(AgentAccess::Review), &call), Outcome::NeedsApproval(_)));
    assert!(matches!(s.review(&agent(AgentAccess::Allow), &call), Outcome::Allowed));
    assert!(matches!(s.review(&Scope::assistant(1461, Arc::new(Live)), &call), Outcome::NeedsApproval(_)));
}

#[test]
fn permission_review_memory_only_commands_need_no_external_access() {
    let mut s = world();
    let mut errors = Vec::new();
    for (id, params) in [
        ("graphics.add", json!({"shape":"text","points":[[0,0]],"text":"Review note"})),
        ("cache.empty", json!({})),
        ("cache.cancel", json!({})),
        ("map.body", json!({"name":"moon","basemap":false})),
    ] {
        if let Err(error) = s.with_scope(agent(AgentAccess::Refuse), |s| s.run(id, &params)).unwrap() {
            errors.push((id, error));
        }
    }
    assert!(errors.is_empty(), "in-memory work was refused: {errors:?}");
    assert_eq!(s.map().body().id, "moon");
    for call in [
        Call::command("graphics.add", json!({"shape":"picture","points":[[0,0],[1,1]],"path":"missing-review.png"})),
        Call::command("cache.emptyIndexes", json!({})),
        Call::command("map.body", json!({"name":"earth"})),
    ] {
        assert!(matches!(s.review(&agent(AgentAccess::Refuse), &call), Outcome::Refused(_)), "real external reach stays gated");
    }
}

#[test]
fn permission_review_postgis_write_declares_network_reach() {
    struct Without(Effect);
    impl Revocation for Without {
        fn allows(&self, effects: Effects) -> bool {
            !effects.contains(self.0)
        }
    }
    let s = world();
    let call = Call::command("pg.saveEdits", json!({"layer":"Cities"}));
    for effect in [Effect::Network, Effect::Files] {
        let scope = Scope::assistant(1462, Arc::new(Without(effect)));
        assert!(matches!(s.review(&scope, &call), Outcome::Refused(_)), "a database write must check connection authority before any I/O");
        assert!(matches!(s.review(&scope, &Call::command("pg.pendingEdits", json!({"layer":"Cities"}))), Outcome::Allowed));
    }
}

#[test]
fn permission_review_url_claim_rejects_another_sessions_request() {
    let mut a = world();
    let mut b = world();
    for (s, url) in [(&mut a, "https://request-a.invalid/a.geojson"), (&mut b, "https://request-b.invalid/b.geojson")] {
        s.request_url(&json!({"url":url})).unwrap();
    }
    let qa = a.take_url_requests().pop().unwrap();
    let qb = b.take_url_requests().pop().unwrap();
    assert_eq!(qa.key, qb.key, "independent sessions reuse their own queue sequence");
    assert!(b.download_started_for(&qa).is_err(), "a foreign opaque request must not consume the matching local queue key");
    let id = b.download_started_for(&qb).unwrap();
    assert!(b.download_continues(id).is_ok(), "the rejected claim preserves the rightful request");
    assert!(a.download_started_for(&qa).is_ok());
    assert!(b.download_started_for(&qb).is_err(), "a rightful claim still has one use");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_review_non_s3_download_survives_an_unrelated_storage_login() {
    if isolate_process_test("tests::review_boundaries::permission_review_non_s3_download_survives_an_unrelated_storage_login") {
        return;
    }
    use crate::render::rustgis_tiles::s3;
    let connection = |name: &str, bucket: &str, endpoint: &str| s3::Connection {
        name: name.into(),
        endpoint: Some(endpoint.into()),
        region: s3::DEFAULT_REGION.into(),
        buckets: vec![bucket.into()],
        credentials: None,
    };
    s3::connect(connection("review-owned", "review-owned", "https://owned-storage.invalid")).unwrap();
    let mut s = world();
    let mut ids = Vec::new();
    for url in ["https://plain-request.invalid/data.geojson", "s3://review-owned/data.geojson"] {
        s.request_url(&json!({"url":url})).unwrap();
        let request = s.take_url_requests().pop().unwrap();
        ids.push(s.download_started_for(&request).unwrap());
    }
    let plain_still = s.download_still(ids[0]).unwrap();
    let storage_still = s.download_still(ids[1]).unwrap();
    s3::connect(connection("review-unrelated", "review-unrelated", "https://other-storage.invalid")).unwrap();
    assert!(s.download_continues(ids[0]).is_ok(), "an unrelated S3 login cannot change this HTTPS request's account choice");
    assert!(plain_still(), "the actual sender keeps the same account choice");

    // A service can follow discovered links even when its first URL looks like a data file.
    s.request_url(&json!({"url":"https://plain-request.invalid/data.geojson","service":true})).unwrap();
    let discovery = s.take_url_requests().pop().unwrap();
    let discovery = s.download_started_for(&discovery).unwrap();
    s3::connect(connection("review-owned", "review-owned", "https://changed-storage.invalid")).unwrap();
    assert!(!storage_still(), "a changed account refuses the actual sender before another request");
    assert!(s.download_continues(ids[1]).is_err(), "a changed endpoint for the actual S3 source still revokes its continuation");
    assert!(s.download_continues(discovery).is_err(), "unresolved service discovery keeps conservative account checks");
    assert!(plain_still(), "a fixed HTTPS address still has no S3 signer");

    let mut newly_covering = connection("review-covering", "review-covering", "https://plain-request.invalid");
    newly_covering.credentials = Some(s3::Credentials {
        access_key_id: "RGSYNTH_review_covering".into(),
        secret_access_key: "RGSYNTH_review_covering_secret".into(),
        session_token: None,
    });
    s3::connect(newly_covering).unwrap();
    assert!(!plain_still(), "absence is authority too: a newly covering signer requires a fresh request");
    assert!(s.download_continues(ids[0]).is_err());
    for name in ["review-owned", "review-unrelated", "review-covering"] {
        s3::disconnect(name);
    }
}

/// Allows ordinary agent work while refusing one source capability before any I/O.
fn without_effect(effect: Effect) -> Scope {
    struct Without(Effect);
    impl Revocation for Without {
        fn allows(&self, effects: Effects) -> bool {
            !effects.contains(self.0)
        }
    }
    Scope::agent(AgentAccess::Allow, true, "review-source-reach", None, Arc::new(Without(effect)))
}

/// Two dates and two camera stops exercise the real time and animation render owners.
fn rendering_session() -> Session {
    let mut s = world();
    s.run("time.set", &json!({"dates":["2020-01-01","2020-01-02"],"date":"2020-01-01"})).unwrap();
    for _ in 0..2 {
        s.run("animation.add", &json!({"seconds":0.1})).unwrap();
    }
    s
}

#[test]
fn permission_review_render_owners_require_tile_network_access() {
    let mut s = rendering_session();
    s.map_mut().tiles.push(rustgis_map::TileLayer::new(
        "Review imagery",
        rustgis_map::TileKind::Raster,
        "https://render-review.invalid/{z}/{x}/{y}.png",
    ));
    let scope = without_effect(Effect::Network);
    let mut admitted = Vec::new();
    for (id, params) in [
        ("time.export", json!({"path":"review-frames.zip"})),
        ("animation.export", json!({"path":"review-camera.zip"})),
        ("project.thumbnail", json!({"create":true})),
        ("map.thumbnail", json!({"create":true})),
    ] {
        if !matches!(s.review(&scope, &Call::command(id, params)), Outcome::Refused(_)) {
            admitted.push(id);
        }
    }
    for id in ["project.thumbnail", "map.thumbnail"] {
        for params in [json!({"image":"supplied-image"}), json!({"create":true,"clear":true})] {
            assert!(matches!(s.review(&scope, &Call::command(id, params)), Outcome::Allowed), "{id} does not render this branch");
        }
        s.run_scoped(scope.clone(), &Call::command(id, json!({"create":true,"clear":true}))).unwrap();
    }
    assert!(admitted.is_empty(), "rendering web tiles needs network authority: {admitted:?}");
}

#[test]
fn permission_review_render_owners_bind_raw_tile_routes() {
    let mut unchanged = Vec::new();
    for (id, params) in [
        ("time.export", json!({"path":"review-frames.zip"})),
        ("animation.export", json!({"path":"review-camera.zip"})),
        ("project.thumbnail", json!({"create":true})),
        ("map.thumbnail", json!({"create":true})),
    ] {
        let mut s = rendering_session();
        s.map_mut().tiles.push(rustgis_map::TileLayer::new(
            "Review imagery",
            rustgis_map::TileKind::Raster,
            "https://render-review.invalid/{z}/{x}/{y}.png?token=RGSYNTH_review_one",
        ));
        let call = Call::command(id, params);
        let scope = Scope::assistant(1463, Arc::new(Live));
        let Outcome::NeedsApproval(approved) = s.review(&scope, &call) else { panic!("{id} needs approval") };
        assert!(!format!("{:?}", approved.review()).contains("RGSYNTH_review_"));
        let remembered = approved.remember().unwrap();
        s.map_mut().tiles[0].url = "https://render-review.invalid/{z}/{x}/{y}.png?token=RGSYNTH_review_two".into();
        let Outcome::NeedsApproval(changed) = s.review(&scope, &call) else { panic!("{id} still needs approval") };
        if changed.covered_by(&remembered) {
            unchanged.push(id);
        }
    }
    assert!(unchanged.is_empty(), "remembered consent cannot cover changed raw tile routes: {unchanged:?}");
}

#[test]
fn permission_review_time_export_includes_only_selected_frame_routes() {
    let mut s = rendering_session();
    let later = rustgis_data::time::parse_input(&json!("2020-01-02")).unwrap();
    let mut tile = rustgis_map::TileLayer::new(
        "Future review imagery",
        rustgis_map::TileKind::Raster,
        "https://time-render-review.invalid/initial/{z}/{x}/{y}.png",
    );
    tile.time_urls = vec![(later, "https://time-render-review.invalid/later/{z}/{x}/{y}.png?token=RGSYNTH_future_one".into())];
    s.map_mut().tiles.push(tile);
    s.run("time.goTo", &json!({"step":0})).unwrap();
    assert!(!s.map().tiles[0].draws(), "the current frame has no imagery");
    let call = |date| Call::command("time.export", json!({"path":"review-time.zip","from":date,"to":date}));
    assert!(
        matches!(s.review(&without_effect(Effect::Network), &call("2020-01-01")), Outcome::Allowed),
        "excluded imagery adds no network reach"
    );
    let later_call = call("2020-01-02");
    let refused = matches!(s.review(&without_effect(Effect::Network), &later_call), Outcome::Refused(_));
    let scope = Scope::assistant(1464, Arc::new(Live));
    let Outcome::NeedsApproval(approved) = s.review(&scope, &later_call) else { panic!("time export needs approval") };
    assert!(!format!("{:?}", approved.review()).contains("RGSYNTH_future_"));
    let remembered = approved.remember().unwrap();
    s.map_mut().tiles[0].time_urls[0].1 = "https://time-render-review.invalid/later/{z}/{x}/{y}.png?token=RGSYNTH_future_two".into();
    let Outcome::NeedsApproval(changed) = s.review(&scope, &later_call) else { panic!("changed time export needs approval") };
    assert!(
        refused && !changed.covered_by(&remembered),
        "a selected later frame needs network authority ({refused}) and exact route consent"
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn permission_review_local_tile_archives_require_files() {
    let dir = std::env::temp_dir().join(format!("rustgis-review-tile-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("review.pmtiles");
    std::fs::write(&path, empty_pmtiles([-24.5, 63.3, -13.5, 66.6])).unwrap();
    let params = json!({"url":path.display().to_string()});
    let mut s = world();
    let mut admitted = Vec::new();
    for (kind, source) in [("pmtiles", path.clone()), ("mbtiles", dir.join("review.mbtiles"))] {
        let call = Call::command("map.addTiles", json!({"url":source.display().to_string(),"kind":kind}));
        if !matches!(s.review(&without_effect(Effect::Files), &call), Outcome::Refused(_)) {
            admitted.push(kind);
        }
    }
    for call in [
        Call::command("map.addTiles", json!({"url":"upload:review.mbtiles","kind":"mbtiles"})),
        Call::command("sourcecoop.list", json!({"account":"review","product":"review","path":"folder/"})),
    ] {
        assert!(
            matches!(s.review(&without_effect(Effect::Files), &call), Outcome::Allowed),
            "memory archives and remote object prefixes do not read files"
        );
    }
    plugin("eng-review-local-tile", &["edit", "network"]);
    let before = s.map().tiles.len();
    let denied = forward(&mut s, "eng-review-local-tile", "map.addTiles", params.clone());
    let unchanged = s.map().tiles.len() == before;
    rustgis_plugins::unload("eng-review-local-tile");
    plugin("eng-review-file-tile", &["edit", "network", "files"]);
    let allowed = forward(&mut world(), "eng-review-file-tile", "map.addTiles", params);
    rustgis_plugins::unload("eng-review-file-tile");
    std::fs::remove_dir_all(dir).unwrap();
    assert!(allowed.is_ok(), "the same synthetic archive is available with files permission: {allowed:?}");
    assert!(admitted.is_empty(), "local archive metadata needs files authority: {admitted:?}");
    assert!(
        unchanged && denied.as_ref().err().is_some_and(|e| e.contains("files")),
        "a plugin without files cannot insert/read the archive: {denied:?}"
    );
}

#[test]
fn permission_review_tile_metadata_helpers_require_source_access() {
    let mut s = world();
    s.map_mut().tiles.push(rustgis_map::TileLayer::new(
        "Review archive",
        rustgis_map::TileKind::PmTiles,
        std::env::temp_dir().join("rustgis-review-metadata.pmtiles").display().to_string(),
    ));
    let mut wrong = Vec::new();
    for bounds in [None, Some([-24.5, 63.3, -13.5, 66.6])] {
        s.map_mut().tiles[0].bounds = bounds;
        for (id, params) in [
            ("map.search", json!({"query":"Review archive","go":true})),
            ("map.tiles", json!({})),
            ("info.project", json!({})),
            ("map.setTiles", json!({"index":0,"opacity":0.5})),
        ] {
            let needs_files = id != "map.search" || bounds.is_none();
            let refused = matches!(s.review(&without_effect(Effect::Files), &Call::command(id, params)), Outcome::Refused(_));
            if refused != needs_files {
                wrong.push(format!("{id}: bounds={}, needs files={needs_files}", bounds.is_some()));
            }
        }
    }
    assert!(matches!(
        s.review(&without_effect(Effect::Files), &Call::command("map.search", json!({"query":"Review archive"}))),
        Outcome::Allowed
    ));
    s.map_mut().tiles[0] = rustgis_map::TileLayer::new("Review COG", rustgis_map::TileKind::Cog, "https://cog-review.invalid/image.tif");
    for bounds in [None, Some([-24.5, 63.3, -13.5, 66.6])] {
        s.map_mut().tiles[0].bounds = bounds;
        for (id, params, needs_network) in [
            ("map.search", json!({"query":"Review COG","go":true}), bounds.is_none()),
            ("map.setTiles", json!({"index":0,"bands":[1]}), true),
            ("map.setTiles", json!({"index":0,"opacity":0.5}), false),
        ] {
            let refused = matches!(s.review(&without_effect(Effect::Network), &Call::command(id, params)), Outcome::Refused(_));
            if refused != needs_network {
                wrong.push(format!("{id}: bounds={}, needs network={needs_network}", bounds.is_some()));
            }
        }
    }
    assert!(wrong.is_empty(), "metadata readers must declare their actual source reach: {wrong:?}");
}

/// Rejects an over-limit script before its first edit instead of silently running a prefix.
#[test]
fn permission_review_script_line_limit_refuses_before_any_edit() {
    let mut s = world();
    let before = serde_json::to_value(&s.project).unwrap();
    let prefix = format!(
        "layer.rename {{\"layer\":\"Cities\",\"name\":\"Renamed\"}}\n{}",
        "# kept\n".repeat(crate::cmd::workflows::MAX_SCRIPT_LINES - 1)
    );
    let oversized = format!("{prefix}map.basemap {{\"name\":\"blank\"}}");
    let result = s.run("script.run", &json!({"script":oversized}));
    assert!(result.is_err(), "a script over the line limit must be refused, not partially run");
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    s.run("script.run", &json!({"script":prefix})).unwrap();
    assert!(s.map().layer_by_name("Renamed").is_some(), "the exact boundary remains supported");
}
