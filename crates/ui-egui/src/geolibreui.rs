//! Asynchronous Zarr slices for imported projects, independent of the Add Zarr dialog.
//! Uses the same metadata, planning and assembly functions as the dialog, keeping each import's
//! state separate so simultaneous stores and a user-operated picker cannot overwrite each other.
use crate::{RustGisApp, Slot};
use rustgis_engine::data::raster::{geotiff, zarr};
use rustgis_engine::render::rustgis_tiles::cache;
use serde_json::{Value, json};
use std::collections::HashMap;

type Answer = Slot<Result<Vec<u8>, String>>;

/// One active imported store shares this budget between retained bytes and its download batch.
const MAX_IMPORT_BYTES: usize = 128 << 20;

/// One bounded metadata/chunk batch, followed by a single layer download completion.
pub struct Read {
    id: u64,
    params: Value,
    root: String,
    queue: Vec<String>,
    waiting: Vec<(String, Answer)>,
    got: HashMap<String, Vec<u8>>,
    phase: Option<(zarr::Store, zarr::Plan)>,
    bytes: usize,
    /// Missing keys can return 403; report access denial only if no metadata was readable.
    forbidden: usize,
    /// Retain a failed batch until its callbacks drain before another store can start.
    error: Option<String>,
}

/// Starts an import read; the engine has already reserved its layer and drawing position.
pub fn start(app: &mut RustGisApp, p: &Value, claim: Option<u64>) -> Result<(), String> {
    if app.ui.geolibre_zarr.len() >= 16 {
        return Err(tl!("16 imported Zarr stores are already queued").into());
    }
    let root = p.get("url").and_then(Value::as_str).ok_or(tl!("Zarr URL is required"))?.trim_end_matches('/').to_string();
    let id = claim.unwrap_or_else(|| app.session.download_started());
    app.ui.geolibre_zarr.push(Read {
        id,
        params: p.clone(),
        root,
        queue: zarr::METADATA_KEYS.iter().map(|s| s.to_string()).collect(),
        waiting: Vec::new(),
        got: HashMap::new(),
        phase: None,
        bytes: 0,
        forbidden: 0,
        error: None,
    });
    Ok(())
}

