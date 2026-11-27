//! Font discovery: a good installed sans-serif when there is one (Segoe UI, SF, Inter, Noto Sans,
//! Open Sans, DejaVu Sans), else the Ubuntu Light face that ships with egui; and an installed
//! serif for the layout texts that ask for one (#517). Fonts are read at run time and never
//! bundled.

use std::sync::OnceLock;

/// Candidate files for (regular, semibold), in order of preference.
const CANDIDATES: &[(&str, &str)] = &[
    ("C:\\Windows\\Fonts\\segoeui.ttf", "C:\\Windows\\Fonts\\seguisb.ttf"),
    ("/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/SFNS.ttf"),
    ("/System/Library/Fonts/Supplemental/Arial.ttf", "/System/Library/Fonts/Supplemental/Arial Bold.ttf"),
    ("/system/fonts/Roboto-Regular.ttf", "/system/fonts/Roboto-Medium.ttf"),
    ("/system/fonts/RobotoStatic-Regular.ttf", "/system/fonts/RobotoStatic-Medium.ttf"),
    ("/system/fonts/NotoSans-Regular.ttf", "/system/fonts/NotoSans-Bold.ttf"),
    ("/usr/share/fonts/TTF/Inter-Regular.ttf", "/usr/share/fonts/TTF/Inter-SemiBold.ttf"),
    ("/usr/share/fonts/inter/Inter-Regular.ttf", "/usr/share/fonts/inter/Inter-SemiBold.ttf"),
    ("/usr/share/fonts/noto/NotoSans-Regular.ttf", "/usr/share/fonts/noto/NotoSans-SemiBold.ttf"),
    ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf"),
    ("/usr/share/fonts/google-noto/NotoSans-Regular.ttf", "/usr/share/fonts/google-noto/NotoSans-SemiBold.ttf"),
    ("/usr/share/fonts/TTF/OpenSans-Regular.ttf", "/usr/share/fonts/TTF/OpenSans-SemiBold.ttf"),
    ("/usr/share/fonts/truetype/open-sans/OpenSans-Regular.ttf", "/usr/share/fonts/truetype/open-sans/OpenSans-SemiBold.ttf"),
    ("/usr/share/fonts/TTF/DejaVuSans.ttf", "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
];

/// Largest regular/script font or downloaded font (32 MiB). Installed CJK collections
/// have a separate 128 MiB budget; downloads use the smaller bound on every platform.
pub const MAX_FONT_BYTES: usize = 32 << 20;

pub struct UiFonts {
    pub regular: Vec<u8>,
    pub semibold: Vec<u8>,
    /// File the regular face came from, or "built-in".
    pub source: String,
}

/// Reads a regular font within its budget, including a file that grows after metadata is read.
/// Uses a nonblocking open on Linux and macOS so a named pipe swapped into a candidate path can
/// never block font discovery waiting for a writer.
fn read_bounded(path: impl AsRef<std::path::Path>, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let path = path.as_ref();
    let pre_meta = std::fs::metadata(path).ok()?;
    if !pre_meta.is_file() || pre_meta.len() > limit {
        return None;
    }
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(all(
        target_os = "linux",
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "x86",
            target_arch = "arm",
            target_arch = "riscv64",
            target_arch = "powerpc64",
            target_arch = "s390x",
            target_arch = "loongarch64"
        )
    ))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Linux asm-generic `O_NONBLOCK` (`0o4000`).
        open.custom_flags(0o4000);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // macOS/BSD `O_NONBLOCK` (`0x0004`).
        open.custom_flags(0x0004);
    }
    let file = open.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn read_path(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = read_bounded(path, MAX_FONT_BYTES as u64)?;
    ab_glyph::FontRef::try_from_slice(&bytes).ok()?;
    Some(bytes)
}

fn read(path: &str) -> Option<Vec<u8>> {
    read_path(std::path::Path::new(path))
}

/// The weight the semibold face is drawn at where it is a variable font (#591): macOS's system
/// font is one file for every weight, drawn at its default 400 unless told otherwise. 600 is
/// semibold, as the static faces used elsewhere (Segoe UI Semibold, Inter SemiBold) are; a face
/// with no weight axis ignores it. Both renderers set it, so text is as heavy on screen as in an
/// export.
pub const SEMIBOLD_WEIGHT: f32 = 600.0;

/// Whether bold text in the interface fonts has no heavier face to be drawn in: the semibold face
/// is the regular one, with no weight axis to draw it heavier (the built-in Ubuntu Light, which the
/// browser always has, or a system font found without its bold file). Bold text is then thickened
/// by hand ([`bold_thickening`]), as italic is slanted where there is no italic face. Both
/// renderers ask here, so bold text is as heavy on screen as in an export.
pub fn bold_is_synthetic() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| {
        let f = ui_fonts();
        synthetic_bold(&f.regular, &f.semibold)
    })
}

fn synthetic_bold(regular: &[u8], semibold: &[u8]) -> bool {
    use ab_glyph::VariableFont;
    regular == semibold && ab_glyph::FontRef::try_from_slice(semibold).is_ok_and(|f| !f.variations().iter().any(|a| &a.tag == b"wght"))
}

/// How far (pixels, at a text size of `px` pixels) bold text thickened by hand is drawn a second
/// time to the right of itself: a thirtieth of its size, about the stem a bold face adds.
pub fn bold_thickening(px: f32) -> f32 {
    if px.is_finite() && px > 0.0 { (px / 30.0).clamp(0.25, 3.0) } else { 0.0 }
}

/// The interface fonts, discovered once.
pub fn ui_fonts() -> &'static UiFonts {
    static F: OnceLock<UiFonts> = OnceLock::new();
    F.get_or_init(|| {
        if std::env::var_os("RUSTGIS_BUILTIN_FONTS").is_none() {
            for (r, b) in CANDIDATES {
                if let Some(regular) = read(r) {
                    let semibold = read(b).unwrap_or_else(|| regular.clone());
                    return UiFonts { regular, semibold, source: (*r).to_string() };
                }
            }
            for root in extra_linux_roots(&linux_font_roots()) {
                for (r, b) in CANDIDATES {
                    if let (Some(r_rel), Some(b_rel)) = (r.strip_prefix(LINUX_SYSTEM_FONTS), b.strip_prefix(LINUX_SYSTEM_FONTS)) {
                        let r_path = root.join(r_rel);
                        if let Some(regular) = read_path(&r_path) {
                            let semibold = read_path(&root.join(b_rel)).unwrap_or_else(|| regular.clone());
                            return UiFonts { regular, semibold, source: r_path.display().to_string() };
                        }
                    }
                }
            }
        }
        let b = epaint_default_fonts::UBUNTU_LIGHT.to_vec();
        UiFonts { regular: b.clone(), semibold: b, source: "built-in".into() }
    })
}

/// Offset in ems that puts an installed CJK face on the interface face's baseline when their
/// line boxes are centred. Hiragino Sans GB, for example, has half an em of extra line gap.
pub fn cjk_ui_baseline_offset(bytes: &[u8], index: u32) -> f32 {
    fn baseline(bytes: &[u8], index: u32) -> Option<f32> {
        let face = ttf_parser::Face::parse(bytes, index).ok()?;
        let ascent = f32::from(face.ascender());
        let height = ascent - f32::from(face.descender()) + f32::from(face.line_gap());
        Some((ascent - height / 2.0) / f32::from(face.units_per_em()))
    }
    baseline(&ui_fonts().regular, 0).zip(baseline(bytes, index)).map_or(0.0, |(ui, cjk)| ui - cjk)
}

/// Installed serif families: (regular, bold, italic, bold italic) files, in order of preference.
/// Full paths, as the sans candidates are; a variable Noto Serif is looked for after these (see
/// [`variable_noto_serif`]), then [`SERIF_LAST`].
const SERIF_CANDIDATES: &[[&str; 4]] = &[
    [
        "C:\\Windows\\Fonts\\georgia.ttf",
        "C:\\Windows\\Fonts\\georgiab.ttf",
        "C:\\Windows\\Fonts\\georgiai.ttf",
        "C:\\Windows\\Fonts\\georgiaz.ttf",
    ],
    [
        "C:\\Windows\\Fonts\\times.ttf",
        "C:\\Windows\\Fonts\\timesbd.ttf",
        "C:\\Windows\\Fonts\\timesi.ttf",
        "C:\\Windows\\Fonts\\timesbi.ttf",
    ],
    [
        "/System/Library/Fonts/Supplemental/Georgia.ttf",
        "/System/Library/Fonts/Supplemental/Georgia Bold.ttf",
        "/System/Library/Fonts/Supplemental/Georgia Italic.ttf",
        "/System/Library/Fonts/Supplemental/Georgia Bold Italic.ttf",
    ],
    [
        "/System/Library/Fonts/Supplemental/Times New Roman.ttf",
        "/System/Library/Fonts/Supplemental/Times New Roman Bold.ttf",
        "/System/Library/Fonts/Supplemental/Times New Roman Italic.ttf",
        "/System/Library/Fonts/Supplemental/Times New Roman Bold Italic.ttf",
    ],
    [
        "/usr/share/fonts/noto/NotoSerif-Regular.ttf",
        "/usr/share/fonts/noto/NotoSerif-Bold.ttf",
        "/usr/share/fonts/noto/NotoSerif-Italic.ttf",
        "/usr/share/fonts/noto/NotoSerif-BoldItalic.ttf",
    ],
    [
        "/usr/share/fonts/truetype/noto/NotoSerif-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSerif-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSerif-Italic.ttf",
        "/usr/share/fonts/truetype/noto/NotoSerif-BoldItalic.ttf",
    ],
    [
        "/usr/share/fonts/google-noto/NotoSerif-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSerif-Bold.ttf",
        "/usr/share/fonts/google-noto/NotoSerif-Italic.ttf",
        "/usr/share/fonts/google-noto/NotoSerif-BoldItalic.ttf",
    ],
];

/// Serif families taken when none of the above (nor a variable Noto Serif) is installed.
const SERIF_LAST: &[[&str; 4]] = &[
    [
        "/usr/share/fonts/TTF/DejaVuSerif.ttf",
        "/usr/share/fonts/TTF/DejaVuSerif-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSerif-Italic.ttf",
        "/usr/share/fonts/TTF/DejaVuSerif-BoldItalic.ttf",
    ],
    [
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Italic.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif-BoldItalic.ttf",
    ],
    [
        "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSerif-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSerif-Italic.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSerif-BoldItalic.ttf",
    ],
    [
        "/usr/share/fonts/truetype/liberation2/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/truetype/liberation2/LiberationSerif-Bold.ttf",
        "/usr/share/fonts/truetype/liberation2/LiberationSerif-Italic.ttf",
        "/usr/share/fonts/truetype/liberation2/LiberationSerif-BoldItalic.ttf",
    ],
    [
        "/usr/share/fonts/liberation-serif/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/liberation-serif/LiberationSerif-Bold.ttf",
        "/usr/share/fonts/liberation-serif/LiberationSerif-Italic.ttf",
        "/usr/share/fonts/liberation-serif/LiberationSerif-BoldItalic.ttf",
    ],
];

/// An installed serif family (#517): its regular face, and its bold, italic and bold italic ones
/// where it has them. A style it lacks is drawn from the regular: slanted for italic, but not
/// made heavier, so bold in a family with no bold file looks as the regular does.
pub struct SerifFonts {
    pub regular: Vec<u8>,
    pub bold: Option<Vec<u8>>,
    pub italic: Option<Vec<u8>>,
    pub bold_italic: Option<Vec<u8>>,
    /// One file for every weight (a variable Noto Serif): `bold` and `bold_italic` are left empty
    /// and drawn from `regular` and `italic` at the bold weight, so its bytes are kept once.
    pub variable: bool,
    /// File the regular face came from.
    pub source: String,
}

/// The installed serif, found once: `None` when there is none (the web app, a bare Linux box),
/// or with `RUSTGIS_BUILTIN_FONTS` set, so tests draw alike on every machine. Text asking for a
/// serif is then drawn in the sans.
pub fn serif_fonts() -> Option<&'static SerifFonts> {
    static F: OnceLock<Option<SerifFonts>> = OnceLock::new();
    F.get_or_init(|| {
        if std::env::var_os("RUSTGIS_BUILTIN_FONTS").is_some() {
            return None;
        }
        let roots = linux_font_roots();
        let family = |[r, b, i, bi]: &[&str; 4]| {
            let regular = read(r)?;
            Some(SerifFonts { regular, bold: read(b), italic: read(i), bold_italic: read(bi), variable: false, source: (*r).to_string() })
        };
        let find_fixed = |list: &[[&str; 4]]| {
            list.iter().find_map(family).or_else(|| {
                extra_linux_roots(&roots).find_map(|root| {
                    list.iter().find_map(|[r, b, i, bi]| {
                        let join = |p: &str| p.strip_prefix(LINUX_SYSTEM_FONTS).map(|rel| root.join(rel));
                        let r_path = join(r)?;
                        let regular = read_path(&r_path)?;
                        Some(SerifFonts {
                            regular,
                            bold: join(b).as_deref().and_then(read_path),
                            italic: join(i).as_deref().and_then(read_path),
                            bold_italic: join(bi).as_deref().and_then(read_path),
                            variable: false,
                            source: r_path.display().to_string(),
                        })
                    })
                })
            })
        };
        // Search by family tier (system paths before extra Linux roots within each tier):
        // 1) static preferred serifs (`SERIF_CANDIDATES`: Georgia, Times New Roman, static Noto Serif),
        // 2) variable Noto Serif across all roots (`serif_font_roots`),
        // 3) fallback serifs (`SERIF_LAST`: DejaVu Serif, Liberation Serif).
        find_fixed(SERIF_CANDIDATES).or_else(|| variable_noto_serif(&serif_font_roots(&roots))).or_else(|| find_fixed(SERIF_LAST))
    })
    .as_ref()
}

/// System font directory whose relative paths are expanded across Linux, Flatpak host, and XDG roots.
const LINUX_SYSTEM_FONTS: &str = "/usr/share/fonts/";

