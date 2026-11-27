//! Installed font families by name, so a label can be drawn in the family a project names (as
//! QGIS's text formats name theirs). The system's font folders are indexed once, from each
//! file's `name` and `OS/2` tables, read piecemeal so indexing never loads a whole file; a face
//! is read when it is first drawn, and kept. Nothing is installed or bundled: a family a machine
//! lacks is drawn in the label font. In the browser, and with `RUSTGIS_BUILTIN_FONTS` set (so
//! tests draw alike on every machine), there are none.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// One face of an installed family: its file, its index in a collection, its weight (100–900)
/// and whether it is an italic (or oblique) face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledFace {
    pub path: PathBuf,
    pub index: u32,
    pub weight: u16,
    pub italic: bool,
}

/// An installed family: its name, and its faces. A face is listed under its typographic family
/// ("Noto Sans") and under its legacy one ("Noto Sans Light") when they differ, so either name
/// finds it; only the first kind is `listed` in a font picker. `serif` says whether its first
/// face is classed as a serif (see [`FaceInfo`]), for the generic family it falls back to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    pub listed: bool,
    pub faces: Vec<InstalledFace>,
    pub serif: bool,
}

/// The longest family name kept (characters): longer ones are from broken files.
pub const MAX_NAME: usize = 128;
/// Most font files indexed, folders walked deep and faces in one collection.
const MAX_FILES: usize = 50_000;
const MAX_DEPTH: usize = 8;
/// Most folders walked.
const MAX_DIRS: usize = 20_000;
const MAX_COLLECTION: u32 = 64;
/// Most tables a face's directory lists, and the most of its `name` table read.
const MAX_TABLES: u16 = 512;
const MAX_NAME_TABLE: u32 = 1 << 20;
/// Largest font file read, and the most face bytes kept in all: a few families' worth, or two or
/// three CJK collections (30–48 MB each). Kept bytes are never freed.
const MAX_FONT: u64 = 48 << 20;
const MAX_LOADED: usize = 128 << 20;

/// The weight bold text is drawn at in a variable face whose default is lighter (a variable
/// Noto Sans is one file for every weight); a static face ignores it.
pub const BOLD_WEIGHT: f32 = 700.0;

/// Whether bold text in `face` is drawn heavier than the face is, at [`BOLD_WEIGHT`]: the face
/// standing in for the bold one is lighter than semibold (a variable face, or a family with no
/// bold). The map view and the export must agree on this, so both ask here.
pub fn heavier(face: &InstalledFace, bold: bool) -> bool {
    bold && face.weight < 600
}

static FAMILIES: OnceLock<Vec<Family>> = OnceLock::new();
/// Whether [`warm_up`] has started indexing them.
static WARMING: AtomicBool = AtomicBool::new(false);

/// The installed families, indexed once, by name (in any case). The first call walks the font
/// folders; the UI starts that early with [`warm_up`] and reads [`installed_now`].
pub fn installed() -> &'static [Family] {
    FAMILIES.get_or_init(|| if std::env::var_os("RUSTGIS_BUILTIN_FONTS").is_some() { Vec::new() } else { index(&font_dirs()) })
}

/// The installed families if they are indexed, without waiting: `None` while [`warm_up`]'s
/// indexing runs (a machine can have tens of thousands of font files).
pub fn installed_now() -> Option<&'static [Family]> {
    FAMILIES.get().map(Vec::as_slice)
}

/// Indexes the installed families on a thread of their own (once), so the UI never waits for
/// the folder walk; in the browser, where there are none, at once.
pub fn warm_up() {
    if WARMING.swap(true, Ordering::Relaxed) {
        return;
    }
    crate::fonts::enable_async_cjk_scan();
    #[cfg(not(target_arch = "wasm32"))]
    if std::thread::Builder::new()
        .name("font-index".into())
        .spawn(|| {
            installed();
        })
        .is_ok()
    {
        return;
    }
    installed();
}

/// The installed family named `name` (in any case, spaces trimmed): its place in [`installed`].
pub fn find(name: &str) -> Option<u16> {
    find_in(installed(), name)
}

