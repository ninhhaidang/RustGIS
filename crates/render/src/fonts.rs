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

/// Largest font file accepted (32 MiB).
const MAX_FONT: u64 = 32 << 20;

pub struct UiFonts {
    pub regular: Vec<u8>,
    pub semibold: Vec<u8>,
    /// File the regular face came from, or "built-in".
    pub source: String,
}

fn read(path: &str) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FONT {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    ab_glyph::FontRef::try_from_slice(&bytes).ok()?;
    Some(bytes)
}

/// The weight the semibold face is drawn at where it is a variable font (#591): macOS's system
/// font is one file for every weight, drawn at its default 400 unless told otherwise. 600 is
/// semibold, as the static faces used elsewhere (Segoe UI Semibold, Inter SemiBold) are; a face
/// with no weight axis ignores it. Both renderers set it, so text is as heavy on screen as in an
/// export.
pub const SEMIBOLD_WEIGHT: f32 = 600.0;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CjkRegion {
    SimplifiedChinese,
    Japanese,
    Korean,
}

/// Installed CJK font candidates: (path, face index in a collection).
fn cjk_candidates(region: CjkRegion) -> &'static [(&'static str, u32)] {
    // Noto Sans CJK collections order their faces JP, KR, SC, TC, HK.
    match region {
        CjkRegion::SimplifiedChinese => &[
            ("C:\\Windows\\Fonts\\msyh.ttc", 0),
            ("/System/Library/Fonts/PingFang.ttc", 0),
            ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/noto-cjk/NotoSansSC-Regular.otf", 0),
            ("/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc", 0),
        ],
        CjkRegion::Japanese => &[
            ("C:\\Windows\\Fonts\\YuGothR.ttc", 0),
            ("C:\\Windows\\Fonts\\meiryo.ttc", 0),
            ("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansJP-Regular.otf", 0),
        ],
        CjkRegion::Korean => &[
            ("C:\\Windows\\Fonts\\malgun.ttf", 0),
            ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 1),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 1),
            ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 1),
            ("/usr/share/fonts/noto-cjk/NotoSansKR-Regular.otf", 0),
        ],
    }
}

/// An installed CJK font for `region` (bytes and face index), found once. `None` on the web or
/// when nothing suitable is installed.
pub fn cjk_font(region: CjkRegion) -> Option<&'static (Vec<u8>, u32)> {
    static FOUND: [OnceLock<Option<(Vec<u8>, u32)>>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let slot = match region {
        CjkRegion::SimplifiedChinese => &FOUND[0],
        CjkRegion::Japanese => &FOUND[1],
        CjkRegion::Korean => &FOUND[2],
    };
    slot.get_or_init(|| {
        cjk_candidates(region).iter().find_map(|(path, index)| {
            let meta = std::fs::metadata(path).ok()?;
            // CJK collections are large; accept up to 128 MiB.
            if meta.len() > 128 << 20 {
                return None;
            }
            std::fs::read(path).ok().filter(|b| valid_font(b, *index)).map(|b| (b, *index))
        })
    })
    .as_ref()
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
        CjkRegion::Japanese => "https://assets.rustgis.app/fonts/NotoSansJP-Regular.otf",
        CjkRegion::Korean => "https://assets.rustgis.app/fonts/NotoSansKR-Regular.otf",
    }
}

/// A script the interface fonts may lack, and that an interface language needs a face for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptFont {
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
    "C:\\Windows\\Fonts\\siyamrupali.ttf",
    "C:\\Windows\\Fonts\\Nirmala.ttf",
    "/System/Library/Fonts/Supplemental/NotoSansBengali-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansBengali-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansBengaliUI-Regular.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansBengali-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansBengali-Regular.ttf",
];

fn covers_bengali(bytes: &[u8]) -> bool {
    use ab_glyph::Font;
    ab_glyph::FontRef::try_from_slice(bytes).is_ok_and(|f| ['\u{0985}', '\u{09be}', 'A', 'z'].iter().all(|c| f.glyph_id(*c).0 != 0))
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
    use ab_glyph::Font;
    ab_glyph::FontRef::try_from_slice_and_index(bytes, index)
        .is_ok_and(|f| ['\u{6CC}', '\u{6AF}', 'A', 'z'].iter().all(|c| f.glyph_id(*c).0 != 0))
}