/// Flatpak runtime mounts exposing the host system, local, and user font directories (#2313).
const FLATPAK_FONT_DIRS: &[&str] = &["/run/host/fonts", "/run/host/local-fonts", "/run/host/user-fonts"];

fn push_unique_root(out: &mut Vec<std::path::PathBuf>, path: std::path::PathBuf) {
    if !path.as_os_str().is_empty() && !out.contains(&path) {
        out.push(path);
    }
}

/// POSIX absolute path check used for `XDG_DATA_HOME` (required by the XDG Base Directory spec)
/// and `HOME`, working consistently even when Linux root logic is unit-tested on Windows.
pub(crate) fn is_posix_absolute(path: &std::ffi::OsStr) -> bool {
    path.as_encoded_bytes().starts_with(b"/")
}

/// Linux font roots in search order: system, local, Flatpak host mounts, and XDG/user directories.
/// Shared with `crate::families::font_dirs` on Linux.
pub(crate) fn linux_font_roots_with(xdg_data_home: Option<&std::ffi::OsStr>, home: Option<&std::ffi::OsStr>) -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    for dir in ["/usr/share/fonts", "/usr/local/share/fonts"] {
        push_unique_root(&mut roots, std::path::PathBuf::from(dir));
    }
    for dir in FLATPAK_FONT_DIRS {
        push_unique_root(&mut roots, std::path::PathBuf::from(dir));
    }
    if let Some(xdg) = xdg_data_home.filter(|v| is_posix_absolute(v)) {
        push_unique_root(&mut roots, std::path::PathBuf::from(xdg).join("fonts"));
    } else if let Some(h) = home.filter(|v| is_posix_absolute(v)) {
        push_unique_root(&mut roots, std::path::PathBuf::from(h).join(".local").join("share").join("fonts"));
    }
    if let Some(h) = home.filter(|v| is_posix_absolute(v)) {
        push_unique_root(&mut roots, std::path::PathBuf::from(h).join(".fonts"));
    }
    roots
}

fn linux_font_roots() -> Vec<std::path::PathBuf> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    linux_font_roots_with(std::env::var_os("XDG_DATA_HOME").as_deref(), std::env::var_os("HOME").as_deref())
}

fn extra_linux_roots(roots: &[std::path::PathBuf]) -> impl Iterator<Item = &std::path::PathBuf> {
    roots.iter().filter(|r| *r != std::path::Path::new("/usr/share/fonts"))
}

/// The folders under each root where distributions put each font package's own folder.
/// Always includes `/usr/share/fonts` first so non-Linux Unix systems (e.g. FreeBSD) keep
/// the original system `FONT_ROOTS` search for variable Noto Serif when Linux-only Flatpak/XDG
/// roots are gated off.
fn serif_font_roots(roots: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut base = vec![std::path::PathBuf::from("/usr/share/fonts")];
    for root in roots {
        push_unique_root(&mut base, root.clone());
    }
    let mut out = Vec::new();
    for root in base {
        out.push(root.clone());
        out.push(root.join("truetype"));
        out.push(root.join("opentype"));
    }
    out
}

/// Noto Serif as recent distributions ship it: a variable font (`NotoSerif[wdth,wght].ttf`, or
/// `[wght]`), with its italic beside it, in a package folder under one of `roots`
/// ([`serif_font_roots`]). Found by its name's start, whatever axes it lists; its bold is the
/// same file (kept once), drawn at the bold weight.
fn variable_noto_serif<P: AsRef<std::path::Path>>(roots: &[P]) -> Option<SerifFonts> {
    // Shared budgets across all roots so adding Flatpak/XDG roots keeps worst-case UI-thread
    // directory work bounded near the original 3-root × 1,000-folder limit.
    const MAX_DIRS: usize = 3_000;
    const MAX_ENTRIES_PER_DIR: usize = 10_000;
    const MAX_TOTAL_ENTRIES: usize = 100_000;
    let named = |files: &[std::path::PathBuf], stem: &str| {
        files.iter().find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(stem) && n.ends_with(".ttf"))).cloned()
    };
    let mut entries_remaining = MAX_TOTAL_ENTRIES;
    // Folders and files in name order, so the same one is picked on every machine whatever order
    // the system lists them in; loose files don't count against the folders taken.
    let listed = |dir: &std::path::Path, entries_remaining: &mut usize| -> Vec<std::path::PathBuf> {
        let limit = (*entries_remaining).min(MAX_ENTRIES_PER_DIR);
        if limit == 0 {
            return Vec::new();
        }
        let mut paths: Vec<std::path::PathBuf> =
            std::fs::read_dir(dir).map(|e| e.flatten().take(limit).map(|e| e.path()).collect()).unwrap_or_default();
        *entries_remaining = entries_remaining.saturating_sub(paths.len());
        paths.sort();
        paths
    };
    let mut dirs_visited = 0usize;
    for root in roots {
        if dirs_visited >= MAX_DIRS || entries_remaining == 0 {
            break;
        }
        for dir in listed(root.as_ref(), &mut entries_remaining).into_iter().filter(|p| p.is_dir()) {
            if dirs_visited >= MAX_DIRS || entries_remaining == 0 {
                break;
            }
            dirs_visited += 1;
            let files = listed(&dir, &mut entries_remaining);
            let Some(r) = named(&files, "NotoSerif[") else { continue };
            let Some(regular) = read_path(&r) else { continue };
            let italic = named(&files, "NotoSerif-Italic[").as_deref().and_then(read_path);
            let source = r.display().to_string();
            return Some(SerifFonts { regular, bold: None, italic, bold_italic: None, variable: true, source });
        }
    }
    None
}

/// Whether a serif is installed: text asking for one is drawn in it, not in the sans.
pub fn serif_available() -> bool {
    serif_fonts().is_some()
}

/// Regional CJK glyph styles (the same code point is drawn differently by region).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CjkRegion {
    SimplifiedChinese,
    /// Taiwan's standard glyph forms (Traditional Chinese).
    TraditionalChinese,
    Japanese,
    Korean,
}

/// Installed CJK font candidates: (path, face index in a collection).
fn cjk_candidates(region: CjkRegion) -> &'static [(&'static str, u32)] {
    // Noto Sans CJK collections order their faces JP, KR, SC, TC, HK.
    match region {
        CjkRegion::SimplifiedChinese => &[
            ("C:\\Windows\\Fonts\\msyh.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/noto-cjk/NotoSansSC-Regular.otf", 0),
            ("/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc", 0),
        ],
        // PingFang TC has no fixed place in its collection: see `cjk_named_candidates`.
        CjkRegion::TraditionalChinese => &[
            ("C:\\Windows\\Fonts\\msjh.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 3),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 3),
            ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 3),
            ("/usr/share/fonts/noto-cjk/NotoSansTC-Regular.otf", 0),
        ],
        CjkRegion::Japanese => &[
            ("C:\\Windows\\Fonts\\YuGothR.ttc", 0),
            ("C:\\Windows\\Fonts\\meiryo.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansJP-Regular.otf", 0),
        ],
        CjkRegion::Korean => &[
            ("C:\\Windows\\Fonts\\malgun.ttf", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 1),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 1),
            ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 1),
            ("/usr/share/fonts/noto-cjk/NotoSansKR-Regular.otf", 0),
        ],
    }
}

/// Where Apple systems keep their fonts: macOS in the top folder, iOS and iPadOS in subfolders
/// (#2153). The same collection can sit in a different one from one release to the next, so each
/// Apple face is looked for in all of them.
const APPLE_FONT_DIRS: &[&str] = &[
    "/System/Library/Fonts",
    "/System/Library/Fonts/Core",
    "/System/Library/Fonts/CoreAddition",
    "/System/Library/Fonts/CoreUI",
    "/System/Library/Fonts/LanguageSupport",
    "/System/Library/Fonts/UnicodeSupport",
    "/System/Library/Fonts/AppFonts",
];

/// Apple's CJK collections for `region`, by file name: (file, face index). macOS and iOS name some
/// of them differently (Hiragino Kaku Gothic, PingFang).
fn apple_cjk_files(region: CjkRegion) -> &'static [(&'static str, u32)] {
    match region {
        CjkRegion::SimplifiedChinese => &[("PingFang.ttc", 0), ("PingFangUI.ttc", 0), ("Hiragino Sans GB.ttc", 0)],
        // PingFang TC has no fixed place in its collection: see `apple_cjk_named_files`.
        CjkRegion::TraditionalChinese => &[],
        CjkRegion::Japanese => &[("ヒラギノ角ゴシック W3.ttc", 0), ("HiraginoKakuGothic.ttc", 0)],
        CjkRegion::Korean => &[("AppleSDGothicNeo.ttc", 0)],
    }
}

/// Apple's CJK collections whose face is found by family name, not by index: (file, family).
fn apple_cjk_named_files(region: CjkRegion) -> &'static [(&'static str, &'static str)] {
    match region {
        CjkRegion::TraditionalChinese => &[("PingFang.ttc", "PingFang TC"), ("PingFangUI.ttc", "PingFang TC")],
        _ => &[],
    }
}

/// `file` in each of the Apple font folders, in order.
fn in_apple_dirs<T: Copy>(files: &[(&str, T)]) -> impl Iterator<Item = (std::path::PathBuf, T)> {
    APPLE_FONT_DIRS.iter().flat_map(move |dir| files.iter().map(move |(file, x)| (std::path::PathBuf::from(dir).join(file), *x)))
}

/// Android's system and manufacturer font folders. Shared with the family index, so labels
/// can select the same installed faces as the interface.
pub(crate) const ANDROID_FONT_DIRS: &[&str] = &["/system/fonts", "/product/fonts", "/system_ext/fonts"];

