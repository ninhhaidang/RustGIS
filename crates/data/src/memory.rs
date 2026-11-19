//! The computer's memory, and the limits that follow it (#1291 step 2, #1318).
//!
//! On the desktop a memory limit plans for a share of the physical memory ([`SHARE`]): never below
//! the fixed value it had before (its floor, still the browser's limit), never above a ceiling. A
//! container's memory limit counts, when it is lower. The memory is read once, at the first limit
//! asked for, so a session keeps the same limits; [`scaled`] takes it as an argument, so tests pass
//! fixed amounts.

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
pub fn physical() -> Option<u64> {
    reading().0
}

/// Where [`physical`]'s figure came from: `"override"` ([`OVERRIDE`]), `"container"` (a
/// container's limit, lower than the computer's memory), `"computer"`, or `"unknown"` (the browser,
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

/// The environment variable that replaces the memory [`physical`] reads.
pub const OVERRIDE: &str = "RUSTGIS_MEMORY_BYTES";

/// The memory [`OVERRIDE`]'s value gives: a positive whole number of bytes, else `None`.
#[cfg(any(test, all(not(fuzzing), not(target_arch = "wasm32"))))]
fn overridden(value: Option<&str>) -> Option<u64> {
    value?.trim().parse::<u64>().ok().filter(|m| *m > 0)
}

/// The memory to plan for: the override when it is set, else physical memory, or a container's
/// limit when that is lower.
#[cfg(all(not(fuzzing), not(target_arch = "wasm32")))]
fn read() -> (Option<u64>, &'static str) {
    if let Some(m) = overridden(std::env::var(OVERRIDE).ok().as_deref()) {
        return (Some(m), "override");
    }
    let Some(mut s) = sysinfo::System::new().ok() else { return (None, "unknown") };
    s.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
    let Some(total) = Some(s.total_memory()).filter(|m| *m > 0) else { return (None, "unknown") };
    // A container's limit, on Linux; none is reported elsewhere.
    match s.cgroup_limits().map(|c| c.total_memory).filter(|m| *m > 0 && *m < total) {
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
/// bounds the threads a high thread setting starts ([`crate::threads`]).
pub fn thread_scratch() -> usize {
    scaled(physical().map(|m| m / 4), SCRATCH_FLOOR, SCRATCH_CEILING, 1)
}

/// Feature storage kept only by undo and redo snapshots. On the desktop the history can use
/// a quarter of physical memory, from 256 MiB up to 4 GiB. The browser and unknown hosts use
/// the floor.
pub fn undo_history() -> usize {
    scaled(physical(), 256 << 20, usize::try_from(4_u64 << 30).unwrap_or(usize::MAX), 1)
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
    let Some(memory) = memory else { return floor };
    let n = memory / SHARE / bytes.max(1);
    usize::try_from(n).unwrap_or(usize::MAX).clamp(floor, ceiling.max(floor))
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
    }
}