/// Whether a face draws Lao letters (ko, kho, the vowel sign aa and a tone mark). Latin letters
/// aren't required: the face falls back after the interface face, which draws them.
fn covers_lao(bytes: &[u8], index: u32) -> bool {
    use ab_glyph::Font;
    ab_glyph::FontRef::try_from_slice_and_index(bytes, index)
        .is_ok_and(|f| ['\u{0E81}', '\u{0E82}', '\u{0EB2}', '\u{0EC8}'].iter().all(|c| f.glyph_id(*c).0 != 0))
}

/// An installed face for `script` (bytes and face index), found once. `None` on the web or when
/// nothing suitable is installed.
pub fn script_font(script: ScriptFont) -> Option<&'static (Vec<u8>, u32)> {
    match script {
        ScriptFont::Cjk(region) => cjk_font(region),
        ScriptFont::Arabic => {
            static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
            FOUND.get_or_init(|| ARABIC_CANDIDATES.iter().find_map(|p| read(p).filter(|b| covers_arabic(b, 0)).map(|b| (b, 0)))).as_ref()
        }
        ScriptFont::Bengali => {
            static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
            FOUND.get_or_init(|| BENGALI_CANDIDATES.iter().find_map(|p| read(p).filter(|b| covers_bengali(b)).map(|b| (b, 0)))).as_ref()
        }
        ScriptFont::Lao => {
            static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
            FOUND.get_or_init(|| LAO_CANDIDATES.iter().find_map(|p| read(p).filter(|b| covers_lao(b, 0)).map(|b| (b, 0)))).as_ref()
        }
    }
}

/// Installed faces with the symbols the interface marks things with (checked by
/// [`covers_symbols`]): the interface faces lack some, Segoe UI the check mark among them.
const SYMBOL_CANDIDATES: &[&str] = &[
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
    use ab_glyph::Font;
    ab_glyph::FontRef::try_from_slice_and_index(bytes, index).is_ok_and(|f| ['\u{2713}', '\u{2717}'].iter().all(|c| f.glyph_id(*c).0 != 0))
}

/// An installed face for symbols the interface faces may lack (bytes and face index), found
/// once. `None` on the web or when nothing suitable is installed.
pub fn symbol_font() -> Option<&'static (Vec<u8>, u32)> {
    static FOUND: OnceLock<Option<(Vec<u8>, u32)>> = OnceLock::new();
    FOUND.get_or_init(|| SYMBOL_CANDIDATES.iter().find_map(|p| read(p).filter(|b| covers_symbols(b, 0)).map(|b| (b, 0)))).as_ref()
}

/// Where the web app downloads a verified face for `script` from. Bengali and Lao have no verified
/// hosted asset yet, so they are discovered only among installed desktop fonts.
pub fn script_font_url(script: ScriptFont) -> Option<&'static str> {
    match script {
        ScriptFont::Cjk(region) => Some(cjk_font_url(region)),
        ScriptFont::Arabic => Some("https://assets.rustgis.app/fonts/Vazirmatn-Regular.ttf"),
        // No verified Bengali asset is hosted by RustGIS; don't guess a URL.
        ScriptFont::Bengali | ScriptFont::Lao => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bengali_coverage_requires_bengali_and_latin_glyphs() {
        assert!(!super::covers_bengali(epaint_default_fonts::UBUNTU_LIGHT));
        assert!(!super::covers_bengali(b"not a font"));
        assert_eq!(super::script_font_url(super::ScriptFont::Bengali), None);
    }

    #[test]
    fn lao_coverage_requires_lao_glyphs() {
        assert!(!super::covers_lao(epaint_default_fonts::UBUNTU_LIGHT, 0));
        assert!(!super::covers_lao(b"not a font", 0));
        assert_eq!(super::script_font_url(super::ScriptFont::Lao), None);
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
