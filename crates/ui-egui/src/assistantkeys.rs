//! Session cache and ordered, off-frame keychain operations for Assistant API keys.

use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};

use rustgis_engine::data::secrets::{Secret, SecretError, SecretStore};

/// What the settings can truthfully say about the selected provider's key.
#[derive(Clone)]
pub(crate) enum Status {
    Session,
    Legacy,
    Pending { loading: bool },
    Stored,
    Failed { error: SecretError, legacy: bool, deleting: bool, reading: bool },
}

/// A user-entered key to remember; environment keys and empty keys never enter the store.
pub(crate) fn remembered(key: &str, remember: bool, environment: &str) -> Option<Secret> {
    let key = key.trim();
    (remember && !key.is_empty() && key != environment).then(|| Secret::new(key))
}

struct Cached {
    key: Secret,
    revision: u64,
    status: Status,
    saved: Option<Option<Secret>>,
    changed: bool,
    requested: bool,
}

enum Action {
    Load,
    Save(Option<Secret>),
}

struct Job {
    provider: String,
    revision: u64,
    legacy: Option<Secret>,
    action: Action,
    store: Arc<dyn SecretStore>,
    ctx: egui::Context,
}

struct Completed {
    provider: String,
    revision: u64,
    key: Option<Secret>,
    status: Status,
    clear: Option<Secret>,
}

impl Completed {
    /// A backend panic becomes a failure without exposing its payload or discarding a legacy key.
    fn panicked(job: &Job) -> Self {
        Self {
            provider: job.provider.clone(),
            revision: job.revision,
            key: None,
            status: Status::Failed {
                error: SecretError::Failed("the keychain backend stopped unexpectedly".into()),
                legacy: matches!(job.action, Action::Load) && job.legacy.is_some(),
                deleting: matches!(job.action, Action::Save(None)),
                reading: matches!(job.action, Action::Load) && job.legacy.is_none(),
            },
            clear: None,
        }
    }
}

/// Executes one operation without ever putting a secret in diagnostics.
fn perform(job: Job) -> Completed {
    let account = rustgis_assistant::provider::key_account(&job.provider);
    let deleting = matches!(job.action, Action::Save(None));
    let reading = matches!(job.action, Action::Load) && job.legacy.is_none();
    let migrating = matches!(job.action, Action::Load) && job.legacy.is_some();
    let (result, key) = match job.action {
        Action::Load => match job.store.load(&account) {
            Ok(Some(key)) => (Ok(Status::Stored), Some(key)),
            Ok(None) => match &job.legacy {
                Some(key) => (job.store.save(&account, key).map(|_| Status::Stored), Some(key.clone())),
                None => (Ok(Status::Session), None),
            },
            Err(error) => (Err(error), None),
        },
        Action::Save(Some(key)) => (job.store.save(&account, &key).map(|_| Status::Stored), None),
        Action::Save(None) => (job.store.delete(&account).map(|_| Status::Session), None),
    };
    let clear = result.is_ok().then(|| job.legacy.clone()).flatten();
    let status = result.unwrap_or_else(|error| Status::Failed { error, legacy: migrating, deleting, reading });
    Completed { provider: job.provider, revision: job.revision, key, status, clear }
}

struct Worker {
    send: mpsc::Sender<Job>,
    receive: mpsc::Receiver<Completed>,
}

impl Worker {
    /// One worker serializes every account, including a delete queued behind a pending save.
    fn start() -> Result<Self, SecretError> {
        let (send, requests) = mpsc::channel::<Job>();
        let (replies, receive) = mpsc::channel();
        std::thread::Builder::new()
            .name("assistant-keys".into())
            .spawn(move || {
                for job in requests {
                    let ctx = job.ctx.clone();
                    let failure = Completed::panicked(&job);
                    let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| perform(job))).unwrap_or(failure);
                    let _ = replies.send(completed);
                    ctx.request_repaint();
                }
            })
            .map_err(|_| SecretError::Unavailable("could not start the keychain worker".into()))?;
        Ok(Self { send, receive })
    }
}

/// Cached keys survive provider switches; only the worker touches the OS store.
#[derive(Default)]
pub(crate) struct Keys {
    entries: BTreeMap<String, Cached>,
    worker: Option<Worker>,
}

