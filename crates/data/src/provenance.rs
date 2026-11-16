//! File inputs observed while a command runs. Metadata-only capture never rereads file contents.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::{File, Metadata};
use std::marker::PhantomData;
use std::path::Path;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use sha2::Digest;

/// One observed local input. Missing metadata is explicit, as are reads without a cheap hash.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Fingerprint {
    pub path: String,
    pub size: Option<u64>,
    /// Nanoseconds since 1970 UTC, as text to preserve filesystem precision in JSON consumers.
    pub modified_ns: Option<String>,
    pub sha256: Option<String>,
    pub hash_unavailable: Option<String>,
}

thread_local! {
    static INPUTS: RefCell<Option<BTreeSet<Fingerprint>>> = const { RefCell::new(None) };
}

/// Restores the enclosing synchronous capture on every return and unwind. Never hold across await.
#[must_use]
pub struct Capture {
    previous: Option<BTreeSet<Fingerprint>>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl Capture {
    /// Starts an isolated command or worker capture; nested captures also contribute to their parent.
    pub fn start() -> Self {
        Self { previous: INPUTS.with(|v| v.replace(Some(BTreeSet::new()))), _thread_bound: PhantomData }
    }

    /// Copies the observed inputs before the scope ends, for delivery with a worker's result.
    pub fn inputs(&self) -> Vec<Fingerprint> {
        current()
    }
}

impl Drop for Capture {
    /// Restores and merges into the parent even when command execution unwinds.
    fn drop(&mut self) {
        let inputs = INPUTS.with(|v| v.replace(self.previous.take())).unwrap_or_default();
        record(inputs);
    }
}

/// Reads generated files or reloads outputs without adding them to the enclosing command's inputs.
/// Restores capture on every return and unwind; nested ordinary commands may still capture their own inputs.
pub fn without<T>(read: impl FnOnce() -> T) -> T {
    let _restore = Capture { previous: INPUTS.with(|v| v.replace(None)), _thread_bound: PhantomData };
    read()
}

/// Adds worker inputs to the landing command. Repeated identical observations take one place.
pub fn record(inputs: impl IntoIterator<Item = Fingerprint>) {
    INPUTS.with(|v| {
        if let Some(kept) = v.borrow_mut().as_mut() {
            kept.extend(inputs);
        }
    });
}

/// The inputs observed in the current synchronous scope, sorted for stable exports.
pub fn current() -> Vec<Fingerprint> {
    INPUTS.with(|v| {
        let inputs = v.borrow();
        let Some(inputs) = inputs.as_ref() else { return Vec::new() };
        let mut kept: Vec<Fingerprint> = Vec::new();
        for input in inputs {
            if input.sha256.is_some()
                && kept.last().is_some_and(|last| {
                    last.path == input.path && last.size == input.size && last.modified_ns == input.modified_ns && last.sha256.is_none()
                })
            {
                kept.pop();
            }
            kept.push(input.clone());
        }
        kept
    })
}

/// Whether provenance is wanted on this thread; ordinary rendering and reads pay no hash cost.
pub(crate) fn active() -> bool {
    INPUTS.with(|v| v.borrow().is_some())
}

/// Records a path opened by a library or external reader without opening it again.
pub fn path(path: &Path) {
    if active() {
        let metadata = std::fs::metadata(path).ok();
        if metadata.as_ref().is_none_or(Metadata::is_file) {
            observe(path, metadata.as_ref(), None);
        }
    }
}

/// SQLite opens its own handles; present companions may contain uncheckpointed input data.
/// Include their metadata without reading either the database or its journals again.
pub fn sqlite(path: &Path) {
    if !active() {
        return;
    }
    self::path(path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut companion = path.as_os_str().to_os_string();
        companion.push(suffix);
        let companion = Path::new(&companion);
        if let Ok(metadata) = std::fs::metadata(companion)
            && metadata.is_file()
        {
            observe(companion, Some(&metadata), None);
        }
    }
}

/// Keeps the metadata from the opened handle and hashes only already resident small buffers.
/// Larger and random-access reads deliberately use metadata alone, with the reason in the log.
pub fn bytes(path: &Path, metadata: Option<&Metadata>, bytes: &[u8]) {
    if active() {
        observe(path, metadata, Some(bytes));
    }
}

/// A bounded reader's complete file buffer; never use this for a prefix or decoded content.
pub fn buffer(path: &Path, file: &File, bytes: &[u8]) {
    if active() {
        observe(path, file.metadata().ok().as_ref(), Some(bytes));
    }
}

/// Captures metadata without failing a successful read when the filesystem cannot supply it.
fn observe(path: &Path, metadata: Option<&Metadata>, bytes: Option<&[u8]>) {
    #[cfg(not(target_arch = "wasm32"))]
    let sha256 = bytes.filter(|b| b.len() <= 8 << 20).map(|b| sha2::Sha256::digest(b).iter().map(|b| format!("{b:02x}")).collect());
    #[cfg(target_arch = "wasm32")]
    let sha256: Option<String> = None;
    let hash_unavailable = sha256.is_none().then(|| {
        if bytes.is_some_and(|b| b.len() > 8 << 20) {
            "buffer larger than 8 MiB; metadata only"
        } else if bytes.is_some() {
            "content hash unavailable on this platform; metadata only"
        } else {
            "streamed or random-access read; metadata only"
        }
        .to_string()
    });
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).display().to_string();
    record([Fingerprint {
        path,
        size: metadata.map(Metadata::len).or_else(|| bytes.map(|bytes| bytes.len() as u64)),
        modified_ns: modified_ns(metadata),
        sha256,
        hash_unavailable,
    }]);
}