impl Read {
    /// Polls one batch. Missing chunks retain the format's fill values; other errors stop it.
    fn poll(&mut self) -> Option<Result<Vec<u8>, String>> {
        let mut error = self.error.take();
        self.waiting.retain(|(key, slot)| match slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(Ok(bytes)) => {
                self.bytes = self.bytes.saturating_add(bytes.len());
                if self.bytes > MAX_IMPORT_BYTES {
                    error = Some(tl!("Imported Zarr slice exceeds the 128 MiB download limit").into());
                }
                self.got.insert(key.clone(), bytes);
                false
            }
            Some(Err(e)) => {
                if !rustgis_engine::cmd::zarr::is_absent(&e) {
                    error = Some(e);
                } else if cache::http_status(&e) == Some(403) {
                    self.forbidden += 1;
                }
                false
            }
            None => true,
        });
        self.error = error;
        if self.waiting.is_empty()
            && let Some(error) = self.error.take()
        {
            return Some(Err(error));
        }
        // Wait for the entire batch before reserving another: partially finished batches
        // still own their byte allowance in callback slots, even before we consume them.
        if !self.waiting.is_empty() {
            return None;
        }
        let count = self.queue.len().min(6);
        let allowance = MAX_IMPORT_BYTES.saturating_sub(self.bytes) / count.max(1);
        if count > 0 && allowance == 0 {
            return Some(Err(tl!("Imported Zarr slice exceeds the 128 MiB download limit").into()));
        }
        while self.waiting.len() < count
            && let Some(key) = self.queue.pop()
        {
            let slot: Answer = Slot::default();
            let land = slot.clone();
            let file_limit = if self.phase.is_none() { zarr::MAX_METADATA } else { zarr::MAX_CHUNK_FILE as usize };
            let limit = file_limit.min(allowance);
            cache::download_limited(
                &format!("{}/{key}", self.root),
                limit,
                Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
            );
            self.waiting.push((key, slot));
        }
        if !self.queue.is_empty() || !self.waiting.is_empty() {
            return None;
        }
        if let Some((store, plan)) = self.phase.take() {
            if self.forbidden > 0 {
                return Some(Err(crate::i18n::fmt(
                    "{url}: access denied (HTTP 403) to {count} of the slice’s chunks",
                    &[("url", &rustgis_engine::sources::address_in_message(&self.root)), ("count", &self.forbidden.to_string())],
                )));
            }
            let name = self.params.get("name").and_then(Value::as_str).unwrap_or("Zarr");
            return Some(
                zarr::assemble(&store, &plan, &self.got, name, None)
                    .map_err(|e| e.to_string())
                    .and_then(|r| geotiff::write(&r, 0).map_err(|e| e.to_string())),
            );
        }
        if self.got.is_empty() && self.forbidden > 0 {
            return Some(Err(crate::i18n::fmt(
                "{url}: access denied (HTTP 403)",
                &[("url", &rustgis_engine::sources::address_in_message(&self.root))],
            )));
        }
        self.forbidden = 0;
        let docs = std::mem::take(&mut self.got).into_iter().collect::<Vec<_>>();
        let result = (|| {
            let store = zarr::parse_store(&docs).map_err(|e| e.to_string())?;
            let mut sel = rustgis_engine::cmd::zarr::selection(&self.params);
            rustgis_engine::cmd::zarr::pick_variable(&store, &mut sel)?;
            let plan = zarr::plan(&store, &sel).map_err(|e| e.to_string())?;
            self.queue = plan.keys.clone();
            self.phase = Some((store, plan));
            Ok(())
        })();
        result.err().map(Err)
    }
}

