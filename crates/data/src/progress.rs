//! Cancelling a long read or write and following its progress (#1291 step 3). Whoever runs it
//! (the app's export on a thread of its own) installs a [`Probe`] on that thread; the writers'
//! loops (and a layout export's pages) say how far they have come with [`begin`] and [`step`],
//! and `step` stops them once the probe was cancelled; [`check`] only stops (the map's wait for
//! its tiles). With none installed, as for a script or the web app, they only run.
//!
//! The analysis crate's run control is a probe, so the same Cancel and progress reach a tool and
//! an export.

use std::cell::RefCell;
use std::sync::Arc;

use crate::{DataError, Result};

/// A run's cancel flag and progress count, as a writer sees them.
pub trait Probe: Send + Sync {
    /// Whether the run was asked to stop.
    fn cancelled(&self) -> bool;
    /// Starts counting a step of `total` items (features, rows, tiles), from none done.
    fn begin(&self, total: u64);
    /// Counts `n` more items of the step done.
    fn advance(&self, n: u64);
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<dyn Probe>>> = const { RefCell::new(None) };
}

/// Makes `probe` this thread's current one until the guard is dropped, which puts back the one
/// before.
pub fn install(probe: Option<Arc<dyn Probe>>) -> Installed {
    Installed(CURRENT.with(|c| c.replace(probe)))
}

/// The calling thread's probe, for installing on workers of the same run.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn current() -> Option<Arc<dyn Probe>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Puts back the probe before when dropped (see [`install`]).
pub struct Installed(Option<Arc<dyn Probe>>);

impl Drop for Installed {
    fn drop(&mut self) {
        let before = self.0.take();
        // Never during the thread's teardown: a guard outlives nothing that late.
        let _ = CURRENT.try_with(|c| c.replace(before));
    }
}

/// Makes the run on this thread stop also once `stop` is set, its progress counted as before,
/// until the guard is dropped: for a caller with a stop of its own (a map frame no longer
/// wanted), so that a reader's long loops that only look at Cancel see it too.
pub fn also_stop_when(stop: Arc<std::sync::atomic::AtomicBool>) -> Installed {
    struct Either(Option<Arc<dyn Probe>>, Arc<std::sync::atomic::AtomicBool>);
    impl Probe for Either {
        fn cancelled(&self) -> bool {
            self.1.load(std::sync::atomic::Ordering::Relaxed) || self.0.as_ref().is_some_and(|p| p.cancelled())
        }
        fn begin(&self, total: u64) {
            if let Some(p) = &self.0 {
                p.begin(total);
            }
        }
        fn advance(&self, n: u64) {
            if let Some(p) = &self.0 {
                p.advance(n);
            }
        }
    }
    let before = CURRENT.with(|c| c.borrow().clone());
    install(Some(Arc::new(Either(before, stop))))
}

/// Starts counting a step of `total` items for the progress.
pub fn begin(total: usize) {
    CURRENT.with(|c| {
        if let Some(p) = c.borrow().as_ref() {
            p.begin(total as u64);
        }
    });
}

/// Counts `n` more items done, and stops the write once its run was asked to: call it for each
/// feature, row or tile of a long loop.
pub fn step(n: usize) -> Result<()> {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(p) if p.cancelled() => Err(DataError::Cancelled),
        Some(p) => {
            p.advance(n as u64);
            Ok(())
        }
        None => Ok(()),
    })
}

/// Runs a nested writer without resetting or advancing its source's scan count. Cancellation
/// still reaches every writer check; dropping the guard restores the source probe on errors too.
pub(crate) fn without_counting<T>(run: impl FnOnce() -> T) -> T {
    struct CancelOnly(Arc<dyn Probe>);
    impl Probe for CancelOnly {
        fn cancelled(&self) -> bool {
            self.0.cancelled()
        }
        fn begin(&self, _: u64) {}
        fn advance(&self, _: u64) {}
    }
    let probe = CURRENT.with(|c| c.borrow().clone());
    let _guard = install(probe.map(|p| Arc::new(CancelOnly(p)) as Arc<dyn Probe>));
    run()
}

/// Stops the write once its run was asked to, counting nothing: for a check after a loop that
/// can't stop by itself.
pub fn check() -> Result<()> {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(p) if p.cancelled() => Err(DataError::Cancelled),
        _ => Ok(()),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// A probe that counts, and is cancelled when told, (with `after`) once that many items
    /// were counted, or (with `after_checks`) once asked that many times, for a loop that only
    /// checks.
    #[derive(Default)]
    pub(crate) struct Counted {
        pub(crate) cancel: AtomicBool,
        pub(crate) done: AtomicU64,
        pub(crate) total: AtomicU64,
        pub(crate) after: Option<u64>,
        pub(crate) after_checks: Option<u64>,
        pub(crate) checks: AtomicU64,
    }

    impl Counted {
        /// A probe cancelled once `n` items were counted.
        pub(crate) fn cancelling_after(n: u64) -> Arc<Self> {
            Arc::new(Counted { after: Some(n), ..Default::default() })
        }
    }

    impl Probe for Counted {
        fn cancelled(&self) -> bool {
            let asked = self.checks.fetch_add(1, Ordering::Relaxed);
            self.cancel.load(Ordering::Relaxed)
                || self.after.is_some_and(|n| self.done.load(Ordering::Relaxed) >= n)
                || self.after_checks.is_some_and(|n| asked >= n)
        }
        fn begin(&self, total: u64) {
            self.total.store(total, Ordering::Relaxed);
            self.done.store(0, Ordering::Relaxed);
        }
        fn advance(&self, n: u64) {
            self.done.fetch_add(n, Ordering::Relaxed);
        }
    }

    #[test]
    fn a_step_counts_until_cancelled_and_the_probe_before_comes_back() {
        assert!(step(1).is_ok(), "without a probe a write only runs");
        let p = Arc::new(Counted::default());
        {
            let _g = install(Some(p.clone()));
            begin(10);
            step(3).unwrap();
            assert_eq!((p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed)), (3, 10));
            without_counting(|| {
                begin(99);
                step(7).unwrap();
            });
            assert_eq!((p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed)), (3, 10));
            p.cancel.store(true, Ordering::Relaxed);
            assert!(matches!(without_counting(|| step(1)), Err(DataError::Cancelled)));
            assert!(matches!(step(1), Err(DataError::Cancelled)));
            assert!(matches!(check(), Err(DataError::Cancelled)));
        }
        assert!(check().is_ok());
        assert!(step(1).is_ok(), "the guard put back no probe");
        // A stop of the caller's own stops the run too, and the progress still reaches the probe.
        let p = Arc::new(Counted::default());
        let _g = install(Some(p.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let _also = also_stop_when(stop.clone());
            begin(4);
            step(1).unwrap();
            stop.store(true, Ordering::Relaxed);
            assert!(matches!(check(), Err(DataError::Cancelled)));
        }
        assert_eq!((p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed)), (1, 4));
        assert!(check().is_ok(), "the caller's stop is gone with its guard");
    }
}
