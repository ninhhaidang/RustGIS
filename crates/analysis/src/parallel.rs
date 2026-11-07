//! Work spread over the machine's cores (#1252): a tool's independent items (features, raster
//! rows) made on several threads, in their order, as one thread would make them.
//!
//! The browser has no threads, so there everything runs on the calling thread, as it does below
//! a tool's threshold, where starting threads costs more than they save.

use crate::control::{self, Cancelled};

/// The most threads a tool uses.
const MAX_THREADS: usize = 16;
/// The items a thread takes at a time: few enough that expensive items lying together are shared
/// out, enough that taking them costs nothing.
pub(crate) const BATCH: usize = 16;
/// Each worker's stack, as large as the main thread's usually is.
const STACK_BYTES: usize = 8 << 20;

/// The work below which a raster tool's rows are made on one thread, in the tool's own units:
/// cell-point pairs for IDW (about 30 ns each, so some 5 ms of it), the same plus each cell's
/// system for kriging, ray steps for a viewshed. Starting a thread costs tens of microseconds.
pub(crate) const MIN_ROW_WORK: usize = 200_000;
/// The scratch memory a tool's threads may take together beyond one thread's, in the browser and
/// at least on the desktop ([`scratch_budget`]).
const SCRATCH_FLOOR: usize = 256 << 20;
/// The most [`scratch_budget`] allows, however much memory the computer has.
const SCRATCH_CEILING: usize = 4 << 30;

/// The scratch memory a tool's threads may take together beyond one thread's: on the desktop a
/// sixteenth of the computer's memory, on top of the quarter its input and output limits plan for,
/// from [`SCRATCH_FLOOR`] up to [`SCRATCH_CEILING`] (#1291 step 2).
pub fn scratch_budget() -> usize {
    rustgis_data::memory::scaled(rustgis_data::memory::physical().map(|m| m / 4), SCRATCH_FLOOR, SCRATCH_CEILING, 1)
}

/// How many threads to make `items` items on: one below `min_items`, always one in the browser,
/// and no more than there are batches.
pub(crate) fn threads_for(items: usize, min_items: usize) -> usize {
    if cfg!(target_arch = "wasm32") || items < min_items {
        return 1;
    }
    cores().min(items.div_ceil(BATCH)).max(1)
}

/// [`threads_for`], each thread holding up to `scratch_bytes` at its peak (a feature's raster
/// masks, say): beyond one thread's, no more than [`scratch_budget`] holds.
pub(crate) fn threads_for_scratch(items: usize, min_items: usize, scratch_bytes: usize) -> usize {
    threads_for(items, min_items).min(1 + scratch_budget() / scratch_bytes.max(1))
}

/// How many threads to make `rows` raster rows on, `work` in all, each thread taking
/// `scratch_bytes` of scratch: one below [`MIN_ROW_WORK`] and in the browser, no more than there
/// are rows, and, beyond one thread's scratch, no more than [`scratch_budget`] holds (a scratch per point, for millions of
/// points, is large).
pub(crate) fn threads_for_rows(rows: usize, work: usize, scratch_bytes: usize) -> usize {
    if cfg!(target_arch = "wasm32") || work < MIN_ROW_WORK {
        return 1;
    }
    let affordable = 1 + scratch_budget() / scratch_bytes.max(1);
    cores().min(rows).min(affordable).max(1)
}

/// A work cap tuned for one thread (a second or two of its work) for a run on `threads` threads:
/// as many times larger, since the threads share the work and the run takes about as long
/// (#1291 step 2). On one thread, and so in the browser, the cap as it is. None for a run that
/// can be stopped while it goes ([`crate::control::uncapped`]): its progress shows, and Cancel
/// stops it.
pub(crate) fn work_cap(one_thread: usize, threads: usize) -> usize {
    if crate::control::uncapped() { usize::MAX } else { one_thread.saturating_mul(threads.max(1)) }
}

/// What a refusal past [`work_cap`] adds: where the work isn't capped. Nothing in the browser.
pub(crate) const UNCAPPED_HINT: &str = if cfg!(target_arch = "wasm32") {
    ""
} else {
    " (or run it from its pane in the desktop app, which takes any amount of work and can be cancelled)"
};

/// The cores to use, at most [`MAX_THREADS`].
fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).min(MAX_THREADS)
}