/// Finishes through the same target-layer replacement as other imported URL data.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let mut done = Vec::new();
    // Serial reads bound aggregate buffers and assembly to one store. Queued jobs hold
    // parameters only; they cannot start downloads until earlier stores complete.
    if let Some(id) = app.ui.geolibre_zarr.first().map(|job| job.id) {
        let allowed = app.session.download_continues(id);
        let still = app.session.download_still(id);
        let result = match allowed {
            Err(e) => Some(Err(e)),
            Ok(()) => cache::with_optional_still(still, || app.ui.geolibre_zarr.first_mut().and_then(Read::poll)),
        };
        if let Some(result) = result {
            let Some(job) = app.ui.geolibre_zarr.first() else {
                return;
            };
            done.push((job.id, job.root.clone(), job.params.clone(), result));
            app.ui.geolibre_zarr.remove(0);
        }
    }
    for (id, url, mut params, result) in done {
        match result {
            Ok(bytes) => {
                params["geolibreZarr"] = json!(true);
                app.add_landed(crate::serviceui::Landed { id, url, params, origin: None }, bytes);
            }
            Err(e) => {
                app.session.download_failed(id);
                rustgis_engine::cmd::geolibre::discard_pending(&mut app.session, &params);
                app.session.notify(rustgis_engine::sources::message_with_address(&url, &e), true);
            }
        }
    }
    if !app.ui.geolibre_zarr.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A minimal consolidated store with geographic coordinates and one value array.
    fn metadata() -> Value {
        let array = |shape: Value| json!({"zarr_format":2,"shape":shape,"chunks":shape,"dtype":"<f8","compressor":null,"filters":null,"order":"C","fill_value":null});
        let metadata = json!({"zarr_consolidated_format":1,"metadata":{
            ".zgroup":{"zarr_format":2},
            "value/.zarray":array(json!([2,2])),"value/.zattrs":{"_ARRAY_DIMENSIONS":["lat","lon"]},
            "lat/.zarray":array(json!([2])),"lat/.zattrs":{"_ARRAY_DIMENSIONS":["lat"],"units":"degrees_north"},
            "lon/.zarray":array(json!([2])),"lon/.zattrs":{"_ARRAY_DIMENSIONS":["lon"],"units":"degrees_east"}
        }});
        metadata
    }

    /// Ready callback slots exercise HTTP handling without issuing network requests.
    fn reader(answers: Vec<(&str, Result<Vec<u8>, String>)>) -> Read {
        Read {
            id: 0,
            params: json!({"variable":"value"}),
            root: "https://example.org/grid.zarr".into(),
            queue: Vec::new(),
            waiting: answers
                .into_iter()
                .map(|(key, answer)| {
                    let slot = Slot::default();
                    *slot.lock().unwrap() = Some(answer);
                    (key.into(), slot)
                })
                .collect(),
            got: HashMap::new(),
            phase: None,
            bytes: 0,
            forbidden: 0,
            error: None,
        }
    }

    #[test]
    fn geolibre_zarr_failed_batch_drains_before_the_next_store_can_start() {
        let mut read = reader(vec![("failed", Err("HTTP 500".into()))]);
        let pending: Answer = Slot::default();
        read.waiting.push(("still running".into(), pending.clone()));
        assert!(read.poll().is_none());
        assert!(read.error.is_some());
        assert_eq!(read.waiting.len(), 1);
        *pending.lock().unwrap() = Some(Ok(vec![0; 16]));
        assert!(read.poll().unwrap().unwrap_err().contains("HTTP 500"));
        assert!(read.waiting.is_empty());
    }

    #[test]
    fn geolibre_zarr_missing_metadata_403_keeps_readable_store() {
        let mut read = reader(vec![(".zmetadata", Ok(metadata().to_string().into_bytes())), ("zarr.json", Err("HTTP 403".into()))]);
        assert!(read.poll().is_none());
        assert!(read.phase.is_some());
        assert!(!read.queue.is_empty());
    }

    #[test]
    fn geolibre_zarr_unreadable_metadata_reports_access_denied() {
        let mut read = reader(vec![(".zmetadata", Err("HTTP 403".into())), ("zarr.json", Err("HTTP 404".into()))]);
        assert!(read.poll().unwrap().unwrap_err().contains("access denied (HTTP 403)"));
    }

    #[test]
    fn geolibre_zarr_refused_chunk_403_reports_access_denied() {
        let mut metadata = metadata();
        metadata["metadata"]["value/.zarray"]["chunks"] = json!([1, 2]);
        let store = zarr::parse_store(&[(".zmetadata".into(), metadata.to_string().into_bytes())]).unwrap();
        let plan = zarr::plan(&store, &zarr::Selection { variable: "value".into(), index: Vec::new() }).unwrap();
        let mut read = reader(vec![("value/1.0", Err("HTTP 403".into()))]);
        for (key, values) in [("value/0.0", vec![1.0f64, 2.0]), ("lat/0", vec![0.5, 1.5]), ("lon/0", vec![0.5, 1.5])] {
            read.got.insert(key.into(), values.iter().flat_map(|v| v.to_le_bytes()).collect());
        }
        read.phase = Some((store, plan));
        assert!(read.poll().unwrap().unwrap_err().contains("access denied (HTTP 403) to 1"));
    }

    #[test]
    fn geolibre_zarr_missing_chunk_404_preserves_fill_values() {
        let mut metadata = metadata();
        metadata["metadata"]["value/.zarray"]["chunks"] = json!([1, 2]);
        let store = zarr::parse_store(&[(".zmetadata".into(), metadata.to_string().into_bytes())]).unwrap();
        let plan = zarr::plan(&store, &zarr::Selection { variable: "value".into(), index: Vec::new() }).unwrap();
        let mut read = reader(vec![("value/1.0", Err("HTTP 404".into()))]);
        for (key, values) in [("value/0.0", vec![1.0f64, 2.0]), ("lat/0", vec![0.5, 1.5]), ("lon/0", vec![0.5, 1.5])] {
            read.got.insert(key.into(), values.iter().flat_map(|v| v.to_le_bytes()).collect());
        }
        read.phase = Some((store, plan));
        assert!(!read.poll().unwrap().unwrap().is_empty());
    }

    #[test]
    fn geolibre_zarr_completion_replaces_reserved_layer_after_map_switch() {
        let metadata = metadata();
        let store = zarr::parse_store(&[(".zmetadata".into(), metadata.to_string().into_bytes())]).unwrap();
        let plan = zarr::plan(&store, &zarr::Selection { variable: "value".into(), index: Vec::new() }).unwrap();
        let mut got = HashMap::new();
        for (key, values) in [("value/0.0", vec![1.0f64, 2.0, 3.0, 4.0]), ("lat/0", vec![0.5, 1.5]), ("lon/0", vec![0.5, 1.5])] {
            got.insert(key.into(), values.iter().flat_map(|v| v.to_le_bytes()).collect());
        }
        let mut app = RustGisApp::with_sample();
        let target = app.session.project.active_map;
        let mut ds = rustgis_engine::data::Dataset::new("Grid", rustgis_engine::data::GeometryType::Point, Vec::new());
        ds.source = Some("geolibre-pending:zarr-test".into());
        let layer = app.session.map_mut().add_dataset(ds);
        // A user edit differs from the saved opacity (0.4), which would produce 60.0.
        app.session.map_mut().layer_mut(layer).unwrap().transparency = 25.0;
        app.run("map.new", json!({"name":"Other"})).unwrap();
        let active = app.session.project.active_map;
        let id = app.session.download_started();
        let params = json!({"name":"Grid","geolibreLayer":{"token":"geolibre-pending:zarr-test","record":{"name":"Grid","opacity":0.4}}});
        app.ui.geolibre_zarr.push(Read {
            id,
            params,
            root: "https://example.org/grid.zarr".into(),
            queue: Vec::new(),
            waiting: Vec::new(),
            got,
            phase: Some((store, plan)),
            bytes: 0,
            forbidden: 0,
            error: None,
        });
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
        // Logic-only frames do not paint; explicitly discard their font texture updates.
        output.textures_delta.clear();
        assert!(app.ui.geolibre_zarr.is_empty());
        assert_eq!(app.session.project.active_map, active);
        let loaded = app.session.project.maps[target].layer(layer).unwrap();
        assert!(loaded.is_raster());
        assert_eq!(loaded.transparency, 25.0);
        assert!(app.session.map().layers.is_empty());
    }

    /// Uses the real importer to reserve a grouped URL layer without starting its read.
    fn deferred_zarr(app: &mut RustGisApp) -> (usize, u32) {
        app.session
            .run_leaving_downloads(
                "project.importGeolibre",
                &json!({"text":json!({
                    "name":"Deferred Zarr", "version":"0.2.0", "mapView":{},
                    "layers":[{"id":"overflow","name":"Overflow","type":"zarr","groupId":"folder",
                        "source":{"url":"https://example.invalid/overflow.zarr"}}],
                    "layerGroups":[{"id":"folder","name":"Folder"}]
                }).to_string()}),
            )
            .unwrap();
        (app.session.project.active_map, app.session.map().layer_by_name("Overflow").unwrap().id)
    }

    #[test]
    fn geolibre_zarr_start_failure_removes_reservation_through_app_frame() {
        for late_failure in [false, true] {
            let mut app = RustGisApp::with_sample();
            let (target, id) = deferred_zarr(&mut app);
            // Fill capacity after admission so the queued import reaches the reader's start check.
            // Existing native reader tickets keep sixteen held slots valid without any sends.
            if !late_failure {
                for _ in 0..16 {
                    let id = app.session.download_started();
                    app.ui.geolibre_zarr.push(Read {
                        id,
                        params: json!({}),
                        root: "https://example.org/grid.zarr".into(),
                        queue: Vec::new(),
                        waiting: vec![(".zmetadata".into(), Slot::default())],
                        got: HashMap::new(),
                        phase: None,
                        bytes: 0,
                        forbidden: 0,
                        error: None,
                    });
                }
                let unrelated = app.session.download_started();
                let error = app.add_url(&json!({"url":"https://example.org/unclaimed.zarr","geolibreZarr":true}), None, None).unwrap_err();
                assert!(error.contains("16 imported Zarr stores"), "{error}");
                assert!(app.session.download_is_pending(unrelated), "an unclaimed failure must leave other downloads alone");
                app.session.download_failed(unrelated);
            }
            assert!(app.session.map().groups.iter().any(|g| g.children.contains(&id)));
            if late_failure {
                // Claim/start in the origin map, then let the actual admitted reader fail on a
                // later frame after a map switch. Its cleanup owns the original reservation.
                let request = app.session.take_url_requests().pop().unwrap();
                app.add_url(request.params(), Some(&request), None).unwrap();
                let claim = app.ui.geolibre_zarr[0].id;
                assert!(app.session.download_is_pending(claim));
                assert!(app.session.download_still(claim).unwrap()(), "a successful start retains its live claim");
                app.ui.geolibre_zarr[0].queue.clear();
                app.ui.geolibre_zarr[0].error = Some("synthetic imported read failure".into());
                app.run("map.new", json!({"name":"Other"})).unwrap();
            }
            let active = app.session.project.active_map;
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
            output.textures_delta.clear();
            assert_eq!(app.session.project.active_map, active);
            assert!(app.session.project.maps[target].layer(id).is_none());
            assert!(app.session.project.maps[target].groups.iter().all(|g| !g.children.contains(&id)));
            assert_eq!(app.ui.geolibre_zarr.len(), if late_failure { 0 } else { 16 });
            assert!(app.session.take_url_requests().is_empty());
            // Queue/claim only: no reads start. A failed import must leave all 64 claim slots reusable.
            let mut claims = Vec::new();
            for n in 0..64 {
                app.session.request_url(&json!({"url":format!("https://example.org/capacity-{n}.csv")})).unwrap();
                let request = app.session.take_url_requests().pop().unwrap();
                let claim = app.session.download_started_for(&request);
                assert!(claim.is_ok(), "claim {n} after failed imported read: {claim:?}");
                claims.push(claim.unwrap());
            }
            for claim in claims {
                app.session.download_failed(claim);
            }
        }
    }

    /// Rejections before reader creation must retire both live and undo/redo reservations.
    #[test]
    fn geolibre_zarr_pre_start_rejection_retires_reservation_through_app_frame() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let _turn = crate::fetching::turn();
        let sent = Arc::new(AtomicUsize::new(0));
        let count = sent.clone();
        cache::set_fetcher(move |_, done| {
            count.fetch_add(1, Ordering::SeqCst);
            done(Err("unexpected imported read".into()));
        });
        for case in ["revoked", "map", "capacity"] {
            let mut app = RustGisApp::with_sample();
            let unrelated = serde_json::to_value(&app.session.project.maps[0]).unwrap();
            // These logins belong only to this Session, not a process-wide Earthdata account.
            let login = if case == "revoked" {
                let id = app
                    .session
                    .run(
                        "connection.new",
                        &json!({
                            "kind":"wms", "address":"https://retirement.invalid", "auth":"header", "keyName":"X-Key"
                        }),
                    )
                    .unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                app.session.run("connection.login", &json!({"id":id,"apiKey":"RGSYNTH_retirement_session"})).unwrap();
                Some(id)
            } else {
                None
            };
            let (target, id) = deferred_zarr(&mut app);
            let mut claims = Vec::new();
            if case == "capacity" {
                use rustgis_engine::policy;
                struct Live;
                impl policy::Revocation for Live {
                    fn allows(&self, _: policy::Effects) -> bool {
                        true
                    }
                }
                // Grow unrelated work after admission. Its owner drains only its own requests,
                // leaving the import queued for the actual app frame's start and cleanup path.
                app.session.active_layer = None;
                assert!(app.session.selected_layers().is_empty());
                let scope = policy::Scope::agent(policy::AgentAccess::Allow, true, "held-capacity", None, Arc::new(Live));
                claims = app
                    .session
                    .with_scope(scope, |s| -> Result<Vec<u64>, String> {
                        let mut claims = Vec::new();
                        for n in 0..64 {
                            s.request_url(&json!({"url":format!("https://example.invalid/held-{n}.csv")}))?;
                            let request = s.take_url_requests().pop().ok_or("held request was not queued")?;
                            claims.push(s.download_started_for(&request)?);
                        }
                        Ok(claims)
                    })
                    .unwrap()
                    .unwrap();
            }
            let token = app.session.map().layer(id).unwrap().dataset.source.clone().unwrap();
            assert!(app.session.map().groups.iter().any(|g| g.children.contains(&id)));
            // This ordinary edit leaves a real undo snapshot containing the reservation.
            app.session.run("map.rename", &json!({"name":"Queued import"})).unwrap();
            if let Some(login) = login {
                app.session.run("connection.logout", &json!({"id":login})).unwrap();
            }
            if case == "map" {
                let name = app.session.project.maps[0].name.clone();
                app.session.run("map.activate", &json!({"name":name})).unwrap();
            }
            let active = app.session.project.active_map;
            let mark = app.session.notice_mark();
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
            output.textures_delta.clear();
            assert_eq!(sent.load(Ordering::SeqCst), 0, "{case}: rejected before any send");
            assert_eq!(app.session.project.active_map, active, "{case}");
            assert_eq!(serde_json::to_value(&app.session.project.maps[0]).unwrap(), unrelated, "{case}: unrelated map");
            assert!(app.session.project.maps[target].layer(id).is_none(), "{case}: reservation removed");
            assert!(app.session.project.maps[target].groups.iter().all(|g| !g.children.contains(&id)), "{case}: group repaired");
            assert!(app.ui.geolibre_zarr.is_empty(), "{case}: no reader started");
            assert!(app.session.take_url_requests().is_empty());
            let errors = app.session.notices_since(mark).iter().filter(|n| n.error).count();
            assert_eq!(errors, 1, "{case}: one refusal notice");
            assert!(app.session.project.saved_text(None).is_ok(), "{case}: saving is no longer blocked");
            for claim in &claims {
                assert!(app.session.download_is_pending(*claim), "capacity refusal preserves other downloads");
                assert!(app.session.download_still(*claim).unwrap()());
            }
            for command in ["edit.undo", "edit.redo"] {
                app.session.run(command, &Value::Null).unwrap();
                let saved = app.session.project.saved_text(None).unwrap().0;
                assert!(!saved.contains(&token), "{case}: {command} cannot resurrect the reservation");
            }
            for claim in claims {
                app.session.download_failed(claim);
            }
        }
    }

    /// Failed foreign and duplicate claims must leave the rightful import untouched.
    #[test]
    fn geolibre_zarr_rejected_claim_preserves_the_original_request() {
        let mut a = RustGisApp::with_sample();
        let mut b = RustGisApp::with_sample();
        let (map_a, layer_a) = deferred_zarr(&mut a);
        let (map_b, layer_b) = deferred_zarr(&mut b);
        let qa = a.session.take_url_requests().pop().unwrap();
        let qb = b.session.take_url_requests().pop().unwrap();
        assert!(b.session.download_started_for(&qa).unwrap_err().contains("another session"));
        assert!(a.session.project.maps[map_a].layer(layer_a).is_some());
        assert!(b.session.project.maps[map_b].layer(layer_b).is_some());
        let foreign = b.session.with_scope(rustgis_engine::policy::Scope::peer(), |s| s.download_started_for(&qb)).unwrap();
        assert!(foreign.unwrap_err().contains("another caller"));
        assert!(b.session.project.maps[map_b].layer(layer_b).is_some());
        let id = b.session.download_started_for(&qb).unwrap();
        assert!(b.session.download_started_for(&qb).is_err(), "a duplicate does not retire the already claimed import");
        assert!(b.session.project.maps[map_b].layer(layer_b).is_some());
        assert!(b.session.download_continues(id).is_ok());
        assert!(a.session.download_started_for(&qa).is_ok(), "foreign rejection preserves the other session too");
    }

    /// A deterministic stage boundary covers invalidation after claim but before reader creation.
    #[test]
    fn geolibre_zarr_claimed_invalidation_retires_only_its_reservation() {
        let mut app = RustGisApp::with_sample();
        let connection = app
            .session
            .run(
                "connection.new",
                &json!({
                    "kind":"wms", "address":"https://claimed-retirement.invalid", "auth":"header", "keyName":"X-Key"
                }),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        app.session.run("connection.login", &json!({"id":connection,"apiKey":"RGSYNTH_claimed_retirement"})).unwrap();
        let (target, layer) = deferred_zarr(&mut app);
        let request = app.session.take_url_requests().pop().unwrap();
        let claimed = app.session.download_started_for(&request).unwrap();
        app.session.run("connection.logout", &json!({"id":connection})).unwrap();
        // This unrelated current request has no default dependency on the reserved layer.
        app.session.active_layer = None;
        assert!(app.session.selected_layers().is_empty());
        app.session.request_url(&json!({"url":"https://example.invalid/unrelated.csv"})).unwrap();
        let unrelated = app.session.take_url_requests().pop().unwrap();
        let unrelated = app.session.download_started_for(&unrelated).unwrap();
        let error = app.session.download_continues(claimed).unwrap_err();
        assert!(error.contains("account"), "{error}");
        assert!(!app.session.download_is_pending(claimed));
        assert!(app.session.project.maps[target].layer(layer).is_none());
        assert!(app.session.project.maps[target].groups.iter().all(|g| !g.children.contains(&layer)));
        assert!(app.session.project.saved_text(None).is_ok());
        assert!(app.session.download_continues(unrelated).is_ok());
        app.session.download_failed(unrelated);
    }

    /// A full active reader must not let queued stores issue downloads or retain chunk buffers.
    #[test]
    fn geolibre_zarr_waiting_store_does_not_multiply_the_active_byte_budget() {
        let mut app = RustGisApp::with_sample();
        let id = app.session.download_started();
        app.ui.geolibre_zarr.push(Read {
            id,
            params: json!({}),
            root: "https://example.org/active.zarr".into(),
            queue: vec!["next".into()],
            waiting: vec![("pending".into(), Slot::default())],
            got: HashMap::new(),
            phase: None,
            bytes: MAX_IMPORT_BYTES,
            forbidden: 0,
            error: None,
        });
        start(&mut app, &json!({"url":"https://example.org/queued.zarr"}), None).unwrap();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(app.ui.geolibre_zarr.len(), 2);
        let queued = &app.ui.geolibre_zarr[1];
        assert!(queued.waiting.is_empty());
        assert!(queued.got.is_empty());
        assert_eq!(queued.bytes, 0);
        assert_eq!(queued.queue.len(), zarr::METADATA_KEYS.len());
        // Once the active batch lands, an exhausted budget fails before another request.
        let active = &mut app.ui.geolibre_zarr[0];
        active.waiting.clear();
        assert!(active.poll().unwrap().is_err());
        assert!(active.waiting.is_empty());
    }
}
