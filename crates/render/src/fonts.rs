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
fn read_bounded(path: impl AsRef<std::path::Path>, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn read(path: &str) -> Option<Vec<u8>> {
    let bytes = read_bounded(path, MAX_FONT_BYTES as u64)?;
    ab_glyph::FontRef::try_from_slice(&bytes).ok()?;
    Some(bytes)
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
        let family = |[r, b, i, bi]: &[&str; 4]| {
            let regular = read(r)?;
            Some(SerifFonts { regular, bold: read(b), italic: read(i), bold_italic: read(bi), variable: false, source: (*r).to_string() })
        };
        SERIF_CANDIDATES.iter().find_map(family).or_else(|| variable_noto_serif(&FONT_ROOTS)).or_else(|| SERIF_LAST.iter().find_map(family))
    })
    .as_ref()
}

/// The folders under which distributions put each font package's own folder.
const FONT_ROOTS: [&str; 3] = ["/usr/share/fonts", "/usr/share/fonts/truetype", "/usr/share/fonts/opentype"];

/// Noto Serif as recent distributions ship it: a variable font (`NotoSerif[wdth,wght].ttf`, or
/// `[wght]`), with its italic beside it, in a package folder under one of `roots` ([`FONT_ROOTS`]).
/// Found by its name's start, whatever axes it lists; its bold is the same file (kept once),
/// drawn at the bold weight.
fn variable_noto_serif(roots: &[&str]) -> Option<SerifFonts> {
    let named = |files: &[std::path::PathBuf], stem: &str| {
        files.iter().find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(stem) && n.ends_with(".ttf"))).cloned()
    };
    // Folders and files in name order, so the same one is picked on every machine whatever order
    // the system lists them in; loose files don't count against the folders taken.
    let listed = |dir: &std::path::Path| {
        let mut paths: Vec<std::path::PathBuf> =
            std::fs::read_dir(dir).map(|e| e.flatten().take(10_000).map(|e| e.path()).collect()).unwrap_or_default();
        paths.sort();
        paths
    };
    for root in roots {
        for dir in listed(std::path::Path::new(root)).into_iter().filter(|p| p.is_dir()).take(1000) {
            let files = listed(&dir);
            let Some(r) = named(&files, "NotoSerif[") else { continue };
            let Some(regular) = r.to_str().and_then(read) else { continue };
            let italic = named(&files, "NotoSerif-Italic[").and_then(|p| p.to_str().and_then(read));
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
fn in_apple_dirs<T: Copy>(files: &[(&str, T)]) -> impl Iterator<Item = (String, T)> {
    APPLE_FONT_DIRS.iter().flat_map(move |dir| files.iter().map(move |(file, x)| (format!("{dir}/{file}"), *x)))
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
/// Windows first, then Apple's (macOS, iOS), Linux's and Android's.
fn cjk_paths(region: CjkRegion) -> Vec<(String, u32)> {
    let listed = cjk_candidates(region);
    let (windows, rest) = listed.split_at(listed.iter().take_while(|(p, _)| p.starts_with("C:\\")).count());
    windows
        .iter()
        .map(|(p, i)| (p.to_string(), *i))
        .chain(in_apple_dirs(apple_cjk_files(region)))
        .chain(rest.iter().map(|(p, i)| (p.to_string(), *i)))
        .chain(ANDROID_FONT_DIRS.iter().flat_map(|dir| android_cjk_files(region).map(|(file, i)| (format!("{dir}/{file}"), i))))
        .collect()
}

/// Installed CJK collections whose face is found by family name, not by index: (path, family).
fn cjk_named_candidates(region: CjkRegion) -> Vec<(String, &'static str)> {
    in_apple_dirs(apple_cjk_named_files(region)).collect()
}

/// A CJK font file's bytes. CJK collections are large; accept up to 128 MiB.
fn read_cjk(path: &str) -> Option<Vec<u8>> {
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

/// An installed CJK font for `region` (bytes and face index), found once. `None` on the web or
/// when nothing suitable is installed. Traditional Chinese falls back to the Simplified face,
/// which has the Traditional characters in mainland China's glyph forms.
pub fn cjk_font(region: CjkRegion) -> Option<&'static (Vec<u8>, u32)> {
    if std::env::var_os("RUSTGIS_BUILTIN_FONTS").is_some() {
        return None;
    }
    static FOUND: [OnceLock<Option<(Vec<u8>, u32)>>; 4] = [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let slot = match region {
        CjkRegion::SimplifiedChinese => &FOUND[0],
        CjkRegion::Japanese => &FOUND[1],
        CjkRegion::Korean => &FOUND[2],
        CjkRegion::TraditionalChinese => &FOUND[3],
    };
    let found = slot.get_or_init(|| {
        let indexed =
            cjk_paths(region).iter().find_map(|(path, index)| read_cjk(path).filter(|b| draws_cjk(b, *index, region)).map(|b| (b, *index)));
        indexed
            .or_else(|| {
                cjk_named_candidates(region).iter().find_map(|(path, family)| {
                    let bytes = read_cjk(path)?;
                    let index = regular_face(&bytes, family).filter(|i| draws_cjk(&bytes, *i, region))?;
                    Some((bytes, index))
                })
            })
            .or_else(|| cached_script_font(ScriptFont::Cjk(region)))
    });
    match (found, region) {
        (None, CjkRegion::TraditionalChinese) => cjk_font(CjkRegion::SimplifiedChinese),
        (found, _) => found.as_ref(),
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

/// The existing script candidates in every Apple font folder (iOS uses subfolders), and
/// Android system candidates in manufacturer folders too. Keep the listed paths first.
fn script_paths(candidates: &[&str]) -> Vec<String> {
    let mut paths: Vec<String> = candidates.iter().map(|p| (*p).to_string()).collect();
    for path in candidates {
        if let Some(file) = path.strip_prefix("/System/Library/Fonts/Supplemental/").or_else(|| path.strip_prefix("/System/Library/Fonts/"))
        {
            paths.extend(in_apple_dirs(&[(file, ())]).map(|(p, _)| p));
        } else if let Some(file) = path.strip_prefix("/system/fonts/") {
            paths.extend(ANDROID_FONT_DIRS.iter().map(|dir| format!("{dir}/{file}")));
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
        for candidates in [super::BENGALI_CANDIDATES, super::ARABIC_CANDIDATES, super::LAO_CANDIDATES, super::SYMBOL_CANDIDATES] {
            let paths = super::script_paths(candidates);
            assert!(paths.iter().any(|p| p.starts_with("/System/Library/Fonts/Core/")), "{candidates:?}");
        }
        for (candidates, file) in
            [(super::BENGALI_CANDIDATES, "NotoSansBengali-VF.ttf"), (super::LAO_CANDIDATES, "NotoSansLao-Regular.ttf")]
        {
            let paths = super::script_paths(candidates);
            assert!(paths.contains(&format!("/system/fonts/{file}")));
            assert!(paths.contains(&format!("/product/fonts/{file}")));
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
        use super::{CjkRegion, cjk_named_candidates, cjk_paths};
        for region in [CjkRegion::SimplifiedChinese, CjkRegion::TraditionalChinese, CjkRegion::Japanese, CjkRegion::Korean] {
            let paths: Vec<String> =
                cjk_paths(region).into_iter().map(|(p, _)| p).chain(cjk_named_candidates(region).into_iter().map(|(p, _)| p)).collect();
            let in_dir = |dir: &str| paths.iter().any(|p| p.rsplit_once('/').is_some_and(|(d, _)| d == dir));
            assert!(in_dir("/System/Library/Fonts"), "{region:?}: no macOS face");
            assert!(in_dir("/System/Library/Fonts/Core"), "{region:?}: no iOS face");
            assert!(in_dir("/system/fonts"), "{region:?}: no Android face");
            assert!(paths.first().is_some_and(|p| p.starts_with("C:\\")), "{region:?}: Windows first");
        }
        for (region, index) in
            [(CjkRegion::Japanese, 0), (CjkRegion::Korean, 1), (CjkRegion::SimplifiedChinese, 2), (CjkRegion::TraditionalChinese, 3)]
        {
            let paths = cjk_paths(region);
            for dir in super::ANDROID_FONT_DIRS {
                assert!(paths.contains(&(format!("{dir}/NotoSansCJK-Regular.ttc"), index)), "{region:?}");
                assert!(paths.contains(&(format!("{dir}/DroidSansFallback.ttf"), 0)), "{region:?}: legacy face");
            }
        }
        let korean = cjk_paths(CjkRegion::Korean);
        assert!(korean.iter().any(|(p, i)| p == "/System/Library/Fonts/Core/AppleSDGothicNeo.ttc" && *i == 0));
        let japanese = cjk_paths(CjkRegion::Japanese);
        assert!(japanese.iter().any(|(p, _)| p == "/System/Library/Fonts/Core/HiraginoKakuGothic.ttc"));
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
        for (p, i) in super::cjk_paths(CjkRegion::Korean).iter().filter(|(p, _)| p.ends_with("NotoSansCJK-Regular.ttc")) {
            if let Ok(b) = std::fs::read(p) {
                assert!(draws_cjk(&b, *i, CjkRegion::Korean), "{p}");
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
}
