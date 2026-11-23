//! Retains repeated label-width results without changing the font measurement itself.

use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

/// Each shard releases its table and strings when their estimated retained size exceeds this.
/// Allocator overhead, measurement workspaces and simultaneous misses are outside this estimate.
const SHARD_BYTES: usize = 128 * 1024;
/// Longer inputs use the caller's unchanged measurement path and are never copied into the cache.
const MAX_TEXT_BYTES: usize = 4096;

/// The full key accompanies its hash so a collision can only cause a miss, never a wrong width.
/// `S` is the face measured in: whether it's bold, or a label's whole face.
struct Entry<S> {
    text: String,
    size: u32,
    bold: S,
    width: f32,
}

/// One shard's entries and owned string capacities; an eviction drops both allocations.
struct Widths<S> {
    entries: HashMap<u64, Entry<S>>,
    strings: usize,
}

impl<S> Default for Widths<S> {
    fn default() -> Self {
        Widths { entries: HashMap::new(), strings: 0 }
    }
}

impl<S: Copy + PartialEq> Widths<S> {
    /// Checks the complete measurement key, keeping signed zero and distinct finite sizes apart.
    fn get(&self, hash: u64, text: &str, size: u32, bold: S) -> Option<f32> {
        self.entries.get(&hash).filter(|e| e.text == text && e.size == size && e.bold == bold).map(|e| e.width)
    }

    /// Estimates retained strings and table capacity with slack for buckets, using saturation.
    /// This is a retention policy, not a strict allocator or process-memory bound.
    fn retained_bytes(&self) -> usize {
        self.strings.saturating_add(self.entries.capacity().saturating_mul(2 * std::mem::size_of::<(u64, Entry<S>)>()))
    }

    /// Replaces a colliding entry and accounts for its old string before checking the budget.
    /// Over-budget shards release their capacity rather than retaining an empty large table.
    fn insert(&mut self, hash: u64, text: &str, size: u32, bold: S, width: f32) {
        let text = text.to_owned();
        self.strings = self.strings.saturating_add(text.capacity());
        if let Some(old) = self.entries.insert(hash, Entry { text, size, bold, width }) {
            self.strings = self.strings.saturating_sub(old.text.capacity());
        }
        if self.retained_bytes() > SHARD_BYTES {
            *self = Self::default();
        }
    }
}

/// Sixteen independent shards retain at most 16 × the per-shard estimate after insertion.
/// Misses measure without a lock, so concurrent misses may repeat work but cannot mix results.
pub(super) struct Cache<S = bool> {
    shards: [Mutex<Widths<S>>; 16],
}

impl<S> Default for Cache<S> {
    fn default() -> Self {
        Cache { shards: std::array::from_fn(|_| Mutex::default()) }
    }
}

impl<S: Copy + PartialEq + Hash> Cache<S> {
    /// Returns a matching width or calls `uncached` once, preserving its exact floating-point result.
    /// Oversized text and non-finite sizes bypass hashing, locking and retention. Poisoned locks
    /// are recovered: entries are only installed after the caller has measured successfully.
    pub(super) fn measure(&self, text: &str, size: f32, bold: S, uncached: impl FnOnce() -> f32) -> f32 {
        if text.len() > MAX_TEXT_BYTES || !size.is_finite() {
            return uncached();
        }
        let bits = size.to_bits();
        let mut hasher = DefaultHasher::new();
        (text, bits, bold).hash(&mut hasher);
        let hash = hasher.finish();
        let Some(shard) = self.shards.get(hash as usize % self.shards.len()) else { return uncached() };
        if let Some(width) = shard.lock().unwrap_or_else(|e| e.into_inner()).get(hash, text, bits, bold) {
            return width;
        }
        let width = uncached();
        shard.lock().unwrap_or_else(|e| e.into_inner()).insert(hash, text, bits, bold, width);
        width
    }
}

#[expect(clippy::disallowed_methods, reason = "a thread the system refuses fails the test, as it should")]
#[cfg(test)]
mod tests;