/// `f` on each of a raster's `rows` (its output rows, `chunks_mut` of the bands it writes) with
/// the row's number, on `threads` threads ([`threads_for_rows`]), this one among them. Each row
/// is written where it lies, so the output is held once, as on one thread. Rows are taken a few
/// at a time, by the rows still left: one each when there are few per thread (and so at the
/// end), so rows that cost more than others are shared out, up to [`BATCH`] when there are many,
/// so cheap rows don't wait on the lock.
/// Each thread makes its own `scratch()` once; one that panics stops the others after the rows
/// they are making, and its panic is raised again here. Each row made counts toward the run's
/// progress; a cancelled run stops after the rows being made, and the rest are left unwritten.
pub(crate) fn for_each_row<T: Send, S>(
    rows: impl ExactSizeIterator<Item = T> + Send,
    threads: usize,
    scratch: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize, T) + Sync,
) -> Result<(), Cancelled> {
    use std::sync::atomic::{AtomicBool, Ordering};
    if threads <= 1 {
        let mut s = scratch();
        for (i, row) in rows.enumerate() {
            control::check()?;
            f(&mut s, i, row);
            control::advance(1);
        }
        return Ok(());
    }
    let run = control::current();
    let still = rustgis_data::source::remote::current_still();
    let rows = std::sync::Mutex::new(rows.enumerate());
    let failed = AtomicBool::new(false);
    let work = || {
        rustgis_data::source::remote::with_optional_still(still.clone(), || {
            let _stop = StopOnPanic(&failed);
            let _run = control::install(run.clone());
            let mut s = scratch();
            let mut taken = Vec::with_capacity(BATCH);
            while !failed.load(Ordering::Relaxed) && !control::cancelled() {
                // The lock is held only to take rows; a poisoned one means a thread panicked.
                if let Ok(mut r) = rows.lock() {
                    let take = (r.len() / threads.saturating_mul(BATCH)).clamp(1, BATCH);
                    taken.extend(r.by_ref().take(take));
                }
                if taken.is_empty() {
                    break;
                }
                let n = taken.len();
                for (i, row) in taken.drain(..) {
                    f(&mut s, i, row);
                }
                control::advance(n);
            }
        })
    };
    std::thread::scope(|scope| {
        let others: Vec<_> =
            (1..threads).filter_map(|_| std::thread::Builder::new().stack_size(STACK_BYTES).spawn_scoped(scope, work).ok()).collect();
        work();
        for h in others {
            if let Err(e) = h.join() {
                std::panic::resume_unwind(e);
            }
        }
    });
    control::check()
}

/// `f` on each of `items` with its index, the results in their order: on several threads when
/// there are at least `min_items` of them (one in the browser). Each thread makes its own
/// `scratch()` once and passes it to every `f` it runs, for buffers to reuse; the index lets a
/// raster tool map over its rows' numbers. The first error stops the threads after the items
/// they are making, and comes back instead of the results: on one thread the first in input
/// order, on several the earliest of those found before they stopped. Each item made counts toward
/// the run's progress; a cancelled run stops likewise, with [`Cancelled`] as its error.
pub(crate) fn try_map<T: Sync, S, R: Send, E: Send + From<Cancelled>>(
    items: &[T],
    min_items: usize,
    scratch: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize, &T) -> Result<R, E> + Sync,
) -> Result<Vec<R>, E> {
    try_map_on(items, threads_for(items.len(), min_items), scratch, f)
}

