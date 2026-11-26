//! The computer's memory, and the limits that follow it (#1291 step 2, #1318).
//!
//! On the desktop a memory limit plans for a share of the physical memory ([`SHARE`]): never below
//! the fixed value it had before (its floor, still the browser's limit), never above a ceiling. A
//! control-group limit (a container's, or a service's such as systemd's `MemoryMax`) counts, when
//! it is lower. The memory is read once, at the first limit
//! asked for; [`scaled`] takes it as an argument, so tests pass fixed amounts. Most limits plan for
//! what is left once the process holds more than half the memory ([`planned`], #2431 fix 3), so
//! they can be lower later in a session, never below their floors.
//!
//! A phone ([`set_phone`]) plans the same way from what its system allows, which its app says in
//! place of the memory read here, and takes the browser's floors where those are lower.

use std::sync::atomic::{AtomicU64, Ordering};

/// A limit plans for a quarter of the physical memory: room for the layers already held, the
/// drawing, and a second job running at the same time.
pub const SHARE: u64 = 4;

/// The computer's physical memory in bytes (a container's limit, when it is lower), read once;
/// `None` in the browser, or when it can't be read. Also `None` in a fuzzing build (cargo-fuzz sets
/// `cfg(fuzzing)`), so the limits sit at their floors: a quarter of the runner's memory is more than
/// the fuzzer's 2 GB memory cap, which a header claiming a vast image would reach by design (#1952).
///
/// On the desktop, [`OVERRIDE`] set to a positive whole number of bytes replaces the memory read,
/// a container's limit included, for tests and to reproduce a smaller computer; any other value is
/// ignored. [`source`] says which one was used.
///
/// On a phone it is the figure its app gave ([`set_phone`]), unless [`OVERRIDE`] is set.
pub fn physical() -> Option<u64> {
    reading().0
}

/// Where [`physical`]'s figure came from: `"override"` ([`OVERRIDE`]), `"container"` (a
/// control-group limit, a container's or a service's such as systemd's `MemoryMax`, lower than
/// the computer's memory), `"computer"`, `"phone"` ([`set_phone`]), or `"unknown"` (the browser,
/// a fuzzing build, or memory that couldn't be read), so a leftover override doesn't change the
/// limits unnoticed.
pub fn source() -> &'static str {
    reading().1
}

/// The memory and its source, read once.
fn reading() -> (Option<u64>, &'static str) {
    #[cfg(fuzzing)]
    {
        (None, "unknown")
    }
    #[cfg(all(not(fuzzing), not(target_arch = "wasm32")))]
    {
        static MEMORY: std::sync::OnceLock<(Option<u64>, &'static str)> = std::sync::OnceLock::new();
        *MEMORY.get_or_init(read)
    }
    #[cfg(all(not(fuzzing), target_arch = "wasm32"))]
    {
        (None, "unknown")
    }
}

/// What a phone's system allows RustGIS ([`set_phone`]).
#[cfg(not(target_arch = "wasm32"))]
struct Phone {
    /// The bytes it plans with, in place of the device's memory.
    allowed: u64,
    /// The bytes free now; `None` when the system can't say.
    free: Box<dyn Fn() -> Option<u64> + Send + Sync>,
}

/// The phone's allowance, or that there is none: settled by [`set_phone`] or by the first read of
/// the memory, whichever comes first.
#[cfg(not(target_arch = "wasm32"))]
static PHONE: std::sync::OnceLock<Option<Phone>> = std::sync::OnceLock::new();

/// Says RustGIS runs on a phone, for the phone app to call once, before anything asks for a limit.
/// The limits then plan with `allowed` bytes in place of the memory read here ([`physical`]):
/// what the system lets the app use where it says (iOS), else the device's memory (Android). They
/// plan for no more than `free` says is free each time it is asked
/// ([`available`]; any thread asks). Where the browser's floor is lower than the desktop's, they
/// take it ([`on_phone`]). Returns whether it took: not for no bytes, a second call, or once the
/// memory has been read.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_phone(allowed: u64, free: impl Fn() -> Option<u64> + Send + Sync + 'static) -> bool {
    allowed > 0 && PHONE.set(Some(Phone { allowed, free: Box::new(free) })).is_ok()
}

/// Whether a phone's app said what its system allows (`set_phone`, which the browser lacks).
pub fn on_phone() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        matches!(PHONE.get(), Some(Some(_)))
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// The environment variable that replaces the memory [`physical`] reads.
pub const OVERRIDE: &str = "RUSTGIS_MEMORY_BYTES";

