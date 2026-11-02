//! Cancelling a running tool and following its progress (#1291). A caller hands
//! [`crate::run_with`] a [`Control`] and keeps a copy: setting its flag from another thread (a
//! Cancel button, a timeout, Ctrl-C) stops the tool at its next check, and its count says how far
//! the tool has come. The run's control is the current one of the thread the tool runs on, so a
//! tool asks [`check`] in its long loops without passing it along; the threads of
//! [`crate::parallel`] take it over from the thread that starts them.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A running tool's cancel flag and progress, shared between the tool and whoever runs it.
#[derive(Debug, Default)]
pub struct Control {
    cancel: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
}

impl Control {
    pub fn new() -> Arc<Control> {
        Arc::new(Control::default())
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
    fn no_progress_before_a_step_is_counted() {
        let c = Control::new();
        assert_eq!(c.progress(), None);
        let _g = install(Some(c.clone()));
        begin(3);
        advance(5);
        assert_eq!(c.progress(), Some((3, 3)), "never past the total");
    }
}
