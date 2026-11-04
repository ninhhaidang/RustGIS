//! Cancelling a running tool and following its progress (#1291). A caller hands
//! [`crate::run_with`] a [`Control`] and keeps a copy: setting its flag from another thread (a
//! Cancel button, a timeout, Ctrl-C) stops the tool at its next check, and its count says how far
//! the tool has come. The run's control is the current one of the thread the tool runs on, so a
//! tool asks [`check`] in its long loops without passing it along; the threads of
//! [`crate::parallel`] take it over from the thread that starts them.
//!
//! A run someone can stop while it goes ([`Control::stoppable`]: the app's panes, `rustgis-cli
//! run`) takes no work cap ([`uncapped`]): the caps, a second or two of work, are for the calls
//! that wait on a tool with no way to stop it (a script, the assistant, the web app).

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A running tool's cancel flag and progress, shared between the tool and whoever runs it.
#[derive(Debug, Default)]
pub struct Control {
    cancel: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
    /// Someone can stop the run while it goes ([`Control::stoppable`]).
    stoppable: bool,
}

impl Control {
    pub fn new() -> Arc<Control> {
        Arc::new(Control::default())
    }

    /// A control for a run someone can stop while it goes, with a Cancel button or Ctrl-C: its
    /// tools take no work cap ([`uncapped`]). Not in the browser, whose one thread a run holds.
    pub fn stoppable() -> Arc<Control> {
        Arc::new(Control { stoppable: !cfg!(target_arch = "wasm32"), ..Control::default() })
    }

    /// Asks the tool to stop at its next check.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The items (rows, features) done of those the tool's current step has, once it has said
    /// how many: `None` before, or for a tool that doesn't count.
    pub fn progress(&self) -> Option<(u64, u64)> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.done.load(Ordering::Relaxed).min(total), total))
    }
}

/// The same flag and count reach the data crate's writers (an export run under the control):
/// their steps count here, and they stop when the run is cancelled.
impl rustgis_data::progress::Probe for Control {
    fn cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn begin(&self, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }
    fn advance(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
}

/// A tool stopped because it was asked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// What [`crate::run_with`] says of a run that was cancelled (it names the tool and how far it
/// came).
pub(crate) const CANCELLED: &str = "cancelled";

impl From<Cancelled> for String {
    fn from(_: Cancelled) -> String {
        CANCELLED.into()
    }
}

impl From<Cancelled> for &'static str {
    fn from(_: Cancelled) -> &'static str {
        CANCELLED
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Control>>> = const { RefCell::new(None) };
}

/// The control of the run on this thread, if any.
pub(crate) fn current() -> Option<Arc<Control>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Makes `control` this thread's current one until the guard is dropped, which puts back the one
/// before (a tool run inside another's keeps the outer run's afterwards).
pub(crate) fn install(control: Option<Arc<Control>>) -> Installed {
    Installed(CURRENT.with(|c| c.replace(control)))
}

pub(crate) struct Installed(Option<Arc<Control>>);

impl Drop for Installed {
    fn drop(&mut self) {
        let before = self.0.take();
        // Never during the thread's teardown: a guard outlives nothing that late.
        let _ = CURRENT.try_with(|c| c.replace(before));
    }
}

/// Whether the run on this thread was asked to stop.
pub(crate) fn cancelled() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|c| c.is_cancelled()))
}

/// Whether the run on this thread can be stopped while it goes ([`Control::stoppable`]), and so
/// takes no work cap.
pub(crate) fn uncapped() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|c| c.stoppable))
}

/// The work units (a distance, a ray step, a multiply-add) between two looks at the cancel flag
/// inside an item of a loop ([`Pace`]): a few milliseconds.
const PACE_WORK: usize = 1 << 22;

/// Looks at the cancel flag about every [`PACE_WORK`] units of work, within an item that may
/// take long on its own (a row of a wide grid without a work cap), so Cancel stops the run soon.
#[derive(Default)]
pub(crate) struct Pace(usize);

impl Pace {
    /// Whether to stop after `work` more units: once the run was asked to.
    pub(crate) fn stop(&mut self, work: usize) -> bool {
        self.0 = self.0.saturating_add(work);
        if self.0 < PACE_WORK {
            return false;
        }
        self.0 = 0;
        cancelled()
    }
}

/// Stops the tool when its run was asked to: call it between rows or features of a long loop.
pub(crate) fn check() -> Result<(), Cancelled> {
    if cancelled() { Err(Cancelled) } else { Ok(()) }
}

/// Starts counting a step of `total` items (rows, features) for the progress, from none done.
pub(crate) fn begin(total: usize) {
    CURRENT.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.done.store(0, Ordering::Relaxed);
            c.total.store(total as u64, Ordering::Relaxed);
        }
    });
}

/// Says that the first `done` items of the step are made: for a loop over them, at the top of each
/// (its index).
pub(crate) fn reached(done: usize) {
    CURRENT.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.done.store(done as u64, Ordering::Relaxed);
        }
    });
}

/// Counts every item of the step done: the tool has finished it (a loop counting with [`reached`]
/// otherwise ends one short).
pub(crate) fn finish() {
    CURRENT.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.done.store(c.total.load(Ordering::Relaxed), Ordering::Relaxed);
        }
    });
}

/// Counts `n` more items of the step done.
pub(crate) fn advance(n: usize) {
    CURRENT.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.done.fetch_add(n as u64, Ordering::Relaxed);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_sees_its_own_control_and_the_one_before_comes_back() {
        assert!(current().is_none() && check().is_ok());
        let outer = Control::new();
        {
            let _g = install(Some(outer.clone()));
            begin(10);
            advance(4);
            assert_eq!(outer.progress(), Some((4, 10)));
            let inner = Control::new();
            {
                let _g = install(Some(inner.clone()));
                inner.cancel();
                assert_eq!(check(), Err(Cancelled));
            }
            assert!(check().is_ok(), "the outer run wasn't cancelled");
            outer.cancel();
            assert!(cancelled());
        }
        assert!(current().is_none() && check().is_ok());
    }

    #[test]
    fn only_a_stoppable_run_is_uncapped_and_pace_sees_its_cancel() {
        assert!(!uncapped(), "no run");
        {
            let _g = install(Some(Control::new()));
            assert!(!uncapped(), "a run nobody can stop keeps its caps");
        }
        let c = Control::stoppable();
        let _g = install(Some(c.clone()));
        assert!(uncapped());
        let mut pace = Pace::default();
        assert!(!pace.stop(PACE_WORK), "not cancelled");
        c.cancel();
        assert!(!pace.stop(1), "not looked at again before PACE_WORK more");
        assert!(pace.stop(PACE_WORK));
    }

    #[test]
    fn no_progress_before_a_step_is_counted() {
        let c = Control::new();
        assert_eq!(c.progress(), None);
        let _g = install(Some(c.clone()));
        begin(3);
        advance(5);
        assert_eq!(c.progress(), Some((3, 3)), "never past the total");
    }
}