/// [`find`] for drawing. Where [`warm_up`] is indexing the families (the app, which draws the
/// map on its UI thread), `None` until the index is done rather than waiting for it, so a label
/// draws in the label font for a moment; elsewhere (a script, the command line, which never warm
/// up) the families are indexed now, so an export draws in the named family.
pub fn find_ready(name: &str) -> Option<u16> {
    find_in(available()?, name)
}

/// The installed families without waiting for [`warm_up`]'s index: `None` while it runs (the
/// app); where nothing warmed up (a script, the command line), indexed now.
pub fn available() -> Option<&'static [Family]> {
    if WARMING.load(Ordering::Relaxed) { installed_now() } else { Some(installed()) }
}

fn find_in(families: &[Family], name: &str) -> Option<u16> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let i = families.binary_search_by(|f| cmp_names(&f.name, name)).ok()?;
    u16::try_from(i).ok()
}

/// Names compared as the index sorts them: in any case.
fn cmp_names(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

/// The face of `family` that draws `bold` and `italic` text, and whether it has to be slanted by
/// hand (an italic asked of a family with no italic face): the nearest weight to 400, or 700 for
/// bold, among the faces of the right slope, else among the upright ones.
pub fn pick(family: &Family, bold: bool, italic: bool) -> Option<(&InstalledFace, bool)> {
    let target: i32 = if bold { 700 } else { 400 };
    let best = |slope: bool| {
        family.faces.iter().filter(|f| f.italic == slope).min_by_key(|f| {
            let w = i32::from(f.weight);
            // At a tie, a regular takes the lighter face and a bold the heavier, as CSS does.
            ((w - target).abs(), if bold { w < target } else { w > target })
        })
    };
    match best(italic) {
        Some(f) => Some((f, false)),
        None => best(!italic).map(|f| (f, italic && !f.italic)),
    }
}

/// The bytes of `face`'s file, read once and kept for the rest of the run (faces are drawn from
/// borrowed bytes); `None` when it can't be read, is too large, or `face` isn't a font in it. A
/// collection's file is kept by its path and each face checked on its own, so one damaged face
/// doesn't hide its siblings.
pub fn bytes(face: &InstalledFace) -> Option<&'static [u8]> {
    static LOADED: OnceLock<Mutex<(HashMap<PathBuf, Option<&'static [u8]>>, usize)>> = OnceLock::new();
    let parses = |b: &[u8], index: u32| ab_glyph::FontRef::try_from_slice_and_index(b, index).is_ok();
    let lock = LOADED.get_or_init(|| Mutex::new((HashMap::new(), 0)));
    let room = {
        let Ok(loaded) = lock.lock() else { return None };
        if let Some(b) = loaded.0.get(&face.path) {
            return b.filter(|b| parses(b, face.index));
        }
        MAX_LOADED.saturating_sub(loaded.1)
    };
    // Read with the lock let go: a large file takes a while, and other threads' faces are kept.
    // Kept when this face or the file's first one is a font (a collection's other faces may be).
    let read = std::fs::metadata(&face.path)
        .ok()
        .filter(|m| m.len() <= MAX_FONT && m.len() as usize <= room)
        .and_then(|_| std::fs::read(&face.path).ok())
        .filter(|b| parses(b, face.index) || parses(b, 0));
    let Ok(mut loaded) = lock.lock() else { return None };
    // Another thread read it meanwhile: its copy is the one kept.
    if let Some(b) = loaded.0.get(&face.path) {
        return b.filter(|b| parses(b, face.index));
    }
    let kept: Option<&'static [u8]> = read.filter(|b| loaded.1 + b.len() <= MAX_LOADED).map(|b| {
        loaded.1 += b.len();
        &*Box::leak(b.into_boxed_slice())
    });
    loaded.0.insert(face.path.clone(), kept);
    kept.filter(|b| parses(b, face.index))
}

