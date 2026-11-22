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
    /// Set when the inputs behind `path` can't all be listed (a folder too large to walk, a
    /// header that couldn't be read for the files it names): the reason, so a reader of the log
    /// knows the list is incomplete there. Absent in older exported logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<String>,
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
/// Restores capture on every return and unwind. Nested ordinary commands run inside it start no
/// capture of their own (only an outermost command does), so their reads are dropped too.
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
                    last.path == input.path
                        && last.size == input.size
                        && last.modified_ns == input.modified_ns
                        && last.sha256.is_none()
                        && last.incomplete.is_none()
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

/// Records a path opened by a library or external reader without opening it again. A folder
/// (a File Geodatabase read through GDAL) records the files inside it, up to
/// [`MAX_FOLDER_FILES`] at most [`MAX_FOLDER_DEPTH`] folders down, and an
/// [`incomplete`](Fingerprint::incomplete) mark when it can't list them all.
pub fn path(path: &Path) {
    if active() {
        let metadata = std::fs::metadata(path).ok();
        if metadata.as_ref().is_some_and(Metadata::is_dir) {
            folder(path);
        } else if metadata.as_ref().is_none_or(Metadata::is_file) {
            observe(path, metadata.as_ref(), None);
        }
    }
}

/// Most files a folder input lists before it is marked incomplete.
pub const MAX_FOLDER_FILES: usize = 1000;
/// Most folders down a folder input is walked before it is marked incomplete.
pub const MAX_FOLDER_DEPTH: usize = 8;

/// Most entries (files and folders) a folder input looks at in all, so a tree of countless
/// empty folders costs a bounded walk too, before it is marked incomplete.
pub const MAX_FOLDER_ENTRIES: usize = 10_000;

/// Records the regular files inside `root` by metadata alone, in name order, bounded by
/// [`MAX_FOLDER_FILES`], [`MAX_FOLDER_DEPTH`] and [`MAX_FOLDER_ENTRIES`]; a folder inside is read
/// up to [`MAX_FOLDER_FILES`] + 1 entries. Linked folders aren't followed (they could loop).
/// Whatever can't be listed leaves one mark on `root` giving each reason once. The walk is
/// bounded rather than cancellable: at most [`MAX_FOLDER_ENTRIES`] entries looked at.
fn folder(root: &Path) {
    let mut gaps: Vec<String> = Vec::new();
    let mut gap = |why: String| {
        if !gaps.contains(&why) {
            gaps.push(why);
        }
    };
    let (mut files, mut looked) = (0usize, 0usize);
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    'walk: while let Some((dir, depth)) = stack.pop() {
        let mut entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                gap(format!("a folder inside couldn't be listed ({e}); its files aren't named"));
                continue;
            }
        };
        // One more than may be kept, so a folder of millions isn't listed whole to be cut.
        let mut paths = Vec::new();
        for entry in entries.by_ref().take(MAX_FOLDER_FILES.saturating_add(1)) {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(e) => gap(format!("a folder inside couldn't be listed in full ({e})")),
            }
        }
        if entries.next().is_some() {
            gap(format!("a folder inside holds more than {} entries; the rest aren't named", MAX_FOLDER_FILES + 1));
        }
        paths.sort();
        let mut folders = Vec::new();
        for p in paths {
            looked += 1;
            if looked > MAX_FOLDER_ENTRIES {
                gap(format!("more than {MAX_FOLDER_ENTRIES} files and folders; the rest aren't named"));
                break 'walk;
            }
            let linked = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
            let metadata = match std::fs::metadata(&p) {
                Ok(metadata) => metadata,
                Err(e) => {
                    gap(format!("an entry inside couldn't be looked at ({e}); it isn't named"));
                    continue;
                }
            };
            if metadata.is_dir() {
                if linked {
                    gap("a linked folder inside isn't followed; its files aren't named".into());
                } else if depth >= MAX_FOLDER_DEPTH {
                    gap(format!("folders more than {MAX_FOLDER_DEPTH} deep aren't listed"));
                } else {
                    folders.push((p, depth + 1));
                }
            } else if metadata.is_file() {
                if files >= MAX_FOLDER_FILES {
                    gap(format!("more than {MAX_FOLDER_FILES} files; only the first {MAX_FOLDER_FILES} are named"));
                    break 'walk;
                }
                files += 1;
                observe(&p, Some(&metadata), None);
            }
        }
        // Popped in name order.
        stack.extend(folders.into_iter().rev());
    }
    if !gaps.is_empty() {
        incomplete(root, &gaps.join("; "));
    }
}