impl Keys {
    /// Loads a provider once. The legacy key stays usable while its migration is pending.
    pub fn load(
        &mut self,
        provider: &str,
        environment: &str,
        legacy: Option<String>,
        remember: bool,
        store: Option<Arc<dyn SecretStore>>,
        ctx: &egui::Context,
    ) -> String {
        if let Some(cached) = self.entries.get_mut(provider) {
            let key = cached.key.expose().to_string();
            let read = remember && !cached.requested && !cached.changed;
            cached.requested |= remember;
            if read {
                self.submit(provider, legacy.map(Secret::new), Action::Load, store, ctx);
            }
            return key;
        }
        let key = Secret::new(legacy.as_deref().unwrap_or(environment));
        self.entries.insert(
            provider.into(),
            Cached {
                key: key.clone(),
                revision: 0,
                status: if legacy.is_some() { Status::Legacy } else { Status::Session },
                saved: None,
                changed: false,
                requested: remember,
            },
        );
        // An unticked Remember must not re-open a key left behind by a failed deletion.
        if remember {
            self.submit(provider, legacy.map(Secret::new), Action::Load, store, ctx);
        }
        key.expose().to_string()
    }

    /// Opting in reads an unedited key first, rather than deleting an empty/environment fallback.
    pub fn enable(&mut self, provider: &str, legacy: Option<String>, store: Option<Arc<dyn SecretStore>>, ctx: &egui::Context) -> bool {
        let Some(cached) = self.entries.get_mut(provider) else { return false };
        if cached.changed {
            return false;
        }
        cached.revision = cached.revision.saturating_add(1);
        cached.requested = true;
        self.submit(provider, legacy.map(Secret::new), Action::Load, store, ctx);
        true
    }

    /// Invalidates an old load before a typed key or Remember change can be overwritten.
    pub fn edit(&mut self, provider: &str, key: &str) {
        if let Some(cached) = self.entries.get_mut(provider) {
            cached.key = Secret::new(key);
            cached.revision = cached.revision.saturating_add(1);
            cached.saved = None;
            cached.changed = true;
            cached.status = Status::Session;
        }
    }

    /// Saves or deletes once per change, without touching preference storage on the worker.
    pub fn save(
        &mut self,
        provider: &str,
        key: Option<Secret>,
        legacy: Option<String>,
        store: Option<Arc<dyn SecretStore>>,
        ctx: &egui::Context,
    ) {
        let Some(cached) = self.entries.get_mut(provider) else { return };
        if !cached.changed {
            return;
        }
        if cached.saved.as_ref() == Some(&key) && !matches!(cached.status, Status::Failed { .. }) {
            return;
        }
        cached.saved = Some(key.clone());
        cached.revision = cached.revision.saturating_add(1);
        self.submit(provider, legacy.map(Secret::new), Action::Save(key), store, ctx);
    }