/// The folders fonts are installed in on this system, the user's own included.
fn font_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        let windir = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        dirs.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
        dirs.extend(home.iter().map(|h| h.join("Library").join("Fonts")));
    } else if cfg!(target_os = "ios") {
        // iOS keeps its fonts in subfolders (Core, CoreAddition, ...), which the walk reaches. An
        // app's home is its sandbox container, which holds no fonts, so no home folder is walked.
        dirs.push(PathBuf::from("/System/Library/Fonts"));
    } else if cfg!(target_os = "android") {
        // Android's app home has no system fonts; manufacturer faces can live on /product.
        dirs.extend(crate::fonts::ANDROID_FONT_DIRS.iter().map(PathBuf::from));
    } else if cfg!(target_os = "linux") {
        dirs.extend(crate::fonts::linux_font_roots_with(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            home.as_deref().map(Path::as_os_str),
        ));
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        match std::env::var_os("XDG_DATA_HOME").filter(|v| crate::fonts::is_posix_absolute(v)) {
            Some(d) => dirs.push(PathBuf::from(d).join("fonts")),
            None => dirs.extend(home.iter().map(|h| h.join(".local").join("share").join("fonts"))),
        }
        dirs.extend(home.iter().map(|h| h.join(".fonts")));
    }
    dirs
}

/// The families of the font files under `dirs`, sorted by name.
fn index(dirs: &[PathBuf]) -> Vec<Family> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for d in dirs {
        walk(d, 0, &mut files, &mut seen);
    }
    // In name order, so the same file wins a duplicate face on every machine.
    files.sort();
    files.dedup();
    let mut by_name: HashMap<String, Family> = HashMap::new();
    for path in files {
        for (index, face) in read_faces(&path).into_iter().enumerate() {
            let Some(face) = face else { continue };
            let Ok(index) = u32::try_from(index) else { continue };
            let installed = InstalledFace { path: path.clone(), index, weight: face.weight, italic: face.italic };
            for (n, name) in face.names.iter().enumerate() {
                let fam = by_name.entry(name.to_lowercase()).or_insert_with(|| Family {
                    name: name.clone(),
                    listed: false,
                    faces: Vec::new(),
                    serif: face.serif,
                });
                fam.listed |= n == 0;
                // One face per weight and slope: the first file's.
                if !fam.faces.iter().any(|f| f.weight == installed.weight && f.italic == installed.italic) {
                    fam.faces.push(installed.clone());
                }
            }
        }
    }
    let mut families: Vec<Family> = by_name.into_values().collect();
    families.sort_by(|a, b| cmp_names(&a.name, &b.name));
    families
}

/// The font files under `dir`, at most [`MAX_DEPTH`] folders deep, until there are
/// [`MAX_FILES`]. A folder is walked once (`seen`, by its real path, at most [`MAX_DIRS`]), so a
/// link back up the tree, or two links to one folder, add nothing twice.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES || seen.len() >= MAX_DIRS {
        return;
    }
    if !seen.insert(std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = e.path();
        // (A link to a folder is followed as the folder, within the limits.)
        if path.is_dir() {
            walk(&path, depth + 1, out, seen);
        } else if path
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| ["ttf", "otf", "ttc", "otc"].iter().any(|k| x.eq_ignore_ascii_case(k)))
        {
            out.push(path);
        }
    }
}

/// What indexing reads of a face: its family names (the typographic one first), weight, slope,
/// and whether it is a serif: its `OS/2` `sFamilyClass` is one of the serif classes (oldstyle,
/// transitional, modern, Clarendon, slab or freeform serifs), or, unclassed, its name says
/// "Serif" without "Sans".
#[derive(Debug, PartialEq, Eq)]
struct FaceInfo {
    names: Vec<String>,
    weight: u16,
    italic: bool,
    serif: bool,
}

/// The faces of the font file at `path` (one, or a collection's), each `None` where it can't be
/// read.
fn read_faces(path: &Path) -> Vec<Option<FaceInfo>> {
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    faces_of(&mut file)
}

/// The faces of a font file (a reader over it): see [`read_faces`]. Only the table directories,
/// the `name` table and the start of `OS/2` are read.
fn faces_of(r: &mut (impl Read + Seek)) -> Vec<Option<FaceInfo>> {
    let Some(head) = read_at(r, 0, 12) else { return Vec::new() };
    let starts: Vec<u32> = if head.get(0..4) == Some(b"ttcf") {
        let n = u32_at(&head, 8).unwrap_or(0).min(MAX_COLLECTION);
        let Some(offsets) = read_at(r, 12, n * 4) else { return Vec::new() };
        (0..n).filter_map(|i| u32_at(&offsets, i as usize * 4)).collect()
    } else {
        vec![0]
    };
    starts.into_iter().map(|start| face_info(r, start)).collect()
}