/// The memory [`OVERRIDE`]'s value gives: a positive whole number of bytes, else `None`.
#[cfg(any(test, all(not(fuzzing), not(target_arch = "wasm32"))))]
fn overridden(value: Option<&str>) -> Option<u64> {
    value?.trim().parse::<u64>().ok().filter(|m| *m > 0)
}

/// The memory to plan for: the override when it is set, else what a phone's system allows, else
/// physical memory, or a control-group limit (a container's, or a service's such as systemd's
/// `MemoryMax`) when that is lower.
#[cfg(all(not(fuzzing), not(target_arch = "wasm32")))]
fn read() -> (Option<u64>, &'static str) {
    // Settled first, so a phone's app can't say what it allows once this has read without it.
    let phone = PHONE.get_or_init(|| None);
    if let Some(m) = overridden(std::env::var(OVERRIDE).ok().as_deref()) {
        return (Some(m), "override");
    }
    if let Some(phone) = phone {
        return (Some(phone.allowed), "phone");
    }
    let Some(mut s) = sysinfo::System::new().ok() else { return (None, "unknown") };
    s.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
    let Some(total) = Some(s.total_memory()).filter(|m| *m > 0) else { return (None, "unknown") };
    // A container's limit, on Linux; none is reported elsewhere. The limits of this process's own
    // control group and those above it count, not only the root's: a `systemd-run` scope or a
    // service's `MemoryMax` sits below the root, which is all a container's own namespace shows.
    let own = sysinfo::get_current_pid().ok().and_then(|pid| {
        s.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), false, sysinfo::ProcessRefreshKind::nothing());
        s.process(pid)?.cgroup_limits()
    });
    let limits = [own, s.cgroup_limits()].into_iter().flatten().map(|c| c.total_memory);
    match limits.filter(|m| *m > 0 && *m < total).min() {
        Some(limit) => (Some(limit), "container"),
        None => (Some(total), "computer"),
    }
}

/// The scratch memory work's threads may take together beyond one thread's, in the browser and
/// at least on the desktop ([`thread_scratch`]).
pub const SCRATCH_FLOOR: usize = 256 << 20;
/// The most [`thread_scratch`] allows, however much memory the computer has.
pub const SCRATCH_CEILING: usize = 4 << 30;

/// The scratch memory work's threads (a tool's, a GeoTIFF read's) may take together beyond one
/// thread's: on the desktop a sixteenth of the computer's memory, on top of the quarter the input
/// and output limits plan for, from [`SCRATCH_FLOOR`] up to [`SCRATCH_CEILING`] (#1291 step 2). It
/// bounds the threads a high thread setting starts ([`crate::threads`]). A phone ([`on_phone`])
/// takes [`SCRATCH_FLOOR`], as the browser does: this is asked while drawing, too often to ask the
/// system what is free, and a phone's whole figure isn't free to take.
pub fn thread_scratch() -> usize {
    if on_phone() {
        return SCRATCH_FLOOR;
    }
    scaled(physical().map(|m| m / 4), SCRATCH_FLOOR, SCRATCH_CEILING, 1)
}

/// Feature storage kept only by undo and redo snapshots. On the desktop the history can use
/// a quarter of physical memory, from 256 MiB up to 4 GiB. The browser and unknown hosts use
/// the floor. A phone ([`on_phone`]) plans it from what is free too ([`planned`]), when a
/// session starts.
pub fn undo_history() -> usize {
    let (floor, ceiling) = (256 << 20, usize::try_from(4_u64 << 30).unwrap_or(usize::MAX));
    if on_phone() {
        return planned(floor, ceiling, 1);
    }
    scaled(physical(), floor, ceiling, 1)
}

/// The memory RustGIS plans to hold on a computer with `memory` bytes (`None`: not known), what the
/// session's layers, rasters and Undo history may take together before an edit that copies a
/// layer is refused (#2431 fix 3): all of it but [`SHARE`], which is left for drawing, the window
/// and a job running at the same time.
pub fn usable(memory: Option<u64>) -> Option<u64> {
    memory.map(|m| m.saturating_sub(m / SHARE))
}

/// A limit of units costing `bytes` each: as many as [`SHARE`] of `memory` holds, within `floor`
/// and `ceiling`; `floor` when the memory isn't known.
pub fn scaled(memory: Option<u64>, floor: usize, ceiling: usize, bytes: u64) -> usize {
    within(memory.map(|m| m / SHARE), floor, ceiling, bytes)
}

