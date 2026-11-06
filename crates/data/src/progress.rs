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

/// Puts back the probe before when dropped (see [`install`]).
pub struct Installed(Option<Arc<dyn Probe>>);

impl Drop for Installed {
    fn drop(&mut self) {
        let before = self.0.take();
        // Never during the thread's teardown: a guard outlives nothing that late.
        let _ = CURRENT.try_with(|c| c.replace(before));
    }
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

/// Stops the write once its run was asked to, counting nothing: for a check after a loop that
/// can't stop by itself.
pub fn check() -> Result<()> {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(p) if p.cancelled() => Err(DataError::Cancelled),
        _ => Ok(()),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// A probe that counts, and is cancelled when told or (with `after`) once that many items
    /// were counted.
    #[derive(Default)]
    pub(crate) struct Counted {
        pub(crate) cancel: AtomicBool,
        pub(crate) done: AtomicU64,
        pub(crate) total: AtomicU64,
        pub(crate) after: Option<u64>,
    }

    impl Counted {
        /// A probe cancelled once `n` items were counted.
        pub(crate) fn cancelling_after(n: u64) -> Arc<Self> {
            Arc::new(Counted { after: Some(n), ..Default::default() })
        }
    }

    impl Probe for Counted {
        fn cancelled(&self) -> bool {
            self.cancel.load(Ordering::Relaxed) || self.after.is_some_and(|n| self.done.load(Ordering::Relaxed) >= n)
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
            p.cancel.store(true, Ordering::Relaxed);
            assert!(matches!(step(1), Err(DataError::Cancelled)));
            assert!(matches!(check(), Err(DataError::Cancelled)));
        }
        assert!(check().is_ok());
        assert!(step(1).is_ok(), "the guard put back no probe");
    }
}