fn face_info(r: &mut (impl Read + Seek), start: u32) -> Option<FaceInfo> {
    let dir = read_at(r, start, 12)?;
    let n = u16_at(&dir, 4)?.min(MAX_TABLES);
    let records = read_at(r, start.checked_add(12)?, u32::from(n) * 16)?;
    let table = |tag: &[u8; 4]| {
        records.as_chunks::<16>().0.iter().find(|t| t.get(0..4) == Some(tag)).and_then(|t| Some((u32_at(t, 8)?, u32_at(t, 12)?)))
    };
    let (name_at, name_len) = table(b"name")?;
    let names = read_at(r, name_at, name_len.min(MAX_NAME_TABLE))?;
    let read_name = |id: u16| name(&names, id);
    let family = read_name(1)?;
    let typographic = read_name(16);
    let style = read_name(17).or_else(|| read_name(2)).unwrap_or_default().to_lowercase();
    let (weight, selection, class) = match table(b"OS/2") {
        Some((at, len)) => {
            // An unreadable `OS/2` (cut short, past the end) keeps the face, at the defaults.
            let os2 = read_at(r, at, len.min(64)).unwrap_or_default();
            (u16_at(&os2, 4).unwrap_or(400), u16_at(&os2, 62).unwrap_or(0), u16_at(&os2, 30).map_or(0, |c| c >> 8))
        }
        None => (400, 0, 0),
    };
    // fsSelection's ITALIC or OBLIQUE bit, or a style that says so.
    let italic = selection & 0x0201 != 0 || style.contains("italic") || style.contains("oblique");
    let lower = family.to_lowercase();
    let serif = matches!(class, 1..=5 | 7) || (class == 0 && lower.contains("serif") && !lower.contains("sans"));
    let mut names = Vec::with_capacity(2);
    names.extend(typographic);
    if !names.iter().any(|n: &String| n.eq_ignore_ascii_case(&family)) {
        names.push(family);
    }
    Some(FaceInfo { names, weight: weight.clamp(1, 1000), italic, serif })
}

/// Name `id` of a `name` table, in English where it has it: a Windows (UTF-16) record first, then
/// a Unicode one, then a Macintosh Roman one. Control characters are dropped; `None` when it is
/// missing or blank.
fn name(table: &[u8], id: u16) -> Option<String> {
    let count = u16_at(table, 2)?;
    let strings = usize::from(u16_at(table, 4)?);
    let mut best: Option<(u8, String)> = None;
    for i in 0..usize::from(count) {
        // A table cut short keeps the records before the cut (and what they matched).
        let Some(rec) = table.get(6 + i * 12..6 + i * 12 + 12) else { break };
        let (platform, lang, name_id) = (u16_at(rec, 0)?, u16_at(rec, 4)?, u16_at(rec, 6)?);
        if name_id != id {
            continue;
        }
        let (len, at) = (usize::from(u16_at(rec, 8)?), usize::from(u16_at(rec, 10)?));
        let Some(raw) = table.get(strings + at..strings + at + len) else { continue };
        let (rank, text) = match platform {
            3 | 0 => {
                let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
                let english = platform == 0 || lang == 0x0409;
                (if english { 0 } else { 2 }, String::from_utf16_lossy(&units))
            }
            // Macintosh Roman: its ASCII half is all a family name needs.
            1 => (if lang == 0 { 1 } else { 3 }, raw.iter().map(|b| if b.is_ascii() { char::from(*b) } else { '?' }).collect()),
            _ => continue,
        };
        let text: String = text.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
        let text = text.trim().to_string();
        if !text.is_empty() && best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, text));
        }
    }
    best.map(|(_, t)| t)
}