/// A limit of units costing `bytes` each that plans for what is left (#2431 fix 3): as many as
/// [`share`] holds with what the process holds now ([`held`]) and what the computer has free now
/// ([`available`]), within `floor` and `ceiling`; `floor` when the memory isn't known. So a limit
/// asked for again later can be lower, never below its floor.
pub fn planned(floor: usize, ceiling: usize, bytes: u64) -> usize {
    within(share(physical(), held(), available()), floor, ceiling, bytes)
}

/// The memory a job plans for on a computer with `memory` bytes (`None`: not known) while `held`
/// bytes are held and `free` bytes are free (`None`: not known): the smallest of [`SHARE`] of it,
/// what is left of [`usable`] after what is held, and what is free less [`margin`]. With half the
/// memory or less held and enough free, it is the share.
pub fn share(memory: Option<u64>, held: u64, free: Option<u64>) -> Option<u64> {
    let memory = memory?;
    let left = usable(Some(memory)).unwrap_or(0).saturating_sub(held);
    let free = free.map_or(u64::MAX, |free| free.saturating_sub(margin(memory)));
    Some((memory / SHARE).min(left).min(free))
}

/// What is kept free beside a job or an edit's copy on a computer with `memory` bytes, for what
/// RustGIS's estimates don't count (drawing, the window, the allocator's slack): a twentieth of
/// the memory, from 256 MiB up to 2 GiB, as drawing doesn't grow with the memory.
pub fn margin(memory: u64) -> u64 {
    (memory / 20).clamp(256 << 20, 2 << 30)
}

/// The memory the computer has free now, read each time (#2431): what the system can give without
/// swapping, the cache it can drop counted as free, with RustGIS's own use already taken off.
/// It reads files or asks the system, so it is read once a job or an edit, never in a loop.
///
/// `None`:
/// - in the browser, in a fuzzing build, or when it can't be read;
/// - when [`OVERRIDE`] is set, so a test or a reproduction gives the same limits whatever else
///   runs;
/// - in a container whose limit the limits follow: its usage counts the file cache the kernel only
///   drops near the limit, so it would read as nearly full after any large read; the limit itself
///   bounds RustGIS there.
///
/// Memory RustGIS freed that its allocator keeps for reuse still counts as used here. On macOS
/// the system counts pages programs are using as available too, so this rarely bounds anything
/// there. On a phone it is what the app says its system has free ([`set_phone`]).
pub fn available() -> Option<u64> {
    #[cfg(all(not(fuzzing), not(target_arch = "wasm32")))]
    {
        match source() {
            "computer" => {}
            "phone" => return PHONE.get().and_then(Option::as_ref).and_then(|phone| (phone.free)()),
            _ => return None,
        }
        let mut s = sysinfo::System::new().ok()?;
        s.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
        Some(s.available_memory()).filter(|m| *m > 0)
    }
    #[cfg(any(fuzzing, target_arch = "wasm32"))]
    {
        None
    }
}

/// As many units of `bytes` as `share` holds, within `floor` and `ceiling`.
fn within(share: Option<u64>, floor: usize, ceiling: usize, bytes: u64) -> usize {
    let Some(share) = share else { return floor };
    usize::try_from(share / bytes.max(1)).unwrap_or(usize::MAX).clamp(floor, ceiling.max(floor))
}

/// What the process holds (#2431 fix 3): the sum of every [`Holding`]'s bytes, set by each
/// session for its layers, tables, rasters and Undo history, and for copies still being made.
static HELD: AtomicU64 = AtomicU64::new(0);

/// The memory the process holds now: what every live [`Holding`] says.
pub fn held() -> u64 {
    HELD.load(Ordering::Relaxed)
}

/// One holder's part of [`held`]: what it sets is counted until it sets another amount or is
/// dropped.
#[derive(Debug, Default)]
pub struct Holding {
    bytes: u64,
}

impl Holding {
    /// A part of [`held`] counting `bytes` until dropped.
    pub fn of(bytes: u64) -> Self {
        let mut holding = Self::default();
        holding.set(bytes);
        holding
    }

    /// Counts `bytes` in place of what this holding counted before.
    pub fn set(&mut self, bytes: u64) {
        let was = std::mem::replace(&mut self.bytes, bytes);
        if bytes >= was {
            HELD.fetch_add(bytes - was, Ordering::Relaxed);
        } else {
            HELD.fetch_sub(was - bytes, Ordering::Relaxed);
        }
    }

