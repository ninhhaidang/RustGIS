//! The threads one piece of work may use (#1562): a tool's rows or features, a raster's chunks,
//! the tiles of a frame. The places that start threads for CPU work take their count from [`max`],
//! so the setting (Settings › Performance › Threads on the desktop, `gp.threads`, the command
//! line's `--threads`, Python's `Session(threads=…)`) reaches all of them. Drawing a raster on the
//! map stays at 8 threads at most; threads that wait on the network (a data cube's readers, a Zarr
//! store's fetches) keep their own counts.
//!
//! By default the count is the computer's cores, at most [`DEFAULT_MAX`]. A setting replaces that
//! cap, lower to keep cores free for other work or higher on a large machine; it is never more
//! than the cores. The setting holds for the whole process and isn't saved with a project. The
//! browser has no threads, so there the count is always one.

use std::sync::atomic::{AtomicUsize, Ordering};

/// The most threads one piece of work uses until a setting says otherwise.
pub const DEFAULT_MAX: usize = 16;
/// The largest setting taken: far beyond any computer's cores, so a script written for a large
/// machine runs unchanged on a small one (it gets that machine's cores).
pub const MOST: usize = 1024;

/// The setting, `0` for none (the default).
static SETTING: AtomicUsize = AtomicUsize::new(0);

/// The computer's cores (the threads it runs at once), read once; one in the browser.
pub fn cores() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *CORES.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        1
    }
}

/// The most threads one piece of work may use now: the setting, or [`DEFAULT_MAX`] without one,
/// never more than the cores; one in the browser.
pub fn max() -> usize {
    effective(setting(), cores())
}

/// [`max`] without a setting: the cores, at most [`DEFAULT_MAX`].
pub fn default_max() -> usize {
    effective(None, cores())
}

/// A setting given as JSON: a whole number from 1 to [`MOST`], or `null` for the default.
pub fn parse(v: &serde_json::Value) -> Result<Option<usize>, String> {
    match v {
        serde_json::Value::Null => Ok(None),
        _ => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=MOST).contains(n))
            .map(Some)
            .ok_or_else(|| format!("threads is a whole number from 1 to {MOST}, or null for the default")),
    }
}

/// The most threads to make `items` independent items on: [`max`], no more than there are items,
/// and at least one.
pub fn for_items(items: usize) -> usize {
    max().min(items).max(1)
}

/// The setting, if one was made.
pub fn setting() -> Option<usize> {
    Some(SETTING.load(Ordering::Relaxed)).filter(|n| *n > 0)
}

/// Sets the most threads one piece of work may use, for the whole process (`None`: the default).
/// A setting from 1 to [`MOST`] is taken; one above the cores means all of them. Work already
/// running keeps the threads it started with; work done in pieces (a data cube's trend) takes
/// the new count from its next piece.
pub fn set_max(threads: Option<usize>) -> Result<(), String> {
    match threads {
        Some(n) if !(1..=MOST).contains(&n) => Err(format!("threads is 1 to {MOST}, or null for the default")),
        _ => {
            SETTING.store(threads.unwrap_or(0), Ordering::Relaxed);
            Ok(())
        }
    }
}

/// [`max`] for a `setting` on a computer with `cores` cores.
fn effective(setting: Option<usize>, cores: usize) -> usize {
    setting.unwrap_or(DEFAULT_MAX).min(cores).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_follows_the_setting_up_to_the_cores() {
        assert_eq!(effective(None, 8), 8, "the default: every core of a small machine");
        assert_eq!(effective(None, 64), DEFAULT_MAX, "the default cap on a large one");
        assert_eq!(effective(Some(4), 64), 4, "lower, to keep cores free");
        assert_eq!(effective(Some(48), 64), 48, "higher than the default cap");
        assert_eq!(effective(Some(48), 8), 8, "never more than the cores");
        assert_eq!(effective(Some(4), 0), 1, "at least one");
    }

    #[test]
    fn a_setting_out_of_range_is_refused_and_changes_nothing() {
        let before = setting();
        for n in [0, MOST + 1] {
            let e = set_max(Some(n)).unwrap_err();
            assert!(e.contains("1 to 1024"), "{e}");
            assert_eq!(setting(), before);
        }
    }

    #[test]
    fn a_count_is_at_least_one_and_no_more_than_the_items() {
        assert_eq!(for_items(0), 1);
        assert_eq!(for_items(1), 1);
        assert!(for_items(usize::MAX) <= cores());
        assert!(max() >= 1 && max() <= cores());
        assert_eq!(default_max(), cores().min(DEFAULT_MAX));
    }

    #[test]
    fn a_setting_is_read_from_json() {
        use serde_json::json;
        assert_eq!(parse(&json!(null)), Ok(None));
        assert_eq!(parse(&json!(4)), Ok(Some(4)));
        assert_eq!(parse(&json!(1024)), Ok(Some(1024)));
        for bad in [json!(0), json!(1025), json!(-1), json!(2.5), json!("4"), json!([4])] {
            assert!(parse(&bad).unwrap_err().contains("1 to 1024"), "{bad}");
        }
    }
}