/// Marks the inputs behind `path` as not all listed, saying why ([`Fingerprint::incomplete`]).
pub fn incomplete(path: &Path, why: &str) {
    if active() {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).display().to_string();
        record([Fingerprint {
            path,
            size: None,
            modified_ns: None,
            sha256: None,
            hash_unavailable: None,
            incomplete: Some(why.to_string()),
        }]);
    }
}

/// A DuckDB database DuckDB opens itself: the file, and its write-ahead log (`<file>.wal`) when
/// one is present, which holds changes not yet checkpointed into it.
pub fn duckdb(path: &Path) {
    if !active() {
        return;
    }
    self::path(path);
    let mut wal = path.as_os_str().to_os_string();
    wal.push(".wal");
    let wal = std::path::PathBuf::from(wal);
    if let Ok(metadata) = std::fs::metadata(&wal)
        && metadata.is_file()
    {
        observe(&wal, Some(&metadata), None);
    }
}

/// What `observe` records, captured on its own and not added to the enclosing command's inputs:
/// a converted file's original as it was when converted, recorded with each later read of the
/// conversion ([`record`]) in place of the original as it is by then. Captured whether or not a
/// command is capturing, as the conversion may be opened while none is. Gives what `observe`
/// returned too.
pub fn snapshot<T>(observe: impl FnOnce() -> T) -> (T, Vec<Fingerprint>) {
    let previous = INPUTS.with(|v| v.replace(Some(BTreeSet::new())));
    // Restored even if `observe` unwinds.
    struct Restore(Option<Option<BTreeSet<Fingerprint>>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                INPUTS.with(|v| v.replace(previous));
            }
        }
    }
    let restore = Restore(Some(previous));
    let out = observe();
    let seen = current();
    drop(restore);
    (out, seen)
}

