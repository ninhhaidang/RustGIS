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
/// `None` in the browser, or when it can't be read.
pub fn physical() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static MEMORY: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        *MEMORY.get_or_init(read)
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read() -> Option<u64> {
    let mut s = sysinfo::System::new().ok()?;
    s.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
    let total = Some(s.total_memory()).filter(|m| *m > 0)?;
    // A container's limit, on Linux; none is reported elsewhere.
    Some(s.cgroup_limits().map(|c| c.total_memory).filter(|m| *m > 0).map_or(total, |c| c.min(total)))
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

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_memory_is_read_once_and_stays() {
        let first = physical();
        assert!(first.is_none_or(|m| m >= 64 << 20), "{first:?}");
        assert_eq!(physical(), first);
    }
}