/// [`try_map`] on `threads` threads, this one among them. Each takes the next [`BATCH`] of items
/// until none are left or one has failed, so the work is shared however it is spread, and the
/// results go back in their places. A thread that can't start (a limit on threads, say) leaves
/// its share to the others; one that panics passes its panic on, as an item made here would.
pub(crate) fn try_map_on<T: Sync, S, R: Send, E: Send + From<Cancelled>>(
    items: &[T],
    threads: usize,
    scratch: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize, &T) -> Result<R, E> + Sync,
) -> Result<Vec<R>, E> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    if threads <= 1 || items.len() <= 1 {
        let mut s = scratch();
        return items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                control::check()?;
                let r = f(&mut s, i, item);
                control::advance(1);
                r
            })
            .collect();
    }
    let run = control::current();
    let still = rustgis_data::source::remote::current_still();
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let work = || {
        rustgis_data::source::remote::with_optional_still(still.clone(), || {
            let _stop = StopOnPanic(&failed);
            let _run = control::install(run.clone());
            let mut s = scratch();
            let mut made = Vec::new();
            while !failed.load(Ordering::Relaxed) {
                let from = next.fetch_add(BATCH, Ordering::Relaxed);
                let Some(batch) = items.get(from..items.len().min(from.saturating_add(BATCH))).filter(|b| !b.is_empty()) else { break };
                // Cancel is looked at, and progress counted, item by item, as on one thread: an item
                // can take long (a large zone, say), so a batch of them would hold both back.
                for (k, item) in batch.iter().enumerate() {
                    if control::cancelled() {
                        failed.store(true, Ordering::Relaxed);
                        return Err((from + k, Cancelled.into()));
                    }
                    match f(&mut s, from + k, item) {
                        Ok(r) => made.push((from + k, r)),
                        Err(e) => {
                            failed.store(true, Ordering::Relaxed);
                            return Err((from + k, e));
                        }
                    }
                    control::advance(1);
                }
            }
            Ok(made)
        })
    };

    let mut parts = Vec::with_capacity(threads);
    std::thread::scope(|scope| {
        let others: Vec<_> =
            (1..threads).filter_map(|_| std::thread::Builder::new().stack_size(STACK_BYTES).spawn_scoped(scope, work).ok()).collect();
        parts.push(work());
        for h in others {
            // The worker's panic, raised again here as a serial run would raise it, for
            // `analysis::run` to catch.
            parts.push(h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)));
        }
    });
    let mut made: Vec<(usize, R)> = Vec::with_capacity(items.len());
    let mut first: Option<(usize, E)> = None;
    for part in parts {
        match part {
            Ok(m) => made.extend(m),
            Err((i, e)) => {
                if first.as_ref().is_none_or(|(j, _)| i < *j) {
                    first = Some((i, e));
                }
            }
        }
    }
    if let Some((_, e)) = first {
        return Err(e);
    }
    made.sort_unstable_by_key(|(i, _)| *i);
    Ok(made.into_iter().map(|(_, r)| r).collect())
}