/// Android's Noto collections use JP, KR, SC, TC, HK face order (AOSP `data/fonts/fonts.xml`).
/// Older devices can have only Droid Sans Fallback, whose single face covers CJK.
fn android_cjk_files(region: CjkRegion) -> [(&'static str, u32); 3] {
    let index = match region {
        CjkRegion::Japanese => 0,
        CjkRegion::Korean => 1,
        CjkRegion::SimplifiedChinese => 2,
        CjkRegion::TraditionalChinese => 3,
    };
    [("NotoSansCJK-Regular.ttc", index), ("NotoSerifCJK-Regular.ttc", index), ("DroidSansFallback.ttf", 0)]
}

/// Every installed CJK font candidate for `region`, in order of preference: (path, face index).
/// Windows first, then Apple's (macOS, iOS), Linux's (system, Flatpak host, XDG) and Android's.
fn cjk_paths(region: CjkRegion) -> Vec<(std::path::PathBuf, u32)> {
    cjk_paths_with(region, &linux_font_roots())
}

fn cjk_paths_with(region: CjkRegion, roots: &[std::path::PathBuf]) -> Vec<(std::path::PathBuf, u32)> {
    let listed = cjk_candidates(region);
    let (windows, rest) = listed.split_at(listed.iter().take_while(|(p, _)| p.starts_with("C:\\")).count());
    let linux_extra = extra_linux_roots(roots)
        .flat_map(|root| rest.iter().filter_map(move |(p, i)| p.strip_prefix(LINUX_SYSTEM_FONTS).map(|rel| (root.join(rel), *i))));
    windows
        .iter()
        .map(|(p, i)| (std::path::PathBuf::from(p), *i))
        .chain(in_apple_dirs(apple_cjk_files(region)))
        .chain(rest.iter().map(|(p, i)| (std::path::PathBuf::from(p), *i)))
        .chain(linux_extra)
        .chain(
            ANDROID_FONT_DIRS
                .iter()
                .flat_map(|dir| android_cjk_files(region).map(|(file, i)| (std::path::PathBuf::from(dir).join(file), i))),
        )
        .collect()
}

const ALL_CJK_REGIONS: [CjkRegion; 4] =
    [CjkRegion::SimplifiedChinese, CjkRegion::TraditionalChinese, CjkRegion::Japanese, CjkRegion::Korean];

/// Deduplicated Linux CJK font filenames and face indices in preference order.
fn cjk_linux_filenames(region: CjkRegion) -> Vec<(&'static str, u32)> {
    let mut out: Vec<(&'static str, u32)> = Vec::new();
    for &(path, index) in cjk_candidates(region) {
        if path.starts_with(LINUX_SYSTEM_FONTS)
            && let Some(name) = path.rsplit('/').next()
            && !out.iter().any(|&(n, i)| n == name && i == index)
        {
            out.push((name, index));
        }
    }
    out
}

/// Union of Linux CJK filenames across all four regions so a single bounded scan serves every region.
fn all_cjk_linux_filenames() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for region in ALL_CJK_REGIONS {
        for (name, _) in cjk_linux_filenames(region) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Diagnostics from the bounded Linux CJK font-folder fallback scan, recording any unreadable
/// directories, directory-entry errors, or folders truncated/skipped at the scan limits.
/// Clean (`is_clean()`) when every existing folder under the scanned roots was walked in full.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CjkRootScanSummary {
    /// Directories whose `read_dir` failed for a reason other than `NotFound`.
    pub unreadable_dirs: usize,
    /// Directory entries that returned an I/O error during iteration.
    pub entry_errors: usize,
    /// Directories truncated or skipped because a depth, directory, file, or entry cap was hit.
    pub truncated_dirs: usize,
    /// Whether the OS refused to spawn the background scan thread in an async UI session.
    pub thread_spawn_failed: bool,
    /// First affected path, so notices say where skipping or errors happened.
    pub sample_path: Option<std::path::PathBuf>,
}

impl CjkRootScanSummary {
    /// Whether every existing folder under the scanned roots was walked completely without errors.
    pub fn is_clean(&self) -> bool {
        self.unreadable_dirs == 0 && self.entry_errors == 0 && self.truncated_dirs == 0 && !self.thread_spawn_failed
    }

    /// Human-readable notice naming what was skipped or unreadable, how much, and where,
    /// or `None` when the scan completed cleanly.
    pub fn notice(&self) -> Option<String> {
        if self.is_clean() {
            return None;
        }
        let mut parts = Vec::new();
        if self.thread_spawn_failed {
            parts.push("background scan thread could not start".to_string());
        }
        if self.truncated_dirs > 0 {
            parts.push(format!("{} folder(s) reached the scan limit", self.truncated_dirs));
        }
        if self.unreadable_dirs > 0 {
            parts.push(format!("{} unreadable folder(s)", self.unreadable_dirs));
        }
        if self.entry_errors > 0 {
            parts.push(format!("{} directory entry error(s)", self.entry_errors));
        }
        let where_text = self.sample_path.as_deref().map(|p| format!(" (e.g. {})", p.display())).unwrap_or_default();
        Some(format!("CJK font folder search stopped early: {}{where_text}; some installed CJK fonts may not be found.", parts.join(", ")))
    }
}

struct CjkRootIndex {
    files: std::collections::HashMap<&'static str, Vec<std::path::PathBuf>>,
    summary: CjkRootScanSummary,
}

/// Shared bounded BFS under Linux `roots` indexing known CJK font filenames across all four regions
/// (`MAX_DEPTH = 6`, `MAX_DIRS = 2_000`, `MAX_FILES = 20_000`).
///
/// In desktop UI sessions (`crate::families::warm_up` enables async fallback), this scan starts
/// lazily on a background `"cjk-font-scan"` thread only if a CJK lookup misses every fixed path;
/// `cjk_font` returns `None` without blocking while the walk runs and the UI retries once
/// [`cjk_roots_ready`] flips true. Both that background walk and synchronous CLI/Python/headless
/// runs execute once per process as a bounded non-cancellable step.
///
/// Out of scope for #2313: Arabic, Bengali, Lao, and symbol faces remain on their fixed paths
/// (`script_paths`) without nested directory fallback search.
fn scan_cjk_roots(roots: &[std::path::PathBuf]) -> CjkRootIndex {
    const MAX_DEPTH: usize = 6;
    const MAX_DIRS: usize = 2_000;
    const MAX_FILES: usize = 20_000;
    scan_cjk_roots_bounded(roots, MAX_DEPTH, MAX_DIRS, MAX_FILES)
}

fn scan_cjk_roots_bounded(roots: &[std::path::PathBuf], max_depth: usize, max_dirs: usize, max_files: usize) -> CjkRootIndex {
    // Bound the entries read from read_dir across the whole scan, so an oversized directory cannot
    // grow the buffers without limit. Which entries survive the cut follows the filesystem's
    // listing order, not the sorted order.
    let max_entries = max_files.saturating_add(max_dirs);

    let targets = all_cjk_linux_filenames();
    let mut files: std::collections::HashMap<&'static str, Vec<std::path::PathBuf>> = std::collections::HashMap::new();
    let mut summary = CjkRootScanSummary::default();
    if targets.is_empty() || roots.is_empty() {
        return CjkRootIndex { files, summary };
    }

    // Canonical directory -> (shallowest depth visited, direct subdirectory names discovered on
    // first visit). Revisiting at a strictly shallower depth re-enqueues only cached subdirectories
    // without re-reading the directory or double-counting against the directory/entry/file caps.
    let mut seen: std::collections::HashMap<std::path::PathBuf, (usize, Vec<std::ffi::OsString>)> = std::collections::HashMap::new();
    let mut skipped_dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut dirs_visited = 0usize;
    let mut files_seen = 0usize;
    let mut entries_seen = 0usize;

    for root in roots {
        if dirs_visited >= max_dirs || files_seen >= max_files || entries_seen >= max_entries {
            // Not `exists()`: it also hides errors other than NotFound, which must count as skipped.
            if !matches!(std::fs::metadata(root), Err(ref e) if e.kind() == std::io::ErrorKind::NotFound) {
                skipped_dirs.push(root.clone());
            }
            continue;
        }
        let mut queue = std::collections::VecDeque::from([(root.clone(), 0usize)]);
        let mut queued_depth: std::collections::HashMap<std::path::PathBuf, usize> = std::collections::HashMap::new();
        while let Some((dir, depth)) = queue.pop_front() {
            if depth > max_depth || dirs_visited >= max_dirs || files_seen >= max_files || entries_seen >= max_entries {
                skipped_dirs.push(dir);
                skipped_dirs.extend(queue.drain(..).map(|(p, _)| p));
                break;
            }
            let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
            // Revisit only when reached at a strictly shallower depth so deep symlinks cannot shadow shallow paths.
            if let Some((prev_depth, child_subdirs)) = seen.get_mut(&canonical) {
                if *prev_depth <= depth {
                    continue;
                }
                *prev_depth = depth;
                let names = child_subdirs.clone();
                queued_depth.entry(canonical).and_modify(|d| *d = (*d).min(depth)).or_insert(depth);
                for name in names {
                    let child = dir.join(name);
                    let next_depth = depth + 1;
                    if next_depth > max_depth {
                        skipped_dirs.push(child);
                        continue;
                    }
                    let child_canonical = std::fs::canonicalize(&child).unwrap_or_else(|_| child.clone());
                    if seen.get(&child_canonical).is_some_and(|&(prev, _)| prev <= next_depth)
                        || queued_depth.get(&child_canonical).is_some_and(|&prev| prev <= next_depth)
                    {
                        continue;
                    }
                    // Revisited descendants already in `seen` do not consume `dirs_visited`, so cap
                    // the revisit queue by `max_dirs` rather than the remaining first-visit budget.
                    if queue.len() < max_dirs {
                        queued_depth.insert(child_canonical, next_depth);
                        queue.push_back((child, next_depth));
                    } else {
                        skipped_dirs.push(child);
                    }
                }
                continue;
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(err) => {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        summary.unreadable_dirs += 1;
                        summary.sample_path.get_or_insert_with(|| dir.clone());
                    }
                    continue;
                }
            };
            queued_depth.entry(canonical.clone()).and_modify(|d| *d = (*d).min(depth)).or_insert(depth);
            dirs_visited += 1;

            let mut children = Vec::new();
            let mut entry_cap_hit = false;
            for entry in entries {
                match entry {
                    Ok(e) => {
                        if entries_seen >= max_entries {
                            entry_cap_hit = true;
                            break;
                        }
                        entries_seen += 1;
                        children.push(e.path());
                    }
                    Err(_) => {
                        summary.entry_errors += 1;
                        summary.sample_path.get_or_insert_with(|| dir.clone());
                    }
                }
            }
            children.sort();

            let mut child_subdirs = Vec::new();
            let mut dir_truncated = entry_cap_hit;
            let mut iter = children.into_iter();
            while let Some(path) = iter.next() {
                match std::fs::metadata(&path) {
                    Ok(meta) if meta.is_dir() => {
                        if let Some(name) = path.file_name() {
                            child_subdirs.push(name.to_os_string());
                        }
                        let next_depth = depth + 1;
                        if next_depth > max_depth {
                            skipped_dirs.push(path);
                            continue;
                        }
                        let child_canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                        if seen.get(&child_canonical).is_some_and(|&(prev, _)| prev <= next_depth)
                            || queued_depth.get(&child_canonical).is_some_and(|&prev| prev <= next_depth)
                        {
                            continue;
                        }
                        if queue.len() < max_dirs.saturating_sub(dirs_visited) {
                            queued_depth.insert(child_canonical, next_depth);
                            queue.push_back((path, next_depth));
                        } else {
                            skipped_dirs.push(path);
                        }
                    }
                    Ok(meta) if meta.is_file() => {
                        if files_seen >= max_files {
                            dir_truncated = true;
                            for remaining in iter {
                                match std::fs::metadata(&remaining) {
                                    Ok(meta) if meta.is_dir() => skipped_dirs.push(remaining),
                                    Ok(_) => {}
                                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                                    Err(_) => {
                                        summary.entry_errors += 1;
                                        summary.sample_path.get_or_insert(remaining);
                                    }
                                }
                            }
                            break;
                        }
                        files_seen += 1;
                        if let Some(name) = path.file_name().and_then(|n| n.to_str())
                            && let Some(&target_name) = targets.iter().find(|&&t| t == name)
                        {
                            let bucket = files.entry(target_name).or_default();
                            if !bucket.contains(&path) {
                                bucket.push(path);
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        if err.kind() != std::io::ErrorKind::NotFound {
                            summary.entry_errors += 1;
                            summary.sample_path.get_or_insert(path);
                        }
                    }
                }
            }
            seen.insert(canonical, (depth, child_subdirs));
            if dir_truncated {
                summary.truncated_dirs += 1;
                summary.sample_path.get_or_insert_with(|| dir.clone());
            }
        }
    }

    // Count only skipped directories that were not subsequently visited through a shallower path.
    let mut skipped_seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    for path in skipped_dirs {
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.contains_key(&canonical) && skipped_seen.insert(canonical) {
            summary.truncated_dirs += 1;
            summary.sample_path.get_or_insert(path);
        }
    }

    CjkRootIndex { files, summary }
}

struct CjkScanState {
    async_enabled: std::sync::atomic::AtomicBool,
    started: std::sync::atomic::AtomicBool,
    done: std::sync::atomic::AtomicBool,
    notice_needed: std::sync::atomic::AtomicBool,
    notice_taken: std::sync::atomic::AtomicBool,
    index: OnceLock<CjkRootIndex>,
    fixed: [OnceLock<Option<(Vec<u8>, u32)>>; 4],
    fallback: [OnceLock<Option<(Vec<u8>, u32)>>; 4],
}

enum CjkResolveResult {
    Pending,
    Resolved(Option<&'static (Vec<u8>, u32)>),
}

impl CjkScanState {
    const fn new() -> Self {
        Self {
            async_enabled: std::sync::atomic::AtomicBool::new(false),
            started: std::sync::atomic::AtomicBool::new(false),
            done: std::sync::atomic::AtomicBool::new(false),
            notice_needed: std::sync::atomic::AtomicBool::new(false),
            notice_taken: std::sync::atomic::AtomicBool::new(false),
            index: OnceLock::new(),
            fixed: [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
            fallback: [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
        }
    }

    fn slot(region: CjkRegion) -> usize {
        match region {
            CjkRegion::SimplifiedChinese => 0,
            CjkRegion::Japanese => 1,
            CjkRegion::Korean => 2,
            CjkRegion::TraditionalChinese => 3,
        }
    }

    fn enable_async(&self) {
        self.async_enabled.store(true, std::sync::atomic::Ordering::Release);
    }

    fn is_pending(&self) -> bool {
        self.started.load(std::sync::atomic::Ordering::Acquire) && !self.done.load(std::sync::atomic::Ordering::Acquire)
    }

    fn is_ready(&self) -> bool {
        self.done.load(std::sync::atomic::Ordering::Acquire)
    }

    fn take_summary(&self) -> Option<CjkRootScanSummary> {
        if !self.notice_needed.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        if self.notice_taken.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        self.index.get().filter(|idx| !idx.summary.is_clean()).map(|idx| idx.summary.clone())
    }

    fn publish_done(&self) {
        use std::sync::atomic::Ordering;
        // Publish `done` before advancing `FONT_GENERATION` so any reader observing the new
        // generation resolves against the completed scan (and disk cache) rather than `Pending`.
        if !self.done.swap(true, Ordering::AcqRel) {
            FONT_GENERATION.fetch_add(1, Ordering::Release);
        }
    }

    #[cfg(any(not(target_arch = "wasm32"), test))]
    fn record_spawn_refused(&self) {
        let _ = self.index.set(CjkRootIndex {
            files: std::collections::HashMap::new(),
            summary: CjkRootScanSummary { thread_spawn_failed: true, ..Default::default() },
        });
        self.notice_needed.store(true, std::sync::atomic::Ordering::Release);
        self.publish_done();
    }

    /// Starts the bounded Linux CJK folder walk on a background thread once. Returns `true`
    /// while the scan is running off-thread so UI callers return `None` without blocking.
    /// If the OS refuses to spawn the thread, records a diagnostic summary and marks the scan done
    /// without running the folder walk on the UI thread.
    fn start_async_scan(&'static self, scan: impl Fn() -> CjkRootIndex + Clone + Send + 'static) -> bool {
        use std::sync::atomic::Ordering;
        if self.done.load(Ordering::Acquire) || self.index.get().is_some() {
            return false;
        }
        if self.started.swap(true, Ordering::AcqRel) {
            return !self.done.load(Ordering::Acquire) && self.index.get().is_none();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            struct FinishOnDrop(&'static CjkScanState);
            impl Drop for FinishOnDrop {
                fn drop(&mut self) {
                    self.0.publish_done();
                }
            }
            if std::thread::Builder::new()
                .name("cjk-font-scan".into())
                .spawn(move || {
                    let _done = FinishOnDrop(self);
                    let index = self.index.get_or_init(scan);
                    if !index.summary.is_clean() {
                        self.notice_needed.store(true, Ordering::Release);
                    }
                })
                .is_ok()
            {
                return true;
            }
            self.record_spawn_refused();
            false
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = scan;
            let _ = self.index.set(CjkRootIndex { files: std::collections::HashMap::new(), summary: CjkRootScanSummary::default() });
            self.publish_done();
            false
        }
    }

    fn resolve_region(
        &'static self,
        region: CjkRegion,
        find_fixed: impl FnOnce(CjkRegion) -> Option<(Vec<u8>, u32)>,
        use_root_scan: bool,
        scan: impl Fn() -> CjkRootIndex + Clone + Send + 'static,
        find_cached: impl FnOnce(CjkRegion) -> Option<(Vec<u8>, u32)>,
    ) -> CjkResolveResult {
        let slot = Self::slot(region);
        let Some(fixed_slot) = self.fixed.get(slot) else {
            return CjkResolveResult::Resolved(None);
        };
        if let Some(found) = fixed_slot.get_or_init(|| find_fixed(region)).as_ref() {
            return CjkResolveResult::Resolved(Some(found));
        }
        if use_root_scan
            && !BLOCKING_CJK_SCAN.get()
            && self.async_enabled.load(std::sync::atomic::Ordering::Acquire)
            && self.start_async_scan(scan.clone())
        {
            PENDING_CJK_HIT.set(true);
            return CjkResolveResult::Pending;
        }
        let Some(fallback_slot) = self.fallback.get(slot) else {
            return CjkResolveResult::Resolved(None);
        };
        let found = fallback_slot
            .get_or_init(|| {
                let scanned = if use_root_scan {
                    let index = self.index.get_or_init(scan);
                    if !index.summary.is_clean() {
                        self.notice_needed.store(true, std::sync::atomic::Ordering::Release);
                    }
                    if self.async_enabled.load(std::sync::atomic::Ordering::Acquire) {
                        self.publish_done();
                    }
                    find_cjk_in_index(&index.files, region).map(|(_, bytes, index)| (bytes, index))
                } else {
                    None
                };
                scanned.or_else(|| find_cached(region))
            })
            .as_ref();
        CjkResolveResult::Resolved(found)
    }
}

thread_local! {
    /// Set by background export workers ([`with_blocking_cjk_scan`]) so exports wait for any
    /// in-flight Linux CJK folder scan rather than omitting glyphs.
    static BLOCKING_CJK_SCAN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set when [`CjkScanState::resolve_region`] returns `Pending` on this thread, so a
    /// synchronous non-blocking export can warn if a CJK lookup was still waiting on the scan.
    static PENDING_CJK_HIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with blocking Linux CJK folder resolution on the current thread. Intended for
/// background export workers (off the UI thread) so exports wait for an in-flight scan and can
/// use discovered or disk-cached faces instead of returning `Pending`.
pub fn with_blocking_cjk_scan<T>(f: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            BLOCKING_CJK_SCAN.set(self.0);
        }
    }
    let prev = BLOCKING_CJK_SCAN.replace(true);
    let _reset = Reset(prev);
    f()
}

/// Runs `f` and reports whether any CJK lookup on this thread returned `Pending` while an async
/// Linux CJK folder scan was running, so synchronous callers never silently emit missing glyphs.
pub fn track_pending_cjk<T>(f: impl FnOnce() -> T) -> (T, bool) {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            PENDING_CJK_HIT.set(self.0 || PENDING_CJK_HIT.get());
        }
    }
    let prev = PENDING_CJK_HIT.replace(false);
    let _reset = Reset(prev);
    let out = f();
    let hit = PENDING_CJK_HIT.get();
    (out, hit)
}

static CJK_SCAN: CjkScanState = CjkScanState::new();

/// Enables non-blocking background CJK folder scanning for UI sessions (`crate::families::warm_up`).
/// Does not start a scan eagerly; a thread is spawned only if a CJK lookup misses fixed paths.
pub(crate) fn enable_async_cjk_scan() {
    CJK_SCAN.enable_async();
}

/// Whether the background Linux CJK folder fallback scan is currently running off the UI thread.
pub fn cjk_roots_pending() -> bool {
    CJK_SCAN.is_pending()
}

/// Whether a background Linux CJK folder fallback scan has finished and its index is ready.
pub fn cjk_roots_ready() -> bool {
    CJK_SCAN.is_ready()
}

/// Returns truncation or directory-read diagnostics once if a CJK font lookup actually fell back
/// to the bounded recursive scan and encountered limits or errors. Never starts a scan itself.
pub fn take_cjk_scan_summary() -> Option<CjkRootScanSummary> {
    CJK_SCAN.take_summary()
}

/// Selects the first usable CJK font from a scanned root index in `region`'s preference order.
fn find_cjk_in_index(
    indexed_files: &std::collections::HashMap<&'static str, Vec<std::path::PathBuf>>,
    region: CjkRegion,
) -> Option<(std::path::PathBuf, Vec<u8>, u32)> {
    for (target_name, index) in cjk_linux_filenames(region) {
        if let Some(paths) = indexed_files.get(target_name) {
            for path in paths {
                if let Some(bytes) = read_cjk_path(path).filter(|b| draws_cjk(b, index, region)) {
                    return Some((path.clone(), bytes, index));
                }
            }
        }
    }
    None
}

/// Installed CJK collections whose face is found by family name, not by index: (path, family).
fn cjk_named_candidates(region: CjkRegion) -> Vec<(std::path::PathBuf, &'static str)> {
    in_apple_dirs(apple_cjk_named_files(region)).collect()
}

/// A CJK font file's bytes. CJK collections are large; accept up to 128 MiB.
fn read_cjk_path(path: &std::path::Path) -> Option<Vec<u8>> {
    read_bounded(path, 128 << 20)
}

/// The regular face of `family` in a font file or collection: the face whose family name is
/// `family` and whose style is Regular. Other weights name themselves differently
/// ("PingFang TC Medium").
fn regular_face(bytes: &[u8], family: &str) -> Option<u32> {
    use ttf_parser::name_id::{FAMILY, SUBFAMILY};
    let faces = ttf_parser::fonts_in_collection(bytes).unwrap_or(1).min(256);
    (0..faces).find(|&i| {
        ttf_parser::Face::parse(bytes, i).is_ok_and(|face| {
            let named = |id: u16, want: &str| face.names().into_iter().any(|n| n.name_id == id && n.to_string().as_deref() == Some(want));
            named(FAMILY, family) && named(SUBFAMILY, "Regular")
        })
    })
}

fn find_fixed_cjk(region: CjkRegion) -> Option<(Vec<u8>, u32)> {
    cjk_paths(region)
        .iter()
        .find_map(|(path, index)| read_cjk_path(path).filter(|b| draws_cjk(b, *index, region)).map(|b| (b, *index)))
        .or_else(|| {
            cjk_named_candidates(region).iter().find_map(|(path, family)| {
                let bytes = read_cjk_path(path)?;
                let index = regular_face(&bytes, family).filter(|i| draws_cjk(&bytes, *i, region))?;
                Some((bytes, index))
            })
        })
}

/// An installed CJK font for `region` (bytes and face index), found once. `None` on the web,
/// while an async Linux folder fallback scan is in flight, or when nothing suitable is installed.
/// Traditional Chinese falls back to the Simplified face, which has the Traditional characters in
/// mainland China's glyph forms.
pub fn cjk_font(region: CjkRegion) -> Option<&'static (Vec<u8>, u32)> {
    if std::env::var_os("RUSTGIS_BUILTIN_FONTS").is_some() {
        return None;
    }
    let found = CJK_SCAN.resolve_region(
        region,
        find_fixed_cjk,
        cfg!(target_os = "linux"),
        || scan_cjk_roots(&linux_font_roots()),
        |r| cached_script_font(ScriptFont::Cjk(r)),
    );
    match (found, region) {
        (CjkResolveResult::Pending, _) => None,
        (CjkResolveResult::Resolved(None), CjkRegion::TraditionalChinese) => cjk_font(CjkRegion::SimplifiedChinese),
        (CjkResolveResult::Resolved(found), _) => found,
    }
}

/// Whether face `index` of `bytes` draws an outline for a character of `region`'s script. A face
/// that parses but whose outlines the parser can't read (iOS's PingFangUI keeps them in an `hvgl`
/// table) would draw blanks and hide the candidates after it.
fn draws_cjk(bytes: &[u8], index: u32, region: CjkRegion) -> bool {
    use ab_glyph::Font;
    let c = match region {
        CjkRegion::SimplifiedChinese => '\u{4E2D}',  // 中
        CjkRegion::TraditionalChinese => '\u{9AD4}', // 體
        CjkRegion::Japanese => '\u{3042}',           // あ
        CjkRegion::Korean => '\u{D55C}',             // 한
    };
    ab_glyph::FontRef::try_from_slice_and_index(bytes, index).is_ok_and(|f| {
        let id = f.glyph_id(c);
        id.0 != 0 && f.outline(id).is_some()
    })
}

/// The CJK faces the web app downloaded for its interface or its labels, by region: the web app
/// has no installed fonts, so without these the renderer (exported images, PDFs and the layout
/// view) would leave CJK text out. Each region's face is kept once and never freed.
static DOWNLOADED_CJK: [OnceLock<(Vec<u8>, u32)>; 4] = [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];

/// Changes each time the faces text is drawn in change: a face handed over at run time
/// ([`add_downloaded_cjk_font`]) or another preferred one ([`prefer_downloaded_cjk`]). Cached text
/// widths measured before are stale.
static FONT_GENERATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The downloaded face ideographs are drawn in first ([`prefer_downloaded_cjk`]), as a slot number
/// one above its index (0 for none).
static PREFERRED_CJK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The CJK scripts the renderer has met with no face to draw them, one bit each
/// ([`note_missing_cjk`]), until the interface takes them ([`take_missing_cjk`]).
static MISSING_CJK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The CJK scripts, by the regional face each calls for: Hangul a Korean face, kana a Japanese
/// one, ideographs (and CJK punctuation, full-width forms and bopomofo) a Chinese one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CjkScript {
    Han,
    Kana,
    Hangul,
}

impl CjkScript {
    /// The CJK script of `c`, `None` for any other character.
    pub fn of(c: char) -> Option<CjkScript> {
        match c {
            '\u{1100}'..='\u{11FF}'
            | '\u{3130}'..='\u{318F}'
            | '\u{A960}'..='\u{A97F}'
            | '\u{AC00}'..='\u{D7FF}'
            | '\u{FFA0}'..='\u{FFDC}' => Some(CjkScript::Hangul),
            '\u{3040}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{FF66}'..='\u{FF9F}' => Some(CjkScript::Kana),
            '\u{2E80}'..='\u{2FDF}'
            | '\u{3000}'..='\u{303F}'
            | '\u{3100}'..='\u{312F}'
            | '\u{31A0}'..='\u{31BF}'
            | '\u{31C0}'..='\u{31EF}'
            | '\u{3200}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE30}'..='\u{FE4F}'
            | '\u{FF00}'..='\u{FFEF}'
            | '\u{20000}'..='\u{3FFFF}' => Some(CjkScript::Han),
            _ => None,
        }
    }

    fn bit(self) -> u8 {
        match self {
            CjkScript::Han => 1,
            CjkScript::Kana => 2,
            CjkScript::Hangul => 4,
        }
    }
}

/// Records that `c`, a character the renderer is drawing or measuring, has no face to draw it in,
/// when it is CJK: the web app, which has no installed fonts, then downloads a face for it
/// ([`take_missing_cjk`]). Other scripts are left alone.
pub fn note_missing_cjk(c: char) {
    if let Some(script) = CjkScript::of(c) {
        MISSING_CJK.fetch_or(script.bit(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// The CJK scripts the renderer has met with no face since the last call, cleared: the text a
/// map's labels, a layout or an export needed, whatever the interface language.
pub fn take_missing_cjk() -> Vec<CjkScript> {
    scripts(MISSING_CJK.swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// The CJK scripts noted since the last [`take_missing_cjk`], left noted: the interface asks for
/// another frame when one is new to it, as the next frame takes them.
pub fn peek_missing_cjk() -> Vec<CjkScript> {
    scripts(MISSING_CJK.load(std::sync::atomic::Ordering::Relaxed))
}

fn scripts(bits: u8) -> Vec<CjkScript> {
    [CjkScript::Han, CjkScript::Kana, CjkScript::Hangul].into_iter().filter(|s| bits & s.bit() != 0).collect()
}

/// Draws ideographs in the face downloaded for `region` first, when there is one, ahead of the
/// regional order the renderer otherwise follows (the web app passes its interface's region, so a
/// Traditional Chinese interface draws them in Traditional Chinese forms even after a Simplified
/// Chinese face was downloaded earlier). Installed faces aren't affected.
pub fn prefer_downloaded_cjk(region: Option<CjkRegion>) {
    let slot = region.map_or(0, |r| match r {
        CjkRegion::SimplifiedChinese => 1,
        CjkRegion::Japanese => 2,
        CjkRegion::Korean => 3,
        CjkRegion::TraditionalChinese => 4,
    });
    if PREFERRED_CJK.swap(slot, std::sync::atomic::Ordering::Relaxed) != slot {
        FONT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
    }
}

/// The region [`prefer_downloaded_cjk`] last set.
pub fn preferred_downloaded_cjk() -> Option<CjkRegion> {
    match PREFERRED_CJK.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Some(CjkRegion::SimplifiedChinese),
        2 => Some(CjkRegion::Japanese),
        3 => Some(CjkRegion::Korean),
        4 => Some(CjkRegion::TraditionalChinese),
        _ => None,
    }
}

fn downloaded_slot(region: CjkRegion) -> &'static OnceLock<(Vec<u8>, u32)> {
    match region {
        CjkRegion::SimplifiedChinese => &DOWNLOADED_CJK[0],
        CjkRegion::Japanese => &DOWNLOADED_CJK[1],
        CjkRegion::Korean => &DOWNLOADED_CJK[2],
        CjkRegion::TraditionalChinese => &DOWNLOADED_CJK[3],
    }
}

/// Hands the renderer a CJK face the web app downloaded (face 0 of `bytes`), for the text it
/// draws where no font is installed. The face is kept for good, and the kept bytes are returned
/// so the interface can share them instead of holding a second copy of a font several megabytes
/// in size. The bytes come back unkept when they aren't a usable font or the region has a face
/// already.
pub fn add_downloaded_cjk_font(region: CjkRegion, bytes: Vec<u8>) -> Result<&'static [u8], Vec<u8>> {
    if !valid_font(&bytes, 0) {
        return Err(bytes);
    }
    let mut offered = Some(bytes);
    let kept = downloaded_slot(region).get_or_init(|| (offered.take().unwrap_or_default(), 0));
    if let Some(bytes) = offered {
        return Err(bytes);
    }
    FONT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
    Ok(&kept.0)
}

/// The CJK face downloaded for `region`, if one has been handed over.
pub fn downloaded_cjk_font(region: CjkRegion) -> Option<&'static (Vec<u8>, u32)> {
    downloaded_slot(region).get()
}

/// Changes each time the faces text is drawn in change (a face handed over at run time, another
/// preferred one): a key part for cached text widths.
pub fn font_generation() -> u32 {
    FONT_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// Whether `bytes` hold a font with face `index` that the font parser accepts. Fonts handed to the
/// UI are checked first: a corrupt file or an out-of-range face index must never reach it.
pub fn valid_font(bytes: &[u8], index: u32) -> bool {
    ab_glyph::FontRef::try_from_slice_and_index(bytes, index).is_ok()
}

/// Where the web app downloads a CJK face from (Noto Sans, SIL Open Font License).
pub fn cjk_font_url(region: CjkRegion) -> &'static str {
    match region {
        CjkRegion::SimplifiedChinese => "https://assets.rustgis.app/fonts/NotoSansSC-Regular.otf",
        CjkRegion::TraditionalChinese => "https://assets.rustgis.app/fonts/NotoSansTC-Regular.otf",
        CjkRegion::Japanese => "https://assets.rustgis.app/fonts/NotoSansJP-Regular.otf",
        CjkRegion::Korean => "https://assets.rustgis.app/fonts/NotoSansKR-Regular.otf",
    }
}

/// A script the interface fonts may lack, and that an interface language needs a face for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScriptFont {
    /// Extended Latin, including Azerbaijani and Vietnamese letters absent from the bundled face.
    Latin,
    Cjk(CjkRegion),
    /// Arabic script (Persian). The face has Latin letters too: egui shapes each run of one face
    /// on its own, so a line of Persian with a Latin word in it reads in order only when one face
    /// draws the whole line.
    Arabic,
    /// Bengali glyph coverage only; the current text stack does not shape Indic conjuncts.
    Bengali,
    /// Lao letters, drawn after the interface face (which keeps drawing Latin words). Vowel and
    /// tone marks are placed by the face's default offsets; there is no mark positioning.
    Lao,
}

const BENGALI_CANDIDATES: &[&str] = &[
    "/system/fonts/NotoSansBengali-VF.ttf",
    "/system/fonts/NotoSansBengali-Regular.ttf",
    "/system/fonts/NotoSansBengaliUI-Regular.ttf",
    "C:\\Windows\\Fonts\\siyamrupali.ttf",
    "C:\\Windows\\Fonts\\Nirmala.ttf",
    "/System/Library/Fonts/Supplemental/NotoSansBengali-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansBengali-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansBengaliUI-Regular.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansBengali-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansBengali-Regular.ttf",
];

/// Bengali fallback faces need Bengali outlines; Latin comes from the preceding UI font.
fn covers_bengali_at(bytes: &[u8], index: u32) -> bool {
    outlines(bytes, index, &['\u{0985}', '\u{09be}'])
}

/// A cmap entry alone is insufficient: some phone fonts expose glyph ids but no readable outlines.
fn outlines(bytes: &[u8], index: u32, chars: &[char]) -> bool {
    use ab_glyph::Font;
    ab_glyph::FontRef::try_from_slice_and_index(bytes, index).is_ok_and(|f| {
        chars.iter().all(|c| {
            let id = f.glyph_id(*c);
            id.0 != 0 && f.outline(id).is_some()
        })
    })
}

/// Installed faces with both Arabic and Latin letters (checked by [`covers_arabic`]).
const ARABIC_CANDIDATES: &[&str] = &[
    "C:\\Windows\\Fonts\\segoeui.ttf",
    "C:\\Windows\\Fonts\\tahoma.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/System/Library/Fonts/Supplemental/Tahoma.ttf",
    "/usr/share/fonts/TTF/Vazirmatn-Regular.ttf",
    "/usr/share/fonts/truetype/vazirmatn/Vazirmatn-Regular.ttf",
    "/usr/share/fonts/vazirmatn-fonts/Vazirmatn-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
];

/// Installed faces with Lao letters (checked by [`covers_lao`]).
const LAO_CANDIDATES: &[&str] = &[
    // Windows 10/11 (Leelawadee UI; plain Leelawadee has Thai only).
    "/system/fonts/NotoSansLao-Regular.ttf",
    "/system/fonts/NotoSansLaoUI-Regular.ttf",
    "C:\\Windows\\Fonts\\LeelawUI.ttf",
    "C:\\Windows\\Fonts\\DokChamp.ttf",
    "/System/Library/Fonts/Supplemental/Lao Sangam MN.ttf",
    "/System/Library/Fonts/Supplemental/Lao MN.ttc",
    "/usr/share/fonts/noto/NotoSansLao-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansLao-Regular.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansLao-Regular.otf",
    "/usr/share/fonts/google-noto/NotoSansLao-Regular.ttf",
    "/usr/share/fonts/google-noto-vf/NotoSansLao[wdth,wght].ttf",
    "/usr/share/fonts/TTF/NotoSansLao-Regular.ttf",
    "/usr/share/fonts/truetype/lao/Phetsarath_OT.ttf",
    // DejaVu Sans draws Lao too, where no Lao face is installed.
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
];

/// Whether a face draws Persian letters (farsi yeh, gaf) and Latin ones.
fn covers_arabic(bytes: &[u8], index: u32) -> bool {
    outlines(bytes, index, &['\u{6CC}', '\u{6AF}', 'A', 'z'])
}

/// Whether a face has readable Lao outlines (letters, a vowel and a tone mark).
fn covers_lao(bytes: &[u8], index: u32) -> bool {
    outlines(bytes, index, &['\u{0E81}', '\u{0E82}', '\u{0EB2}', '\u{0EC8}'])
}

/// The existing script candidates in every Apple font folder (iOS uses subfolders),
/// Android system candidates in manufacturer folders, and Linux Flatpak/XDG font roots.
/// Keep the listed paths first.
fn script_paths(candidates: &[&str]) -> Vec<std::path::PathBuf> {
    script_paths_with(candidates, &linux_font_roots())
}

fn script_paths_with(candidates: &[&str], roots: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = candidates.iter().map(std::path::PathBuf::from).collect();
    for path in candidates {
        if let Some(file) = path.strip_prefix("/System/Library/Fonts/Supplemental/").or_else(|| path.strip_prefix("/System/Library/Fonts/"))
        {
            paths.extend(in_apple_dirs(&[(file, ())]).map(|(p, _)| p));
        } else if let Some(file) = path.strip_prefix("/system/fonts/") {
            paths.extend(ANDROID_FONT_DIRS.iter().map(|dir| std::path::PathBuf::from(dir).join(file)));
        } else if let Some(rel) = path.strip_prefix(LINUX_SYSTEM_FONTS) {
            paths.extend(extra_linux_roots(roots).map(|root| root.join(rel)));
        }
    }
    paths
}

/// Reads only bounded files, then finds a face with real outlines for the script. A collection
/// may have its usable face after index zero; an unsupported outline format must be skipped.
fn find_script(candidates: &[&str], covers: fn(&[u8], u32) -> bool) -> Option<(Vec<u8>, u32)> {
    script_paths(candidates).iter().find_map(|path| {
        let bytes = read_bounded(path, MAX_FONT_BYTES as u64)?;
        let count = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1).min(256);
        let index = (0..count).find(|i| covers(&bytes, *i))?;
        Some((bytes, index))
    })
}

/// An installed face for `script` (bytes and face index), found once. `None` on the web or when
/// nothing suitable is installed.
pub fn script_font(script: ScriptFont) -> Option<&'static (Vec<u8>, u32)> {
    if std::env::var_os("RUSTGIS_BUILTIN_FONTS").is_some() {
        return None;
    }
    match script {
        ScriptFont::Latin => {
            static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
            FOUND
                .get_or_init(|| {
                    let paths: Vec<&str> = CANDIDATES.iter().map(|(regular, _)| *regular).collect();
                    find_script(&paths, |b, i| outlines(b, i, &['ə', 'ệ'])).or_else(|| cached_script_font(script))
                })
                .as_ref()
        }
        ScriptFont::Cjk(region) => cjk_font(region),
        ScriptFont::Arabic => {
            static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
            FOUND.get_or_init(|| find_script(ARABIC_CANDIDATES, covers_arabic).or_else(|| cached_script_font(script))).as_ref()
        }
        ScriptFont::Bengali => {
            static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
            FOUND.get_or_init(|| find_script(BENGALI_CANDIDATES, covers_bengali_at).or_else(|| cached_script_font(script))).as_ref()
        }
        ScriptFont::Lao => {
            static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
            FOUND.get_or_init(|| find_script(LAO_CANDIDATES, covers_lao).or_else(|| cached_script_font(script))).as_ref()
        }
    }
}

/// Installed faces with the symbols the interface marks things with (checked by
/// [`covers_symbols`]): the interface faces lack some, Segoe UI the check mark among them.
const SYMBOL_CANDIDATES: &[&str] = &[
    "/system/fonts/NotoSansSymbols-Regular-Subsetted.ttf",
    "/system/fonts/NotoSansSymbols2-Regular.ttf",
    "C:\\Windows\\Fonts\\seguisym.ttf",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
];

/// Whether a face draws the check and cross marks.
fn covers_symbols(bytes: &[u8], index: u32) -> bool {
    outlines(bytes, index, &['\u{2713}', '\u{2717}'])
}

/// An installed face for symbols the interface faces may lack (bytes and face index), found
/// once. `None` on the web or when nothing suitable is installed.
pub fn symbol_font() -> Option<&'static (Vec<u8>, u32)> {
    if std::env::var_os("RUSTGIS_BUILTIN_FONTS").is_some() {
        return None;
    }
    static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
    FOUND.get_or_init(|| find_script(SYMBOL_CANDIDATES, covers_symbols)).as_ref()
}

