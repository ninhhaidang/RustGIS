//! Engine-held claims for deferred URL stages. Payload JSON never creates authority.
use super::Continuation;
use crate::{CmdResult, Session, UrlRequest};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct Claim {
    pub c: Continuation,
    pub params: Value,
    pub live: Arc<AtomicBool>,
    import: bool,
}
impl Drop for Claim {
    /// Revokes queued browser/native predicates when an engine claim is retired.
    fn drop(&mut self) {
        self.live.store(false, Ordering::SeqCst);
    }
}
#[derive(Clone)]
pub(super) struct Queued {
    c: std::sync::Weak<Continuation>,
    import: bool,
    consumed: Arc<AtomicBool>,
}
#[derive(Default)]
pub(crate) struct Work {
    pub(super) queued: BTreeMap<u64, Queued>,
    pub(super) claims: BTreeMap<u64, Claim>,
}
/// A guarded command restores prior opaque requests and retires every newly issued key.
pub(crate) struct UrlRequestCheckpoint {
    requests: Vec<UrlRequest>,
    queued: BTreeMap<u64, Queued>,
}
impl Session {
    /// The current project generation, used to prune adapter-owned remembered operations.
    pub fn project_generation(&self) -> u64 {
        self.opened
    }
    /// Marks queued authority present before a guarded operation, including taken requests.
    pub(crate) fn queued_authority(&self) -> Vec<u64> {
        self.policy.work.queued.keys().copied().collect()
    }
    /// Keeps sealed prior requests, including their original owning Arc, across rollback.
    pub(crate) fn url_request_checkpoint(&self) -> UrlRequestCheckpoint {
        UrlRequestCheckpoint {
            requests: self
                .url_requests
                .iter()
                .map(|q| UrlRequest { key: q.key, params: q.params.clone(), authority: q.authority.clone() })
                .collect(),
            queued: self.policy.work.queued.clone(),
        }
    }
    pub(crate) fn restore_url_request_checkpoint(&mut self, checkpoint: UrlRequestCheckpoint) {
        // Claimed and terminally retired keys stay consumed even if their command later fails.
        let mut queued = checkpoint.queued;
        queued.retain(|_, q| !q.consumed.load(Ordering::SeqCst));
        self.url_requests = checkpoint.requests.into_iter().filter(|q| queued.contains_key(&q.key)).collect();
        self.policy.work.queued = queued;
        self.retire_abandoned_requests();
    }
    /// Only successful plugin suspension may retain its new non-import requests through rollback.
    pub(crate) fn suspend_plugin_requests(&mut self, before: &[u64], asked: &[(u64, Value)]) -> Vec<UrlRequest> {
        let mut retained = Vec::new();
        for q in std::mem::take(&mut self.url_requests) {
            let number = q.params.get("request").and_then(Value::as_u64);
            if !before.contains(&q.key)
                && self.policy.work.queued.get(&q.key).is_some_and(|queued| !queued.import)
                && asked.iter().any(|(n, _)| Some(*n) == number)
            {
                retained.push(q);
            } else {
                self.url_requests.push(q);
            }
        }
        retained
    }
    pub(crate) fn restore_plugin_requests(&mut self, requests: Vec<UrlRequest>) -> Result<(), String> {
        self.retire_abandoned_requests();
        if self.policy.work.queued.len().saturating_add(requests.len()) > crate::MAX_URL_REQUESTS {
            return Err(format!("{} downloads are already waiting; try again when they have landed", crate::MAX_URL_REQUESTS));
        }
        for q in &requests {
            self.check_delivery(&q.authority)?;
        }
        for q in requests {
            self.policy
                .work
                .queued
                .insert(q.key, Queued { c: Arc::downgrade(&q.authority), import: false, consumed: Arc::new(AtomicBool::new(false)) });
            self.url_requests.push(q);
        }
        Ok(())
    }
    pub(crate) fn discard_plugin_requests(&mut self, keys: &[u64]) {
        self.url_requests.retain(|q| !keys.contains(&q.key));
        self.policy.work.queued.retain(|key, queued| {
            let keep = !keys.contains(key);
            if !keep {
                queued.consumed.store(true, Ordering::SeqCst);
            }
            keep
        });
    }
    /// Plugin pins and caller lifetimes are part of request sharing, not just URL text.
    pub(crate) fn same_url_caller(&self, a: &Continuation, b: &Continuation) -> bool {
        a.scope.same_caller(&b.scope) && Arc::ptr_eq(&a.epoch, &b.epoch) && a.opened == b.opened && a.map == b.map
    }
    /// Failure notification settles routing only; named ingress must own the original wait.
    /// Native adapters retain the documented root delivery convention.
    pub fn request_failed(&mut self, p: &Value, error: &str) {
        let Some(n) = p.get("request").and_then(Value::as_u64) else { return };
        if !self.policy_ingress_idle() && self.url_waits.owner(n).is_none_or(|c| self.check_delivery_owner(c).is_err()) {
            return;
        }
        if self.url_waits.holds(n, p) {
            self.url_waits.land(n, Err(crate::sources::redact_text(error)), crate::cmd::plugins::fetch::Added::default());
        }
        if self.url_waits.given_up(n, p) {
            self.url_waits.refused(n);
        }
    }
    /// Only a validated frozen claim can consume retry routing or remove its landed bytes.
    pub(super) fn url_completion_refusal(&mut self, command: &str, p: &Value) -> Result<Option<String>, String> {
        if !matches!(command, "layer.addUrl" | "layer.addDownload") {
            return Ok(None);
        }
        let Some(id) = p.get("id").and_then(Value::as_u64).filter(|id| self.claim_for(*id).is_some()) else { return Ok(None) };
        let c = self.claim_for(id).ok_or("no claimed download with that id")?;
        self.continuation_owner(&c)?;
        self.download_params(id, p, command == "layer.addUrl")?;
        let Some(n) = p.get("request").and_then(Value::as_u64) else { return Ok(None) };
        let added = self.url_waits.added_already(n, p);
        if !added && !self.url_waits.given_up(n, p) {
            return Ok(None);
        }
        self.downloaded.retain(|(k, _)| *k != id);
        self.landed_reads.retain(|(k, _)| *k != id);
        #[cfg(not(target_arch = "wasm32"))]
        self.landed_windows.retain(|(k, _)| *k != id);
        let url = crate::sources::redact_text(p.get("url").and_then(Value::as_str).unwrap_or("download"));
        if added {
            Ok(Some(format!("{url}: it was added already for its plugin command, so it wasn't added again")))
        } else {
            self.url_waits.refused(n);
            Ok(Some(format!("{url}: it came after its plugin command stopped waiting, so it wasn't added")))
        }
    }
    /// Admitted owner-only completions share retry bookkeeping, including in-place probes.
    pub(super) fn url_completion_landed(&mut self, command: &str, p: &Value, result: &CmdResult) {
        if !matches!(command, "layer.addUrl" | "layer.addDownload") {
            return;
        }
        let Some(id) = p.get("id").and_then(Value::as_u64) else { return };
        let Some(c) = self.claim_for(id) else { return };
        if self.check_delivery_owner(&c).is_err() || self.download_params(id, p, command == "layer.addUrl").is_err() {
            return;
        }
        if let Some(n) = p.get("request").and_then(Value::as_u64)
            && self.url_waits.holds(n, p)
            && !result.as_ref().is_ok_and(|v| v.get("pending").and_then(Value::as_bool) == Some(true))
        {
            let added = crate::cmd::plugins::fetch::Added::of(self.map(), result);
            self.url_waits.land(n, result.clone(), added);
        }
    }
    /// Captures a handler's authority, or checks the equivalent direct native ingress.
    pub fn request_url(&mut self, p: &Value) -> CmdResult {
        if ["geolibreProject", "geolibreLayer", "geolibreZarr"].iter().any(|k| p.get(k).is_some()) {
            return Err("internal import targets require an engine import request".into());
        }
        if self.policy_has_no_operation() {
            return self.request_url_authorized(p);
        }
        self.enqueue_url(p)
    }
    /// Only the engine import executor supplies reservation flags; JSON never chooses purpose.
    pub(crate) fn request_import_url(&mut self, p: &Value) -> CmdResult {
        if self.policy_has_no_operation() {
            return self.request_import_url_authorized(p);
        }
        self.enqueue_url_for(p, true)
    }
    /// Drops only expired weak queue entries; taken requests own their authority until claimed/dropped.
    pub(crate) fn retire_abandoned_requests(&mut self) {
        self.policy.work.queued.retain(|_, q| q.c.strong_count() > 0);
    }
    /// Counts live unclaimed requests, including opaque handles already taken by an adapter.
    /// Dropped handles do not occupy a slot, matching abandoned-request retirement.
    pub(crate) fn queued_url_count(&self) -> usize {
        self.policy.work.queued.values().filter(|q| q.c.strong_count() > 0).count()
    }
    /// Deduplicates exact requests within one owner and retains their initiating node.
    pub(super) fn enqueue_url(&mut self, p: &Value) -> CmdResult {
        self.enqueue_url_for(p, false)
    }
    /// Owns the originating continuation in the opaque request and bounds all retained unclaimed requests.
    fn enqueue_url_for(&mut self, p: &Value, import: bool) -> CmdResult {
        let mut c = self.continuation()?;
        self.check_continuation(&c)?;
        // Only the built-in data reader proves this bounded set. Services, imports and
        // plugin-initiated discovery retain the full active storage identity.
        if !import
            && matches!(&c.node.op, super::plan::Op::Command(spec) if spec.id == "layer.addUrl")
            && let Some(urls) = crate::cmd::layer::fixed_download_urls(p)
        {
            let selected = urls
                .iter()
                .map(|url| crate::render::rustgis_tiles::s3::account_identity_for(crate::render::rustgis_tiles::logins::inner(url)))
                .collect::<Result<Vec<_>, _>>()?;
            // A concurrent account change cannot become fresh authority during narrowing.
            self.check_continuation(&c)?;
            c.s3 = Some(selected);
        }
        let c = Arc::new(c);
        self.retire_abandoned_requests();
        if !p.is_object() {
            return Err("a download is asked for by an object: its \"url\" and how to read it".into());
        }
        let asked = self.asked_for(p);
        let waiting = self
            .url_requests
            .iter()
            .find(|q| {
                crate::cmd::plugins::fetch::same_request(&q.params, &asked)
                    && self.same_url_caller(&q.authority, &c)
                    && self.check_continuation(&q.authority).is_ok()
            })
            .and_then(|q| q.params.get("request").and_then(Value::as_u64));
        // A handed-back receipt is honored only through the original validated opaque claim.
        let given = p
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| self.download_params(*id, p, true).is_ok())
            .and_then(|id| self.policy.work.claims.get(&id))
            .filter(|claim| self.same_url_caller(&claim.c, &c))
            .and_then(|claim| claim.params.get("request").and_then(Value::as_u64))
            .filter(|n| self.url_waits.holds(*n, &asked));
        let made = self
            .call_requests
            .as_ref()
            .and_then(|_| {
                self.url_waits.in_flight(&asked, |n| {
                    self.url_waits.owner(n).is_some_and(|owner| self.same_url_caller(owner, &c) && self.check_continuation(owner).is_ok())
                })
            })
            .filter(|n| !self.url_waits.taken_away(*n, &self.project));
        let number = match given.or(waiting).or(made) {
            Some(n) => n,
            None => {
                self.next_request = self.next_request.checked_add(1).ok_or("download request numbers were exhausted")?;
                self.next_request
            }
        };
        if (given.is_some() && waiting != Some(number)) || (waiting.is_none() && !self.url_waits.has(number)) {
            if self.policy.work.queued.len() >= crate::MAX_URL_REQUESTS {
                return Err(format!("{} downloads are already waiting; try again when they have landed", crate::MAX_URL_REQUESTS));
            }
            let mut params = asked.clone();
            params.as_object_mut().ok_or("download parameters must be an object")?.insert("request".into(), json!(number));
            if given.is_some() {
                params.as_object_mut().ok_or("download parameters must be an object")?.insert("whole".into(), json!(true));
            }
            self.next_pending = self.next_pending.wrapping_add(1);
            let key = self.next_pending;
            self.policy.work.queued.insert(key, Queued { c: Arc::downgrade(&c), import, consumed: Arc::new(AtomicBool::new(false)) });
            self.url_requests.push(UrlRequest { key, params, authority: c });
        }
        if !import
            && let Some(asked_now) = self.call_requests.as_mut()
            && !asked_now.iter().any(|(n, _)| *n == number)
        {
            asked_now.push((number, asked));
        }
        self.request_only = true;
        Ok(json!({"pending":true,"url":p.get("url").and_then(Value::as_str).map(crate::sources::redact)}))
    }
    /// Taking an opaque request sends nothing and retains other callers' queued requests.
    /// Authority is checked again when the owner or native host claims it.
    pub fn take_url_requests(&mut self) -> Vec<UrlRequest> {
        let mut live = Vec::new();
        let mut revoked = 0;
        for request in std::mem::take(&mut self.url_requests) {
            if self.check_delivery_owner(&request.authority).is_err() {
                self.url_requests.push(request);
                continue;
            }
            let ok = self
                .policy
                .work
                .queued
                .get(&request.key)
                .and_then(|q| q.c.upgrade())
                .ok_or("the request was already claimed")
                .and_then(|c| self.check_continuation_origin(&c).map_err(|_| "this URL request was revoked"));
            if ok.is_ok() {
                live.push(request);
            } else {
                self.request_failed(&request.params, "this URL request was revoked before it started");
                if let Some(queued) = self.policy.work.queued.remove(&request.key) {
                    queued.consumed.store(true, Ordering::SeqCst);
                    if queued.import {
                        crate::cmd::geolibre::discard_pending(self, &request.params);
                    }
                }
                revoked += 1;
            }
        }
        match revoked {
            0 => {}
            1 => self.notify("1 deferred URL request was revoked before it started", true),
            n => self.notify(format!("{n} deferred URL requests were revoked before they started"), true),
        }
        live
    }
    /// Claims one engine-issued request immediately before its first I/O.
    pub fn download_started_for(&mut self, request: &UrlRequest) -> Result<u64, String> {
        let q = self.policy.work.queued.get(&request.key).ok_or("this URL request was already claimed or dropped")?;
        let c = q.c.upgrade().ok_or("this URL request was dropped")?;
        if !Arc::ptr_eq(&request.authority, &c) {
            return Err("this URL request belongs to another session".into());
        }
        self.check_delivery_owner(&c)?;
        let import = q.import;
        let ready = if self.policy.work.claims.len() >= 64 {
            Err("64 downloads are already awaiting completion".into())
        } else {
            self.check_delivery(&c)
        };
        if let Err(error) = ready {
            self.request_failed(&request.params, &error);
            if import {
                // Only the verified owner may abandon this sealed reservation before a reader exists.
                if let Some(queued) = self.policy.work.queued.remove(&request.key) {
                    queued.consumed.store(true, Ordering::SeqCst);
                }
                crate::cmd::geolibre::discard_pending(self, &request.params);
            }
            return Err(error);
        }
        if let Some(q) = self.policy.work.queued.remove(&request.key) {
            q.consumed.store(true, Ordering::SeqCst);
        }
        let c = (*c).clone();
        let id = self.download_started();
        self.policy.work.claims.insert(id, Claim { c, params: request.params.clone(), live: Arc::new(AtomicBool::new(true)), import });
        Ok(id)
    }
    /// Rechecks every new stage, including a probe's fallback and later pages or keys.
    pub fn download_continues(&mut self, id: u64) -> Result<(), String> {
        self.download_owner(id)?;
        if !self.download_is_pending(id)
            && !self.downloaded.iter().any(|(n, _)| *n == id)
            && !self.landed_reads.iter().any(|(n, _)| *n == id)
            && !self.landed_window(id)
        {
            return Err("this download belongs to a replaced project or was dropped".into());
        }
        if let Some(claim) = self.policy.work.claims.get(&id)
            && let Err(e) = self.check_continuation(&claim.c)
        {
            if let Some(claim) = self.policy.work.claims.remove(&id) {
                self.request_failed(&claim.params, &e);
                if claim.import {
                    crate::cmd::geolibre::discard_pending(self, &claim.params);
                }
            }
            self.downloading.retain(|n| *n != id);
            self.downloaded.retain(|(n, _)| *n != id);
            self.landed_reads.retain(|(n, _)| *n != id);
            #[cfg(not(target_arch = "wasm32"))]
            self.landed_windows.retain(|(n, _)| *n != id);
            return Err(e);
        }
        Ok(())
    }
    /// Whether a window read off the frame landed for download `id` (#1330).
    fn landed_window(&self, id: u64) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.landed_windows.iter().any(|(n, _)| *n == id);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = id;
            false
        }
    }
    /// Owns a principal-free predicate that survives queues and browser awaits.
    pub fn download_still(&self, id: u64) -> Option<crate::render::rustgis_tiles::cache::Still> {
        let claim = self.policy.work.claims.get(&id)?;
        let (c, live) = (claim.c.clone(), claim.live.clone());
        Some(Arc::new(move || live.load(Ordering::SeqCst) && c.check().is_ok()))
    }
    /// Completes a read-in-place probe under its engine-held owner and exact original address.
    pub fn add_in_place_for(&mut self, id: u64, p: &Value) -> CmdResult {
        let c = self.policy.work.claims.get(&id).ok_or("no claimed URL request with that id")?.c.clone();
        self.check_delivery(&c)?;
        self.download_continues(id)?;
        self.download_params(id, p, true)?;
        let r = self.resume(&c, |s| s.run_url_completion(id, "layer.addUrl", p));
        self.policy.work.claims.remove(&id);
        self.downloading.retain(|n| *n != id);
        r?
    }
    /// Native delivery resumes the claim. Ordinary user-owned downloads retain their ingress.
    pub fn complete_download(&mut self, id: u64, p: &Value) -> CmdResult {
        if let Some(claim) = self.policy.work.claims.get(&id) {
            let c = claim.c.clone();
            if !self.policy_ingress_idle() {
                self.continuation_owner(&c)?;
            }
            let result = self.resume(&c, |s| s.run("layer.addDownload", p));
            // Consume only the owner's claim. A rejected foreign call never reaches this path.
            self.policy.work.claims.remove(&id);
            result?
        } else {
            self.run("layer.addDownload", p)
        }
    }
    /// Reconstructs the frozen effective request, allowing only documented completion refinements.
    pub(super) fn download_params(&self, id: u64, p: &Value, in_place: bool) -> Result<Value, String> {
        let claim = self.policy.work.claims.get(&id).ok_or("no claimed download with that id")?;
        self.check_continuation(&claim.c)?;
        let given = p.as_object().ok_or("download parameters must be an object")?;
        let mut params = claim.params.as_object().cloned().ok_or("the stored download parameters are not an object")?;
        for (key, value) in given {
            if ["geolibreProject", "geolibreLayer", "geolibreZarr"].contains(&key.as_str()) && !claim.import {
                return Err("a simple URL completion cannot import a project's internal targets".into());
            }
            let refinement = (key == "id" && value.as_u64() == Some(id))
                || (key == "cog" && in_place && value.as_bool() == Some(true))
                || (key == "whole" && value.as_bool() == Some(true))
                || (key == "zoom"
                    && crate::cmd::bool_value(value) == Some(false)
                    && claim.params.get(key).and_then(crate::cmd::bool_value) == Some(true));
            if !refinement && claim.params.get(key) != Some(value) {
                return Err("the download's address or parameters changed since it was requested".into());
            }
            params.insert(key.clone(), value.clone());
        }
        params.insert("id".into(), json!(id));
        Ok(Value::Object(params))
    }
    /// Internal import flags are accepted only while applying that engine-issued claim.
    pub(crate) fn import_completion(&self, p: &Value) -> bool {
        p.get("id")
            .and_then(Value::as_u64)
            .and_then(|id| self.policy.work.claims.get(&id))
            .is_some_and(|claim| claim.import && self.check_delivery(&claim.c).is_ok())
    }
    /// Borrows stored completion authority; payload fields cannot manufacture one.
    pub(super) fn claim_for(&self, id: u64) -> Option<Continuation> {
        self.policy.work.claims.get(&id).map(|c| c.c.clone())
    }
    /// Moves claims with the project state so transactional replacement can roll back.
    pub(crate) fn take_policy_work(&mut self) -> Work {
        std::mem::take(&mut self.policy.work)
    }
    /// Restores the original project's still-owned claims after rollback.
    pub(crate) fn restore_policy_work(&mut self, work: Work) {
        self.policy.work = work;
    }
    /// Named callbacks own their claim; the native root retains its documented delivery role.
    pub(crate) fn download_owner(&self, id: u64) -> Result<(), String> {
        if let Some(claim) = self.policy.work.claims.get(&id) {
            self.check_delivery_owner(&claim.c)?;
        }
        Ok(())
    }
    /// A native queue retirement settles only the exact wait held by its sealed claim.
    /// No public request number or current caller can select another wait here.
    pub(crate) fn retire_download_claim(&mut self, id: u64, error: &str) {
        let Some(claim) = self.policy.work.claims.remove(&id) else { return };
        claim.live.store(false, Ordering::SeqCst);
        if let Some(number) = claim.params.get("request").and_then(Value::as_u64)
            && self.url_waits.holds(number, &claim.params)
        {
            self.url_waits.land(number, Err(crate::sources::redact_text(error)), crate::cmd::plugins::fetch::Added::default());
        }
        if claim.import {
            crate::cmd::geolibre::discard_pending(self, &claim.params);
        }
    }
    /// Consumes a completed claim and invalidates every captured predicate.
    pub(crate) fn forget_download_claim(&mut self, id: u64) {
        self.policy.work.claims.remove(&id);
    }
}