/// Sets the stop flag when the thread it lives on panics, so the others stop after the items
/// they are making rather than make the rest for nothing.
struct StopOnPanic<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The raster row workers inherit the initiating predicate, rather than creating a root
    /// request context on a new thread.
    #[test]
    fn raster_rows_keep_their_request_context() {
        let mut rows = vec![0u8; 256];
        rustgis_data::source::remote::with_still(std::sync::Arc::new(|| true), || {
            assert_eq!(
                for_each_row(
                    rows.chunks_mut(1),
                    4,
                    || (),
                    |_, _, row| {
                        assert!(rustgis_data::source::remote::current_still().is_some());
                        row[0] = 1;
                    }
                ),
                Ok(())
            );
            let items = try_map_on(
                &rows,
                4,
                || (),
                |_, _, value| {
                    assert!(rustgis_data::source::remote::current_still().is_some());
                    Ok::<_, Cancelled>(*value)
                },
            );
            assert_eq!(items, Ok(vec![1; 256]));
        });
        assert!(rustgis_data::source::remote::current_still().is_none());
    }

    #[test]
    fn a_work_cap_grows_with_the_threads_a_run_takes() {
        assert_eq!(work_cap(400, 1), 400);
        assert_eq!(work_cap(400, 16), 6400);
        assert_eq!(work_cap(400, 0), 400, "no threads counts as one");
        assert_eq!(work_cap(usize::MAX, 2), usize::MAX, "no overflow");
    }

    /// The infallible case, for the tests.
    fn map_on<T: Sync, S, R: Send>(
        items: &[T],
        threads: usize,
        scratch: impl Fn() -> S + Sync,
        f: impl Fn(&mut S, usize, &T) -> R + Sync,
    ) -> Vec<R> {
        try_map_on(items, threads, scratch, |s, i, item| Ok::<R, Cancelled>(f(s, i, item))).unwrap_or_default()
    }

    fn map<T: Sync, S, R: Send>(
        items: &[T],
        min_items: usize,
        scratch: impl Fn() -> S + Sync,
        f: impl Fn(&mut S, usize, &T) -> R + Sync,
    ) -> Vec<R> {
        try_map(items, min_items, scratch, |s, i, item| Ok::<R, Cancelled>(f(s, i, item))).unwrap_or_default()
    }

    #[test]
    fn items_made_on_several_threads_come_back_in_their_order() {
        let items: Vec<u64> = (0..1_000).collect();
        let alone: Vec<u64> = items.iter().map(|x| x * x + 1).collect();
        // Each item comes with its index, here equal to its value.
        let square = |_: &mut (), i: usize, x: &u64| {
            assert_eq!(i as u64, *x);
            x * x + 1
        };
        for threads in [1, 2, 3, 16, 64] {
            assert_eq!(map_on(&items, threads, || (), square), alone, "{threads} threads");
            assert_eq!(map_on(&items[..3], threads, || (), square), alone[..3], "{threads} threads, 3 items");
            assert!(map_on(&items[..0], threads, || (), square).is_empty());
        }
        // Below the threshold, and in the browser, one thread.
        assert_eq!(threads_for(10, 256), 1);
        assert_eq!(map(&items, 256, || (), square), alone);
    }

    #[test]
    fn each_thread_makes_its_scratch_once() {
        let made = std::sync::atomic::AtomicUsize::new(0);
        let items: Vec<usize> = (0..1_000).collect();
        // Each item records how many it saw before it on its thread, from that thread's scratch.
        let seen = map_on(
            &items,
            4,
            || {
                made.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                0usize
            },
            |count, _, _| {
                *count += 1;
                *count
            },
        );
        let scratches = made.load(std::sync::atomic::Ordering::Relaxed);
        assert!((1..=4).contains(&scratches), "{scratches}");
        // A thread's first item sees 1; a thread that found nothing left has no first item.
        let firsts = seen.iter().filter(|c| **c == 1).count();
        assert!((1..=scratches).contains(&firsts), "{firsts} of {scratches}");
    }

    #[test]
    fn a_workers_panic_reaches_the_caller() {
        // Only the workers panic, not this thread. Each item takes a moment, so workers get some,
        // unless none could start: then nothing panics, which is right too.
        let items: Vec<u32> = (0..200).collect();
        let caller = std::thread::current().id();
        let on_worker = std::sync::atomic::AtomicBool::new(false);
        let r = std::panic::catch_unwind(|| {
            map_on(
                &items,
                4,
                || (),
                |_, _, x| {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    if std::thread::current().id() != caller {
                        on_worker.store(true, std::sync::atomic::Ordering::Relaxed);
                        panic!("a worker's panic");
                    }
                    *x
                },
            )
        });
        assert_eq!(r.is_err(), on_worker.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn a_failing_item_stops_the_threads_and_its_error_comes_back() {
        let items: Vec<u32> = (0..1_000).collect();
        let f = |_: &mut (), _: usize, x: &u32| if *x == 500 || *x == 700 { Err(x.to_string()) } else { Ok(*x) };
        // On one thread, the first in input order.
        assert_eq!(try_map_on(&items, 1, || (), f), Err("500".to_string()));
        for threads in [2, 3, 16] {
            let e = try_map_on(&items, threads, || (), f).err();
            assert!(matches!(e.as_deref(), Some("500" | "700")), "{threads} threads");
            assert_eq!(try_map_on(&items, threads, || (), |_, _, x| Ok::<_, Cancelled>(*x)), Ok(items.clone()), "{threads} threads");
        }
        // Items still waiting when one fails are left: far fewer than all of them run.
        let ran = std::sync::atomic::AtomicUsize::new(0);
        let r = try_map_on(
            &items,
            4,
            || (),
            |_, _, x| {
                ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if *x == 0 { Err(Cancelled) } else { Ok(()) }
            },
        );
        assert!(r.is_err());
        assert!(ran.load(std::sync::atomic::Ordering::Relaxed) < items.len());
    }

    #[test]
    fn each_row_is_written_in_its_place() {
        let alone: Vec<usize> = (0..37 * 5).map(|i| i / 5 * 3 + i % 5).collect();
        for threads in [1, 2, 5, 16, 64] {
            let mut out = vec![0usize; 37 * 5];
            let made = for_each_row(
                out.chunks_mut(5),
                threads,
                || (),
                |_, r, row: &mut [usize]| {
                    row.iter_mut().enumerate().for_each(|(c, o)| *o = r * 3 + c);
                },
            );
            assert_eq!(made, Ok(()));
            assert_eq!(out, alone, "{threads} threads");
        }
        // Many cheap rows per thread, taken several at a time.
        let tall: Vec<usize> = (0..5_000).collect();
        for threads in [2, 16] {
            let mut out = vec![0usize; 5_000];
            assert_eq!(for_each_row(out.chunks_mut(1), threads, || (), |_, r, row: &mut [usize]| row[0] = r), Ok(()));
            assert_eq!(out, tall, "{threads} threads");
        }
        let mut none: Vec<u8> = Vec::new();
        assert_eq!(for_each_row(none.chunks_mut(5), 4, || (), |_, _, _: &mut [u8]| panic!("no rows")), Ok(()));
        // Small work, and the browser: one thread; never more threads than rows.
        assert_eq!(threads_for_rows(100, MIN_ROW_WORK - 1, 0), 1);
        assert!(threads_for_rows(3, usize::MAX, 0) <= 3);
        // Scratch larger than the budget: one thread more than it holds, none beyond.
        assert!(threads_for_rows(100, usize::MAX, scratch_budget()) <= 2);
        assert_eq!(threads_for_rows(100, usize::MAX, usize::MAX), 1);
    }

    #[test]
    fn a_workers_panic_stops_the_other_threads() {
        // A worker panics on its first item; the others stop after the items they are making,
        // far short of all of them.
        let items: Vec<u32> = (0..2_000).collect();
        let caller = std::thread::current().id();
        let ran = std::sync::atomic::AtomicUsize::new(0);
        let r = std::panic::catch_unwind(|| {
            map_on(
                &items,
                4,
                || (),
                |_, _, x| {
                    ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if std::thread::current().id() != caller {
                        panic!("a worker's panic");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    *x
                },
            )
        });
        assert!(r.is_err(), "no worker ran an item");
        assert!(ran.load(std::sync::atomic::Ordering::Relaxed) < items.len() / 2, "{}", ran.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn a_cancelled_run_stops_its_threads_and_counts_what_they_made() {
        for threads in [1, 4] {
            let control = crate::control::Control::new();
            let _run = crate::control::install(Some(control.clone()));
            // Rows: the run is cancelled on the 100th; the threads stop after the rows they hold.
            let made = std::sync::atomic::AtomicUsize::new(0);
            let mut out = vec![0u8; 5_000];
            crate::control::begin(out.len());
            let r = for_each_row(
                out.chunks_mut(1),
                threads,
                || (),
                |_, _, row: &mut [u8]| {
                    if made.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 99 {
                        control.cancel();
                    }
                    row[0] = 1;
                },
            );
            assert_eq!(r, Err(Cancelled), "{threads} threads");
            let made = made.load(std::sync::atomic::Ordering::Relaxed);
            assert!(made < 1_000, "{threads} threads made {made}");
            let (done, total) = control.progress().unwrap_or_default();
            assert!(total == 5_000 && done >= 100 && done <= made as u64, "{threads} threads: {done} of {total}");
            // Items, likewise.
            let control2 = crate::control::Control::new();
            let _inner = crate::control::install(Some(control2.clone()));
            let items: Vec<u32> = (0..5_000).collect();
            let r = try_map_on(
                &items,
                threads,
                || (),
                |_, i, x| {
                    if i == 99 {
                        control2.cancel();
                    }
                    Ok::<_, String>(*x)
                },
            );
            assert_eq!(r.err().as_deref(), Some(crate::control::CANCELLED), "{threads} threads");
        }
    }

    #[test]
    fn a_workers_panic_on_a_row_reaches_the_caller_and_stops_the_others() {
        let caller = std::thread::current().id();
        let ran = std::sync::atomic::AtomicUsize::new(0);
        let mut out = vec![0u8; 2_000];
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for_each_row(
                out.chunks_mut(1),
                4,
                || (),
                |_, _, _: &mut [u8]| {
                    ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if std::thread::current().id() != caller {
                        panic!("a worker's panic");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                },
            )
        }));
        assert!(r.is_err(), "no worker took a row");
        assert!(ran.load(std::sync::atomic::Ordering::Relaxed) < 1_000, "{}", ran.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn no_more_threads_than_batches() {
        assert_eq!(threads_for(10, 256), 1);
        assert!(threads_for(20, 1) <= 2);
        assert!(threads_for(300, 1) <= 300usize.div_ceil(BATCH));
    }
}