/// Native headless callers use the same verified cache as the GUI; a CLI/Python process need
/// not have a UI inbox to reuse a font downloaded by an earlier run. No network I/O occurs here.
fn cached_script_font(script: ScriptFont) -> Option<(Vec<u8>, u32)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let file = script_font_url(script)?.rsplit('/').next()?;
        let path = rustgis_data::dirs::cache_dir()?.join("fonts").join(file);
        let bytes = read_bounded(path, MAX_FONT_BYTES as u64)?;
        draws_script(&bytes, 0, script).then_some((bytes, 0))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = script;
        None
    }
}

/// Supported fallback scripts, shared by URL matching, demand loading and renderer fallback.
impl ScriptFont {
    pub const ALL: [Self; 8] = [
        Self::Latin,
        Self::Arabic,
        Self::Bengali,
        Self::Lao,
        Self::Cjk(CjkRegion::SimplifiedChinese),
        Self::Cjk(CjkRegion::TraditionalChinese),
        Self::Cjk(CjkRegion::Japanese),
        Self::Cjk(CjkRegion::Korean),
    ];

    /// The fallback for a missing character in one of the supported interface scripts.
    pub fn of(c: char) -> Option<Self> {
        match CjkScript::of(c) {
            Some(CjkScript::Han) => Some(Self::Cjk(CjkRegion::SimplifiedChinese)),
            Some(CjkScript::Kana) => Some(Self::Cjk(CjkRegion::Japanese)),
            Some(CjkScript::Hangul) => Some(Self::Cjk(CjkRegion::Korean)),
            None => match c {
                '\u{0600}'..='\u{06ff}'
                | '\u{0750}'..='\u{077f}'
                | '\u{08a0}'..='\u{08ff}'
                | '\u{fb50}'..='\u{fdff}'
                | '\u{fe70}'..='\u{feff}' => Some(Self::Arabic),
                '\u{0964}'..='\u{0965}' | '\u{0980}'..='\u{09ff}' => Some(Self::Bengali),
                '\u{0e80}'..='\u{0eff}' => Some(Self::Lao),
                '\u{00a0}'..='\u{02ff}' | '\u{0300}'..='\u{036f}' | '\u{1e00}'..='\u{1eff}' => Some(Self::Latin),
                _ => None,
            },
        }
    }
}