/// `len` bytes from `at` (`None` when the file ends before them).
fn read_at(r: &mut (impl Read + Seek), at: u32, len: u32) -> Option<Vec<u8>> {
    r.seek(SeekFrom::Start(u64::from(at))).ok()?;
    let mut buf = Vec::new();
    r.take(u64::from(len)).read_to_end(&mut buf).ok()?;
    (buf.len() == len as usize).then_some(buf)
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_families_are_read_without_waiting_once_indexed() {
        // No warm-up here (it would make every test in this process draw without waiting), so
        // drawing finds a family as a command does: the families are indexed at once.
        let all = installed();
        assert_eq!(installed_now().map(<[Family]>::len), Some(all.len()));
        if let Some(f) = all.iter().find(|f| f.listed) {
            assert_eq!(find_ready(&f.name), find(&f.name));
        }
        assert!(find_ready("No Such Family").is_none());
    }

    #[test]
    fn a_font_file_is_indexed_by_its_family_names_weight_and_slope() {
        // egui's own faces, written into a folder as installed fonts would be.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ubuntu");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("Ubuntu-Light.ttf"), epaint_default_fonts::UBUNTU_LIGHT).unwrap();
        std::fs::write(dir.path().join("Hack-Regular.TTF"), epaint_default_fonts::HACK_REGULAR).unwrap();
        std::fs::write(dir.path().join("broken.otf"), b"OTTO\x00\x05garbage").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"not a font").unwrap();
        let fams = index(&[dir.path().to_path_buf()]);
        let names: Vec<&str> = fams.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"Hack"), "{names:?}");
        let ubuntu = fams.iter().find(|f| f.name.to_lowercase().starts_with("ubuntu")).expect("ubuntu indexed");
        assert_eq!(ubuntu.faces.len(), 1);
        assert!(ubuntu.faces[0].weight < 400 && !ubuntu.faces[0].italic, "{ubuntu:?}");
        // Neither is classed as a serif (a layout text in either falls back to the sans).
        assert!(fams.iter().all(|f| !f.serif), "{fams:?}");
        // A face its OS/2 table classes as a serif (sFamilyClass 1, oldstyle serifs) is one.
        let mut serif = epaint_default_fonts::HACK_REGULAR.to_vec();
        let tables = usize::from(u16_at(&serif, 4).unwrap());
        let os2 = (0..tables).map(|i| 12 + i * 16).find(|r| serif.get(*r..r + 4) == Some(b"OS/2")).unwrap();
        let at = u32_at(&serif, os2 + 8).unwrap() as usize;
        serif[at + 30] = 1;
        assert!(faces_of(&mut std::io::Cursor::new(serif))[0].as_ref().unwrap().serif);
        // Found in any case; the face it draws in, its bytes read once.
        let i = find_in(&fams, &ubuntu.name.to_uppercase()).unwrap();
        let fam = &fams[i as usize];
        let (face, slanted) = pick(fam, false, true).unwrap();
        assert!(slanted, "no italic face: slanted by hand");
        // A light face standing in for bold is drawn heavier, by the same rule everywhere.
        assert!(heavier(face, true) && !heavier(face, false));
        assert!(!heavier(&InstalledFace { weight: 700, ..face.clone() }, true));
        // A face the file hasn't (a damaged face of a collection) is none, and doesn't hide the
        // file's good one.
        assert!(bytes(&InstalledFace { index: 5, ..face.clone() }).is_none());
        let b = bytes(face).unwrap();
        assert!(std::ptr::eq(b, bytes(face).unwrap()), "kept, not read again");
        assert!(find_in(&fams, "No Such Family").is_none() && find_in(&fams, "  ").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_is_walked_once_however_many_links_reach_it() {
        // Links back up the tree, four to a folder: followed blindly, they'd fan out to 4^8 folders
        // and list the one font thousands of times.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Hack-Regular.ttf"), epaint_default_fonts::HACK_REGULAR).unwrap();
        for i in 0..4 {
            std::os::unix::fs::symlink(dir.path(), dir.path().join(format!("up{i}"))).unwrap();
        }
        let mut files = Vec::new();
        let mut seen = HashSet::new();
        walk(dir.path(), 0, &mut files, &mut seen);
        assert_eq!((files.len(), seen.len()), (1, 1), "{files:?}");
    }

    #[test]
    fn a_name_table_cut_short_keeps_the_names_before_the_cut() {
        // Two records listed, the second cut off: the first's family name still reads.
        let text: Vec<u8> = "Sans".encode_utf16().flat_map(u16::to_be_bytes).collect();
        let strings = 6 + 12 + 4;
        let mut table = vec![0, 0, 0, 2];
        table.extend((strings as u16).to_be_bytes());
        for v in [3u16, 1, 0x0409, 1, text.len() as u16, 0] {
            table.extend(v.to_be_bytes());
        }
        table.extend([0, 3, 0, 1]);
        table.extend(&text);
        assert_eq!(name(&table, 1).as_deref(), Some("Sans"));
    }

    #[test]
    fn a_family_draws_in_its_nearest_face() {
        let face = |weight, italic| InstalledFace { path: PathBuf::from(format!("{weight}{italic}")), index: 0, weight, italic };
        let fam = Family {
            name: "F".into(),
            listed: true,
            faces: vec![face(300, false), face(500, false), face(800, false), face(400, true)],
            serif: false,
        };
        let w = |bold, italic| pick(&fam, bold, italic).map(|(f, s)| (f.weight, f.italic, s)).unwrap();
        // 300 and 500 are as near 400: the regular takes the lighter.
        assert_eq!(w(false, false), (300, false, false));
        assert_eq!(w(true, false), (800, false, false));
        assert_eq!(w(false, true), (400, true, false));
        assert_eq!(w(true, true), (400, true, false), "the only italic, not a slanted bold");
        let upright = Family { faces: vec![face(400, false)], ..fam.clone() };
        assert_eq!(pick(&upright, false, true).map(|(f, s)| (f.weight, s)), Some((400, true)));
        assert!(pick(&Family { faces: Vec::new(), ..fam }, false, false).is_none());
    }

    #[test]
    fn hostile_font_files_are_skipped() {
        use std::io::Cursor;
        // Truncated headers, a collection claiming millions of faces, tables past the end.
        let mut ttc = b"ttcf\x00\x01\x00\x00\xff\xff\xff\xff".to_vec();
        ttc.extend([0u8; 16]);
        let mut dir = b"\x00\x01\x00\x00\xff\xff\x00\x00\x00\x00\x00\x00".to_vec();
        dir.extend(b"name\x00\x00\x00\x00\xff\xff\xff\xf0\x7f\xff\xff\xff");
        for bytes in [&b""[..], b"\x00\x01", &ttc, &dir] {
            let faces = faces_of(&mut Cursor::new(bytes.to_vec()));
            assert!(faces.iter().all(Option::is_none), "{faces:?}");
        }
        // A name table whose records point past its end.
        let table = [0u8, 0, 0, 1, 0, 18, 0, 3, 0, 1, 4, 9, 0, 1, 0xff, 0xff, 0, 0];
        assert_eq!(name(&table, 1), None);
    }

    #[test]
    fn linux_font_roots_include_flatpak_host_and_xdg_roots() {
        use std::ffi::OsStr;

        let dirs = crate::fonts::linux_font_roots_with(Some(OsStr::new("/xdg/data")), Some(OsStr::new("/home/alice")));
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/usr/share/fonts"),
                PathBuf::from("/usr/local/share/fonts"),
                PathBuf::from("/run/host/fonts"),
                PathBuf::from("/run/host/local-fonts"),
                PathBuf::from("/run/host/user-fonts"),
                PathBuf::from("/xdg/data/fonts"),
                PathBuf::from("/home/alice/.fonts"),
            ]
        );
        let fallback = crate::fonts::linux_font_roots_with(None, Some(OsStr::new("/home/alice")));
        assert!(fallback.contains(&PathBuf::from("/home/alice/.local/share/fonts")));

        let deduped = crate::fonts::linux_font_roots_with(Some(OsStr::new("/usr/share")), Some(OsStr::new("/home/alice")));
        assert_eq!(deduped.iter().filter(|p| *p == &PathBuf::from("/usr/share/fonts")).count(), 1);
    }
}