    /// What this holding counts.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for Holding {
    fn drop(&mut self) {
        // Only what `set` added is taken away, so the sum never goes below zero.
        HELD.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn a_limit_follows_the_memory_between_its_floor_and_ceiling() {
        // 8 bytes a unit: 16 GiB plans for 4 GiB, 512 Mi units.
        assert_eq!(scaled(Some(16 * GIB), 100, 1 << 40, 8), 512 << 20);
        assert_eq!(scaled(Some(GIB), 200_000_000, 1 << 40, 8), 200_000_000, "never below the floor");
        assert_eq!(scaled(Some(1024 * GIB), 100, 1_000, 8), 1_000, "never above the ceiling");
        assert_eq!(scaled(None, 100, 1_000, 8), 100, "unknown memory: the floor");
        assert_eq!(scaled(Some(u64::MAX), 100, usize::MAX, 0) as u64, u64::MAX / SHARE, "no overflow or division by zero");
        assert_eq!(scaled(Some(16 * GIB), 100, 50, 8), 100, "a ceiling below the floor is the floor");
    }

    #[test]
    fn a_job_plans_for_what_is_left_of_the_memory_once_half_is_held() {
        assert_eq!(share(Some(16 * GIB), 0, None), Some(4 * GIB), "nothing held: the share");
        assert_eq!(share(Some(16 * GIB), 8 * GIB, None), Some(4 * GIB), "half held: still the share");
        assert_eq!(share(Some(16 * GIB), 10 * GIB, None), Some(2 * GIB), "what is left of three quarters");
        assert_eq!(share(Some(16 * GIB), 20 * GIB, None), Some(0), "more held than planned for: nothing");
        assert_eq!(share(None, 10 * GIB, Some(GIB)), None, "memory not known");
        // The limits keep their floors and ceilings.
        assert_eq!(within(share(Some(16 * GIB), 10 * GIB, None), 100, 1 << 40, 8), 256 << 20);
        assert_eq!(within(share(Some(16 * GIB), 12 * GIB, None), 100, 1 << 40, 8), 100, "never below the floor");
        assert_eq!(within(share(None, 12 * GIB, None), 100, 1 << 40, 8), 100);
        assert_eq!(within(share(Some(1024 * GIB), 0, None), 100, 1_000, 8), 1_000);
    }

    #[test]
    fn a_job_plans_for_no_more_than_the_computer_has_free() {
        // 8 GiB with little held, but other programs use most of it: what is free, less the margin
        // (a twentieth of 8 GiB, above 256 MiB).
        assert_eq!(share(Some(8 * GIB), GIB, Some(GIB)), Some(GIB - 8 * GIB / 20));
        assert_eq!(margin(GIB), 256 << 20, "at least 256 MiB");
        assert_eq!(share(Some(8 * GIB), GIB, Some(6 * GIB)), Some(2 * GIB), "plenty free: the share");
        assert_eq!(share(Some(8 * GIB), GIB, Some(100 << 20)), Some(0), "less free than the margin: nothing");
        assert_eq!(margin(16 * GIB), 16 * GIB / 20, "a twentieth");
        assert_eq!(margin(1024 * GIB), 2 * GIB, "at most 2 GiB");
        assert_eq!(within(share(Some(8 * GIB), 0, Some(0)), 100, 1 << 40, 8), 100, "never below the floor");
    }

    #[test]
    fn holdings_add_up_and_take_their_part_back() {
        // Only sessions (in the engine) hold, so nothing else here changes the sum; small amounts,
        // so the limits other tests read at the same time don't move.
        let base = held();
        let first = Holding::of(3);
        let mut second = Holding::of(5);
        assert_eq!(held(), base + 8);
        second.set(1);
        assert_eq!(held(), base + 4);
        drop(first);
        assert_eq!(held(), base + 1);
        assert_eq!(second.bytes(), 1);
        drop(second);
        assert_eq!(held(), base);
    }

    #[test]
    fn usable_memory_leaves_a_share() {
        assert_eq!(usable(Some(16 * GIB)), Some(12 * GIB));
        assert_eq!(usable(None), None);
        assert_eq!(usable(Some(3)), Some(3), "a share below a byte leaves it all");
    }

    #[test]
    fn the_override_is_a_positive_number_of_bytes() {
        assert_eq!(overridden(Some("8589934592")), Some(8 * GIB));
        assert_eq!(overridden(Some(" 1024 ")), Some(1024));
        for junk in [None, Some(""), Some("0"), Some("-1"), Some("8 GiB"), Some("1e9"), Some("99999999999999999999")] {
            assert_eq!(overridden(junk), None, "{junk:?}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_memory_is_read_once_and_stays() {
        let first = physical();
        assert!(first.is_none_or(|m| m >= 64 << 20), "{first:?}");
        assert_eq!(physical(), first);
        // Once read, a phone's allowance comes too late, and changes nothing.
        assert!(!set_phone(1 << 30, || None));
        assert!(!on_phone());
        assert_eq!(physical(), first);
    }
}