    /// Starts the worker lazily; a missing store is an error, never a plaintext fallback.
    fn submit(&mut self, provider: &str, legacy: Option<Secret>, action: Action, store: Option<Arc<dyn SecretStore>>, ctx: &egui::Context) {
        let Some(cached) = self.entries.get_mut(provider) else { return };
        let deleting = matches!(action, Action::Save(None));
        let reading = matches!(action, Action::Load) && legacy.is_none();
        let migrating = matches!(action, Action::Load) && legacy.is_some();
        let failure = |error| Status::Failed { error, legacy: migrating, deleting, reading };
        let Some(store) = store else {
            cached.status = failure(SecretError::Unavailable("no system keychain in this build".into()));
            return;
        };
        if self.worker.is_none() {
            match Worker::start() {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    cached.status = failure(error);
                    return;
                }
            }
        }
        cached.status = Status::Pending { loading: reading };
        let job = Job { provider: provider.into(), revision: cached.revision, legacy: legacy.clone(), action, store, ctx: ctx.clone() };
        if self.worker.as_ref().is_none_or(|worker| worker.send.send(job).is_err()) {
            cached.status = failure(SecretError::Unavailable("the keychain worker stopped".into()));
        }
    }

    /// Applies current replies and returns legacy values safe to remove, even for stale replies.
    pub fn poll(&mut self) -> Vec<(String, Secret)> {
        let mut clear = Vec::new();
        if let Some(worker) = &self.worker {
            for completed in worker.receive.try_iter() {
                if let Some(key) = completed.clear {
                    clear.push((completed.provider.clone(), key));
                }
                if let Some(cached) = self.entries.get_mut(&completed.provider)
                    && cached.revision == completed.revision
                {
                    if let Some(key) = completed.key {
                        cached.key = key;
                    }
                    cached.status = completed.status;
                }
            }
        }
        clear
    }

    /// The selected provider's session key, including edits made during a pending load.
    pub fn key(&self, provider: &str) -> Option<&str> {
        self.entries.get(provider).map(|cached| cached.key.expose())
    }

    /// Model, address and provider changes alone must never save or delete an unread key.
    pub fn changed(&self, provider: &str) -> bool {
        self.entries.get(provider).is_some_and(|cached| cached.changed)
    }

    /// The last operation's state, used only for the storage note in settings.
    pub fn status(&self, provider: &str) -> Option<&Status> {
        self.entries.get(provider).map(|cached| &cached.status)
    }

    /// An unread key must arrive before requests can use a temporary environment fallback.
    pub fn loading(&self, provider: &str) -> bool {
        matches!(self.status(provider), Some(Status::Pending { loading: true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::data::secrets::MemorySecrets;
    use std::time::{Duration, Instant};

    /// Waits for a bounded fake-store operation, collecting successful migration receipts.
    fn settle(keys: &mut Keys, provider: &str) -> Vec<(String, Secret)> {
        let end = Instant::now() + Duration::from_secs(3);
        let mut cleared = Vec::new();
        loop {
            cleared.extend(keys.poll());
            if !matches!(keys.status(provider), Some(Status::Pending { .. })) {
                return cleared;
            }
            assert!(Instant::now() < end, "keychain worker did not finish");
            std::thread::sleep(Duration::from_millis(1)); // test-wait: legacy polling for the keychain worker
        }
    }

    /// Existing plaintext stays available and is removable only after the store accepts it.
    #[test]
    fn migration_preserves_the_key_and_reports_when_plaintext_can_be_removed() {
        let ctx = egui::Context::default();
        let store = Arc::new(MemorySecrets::new());
        let mut keys = Keys::default();
        assert_eq!(keys.load("custom", "environment", Some("legacy".into()), true, Some(store.clone()), &ctx), "legacy");
        let clear = settle(&mut keys, "custom");
        assert_eq!(clear, vec![("custom".into(), Secret::new("legacy"))]);
        assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("legacy")));
        assert!(matches!(keys.status("custom"), Some(Status::Stored)));
        for error in [SecretError::Denied, SecretError::Unavailable("no service".into()), SecretError::Failed("offline".into())] {
            let mut keys = Keys::default();
            keys.load("custom", "", Some("legacy".into()), true, Some(Arc::new(MemorySecrets::failing(error))), &ctx);
            assert!(settle(&mut keys, "custom").is_empty());
            assert_eq!(keys.key("custom"), Some("legacy"));
            assert!(matches!(keys.status("custom"), Some(Status::Failed { legacy: true, .. })));
        }
    }

    /// A stale plaintext copy must never replace a key already saved in the system store.
    #[test]
    fn migration_prefers_an_existing_stored_key_over_a_legacy_copy() {
        let ctx = egui::Context::default();
        let store = Arc::new(MemorySecrets::new());
        store.save("assistant/custom", &Secret::new("stored-newer")).unwrap();
        let mut keys = Keys::default();
        assert_eq!(keys.load("custom", "", Some("legacy-older".into()), true, Some(store.clone()), &ctx), "legacy-older");
        assert_eq!(settle(&mut keys, "custom"), vec![("custom".into(), Secret::new("legacy-older"))]);
        assert_eq!(keys.key("custom"), Some("stored-newer"));
        assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("stored-newer")));
        assert!(matches!(keys.status("custom"), Some(Status::Stored)));
    }

    /// A successful empty read does not authorize clearing plaintext when its migration write fails.
    #[test]
    fn migration_preserves_legacy_when_the_write_is_denied() {
        struct RejectsWrites;
        impl SecretStore for RejectsWrites {
            fn load(&self, _account: &str) -> Result<Option<Secret>, SecretError> {
                Ok(None)
            }

            fn save(&self, _account: &str, _key: &Secret) -> Result<(), SecretError> {
                Err(SecretError::Denied)
            }

            fn delete(&self, _account: &str) -> Result<(), SecretError> {
                Ok(())
            }
        }

        let ctx = egui::Context::default();
        let mut keys = Keys::default();
        keys.load("custom", "", Some("legacy".into()), true, Some(Arc::new(RejectsWrites)), &ctx);
        assert!(settle(&mut keys, "custom").is_empty());
        assert_eq!(keys.key("custom"), Some("legacy"));
        assert!(matches!(keys.status("custom"), Some(Status::Failed { error: SecretError::Denied, legacy: true, .. })));
    }

    /// New keys stay in memory on failure, while environment values are never saved.
    #[test]
    fn remembering_excludes_environment_and_empty_keys_and_handles_missing_stores() {
        assert_eq!(remembered(" environment ", true, "environment"), None);
        assert_eq!(remembered("typed", false, "environment"), None);
        assert_eq!(remembered("  ", true, ""), None);
        assert_eq!(remembered(" typed ", true, "environment"), Some(Secret::new("typed")));
        let ctx = egui::Context::default();
        let mut keys = Keys::default();
        keys.load("custom", "environment", None, true, None, &ctx);
        keys.edit("custom", "typed");
        keys.save("custom", remembered("typed", true, "environment"), None, None, &ctx);
        assert_eq!(keys.key("custom"), Some("typed"));
        assert!(matches!(keys.status("custom"), Some(Status::Failed { legacy: false, deleting: false, .. })));
    }

    /// Provider accounts remain independent and switching back retains a session-only edit.
    #[test]
    fn providers_are_isolated_and_forgetting_removes_only_the_selected_account() {
        let ctx = egui::Context::default();
        let store = Arc::new(MemorySecrets::new());
        store.save("assistant/first", &Secret::new("one")).unwrap();
        store.save("assistant/second", &Secret::new("two")).unwrap();
        let mut keys = Keys::default();
        for provider in ["first", "second"] {
            keys.load(provider, "", None, true, Some(store.clone()), &ctx);
            settle(&mut keys, provider);
        }
        assert_eq!(keys.key("first"), Some("one"));
        assert_eq!(keys.key("second"), Some("two"));
        keys.edit("first", "edited");
        keys.save("first", None, None, Some(store.clone()), &ctx);
        settle(&mut keys, "first");
        assert_eq!(store.load("assistant/first").unwrap(), None);
        assert_eq!(store.load("assistant/second").unwrap(), Some(Secret::new("two")));
        assert_eq!(keys.load("first", "", None, false, Some(store), &ctx), "edited");
    }

    /// A refused deletion remains visible, and Remember off never reloads that stored key.
    #[test]
    fn deletion_failure_is_not_reported_as_success_or_reloaded_after_opt_out() {
        let ctx = egui::Context::default();
        let failed = Arc::new(MemorySecrets::failing(SecretError::Denied));
        let mut keys = Keys::default();
        keys.load("custom", "", None, false, Some(failed.clone()), &ctx);
        keys.edit("custom", "");
        keys.save("custom", None, None, Some(failed), &ctx);
        settle(&mut keys, "custom");
        assert!(matches!(keys.status("custom"), Some(Status::Failed { deleting: true, .. })));
        let store = Arc::new(MemorySecrets::new());
        store.save("assistant/custom", &Secret::new("left-behind")).unwrap();
        let mut restarted = Keys::default();
        assert_eq!(restarted.load("custom", "", None, false, Some(store), &ctx), "");
        assert!(matches!(restarted.status("custom"), Some(Status::Session)));
    }

    /// The first read blocks on a test gate, allowing edits and a queued delete to race it.
    struct Gated {
        gate: std::sync::Mutex<mpsc::Receiver<()>>,
        store: MemorySecrets,
        caller: std::thread::ThreadId,
    }

    impl SecretStore for Gated {
        /// Holds the initial read until the test has queued its edits.
        fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
            assert_ne!(self.caller, std::thread::current().id(), "keychain access must be off the UI thread");
            self.gate.lock().unwrap().recv_timeout(Duration::from_secs(3)).unwrap();
            self.store.load(account)
        }
        /// Records the ordered write in the fake store.
        fn save(&self, account: &str, key: &Secret) -> Result<(), SecretError> {
            self.store.save(account, key)
        }
        /// Removes the fake value after earlier operations finish.
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.store.delete(account)
        }
    }

    /// A delayed load cannot overwrite typing, and a later delete runs after the queued save.
    #[test]
    fn delayed_loads_do_not_overwrite_edits_and_writes_remain_ordered() {
        let (release, gate) = mpsc::channel();
        let store = Arc::new(Gated { gate: std::sync::Mutex::new(gate), store: MemorySecrets::new(), caller: std::thread::current().id() });
        store.store.save("assistant/custom", &Secret::new("old")).unwrap();
        let ctx = egui::Context::default();
        let mut keys = Keys::default();
        assert_eq!(keys.load("custom", "environment", None, true, Some(store.clone()), &ctx), "environment");
        assert!(keys.loading("custom"));
        keys.edit("custom", "new");
        assert!(!keys.loading("custom"), "typing a session key must unblock requests before the read finishes");
        keys.save("custom", Some(Secret::new("new")), None, Some(store.clone()), &ctx);
        assert!(!keys.loading("custom"), "a queued save must not block requests with the edited key");
        keys.edit("custom", "new");
        keys.save("custom", None, None, Some(store.clone()), &ctx);
        release.send(()).unwrap();
        settle(&mut keys, "custom");
        assert_eq!(keys.key("custom"), Some("new"));
        assert_eq!(store.store.load("assistant/custom").unwrap(), None);
    }

    /// Saving unrelated settings while a key is unread must not enqueue a deletion.
    #[test]
    fn unread_keys_are_not_deleted_by_unrelated_settings_changes() {
        let (release, gate) = mpsc::channel();
        let store = Arc::new(Gated { gate: std::sync::Mutex::new(gate), store: MemorySecrets::new(), caller: std::thread::current().id() });
        store.store.save("assistant/custom", &Secret::new("saved")).unwrap();
        let ctx = egui::Context::default();
        let mut keys = Keys::default();
        keys.load("custom", "", None, true, Some(store.clone()), &ctx);
        keys.save("custom", None, None, Some(store.clone()), &ctx);
        release.send(()).unwrap();
        settle(&mut keys, "custom");
        assert_eq!(keys.key("custom"), Some("saved"));
        assert_eq!(store.store.load("assistant/custom").unwrap(), Some(Secret::new("saved")));
    }

    /// Opting in after an off startup reads the stored value before any empty/environment save.
    #[test]
    fn enabling_remember_reads_an_unedited_key_without_deleting_it() {
        let ctx = egui::Context::default();
        for fallback in ["", "environment"] {
            let store = Arc::new(MemorySecrets::new());
            store.save("assistant/custom", &Secret::new("saved")).unwrap();
            let mut keys = Keys::default();
            keys.load("custom", fallback, None, false, Some(store.clone()), &ctx);
            assert!(keys.enable("custom", None, Some(store.clone()), &ctx));
            keys.save("custom", remembered(fallback, true, fallback), None, Some(store.clone()), &ctx);
            settle(&mut keys, "custom");
            assert_eq!(keys.key("custom"), Some("saved"));
            assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("saved")));
            // A global Remember change also applies when returning to a cached provider.
            let mut cached = Keys::default();
            cached.load("custom", fallback, None, false, Some(store.clone()), &ctx);
            cached.load("custom", fallback, None, true, Some(store.clone()), &ctx);
            settle(&mut cached, "custom");
            assert_eq!(cached.key("custom"), Some("saved"));
            let mut typed = Keys::default();
            typed.load("custom", fallback, None, false, Some(store.clone()), &ctx);
            typed.edit("custom", "typed");
            assert!(!typed.enable("custom", None, Some(store.clone()), &ctx));
            typed.save("custom", Some(Secret::new("typed")), None, Some(store.clone()), &ctx);
            settle(&mut typed, "custom");
            assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("typed")));
        }
    }

    /// A synthetic backend panics on its first operation, then behaves like the real trait.
    struct Panics {
        first: std::sync::atomic::AtomicBool,
        store: MemorySecrets,
    }

    impl Panics {
        /// Exercises an escaped dependency panic without putting a key in its payload.
        fn check(&self) {
            assert!(!self.first.swap(false, std::sync::atomic::Ordering::SeqCst), "synthetic backend panic");
        }
    }

    impl SecretStore for Panics {
        /// The first read may panic instead of returning a contract error.
        fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
            self.check();
            self.store.load(account)
        }
        /// The first migration write may also panic.
        fn save(&self, account: &str, key: &Secret) -> Result<(), SecretError> {
            self.check();
            self.store.save(account, key)
        }
        /// Subsequent deletes still run on the surviving worker.
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.check();
            self.store.delete(account)
        }
    }

    /// Panics produce failures, preserve legacy copies, and leave the worker able to handle edits.
    #[test]
    fn backend_panics_fail_the_operation_without_stalling_or_losing_keys() {
        let ctx = egui::Context::default();
        for legacy in [None, Some("legacy".to_string())] {
            let store = Arc::new(Panics { first: std::sync::atomic::AtomicBool::new(true), store: MemorySecrets::new() });
            let mut keys = Keys::default();
            keys.load("custom", "environment", legacy.clone(), true, Some(store.clone()), &ctx);
            assert!(settle(&mut keys, "custom").is_empty());
            assert!(!keys.loading("custom"));
            assert_eq!(keys.key("custom"), Some(legacy.as_deref().unwrap_or("environment")));
            assert!(matches!(keys.status("custom"), Some(Status::Failed { .. })));
            keys.edit("custom", "replacement");
            keys.save("custom", Some(Secret::new("replacement")), legacy.clone(), Some(store.clone()), &ctx);
            let clear = settle(&mut keys, "custom");
            assert_eq!(clear.len(), usize::from(legacy.is_some()));
            assert_eq!(store.store.load("assistant/custom").unwrap(), Some(Secret::new("replacement")));
        }
    }
}