/// A hosted fallback for a supported script. Assets are verified before these URLs are shipped.
pub fn script_font_url(script: ScriptFont) -> Option<&'static str> {
    Some(match script {
        ScriptFont::Cjk(region) => cjk_font_url(region),
        ScriptFont::Arabic => "https://assets.rustgis.app/fonts/Vazirmatn-Regular.ttf",
        ScriptFont::Latin => "https://assets.rustgis.app/fonts/NotoSans-VF.ttf",
        ScriptFont::Bengali => "https://assets.rustgis.app/fonts/NotoSansBengali-VF.ttf",
        ScriptFont::Lao => "https://assets.rustgis.app/fonts/NotoSansLao-VF.ttf",
    })
}

/// Parsing a font is not proof of usable script coverage: reject wrong-script and outline-less
/// downloads before they can replace a working fallback or activate a translated interface.
pub fn draws_script(bytes: &[u8], index: u32, script: ScriptFont) -> bool {
    match script {
        ScriptFont::Cjk(region) => draws_cjk(bytes, index, region),
        ScriptFont::Arabic => covers_arabic(bytes, index),
        ScriptFont::Bengali => covers_bengali_at(bytes, index),
        ScriptFont::Lao => covers_lao(bytes, index),
        ScriptFont::Latin => outlines(bytes, index, &['ə', 'ệ']),
    }
}

/// Cmap coverage for a caller's required characters. Callers check script outlines separately.
/// Used for catalog validation without depending on the UI's language registry in this crate.
pub fn has_characters(bytes: &[u8], index: u32, chars: impl IntoIterator<Item = char>) -> bool {
    use ab_glyph::Font;
    ab_glyph::FontRef::try_from_slice_and_index(bytes, index).is_ok_and(|f| chars.into_iter().all(|c| f.glyph_id(c).0 != 0))
}

static DOWNLOADED_SCRIPTS: [OnceLock<(Vec<u8>, u32)>; 4] = [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];

/// Each non-CJK script owns one shared downloaded face for the lifetime of the process.
fn script_slot(script: ScriptFont) -> Option<&'static OnceLock<(Vec<u8>, u32)>> {
    let slot = match script {
        ScriptFont::Latin => 0,
        ScriptFont::Arabic => 1,
        ScriptFont::Bengali => 2,
        ScriptFont::Lao => 3,
        ScriptFont::Cjk(_) => return None,
    };
    DOWNLOADED_SCRIPTS.get(slot)
}