/// Preserves filesystem precision, including timestamps before 1970; no wall clock is read.
fn modified_ns(metadata: Option<&Metadata>) -> Option<String> {
    let time = metadata?.modified().ok()?;
    Some(match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos().to_string(),
        Err(error) => format!("-{}", error.duration().as_nanos()),
    })
}

/// Reuses a digest an existing reader already computed; this adds no hashing or file read.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn hashed(path: &Path, metadata: Option<&Metadata>, sha256: String) {
    if active() {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).display().to_string();
        record([Fingerprint {
            path,
            size: metadata.map(Metadata::len),
            modified_ns: modified_ns(metadata),
            sha256: Some(sha256),
            hash_unavailable: None,
        }]);
    }
}

/// Opens a streamed or random-access input, retaining the original `File` API and I/O behavior.
pub fn open(path: impl AsRef<Path>) -> std::io::Result<File> {
    let path = path.as_ref();
    let file = File::open(path)?;
    if active() {
        let metadata = file.metadata().ok();
        if metadata.as_ref().is_none_or(Metadata::is_file) {
            observe(path, metadata.as_ref(), None);
        }
    }
    Ok(file)
}

/// Reads once; a cheap digest uses those bytes, never a second file read.
pub fn read(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let path = path.as_ref();
    if !active() {
        return std::fs::read(path);
    }
    let mut file = File::open(path)?;
    let metadata = file.metadata().ok();
    let mut out = Vec::new();
    if let Err(error) = file.read_to_end(&mut out) {
        if metadata.as_ref().is_none_or(Metadata::is_file) {
            observe(path, metadata.as_ref(), None);
        }
        return Err(error);
    }
    bytes(path, metadata.as_ref(), &out);
    Ok(out)
}

/// Reads UTF-8 text with the same input capture as a byte read.
pub fn read_to_string(path: impl AsRef<Path>) -> std::io::Result<String> {
    use std::io::Read;
    let path = path.as_ref();
    if !active() {
        return std::fs::read_to_string(path);
    }
    let mut file = File::open(path)?;
    let metadata = file.metadata().ok();
    let mut text = String::new();
    if let Err(error) = file.read_to_string(&mut text) {
        if metadata.as_ref().is_none_or(Metadata::is_file) {
            observe(path, metadata.as_ref(), None);
        }
        return Err(error);
    }
    bytes(path, metadata.as_ref(), text.as_bytes());
    Ok(text)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// An atomic pathname replacement cannot pair replacement metadata with the original bytes.
    #[cfg(unix)]
    #[test]
    fn buffered_provenance_uses_the_opened_file_and_suppression_restores_capture() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.cpg");
        std::fs::write(&path, b"UTF-8").unwrap();
        let scope = Capture::start();
        let mut file = open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"a different file").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        buffer(&path, &file, &contents);
        let input = scope.inputs().pop().unwrap();
        assert_eq!(input.size, Some(5));
        assert_eq!(input.modified_ns, modified_ns(Some(&metadata)));
        assert!(input.sha256.is_some());
        assert_eq!(scope.inputs().len(), 1);
        let output = dir.path().join("output");
        std::fs::write(&output, b"generated").unwrap();
        without(|| {
            read(&output).unwrap();
        });
        assert_eq!(scope.inputs(), vec![input.clone()]);
        assert!(std::panic::catch_unwind(|| without(|| panic!("reader failed"))).is_err());
        assert_eq!(scope.inputs(), vec![input]);
        read(&output).unwrap();
        assert_eq!(scope.inputs().len(), 2);
    }

    /// Sparse input opens must remain metadata-only, and one scope's reads must not leak to another.
    #[test]
    fn large_random_access_inputs_are_not_read_again_for_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.tif");
        let file = File::create(&path).unwrap();
        file.set_len(9 << 20).unwrap();
        drop(file);
        let scope = Capture::start();
        drop(open(&path).unwrap());
        let input = scope.inputs().pop().unwrap();
        assert_eq!(input.size, Some(9 << 20));
        assert!(input.modified_ns.is_some());
        assert_eq!(input.sha256, None);
        assert_eq!(input.hash_unavailable.as_deref(), Some("streamed or random-access read; metadata only"));
        // Nested captures restore and contribute their reads to the enclosing command.
        {
            let nested = Capture::start();
            assert!(nested.inputs().is_empty());
            drop(open(&path).unwrap());
        }
        assert_eq!(scope.inputs(), vec![input]);
        drop(scope);
        assert!(current().is_empty());
        let other = Capture::start();
        assert!(other.inputs().is_empty());
    }
}