/// SQLite opens its own handles; present companions may contain uncheckpointed input data.
/// Include their metadata without reading either the database or its journals again.
pub fn sqlite(path: &Path) {
    if !active() {
        return;
    }
    self::path(path);
    // SQLite names its companions after the database with symbolic links resolved (on unix). A
    // linked folder gives them beside the path as opened too, which is the name the user knows; a
    // link to the file itself gives them only beside its target.
    #[cfg(unix)]
    let resolved = std::fs::canonicalize(path).ok();
    #[cfg(not(unix))]
    let resolved: Option<std::path::PathBuf> = None;
    for suffix in ["-wal", "-shm", "-journal"] {
        let named = |base: &Path| {
            let mut companion = base.as_os_str().to_os_string();
            companion.push(suffix);
            std::path::PathBuf::from(companion)
        };
        let found = std::iter::once(named(path))
            .chain(resolved.as_deref().map(named))
            .find_map(|c| std::fs::metadata(&c).ok().filter(Metadata::is_file).map(|m| (c, m)));
        if let Some((companion, metadata)) = found {
            observe(&companion, Some(&metadata), None);
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
        incomplete: None,
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
            incomplete: None,
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

    /// A folder another program opens lists the files inside it, in name order and without a
    /// mark; one past the limits keeps what it lists and says why the rest aren't (#2639).
    #[test]
    fn a_folder_input_lists_its_files_and_marks_what_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let gdb = dir.path().join("roads.gdb");
        std::fs::create_dir_all(gdb.join("inner")).unwrap();
        std::fs::write(gdb.join("a00000001.gdbtable"), b"table").unwrap();
        std::fs::write(gdb.join("a00000001.gdbtablx"), b"index").unwrap();
        std::fs::write(gdb.join("inner").join("deep.dat"), b"deep").unwrap();
        let scope = Capture::start();
        path(&gdb);
        let inputs = scope.inputs();
        let names: Vec<&str> = inputs.iter().map(|i| i.path.rsplit(['/', '\\']).next().unwrap()).collect();
        assert_eq!(names, ["a00000001.gdbtable", "a00000001.gdbtablx", "deep.dat"]);
        assert!(inputs.iter().all(|i| i.incomplete.is_none() && i.size.is_some()), "{inputs:?}");
        drop(scope);
        // Past the file limit: the first files, and one mark on the folder.
        let big = dir.path().join("big.gdb");
        std::fs::create_dir(&big).unwrap();
        for i in 0..=MAX_FOLDER_FILES {
            std::fs::write(big.join(format!("{i:05}.gdbtable")), b"t").unwrap();
        }
        let scope = Capture::start();
        path(&big);
        let inputs = scope.inputs();
        let marks: Vec<&Fingerprint> = inputs.iter().filter(|i| i.incomplete.is_some()).collect();
        assert_eq!(inputs.len() - marks.len(), MAX_FOLDER_FILES);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].path, std::path::absolute(&big).unwrap().display().to_string());
        assert!(marks[0].incomplete.as_deref().is_some_and(|w| w.contains("more than 1000 files")), "{marks:?}");
        assert!(marks[0].size.is_none() && marks[0].sha256.is_none());
        // A mark is written out only when there is one.
        let json = serde_json::to_value(marks[0]).unwrap();
        assert!(json.get("incomplete").is_some());
        assert!(serde_json::to_value(inputs.last().unwrap()).unwrap().get("incomplete").is_none());
        drop(scope);
        // A linked folder inside isn't followed (it could loop back), and says so, its reason
        // kept beside a later one.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&gdb, gdb.join("loop")).unwrap();
            let scope = Capture::start();
            path(&gdb);
            let inputs = scope.inputs();
            assert_eq!(inputs.iter().filter(|i| i.incomplete.is_none()).count(), 3);
            assert!(inputs.iter().any(|i| i.incomplete.as_deref().is_some_and(|w| w.contains("linked folder"))), "{inputs:?}");
            drop(scope);
            let mixed = dir.path().join("mixed.gdb");
            let deep = (0..=MAX_FOLDER_DEPTH).fold(mixed.clone(), |p, i| p.join(format!("d{i}")));
            std::fs::create_dir_all(&deep).unwrap();
            std::fs::write(deep.join("far.dat"), b"far").unwrap();
            std::os::unix::fs::symlink(&gdb, mixed.join("loop")).unwrap();
            let scope = Capture::start();
            path(&mixed);
            let mark = scope.inputs().into_iter().find_map(|i| i.incomplete).unwrap();
            assert!(mark.contains("linked folder") && mark.contains("8 deep"), "{mark}");
        }
        // Exactly as many entries as are read: no mark. One more, a folder holding a file, past
        // them: marked, though no file limit was reached.
        let full = dir.path().join("full.gdb");
        std::fs::create_dir(&full).unwrap();
        for i in 0..MAX_FOLDER_FILES {
            std::fs::create_dir(full.join(format!("{i:05}"))).unwrap();
        }
        let scope = Capture::start();
        path(&full);
        assert!(scope.inputs().is_empty(), "{:?}", scope.inputs());
        drop(scope);
        for i in MAX_FOLDER_FILES..MAX_FOLDER_FILES + 2 {
            std::fs::create_dir(full.join(format!("{i:05}"))).unwrap();
        }
        std::fs::write(full.join("zz.dat"), b"z").unwrap();
        let scope = Capture::start();
        path(&full);
        let inputs = scope.inputs();
        assert!(inputs.iter().any(|i| i.incomplete.as_deref().is_some_and(|w| w.contains("more than 1001 entries"))), "{inputs:?}");
        drop(scope);
        // Countless empty folders: the walk stops at its entry limit, and says so.
        let wide = dir.path().join("wide.gdb");
        for i in 0..=MAX_FOLDER_ENTRIES / 1000 {
            for j in 0..1000 {
                std::fs::create_dir_all(wide.join(format!("{i:03}")).join(format!("{j:04}"))).unwrap();
            }
        }
        let scope = Capture::start();
        path(&wide);
        let inputs = scope.inputs();
        assert!(
            inputs.iter().any(|i| i.incomplete.as_deref().is_some_and(|w| w.contains("more than 10000 files and folders"))),
            "{inputs:?}"
        );
    }

    /// A DuckDB database's write-ahead log is an input when it is there, and only then.
    #[test]
    fn a_duckdb_database_names_its_write_ahead_log() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("places.duckdb");
        std::fs::write(&db, b"db").unwrap();
        let scope = Capture::start();
        duckdb(&db);
        assert_eq!(scope.inputs().len(), 1);
        drop(scope);
        std::fs::write(dir.path().join("places.duckdb.wal"), b"changes").unwrap();
        let scope = Capture::start();
        duckdb(&db);
        let inputs = scope.inputs();
        assert_eq!(inputs.len(), 2);
        assert!(inputs.iter().any(|i| i.path.ends_with("places.duckdb.wal") && i.size == Some(7)), "{inputs:?}");
        assert!(inputs.iter().all(|i| i.incomplete.is_none()));
    }

    /// A snapshot captures what it observes whether or not a command is capturing, and adds
    /// nothing to the enclosing capture by itself.
    #[test]
    fn a_snapshot_is_taken_apart_from_the_enclosing_capture() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("source.csv");
        std::fs::write(&file, b"a,b").unwrap();
        let ((), seen) = snapshot(|| path(&file));
        assert_eq!(seen.len(), 1);
        assert!(!active());
        let scope = Capture::start();
        let (answer, again) = snapshot(|| {
            path(&file);
            42
        });
        assert_eq!((answer, again), (42, seen.clone()));
        assert!(scope.inputs().is_empty());
        record(seen.clone());
        assert_eq!(scope.inputs(), seen);
    }
}