/// Keeps one usable downloaded face per script, shared by the UI and every exporter. The bytes
/// are returned unkept for a bad face. A repeated request shares the existing slot, so no second
/// full font copy is held.
pub fn add_downloaded_script_font(script: ScriptFont, bytes: Vec<u8>) -> Result<&'static [u8], Vec<u8>> {
    if !draws_script(&bytes, 0, script) {
        return Err(bytes);
    }
    if let Some(kept) = downloaded_script_font(script) {
        return Ok(&kept.0);
    }
    if let ScriptFont::Cjk(region) = script {
        return match add_downloaded_cjk_font(region, bytes) {
            Ok(kept) => Ok(kept),
            Err(bytes) => match downloaded_cjk_font(region) {
                Some(kept) => Ok(&kept.0),
                None => Err(bytes),
            },
        };
    }
    let Some(slot) = script_slot(script) else { return Err(bytes) };
    let mut offered = Some(bytes);
    let kept = slot.get_or_init(|| (offered.take().unwrap_or_default(), 0));
    if offered.is_none() {
        FONT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
    }
    Ok(&kept.0)
}

/// The downloaded face, if any, without triggering disk or network I/O.
pub fn downloaded_script_font(script: ScriptFont) -> Option<&'static (Vec<u8>, u32)> {
    match script {
        ScriptFont::Cjk(region) => downloaded_cjk_font(region),
        _ => script_slot(script)?.get(),
    }
}

/// At most 64 distinct missing characters per supported script; enough to recheck after a face
/// arrives without retaining arbitrary user strings or an unbounded diagnostic corpus.
type MissingScripts = std::collections::HashMap<ScriptFont, std::collections::BTreeSet<char>>;
static MISSING_SCRIPTS: std::sync::Mutex<Option<MissingScripts>> = std::sync::Mutex::new(None);

/// Notes missing supported-script text from the UI, map labels, tile labels or character markers.
/// Work is bounded by the script registry and 64 distinct characters per script.
pub fn note_missing_script(c: char) {
    if crate::text::invisible(c) {
        return;
    }
    note_missing_cjk(c);
    if let Some(script) = ScriptFont::of(c) {
        let mut guard = MISSING_SCRIPTS.lock().unwrap_or_else(|e| e.into_inner());
        let chars = guard.get_or_insert_with(Default::default).entry(script).or_default();
        if chars.len() < 64 {
            chars.insert(c);
        }
    }
}

/// Missing characters left pending, for checking whether a newly activated face resolved them.
pub fn peek_missing_text() -> Vec<(ScriptFont, Vec<char>)> {
    let guard = MISSING_SCRIPTS.lock().unwrap_or_else(|e| e.into_inner());
    ScriptFont::ALL
        .into_iter()
        .filter_map(|script| guard.as_ref()?.get(&script).map(|chars| (script, chars.iter().copied().collect())))
        .collect()
}

/// Takes actual missing characters with their scripts. A caller can recheck these against the
/// current font stack, so a report from the frame before a font arrived does not start another
/// download or announce a loss that has already been resolved.
pub fn take_missing_text() -> Vec<(ScriptFont, Vec<char>)> {
    let mut guard = MISSING_SCRIPTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut missing = guard.take().unwrap_or_default();
    ScriptFont::ALL.into_iter().filter_map(|script| missing.remove(&script).map(|chars| (script, chars.into_iter().collect()))).collect()
}

/// Takes missing script kinds once for coverage tests that do not need representative characters.
#[cfg(test)]
pub(crate) fn take_missing_scripts() -> Vec<ScriptFont> {
    take_missing_text().into_iter().map(|(script, _)| script).collect()
}

#[cfg(test)]
mod tests {
    use super::{bold_thickening, synthetic_bold};

    /// Invisible cluster members must never become font requests or missing-text notices.
    #[test]
    fn invisible_characters_do_not_request_script_fonts() {
        for c in ['\u{00ad}', '\u{feff}'] {
            super::note_missing_script(c);
            assert!(!super::peek_missing_text().iter().any(|(_, chars)| chars.contains(&c)));
        }
    }

    /// The face a CJK character calls for, by its script: ideographs with CJK punctuation,
    /// full-width forms and bopomofo, kana (half-width too), Hangul; other scripts are no CJK.
    #[test]
    fn cjk_scripts_by_character() {
        use super::CjkScript::{self, Han, Hangul, Kana};
        let cases = [
            ('臺', Some(Han)),
            ('。', Some(Han)),
            ('Ａ', Some(Han)),
            ('ㄅ', Some(Han)),
            ('\u{2000B}', Some(Han)),
            ('か', Some(Kana)),
            ('ｶ', Some(Kana)),
            ('한', Some(Hangul)),
            ('ㄱ', Some(Hangul)),
            ('\u{FFA1}', Some(Hangul)),
            ('A', None),
            ('é', None),
            ('Ж', None),
            ('ش', None),
            ('\u{E618}', None),
        ];
        for (c, script) in cases {
            assert_eq!(CjkScript::of(c), script, "{c}");
        }
    }

    /// Bold is thickened by hand only where the bold face is the regular one, with no weight axis
    /// (the built-in face, which the browser has): not where there is a bold file.
    #[test]
    fn bold_is_thickened_only_without_a_heavier_face() {
        let ubuntu = epaint_default_fonts::UBUNTU_LIGHT;
        assert!(synthetic_bold(ubuntu, ubuntu), "one face for both");
        assert!(!synthetic_bold(ubuntu, epaint_default_fonts::HACK_REGULAR), "a bold file of its own");
        assert!(!synthetic_bold(&[], &[]), "not a font");
        assert_eq!(bold_thickening(30.0), 1.0);
        assert_eq!((bold_thickening(1.0), bold_thickening(1000.0)), (0.25, 3.0), "kept visible, and within reason");
        assert_eq!((bold_thickening(f32::NAN), bold_thickening(-4.0)), (0.0, 0.0));
    }

    #[test]
    fn supported_missing_characters_choose_bounded_script_fallbacks() {
        use super::ScriptFont;
        for (c, script) in [
            ('ə', ScriptFont::Latin),
            ('ệ', ScriptFont::Latin),
            ('ی', ScriptFont::Arabic),
            ('অ', ScriptFont::Bengali),
            ('ກ', ScriptFont::Lao),
            ('中', ScriptFont::Cjk(super::CjkRegion::SimplifiedChinese)),
            ('あ', ScriptFont::Cjk(super::CjkRegion::Japanese)),
            ('한', ScriptFont::Cjk(super::CjkRegion::Korean)),
        ] {
            assert_eq!(ScriptFont::of(c), Some(script));
            assert!(super::script_font_url(script).is_some());
        }
        assert_eq!(ScriptFont::of('A'), None);
        assert_eq!(ScriptFont::of('\u{10ffff}'), None);
        let face = include_bytes!("../testdata/test-fallback.otf");
        for script in ScriptFont::ALL {
            assert!(super::draws_script(face, 0, script), "{script:?}");
            assert!(!super::draws_script(b"not a font", 0, script));
            assert!(!super::draws_script(epaint_default_fonts::HACK_REGULAR, 0, script), "{script:?}: unrelated valid font");
        }
    }

    /// A valid script face padded beyond the regular-font budget must be skipped before reading.
    #[test]
    fn script_candidates_keep_the_regular_font_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("padded-script.otf");
        std::fs::write(&path, include_bytes!("../testdata/test-script-only.otf")).unwrap();
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(super::MAX_FONT_BYTES as u64 + 1).unwrap();
        assert!(
            super::find_script(&[path.to_str().unwrap()], super::covers_bengali_at).is_none(),
            "oversized script candidate was admitted"
        );
    }

    #[test]
    fn bengali_fallback_does_not_require_latin_glyphs() {
        assert!(!super::covers_bengali_at(epaint_default_fonts::UBUNTU_LIGHT, 0));
        assert!(!super::covers_bengali_at(b"not a font", 0));
        let face = include_bytes!("../testdata/test-script-only.otf");
        assert!(super::covers_bengali_at(face, 0));
        assert!(!super::covers_arabic(face, 0), "Persian still requires Latin for mixed-direction runs");
        assert!(super::script_font_url(super::ScriptFont::Bengali).is_some());
    }

    /// Candidate expansion follows iOS's subfolders and Android's manufacturer directories,
    /// not just the desktop paths listed for the same faces.
    #[test]
    fn script_faces_follow_mobile_font_folders() {
        use std::path::PathBuf;

        for candidates in [super::BENGALI_CANDIDATES, super::ARABIC_CANDIDATES, super::LAO_CANDIDATES, super::SYMBOL_CANDIDATES] {
            let paths = super::script_paths(candidates);
            assert!(paths.iter().any(|p| p.starts_with("/System/Library/Fonts/Core")), "{candidates:?}");
        }
        for (candidates, file) in
            [(super::BENGALI_CANDIDATES, "NotoSansBengali-VF.ttf"), (super::LAO_CANDIDATES, "NotoSansLao-Regular.ttf")]
        {
            let paths = super::script_paths(candidates);
            assert!(paths.contains(&PathBuf::from(format!("/system/fonts/{file}"))));
            assert!(paths.contains(&PathBuf::from(format!("/product/fonts/{file}"))));
        }
    }

    #[test]
    fn lao_coverage_requires_lao_glyphs() {
        assert!(!super::covers_lao(epaint_default_fonts::UBUNTU_LIGHT, 0));
        assert!(!super::covers_lao(b"not a font", 0));
        assert!(super::script_font_url(super::ScriptFont::Lao).is_some());
    }

    /// A face is found by its family and Regular style, not by its place in a collection
    /// (PingFang TC on macOS); nothing is found in a file without that family.
    #[test]
    fn regular_face_is_found_by_family_name() {
        assert_eq!(super::regular_face(epaint_default_fonts::HACK_REGULAR, "Hack"), Some(0));
        assert_eq!(super::regular_face(epaint_default_fonts::HACK_REGULAR, "PingFang TC"), None);
        assert_eq!(super::regular_face(b"not a font", "Hack"), None);
    }

    /// Every CJK region names an Apple face in macOS's font folder and in iOS's (#2153: Korean
    /// labels drew as boxes on iOS, whose fonts sit in subfolders), and Windows and Android ones; Windows
    /// faces are still tried first.
    #[test]
    fn every_cjk_region_names_a_face_on_each_platform() {
        use std::path::{Path, PathBuf};

        use super::{CjkRegion, cjk_named_candidates, cjk_paths};
        for region in [CjkRegion::SimplifiedChinese, CjkRegion::TraditionalChinese, CjkRegion::Japanese, CjkRegion::Korean] {
            let paths: Vec<PathBuf> =
                cjk_paths(region).into_iter().map(|(p, _)| p).chain(cjk_named_candidates(region).into_iter().map(|(p, _)| p)).collect();
            let in_dir = |dir: &str| paths.iter().any(|p| p.parent() == Some(Path::new(dir)));
            assert!(in_dir("/System/Library/Fonts"), "{region:?}: no macOS face");
            assert!(in_dir("/System/Library/Fonts/Core"), "{region:?}: no iOS face");
            assert!(in_dir("/system/fonts"), "{region:?}: no Android face");
            assert!(paths.first().is_some_and(|p| p.to_string_lossy().starts_with("C:\\")), "{region:?}: Windows first");
        }
        for (region, index) in
            [(CjkRegion::Japanese, 0), (CjkRegion::Korean, 1), (CjkRegion::SimplifiedChinese, 2), (CjkRegion::TraditionalChinese, 3)]
        {
            let paths = cjk_paths(region);
            for dir in super::ANDROID_FONT_DIRS {
                assert!(paths.contains(&(PathBuf::from(dir).join("NotoSansCJK-Regular.ttc"), index)), "{region:?}");
                assert!(paths.contains(&(PathBuf::from(dir).join("DroidSansFallback.ttf"), 0)), "{region:?}: legacy face");
            }
        }
        let korean = cjk_paths(CjkRegion::Korean);
        assert!(korean.iter().any(|(p, i)| p == &PathBuf::from("/System/Library/Fonts/Core/AppleSDGothicNeo.ttc") && *i == 0));
        let japanese = cjk_paths(CjkRegion::Japanese);
        assert!(japanese.iter().any(|(p, _)| p == &PathBuf::from("/System/Library/Fonts/Core/HiraginoKakuGothic.ttc")));
    }

    /// A CJK candidate is taken only when it outlines its region's script: not a face without
    /// the characters, nor junk.
    #[test]
    fn a_cjk_face_must_draw_its_script() {
        use super::{CjkRegion, draws_cjk};
        for region in [CjkRegion::SimplifiedChinese, CjkRegion::TraditionalChinese, CjkRegion::Japanese, CjkRegion::Korean] {
            assert!(!draws_cjk(epaint_default_fonts::HACK_REGULAR, 0, region), "{region:?}");
            assert!(!draws_cjk(b"not a font", 0, region), "{region:?}");
        }
        // Noto Sans CJK draws them all, where it is installed.
        for (p, i) in super::cjk_paths(CjkRegion::Korean)
            .iter()
            .filter(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some("NotoSansCJK-Regular.ttc"))
        {
            if let Ok(b) = std::fs::read(p) {
                assert!(draws_cjk(&b, *i, CjkRegion::Korean), "{}", p.display());
            }
        }
    }

    /// Traditional Chinese has a web face of its own, not the Simplified one.
    #[test]
    fn traditional_chinese_has_its_own_web_face() {
        use super::{CjkRegion, cjk_font_url};
        assert!(cjk_font_url(CjkRegion::TraditionalChinese).ends_with("/NotoSansTC-Regular.otf"));
        assert_ne!(cjk_font_url(CjkRegion::TraditionalChinese), cjk_font_url(CjkRegion::SimplifiedChinese));
    }

    /// A variable Noto Serif (one file for every weight, its italic beside it) is found in a
    /// package folder by its name's start, whatever axes the name lists.
    #[test]
    fn a_variable_noto_serif_is_found() {
        let root = std::env::temp_dir().join(format!("rustgis-noto-{}", std::process::id()));
        let dir = root.join("google-noto-serif-vf");
        std::fs::create_dir_all(&dir).unwrap();
        // Any valid face stands in for the font.
        std::fs::write(dir.join("NotoSerif[wdth,wght].ttf"), epaint_default_fonts::HACK_REGULAR).unwrap();
        std::fs::write(dir.join("NotoSerif-Italic[wdth,wght].ttf"), epaint_default_fonts::UBUNTU_LIGHT).unwrap();
        std::fs::write(dir.join("NotoSerifDisplay[wdth,wght].ttf"), epaint_default_fonts::UBUNTU_LIGHT).unwrap();
        let found = super::variable_noto_serif(&[root.to_str().unwrap(), "/no/such/folder"]);
        let _ = std::fs::remove_dir_all(&root);
        let f = found.expect("found");
        assert!(f.source.ends_with("NotoSerif[wdth,wght].ttf"), "{}", f.source);
        assert_eq!(f.regular, epaint_default_fonts::HACK_REGULAR);
        assert!(f.variable && f.bold.is_none() && f.bold_italic.is_none(), "the bold is the same file, kept once");
        assert_eq!(f.italic.as_deref(), Some(epaint_default_fonts::UBUNTU_LIGHT));
        assert!(super::variable_noto_serif(&["/no/such/folder"]).is_none());
    }

    #[test]
    fn a_symbol_face_needs_the_check_and_cross_marks() {
        // Ubuntu Light has neither; junk is no font at all.
        assert!(!super::covers_symbols(epaint_default_fonts::UBUNTU_LIGHT, 0));
        assert!(!super::covers_symbols(b"not a font", 0));
        // DejaVu Sans has both, where it is installed.
        for p in super::SYMBOL_CANDIDATES.iter().filter(|p| p.contains("DejaVu")) {
            if let Ok(b) = std::fs::read(p) {
                assert!(super::covers_symbols(&b, 0), "{p}");
            }
        }
    }

    #[test]
    fn an_arabic_face_needs_persian_and_latin_letters() {
        // Ubuntu Light has Latin but no Arabic; junk is no font at all.
        assert!(!super::covers_arabic(epaint_default_fonts::UBUNTU_LIGHT, 0));
        assert!(!super::covers_arabic(b"not a font", 0));
        // DejaVu Sans has both, where it is installed.
        for p in super::ARABIC_CANDIDATES.iter().filter(|p| p.contains("DejaVu")) {
            if let Ok(b) = std::fs::read(p) {
                assert!(super::covers_arabic(&b, 0), "{p}");
            }
        }
    }

    #[test]
    fn flatpak_and_xdg_roots_are_included_in_cjk_and_script_paths() {
        use std::ffi::OsStr;
        use std::path::PathBuf;

        let roots_xdg = super::linux_font_roots_with(Some(OsStr::new("/custom/xdg")), Some(OsStr::new("/custom/home")));
        for expected in [
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/run/host/fonts",
            "/run/host/local-fonts",
            "/run/host/user-fonts",
            "/custom/xdg/fonts",
            "/custom/home/.fonts",
        ] {
            assert!(roots_xdg.contains(&PathBuf::from(expected)), "missing {expected} in {roots_xdg:?}");
        }

        let roots_home = super::linux_font_roots_with(None, Some(OsStr::new("/custom/home")));
        assert!(roots_home.contains(&PathBuf::from("/custom/home/.local/share/fonts")), "{roots_home:?}");

        // Per XDG Base Directory spec, relative XDG_DATA_HOME is ignored and falls back to $HOME/.local/share.
        let roots_rel = super::linux_font_roots_with(Some(OsStr::new("relative/xdg")), Some(OsStr::new("/custom/home")));
        assert!(!roots_rel.contains(&PathBuf::from("relative/xdg/fonts")), "{roots_rel:?}");
        assert!(roots_rel.contains(&PathBuf::from("/custom/home/.local/share/fonts")), "{roots_rel:?}");

        for (region, index) in [
            (super::CjkRegion::Japanese, 0),
            (super::CjkRegion::Korean, 1),
            (super::CjkRegion::SimplifiedChinese, 2),
            (super::CjkRegion::TraditionalChinese, 3),
        ] {
            let paths = super::cjk_paths_with(region, &roots_xdg);
            for root in ["/run/host/fonts", "/run/host/local-fonts", "/run/host/user-fonts", "/custom/xdg/fonts", "/custom/home/.fonts"] {
                let expected = PathBuf::from(root).join("opentype/noto/NotoSansCJK-Regular.ttc");
                assert!(paths.contains(&(expected.clone(), index)), "{region:?}: missing {}", expected.display());
            }
        }

        let arabic_paths = super::script_paths_with(super::ARABIC_CANDIDATES, &roots_xdg);
        for root in ["/run/host/fonts", "/run/host/local-fonts", "/run/host/user-fonts", "/custom/xdg/fonts"] {
            let expected = PathBuf::from(root).join("truetype/dejavu/DejaVuSans.ttf");
            assert!(arabic_paths.contains(&expected), "missing {} in {arabic_paths:?}", expected.display());
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            let raw_xdg = OsStr::from_bytes(b"/custom/\xff/xdg");
            let raw_roots = super::linux_font_roots_with(Some(raw_xdg), None);
            let raw_cjk = super::cjk_paths_with(super::CjkRegion::SimplifiedChinese, &raw_roots);
            let raw_script = super::script_paths_with(super::ARABIC_CANDIDATES, &raw_roots);
            let expected_cjk = PathBuf::from(OsStr::from_bytes(b"/custom/\xff/xdg/fonts/opentype/noto/NotoSansCJK-Regular.ttc"));
            let expected_script = PathBuf::from(OsStr::from_bytes(b"/custom/\xff/xdg/fonts/truetype/dejavu/DejaVuSans.ttf"));
            assert!(raw_cjk.contains(&(expected_cjk, 2)));
            assert!(raw_script.contains(&expected_script));
        }
    }

    #[test]
    fn find_cjk_in_roots_discovers_nested_distribution_subfolders_in_preference_order() {
        let dir = tempfile::tempdir().unwrap();
        let valid_font = include_bytes!("../testdata/test-fallback.otf");

        // Higher-priority filename that is corrupt/unusable must be skipped.
        let corrupt_dir = dir.path().join("other");
        std::fs::create_dir_all(&corrupt_dir).unwrap();
        std::fs::write(corrupt_dir.join("NotoSansCJK-Regular.ttc"), b"not a font").unwrap();

        // Lower-priority valid fallback.
        let wqy_dir = dir.path().join("alt");
        std::fs::create_dir_all(&wqy_dir).unwrap();
        let wqy_path = wqy_dir.join("wqy-microhei.ttc");
        std::fs::write(&wqy_path, valid_font).unwrap();

        // Preferred NotoSansSC-Regular.otf placed inside a non-standard nested subfolder.
        let nested_dir = dir.path().join("distro-cjk").join("sub");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let sc_path = nested_dir.join("NotoSansSC-Regular.otf");
        std::fs::write(&sc_path, valid_font).unwrap();

        // Another region's font present in the same tree is indexed in the same scan.
        let jp_path = nested_dir.join("NotoSansJP-Regular.otf");
        std::fs::write(&jp_path, valid_font).unwrap();

        let missing_index = super::scan_cjk_roots(&[dir.path().join("missing")]);
        assert!(missing_index.summary.is_clean(), "missing optional roots should not emit a notice");
        assert!(missing_index.summary.notice().is_none());
        assert!(super::find_cjk_in_index(&missing_index.files, super::CjkRegion::SimplifiedChinese).is_none());

        let index = super::scan_cjk_roots(&[dir.path().to_path_buf()]);
        assert!(index.summary.is_clean());
        assert!(index.summary.notice().is_none());
        let (chosen_path, found_bytes, found_face) =
            super::find_cjk_in_index(&index.files, super::CjkRegion::SimplifiedChinese).expect("should discover nested SC font");
        assert_eq!(chosen_path, sc_path);
        assert_eq!(found_bytes, valid_font);
        assert_eq!(found_face, 0);

        let (chosen_jp, _, jp_face) =
            super::find_cjk_in_index(&index.files, super::CjkRegion::Japanese).expect("single scan should also index Japanese font");
        assert_eq!(chosen_jp, jp_path);
        assert_eq!(jp_face, 0);

        // Remove NotoSansSC-Regular.otf -> falls back to wqy-microhei.ttc.
        std::fs::remove_file(&sc_path).unwrap();
        let fallback_index = super::scan_cjk_roots(&[dir.path().to_path_buf()]);
        let (fallback_path, fallback_bytes, fallback_face) =
            super::find_cjk_in_index(&fallback_index.files, super::CjkRegion::SimplifiedChinese)
                .expect("should fall back to wqy-microhei.ttc");
        assert_eq!(fallback_path, wqy_path);
        assert_eq!(fallback_bytes, valid_font);
        assert_eq!(fallback_face, 0);

        // Corrupt-only directory returns None.
        std::fs::remove_file(&wqy_path).unwrap();
        let corrupt_index = super::scan_cjk_roots(&[dir.path().to_path_buf()]);
        assert!(super::find_cjk_in_index(&corrupt_index.files, super::CjkRegion::SimplifiedChinese).is_none());
    }

    #[test]
    fn serif_font_roots_keeps_system_roots_when_linux_roots_are_empty() {
        use std::path::PathBuf;

        assert_eq!(
            super::serif_font_roots(&[]),
            vec![PathBuf::from("/usr/share/fonts"), PathBuf::from("/usr/share/fonts/truetype"), PathBuf::from("/usr/share/fonts/opentype")]
        );

        let with_extra = super::serif_font_roots(&[PathBuf::from("/usr/share/fonts"), PathBuf::from("/run/host/fonts")]);
        assert_eq!(
            with_extra,
            vec![
                PathBuf::from("/usr/share/fonts"),
                PathBuf::from("/usr/share/fonts/truetype"),
                PathBuf::from("/usr/share/fonts/opentype"),
                PathBuf::from("/run/host/fonts"),
                PathBuf::from("/run/host/fonts/truetype"),
                PathBuf::from("/run/host/fonts/opentype"),
            ]
        );
    }

    #[test]
    fn scan_cjk_roots_reports_truncation_and_is_silent_when_clean() {
        let dir = tempfile::tempdir().unwrap();
        let sub_a = dir.path().join("a");
        let sub_b = dir.path().join("b");
        std::fs::create_dir_all(&sub_a).unwrap();
        std::fs::create_dir_all(&sub_b).unwrap();
        std::fs::write(sub_a.join("f1.ttf"), b"x").unwrap();
        std::fs::write(sub_a.join("f2.ttf"), b"x").unwrap();
        std::fs::write(sub_b.join("f3.ttf"), b"x").unwrap();

        // Clean scan within limits produces no notice.
        let clean = super::scan_cjk_roots_bounded(&[dir.path().to_path_buf()], 6, 10, 10);
        assert!(clean.summary.is_clean(), "{:?}", clean.summary);
        assert!(clean.summary.notice().is_none());

        // Capping files below the directory's file count reports truncation with count and path.
        let capped_files = super::scan_cjk_roots_bounded(&[dir.path().to_path_buf()], 6, 10, 1);
        assert!(!capped_files.summary.is_clean());
        assert!(capped_files.summary.truncated_dirs >= 1, "{:?}", capped_files.summary);
        let notice = capped_files.summary.notice().expect("truncated scan should produce a notice");
        assert!(notice.contains("reached the scan limit"), "{notice}");
        assert!(capped_files.summary.sample_path.is_some());

        // Subfolders sorted after the file that hits max_files are also counted as skipped.
        let behind_dir = tempfile::tempdir().unwrap();
        std::fs::write(behind_dir.path().join("a1.ttf"), b"x").unwrap();
        std::fs::write(behind_dir.path().join("a2.ttf"), b"x").unwrap();
        std::fs::create_dir_all(behind_dir.path().join("z1")).unwrap();
        std::fs::create_dir_all(behind_dir.path().join("z2")).unwrap();
        let capped_behind = super::scan_cjk_roots_bounded(&[behind_dir.path().to_path_buf()], 6, 10, 1);
        assert_eq!(capped_behind.summary.truncated_dirs, 3, "{:?}", capped_behind.summary);

        // Capping depth below the subfolders reports skipped folders.
        let capped_depth = super::scan_cjk_roots_bounded(&[dir.path().to_path_buf()], 0, 10, 10);
        assert_eq!(capped_depth.summary.truncated_dirs, 2, "{:?}", capped_depth.summary);
        assert!(capped_depth.summary.notice().is_some());

        // Unreadable and entry-error counts appear in notice text.
        let err_summary = super::CjkRootScanSummary {
            unreadable_dirs: 2,
            entry_errors: 3,
            truncated_dirs: 0,
            thread_spawn_failed: false,
            sample_path: Some(dir.path().to_path_buf()),
        };
        let err_notice = err_summary.notice().expect("errors should produce a notice");
        assert!(err_notice.contains("2 unreadable folder(s)"), "{err_notice}");
        assert!(err_notice.contains("3 directory entry error(s)"), "{err_notice}");
    }

    #[cfg(unix)]
    #[test]
    fn scan_cjk_roots_revisits_canonical_directory_when_reached_shallower() {
        let dir = tempfile::tempdir().unwrap();
        let valid_font = include_bytes!("../testdata/test-fallback.otf");

        let target = dir.path().join("target");
        let sub = target.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let expected = sub.join("NotoSansSC-Regular.otf");
        std::fs::write(&expected, valid_font).unwrap();

        // Sits at depth 6 under deep_root, so sub sits at depth 7 (> MAX_DEPTH) through the symlink.
        let deep_parent = dir.path().join("deep").join("a").join("b").join("c").join("d").join("e");
        std::fs::create_dir_all(&deep_parent).unwrap();
        std::os::unix::fs::symlink(&target, deep_parent.join("link")).unwrap();

        let deep_only = super::scan_cjk_roots(&[dir.path().join("deep")]);
        assert!(super::find_cjk_in_index(&deep_only.files, super::CjkRegion::SimplifiedChinese).is_none());
        assert_eq!(deep_only.summary.truncated_dirs, 1, "sub at depth 7 should be reported as skipped");

        // Visiting target again as a shallower root revisits it and discovers sub/NotoSansSC-Regular.otf.
        let revisited = super::scan_cjk_roots(&[dir.path().join("deep"), target.clone()]);
        assert!(revisited.summary.is_clean(), "revisited sub should not count as skipped: {:?}", revisited.summary);
        let (chosen, _, _) =
            super::find_cjk_in_index(&revisited.files, super::CjkRegion::SimplifiedChinese).expect("shallower root should revisit target");
        assert_eq!(chosen, expected);
    }

    #[cfg(unix)]
    #[test]
    fn scan_cjk_roots_revisit_does_not_double_count_caps_or_entry_errors() {
        let dir = tempfile::tempdir().unwrap();
        let valid_font = include_bytes!("../testdata/test-fallback.otf");

        let target = dir.path().join("target");
        let sub = target.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(target.join("other.ttf"), b"x").unwrap();
        let expected = sub.join("NotoSansSC-Regular.otf");
        std::fs::write(&expected, valid_font).unwrap();

        let deep = dir.path().join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        std::os::unix::fs::symlink(&target, deep.join("link")).unwrap();

        // Exact unique totals across [deep, target]: 3 directories (deep, target, sub) and 2 files
        // (other.ttf, NotoSansSC-Regular.otf). Under max_depth = 1, target is first visited at
        // depth 1 through deep/link (leaving sub at depth 2) and revisited at depth 0 as the
        // second root. Revisiting must not re-spend directory, file, or entry budgets.
        let tight = super::scan_cjk_roots_bounded(&[deep.clone(), target.clone()], 1, 3, 2);
        assert!(tight.summary.is_clean(), "revisit must not double-count scan caps: {:?}", tight.summary);
        assert!(tight.summary.notice().is_none());
        let (chosen, _, _) =
            super::find_cjk_in_index(&tight.files, super::CjkRegion::SimplifiedChinese).expect("sub must still fit within tight caps");
        assert_eq!(chosen, expected);

        // Metadata errors inside target must likewise be counted once, not duplicated on revisit.
        let loop_a = target.join("loop-a");
        let loop_b = target.join("loop-b");
        std::os::unix::fs::symlink(&loop_b, &loop_a).unwrap();
        std::os::unix::fs::symlink(&loop_a, &loop_b).unwrap();
        let with_loop = super::scan_cjk_roots_bounded(&[deep, target], 1, 10, 10);
        assert_eq!(with_loop.summary.entry_errors, 2, "entry errors must not double-count on revisit: {:?}", with_loop.summary);
        assert_eq!(with_loop.summary.truncated_dirs, 0, "{:?}", with_loop.summary);
        let (chosen_with_loop, _, _) = super::find_cjk_in_index(&with_loop.files, super::CjkRegion::SimplifiedChinese)
            .expect("nested CJK font still found alongside symlink loop");
        assert_eq!(chosen_with_loop, expected);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn cjk_scan_state_stays_non_blocking_while_async_scan_runs_and_retries_when_ready() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("distro").join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        let valid_font = include_bytes!("../testdata/test-fallback.otf");
        std::fs::write(nested.join("NotoSansSC-Regular.otf"), valid_font).unwrap();

        // 1) Fixed-path hit never starts the background scan, even with async enabled.
        static FIXED_STATE: super::CjkScanState = super::CjkScanState::new();
        FIXED_STATE.enable_async();
        let fixed_res = FIXED_STATE.resolve_region(
            super::CjkRegion::SimplifiedChinese,
            |_| Some((valid_font.to_vec(), 0)),
            true,
            || super::scan_cjk_roots(&[]),
            |_| None,
        );
        assert!(matches!(fixed_res, super::CjkResolveResult::Resolved(Some(_))));
        assert!(!FIXED_STATE.is_pending());
        assert!(!FIXED_STATE.is_ready(), "fixed-path hit must not start the folder walk");

        // 2) Async UI mode: a fixed-path miss spawns a background scan and returns Pending without
        // blocking; a blocking export worker waits for the scan and resolves immediately, and UI
        // retries succeed once the background scan completes and bumps FONT_GENERATION.
        static ASYNC_STATE: super::CjkScanState = super::CjkScanState::new();
        ASYNC_STATE.enable_async();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let entered = Arc::new(Mutex::new(Some(entered_tx)));
        let release = Arc::new(Mutex::new(Some(release_rx)));
        let scan_root = dir.path().to_path_buf();
        let gen_before = super::font_generation();

        let first = ASYNC_STATE.resolve_region(
            super::CjkRegion::SimplifiedChinese,
            |_| None,
            true,
            move || {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().unwrap().take() {
                    let _ = rx.recv_timeout(Duration::from_secs(5));
                }
                super::scan_cjk_roots(std::slice::from_ref(&scan_root))
            },
            |_| None,
        );
        assert!(matches!(first, super::CjkResolveResult::Pending), "UI caller must not block while scanning");
        entered_rx.recv_timeout(Duration::from_secs(5)).expect("background scan thread started");
        assert!(ASYNC_STATE.is_pending());
        assert!(!ASYNC_STATE.is_ready());

        // Subsequent UI calls while still scanning stay non-blocking and do not poison cache.
        let second =
            ASYNC_STATE.resolve_region(super::CjkRegion::SimplifiedChinese, |_| None, true, || super::scan_cjk_roots(&[]), |_| None);
        assert!(matches!(second, super::CjkResolveResult::Pending));

        // A background export worker using `with_blocking_cjk_scan` waits for the scan and gets
        // the discovered face rather than `Pending`.
        let (export_tx, export_rx) = mpsc::channel();
        let export_handle = std::thread::Builder::new()
            .name("test-cjk-export".into())
            .spawn(move || {
                let res = super::with_blocking_cjk_scan(|| {
                    ASYNC_STATE.resolve_region(super::CjkRegion::SimplifiedChinese, |_| None, true, || super::scan_cjk_roots(&[]), |_| None)
                });
                let _ =
                    export_tx.send(matches!(res, super::CjkResolveResult::Resolved(Some((bytes, 0))) if bytes.as_slice() == valid_font));
            })
            .unwrap();

        release_tx.send(()).unwrap();
        assert!(export_rx.recv_timeout(Duration::from_secs(5)).unwrap(), "export worker must resolve discovered CJK font");
        export_handle.join().unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !ASYNC_STATE.is_ready() || super::font_generation() == gen_before {
            assert!(Instant::now() < deadline, "timed out waiting for async scan completion");
            std::thread::yield_now();
        }
        assert!(!ASYNC_STATE.is_pending());
        assert!(super::font_generation() > gen_before, "completing scan must bump font generation");

        let (retried, hit_pending_after) = super::track_pending_cjk(|| {
            ASYNC_STATE.resolve_region(super::CjkRegion::SimplifiedChinese, |_| None, true, || super::scan_cjk_roots(&[]), |_| None)
        });
        assert!(!hit_pending_after, "resolved CJK lookup must not report pending");
        match retried {
            super::CjkResolveResult::Resolved(Some((bytes, idx))) => {
                assert_eq!(bytes.as_slice(), valid_font);
                assert_eq!(*idx, 0);
            }
            _ => panic!("retry after background scan completes must return discovered CJK font"),
        }
        assert!(ASYNC_STATE.take_summary().is_none(), "clean scan emits no notice");

        // 3) Empty async scan still bumps FONT_GENERATION so disk-cache fallback unblocks metrics.
        static EMPTY_ASYNC_STATE: super::CjkScanState = super::CjkScanState::new();
        EMPTY_ASYNC_STATE.enable_async();
        let empty_gen = super::font_generation();
        let (empty_first, hit_pending) = super::track_pending_cjk(|| {
            EMPTY_ASYNC_STATE.resolve_region(
                super::CjkRegion::SimplifiedChinese,
                |_| None,
                true,
                || super::scan_cjk_roots(&[]),
                |_| Some((valid_font.to_vec(), 0)),
            )
        });
        assert!(matches!(empty_first, super::CjkResolveResult::Pending));
        assert!(hit_pending, "pending async scan lookup must signal track_pending_cjk");
        let empty_deadline = Instant::now() + Duration::from_secs(5);
        while !EMPTY_ASYNC_STATE.is_ready() || super::font_generation() == empty_gen {
            assert!(Instant::now() < empty_deadline, "timed out waiting for empty async scan");
            std::thread::yield_now();
        }
        assert!(EMPTY_ASYNC_STATE.is_ready());
        assert!(super::font_generation() > empty_gen, "empty async scan must still bump font generation");
        let cached_fallback = EMPTY_ASYNC_STATE.resolve_region(
            super::CjkRegion::SimplifiedChinese,
            |_| None,
            true,
            || super::scan_cjk_roots(&[]),
            |_| Some((valid_font.to_vec(), 0)),
        );
        assert!(matches!(cached_fallback, super::CjkResolveResult::Resolved(Some(_))));

        // 4) Refused scan thread records a non-clean notice without running the walk inline.
        static REFUSED_STATE: super::CjkScanState = super::CjkScanState::new();
        REFUSED_STATE.enable_async();
        REFUSED_STATE.record_spawn_refused();
        assert!(REFUSED_STATE.is_ready());
        assert!(!REFUSED_STATE.is_pending());
        let refused_summary = REFUSED_STATE.take_summary().expect("spawn refusal must produce a notice");
        assert!(refused_summary.thread_spawn_failed);
        assert!(refused_summary.notice().is_some_and(|n| n.contains("background scan thread could not start")));

        // 5) Sync headless/CLI mode (async not enabled): resolves synchronously on the first call.
        static SYNC_STATE: super::CjkScanState = super::CjkScanState::new();
        let sync_root = dir.path().to_path_buf();
        let sync_res = SYNC_STATE.resolve_region(
            super::CjkRegion::SimplifiedChinese,
            |_| None,
            true,
            move || super::scan_cjk_roots(std::slice::from_ref(&sync_root)),
            |_| None,
        );
        match sync_res {
            super::CjkResolveResult::Resolved(Some((bytes, idx))) => {
                assert_eq!(bytes.as_slice(), valid_font);
                assert_eq!(*idx, 0);
            }
            _ => panic!("sync mode must discover nested CJK font immediately"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn scan_cjk_roots_deduplicates_self_link_and_skips_named_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let valid_font = include_bytes!("../testdata/test-fallback.otf");

        // Self-referencing symlink sorted before `z` must not consume the remaining directory slot.
        std::os::unix::fs::symlink(dir.path(), dir.path().join("a-link")).unwrap();
        let z_dir = dir.path().join("z");
        std::fs::create_dir_all(&z_dir).unwrap();
        let expected = z_dir.join("NotoSansSC-Regular.otf");
        std::fs::write(&expected, valid_font).unwrap();

        let tight = super::scan_cjk_roots_bounded(&[dir.path().to_path_buf()], 6, 2, 10);
        assert!(tight.summary.is_clean(), "self-link alias must not exhaust dir cap: {:?}", tight.summary);
        let (chosen, _, _) =
            super::find_cjk_in_index(&tight.files, super::CjkRegion::SimplifiedChinese).expect("z should use the second directory slot");
        assert_eq!(chosen, expected);

        // Named pipe (FIFO) with a matching CJK font filename must be skipped and never block open.
        let fifo_dir = tempfile::tempdir().unwrap();
        let fifo_path = fifo_dir.path().join("NotoSansSC-Regular.otf");
        let mkfifo = std::process::Command::new("mkfifo").arg(&fifo_path).status().expect("mkfifo available on unix");
        assert!(mkfifo.success(), "mkfifo failed");
        let fifo_scan = super::scan_cjk_roots(&[fifo_dir.path().to_path_buf()]);
        assert!(fifo_scan.summary.is_clean(), "{:?}", fifo_scan.summary);
        assert!(fifo_scan.files.is_empty(), "special files must not be indexed: {:?}", fifo_scan.files);
        assert!(super::read_bounded(&fifo_path, super::MAX_FONT_BYTES as u64).is_none(), "read_bounded must reject FIFOs without blocking");
    }

    #[cfg(unix)]
    #[test]
    fn scan_cjk_roots_reports_metadata_error_on_symlink_loop() {
        // Symlink loops produce a non-NotFound metadata error (ELOOP) regardless of user privileges.
        let loop_dir = tempfile::tempdir().unwrap();
        let loop_a = loop_dir.path().join("loop-a");
        let loop_b = loop_dir.path().join("loop-b");
        std::os::unix::fs::symlink(&loop_b, &loop_a).unwrap();
        std::os::unix::fs::symlink(&loop_a, &loop_b).unwrap();
        let loop_scan = super::scan_cjk_roots(&[loop_dir.path().to_path_buf()]);
        assert!(!loop_scan.summary.is_clean(), "{:?}", loop_scan.summary);
        assert_eq!(loop_scan.summary.entry_errors, 2, "{:?}", loop_scan.summary);
        assert_eq!(loop_scan.summary.sample_path.as_deref(), Some(loop_a.as_path()));

        // When scan caps are already exhausted, a later root whose metadata fails with a non-NotFound
        // error is still counted as skipped rather than silently dropped like NotFound.
        let first_root = tempfile::tempdir().unwrap();
        std::fs::write(first_root.path().join("a.ttf"), b"x").unwrap();
        let missing_root = first_root.path().join("does-not-exist");
        let capped = super::scan_cjk_roots_bounded(&[first_root.path().to_path_buf(), missing_root, loop_a.clone()], 6, 10, 1);
        assert_eq!(capped.summary.truncated_dirs, 1, "{:?}", capped.summary);
        assert_eq!(capped.summary.sample_path.as_deref(), Some(loop_a.as_path()));
    }
}
